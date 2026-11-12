//! The one object every connection and background thread shares.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use philharmonic_core::{Event, Task, TaskId};
use tokio::sync::broadcast;

use crate::agent::{Proc, Terminal};
use crate::state::State;

pub type Shared = Arc<Daemon>;

pub struct Daemon {
    /// Tasks and context items (saved to disk).
    pub state: Mutex<State>,
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
            let task = task.clone();
            if let Err(e) = state.save() {
                tracing::error!("saving state: {e:#}");
            }
            task
        };
        self.emit(Event::TaskUpdated { task: task.clone() });
        Some(task)
    }

    pub fn task(&self, id: TaskId) -> Option<Task> {
        self.state.lock().unwrap().get(id).cloned()
    }

    pub fn save(&self) {
        if let Err(e) = self.state.lock().unwrap().save() {
            tracing::error!("saving state: {e:#}");
        }
    }
}
