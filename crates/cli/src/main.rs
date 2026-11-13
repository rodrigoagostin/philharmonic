//! `ph`: the command-line client. It sends one request to the daemon and prints the answer.

mod client;

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use philharmonic_core::{Activity, Agent, Collection, Request, Response, ShipMethod, Task, TaskId};

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
    /// Stop the daemon and its agents (the app starts a new one; resume tasks after).
    Shutdown,
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
        /// Name of the new branch (default: ph/<from the prompt>), e.g. feature/ABC-123-login.
        #[arg(long)]
        branch: Option<String>,
        /// Model for Claude Code (e.g. claude-opus-5-5).
        #[arg(long)]
        model: Option<String>,
        /// Let the agent edit files without asking.
        #[arg(long)]
        auto_approve: bool,
        /// Run this shell command instead of Claude Code (the prompt is in $PH_PROMPT).
        #[arg(long)]
        cmd: Option<String>,
        /// Use this collection's context (repeat for several). Default: the collections
        /// linked to the repo.
        #[arg(short = 'c', long = "collection")]
        collections: Vec<String>,
    },
    /// List the context collections.
    Collections,
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
    /// What a task's agent did (or every agent's latest actions, without an id).
    Log {
        id: Option<TaskId>,
        /// How many lines.
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },
    /// Used by Claude Code hooks: forwards the hook's JSON (stdin) to the daemon.
    #[command(hide = true)]
    Notify {
        /// Ignored (older hook settings passed an event name).
        _event: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum How {
    Pr,
    Merge,
}


#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Hooks must never break the agent: report quietly and always exit 0.
    if let Command::Notify { .. } = cli.command {
        let _ = notify().await;
        return Ok(());
    }

    let request = match cli.command {
        Command::Ping => Request::Ping,
        Command::Shutdown => Request::Shutdown,
        Command::New { prompt, repo, base, branch, model, auto_approve, cmd, collections } => {
            // The daemon runs in a different folder than you, so send an absolute path.
            let repo = repo.canonicalize().with_context(|| format!("repo not found: {}", repo.display()))?;
            let agent = match cmd {
                Some(command) => Agent::Custom { command },
                None => Agent::Claude,
            };
            let collections = if collections.is_empty() { None } else { Some(collection_ids(&collections).await?) };
            Request::NewTask { prompt, repo, base, branch, agent, model, auto_approve, context: vec![], collections }
        }
        Command::Collections => Request::ListCollections,
        Command::Ls => Request::ListTasks,
        Command::Show { id } => Request::Screen { id, history: false },
        Command::Send { id, text } => Request::Send { id, text, context: vec![] },
        Command::Stop { id } => Request::Stop { id },
        Command::Resume { id } => Request::Resume { id },
        Command::Diff { id } => Request::Diff { id, patch: true },
        Command::Ship { id, how } => Request::Ship {
            id,
            how: match how {
                How::Pr => ShipMethod::Pr,
                How::Merge => ShipMethod::Merge,
            },
        },
        Command::Rm { id } => Request::Discard { id },
        Command::Log { id, lines } => Request::Activity { id, limit: lines },
        Command::Notify { .. } => unreachable!("handled above"),
    };

    match client::send(&request).await? {
        Response::Pong { version, pid, .. } => println!("daemon is up (v{version}, pid {pid})"),
        Response::Ok => println!("ok"),
        Response::TaskCreated { task } => {
            println!("created task {} on branch {}", task.id, task.branch);
            println!("worktree: {}", task.worktree.display());
        }
        Response::Task { task } => println!("task {}: {}", task.id, task.status),
        Response::Tasks { tasks } => print_tasks(&tasks),
        Response::Screen { text, .. } => println!("{text}"),
        Response::Diff { patch, .. } => print!("{patch}"),
        Response::Shipped { message } => println!("{message}"),
        Response::Activity { items } => print_activity(&items),
        Response::Collections { collections } => print_collections(&collections),
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

/// Collection names (any case) to ids.
async fn collection_ids(names: &[String]) -> Result<Vec<u64>> {
    let Response::Collections { collections } = client::send(&Request::ListCollections).await? else {
        bail!("the daemon didn't list its collections");
    };
    names
        .iter()
        .map(|name| {
            collections
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
                .map(|c| c.id)
                .with_context(|| format!("no collection called \"{name}\" (see `ph collections`)"))
        })
        .collect()
}

fn print_collections(collections: &[Collection]) {
    if collections.is_empty() {
        println!("no collections yet (make them in the app's Context screen)");
        return;
    }
    let width = collections.iter().map(|c| c.name.len()).max().unwrap_or(0).max("NAME".len());
    println!("{:<4}{:<width$}  LINKED REPOS", "ID", "NAME");
    for c in collections {
        let repos: Vec<String> = c.repos.iter().map(|r| r.display().to_string()).collect();
        println!("{:<4}{:<width$}  {}", c.id, c.name, if repos.is_empty() { "-".into() } else { repos.join(", ") });
    }
}

/// Called by Claude Code hooks inside a task's terminal: `$PH_TASK_ID` says which task.
async fn notify() -> Result<()> {
    let id: TaskId = std::env::var("PH_TASK_ID")?.parse()?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: serde_json::Value = serde_json::from_str(&input)?;
    client::send(&Request::Hook { id, payload }).await?;
    Ok(())
}

/// `14:02  #3  Editing src/main.rs` (subagent actions are indented).
fn print_activity(items: &[Activity]) {
    if items.is_empty() {
        println!("no activity yet");
        return;
    }
    for a in items {
        let secs = a.at % 86_400;
        let indent = if a.agent.is_some() { "  ↳ " } else { "" };
        let text = a.text.lines().next().unwrap_or_default();
        println!("{:02}:{:02} UTC  #{:<3} {indent}{text}", secs / 3600, secs % 3600 / 60, a.task);
    }
}
