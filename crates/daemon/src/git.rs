//! Git operations: one worktree per task (so agents never edit the same files),
//! plus diff, commit, push/PR, merge and cleanup.

use std::path::Path;

use anyhow::{Result, bail};
use philharmonic_core::FileChange;
use tokio::process::Command;

/// The folder name of a repo, e.g. `/Users/me/code/api` -> `"api"`.
pub fn repo_name(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}

/// Does `repo` already have a local branch called `branch`?
pub async fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    // `git rev-parse --verify --quiet refs/heads/<branch>` exits 0 if it exists, 1 if not.
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .await?
        .status;
    Ok(status.success())
}

/// Create a new worktree at `dest`, on a new branch `branch`, starting from `base`.
///
/// # LEARN #2 — running a process and handling errors
///
/// The git command you want is:
///
/// ```text
/// git -C <repo> worktree add -b <branch> <dest> <base>
/// ```
///
/// Steps:
/// 1. Build it with `Command::new("git")` and `.arg(...)` / `.args([...])`.
///    `Path`s can be passed straight to `.arg()`.
/// 2. Run it with `.output().await?`. That gives you an `Output` with `status`,
///    `stdout` and `stderr` (bytes).
/// 3. If `!output.status.success()`, return an error that includes git's stderr:
///    `String::from_utf8_lossy(&output.stderr)` turns the bytes into text, and
///    `anyhow::bail!("git worktree add failed: {}", ...)` returns early with an error
///    (add `bail` to the `use anyhow::...` line to write just `bail!`).
/// 4. Otherwise return `Ok(())`.
///
/// Notice the `?` after `.await`: if git can't even start (not installed), the error is
/// returned to our caller automatically. That's Rust's main error-handling tool.
pub async fn create(repo: &Path, branch: &str, base: &str, dest: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(branch)
        .arg(dest)
        .arg(base)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(())
}

/// Run `git -C <dir> <args…>`. Returns stdout, or an error carrying git's stderr.
async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().await?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Local branch names, e.g. `["main", "develop"]`.
pub async fn branches(repo: &Path) -> Result<Vec<String>> {
    let out = git(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]).await?;
    Ok(out.lines().map(str::to_string).collect())
}

/// Everything the task changed since its branch left `base`, including uncommitted and
/// new files: (per-file +/- counts, unified diff).
pub async fn diff(worktree: &Path, base: &str) -> Result<(Vec<FileChange>, String)> {
    // Mark new files as "intent to add" so `git diff` shows them too. Harmless if none.
    let _ = git(worktree, &["add", "--intent-to-add", "--all"]).await;
    let merge_base = git(worktree, &["merge-base", base, "HEAD"]).await?;
    let merge_base = merge_base.trim();

    let patch = git(worktree, &["diff", merge_base]).await?;
    let numstat = git(worktree, &["diff", "--numstat", merge_base]).await?;
    let files = numstat
        .lines()
        .filter_map(|line| {
            // "<added>\t<removed>\t<path>" ("-" for binary files)
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?.parse().unwrap_or(0);
            let removed = parts.next()?.parse().unwrap_or(0);
            let path = parts.next()?.to_string();
            Some(FileChange { path, added, removed })
        })
        .collect();
    Ok((files, patch))
}

/// Stage and commit everything in the worktree. Returns false if there was nothing to commit.
pub async fn commit_all(worktree: &Path, message: &str) -> Result<bool> {
    git(worktree, &["add", "--all"]).await?;
    let clean = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await?
        .success();
    if clean {
        return Ok(false);
    }
    git(worktree, &["commit", "--quiet", "-m", message]).await?;
    Ok(true)
}

/// Push the branch and open a GitHub pull request with `gh`. Returns the PR URL.
pub async fn open_pr(worktree: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
    git(worktree, &["push", "--quiet", "-u", "origin", branch]).await?;
    let output = Command::new("gh")
        .current_dir(worktree)
        .args(["pr", "create", "--base", base, "--head", branch, "--title", title, "--body", body])
        .output()
        .await?;
    if !output.status.success() {
        bail!("gh pr create failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Merge `branch` into `base` in the main checkout. Refuses if the main checkout isn't on
/// `base`, so we never switch branches under your feet.
pub async fn merge_into_base(repo: &Path, base: &str, branch: &str) -> Result<()> {
    let current = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let current = current.trim();
    if current != base {
        bail!("your main checkout is on '{current}', not '{base}'. Switch it to '{base}' first, then merge again");
    }
    git(repo, &["merge", "--no-ff", "--no-edit", branch]).await?;
    Ok(())
}

/// Delete the worktree folder and the branch.
pub async fn remove(repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
    if worktree.exists() {
        let wt = worktree.to_string_lossy();
        git(repo, &["worktree", "remove", "--force", wt.as_ref()]).await?;
    } else {
        let _ = git(repo, &["worktree", "prune"]).await;
    }
    if branch_exists(repo, branch).await? {
        git(repo, &["branch", "-D", branch]).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::create;
    use std::path::Path;
    use std::process::Command;

    /// Make a throwaway git repo with one commit on `main`.
    fn make_repo(dir: &Path) {
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
    }

    #[tokio::test]
    async fn creates_worktree_on_new_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        make_repo(&repo);
        let dest = tmp.path().join("wt");

        create(&repo, "ph/test", "main", &dest).await.unwrap();

        assert!(dest.join(".git").exists(), "worktree folder should exist");
    }

    #[tokio::test]
    async fn returns_git_error_for_unknown_base() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        make_repo(&repo);

        let err = create(&repo, "ph/test", "no-such-branch", &tmp.path().join("wt"))
            .await
            .unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("no-such-branch"),
            "error should include git's message, got: {msg}"
        );
    }
}
