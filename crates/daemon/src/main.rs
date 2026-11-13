//! `philharmonicd`: the long-running process that owns every task.
//!
//! It listens on a Unix socket. Each client connection is handled in its own
//! lightweight tokio task, so a slow (or crashing) client never blocks the others.

mod activity_log;
mod agent;
mod context_store;
mod daemon;
mod git;
mod handlers;
mod hooks;
mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use philharmonic_core::{Request, Response, paths};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::daemon::{Daemon, Shared};
use crate::state::State;

fn main() -> Result<()> {
    // Changing environment variables is only safe before other threads exist,
    // so this runs before the async runtime starts.
    adopt_login_shell_path();
    raise_open_file_limit();

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

    let exe = std::env::current_exe().ok();
    let (events, _) = broadcast::channel(4096);
    let state = State::load()?;
    let task_ids: Vec<_> = state.list().iter().map(|t| t.id).collect();
    let d: Shared = Arc::new(Daemon {
        state: Mutex::new(state),
        recent: Mutex::new(activity_log::load_recent(&task_ids)),
        procs: Mutex::new(HashMap::new()),
        terms: Mutex::new(HashMap::new()),
        events,
        ph_bin,
        claude_bin,
        exe_mtime: exe.as_deref().map(daemon::mtime).unwrap_or(0),
        exe_hash: exe.as_deref().map(philharmonic_core::fingerprint::file).unwrap_or(0),
        exe,
        dirty: Default::default(),
        shutdown: tokio::sync::Notify::new(),
    });
    d.save(); // persists any live → stopped changes from loading

    // Task changes are written to disk by this loop (see `Daemon::update_task`).
    let saver = Arc::clone(&d);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let d = Arc::clone(&saver);
            let _ = tokio::task::spawn_blocking(move || d.save_if_dirty()).await;
        }
    });

    tracing::info!("listening on {}", socket.display());
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        // `select!` waits for whichever happens first: a new client, Ctrl+C, or `kill`.
        tokio::select! {
            accepted = listener.accept() => {
                // An accept error (e.g. out of file handles) must not take the daemon down,
                // and every running agent with it: log it, back off a little, keep serving.
                let stream = match accepted {
                    Ok((stream, _addr)) => stream,
                    Err(e) => {
                        tracing::error!("accepting a connection: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let d = Arc::clone(&d);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, d).await {
                        tracing::debug!("connection closed: {e:#}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            _ = d.shutdown.notified() => break,
        }
    }

    // Clean shutdown: stop the agents and remove the socket file.
    let ids: Vec<_> = d.procs.lock().unwrap().keys().copied().collect();
    for id in ids {
        agent::kill(&d, id);
    }
    // Give the agents a moment to exit (they mark their tasks stopped), then save.
    for _ in 0..20 {
        if d.procs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // (an agent leaves `procs` just before it updates its task)
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    d.save();
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
            Ok(Request::Subscribe) => return stream_events(lines, writer, d).await,
            Ok(request) => {
                if !matches!(request, Request::Input { .. } | Request::Screen { .. } | Request::Resize { .. }) {
                    tracing::info!(?request, "request");
                }
                handlers::handle(request, &d).await
            }
            Err(e) => Response::Error { message: format!("bad request: {e}") },
        };
        write_line(&mut writer, &response).await?;
    }
    Ok(())
}

async fn stream_events(
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    d: Shared,
) -> Result<()> {
    let mut rx = d.events.subscribe();
    write_line(&mut writer, &Response::Ok).await?;
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => write_line(&mut writer, &Response::Event { event }).await?,
                // A slow client missed some events; keep going (it can re-sync with list calls).
                Err(broadcast::error::RecvError::Lagged(n)) => tracing::warn!("subscriber lagged by {n} events"),
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            // Notice right away when the client hangs up. Without this, a closed connection
            // stays open until the next event, and an idle daemon runs out of file handles.
            line = lines.next_line() => match line {
                Ok(Some(_)) => {} // subscribers don't send anything else; ignore it
                Ok(None) | Err(_) => return Ok(()),
            },
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

/// Apps started from the Finder/Dock get only 256 open files. Each agent, connection and
/// git call uses some, so ask for more (up to the hard limit).
fn raise_open_file_limit() {
    // SAFETY: getrlimit/setrlimit only read and write the struct we pass in.
    unsafe {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            // macOS refuses values above OPEN_MAX (10240) even when the hard limit is "unlimited".
            let wanted = limit.rlim_max.min(10_240);
            if limit.rlim_cur < wanted {
                limit.rlim_cur = wanted;
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
        }
    }
}

/// Apps started from the Dock get a minimal PATH, so `claude`, `gh`, etc. wouldn't be found.
/// Ask your login shell for its PATH and add it to ours.
fn adopt_login_shell_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = std::process::Command::new(&shell)
        .args(["-ilc", "printf '__PH__%s__PH__' \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else { return };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(login_path) = text.split("__PH__").nth(1) else { return };
    let current = std::env::var("PATH").unwrap_or_default();
    // SAFETY: called from `main` before any other thread is started.
    unsafe { std::env::set_var("PATH", format!("{current}:{login_path}")) };
}
