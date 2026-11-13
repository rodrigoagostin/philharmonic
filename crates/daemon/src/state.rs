//! What the daemon remembers: tasks, context items and collections. Saved to `state.json` after every
//! change, and loaded at startup, so a daemon restart loses nothing but running processes.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use philharmonic_core::{Collection, CollectionId, ContextId, ContextItem, Scope, Task, TaskId, TaskStatus, now, paths};
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
    #[serde(default)]
    last_collection_id: CollectionId,
    #[serde(default)]
    collections: BTreeMap<CollectionId, Collection>,
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
        state.migrate_old_scopes();
        state.drop_drafts();
        Ok(state)
    }

    /// Items attached in a New task window that never started its task.
    fn drop_drafts(&mut self) {
        self.context.retain(|_, i| {
            let keep = i.scope != Scope::Draft;
            if !keep {
                let _ = std::fs::remove_file(&i.path);
            }
            keep
        });
    }

    /// Older versions kept items per repo ("Project") or in a shared "Inbox". Each repo's
    /// items become a collection linked to that repo; inbox items go to "Unsorted".
    fn migrate_old_scopes(&mut self) {
        let old: Vec<(ContextId, Scope)> = self
            .context
            .values()
            .filter(|i| matches!(i.scope, Scope::Project { .. } | Scope::Inbox))
            .map(|i| (i.id, i.scope.clone()))
            .collect();
        for (item, scope) in old {
            let (name, repos) = match scope {
                Scope::Project { repo } => {
                    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
                    (name, vec![repo])
                }
                _ => ("Unsorted".to_string(), vec![]),
            };
            let existing = self.collections.values().find(|c| c.name == name && c.repos == repos).map(|c| c.id);
            let id = existing.unwrap_or_else(|| self.add_collection(name, repos));
            if let Some(i) = self.context.get_mut(&item) {
                i.scope = Scope::Collection { id };
            }
        }
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

    // ---- collections ----

    pub fn add_collection(&mut self, name: String, repos: Vec<std::path::PathBuf>) -> CollectionId {
        self.last_collection_id += 1;
        let id = self.last_collection_id;
        self.collections.insert(id, Collection { id, name, repos, created_at: now() });
        id
    }

    pub fn collections(&self) -> &BTreeMap<CollectionId, Collection> {
        &self.collections
    }

    pub fn collections_mut(&mut self) -> &mut BTreeMap<CollectionId, Collection> {
        &mut self.collections
    }

    /// Every task, to change (e.g. stop using a deleted collection).
    pub fn tasks_mut(&mut self) -> impl Iterator<Item = &mut Task> {
        self.tasks.values_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use philharmonic_core::ContextKind;

    fn item(id: ContextId, scope: Scope) -> ContextItem {
        ContextItem { id, scope, kind: ContextKind::Note, title: "n".into(), path: "/x".into(), text: String::new(), created_at: 0 }
    }

    #[test]
    fn old_project_and_inbox_items_become_collections() {
        let mut s = State::default();
        let repo: std::path::PathBuf = "/code/quilora".into();
        s.context.insert(1, item(1, Scope::Project { repo: repo.clone() }));
        s.context.insert(2, item(2, Scope::Project { repo: repo.clone() }));
        s.context.insert(3, item(3, Scope::Inbox));
        s.context.insert(4, item(4, Scope::Task { id: 7 }));

        s.migrate_old_scopes();

        assert_eq!(s.collections.len(), 2);
        let quilora = s.collections.values().find(|c| c.name == "quilora").unwrap();
        assert_eq!(quilora.repos, vec![repo]);
        assert_eq!(s.context[&1].scope, Scope::Collection { id: quilora.id });
        assert_eq!(s.context[&2].scope, Scope::Collection { id: quilora.id });
        let unsorted = s.collections.values().find(|c| c.name == "Unsorted").unwrap();
        assert_eq!(s.context[&3].scope, Scope::Collection { id: unsorted.id });
        assert_eq!(s.context[&4].scope, Scope::Task { id: 7 });
    }
}
