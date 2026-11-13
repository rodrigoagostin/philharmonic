//! Types shared by the daemon (`philharmonicd`) and every client (`ph`, the Tauri app).
//!
//! Keeping these in their own crate means the daemon and the clients can never disagree
//! about what a `Task` or a `Request` looks like: if one changes, both recompile.

pub mod activity;
pub mod context;
pub mod fingerprint;
pub mod paths;
pub mod protocol;
pub mod slug;
pub mod task;

pub use activity::{Activity, ActivityKind, PlanItem, Subagent, Usage};
pub use context::{Collection, CollectionId, ContextId, ContextItem, ContextKind, Scope};
pub use protocol::{Event, FileChange, Request, Response, ShipMethod};
pub use task::{Agent, Task, TaskId, TaskStatus};

/// Seconds since 1970 (for `created_at` fields).
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
