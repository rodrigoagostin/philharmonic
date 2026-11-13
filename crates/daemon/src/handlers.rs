//! Turns each `Request` into a `Response`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use philharmonic_core::{
    ActivityKind, Agent, ContextId, Event, Request, Response, Scope, ShipMethod, Task, TaskId, TaskStatus, Usage,
    now, paths, slug::slugify,
};

use crate::activity_log::{self, record};
use crate::agent::{self, Start};
use crate::context_store;
use crate::daemon::Shared;
use crate::git;
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
            agent::spawn(d, &task, Start::Resume)?;
            record(d, id, None, ActivityKind::Lifecycle, "Resumed");
            // Claude Code comes back idle at its prompt (`--continue` doesn't start working by
            // itself), so it's waiting for you; its hooks switch it to Running when you send
            // something. Other agents don't report, so all we know is that they're running.
            let status = match task.agent {
                Agent::Claude => TaskStatus::NeedsInput,
                Agent::Custom { .. } => TaskStatus::Running,
            };
            d.update_task(id, |t| t.doing = None);
            Response::Task { task: set_status(d, id, status)? }
        }
        Request::Discard { id } => {
            let task = existing(d, id)?;
            agent::kill(d, id);
            // Give the process a moment to let go of its files.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            git::remove(&task.repo, &task.worktree, &task.branch).await?;
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
            agent::write(d, id, &bytes)?;
            Response::Ok
        }
        Request::Resize { id, cols, rows } => {
            agent::resize(d, id, cols.max(10), rows.max(5))?;
            Response::Ok
        }
        Request::Screen { id, history } => {
            let (history, text, cols, rows) = agent::screen(d, id, history);
            Response::Screen { history, text, cols, rows }
        }
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
            let (files, patch) = git::diff(&task.worktree, &task.base, patch).await?;
            Response::Diff { files, patch }
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
    if prompt.trim().is_empty() {
        bail!("the prompt is empty");
    }

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
            let base_slug = slugify(&prompt, 40);
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
    let first_prompt = format!("{}{}", prompt.trim(), context_store::describe(&items));

    if let Err(e) = agent::spawn(d, &task, Start::Fresh { prompt: first_prompt }) {
        set_status(d, task.id, TaskStatus::Failed)?;
        return Err(e.context("the worktree was created, but the agent couldn't start"));
    }
    set_status(d, task.id, TaskStatus::Running)
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
