//! Claude Code's own conversations on disk, so threads you started elsewhere can become
//! tasks here.
//!
//! Claude Code saves each conversation as `~/.claude/projects/<folder>/<session-id>.jsonl`
//! (one JSON object per line), where `<folder>` is the working directory with every
//! character that isn't a letter or digit replaced by `-`. Running sessions register in
//! `~/.claude/sessions/<pid>.json`.
//!
//! This isn't a documented format, so everything here is defensive: unreadable files and
//! lines we don't understand are skipped, never errors.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// `~/.claude`, or `$CLAUDE_CONFIG_DIR` if you set it.
fn claude_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::home_dir().map(|h| h.join(".claude")),
    }
}

/// How Claude Code names a folder's directory under `projects/`.
pub fn encode(path: &Path) -> String {
    normalize(&path.to_string_lossy())
}

fn normalize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// One conversation, as much as we need of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub cwd: PathBuf,
    pub title: String,
    pub prompt: String,
    pub prompts: u32,
    pub updated_at: u64,
}

/// Every conversation whose folder is `root` or a subfolder of it. Blocking (reads files).
pub fn find_under(root: &Path) -> Vec<Session> {
    let Some(projects) = claude_dir().map(|d| d.join("projects")) else { return vec![] };
    find_in(&projects, root)
}

fn find_in(projects: &Path, root: &Path) -> Vec<Session> {
    let enc = encode(root);
    let Ok(dirs) = std::fs::read_dir(projects) else { return vec![] };
    let mut found = Vec::new();
    for dir in dirs.flatten() {
        let name = normalize(&dir.file_name().to_string_lossy());
        // `<root>` itself, or something that may be a subfolder (checked below, since
        // `/code/app-web` encodes like a subfolder of `/code/app`).
        if name != enc && !name.starts_with(&format!("{enc}-")) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Some(mut s) = read(&path)
            {
                // A file without a `cwd` can only be placed by its directory name.
                if s.cwd.as_os_str().is_empty() {
                    if name != enc {
                        continue;
                    }
                    s.cwd = root.to_path_buf();
                }
                // Resuming needs the folder; one that's gone can't come back.
                let Ok(cwd) = s.cwd.canonicalize() else { continue };
                if cwd.starts_with(root) {
                    s.cwd = cwd;
                    found.push(s);
                }
            }
        }
    }
    found
}

/// Read one conversation file. `None` if you never typed a prompt in it.
fn read(path: &Path) -> Option<Session> {
    let id = path.file_stem()?.to_str()?.to_string();
    let file = File::open(path).ok()?;
    let updated_at = file
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut cwd = None;
    let mut prompt: Option<String> = None;
    let mut prompts = 0;
    let mut ai_title = None;
    let mut custom_title = None;

    // Files can be tens of MB: only parse the lines that can matter (cheap substring
    // checks first).
    for line in BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { break };
        let Ok(line) = std::str::from_utf8(&line) else { continue };
        if line.contains("\"custom-title\"") {
            if let Some(t) = field(line, "customTitle") {
                custom_title = Some(t);
            }
        } else if line.contains("\"ai-title\"") {
            if let Some(t) = field(line, "aiTitle") {
                ai_title = Some(t);
            }
        } else if line.contains("\"type\":\"user\"") && !line.contains("\"tool_result\"") {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["type"] != "user" || v["isSidechain"] == true || v["isMeta"] == true {
                continue;
            }
            if cwd.is_none() {
                cwd = v["cwd"].as_str().map(PathBuf::from);
            }
            if let Some(text) = typed_text(&v["message"]["content"]) {
                prompts += 1;
                prompt.get_or_insert(text);
            }
        }
    }

    let prompt = prompt?;
    let title = custom_title.or(ai_title).unwrap_or_else(|| shorten(&prompt, 80));
    Some(Session { id, cwd: cwd.unwrap_or_default(), title, prompt, prompts, updated_at })
}

/// A string field of a JSON line.
fn field(line: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = v[key].as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// What a person typed, skipping what Claude Code injects itself (command output,
/// reminders… which start with `<`).
fn typed_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.as_str(),
        Value::Array(parts) => parts.iter().find(|p| p["type"] == "text")?["text"].as_str()?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

fn shorten(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Tools that write files, and the input field that holds the path.
const EDIT_TOOLS: [(&str, &str); 4] =
    [("Edit", "file_path"), ("MultiEdit", "file_path"), ("Write", "file_path"), ("NotebookEdit", "notebook_path")];

/// Files a conversation edited (with its subagents), absolute. Files changed some other way
/// (a shell command) aren't known. Cheap to call again: each transcript is read once, then
/// only what was appended since.
pub fn touched_files(cwd: &Path, session: &str) -> Vec<PathBuf> {
    let Some(projects) = claude_dir().map(|d| d.join("projects")) else { return vec![] };
    let file = format!("{session}.jsonl");
    // Usually in the folder's directory; otherwise look in all of them.
    let main = projects.join(encode(cwd)).join(&file);
    let main = if main.exists() {
        main
    } else {
        let found = std::fs::read_dir(&projects)
            .into_iter()
            .flatten()
            .flatten()
            .map(|d| d.path().join(&file))
            .find(|p| p.exists());
        match found {
            Some(p) => p,
            None => return vec![],
        }
    };
    let mut transcripts = vec![main.clone()];
    if let Ok(subs) = std::fs::read_dir(main.with_extension("").join("subagents")) {
        transcripts.extend(subs.flatten().map(|f| f.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")));
    }
    let mut all = std::collections::BTreeSet::new();
    for t in transcripts {
        all.extend(edits_in(&t, cwd));
    }
    all.into_iter().collect()
}

/// Per transcript: how far it was read, and the files found so far.
type EditCache = std::collections::HashMap<PathBuf, (u64, HashSet<PathBuf>)>;
static EDITS: std::sync::Mutex<Option<EditCache>> = std::sync::Mutex::new(None);

fn edits_in(path: &Path, cwd: &Path) -> HashSet<PathBuf> {
    use std::io::{Seek, SeekFrom};
    let mut cache = EDITS.lock().unwrap();
    let cache = cache.get_or_insert_with(Default::default);
    let (mut offset, mut files) = cache.remove(path).unwrap_or_default();
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len < offset {
        (offset, files) = (0, HashSet::new()); // rewritten: start over
    }
    if let Ok(mut f) = File::open(path)
        && f.seek(SeekFrom::Start(offset)).is_ok()
    {
        let mut reader = BufReader::new(f);
        let mut line = Vec::new();
        // Only whole lines: one still being written is read next time.
        while let Ok(n) = reader.read_until(b'\n', &mut line) {
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            offset += n as u64;
            if let Ok(text) = std::str::from_utf8(&line)
                && text.contains("\"tool_use\"")
                && (text.contains("\"file_path\"") || text.contains("\"notebook_path\""))
            {
                add_edits(text, cwd, &mut files);
            }
            line.clear();
        }
    }
    cache.insert(path.to_path_buf(), (offset, files.clone()));
    files
}

fn add_edits(line: &str, cwd: &Path, files: &mut HashSet<PathBuf>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let Some(parts) = v["message"]["content"].as_array() else { return };
    for p in parts.iter().filter(|p| p["type"] == "tool_use") {
        let name = p["name"].as_str().unwrap_or_default();
        if let Some((_, field)) = EDIT_TOOLS.iter().find(|(tool, _)| *tool == name)
            && let Some(file) = p["input"][field].as_str()
        {
            let file = cwd.join(file); // absolute paths stay as they are
            files.insert(canonical(&file));
        }
    }
}

/// Like `canonicalize`, also for a file that no longer exists (deleted since): its folder is
/// resolved instead, so paths compare equal to the repo's (`/tmp` vs `/private/tmp`).
fn canonical(file: &Path) -> PathBuf {
    if let Ok(p) = file.canonicalize() {
        return p;
    }
    match (file.parent().and_then(|d| d.canonicalize().ok()), file.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => file.to_path_buf(),
    }
}

/// Ids of the conversations open right now in some Claude Code process.
pub fn open_now() -> HashSet<String> {
    let Some(dir) = claude_dir().map(|d| d.join("sessions")) else { return HashSet::new() };
    let Ok(files) = std::fs::read_dir(dir) else { return HashSet::new() };
    files
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|f| {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(f.path()).ok()?).ok()?;
            let pid = i32::try_from(v["pid"].as_i64()?).ok()?;
            let id = v["sessionId"].as_str()?.to_string();
            is_alive(pid).then_some(id)
        })
        .collect()
}

fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // Signal 0 checks that the process exists without touching it. EPERM means it exists
    // but belongs to someone else.
    // SAFETY: `kill` with signal 0 has no side effects.
    let ok = unsafe { libc::kill(pid, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(encode(Path::new("/Users/me/.philharmonic/wt/app")), "-Users-me--philharmonic-wt-app");
    }

    fn write_session(projects: &Path, cwd: &Path, id: &str, lines: &[String]) {
        let dir = projects.join(encode(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
    }

    fn user(cwd: &Path, content: Value) -> String {
        serde_json::json!({ "type": "user", "cwd": cwd, "message": { "role": "user", "content": content } }).to_string()
    }

    #[test]
    fn finds_sessions_in_a_folder_and_its_subfolders_only() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path().canonicalize().unwrap();
        let projects = tmp.join("projects");
        let root = tmp.join("app");
        let sub = root.join("web");
        let sibling = tmp.join("app-web"); // encodes like a subfolder, but isn't one
        for d in [&sub, &sibling] {
            std::fs::create_dir_all(d).unwrap();
        }

        write_session(
            &projects,
            &root,
            "a",
            &[
                r#"{"type":"mode","mode":"normal"}"#.to_string(),
                user(&root, serde_json::json!("<command-name>/clear</command-name>")),
                user(&root, serde_json::json!("Add rate limiting")),
                user(&root, serde_json::json!([{ "type": "tool_result", "content": "x" }])),
                user(&root, serde_json::json!([{ "type": "text", "text": "and tests" }])),
                r#"{"type":"ai-title","aiTitle":"Rate limiting"}"#.to_string(),
            ],
        );
        write_session(&projects, &sub, "b", &[user(&sub, serde_json::json!("Fix the web build"))]);
        write_session(&projects, &sibling, "c", &[user(&sibling, serde_json::json!("Not ours"))]);
        // No prompt of yours: not worth listing.
        write_session(&projects, &root, "empty", &[r#"{"type":"mode"}"#.to_string()]);
        // Its folder is gone: can't be resumed.
        let gone = root.join("gone");
        write_session(&projects, &gone, "d", &[user(&gone, serde_json::json!("Old"))]);

        let mut found = find_in(&projects, &root);
        found.sort_by(|x, y| x.id.cmp(&y.id));

        let ids: Vec<_> = found.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(found[0].title, "Rate limiting");
        assert_eq!(found[0].prompt, "Add rate limiting");
        assert_eq!(found[0].prompts, 2);
        assert_eq!(found[0].cwd, root);
        assert_eq!(found[1].title, "Fix the web build");
        assert_eq!(found[1].cwd, sub);
    }

    #[test]
    fn a_custom_title_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let projects = root.join("projects");
        write_session(
            &projects,
            &root,
            "a",
            &[
                user(&root, serde_json::json!("hi")),
                r#"{"type":"ai-title","aiTitle":"Greeting"}"#.to_string(),
                r#"{"type":"custom-title","customTitle":"My name"}"#.to_string(),
            ],
        );
        assert_eq!(find_in(&projects, &root)[0].title, "My name");
    }

    #[test]
    fn finds_edited_files_and_reads_only_what_was_appended() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().canonicalize().unwrap();
        let t = cwd.join("s.jsonl");
        let edit = |name: &str, field: &str, file: &str| {
            serde_json::json!({ "type": "assistant", "message": { "content": [
                { "type": "text", "text": "file_path" },
                { "type": "tool_use", "name": name, "input": { field: file, "content": "x" } },
            ] } })
            .to_string()
        };
        let read = edit("Read", "file_path", "/elsewhere/not-an-edit.rs");
        std::fs::write(&t, format!("{}\n{}\n", edit("Write", "file_path", "src/a.rs"), read)).unwrap();
        let mut got: Vec<_> = edits_in(&t, &cwd).into_iter().collect();
        assert_eq!(got, [cwd.join("src/a.rs")]);

        // Appended later, plus a half-written line (not read until it's complete).
        let more = format!("{}\n{}", edit("NotebookEdit", "notebook_path", &cwd.join("n.ipynb").display().to_string()), &edit("Edit", "file_path", "b.rs")[..20]);
        std::fs::OpenOptions::new().append(true).open(&t).unwrap().write_all(more.as_bytes()).unwrap();
        got = edits_in(&t, &cwd).into_iter().collect();
        got.sort();
        assert_eq!(got, [cwd.join("n.ipynb"), cwd.join("src/a.rs")]);
    }

    #[test]
    fn our_own_process_is_alive() {
        assert!(is_alive(std::process::id() as i32));
        assert!(!is_alive(0));
    }
}
