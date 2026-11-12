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

/// Called by Claude Code hooks inside a task's terminal: `$PH_TASK_ID` says which task.
async fn notify(event: Hook) -> Result<()> {
    let id: TaskId = std::env::var("PH_TASK_ID")?.parse()?;
    // Hooks get a JSON object on stdin; `message` explains Notification events.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let message = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_default();
    let event = match event {
        Hook::Running => HookEvent::Running,
        Hook::NeedsInput => HookEvent::NeedsInput,
        Hook::Stopped => HookEvent::Stopped,
    };
    client::send(&Request::Notify { id, event, message }).await?;
    Ok(())
}
