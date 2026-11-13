use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::activity::{PlanItem, Subagent, Usage};

/// Tasks get small, human-friendly numbers (`ph send 3 "..."`) instead of UUIDs.
pub type TaskId = u64;

/// One unit of work: an agent running in its own git worktree and branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// Short name shown in lists (derived from the prompt).
    pub title: String,
    /// The full instruction given to the agent.
    pub prompt: String,
    /// The repo the task belongs to (the main checkout, never modified).
    pub repo: PathBuf,
    /// Branch the task started from, e.g. `main`.
    pub base: String,
    /// Branch created for this task, e.g. `ph/add-rate-limiting`.
    pub branch: String,
    /// Where the worktree lives, e.g. `~/.philharmonic/wt/api/add-rate-limiting`.
    pub worktree: PathBuf,
    pub agent: Agent,
    /// Model override passed to the agent (`None` = the agent's default).
    #[serde(default)]
    pub model: Option<String>,
    /// Let the agent edit files without asking (Claude Code `--permission-mode acceptEdits`).
    #[serde(default)]
    pub auto_approve: bool,
    pub status: TaskStatus,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: u64,
    /// Has the agent received its first prompt (a Claude Code hook fired)? If not, a
    /// resume gives it the prompt again instead of `--continue`-ing a conversation
    /// that doesn't exist. Tasks saved before this field existed count as started.
    #[serde(default = "yes")]
    pub started: bool,
    /// The Claude Code conversation (session id). Resuming uses `--resume <id>`, so the
    /// right conversation comes back even if a folder has several.
    #[serde(default)]
    pub session: Option<String>,
    /// Imported from an existing Claude Code thread: the folder and branch are yours, not
    /// Philharmonic's, so discarding the task only forgets it (nothing is deleted).
    #[serde(default)]
    pub adopted: bool,
    /// Collections whose context this task uses (see `context::Collection`).
    #[serde(default)]
    pub collections: Vec<crate::context::CollectionId>,

    // ---- live details, reported by Claude Code hooks ----
    /// What the main agent is doing right now, e.g. "Editing src/main.rs".
    #[serde(default)]
    pub doing: Option<String>,
    /// The agent's own todo list.
    #[serde(default)]
    pub plan: Vec<PlanItem>,
    /// Helper agents it launched.
    #[serde(default)]
    pub subagents: Vec<Subagent>,
    #[serde(default)]
    pub usage: Usage,
    /// Tool calls made (main agent + subagents).
    #[serde(default)]
    pub tool_calls: u32,
    /// Start of the agent's last final message: what it says it did.
    #[serde(default)]
    pub summary: Option<String>,
    /// Unix seconds of the last thing it did.
    #[serde(default)]
    pub last_activity: u64,
}

fn yes() -> bool {
    true
}

/// Which program runs inside the task's terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Agent {
    /// Claude Code (`claude`). Reports its status through hooks.
    Claude,
    /// Any shell command. The prompt is in `$PH_PROMPT`. Status is only running/stopped.
    Custom { command: String },
}

/// The lifecycle of a task. Mirrors the groups in the Stage sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    NeedsInput,
    Review,
    /// The agent process isn't running (it exited, was stopped, or the daemon restarted).
    Stopped,
    Done,
    Failed,
}

impl TaskStatus {
    /// Is an agent process expected to be alive?
    pub fn is_live(self) -> bool {
        matches!(self, TaskStatus::Queued | TaskStatus::Running | TaskStatus::NeedsInput | TaskStatus::Review)
    }
}

// `Display` is what `println!("{}", status)` uses. `Debug` (`{:?}`) is derived above.
impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::NeedsInput => "needs you",
            TaskStatus::Review => "review",
            TaskStatus::Stopped => "stopped",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        };
        // `pad` (instead of `write_str`) respects width/alignment like `{:<11}`.
        f.pad(s)
    }
}
