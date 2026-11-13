//! Claude Code hooks → task status, live details and the activity log.
//!
//! Every hook runs `ph notify`, which forwards the hook's JSON here. The events we use:
//!
//! - `UserPromptSubmit`: a prompt was given → running
//! - `PreToolUse` / `PostToolUse`: an action starts / ends (`agent_id` is set inside subagents)
//! - `SubagentStart` / `SubagentStop`: a helper agent starts / ends (they run in the background)
//! - `Notification`: the agent is waiting for you
//! - `Stop`: the agent finished its turn (`background_tasks` lists subagents still running)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use philharmonic_core::{ActivityKind, PlanItem, Subagent, Task, TaskId, TaskStatus, Usage, now};
use serde_json::Value;

use crate::activity_log::record;
use crate::daemon::Shared;

pub fn on_hook(d: &Shared, id: TaskId, p: &Value) {
    let event = p["hook_event_name"].as_str().unwrap_or_default();
    let agent_id = p["agent_id"].as_str().map(str::to_string);
    let Some(task) = d.task(id) else { return };
    let worktree = task.worktree.clone();
    if let Some(aid) = &agent_id {
        if event != "SubagentStart" && event != "SubagentStop" {
            match_subagent(d, id, aid, p, false);
        }
    }

    match event {
        "UserPromptSubmit" => {
            update(d, id, |t| {
                set_live_status(t, TaskStatus::Running);
                t.doing = None;
            });
            let prompt = p["prompt"].as_str().unwrap_or_default();
            // `<agent-message …>` = a subagent reporting back (already logged at its handback).
            if !prompt.trim().is_empty() && !prompt.trim_start().starts_with("<agent-message") {
                record(d, id, None, ActivityKind::Prompt, shorten(prompt, 300));
            }
            refresh_usage(d, id, p);
        }

        "PreToolUse" => {
            let tool = p["tool_name"].as_str().unwrap_or_default();
            let input = &p["tool_input"];
            // A subagent handing its result back to the main agent.
            if tool == "SubagentHandback" {
                let (Some(aid), Some(message)) = (agent_id, input["message"].as_str()) else { return };
                let result = shorten(message, 400);
                update(d, id, |t| {
                    if let Some(s) = t.subagents.iter_mut().find(|s| s.id == aid) {
                        s.result = Some(result.clone());
                        s.doing = Some("reporting back".into());
                    }
                });
                record(d, id, Some(aid), ActivityKind::Subagent, format!("Reported back: {}", shorten(message, 200)));
                return;
            }
            let Some(action) = describe_tool(tool, input, &worktree) else { return };
            update(d, id, |t| {
                set_live_status(t, TaskStatus::Running);
                t.tool_calls += 1;
                match &agent_id {
                    Some(aid) => {
                        if let Some(s) = t.subagents.iter_mut().find(|s| &s.id == aid) {
                            s.doing = Some(action.clone());
                            s.tool_calls += 1;
                        }
                    }
                    None => t.doing = Some(action.clone()),
                }
                // Launching a subagent: list it now; `SubagentStart` gives it its real id.
                if is_agent_tool(tool) {
                    t.subagents.push(Subagent {
                        id: p["tool_use_id"].as_str().unwrap_or("pending").to_string(),
                        kind: input["subagent_type"].as_str().unwrap_or("general-purpose").to_string(),
                        description: input["description"].as_str().unwrap_or("subagent").to_string(),
                        running: true,
                        started_at: now(),
                        ended_at: None,
                        doing: Some("starting".into()),
                        tool_calls: 0,
                        result: None,
                        prompt: input["prompt"].as_str().map(str::to_string),
                    });
                }
            });
            let kind = if is_agent_tool(tool) { ActivityKind::Subagent } else { ActivityKind::Tool };
            record(d, id, agent_id, kind, action);
        }

        "PostToolUse" => {
            let tool = p["tool_name"].as_str().unwrap_or_default();
            update(d, id, |t| {
                set_live_status(t, TaskStatus::Running);
                match &agent_id {
                    Some(aid) => {
                        if let Some(s) = t.subagents.iter_mut().find(|s| &s.id == aid) {
                            s.doing = None;
                        }
                    }
                    None => {
                        if !is_agent_tool(tool) {
                            t.doing = None;
                        }
                        update_plan(t, tool, &p["tool_input"], &p["tool_response"]);
                    }
                }
            });
        }

        "SubagentStart" => {
            let Some(aid) = agent_id else { return };
            let kind = p["agent_type"].as_str().unwrap_or("general-purpose").to_string();
            update(d, id, |t| {
                if !t.subagents.iter().any(|s| s.id == aid) {
                    t.subagents.push(Subagent {
                        id: aid.clone(),
                        kind: kind.clone(),
                        description: kind.clone(),
                        running: true,
                        started_at: now(),
                        ended_at: None,
                        doing: None,
                        tool_calls: 0,
                        result: None,
                        prompt: None,
                    });
                }
            });
            match_subagent(d, id, &aid, p, false);
        }

        "SubagentStop" => {
            let Some(aid) = agent_id else { return };
            match_subagent(d, id, &aid, p, true);
            let result = p["last_assistant_message"].as_str().map(|m| shorten(m, 400));
            // Claude Code also runs small internal agents; only report the ones we saw start.
            let mut finished = None;
            update(d, id, |t| {
                if let Some(s) = t.subagents.iter_mut().find(|s| s.id == aid && s.running) {
                    s.running = false;
                    s.ended_at = Some(now());
                    s.doing = None;
                    if s.result.is_none() {
                        s.result = result.clone();
                    }
                    finished = Some(s.description.clone());
                }
            });
            if let Some(description) = finished {
                record(d, id, Some(aid), ActivityKind::Subagent, format!("Finished: {description}"));
            }
            refresh_usage(d, id, p);
        }

        "Notification" => {
            let message = p["message"].as_str().unwrap_or_default().to_string();
            update(d, id, |t| {
                if !t.status.is_live() {
                    return;
                }
                // Claude also sends "waiting for your input" after a minute of idling. If it
                // finished its turn, keep "review" instead of flipping to "needs you".
                if t.status == TaskStatus::Review && message.contains("waiting for your input") {
                    return;
                }
                t.status = TaskStatus::NeedsInput;
            });
            if !message.contains("waiting for your input") {
                record(d, id, agent_id, ActivityKind::Waiting, message);
            }
        }

        "Stop" => {
            // Subagents run in the background: the main agent can stop while they still work.
            let still_running = p["background_tasks"]
                .as_array()
                .map(|tasks| tasks.iter().filter(|t| t["status"] == "running").count())
                .unwrap_or(0);
            let summary = p["last_assistant_message"].as_str().map(|m| shorten(m, 600));
            update(d, id, |t| {
                if !t.status.is_live() {
                    return;
                }
                if still_running > 0 {
                    t.status = TaskStatus::Running;
                    t.doing = Some(format!("waiting for {still_running} subagent{}", if still_running == 1 { "" } else { "s" }));
                } else {
                    t.status = TaskStatus::Review;
                    t.doing = None;
                    // Launches that never started (or were never matched) aren't running.
                    t.subagents.retain(|s| !(s.id.starts_with("toolu_") && s.tool_calls == 0));
                    for s in &mut t.subagents {
                        s.running &= !s.id.starts_with("toolu_");
                    }
                    if summary.is_some() {
                        t.summary = summary.clone();
                    }
                }
            });
            if still_running == 0 {
                record(d, id, None, ActivityKind::Finished, summary.unwrap_or_else(|| "Finished its turn".into()));
            }
            refresh_usage(d, id, p);
        }

        _ => {}
    }
}

/// Claude Code's `SubagentStart` doesn't say *which* launch it is. The subagent's own
/// transcript starts with its prompt, so match that against the launches we saw in
/// `PreToolUse`. With `fallback`, use the oldest pending launch of the same type instead.
fn match_subagent(d: &Shared, id: TaskId, aid: &str, p: &Value, fallback: bool) {
    let unmatched = d.task(id).is_some_and(|t| t.subagents.iter().any(|s| s.id == aid && s.prompt.is_none()));
    if !unmatched {
        return;
    }
    let first_prompt = p["transcript_path"].as_str().and_then(|path| subagent_prompt(Path::new(path), aid));
    if first_prompt.is_none() && !fallback {
        return; // its transcript isn't written yet; try again on its next event
    }
    update(d, id, |t| {
        let Some(me) = t.subagents.iter().position(|s| s.id == aid) else { return };
        let kind = t.subagents[me].kind.clone();
        let pending = t.subagents.iter().position(|s| {
            s.id.starts_with("toolu_")
                && match &first_prompt {
                    Some(fp) => s.prompt.as_deref().is_some_and(|lp| lp.trim() == fp.trim() || fp.contains(lp.trim())),
                    None => s.kind == kind,
                }
        });
        if let Some(i) = pending {
            let launch = t.subagents.remove(i);
            let me = if i < me { me - 1 } else { me };
            let s = &mut t.subagents[me];
            s.description = launch.description;
            s.prompt = launch.prompt;
            s.started_at = launch.started_at;
            s.tool_calls += launch.tool_calls;
        }
    });
}

/// The first user message of `<session>/subagents/agent-<id>.jsonl`.
fn subagent_prompt(main_transcript: &Path, aid: &str) -> Option<String> {
    let path = main_transcript.with_extension("").join("subagents").join(format!("agent-{aid}.jsonl"));
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|e| e["type"] == "user").and_then(|e| {
        let content = &e["message"]["content"];
        content.as_str().map(str::to_string).or_else(|| {
            content.as_array().map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"))
        })
    })
}

fn update(d: &Shared, id: TaskId, change: impl FnOnce(&mut Task)) {
    d.update_task(id, |t| {
        change(t);
        t.last_activity = now();
        tidy(t);
    });
}

/// Finished subagents kept on the task (older ones are still in the activity log).
const KEEP_FINISHED_SUBAGENTS: usize = 10;
/// Plan items kept (the oldest finished ones go first).
const KEEP_PLAN_ITEMS: usize = 50;

/// Keep a task the same size however long the session runs: it's saved and sent to the app
/// on every change, so anything that only grows would make every update slower.
fn tidy(t: &mut Task) {
    let finished = t.subagents.iter().filter(|s| !s.running && !s.id.starts_with("toolu_")).count();
    let mut drop = finished.saturating_sub(KEEP_FINISHED_SUBAGENTS);
    t.subagents.retain(|s| {
        let old = drop > 0 && !s.running && !s.id.starts_with("toolu_");
        if old {
            drop -= 1;
        }
        !old
    });
    for s in &mut t.subagents {
        // Launches keep their full prompt until matched (see `match_subagent`).
        if s.id.starts_with("toolu_") {
            continue;
        }
        if let Some(p) = &s.prompt {
            if p.chars().count() > 200 {
                s.prompt = Some(shorten(p, 200));
            }
        }
    }
    let mut extra = t.plan.len().saturating_sub(KEEP_PLAN_ITEMS);
    t.plan.retain(|item| {
        let old = extra > 0 && item.status == "completed";
        if old {
            extra -= 1;
        }
        !old
    });
    if t.plan.len() > KEEP_PLAN_ITEMS {
        let cut = t.plan.len() - KEEP_PLAN_ITEMS;
        t.plan.drain(..cut);
    }
}

/// Hooks only move live tasks (a stopped or done task stays as it is).
fn set_live_status(t: &mut Task, status: TaskStatus) {
    if t.status.is_live() {
        t.status = status;
    }
}

fn is_agent_tool(tool: &str) -> bool {
    tool == "Agent" || tool == "Task"
}

/// The agent's todo list. Newer Claude Code: `TaskCreate` / `TaskUpdate`. Older: `TodoWrite`.
fn update_plan(t: &mut Task, tool: &str, input: &Value, response: &Value) {
    match tool {
        "TaskCreate" => {
            let id = response["task"]["id"].as_str().map(str::to_string).unwrap_or_else(|| (t.plan.len() + 1).to_string());
            let text = input["subject"].as_str().unwrap_or("(untitled)").to_string();
            t.plan.push(PlanItem { id, text, status: "pending".into() });
        }
        "TaskUpdate" => {
            let id = input["taskId"].as_str().or(input["id"].as_str()).unwrap_or_default();
            if let Some(item) = t.plan.iter_mut().find(|i| i.id == id) {
                if let Some(status) = input["status"].as_str() {
                    item.status = status.to_string();
                }
                if let Some(subject) = input["subject"].as_str() {
                    item.text = subject.to_string();
                }
            }
        }
        "TodoWrite" => {
            if let Some(todos) = input["todos"].as_array() {
                t.plan = todos
                    .iter()
                    .enumerate()
                    .map(|(i, todo)| PlanItem {
                        id: (i + 1).to_string(),
                        text: todo["content"].as_str().unwrap_or_default().to_string(),
                        status: todo["status"].as_str().unwrap_or("pending").to_string(),
                    })
                    .collect();
            }
        }
        _ => {}
    }
}

/// A short, human description of a tool call. `None` = not worth showing.
pub fn describe_tool(tool: &str, input: &Value, worktree: &Path) -> Option<String> {
    let s = |key: &str| input[key].as_str().unwrap_or_default();
    let path = |key: &str| relative(s(key), worktree);
    Some(match tool {
        "ToolSearch" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" | "TodoWrite" => return None,
        "Read" => format!("Reading {}", path("file_path")),
        "Edit" | "MultiEdit" => format!("Editing {}", path("file_path")),
        "Write" => format!("Writing {}", path("file_path")),
        "NotebookEdit" => format!("Editing {}", path("notebook_path")),
        "Bash" => {
            let cmd = s("command").lines().next().unwrap_or_default();
            format!("Running `{}`", shorten(cmd, 100))
        }
        "Grep" => format!("Searching for “{}”", shorten(s("pattern"), 60)),
        "Glob" => format!("Finding files {}", s("pattern")),
        "WebFetch" => format!("Reading {}", s("url")),
        "WebSearch" => format!("Searching the web: {}", s("query")),
        "Agent" | "Task" => format!("Launching subagent: {}", s("description")),
        "Skill" => format!("Using skill {}", s("skill")),
        other => match other.strip_prefix("mcp__") {
            Some(rest) => {
                let mut parts = rest.splitn(2, "__");
                let server = parts.next().unwrap_or_default();
                let name = parts.next().unwrap_or(rest);
                format!("Using {name} ({server})")
            }
            None => format!("Using {other}"),
        },
    })
}

fn relative(path: &str, worktree: &Path) -> String {
    Path::new(path).strip_prefix(worktree).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string())
}

/// First line-ish of a text, at most `max` characters.
pub fn shorten(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max - 1).collect::<String>())
}

// ---- token usage ----

/// Re-count tokens from the session transcript (and its subagents' transcripts) in the
/// background, then store them on the task.
fn refresh_usage(d: &Shared, id: TaskId, p: &Value) {
    let Some(transcript) = p["transcript_path"].as_str().map(PathBuf::from) else { return };
    let d = d.clone();
    tokio::task::spawn_blocking(move || {
        let usage = session_usage(&transcript);
        if usage.total() > 0 {
            d.update_task(id, |t| t.usage = usage);
        }
    });
}

/// `<session>.jsonl` plus `<session>/subagents/*.jsonl`.
fn session_usage(transcript: &Path) -> Usage {
    let mut total = file_usage(transcript);
    let subagents = transcript.with_extension("").join("subagents");
    if let Ok(entries) = std::fs::read_dir(subagents) {
        for entry in entries.flatten() {
            let u = file_usage(&entry.path());
            total.input += u.input;
            total.output += u.output;
            total.cache_read += u.cache_read;
            total.cache_write += u.cache_write;
        }
    }
    total
}

/// How far we've read a transcript, and what it added up to.
#[derive(Default)]
struct Cursor {
    offset: u64,
    usage: Usage,
    seen: std::collections::HashSet<String>,
}

/// Transcripts only grow, so each one is read once: later calls parse just the new lines.
static CURSORS: std::sync::LazyLock<Mutex<HashMap<PathBuf, Cursor>>> = std::sync::LazyLock::new(Default::default);

/// Sum the `usage` of each assistant message. A message can be written on several lines
/// (one per content block) with the same id, so each id is counted once.
fn file_usage(path: &Path) -> Usage {
    use std::io::{Read, Seek, SeekFrom};
    let mut cursors = CURSORS.lock().unwrap();
    let cursor = cursors.entry(path.to_path_buf()).or_default();
    let Ok(mut file) = std::fs::File::open(path) else { return cursor.usage.clone() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < cursor.offset {
        *cursor = Cursor::default(); // rewritten from scratch: start over
    }
    let mut new = Vec::new();
    if file.seek(SeekFrom::Start(cursor.offset)).is_err() || file.read_to_end(&mut new).is_err() {
        return cursor.usage.clone();
    }
    // Only whole lines: the last one may still be being written.
    let Some(end) = new.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else { return cursor.usage.clone() };
    cursor.offset += end as u64;
    for line in String::from_utf8_lossy(&new[..end]).lines() {
        add_line_usage(line, &mut cursor.usage, &mut cursor.seen);
    }
    cursor.usage.clone()
}

fn add_line_usage(line: &str, usage: &mut Usage, seen: &mut std::collections::HashSet<String>) {
    // Cheap check first: most lines aren't assistant messages with usage.
    if !line.contains("\"usage\"") {
        return;
    }
    let Ok(entry) = serde_json::from_str::<Value>(line) else { return };
    if entry["type"] != "assistant" {
        return;
    }
    let message = &entry["message"];
    let u = &message["usage"];
    if u.is_null() || !seen.insert(message["id"].as_str().unwrap_or_default().to_string()) {
        return;
    }
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    usage.input += n("input_tokens");
    usage.output += n("output_tokens");
    usage.cache_read += n("cache_read_input_tokens");
    usage.cache_write += n("cache_creation_input_tokens");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describes_tools_with_relative_paths() {
        let wt = Path::new("/wt/api/x");
        assert_eq!(describe_tool("Read", &json!({"file_path": "/wt/api/x/src/main.rs"}), wt).unwrap(), "Reading src/main.rs");
        assert_eq!(describe_tool("Bash", &json!({"command": "cargo test\necho hi"}), wt).unwrap(), "Running `cargo test`");
        assert_eq!(describe_tool("mcp__github__create_issue", &json!({}), wt).unwrap(), "Using create_issue (github)");
        assert!(describe_tool("ToolSearch", &json!({}), wt).is_none());
    }

    #[test]
    fn counts_each_message_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let line = r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":2,"output_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}"#;
        std::fs::write(&path, format!("{line}\n{line}\n")).unwrap();
        let u = session_usage(&path);
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (2, 10, 100, 5));
    }

    #[test]
    fn reads_only_new_whole_lines() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let msg = |id: &str| format!(r#"{{"type":"assistant","message":{{"id":"{id}","usage":{{"output_tokens":10}}}}}}"#);
        std::fs::write(&path, format!("{}\n", msg("a"))).unwrap();
        assert_eq!(file_usage(&path).output, 10);

        // A line still being written isn't counted yet…
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        let b = msg("b");
        write!(f, "{}", &b[..20]).unwrap();
        assert_eq!(file_usage(&path).output, 10);
        // …and is counted once it's complete. Repeated ids still count once.
        writeln!(f, "{}", &b[20..]).unwrap();
        writeln!(f, "{}", msg("a")).unwrap();
        assert_eq!(file_usage(&path).output, 20);
        assert_eq!(file_usage(&path).output, 20);
    }

    #[test]
    fn tidy_keeps_tasks_small() {
        let mut t: Task = serde_json::from_value(json!({
            "id": 1, "title": "t", "prompt": "p", "repo": "/r", "base": "main", "branch": "b",
            "worktree": "/w", "agent": {"kind": "claude"}, "status": "running", "created_at": 0, "last_activity": 0
        }))
        .unwrap();
        for i in 0..40 {
            let running = i >= 38;
            t.subagents.push(serde_json::from_value(json!({
                "id": format!("a{i}"), "kind": "Explore", "description": "d", "running": running,
                "started_at": 0, "prompt": "p".repeat(5000)
            })).unwrap());
        }
        for i in 0..120 {
            t.plan.push(PlanItem { id: i.to_string(), text: "x".into(), status: if i < 100 { "completed" } else { "pending" }.into() });
        }
        tidy(&mut t);
        assert_eq!(t.subagents.iter().filter(|s| s.running).count(), 2, "running subagents are kept");
        assert_eq!(t.subagents.len(), 2 + KEEP_FINISHED_SUBAGENTS);
        assert!(t.subagents.iter().all(|s| s.prompt.as_ref().unwrap().chars().count() <= 200));
        assert_eq!(t.plan.len(), KEEP_PLAN_ITEMS);
        assert_eq!(t.plan.iter().filter(|p| p.status == "pending").count(), 20, "unfinished steps are kept");
    }
}
