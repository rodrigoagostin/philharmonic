//! The context shelf on disk: every item is a file under `<data_dir>/ctx/<scope>/`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{ContextId, ContextItem, ContextKind, Event, Scope, TaskId, now, paths, slug::slugify};

use crate::daemon::Shared;
use crate::git;

fn scope_dir(scope: &Scope) -> Result<PathBuf> {
    let name = match scope {
        Scope::Task { id } => format!("task-{id}"),
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

/// Change an item's scope (the file stays where it is).
pub fn move_to(d: &Shared, id: ContextId, scope: Scope) -> Result<()> {
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
