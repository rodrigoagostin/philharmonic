//! What the daemon remembers: tasks and context items. Saved to `state.json` after every
//! change, and loaded at startup, so a daemon restart loses nothing but running processes.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use philharmonic_core::{ContextId, ContextItem, Task, TaskId, TaskStatus, paths};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Last id handed out. The first task gets 1.
    last_id: TaskId,
    /// `BTreeMap` keeps tasks sorted by id, so listing them is already in order.
    tasks: BTreeMap<TaskId, Task>,
    #[serde(default)]
    last_context_id: ContextId,
    #[serde(default)]
    context: BTreeMap<ContextId, ContextItem>,
}

impl State {
    /// Read `state.json` (or start empty). Agents don't survive a restart, so any task that
    /// was live is marked `Stopped`; you can resume it from the app.
    pub fn load() -> Result<State> {
        let path = paths::state_file()?;
        let mut state: State = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        for task in state.tasks.values_mut() {
            if task.status.is_live() {
                task.status = TaskStatus::Stopped;
            }
        }
        Ok(state)
    }

    /// Write to a temp file, then rename: a crash mid-write never leaves a broken file.
    pub fn save(&self) -> Result<()> {
        let path = paths::state_file()?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    // ---- tasks ----

    pub fn next_id(&mut self) -> TaskId {
        self.last_id += 1;
        self.last_id
    }

    pub fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.get_mut(&id)
    }

    pub fn remove(&mut self, id: TaskId) -> Option<Task> {
        self.tasks.remove(&id)
    }

    /// Copies of every task, in id order.
    pub fn list(&self) -> Vec<Task> {
        self.tasks.values().cloned().collect()
    }

    // ---- context ----

    pub fn next_context_id(&mut self) -> ContextId {
        self.last_context_id += 1;
        self.last_context_id
    }

    pub fn context(&self) -> &BTreeMap<ContextId, ContextItem> {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut BTreeMap<ContextId, ContextItem> {
        &mut self.context
    }
}
