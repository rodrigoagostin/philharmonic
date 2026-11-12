//! The messages clients and the daemon exchange over the Unix socket.
//!
//! Wire format: one JSON object per line ("newline-delimited JSON"). For example:
//!
//! ```text
//! -> {"type":"ping"}
//! <- {"type":"pong","version":"0.1.0"}
//! ```
//!
//! `#[serde(tag = "type")]` is what puts the variant name in a `"type"` field.
//!
//! Most requests get exactly one response. `Subscribe` is different: after it, the daemon
//! keeps sending `Response::Event` lines on that connection until the client hangs up.
//! Binary data (terminal bytes, images) travels as base64 strings.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::{ContextId, ContextItem, ContextKind, Scope};
use crate::task::{Agent, Task, TaskId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Health check: is the daemon alive?
    Ping,

    // ---- tasks ----
    /// Create a worktree + branch and start the agent in it.
    NewTask {
        prompt: String,
        /// Absolute path to the repo's main checkout.
        repo: PathBuf,
        /// Branch to start from, e.g. `main`.
        base: String,
        #[serde(default = "default_agent")]
        agent: Agent,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        auto_approve: bool,
        /// Context items to attach to the first prompt (they move to the task's scope).
        #[serde(default)]
        context: Vec<ContextId>,
    },
    ListTasks,
    /// Kill the agent process (the worktree stays).
    Stop { id: TaskId },
    /// Start the agent again in the same worktree (Claude Code: `--continue`).
    Resume { id: TaskId },
    /// Stop the agent, delete the worktree and the branch, forget the task.
    Discard { id: TaskId },
    /// Mark a task as done (after you merged or opened a PR yourself).
    MarkDone { id: TaskId },

    // ---- terminal ----
    /// Raw keystrokes for the agent's terminal (base64).
    Input { id: TaskId, data: String },
    Resize { id: TaskId, cols: u16, rows: u16 },
    /// Current terminal contents: full byte history (to replay) + visible text.
    Screen { id: TaskId },
    /// Just the visible text (cheap: no history). For previews.
    ScreenText { id: TaskId },
    /// Type a message into the agent (pasted, then Enter), with context items attached.
    Send {
        id: TaskId,
        text: String,
        #[serde(default)]
        context: Vec<ContextId>,
    },

    // ---- from Claude Code hooks (via `ph notify`) ----
    Notify {
        id: TaskId,
        event: HookEvent,
        #[serde(default)]
        message: String,
    },

    // ---- review & ship ----
    Diff { id: TaskId },
    Ship { id: TaskId, how: ShipMethod },

    // ---- context shelf ----
    AddContext {
        scope: Scope,
        kind: ContextKind,
        /// File name (images/files) or title.
        name: String,
        /// File contents, base64 (images/files).
        #[serde(default)]
        data: Option<String>,
        /// The URL (links) or the text (notes).
        #[serde(default)]
        text: Option<String>,
    },
    ListContext,
    MoveContext { id: ContextId, scope: Scope },
    RemoveContext { id: ContextId },

    // ---- misc ----
    /// Local branches of a repo (for the base-branch picker).
    Branches { repo: PathBuf },
    /// Stream events on this connection from now on.
    Subscribe,
}

fn default_agent() -> Agent {
    Agent::Claude
}

/// What a Claude Code hook reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// The agent is working (a prompt was submitted, or a tool finished).
    Running,
    /// The agent is waiting for you (a permission prompt or a question).
    NeedsInput,
    /// The agent finished its turn.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipMethod {
    /// Commit, push the branch and open a GitHub pull request (`gh`).
    Pr,
    /// Commit and merge the branch into the base branch of the main checkout.
    Merge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong { version: String },
    Ok,
    TaskCreated { task: Task },
    Task { task: Task },
    Tasks { tasks: Vec<Task> },
    Screen {
        /// Every byte the agent printed (capped), base64, to replay in a terminal widget.
        history: String,
        /// The visible screen as plain text, trailing blank lines removed.
        text: String,
        /// Stream position right after the last byte of `history` (see `Event::Output::offset`).
        #[serde(default)]
        end: u64,
    },
    ScreenText { text: String },
    Diff {
        files: Vec<FileChange>,
        /// Unified diff against the point where the branch left `base`.
        patch: String,
    },
    Shipped { message: String },
    ContextAdded { item: ContextItem },
    Context { items: Vec<ContextItem> },
    Branches { names: Vec<String> },
    Event { event: Event },
    Error { message: String },
}

/// Pushed to subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    TaskUpdated { task: Task },
    TaskRemoved { id: TaskId },
    /// New terminal output (base64). `offset` is the position of its first byte in the
    /// task's output stream, so clients can drop bytes they already have and notice gaps.
    Output {
        id: TaskId,
        data: String,
        #[serde(default)]
        offset: u64,
    },
    ContextChanged,
    /// This subscriber missed events: reload everything.
    Resync,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_wire_format() {
        let json = serde_json::to_string(&Request::Ping).unwrap();
        assert_eq!(json, r#"{"type":"ping"}"#);
    }

    #[test]
    fn new_task_defaults_to_claude() {
        let r: Request = serde_json::from_str(
            r#"{"type":"new_task","prompt":"x","repo":"/r","base":"main"}"#,
        )
        .unwrap();
        match r {
            Request::NewTask { agent, auto_approve, context, .. } => {
                assert_eq!(agent, Agent::Claude);
                assert!(!auto_approve);
                assert!(context.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }
}
