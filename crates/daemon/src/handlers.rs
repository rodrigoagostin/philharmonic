//! Turns each `Request` into a `Response`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{
    Agent, ContextId, Event, HookEvent, Request, Response, Scope, ShipMethod, Task, TaskId, TaskStatus, now,
    paths, slug::slugify,
};

use crate::agent::{self, Start};
use crate::context_store;
use crate::daemon::Shared;
use crate::git;

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
        },

        // ---- tasks ----
        Request::NewTask { prompt, repo, base, agent, model, auto_approve, context } => {
            let task = new_task(d, prompt, repo, base, agent, model, auto_approve, context).await?;
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
            let start = if task.agent == Agent::Claude && !task.started {
                Start::Fresh { prompt: first_prompt(&task.prompt, &task_context(d, id)) }
            } else {
                Start::Resume
            };
            Response::Task { task: start_agent(d, &task, start, task.status)? }
        }
        Request::Discard { id } => {
            let task = existing(d, id)?;
            agent::kill(d, id);
            // Give the process a moment to let go of its files.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // Best effort: a task whose repo or folder is gone must still be discardable.
            let problems = git::remove(&task.repo, &task.worktree, &task.branch, &paths::worktrees_dir()?).await;
            for p in &problems {
                tracing::warn!(task = id, "discard: {p}");
            }
            context_store::remove_for_task(d, id);
            agent::forget_terminal(d, id);
            d.state.lock().unwrap().remove(id);
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
        Request::Screen { id } => {
            let (history, text, end) = agent::screen(d, id);
            Response::Screen { history, text, end }
        }
        Request::ScreenText { id } => Response::ScreenText { text: agent::screen_text(d, id) },
        Request::Send { id, text, context } => {
            existing(d, id)?;
            let items = context_store::get(d, &context);
            let message = format!("{}{}", text.trim(), context_store::describe(&items));
            agent::send_message(d, id, &message).await?;
            Response::Ok
        }

        // ---- hooks ----
        Request::Notify { id, event, message } => {
            on_hook(d, id, event, &message);
            Response::Ok
        }

        // ---- review & ship ----
        Request::Diff { id } => {
            let task = existing(d, id)?;
            let (files, patch) = git::diff(&task.worktree, &task.base).await?;
            Response::Diff { files, patch }
        }
        Request::Ship { id, how } => {
            let task = existing(d, id)?;
            let message = ship(&task, how).await?;
            set_status(d, id, TaskStatus::Done)?;
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
    agent_kind: Agent,
    model: Option<String>,
    auto_approve: bool,
    context: Vec<ContextId>,
) -> Result<Task> {
    if prompt.trim().is_empty() {
        bail!("the prompt is empty");
    }
    // One spelling per repo (`/tmp/x` and `/private/tmp/x` are the same repo), whichever
    // client sent it, so filters and project context line up.
    let repo = std::fs::canonicalize(&repo).with_context(|| format!("repo not found: {}", repo.display()))?;

    // Names. If the branch or folder is taken (same prompt twice), try `-2`, `-3`, …
    let base_slug = slugify(&prompt, 40);
    let repo_dir = paths::worktrees_dir()?.join(git::repo_name(&repo));
    let mut n = 1;
    let (branch, dest) = loop {
        let slug = if n == 1 { base_slug.clone() } else { format!("{base_slug}-{n}") };
        let branch = format!("ph/{slug}");
        let dest = repo_dir.join(&slug);
        if !dest.exists() && !git::branch_exists(&repo, &branch).await? {
            break (branch, dest); // `break value` makes the loop return that value
        }
        n += 1;
    };

    // Create the worktree. This `.await`s on git, so it happens BEFORE we lock the state:
    // other clients can keep using the daemon while git runs.
    git::create(&repo, &branch, &base, &dest).await?;

    // Register the task. The lock is held only inside this block.
    let task = {
        let mut state = d.state.lock().unwrap();
        let task = Task {
            id: state.next_id(),
            title: title_from(&prompt),
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
        };
        state.insert(task.clone());
        task
    };
    d.save();
    d.emit(Event::TaskUpdated { task: task.clone() });

    // Attached context moves to the task and is listed in the first prompt.
    for id in &context {
        let _ = context_store::move_to(d, *id, Scope::Task { id: task.id });
    }
    let items = context_store::get(d, &context);
    let start = Start::Fresh { prompt: first_prompt(&prompt, &items) };
    start_agent(d, &task, start, TaskStatus::Failed)
        .map_err(|e| e.context("the worktree was created, but the agent couldn't start"))
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

/// Claude Code hooks report what the agent is doing.
fn on_hook(d: &Shared, id: TaskId, event: HookEvent, message: &str) {
    d.update_task(id, |t| {
        if !t.status.is_live() {
            return; // stopped/done tasks stay as they are
        }
        // Hooks only fire once Claude Code has taken the prompt, so from now on there's a
        // conversation to `--continue`.
        if matches!(event, HookEvent::Running | HookEvent::Stopped) {
            t.started = true;
        }
        t.status = match event {
            HookEvent::Running => TaskStatus::Running,
            // Claude also sends "waiting for your input" after a minute of idling. If it
            // finished its turn, keep "review" instead of flipping to "needs you".
            HookEvent::NeedsInput
                if t.status == TaskStatus::Review && message.contains("waiting for your input") =>
            {
                TaskStatus::Review
            }
            HookEvent::NeedsInput => TaskStatus::NeedsInput,
            HookEvent::Stopped => TaskStatus::Review,
        };
    });
}

async fn ship(task: &Task, how: ShipMethod) -> Result<String> {
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
