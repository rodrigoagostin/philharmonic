//! The activity log: what every agent did, one JSON-lines file per task
//! (`~/.philharmonic/activity/<id>.jsonl`), plus the most recent entries in memory for the
//! Overview's live feed.

use std::collections::VecDeque;
use std::io::Write;

use philharmonic_core::{Activity, ActivityKind, Event, TaskId, now, paths};

use crate::daemon::Shared;

/// How many recent entries (all tasks together) stay in memory.
const RECENT_CAP: usize = 2000;

/// Record something an agent did: append it to the task's log and tell subscribers.
pub fn record(d: &Shared, task: TaskId, agent: Option<String>, kind: ActivityKind, text: impl Into<String>) {
    let item = Activity { task, at: now(), agent, kind, text: text.into() };
    if let Err(e) = append(&item) {
        tracing::warn!("writing activity log: {e:#}");
    }
    {
        let mut recent = d.recent.lock().unwrap();
        recent.push_back(item.clone());
        if recent.len() > RECENT_CAP {
            recent.pop_front();
        }
    }
    d.emit(Event::Activity { item });
}

fn append(item: &Activity) -> anyhow::Result<()> {
    let dir = paths::activity_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{}.jsonl", item.task)))?;
    let mut line = serde_json::to_vec(item)?;
    line.push(b'\n');
    file.write_all(&line)?;
    Ok(())
}

/// One task's log (the last `limit` entries, oldest first).
pub fn for_task(id: TaskId, limit: usize) -> Vec<Activity> {
    let Ok(path) = paths::activity_dir().map(|dir| dir.join(format!("{id}.jsonl"))) else {
        return Vec::new();
    };
    let text = tail(&path, limit);
    let items: Vec<Activity> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let skip = items.len().saturating_sub(limit);
    items.into_iter().skip(skip).collect()
}

/// The end of a log: enough bytes for about `lines` lines, without reading the whole file
/// (a long-running task's log keeps growing). Starts at a line boundary.
fn tail(path: &std::path::Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return String::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    // Entries are short (text is capped), 1 KB each is generous.
    let want = (lines as u64).saturating_mul(1024).min(len);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(len - want)).is_err() || file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    if want < len {
        // We started mid-file: drop the partial first line.
        let start = buf.iter().position(|&b| b == b'\n').map_or(buf.len(), |i| i + 1);
        buf.drain(..start);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The most recent entries across all tasks (oldest first).
pub fn recent(d: &Shared, limit: usize) -> Vec<Activity> {
    let recent = d.recent.lock().unwrap();
    let skip = recent.len().saturating_sub(limit);
    recent.iter().skip(skip).cloned().collect()
}

/// At startup: refill the in-memory feed from the logs on disk.
pub fn load_recent(tasks: &[TaskId]) -> VecDeque<Activity> {
    let mut all: Vec<Activity> = tasks.iter().flat_map(|id| for_task(*id, 200)).collect();
    all.sort_by_key(|a| a.at);
    let skip = all.len().saturating_sub(RECENT_CAP);
    all.into_iter().skip(skip).collect()
}

pub fn remove(id: TaskId) {
    if let Ok(dir) = paths::activity_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{id}.jsonl")));
    }
}
