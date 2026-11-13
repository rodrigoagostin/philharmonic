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
use philharmonic_core::{ActivityKind, Agent, CollectionId, Event, Scope, Task, TaskId, TaskStatus, paths};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::activity_log::record;
use crate::context_store;
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
    /// Collections this agent was told about (at start, or with a later message).
    briefed: Vec<CollectionId>,
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
        // Trim in batches (when 25% over the cap), not on every chunk: dropping the front
        // moves everything after it, and a busy agent sends many small chunks a second.
        if self.history.len() > HISTORY_CAP + HISTORY_CAP / 4 {
            let over = self.history.len() - HISTORY_CAP;
            // Cut just after a newline, so the replay doesn't start inside an escape sequence.
            let cut = self.history[over..].iter().position(|&b| b == b'\n').map_or(over, |i| over + i + 1);
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
    let briefed = task.collections.clone();
    d.procs.lock().unwrap().insert(id, Proc { writer, master: pair.master, killer, stopping: false, briefed });

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

    record(d, id, None, ActivityKind::Lifecycle, format!("Started {}", match &task.agent {
        Agent::Claude => "Claude Code".to_string(),
        Agent::Custom { command } => format!("`{command}`"),
    }));

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
            // Nothing is running any more.
            t.doing = None;
            for s in &mut t.subagents {
                if s.running {
                    s.running = false;
                    s.doing = None;
                }
            }
        });
        let how = if stopped_by_us { "Stopped" } else if success { "Agent exited" } else { "Agent crashed" };
        record(&waiter_daemon, id, None, ActivityKind::Lifecycle, how);
        tracing::info!(task = id, success, "agent exited");
    });

    Ok(())
}

fn command_for(d: &Shared, task: &Task, start: Start) -> Result<CommandBuilder> {
    // Its context: the collections the task uses, plus the task's own folder (screenshots
    // pasted into its message box). `--add-dir` lets the agent read them without asking.
    let brief = context_store::brief(d, &task.collections, CONTEXT_INTRO);
    let mut dirs = brief.as_ref().map(|(_, dirs)| dirs.clone()).unwrap_or_default();
    if let Ok(own) = context_store::scope_dir(&Scope::Task { id: task.id }) {
        let _ = std::fs::create_dir_all(&own);
        dirs.push(own);
    }
    let mut cmd = match &task.agent {
        Agent::Claude => {
            let mut cmd = CommandBuilder::new(&d.claude_bin);
            // What Claude Code remembers per folder (MCP servers you added "for this project",
            // which `.mcp.json` servers you approved) is keyed by your checkout's path. The
            // worktree is another path, so hand those over explicitly.
            let local = project_entry(&task.repo);
            let mut settings = hook_settings(&d.ph_bin);
            for key in ["enabledMcpjsonServers", "disabledMcpjsonServers"] {
                if let Some(list) = local.get(key).filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
                    settings[key] = list.clone();
                }
            }
            cmd.arg("--settings");
            cmd.arg(settings.to_string());
            if let Some(servers) = local.get("mcpServers").filter(|v| v.as_object().is_some_and(|o| !o.is_empty())) {
                // `--mcp-config` takes several values, so use `=`: otherwise it would also
                // swallow the prompt that comes after it.
                cmd.arg(format!("--mcp-config={}", serde_json::json!({ "mcpServers": servers })));
            }
            if let Some(model) = &task.model {
                cmd.args(["--model", model]);
            }
            if task.auto_approve {
                cmd.args(["--permission-mode", "acceptEdits"]);
            }
            // `=`: both options take several values and would swallow what follows.
            for dir in &dirs {
                cmd.arg(format!("--add-dir={}", dir.display()));
            }
            if let Some((text, _)) = &brief {
                cmd.arg(format!("--append-system-prompt={text}"));
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
            // The same context, for whatever the command wants to do with it.
            if let Some((text, _)) = &brief {
                cmd.env("PH_CONTEXT", text);
            }
            cmd.env("PH_CONTEXT_DIRS", std::env::join_paths(&dirs).unwrap_or_default());
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
    if task.agent == Agent::Claude {
        // Print the conversation as normal terminal output instead of a full-screen view.
        // Scrolling then happens in the app's own terminal (instant), instead of every wheel
        // tick and mouse movement being sent to Claude Code, which redraws for each one.
        cmd.env("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1");
        cmd.env("CLAUDE_CODE_DISABLE_MOUSE", "1");
    }
    // Hooks (`ph notify`) use these to find the task and the daemon.
    cmd.env("PH_TASK_ID", task.id.to_string());
    cmd.env("PHILHARMONIC_HOME", paths::data_dir()?);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    Ok(cmd)
}

/// Opens the brief about the task's collections (see `context_store::brief`).
const CONTEXT_INTRO: &str = "# Context for this task (from Philharmonic)\n\
The user runs several tasks at once, across different projects and features. This task \
uses the collections below: material the user gathered for it (docs, designs, \
screenshots, links, notes). Open a file when it's relevant to what you're doing. Only \
these belong to this task. Their folders can get new files while you work.";

/// Claude Code settings (passed with `--settings`, so nothing is written into your repo)
/// that call `ph notify` on each status change.
fn hook_settings(ph: &Path) -> serde_json::Value {
    // Every hook runs the same command; `ph notify` forwards the hook's JSON to the daemon,
    // which looks at `hook_event_name`.
    let command = format!("'{}' notify", ph.display());
    let hook = |matcher: bool| {
        let mut h = serde_json::json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 10 }] }]);
        if matcher {
            h[0]["matcher"] = serde_json::json!("*");
        }
        h
    };
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook(false),
            "PreToolUse": hook(true),
            "PostToolUse": hook(true),
            "SubagentStart": hook(false),
            "SubagentStop": hook(false),
            "Notification": hook(false),
            "Stop": hook(false),
        }
    })
}

/// Claude Code's per-folder entry for `repo` in its config file (`~/.claude.json`, or
/// `$CLAUDE_CONFIG_DIR/.claude.json`). `Null` if there's none.
fn project_entry(repo: &Path) -> serde_json::Value {
    let file = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => std::path::PathBuf::from(dir).join(".claude.json"),
        None => match std::env::var_os("HOME") {
            Some(home) => std::path::PathBuf::from(home).join(".claude.json"),
            None => return serde_json::Value::Null,
        },
    };
    let config: serde_json::Value = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    config["projects"][repo.to_string_lossy().as_ref()].clone()
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

/// (history as base64, visible text, cols, rows)
pub fn screen(d: &Shared, id: TaskId, with_history: bool) -> (String, String, u16, u16) {
    match d.terms.lock().unwrap().get(&id) {
        Some(term) => {
            let (rows, cols) = term.parser.screen().size();
            let history = if with_history { B64.encode(&term.history) } else { String::new() };
            (history, term.text(), cols.get(), rows.get())
        }
        None => (String::new(), String::new(), DEFAULT_COLS, DEFAULT_ROWS),
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

/// Of `collections`, the ones the task's running agent hasn't been told about.
pub fn unbriefed(d: &Shared, id: TaskId, collections: &[CollectionId]) -> Vec<CollectionId> {
    match d.procs.lock().unwrap().get(&id) {
        Some(p) => collections.iter().copied().filter(|c| !p.briefed.contains(c)).collect(),
        None => Vec::new(),
    }
}

pub fn mark_briefed(d: &Shared, id: TaskId, collections: &[CollectionId]) {
    if let Some(p) = d.procs.lock().unwrap().get_mut(&id) {
        p.briefed.extend_from_slice(collections);
    }
}

pub fn forget_terminal(d: &Shared, id: TaskId) {
    d.terms.lock().unwrap().remove(&id);
}
