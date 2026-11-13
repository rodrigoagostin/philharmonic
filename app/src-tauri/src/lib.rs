//! The native side of the desktop app. It's deliberately thin: the UI sends daemon requests
//! through one command (`daemon`), and a background task forwards the daemon's event stream
//! to the UI as `ph-event`. If the daemon isn't running, the app starts it.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use philharmonic_core::paths;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Send one request to the daemon and return its response (JSON in, JSON out).
#[tauri::command]
async fn daemon(request: Value) -> Result<Value, String> {
    send(&request).await.map_err(|e| format!("{e:#}"))
}

async fn send(request: &Value) -> Result<Value> {
    let stream = UnixStream::connect(paths::socket_path()?).await?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    let mut reply = String::new();
    BufReader::new(reader).read_line(&mut reply).await?;
    if reply.is_empty() {
        bail!("the daemon closed the connection");
    }
    Ok(serde_json::from_str(&reply)?)
}

async fn ping() -> bool {
    send(&serde_json::json!({ "type": "ping" })).await.is_ok()
}

/// `philharmonicd` sits next to the app binary: in `target/debug/` during development, and in
/// `Philharmonic.app/Contents/MacOS/` in the packaged app.
fn daemon_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("PH_DAEMON") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("philharmonicd")))
        .unwrap_or_else(|| PathBuf::from("philharmonicd"))
}

/// Start the daemon in the background if nobody answers on the socket.
/// It keeps running after the app quits, so agents survive closing the window.
async fn ensure_daemon() -> Result<()> {
    if ping().await {
        return Ok(());
    }
    let bin = daemon_binary();
    if !bin.exists() {
        bail!("{} not found. Build it with `cargo build -p philharmonicd -p ph`", bin.display());
    }
    std::fs::create_dir_all(paths::data_dir()?)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::daemon_log()?)?;
    std::process::Command::new(&bin)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if ping().await {
            return Ok(());
        }
    }
    bail!("started {} but it isn't answering (see {})", bin.display(), paths::daemon_log()?.display())
}

fn mtime(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Is the running daemon an older build than the one installed next to this app?
/// (Happens after an update: the daemon keeps running so agents survive closing the window.)
#[tauri::command]
async fn daemon_outdated() -> Result<bool, String> {
    let pong = send(&serde_json::json!({ "type": "ping" })).await.map_err(|e| format!("{e:#}"))?;
    let Some(exe) = pong["exe"].as_str() else {
        return Ok(true); // builds before this check existed don't report their binary
    };
    let mine = daemon_binary();
    let same_binary = std::fs::canonicalize(exe).ok() == std::fs::canonicalize(&mine).ok();
    // Only judge the daemon that belongs to this app (not one started from another folder).
    if !same_binary {
        return Ok(false);
    }
    // Compare contents, so rebuilding or reinstalling the same build doesn't count as an
    // update. Daemons from before `exe_hash` existed only report the file's date.
    Ok(match pong["exe_hash"].as_u64().unwrap_or(0) {
        0 => mtime(&mine) != pong["exe_mtime"].as_u64().unwrap_or(0),
        hash => philharmonic_core::fingerprint::file(&mine) != hash,
    })
}

/// Restart the daemon on the installed build, then resume the given tasks' agents (they
/// continue their conversations). Returns the ids that couldn't be resumed, with the reason.
#[tauri::command]
async fn restart_daemon(resume: Vec<u64>) -> Result<Vec<String>, String> {
    restart(resume).await.map_err(|e| format!("{e:#}"))
}

async fn restart(resume: Vec<u64>) -> Result<Vec<String>> {
    let pong = send(&serde_json::json!({ "type": "ping" })).await?;
    if pong["pid"].as_u64().unwrap_or(0) > 0 {
        send(&serde_json::json!({ "type": "shutdown" })).await?;
    } else {
        // Older daemons don't know `shutdown`; SIGTERM makes them stop their agents and exit.
        std::process::Command::new("pkill").arg("-TERM").arg("-f").arg(daemon_binary()).status()?;
    }
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !ping().await {
            break;
        }
    }
    if ping().await {
        bail!("the old daemon didn't stop");
    }
    ensure_daemon().await?;
    let mut failed = Vec::new();
    for id in resume {
        let reply = send(&serde_json::json!({ "type": "resume", "id": id })).await?;
        if reply["type"] == "error" {
            failed.push(format!("task {id}: {}", reply["message"].as_str().unwrap_or("?")));
        }
    }
    Ok(failed)
}

/// Subscribe to the daemon's events and forward them to the UI. Reconnects forever.
/// Tasks whose terminal the window is showing. Output of other tasks isn't sent to it: with
/// many busy agents, that would be a steady stream of work for nothing.
static WATCHED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

#[tauri::command]
fn watch_output(ids: Vec<u64>) {
    *WATCHED.lock().unwrap() = ids;
}

async fn event_loop(app: AppHandle) {
    loop {
        if let Err(e) = stream_events(&app).await {
            let _ = app.emit("ph-connection", serde_json::json!({ "connected": false, "error": format!("{e:#}") }));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn stream_events(app: &AppHandle) -> Result<()> {
    ensure_daemon().await?;
    let stream = UnixStream::connect(paths::socket_path()?).await?;
    let (reader, mut writer) = stream.into_split();
    writer.write_all(b"{\"type\":\"subscribe\"}\n").await?;
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        match value["type"].as_str() {
            Some("event") => {
                let event = &value["event"];
                if event["kind"] == "output" {
                    let id = event["id"].as_u64().unwrap_or(0);
                    if !WATCHED.lock().unwrap().contains(&id) {
                        continue;
                    }
                }
                app.emit("ph-event", event)?
            }
            Some("ok") => app.emit("ph-connection", serde_json::json!({ "connected": true }))?,
            _ => {}
        }
    }
    bail!("the daemon closed the event stream")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![daemon, daemon_outdated, restart_daemon, watch_output])
        .setup(|app| {
            tauri::async_runtime::spawn(event_loop(app.handle().clone()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Philharmonic app");
}
