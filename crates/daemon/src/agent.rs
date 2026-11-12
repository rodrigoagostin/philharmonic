//! Running agents inside pseudo-terminals (PTYs).
//!
//! A PTY makes the agent believe it's in a real terminal window, so interactive programs
//! like Claude Code work normally. We read everything it prints (to show it in the app and
//! keep a history) and we can type into it.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{Agent, Event, Task, TaskId, TaskStatus, paths};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::daemon::Shared;

/// Max bytes of output we keep per task (older output is dropped).
const HISTORY_CAP: usize = 2 * 1024 * 1024;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// A live agent process.
pub struct Proc {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Set when *we* stop it, so an exit isn't reported as a failure.
    stopping: bool,
}

/// What the agent printed: raw bytes (to replay in a terminal widget) plus a parsed screen
/// (to show plain-text previews).
pub struct Terminal {
    history: Vec<u8>,
    parser: vt100::Parser,
}

impl Terminal {
    fn new() -> Terminal {
        Terminal { history: Vec::new(), parser: vt100::Parser::new(nz(DEFAULT_ROWS), nz(DEFAULT_COLS), 0) }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.history.extend_from_slice(bytes);
        if self.history.len() > HISTORY_CAP {
            let cut = self.history.len() - HISTORY_CAP;
            self.history.drain(..cut);
        }
    }

    /// The visible screen as text, without trailing blank lines.
    fn text(&self) -> String {
        let contents = self.parser.screen().contents();
        contents.trim_end().to_string()
    }
}

/// vt100 wants sizes that can't be zero.
fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

/// Prompts that appear before Claude Code's hooks are active (so no hook reports them).
fn looks_like_a_question(screen: &str) -> bool {
    const PROMPTS: [&str; 3] = ["trust this folder", "Do you trust the files", "one you trust?"];
    PROMPTS.iter().any(|p| screen.contains(p))
}

pub enum Start {
    /// First run: give the agent its prompt.
    Fresh { prompt: String },
    /// Run again in the same worktree, continuing the previous conversation.
    Resume,
}

/// Start the task's agent in a new PTY, in its worktree.
pub fn spawn(d: &Shared, task: &Task, start: Start) -> Result<()> {
    if d.procs.lock().unwrap().contains_key(&task.id) {
        bail!("task {} is already running", task.id);
    }

    let cmd = command_for(d, task, start)?;
    let pair = native_pty_system()
        .openpty(PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 })
        .context("opening a pseudo-terminal")?;
    let mut child = pair.slave.spawn_command(cmd).context("starting the agent")?;
    drop(pair.slave); // the child has its own handle now

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let killer = child.clone_killer();

    let id = task.id;
    d.terms.lock().unwrap().entry(id).or_insert_with(Terminal::new);
    d.procs.lock().unwrap().insert(id, Proc { writer, master: pair.master, killer, stopping: false });

    // Reading a PTY blocks, so it gets a plain OS thread rather than an async task.
    let reader_daemon = d.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break, // the agent closed its terminal
                Ok(n) => {
                    let chunk = &buf[..n];
                    let asking = match reader_daemon.terms.lock().unwrap().get_mut(&id) {
                        Some(term) => {
                            term.feed(chunk);
                            looks_like_a_question(&term.text())
                        }
                        None => false,
                    };
                    reader_daemon.emit(Event::Output { id, data: B64.encode(chunk) });
                    if asking {
                        let status = reader_daemon.task(id).map(|t| t.status);
                        if matches!(status, Some(TaskStatus::Queued | TaskStatus::Running)) {
                            reader_daemon.update_task(id, |t| t.status = TaskStatus::NeedsInput);
                        }
                    }
                }
            }
        }
    });

    // Another thread waits for the process to exit and updates the task.
    let waiter_daemon = d.clone();
    std::thread::spawn(move || {
        let exit = child.wait();
        let stopped_by_us = waiter_daemon
            .procs
            .lock()
            .unwrap()
            .remove(&id)
            .map(|p| p.stopping)
            .unwrap_or(true);
        let success = exit.map(|s| s.success()).unwrap_or(false);
        waiter_daemon.update_task(id, |t| {
            // Don't overwrite a status we set ourselves (done, discarded…).
            if t.status.is_live() {
                t.status = if success || stopped_by_us { TaskStatus::Stopped } else { TaskStatus::Failed };
            }
        });
        tracing::info!(task = id, success, "agent exited");
    });

    Ok(())
}

fn command_for(d: &Shared, task: &Task, start: Start) -> Result<CommandBuilder> {
    let mut cmd = match &task.agent {
        Agent::Claude => {
            let mut cmd = CommandBuilder::new(&d.claude_bin);
            cmd.arg("--settings");
            cmd.arg(hook_settings(&d.ph_bin));
            if let Some(model) = &task.model {
                cmd.args(["--model", model]);
            }
            if task.auto_approve {
                cmd.args(["--permission-mode", "acceptEdits"]);
            }
            match start {
                Start::Fresh { prompt } => cmd.arg(prompt),
                Start::Resume => cmd.arg("--continue"),
            }
            cmd
        }
        Agent::Custom { command } => {
            let mut cmd = CommandBuilder::new("sh");
            cmd.args(["-c", command]);
            if let Start::Fresh { prompt } = start {
                cmd.env("PH_PROMPT", prompt);
            }
            cmd
        }
    };
    cmd.cwd(&task.worktree);
    // If the daemon was started from inside a Claude Code session, don't let the agent
    // think it's a child of that session.
    for (key, _) in std::env::vars() {
        if key.starts_with("CLAUDE_CODE_") || key == "CLAUDECODE" {
            cmd.env_remove(key);
        }
    }
    // Hooks (`ph notify`) use these to find the task and the daemon.
    cmd.env("PH_TASK_ID", task.id.to_string());
    cmd.env("PHILHARMONIC_HOME", paths::data_dir()?);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    Ok(cmd)
}

/// Claude Code settings (passed with `--settings`, so nothing is written into your repo)
/// that call `ph notify` on each status change.
fn hook_settings(ph: &Path) -> String {
    let hook = |event: &str| {
        serde_json::json!([{ "hooks": [{ "type": "command", "command": format!("'{}' notify {event}", ph.display()) }] }])
    };
    let mut post_tool = hook("running");
    post_tool[0]["matcher"] = serde_json::json!("*");
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook("running"),
            "PostToolUse": post_tool,
            "Notification": hook("needs_input"),
            "Stop": hook("stopped"),
        }
    })
    .to_string()
}

// ---- talking to a running agent ----

pub fn write(d: &Shared, id: TaskId, bytes: &[u8]) -> Result<()> {
    let mut procs = d.procs.lock().unwrap();
    let proc = procs.get_mut(&id).context("the agent isn't running (resume it first)")?;
    proc.writer.write_all(bytes)?;
    proc.writer.flush()?;
    Ok(())
}

pub fn resize(d: &Shared, id: TaskId, cols: u16, rows: u16) -> Result<()> {
    if let Some(proc) = d.procs.lock().unwrap().get(&id) {
        proc.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
    }
    if let Some(term) = d.terms.lock().unwrap().get_mut(&id) {
        term.parser.screen_mut().set_size(nz(rows), nz(cols));
    }
    Ok(())
}

/// (history as base64, visible text)
pub fn screen(d: &Shared, id: TaskId) -> (String, String) {
    match d.terms.lock().unwrap().get(&id) {
        Some(term) => (B64.encode(&term.history), term.text()),
        None => (String::new(), String::new()),
    }
}

/// Type a message the way a person pastes it, then press Enter.
pub async fn send_message(d: &Shared, id: TaskId, text: &str) -> Result<()> {
    // Bracketed paste: the agent receives the text as one paste, newlines included,
    // instead of submitting at the first newline.
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend_from_slice(text.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    write(d, id, &paste)?;
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    write(d, id, b"\r")
}

/// Ask the agent's process to stop.
pub fn kill(d: &Shared, id: TaskId) -> bool {
    match d.procs.lock().unwrap().get_mut(&id) {
        Some(proc) => {
            proc.stopping = true;
            let _ = proc.killer.kill();
            true
        }
        None => false,
    }
}

pub fn forget_terminal(d: &Shared, id: TaskId) {
    d.terms.lock().unwrap().remove(&id);
}
