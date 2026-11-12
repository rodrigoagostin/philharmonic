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

/// Subscribe to the daemon's events and forward them to the UI. Reconnects forever.
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
            Some("event") => app.emit("ph-event", &value["event"])?,
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
        .invoke_handler(tauri::generate_handler![daemon])
        .setup(|app| {
            tauri::async_runtime::spawn(event_loop(app.handle().clone()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Philharmonic app");
}
