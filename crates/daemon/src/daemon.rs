//! The one object every connection and background thread shares.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use philharmonic_core::{Activity, Event, Task, TaskId};
use tokio::sync::broadcast;

use crate::agent::{Proc, Terminal};
use crate::state::State;

pub type Shared = Arc<Daemon>;

pub struct Daemon {
    /// Tasks and context items (saved to disk).
    pub state: Mutex<State>,
    /// The latest activity of every agent (the full logs are on disk).
    pub recent: Mutex<VecDeque<Activity>>,
    /// Running agent processes, by task.
    pub procs: Mutex<HashMap<TaskId, Proc>>,
    /// Terminal output per task. Kept after the process exits, so you can still read it.
    pub terms: Mutex<HashMap<TaskId, Terminal>>,
    /// Fan-out of events to every subscribed client.
    pub events: broadcast::Sender<Event>,
    /// Absolute path of the `ph` binary (Claude Code hooks call `ph notify`).
    pub ph_bin: PathBuf,
    /// The Claude Code executable.
    pub claude_bin: String,
    /// This daemon's own binary and its modification time at startup (to detect updates).
    pub exe: Option<PathBuf>,
    pub exe_mtime: u64,
    pub exe_hash: u64,
    /// Set when `state` changed and isn't on disk yet (see `save_soon`).
    pub dirty: std::sync::atomic::AtomicBool,
    /// Signalled by a `Shutdown` request.
    pub shutdown: tokio::sync::Notify,
}

/// Modification time of a file, in Unix seconds (0 if unknown).
pub fn mtime(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Lock order, to avoid deadlocks: never lock `state` while holding `procs` or `terms`.
// Every method below takes one lock at a time.

impl Daemon {
    pub fn emit(&self, event: Event) {
        // An error only means nobody is subscribed right now.
        let _ = self.events.send(event);
    }

    /// Change a task, save, and tell subscribers. Returns the updated task.
    pub fn update_task(&self, id: TaskId, change: impl FnOnce(&mut Task)) -> Option<Task> {
        let task = {
            let mut state = self.state.lock().unwrap();
            let task = state.get_mut(id)?;
            change(task);
            task.clone()
        };
        // Agents report every tool call; writing all tasks to disk each time would get slower
        // as the session grows. A background loop writes at most twice a second.
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        self.emit(Event::TaskUpdated { task: task.clone() });
        Some(task)
    }

    pub fn task(&self, id: TaskId) -> Option<Task> {
        self.state.lock().unwrap().get(id).cloned()
    }

    /// Write the state if something changed since the last write.
    pub fn save_if_dirty(&self) {
        if self.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.save();
        }
    }

    pub fn save(&self) {
        if let Err(e) = self.state.lock().unwrap().save() {
            tracing::error!("saving state: {e:#}");
        }
    }
}
