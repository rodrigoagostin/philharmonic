//! Turns each `Request` into a `Response`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{
    ActivityKind, Agent, ContextId, Event, FoundSession, Request, Response, Scope, ShipMethod, Task, TaskId, TaskStatus,
    Usage, now, paths, slug::slugify,
};

use crate::activity_log::{self, record};
use crate::agent::{self, Start};
use crate::context_store;
use crate::daemon::Shared;
use crate::git;
use crate::sessions;
use crate::hooks;

pub async fn handle(request: Request, d: &Shared) -> Response {
    match dispatch(request, d).await {
        Ok(response) => response,
        // `{:#}` prints the error *and* every `.context(...)` added on the way up.
        Err(e) => Response::Error { message: format!("{e:#}") },
    }
}

async fn dispatch(request: Request, d: &Shared) -> Result<Response> {
    Ok(match request {
        Request::Ping => Response::Pong {
            // `env!` reads the version from Cargo.toml at compile time.
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            exe: d.exe.clone(),
            exe_mtime: d.exe_mtime,
            exe_hash: d.exe_hash,
        },
        Request::Shutdown => {
            // Answer first, then stop: the main loop stops the agents and exits.
            let d = d.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                d.shutdown.notify_one();
            });
            Response::Ok
        }

        // ---- tasks ----
        Request::NewTask { prompt, repo, base, branch, agent, model, auto_approve, context, collections } => {
            let task = new_task(d, prompt, repo, base, branch, agent, model, auto_approve, context, collections).await?;
            Response::TaskCreated { task }
        }
        Request::ListTasks => Response::Tasks { tasks: d.state.lock().unwrap().list() },
        Request::Stop { id } => {
            existing(d, id)?;
            if !agent::kill(d, id) {
                bail!("task {id} isn't running");
            }
            Response::Ok
        }
        Request::Resume { id } => {
            let task = existing(d, id)?;
            // Claude Code can only `--continue` a conversation that exists. If the first
            // prompt never got through (stopped at the trust question, daemon restarted,
            // first start failed…), give it the prompt again.
            let fresh = task.agent == Agent::Claude && !task.started;
            let start = if fresh {
                Start::Fresh { prompt: first_prompt(&task.prompt, &task_context(d, id)) }
            } else {
                Start::Resume
            };
            // Two Claude Code processes on one conversation would talk over each other.
            if let Some(session) = &task.session
                && sessions::open_now().contains(session)
                && !d.procs.lock().unwrap().contains_key(&id)
            {
                bail!("this conversation is open in another terminal. Quit Claude Code there first, then resume it here");
            }
            start_agent(d, &task, start, task.status)?;
            record(d, id, None, ActivityKind::Lifecycle, "Resumed");
            // Claude Code comes back idle at its prompt (`--continue` doesn't start working by
            // itself), so it's waiting for you; its hooks switch it to Running when you send
            // something. Other agents don't report, so all we know is that they're running.
            let idle = matches!(task.agent, Agent::Claude) && !fresh;
            let task = d
                .update_task(id, |t| {
                    t.doing = None;
                    if idle && t.status == TaskStatus::Running {
                        t.status = TaskStatus::NeedsInput;
                    }
                })
                .with_context(|| format!("no task {id}"))?;
            Response::Task { task }
        }
        Request::Discard { id } => {
            let task = existing(d, id)?;
            agent::kill(d, id);
            // Give the process a moment to let go of its files.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // An imported thread lives in your folder, on your branch: only forget it.
            if !task.adopted {
                // Best effort: a task whose repo or folder is gone must still be discardable.
                let problems = git::remove(&task.repo, &task.worktree, &task.branch, &paths::worktrees_dir()?).await;
                for p in &problems {
                    tracing::warn!(task = id, "discard: {p}");
                }
            }
            context_store::remove_for_task(d, id);
            agent::forget_terminal(d, id);
            d.state.lock().unwrap().remove(id);
            activity_log::remove(id);
            d.save();
            d.emit(Event::TaskRemoved { id });
            Response::Ok
        }
        Request::MarkDone { id } => {
            existing(d, id)?;
            let task = set_status(d, id, TaskStatus::Done)?;
            agent::kill(d, id);
            Response::Task { task }
        }

        // ---- terminal ----
        Request::Input { id, data } => {
            let bytes = B64.decode(data.as_bytes()).context("input isn't valid base64")?;
            agent::write(d, id, bytes).await?;
            Response::Ok
        }
        Request::Resize { id, cols, rows } => {
            agent::resize(d, id, cols.max(10), rows.max(5))?;
            Response::Ok
        }
        Request::Screen { id, history } => {
            let (history, text, end, cols, rows) = agent::screen(d, id, history);
            Response::Screen { history, text, end, cols, rows }
        }
        Request::ScreenText { id } => Response::ScreenText { text: agent::screen_text(d, id) },
        Request::Send { id, text, context } => {
            existing(d, id)?;
            let items = context_store::get(d, &context);
            let mut message = format!("{}{}", text.trim(), context_store::describe(&items));
            // Collections added to the task since its agent started: tell it about them now.
            let task = existing(d, id)?;
            let new = agent::unbriefed(d, id, &task.collections);
            if let Some((brief, _)) = context_store::brief(d, &new, "\n\n---\nThe user added context to this task:") {
                message.push_str(&brief);
            }
            agent::send_message(d, id, &message).await?;
            agent::mark_briefed(d, id, &new);
            if existing(d, id)?.agent != Agent::Claude {
                // Claude Code logs it through its UserPromptSubmit hook.
                record(d, id, None, ActivityKind::Prompt, hooks::shorten(&message, 300));
            }
            Response::Ok
        }

        // ---- hooks ----
        Request::Hook { id, payload } => {
            hooks::on_hook(d, id, &payload);
            Response::Ok
        }
        Request::Activity { id, limit } => Response::Activity {
            items: match id {
                Some(id) => activity_log::for_task(id, limit),
                None => activity_log::recent(d, limit),
            },
        },

        // ---- review & ship ----
        Request::Diff { id, patch } => {
            let task = existing(d, id)?;
            // An agent opened in a folder without git: there's nothing to compare against.
            if task.base.is_empty() {
                Response::Diff { files: Vec::new(), patch: String::new() }
            } else {
                // Who edited what, from the conversation (main agent and subagents).
                let edits = match task.session.clone() {
                    Some(s) => {
                        let cwd = task.worktree.clone();
                        tokio::task::spawn_blocking(move || sessions::edits(&cwd, &s)).await?
                    }
                    None => Default::default(),
                };
                // A thread working in your own folder shares it with you and other threads:
                // show only the files this conversation edited.
                let only: Option<Vec<PathBuf>> = task.adopted.then(|| edits.keys().cloned().collect());
                let (mut files, patch) = git::diff(&task.worktree, &task.base, patch, only.as_deref()).await?;
                if !edits.is_empty() {
                    // numstat paths are relative to the repo's top folder.
                    let top = git::toplevel(&task.worktree).await?;
                    for f in &mut files {
                        f.by = edits.get(&top.join(&f.path)).cloned().unwrap_or_default();
                    }
                }
                Response::Diff { files, patch }
            }
        }
        Request::Ship { id, how } => {
            let task = existing(d, id)?;
            let message = ship(&task, how).await?;
            set_status(d, id, TaskStatus::Done)?;
            record(d, id, None, ActivityKind::Lifecycle, message.clone());
            agent::kill(d, id);
            Response::Shipped { message }
        }

        // ---- context ----
        Request::AddContext { scope, kind, name, data, text } => {
            Response::ContextAdded { item: context_store::add(d, scope, kind, name, data, text)? }
        }
        Request::ListContext => Response::Context { items: context_store::list(d) },
        Request::MoveContext { id, scope } => {
            context_store::move_to(d, id, scope)?;
            Response::Ok
        }
        Request::RemoveContext { id } => {
            context_store::remove(d, id)?;
            Response::Ok
        }

        // ---- existing threads ----
        Request::Discover { repo } => Response::Discovered { sessions: discover(d, &repo).await? },
        Request::Import { repo, sessions } => Response::Tasks { tasks: import(d, &repo, &sessions).await? },
        Request::OpenAgent { folder, prompt } => Response::Task { task: open_agent(d, &folder, prompt).await? },

        // ---- collections ----
        Request::ListCollections => Response::Collections { collections: context_store::collections(d) },
        Request::SaveCollection { id, name, repos } => {
            Response::Collection { collection: context_store::save_collection(d, id, name, repos)? }
        }
        Request::DeleteCollection { id } => {
            context_store::delete_collection(d, id)?;
            Response::Ok
        }
        Request::SetTaskCollections { id, collections } => {
            existing(d, id)?;
            let collections = context_store::existing_collections(d, &collections);
            Response::Task { task: d.update_task(id, |t| t.collections = collections).context("no such task")? }
        }

        // ---- misc ----
        Request::Branches { repo } => Response::Branches { names: git::branches(&repo).await? },
        Request::Subscribe => bail!("subscribe is handled by the connection loop"),
    })
}

fn existing(d: &Shared, id: TaskId) -> Result<Task> {
    d.task(id).with_context(|| format!("no task {id}"))
}

fn set_status(d: &Shared, id: TaskId, status: TaskStatus) -> Result<Task> {
    d.update_task(id, |t| t.status = status).with_context(|| format!("no task {id}"))
}

#[allow(clippy::too_many_arguments)]
async fn new_task(
    d: &Shared,
    prompt: String,
    repo: PathBuf,
    base: String,
    branch: Option<String>,
    agent_kind: Agent,
    model: Option<String>,
    auto_approve: bool,
    context: Vec<ContextId>,
    collections: Option<Vec<philharmonic_core::CollectionId>>,
) -> Result<Task> {
    // An empty prompt is fine: Claude Code opens at its prompt and waits for you.
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() && !matches!(agent_kind, Agent::Claude) {
        bail!("a custom command needs a prompt (it gets it in $PH_PROMPT)");
    }
    // One spelling per repo (`/tmp/x` and `/private/tmp/x` are the same repo), whichever
    // client sent it, so filters and project context line up.
    let repo = std::fs::canonicalize(&repo).with_context(|| format!("repo not found: {}", repo.display()))?;
    // Tasks run in git worktrees, but any folder will do: one that isn't a repo yet (a
    // brand-new project) quietly becomes one, with an empty first commit to branch from.
    let base = if git::is_repo(&repo).await && git::has_commits(&repo).await {
        base
    } else {
        git::init(&repo).await.context("setting up git in the project folder")?;
        // The app can't list branches before that, so it may not have sent a real one.
        let names = git::branches(&repo).await?;
        if names.contains(&base) { base } else { git::current_branch(&repo).await? }
    };
    // No base picked (quick start): the branch work usually merges into.
    let base = if base.trim().is_empty() { git::default_branch(&repo).await? } else { base };
    let folder_name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let repo_dir = paths::worktrees_dir()?.join(git::repo_name(&repo));
    let (branch, dest) = match branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        // You named the branch (e.g. with a ticket key): use it exactly, or say why not.
        Some(name) => {
            let name = git::check_branch_name(&repo, name).await?;
            if git::branch_exists(&repo, &name).await? {
                bail!("the branch {name} already exists");
            }
            // The folder is named after the branch: `feature/ABC-123-x` -> `feature-abc-123-x`.
            let slug = slugify(&name, 60);
            let mut n = 1;
            let dest = loop {
                let dest = repo_dir.join(if n == 1 { slug.clone() } else { format!("{slug}-{n}") });
                if !dest.exists() {
                    break dest;
                }
                n += 1;
            };
            (name, dest)
        }
        // Otherwise `ph/<from the prompt>`. If it's taken (same prompt twice), try `-2`, `-3`, …
        None => {
            let base_slug = match slugify(&prompt, 40) {
                s if s.is_empty() => "agent".to_string(),
                s => s,
            };
            let mut n = 1;
            loop {
                let slug = if n == 1 { base_slug.clone() } else { format!("{base_slug}-{n}") };
                let branch = format!("ph/{slug}");
                let dest = repo_dir.join(&slug);
                if !dest.exists() && !git::branch_exists(&repo, &branch).await? {
                    break (branch, dest); // `break value` makes the loop return that value
                }
                n += 1;
            }
        }
    };

    // Create the worktree. This `.await`s on git, so it happens BEFORE we lock the state:
    // other clients can keep using the daemon while git runs.
    git::create(&repo, &branch, &base, &dest).await?;
    // Files git doesn't track (so a worktree doesn't have them) but the agent needs:
    // your local Claude settings and whatever the repo lists in `.worktreeinclude`.
    let copied = git::copy_untracked(&repo, &dest).await;

    let chosen = match &collections {
        Some(ids) => context_store::existing_collections(d, ids),
        None => context_store::linked_to(d, &repo),
    };

    // Register the task. The lock is held only inside this block.
    let task = {
        let mut state = d.state.lock().unwrap();
        let task = Task {
            id: state.next_id(),
            title: if prompt.is_empty() { format!("Agent in {folder_name} (own copy)") } else { title_from(&prompt) },
            prompt: prompt.clone(),
            repo,
            base,
            branch,
            worktree: dest,
            agent: agent_kind,
            model: model.filter(|m| !m.trim().is_empty()),
            auto_approve,
            status: TaskStatus::Queued,
            created_at: now(),
            started: false,
            session: None,
            adopted: false,
            collections: chosen,
            doing: None,
            plan: Vec::new(),
            subagents: Vec::new(),
            usage: Usage::default(),
            tool_calls: 0,
            summary: None,
            last_activity: now(),
        };
        state.insert(task.clone());
        task
    };
    d.save();
    d.emit(Event::TaskUpdated { task: task.clone() });
    match copied {
        Ok(files) if !files.is_empty() => {
            let more = if files.len() > 5 { format!(" and {} more", files.len() - 5) } else { String::new() };
            let names = files.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            record(d, task.id, None, ActivityKind::Lifecycle, format!("Copied into the worktree: {names}{more}"));
        }
        Ok(_) => {}
        Err(e) => record(d, task.id, None, ActivityKind::Lifecycle, format!("Couldn't copy local files: {e:#}")),
    }

    // Files attached in the New task window move to the task; items picked from a
    // collection stay there. All of them are listed in the first prompt.
    for item in context_store::get(d, &context) {
        if item.scope == Scope::Draft {
            let _ = context_store::move_to(d, item.id, Scope::Task { id: task.id });
        }
    }
    let items = context_store::get(d, &context);
    let start = Start::Fresh { prompt: first_prompt(&prompt, &items) };
    let running = start_agent(d, &task, start, TaskStatus::Failed)
        .map_err(|e| e.context("the worktree was created, but the agent couldn't start"))?;
    if prompt.is_empty() && items.is_empty() {
        // Nothing to work on yet: it waits for you.
        return d
            .update_task(task.id, |t| {
                if t.status == TaskStatus::Running {
                    t.status = TaskStatus::NeedsInput;
                }
            })
            .context("no such task");
    }
    Ok(running)
}

/// Mark the task running, then start its agent. In that order: the agent may report
/// "needs you" (or exit) right away, and that must not be overwritten by "running".
/// If it can't start, the status becomes `on_error`.
fn start_agent(d: &Shared, task: &Task, start: Start, on_error: TaskStatus) -> Result<Task> {
    if d.procs.lock().unwrap().contains_key(&task.id) {
        bail!("task {} is already running", task.id);
    }
    let running = set_status(d, task.id, TaskStatus::Running)?;
    if let Err(e) = agent::spawn(d, task, start) {
        set_status(d, task.id, on_error)?;
        return Err(e);
    }
    Ok(running)
}

/// The prompt plus the list of attached context.
fn first_prompt(prompt: &str, items: &[philharmonic_core::ContextItem]) -> String {
    format!("{}{}", prompt.trim(), context_store::describe(items))
}

/// Context items that belong to a task.
fn task_context(d: &Shared, id: TaskId) -> Vec<philharmonic_core::ContextItem> {
    context_store::list(d).into_iter().filter(|i| i.scope == Scope::Task { id }).collect()
}

/// First line of the prompt, shortened for lists.
fn title_from(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_string()
    }
}

async fn ship(task: &Task, how: ShipMethod) -> Result<String> {
    if task.base.is_empty() {
        bail!("this folder isn't a git repository, so there's nothing to merge or open a PR from");
    }
    // Imported threads can work right on `main`, or on a detached HEAD.
    if task.branch == task.base || task.branch == "HEAD" {
        bail!(
            "this task works directly on '{}', not on a branch of its own, so there's nothing to merge or open a PR from. Commit and push it yourself",
            task.branch
        );
    }
    let message = format!("{}\n\n{}\n\n(Philharmonic task {})", task.title, task.prompt, task.id);
    git::commit_all(&task.worktree, &message).await?;
    match how {
        ShipMethod::Pr => {
            let url = git::open_pr(&task.worktree, &task.branch, &task.base, &task.title, &task.prompt).await?;
            Ok(format!("Opened pull request: {url}"))
        }
        ShipMethod::Merge => {
            git::merge_into_base(&task.repo, &task.base, &task.branch).await?;
            Ok(format!("Merged {} into {}", task.branch, task.base))
        }
    }
}

/// The Claude Code conversations already started in a repo: in its main checkout, its
/// worktrees, or their subfolders. Newest first.
async fn discover(d: &Shared, repo: &std::path::Path) -> Result<Vec<FoundSession>> {
    // A git repo: its main checkout and every worktree. Any other folder: just itself.
    let git_checkouts = git::worktrees(repo).await.ok();
    let roots = match &git_checkouts {
        Some(c) => c.clone(),
        None => vec![std::fs::canonicalize(repo).with_context(|| format!("folder not found: {}", repo.display()))?],
    };
    let found = tokio::task::spawn_blocking(move || {
        let mut all: Vec<sessions::Session> = Vec::new();
        for root in &roots {
            for s in sessions::find_under(root) {
                if !all.iter().any(|a| a.id == s.id) {
                    all.push(s);
                }
            }
        }
        all
    })
    .await?;

    let open = sessions::open_now();
    let tasks = d.state.lock().unwrap().list();
    let mut out = Vec::new();
    for s in found {
        // A subfolder may be a different repo (one nested inside this one): skip those.
        if let Some(checkouts) = &git_checkouts {
            match git::toplevel(&s.cwd).await {
                Ok(top) if checkouts.contains(&top) => {}
                _ => continue,
            }
        }
        let branch = git::current_branch(&s.cwd).await.unwrap_or_default();
        let task = tasks
            .iter()
            .find(|t| t.session.as_deref() == Some(s.id.as_str()) || (!t.adopted && t.session.is_none() && t.worktree == s.cwd))
            .map(|t| t.id);
        out.push(FoundSession {
            open_elsewhere: open.contains(&s.id),
            id: s.id,
            cwd: s.cwd,
            branch,
            title: s.title,
            prompt: s.prompt,
            prompts: s.prompts,
            updated_at: s.updated_at,
            task,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    Ok(out)
}

/// Where a task in `folder` belongs: (the repo's main checkout, its default branch), so
/// merges go there whichever folder was picked. A folder without git: (itself, "").
async fn home_of(folder: &std::path::Path) -> Result<(PathBuf, String)> {
    match git::worktrees(folder).await {
        Ok(checkouts) => {
            let main = checkouts.into_iter().next().context("no main checkout")?;
            let base = git::default_branch(&main).await?;
            Ok((main, base))
        }
        Err(_) => Ok((std::fs::canonicalize(folder)?, String::new())),
    }
}

/// Start Claude Code right in your folder, as it is: no worktree, no branch of ours.
async fn open_agent(d: &Shared, folder: &std::path::Path, prompt: Option<String>) -> Result<Task> {
    let folder = std::fs::canonicalize(folder).with_context(|| format!("folder not found: {}", folder.display()))?;
    if !folder.is_dir() {
        bail!("{} isn't a folder", folder.display());
    }
    let (repo, base) = home_of(&folder).await?;
    let branch = if base.is_empty() { String::new() } else { git::current_branch(&folder).await.unwrap_or_default() };
    let prompt = prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string());
    let collections = context_store::linked_to(d, &repo);
    let task = {
        let mut state = d.state.lock().unwrap();
        let task = Task {
            id: state.next_id(),
            title: prompt.as_deref().map(title_from).unwrap_or_else(|| format!("Agent in {name}")),
            prompt: prompt.clone().unwrap_or_default(),
            repo,
            base,
            branch,
            worktree: folder,
            agent: Agent::Claude,
            model: None,
            auto_approve: false,
            status: TaskStatus::Queued,
            created_at: now(),
            started: false,
            session: None,
            adopted: true,
            collections,
            doing: None,
            plan: Vec::new(),
            subagents: Vec::new(),
            usage: Usage::default(),
            tool_calls: 0,
            summary: None,
            last_activity: now(),
        };
        state.insert(task.clone());
        task
    };
    d.save();
    d.emit(Event::TaskUpdated { task: task.clone() });
    record(d, task.id, None, ActivityKind::Lifecycle, format!("Opened in {}", task.worktree.display()));
    // No prompt: Claude Code opens at its prompt, waiting for you.
    let start = Start::Fresh { prompt: task.prompt.clone() };
    let running = start_agent(d, &task, start, TaskStatus::Failed)?;
    if prompt.is_none() {
        return d
            .update_task(task.id, |t| {
                if t.status == TaskStatus::Running {
                    t.status = TaskStatus::NeedsInput;
                }
            })
            .context("no such task");
    }
    Ok(running)
}

/// Turn found conversations into stopped tasks. Ones that are already tasks are skipped.
async fn import(d: &Shared, repo: &std::path::Path, ids: &[String]) -> Result<Vec<Task>> {
    let found = discover(d, repo).await?;
    let (main, base) = home_of(repo).await?;
    // Like a new task there: it uses the collections linked to the repo.
    let collections = context_store::linked_to(d, &main);
    // Their subagents ran outside Philharmonic (no hook reported them): read them from disk.
    let wanted: Vec<(String, PathBuf)> =
        found.iter().filter(|f| ids.contains(&f.id)).map(|f| (f.id.clone(), f.cwd.clone())).collect();
    let subagents: std::collections::HashMap<String, Vec<philharmonic_core::Subagent>> =
        tokio::task::spawn_blocking(move || {
            wanted.into_iter().map(|(id, cwd)| { let list = sessions::subagents(&cwd, &id); (id, list) }).collect()
        })
        .await?;

    let mut created = Vec::new();
    for id in ids {
        let f = found.iter().find(|f| &f.id == id).with_context(|| format!("no conversation {id} in this repo"))?;
        if f.task.is_some() {
            continue;
        }
        let task = {
            let mut state = d.state.lock().unwrap();
            let task = Task {
                id: state.next_id(),
                title: f.title.clone(),
                prompt: f.prompt.clone(),
                repo: main.clone(),
                base: base.clone(),
                branch: f.branch.clone(),
                worktree: f.cwd.clone(),
                agent: Agent::Claude,
                model: None,
                auto_approve: false,
                status: TaskStatus::Stopped,
                created_at: f.updated_at,
                started: true,
                session: Some(f.id.clone()),
                adopted: true,
                collections: collections.clone(),
                doing: None,
                plan: Vec::new(),
                subagents: subagents.get(&f.id).cloned().unwrap_or_default(),
                usage: Usage::default(),
                tool_calls: 0,
                summary: None,
                last_activity: f.updated_at,
            };
            state.insert(task.clone());
            task
        };
        d.emit(Event::TaskUpdated { task: task.clone() });
        created.push(task);
    }
    d.save();
    Ok(created)
}
