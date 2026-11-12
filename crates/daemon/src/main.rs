//! `philharmonicd`: the long-running process that owns every task.
//!
//! It listens on a Unix socket. Each client connection is handled in its own
//! lightweight tokio task, so a slow (or crashing) client never blocks the others.

mod agent;
mod context_store;
mod daemon;
mod git;
mod handlers;
mod sessions;
mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use philharmonic_core::{Event, Request, Response, paths};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::daemon::{Daemon, Shared};
use crate::state::State;

fn main() -> Result<()> {
    // Changing environment variables is only safe before other threads exist,
    // so this runs before the async runtime starts.
    adopt_login_shell_path();

    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = paths::data_dir()?;
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;

    // A socket file left behind by a previous run would make `bind` fail.
    let socket = paths::socket_path()?;
    if socket.exists() {
        // ...unless another daemon is actually using it.
        if UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("another philharmonicd is already running on {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;

    let ph_bin = sibling_binary("ph");
    if !ph_bin.exists() {
        tracing::warn!(
            "{} not found: agent status updates won't work. Build it with `cargo build -p ph`",
            ph_bin.display()
        );
    }
    let claude_bin = which("claude").unwrap_or_else(|| "claude".to_string());

    let (events, _) = broadcast::channel(4096);
    let d: Shared = Arc::new(Daemon {
        state: Mutex::new(State::load()?),
        procs: Mutex::new(HashMap::new()),
        terms: Mutex::new(HashMap::new()),
        events,
        ph_bin,
        claude_bin,
    });
    d.save(); // persists any live → stopped changes from loading

    tracing::info!("listening on {}", socket.display());
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        // `select!` waits for whichever happens first: a new client, Ctrl+C, or `kill`.
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let d = Arc::clone(&d);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, d).await {
                            tracing::debug!("connection closed: {e:#}");
                        }
                    });
                }
                // One failed connection (e.g. too many open files) must not take the whole
                // daemon down. Pause briefly so a persistent error doesn't spin the CPU.
                Err(e) => {
                    tracing::warn!("accepting a connection: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
    }

    // Clean shutdown: stop the agents and remove the socket file.
    let ids: Vec<_> = d.procs.lock().unwrap().keys().copied().collect();
    for id in ids {
        agent::kill(&d, id);
    }
    let _ = std::fs::remove_file(&socket);
    tracing::info!("bye");
    Ok(())
}

/// Read one JSON request per line, answer each with one JSON response per line.
/// After a `subscribe` request, stream events instead.
async fn handle_connection(stream: UnixStream, d: Shared) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Subscribe) => return stream_events(writer, d).await,
            Ok(request) => {
                log_request(&request);
                handlers::handle(request, &d).await
            }
            Err(e) => Response::Error { message: format!("bad request: {e}") },
        };
        write_line(&mut writer, &response).await?;
    }
    Ok(())
}

/// Log requests, but not the very frequent ones, and never file contents (a pasted
/// screenshot is megabytes of base64).
fn log_request(request: &Request) {
    match request {
        Request::Input { .. } | Request::Screen { .. } | Request::ScreenText { .. } | Request::Resize { .. } => {}
        Request::AddContext { scope, kind, name, .. } => tracing::info!(?scope, ?kind, name, "request: add_context"),
        other => tracing::info!(request = ?other, "request"),
    }
}

async fn stream_events(mut writer: tokio::net::unix::OwnedWriteHalf, d: Shared) -> Result<()> {
    let mut rx = d.events.subscribe();
    write_line(&mut writer, &Response::Ok).await?;
    loop {
        match rx.recv().await {
            Ok(event) => write_line(&mut writer, &Response::Event { event }).await?,
            // A slow client missed some events: tell it to reload.
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("subscriber lagged by {n} events");
                write_line(&mut writer, &Response::Event { event: Event::Resync }).await?;
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn write_line(writer: &mut tokio::net::unix::OwnedWriteHalf, response: &Response) -> Result<()> {
    let mut out = serde_json::to_string(response)?;
    out.push('\n');
    writer.write_all(out.as_bytes()).await?;
    Ok(())
}

/// A binary that sits next to this one (`target/debug/ph`, or inside the app bundle).
fn sibling_binary(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Find an executable on PATH.
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Apps started from the Dock get a minimal PATH, so `claude`, `gh`, etc. wouldn't be found.
/// Ask your login shell for its PATH and add it to ours. Gives up after a few seconds, in
/// case your shell startup files wait for something.
fn adopt_login_shell_path() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let child = Command::new(&shell)
        .args(["-ilc", "printf '__PH__%s__PH__' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                eprintln!("your login shell took too long to print its PATH; using the current PATH");
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    let text = String::from_utf8_lossy(&stdout);
    let Some(login_path) = text.split("__PH__").nth(1) else { return };
    let current = std::env::var("PATH").unwrap_or_default();
    // SAFETY: called from `main` before any other thread is started.
    unsafe { std::env::set_var("PATH", format!("{current}:{login_path}")) };
}
