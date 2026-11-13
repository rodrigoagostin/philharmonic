//! What agents are doing: a log of actions, plus live details kept on each task
//! (current action, plan, subagents, token usage).

use serde::{Deserialize, Serialize};

use crate::task::TaskId;

/// One line in an agent's activity log, e.g. "Editing src/main.rs".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub task: TaskId,
    /// Unix seconds.
    pub at: u64,
    /// Set when a subagent did it (Claude Code's `agent_id`); `None` = the task's main agent.
    #[serde(default)]
    pub agent: Option<String>,
    pub kind: ActivityKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// The agent started, stopped or exited.
    Lifecycle,
    /// A prompt or message was given to the agent.
    Prompt,
    /// The agent used a tool (read, edit, run a command…).
    Tool,
    /// A subagent was launched or finished.
    Subagent,
    /// The agent is waiting for you.
    Waiting,
    /// The agent finished its turn (text = its final message).
    Finished,
}

/// A helper agent that Claude Code launched inside a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subagent {
    /// Claude Code's `agent_id` (before it starts: the id of the tool call that launches it).
    pub id: String,
    /// e.g. `general-purpose`, `Explore`.
    pub kind: String,
    /// What it was asked to do, in a few words.
    pub description: String,
    pub running: bool,
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// What it's doing right now.
    #[serde(default)]
    pub doing: Option<String>,
    #[serde(default)]
    pub tool_calls: u32,
    /// Start of its final answer.
    #[serde(default)]
    pub result: Option<String>,
    /// The prompt it was launched with (used to tell same-type subagents apart).
    #[serde(default)]
    pub prompt: Option<String>,
}

/// One item of the agent's own todo list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub text: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

/// Tokens used, from Claude Code's session transcripts (main agent + subagents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}
