//! Context on disk: every item is a file under `<data_dir>/ctx/<scope>/`, one folder per
//! collection and per task. Also collections themselves, and the brief an agent gets about
//! the context its task uses.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{
    Collection, CollectionId, ContextId, ContextItem, ContextKind, Event, Scope, Task, TaskId, now, paths, slug::slugify,
};

use crate::daemon::Shared;
use crate::git;

pub fn scope_dir(scope: &Scope) -> Result<PathBuf> {
    let name = match scope {
        Scope::Task { id } => format!("task-{id}"),
        Scope::Collection { id } => format!("collection-{id}"),
        Scope::Draft => "drafts".to_string(),
        Scope::Project { repo } => format!("project-{}", git::repo_name(repo)),
        Scope::Inbox => "inbox".to_string(),
    };
    Ok(paths::context_dir()?.join(name))
}

/// Keep the extension, make the rest safe for a file name.
fn safe_file_name(name: &str, fallback_ext: &str) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 8 => (stem, ext.to_ascii_lowercase()),
        _ => (name, fallback_ext.to_string()),
    };
    format!("{}.{}", slugify(stem, 60), ext)
}

pub fn add(
    d: &Shared,
    scope: Scope,
    kind: ContextKind,
    name: String,
    data: Option<String>,
    text: Option<String>,
) -> Result<ContextItem> {
    check_scope(d, &scope)?;
    let dir = scope_dir(&scope)?;
    std::fs::create_dir_all(&dir)?;
    let id = d.state.lock().unwrap().next_context_id();
    let text = text.unwrap_or_default();

    let (title, file_name, bytes) = match kind {
        ContextKind::Image | ContextKind::File => {
            let data = data.context("missing file data")?;
            let bytes = B64.decode(data.as_bytes()).context("file data isn't valid base64")?;
            let fallback = if kind == ContextKind::Image { "png" } else { "bin" };
            (name.clone(), safe_file_name(&name, fallback), bytes)
        }
        ContextKind::Link => {
            if text.trim().is_empty() {
                bail!("a link needs a URL");
            }
            let title = if name.trim().is_empty() { text.clone() } else { name.clone() };
            let body = format!("# {title}\n\n{}\n\nOpen this URL to read it.\n", text.trim());
            (title, format!("link-{}.md", slugify(&text, 50)), body.into_bytes())
        }
        ContextKind::Note => {
            let first_line = text.lines().next().unwrap_or("").trim().to_string();
            let title = if name.trim().is_empty() { first_line } else { name.clone() };
            (title.clone(), format!("note-{}.md", slugify(&title, 40)), text.clone().into_bytes())
        }
    };

    let path = dir.join(format!("{id}-{file_name}"));
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;

    let item = ContextItem { id, scope, kind, title, path, text, created_at: now() };
    d.state.lock().unwrap().context_mut().insert(id, item.clone());
    d.save();
    d.emit(Event::ContextChanged);
    Ok(item)
}

pub fn list(d: &Shared) -> Vec<ContextItem> {
    d.state.lock().unwrap().context().values().cloned().collect()
}

pub fn get(d: &Shared, ids: &[ContextId]) -> Vec<ContextItem> {
    let state = d.state.lock().unwrap();
    ids.iter().filter_map(|id| state.context().get(id).cloned()).collect()
}

/// Items go to a task or a collection that exists (the old scopes are only read).
fn check_scope(d: &Shared, scope: &Scope) -> Result<()> {
    let state = d.state.lock().unwrap();
    match scope {
        Scope::Task { id } if state.get(*id).is_none() => bail!("no task {id}"),
        Scope::Collection { id } if !state.collections().contains_key(id) => bail!("no collection {id}"),
        Scope::Project { .. } | Scope::Inbox => bail!("put it in a collection or a task"),
        _ => Ok(()),
    }
}

/// Change an item's scope (the file stays where it is).
pub fn move_to(d: &Shared, id: ContextId, scope: Scope) -> Result<()> {
    check_scope(d, &scope)?;
    {
        let mut state = d.state.lock().unwrap();
        let item = state.context_mut().get_mut(&id).context("no such context item")?;
        item.scope = scope;
    }
    d.save();
    d.emit(Event::ContextChanged);
    Ok(())
}

pub fn remove(d: &Shared, id: ContextId) -> Result<()> {
    let item = d.state.lock().unwrap().context_mut().remove(&id).context("no such context item")?;
    let _ = std::fs::remove_file(&item.path);
    d.save();
    d.emit(Event::ContextChanged);
    Ok(())
}

/// Delete everything that belongs to a task.
pub fn remove_for_task(d: &Shared, task: TaskId) {
    let ids: Vec<ContextId> = list(d)
        .into_iter()
        .filter(|i| i.scope == Scope::Task { id: task })
        .map(|i| i.id)
        .collect();
    for id in ids {
        let _ = remove(d, id);
    }
}

/// The text appended to a message so the agent knows what's attached.
pub fn describe(items: &[ContextItem]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nAttached context (open these to see them):");
    for item in items {
        let line = match item.kind {
            ContextKind::Image => format!("\n- Screenshot/image: {}", item.path.display()),
            ContextKind::File => format!("\n- File \"{}\": {}", item.title, item.path.display()),
            ContextKind::Link => format!("\n- Link: {}", item.text.trim()),
            ContextKind::Note => format!("\n- Note ({}): {}", item.path.display(), item.text.trim()),
        };
        out.push_str(&line);
    }
    out
}

// ---- collections ----

pub fn collections(d: &Shared) -> Vec<Collection> {
    d.state.lock().unwrap().collections().values().cloned().collect()
}

/// Create (`id` = None) or update a collection.
pub fn save_collection(d: &Shared, id: Option<CollectionId>, name: String, repos: Vec<PathBuf>) -> Result<Collection> {
    let name = name.trim().to_string();
    if name.is_empty() {
        bail!("a collection needs a name");
    }
    let mut repos: Vec<PathBuf> = repos.into_iter().map(|r| std::fs::canonicalize(&r).unwrap_or(r)).collect();
    repos.dedup();
    let collection = {
        let mut state = d.state.lock().unwrap();
        let id = match id {
            Some(id) => {
                let c = state.collections_mut().get_mut(&id).context("no such collection")?;
                c.name = name;
                c.repos = repos;
                id
            }
            None => state.add_collection(name, repos),
        };
        state.collections()[&id].clone()
    };
    d.save();
    d.emit(Event::ContextChanged);
    Ok(collection)
}

/// Delete a collection, its items and their files. Tasks stop using it.
pub fn delete_collection(d: &Shared, id: CollectionId) -> Result<()> {
    let changed: Vec<Task> = {
        let mut state = d.state.lock().unwrap();
        state.collections_mut().remove(&id).context("no such collection")?;
        let items: Vec<ContextId> =
            state.context().values().filter(|i| i.scope == Scope::Collection { id }).map(|i| i.id).collect();
        for item in items {
            if let Some(i) = state.context_mut().remove(&item) {
                let _ = std::fs::remove_file(&i.path);
            }
        }
        state
            .tasks_mut()
            .filter(|t| t.collections.contains(&id))
            .map(|t| {
                t.collections.retain(|c| *c != id);
                t.clone()
            })
            .collect()
    };
    if let Ok(dir) = scope_dir(&Scope::Collection { id }) {
        let _ = std::fs::remove_dir(dir); // only if empty: never delete files we don't know about
    }
    d.save();
    for task in changed {
        d.emit(Event::TaskUpdated { task });
    }
    d.emit(Event::ContextChanged);
    Ok(())
}

/// The collections linked to a repo: what a new task there uses unless you choose.
pub fn linked_to(d: &Shared, repo: &Path) -> Vec<CollectionId> {
    let repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    d.state.lock().unwrap().collections().values().filter(|c| c.repos.contains(&repo)).map(|c| c.id).collect()
}

/// Only ids of collections that exist, without repeats.
pub fn existing_collections(d: &Shared, ids: &[CollectionId]) -> Vec<CollectionId> {
    let state = d.state.lock().unwrap();
    let mut out = Vec::new();
    for id in ids {
        if state.collections().contains_key(id) && !out.contains(id) {
            out.push(*id);
        }
    }
    out
}

/// What an agent is told about its task's collections: (text, folders to give it access to).
/// `None` if the task uses none.
pub fn brief(d: &Shared, collections: &[CollectionId], intro: &str) -> Option<(String, Vec<PathBuf>)> {
    let state = d.state.lock().unwrap();
    let used: Vec<&Collection> = collections.iter().filter_map(|id| state.collections().get(id)).collect();
    if used.is_empty() {
        return None;
    }
    let mut text = String::from(intro);
    let mut dirs = Vec::new();
    for c in used {
        let dir = scope_dir(&Scope::Collection { id: c.id }).ok()?;
        let _ = std::fs::create_dir_all(&dir);
        text.push_str(&format!("\n\n## {}\nFolder: {}", c.name, dir.display()));
        let mut items: Vec<&ContextItem> =
            state.context().values().filter(|i| i.scope == Scope::Collection { id: c.id }).collect();
        items.sort_by_key(|i| i.created_at);
        if items.is_empty() {
            text.push_str("\n(empty for now)");
        }
        for item in items {
            text.push_str(&describe_item(item));
        }
        dirs.push(dir);
    }
    Some((text, dirs))
}

/// Notes up to this long are included in the brief itself (longer ones: just the file).
const INLINE_NOTE: usize = 1500;

fn describe_item(item: &ContextItem) -> String {
    match item.kind {
        ContextKind::Image => format!("\n- Image \"{}\": {}", item.title, item.path.display()),
        ContextKind::File => format!("\n- File \"{}\": {}", item.title, item.path.display()),
        ContextKind::Link => format!("\n- Link: {}", item.text.trim()),
        ContextKind::Note if item.text.len() <= INLINE_NOTE => {
            format!("\n- Note: {}", item.text.trim().replace('\n', "\n  "))
        }
        ContextKind::Note => format!("\n- Note \"{}\": {}", item.title, item.path.display()),
    }
}
