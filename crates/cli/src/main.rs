//! `ph`: the command-line client. It sends one request to the daemon and prints the answer.

mod client;

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use philharmonic_core::{Agent, HookEvent, Request, Response, ShipMethod, Task, TaskId};

/// Run and steer many coding agents in parallel.
#[derive(Parser)]
#[command(name = "ph", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// Each variant becomes a subcommand; its fields become arguments and flags.
// The `///` doc comments become the `--help` text.
#[derive(Subcommand)]
enum Command {
    /// Check that the daemon is running.
    Ping,
    /// Start a new task: a worktree + branch, with an agent working on the prompt.
    New {
        /// What the agent should do.
        prompt: String,
        /// The repo to work in.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Branch to start from.
        #[arg(long, default_value = "main")]
        base: String,
        /// Model for Claude Code (e.g. claude-opus-5-5).
        #[arg(long)]
        model: Option<String>,
        /// Let the agent edit files without asking.
        #[arg(long)]
        auto_approve: bool,
        /// Run this shell command instead of Claude Code (the prompt is in $PH_PROMPT).
        #[arg(long)]
        cmd: Option<String>,
    },
    /// List all tasks.
    Ls,
    /// Print what's on a task's terminal right now.
    Show { id: TaskId },
    /// Send a message to a task's agent.
    Send { id: TaskId, text: String },
    /// Stop a task's agent (the worktree stays).
    Stop { id: TaskId },
    /// Start a stopped task's agent again, continuing its conversation.
    Resume { id: TaskId },
    /// Show what a task changed.
    Diff { id: TaskId },
    /// Commit and open a pull request, or merge into the base branch.
    Ship {
        id: TaskId,
        #[arg(value_enum)]
        how: How,
    },
    /// Stop the agent, delete the worktree and branch, forget the task.
    Rm { id: TaskId },
    /// List the Claude Code conversations already started in a repo.
    Threads {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Turn existing Claude Code conversations into tasks (all of them if no ids are given).
    Import {
        /// Session ids, from `ph threads`.
        ids: Vec<String>,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Used by Claude Code hooks to report status. Reads the hook's JSON on stdin.
    #[command(hide = true)]
    Notify {
        #[arg(value_enum)]
        event: Hook,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum How {
    Pr,
    Merge,
}

#[derive(Clone, Copy, ValueEnum)]
enum Hook {
    Running,
    NeedsInput,
    Stopped,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Hooks must never break the agent: report quietly and always exit 0.
    if let Command::Notify { event } = cli.command {
        let _ = notify(event).await;
        return Ok(());
    }

    let request = match cli.command {
        Command::Ping => Request::Ping,
        Command::New { prompt, repo, base, model, auto_approve, cmd } => {
            // The daemon runs in a different folder than you, so send an absolute path.
            let repo = repo.canonicalize().with_context(|| format!("repo not found: {}", repo.display()))?;
            let agent = match cmd {
                Some(command) => Agent::Custom { command },
                None => Agent::Claude,
            };
            Request::NewTask { prompt, repo, base, agent, model, auto_approve, context: vec![] }
        }
        Command::Ls => Request::ListTasks,
        Command::Show { id } => Request::ScreenText { id },
        Command::Send { id, text } => Request::Send { id, text, context: vec![] },
        Command::Stop { id } => Request::Stop { id },
        Command::Resume { id } => Request::Resume { id },
        Command::Diff { id } => Request::Diff { id },
        Command::Ship { id, how } => Request::Ship {
            id,
            how: match how {
                How::Pr => ShipMethod::Pr,
                How::Merge => ShipMethod::Merge,
            },
        },
        Command::Rm { id } => Request::Discard { id },
        Command::Threads { repo } => Request::Discover { repo: absolute(&repo)? },
        Command::Import { ids, repo } => {
            let repo = absolute(&repo)?;
            let sessions = if ids.is_empty() {
                // Everything that isn't a task yet.
                match client::send(&Request::Discover { repo: repo.clone() }).await? {
                    Response::Discovered { sessions } => {
                        sessions.into_iter().filter(|s| s.task.is_none()).map(|s| s.id).collect()
                    }
                    Response::Error { message } => bail!("daemon: {message}"),
                    other => bail!("unexpected answer: {other:?}"),
                }
            } else {
                ids
            };
            Request::Import { repo, sessions }
        }
        Command::Notify { .. } => unreachable!("handled above"),
    };

    match client::send(&request).await? {
        Response::Pong { version } => println!("daemon is up (v{version})"),
        Response::Ok => println!("ok"),
        Response::TaskCreated { task } => {
            println!("created task {} on branch {}", task.id, task.branch);
            println!("worktree: {}", task.worktree.display());
        }
        Response::Task { task } => println!("task {}: {}", task.id, task.status),
        Response::Tasks { tasks } => print_tasks(&tasks),
        Response::Screen { text, .. } | Response::ScreenText { text } => println!("{text}"),
        Response::Diff { patch, .. } => print!("{patch}"),
        Response::Shipped { message } => println!("{message}"),
        Response::Discovered { sessions } => print_threads(&sessions),
        Response::Error { message } => bail!("daemon: {message}"),
        other => println!("{other:?}"),
    }
    Ok(())
}

/// Print the tasks as an aligned table:
///
/// ```text
/// ID  STATUS     BRANCH                        TITLE
/// 1   running    ph/add-rate-limiting-to-auth  Add rate limiting to /auth
/// ```
fn print_tasks(tasks: &[Task]) {
    if tasks.is_empty() {
        println!("no tasks yet. try: ph new \"...\"");
        return;
    }

    // The BRANCH column is as wide as the longest branch (at least as wide as its header).
    let branch_width = tasks.iter().map(|t| t.branch.len()).max().unwrap_or(0).max("BRANCH".len());

    // `{:<4}` = left-align in 4 columns; `{:<branch_width$}` takes the width from a variable.
    println!("{:<4}{:<11}{:<branch_width$}  TITLE", "ID", "STATUS", "BRANCH");
    for task in tasks {
        println!("{:<4}{:<11}{:<branch_width$}  {}", task.id, task.status, task.branch, task.title);
    }
}

fn absolute(repo: &std::path::Path) -> Result<PathBuf> {
    repo.canonicalize().with_context(|| format!("repo not found: {}", repo.display()))
}

fn print_threads(sessions: &[philharmonic_core::FoundSession]) {
    if sessions.is_empty() {
        println!("no Claude Code conversations found for this repo");
        return;
    }
    let width = sessions.iter().map(|s| s.branch.len()).max().unwrap_or(0).max("BRANCH".len()) + 2;
    println!("{:<38}{:<8}{:<width$}TITLE", "SESSION", "TASK", "BRANCH");
    for s in sessions {
        let task = match (s.task, s.open_elsewhere) {
            (Some(id), _) => format!("#{id}"),
            (None, true) => "open".to_string(),
            (None, false) => "-".to_string(),
        };
        println!("{:<38}{:<8}{:<width$}{}", s.id, task, s.branch, s.title);
    }
}

/// Called by Claude Code hooks inside a task's terminal: `$PH_TASK_ID` says which task.
async fn notify(event: Hook) -> Result<()> {
    let id: TaskId = std::env::var("PH_TASK_ID")?.parse()?;
    // Hooks get a JSON object on stdin; `message` explains Notification events.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let json = serde_json::from_str::<serde_json::Value>(&input).unwrap_or_default();
    let text = |key: &str| json.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let message = text("message").unwrap_or_default();
    let session_id = text("session_id");
    let event = match event {
        Hook::Running => HookEvent::Running,
        Hook::NeedsInput => HookEvent::NeedsInput,
        Hook::Stopped => HookEvent::Stopped,
    };
    client::send(&Request::Notify { id, event, message, session_id }).await?;
    Ok(())
}
