//! Where Philharmonic keeps its files.
//!
//! Default: `~/.philharmonic`. Set `PHILHARMONIC_HOME` to use another folder
//! (handy for tests, or to run a second, throwaway daemon).

use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("PHILHARMONIC_HOME") {
        let dir = PathBuf::from(dir);
        // Always absolute: git and agents run in other folders.
        return Ok(if dir.is_absolute() { dir } else { std::env::current_dir()?.join(dir) });
    }
    // `.context(...)` turns `Option<PathBuf>` into `Result<PathBuf>` with a helpful message.
    let home = dirs::home_dir().context("could not find your home directory")?;
    Ok(home.join(".philharmonic"))
}

/// The Unix socket the daemon listens on.
pub fn socket_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("philharmonicd.sock"))
}

/// Parent folder for every task's worktree: `<data_dir>/wt/<repo-name>/<slug>`.
pub fn worktrees_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("wt"))
}

/// Context shelf files: `<data_dir>/ctx/<scope>/<file>`.
pub fn context_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("ctx"))
}

/// Tasks and context items, saved after every change.
pub fn state_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("state.json"))
}

/// Where the daemon writes its log when the app starts it in the background.
pub fn daemon_log() -> Result<PathBuf> {
    Ok(data_dir()?.join("daemon.log"))
}
