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

use crate::activity::Activity;
use crate::context::{Collection, CollectionId, ContextId, ContextItem, ContextKind, Scope};
use crate::task::{Agent, Task, TaskId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Health check: is the daemon alive? (and which build is it running?)
    Ping,
    /// Stop every agent and exit (used to restart the daemon after an update).
    Shutdown,

    // ---- tasks ----
    /// Create a worktree + branch and start the agent in it.
    NewTask {
        prompt: String,
        /// Absolute path to the repo's main checkout.
        repo: PathBuf,
        /// Branch to start from, e.g. `main`.
        base: String,
        /// Name of the new branch (e.g. `feature/ABC-123-login`). Empty: `ph/<from the prompt>`.
        #[serde(default)]
        branch: Option<String>,
        #[serde(default = "default_agent")]
        agent: Agent,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        auto_approve: bool,
        /// Context items to attach to the first prompt (they move to the task's scope).
        #[serde(default)]
        context: Vec<ContextId>,
        /// Collections the task uses. Missing: the ones linked to `repo`.
        #[serde(default)]
        collections: Option<Vec<CollectionId>>,
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
    /// `history: false` skips the history (it can be megabytes; previews only need the text).
    Screen {
        id: TaskId,
        #[serde(default = "yes")]
        history: bool,
    },
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
    /// A hook fired inside a task's agent. `payload` is the JSON Claude Code gave the hook
    /// (`hook_event_name`, `tool_name`, `tool_input`, `agent_id`, …).
    Hook { id: TaskId, payload: serde_json::Value },
    /// The activity log: one task's (oldest first), or every task's most recent.
    Activity {
        #[serde(default)]
        id: Option<TaskId>,
        #[serde(default = "default_limit")]
        limit: usize,
    },

    // ---- review & ship ----
    /// `patch: false` = only the list of changed files (cheap, for live panels).
    Diff {
        id: TaskId,
        #[serde(default = "yes")]
        patch: bool,
    },
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

    // ---- existing Claude Code threads ----
    /// Find the Claude Code conversations already started in a repo (its main checkout,
    /// its worktrees and their subfolders).
    Discover { repo: PathBuf },
    /// Turn found conversations into (stopped) tasks you can resume. Ids from `Discover`.
    Import { repo: PathBuf, sessions: Vec<String> },

    // ---- collections ----
    ListCollections,
    /// Create (no `id`) or rename / relink a collection.
    SaveCollection {
        #[serde(default)]
        id: Option<CollectionId>,
        name: String,
        #[serde(default)]
        repos: Vec<PathBuf>,
    },
    /// Delete a collection and its items (tasks stop using it).
    DeleteCollection { id: CollectionId },
    /// Which collections a task uses. A running agent hears about added ones with your
    /// next message.
    SetTaskCollections { id: TaskId, collections: Vec<CollectionId> },

    // ---- misc ----
    /// Local branches of a repo (for the base-branch picker).
    Branches { repo: PathBuf },
    /// Stream events on this connection from now on.
    Subscribe,
}

fn default_agent() -> Agent {
    Agent::Claude
}

fn yes() -> bool {
    true
}

fn default_limit() -> usize {
    300
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipMethod {
    /// Commit, push the branch and open a GitHub pull request (`gh`).
    Pr,
    /// Commit and merge the branch into the base branch of the main checkout.
    Merge,
}

/// A Claude Code conversation found on disk (see `Request::Discover`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundSession {
    /// Claude Code's session id (the `.jsonl` file name).
    pub id: String,
    /// The folder it runs in.
    pub cwd: PathBuf,
    /// The branch checked out there now.
    pub branch: String,
    /// Claude's title for it, else the first prompt (shortened).
    pub title: String,
    /// The first thing you asked.
    pub prompt: String,
    /// How many prompts you typed.
    pub prompts: u32,
    /// Last activity, Unix seconds.
    pub updated_at: u64,
    /// Open right now in another terminal (resuming it here would run it twice).
    pub open_elsewhere: bool,
    /// Already a task here.
    pub task: Option<TaskId>,
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
    Pong {
        version: String,
        /// The daemon's process id.
        #[serde(default)]
        pid: u32,
        /// The binary it runs, and that file's modification time (Unix seconds) when it
        /// started. If the file on disk is newer now, this daemon is running an old build.
        #[serde(default)]
        exe: Option<PathBuf>,
        #[serde(default)]
        exe_mtime: u64,
        /// Fingerprint of that binary's contents at startup (see `fingerprint::file`).
        /// 0 from builds before this field existed.
        #[serde(default)]
        exe_hash: u64,
    },
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
        /// The terminal's current size. Replay `history` at this size: the agent's redraws
        /// (cursor moves) only line up at the size they were drawn at.
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
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
    Collection { collection: Collection },
    Collections { collections: Vec<Collection> },
    Branches { names: Vec<String> },
    Discovered { sessions: Vec<FoundSession> },
    Activity { items: Vec<Activity> },
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
    /// Context items or collections changed: reload them.
    ContextChanged,
    /// This subscriber missed events: reload everything.
    Resync,
    /// Something an agent did (also kept in the task's activity log).
    Activity { item: Activity },
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
