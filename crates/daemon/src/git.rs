//! Git operations: one worktree per task (so agents never edit the same files),
//! plus diff, commit, push/PR, merge and cleanup.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
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
    git_with_index(dir, args, None).await
}

/// Like `git`, optionally with `GIT_INDEX_FILE` pointing at another index.
async fn git_with_index(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let output = cmd.output().await?;
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
///
/// This runs often while an agent works, so it never touches the worktree's real index
/// (that would take `index.lock` and make the agent's own `git add`/`commit` fail). It
/// works on a throwaway copy instead.
pub async fn diff(worktree: &Path, base: &str) -> Result<(Vec<FileChange>, String)> {
    let merge_base = git(worktree, &["merge-base", base, "HEAD"]).await?;
    let merge_base = merge_base.trim();

    let index = TempIndex::copy_of(worktree).await?;
    let ix = Some(index.path());
    // Mark new files as "intent to add" (in the copy) so `git diff` shows them too.
    let _ = git_with_index(worktree, &["add", "--intent-to-add", "--all"], ix).await;
    // `--no-renames`: otherwise numstat says `old => new` while the patch says `new`,
    // and the two lists don't match up in the app.
    let patch = git_with_index(worktree, &["diff", "--no-renames", merge_base], ix).await?;
    let numstat = git_with_index(worktree, &["diff", "--no-renames", "--numstat", merge_base], ix).await?;
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

/// A private copy of a worktree's index, deleted when dropped.
struct TempIndex(PathBuf);

impl TempIndex {
    async fn copy_of(worktree: &Path) -> Result<TempIndex> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Relative to the worktree, unless git prints an absolute path (it usually does
        // for linked worktrees); `join` handles both.
        let real = worktree.join(git(worktree, &["rev-parse", "--git-path", "index"]).await?.trim());
        let tmp = std::env::temp_dir().join(format!(
            "ph-index-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if real.exists() {
            tokio::fs::copy(&real, &tmp).await.with_context(|| format!("copying {}", real.display()))?;
        }
        Ok(TempIndex(tmp))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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
/// `base` or has uncommitted changes, and backs out if the merge conflicts, so your
/// checkout is never left half-merged.
pub async fn merge_into_base(repo: &Path, base: &str, branch: &str) -> Result<()> {
    let current = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let current = current.trim();
    if current != base {
        bail!("your main checkout is on '{current}', not '{base}'. Switch it to '{base}' first, then merge again");
    }
    let dirty = git(repo, &["status", "--porcelain", "--untracked-files=no"]).await?;
    if !dirty.trim().is_empty() {
        bail!("your main checkout has uncommitted changes. Commit or stash them first, then merge again");
    }
    if let Err(e) = git(repo, &["merge", "--no-ff", "--no-edit", branch]).await {
        // Conflicts leave a merge in progress: undo it. (Fails harmlessly if git refused
        // before starting, e.g. because untracked files would be overwritten.)
        let _ = git(repo, &["merge", "--abort"]).await;
        return Err(e.context("the merge didn't go through and was undone; your checkout is as it was. Merge it yourself, or open a PR instead"));
    }
    Ok(())
}

/// Delete the worktree folder and the branch, as far as possible.
///
/// Best effort: if git can't do it (the repo was moved or deleted, the folder isn't a
/// registered worktree…), the folder is deleted directly, but only if it's inside
/// `owned_dir` (Philharmonic's own worktrees folder), never anywhere else. Returns the
/// problems it ran into, for the log.
pub async fn remove(repo: &Path, worktree: &Path, branch: &str, owned_dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if worktree.exists() {
        let wt = worktree.to_string_lossy();
        if let Err(e) = git(repo, &["worktree", "remove", "--force", wt.as_ref()]).await {
            problems.push(format!("{e:#}"));
            if worktree.starts_with(owned_dir)
                && let Err(e) = tokio::fs::remove_dir_all(worktree).await
            {
                problems.push(format!("deleting {}: {e}", worktree.display()));
            }
        }
    }
    let _ = git(repo, &["worktree", "prune"]).await;
    match branch_exists(repo, branch).await {
        Ok(true) => {
            if let Err(e) = git(repo, &["branch", "-D", branch]).await {
                problems.push(format!("{e:#}"));
            }
        }
        Ok(false) => {}
        Err(e) => problems.push(format!("{e:#}")),
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::{create, diff, merge_into_base, remove};
    use std::path::Path;
    use std::process::Command;

    /// Run git in `dir` (with a test identity) and return stdout; panics on failure.
    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repo on `main` plus a task worktree at `<tmp>/wt` on branch `ph/t`.
    fn repo_with_worktree(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let repo = tmp.join("repo");
        std::fs::create_dir(&repo).unwrap();
        make_repo(&repo);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        run(&repo, &["add", "a.txt"]);
        run(&repo, &["commit", "-q", "-m", "a"]);
        let wt = tmp.join("wt");
        run(&repo, &["worktree", "add", "-q", "-b", "ph/t", wt.to_str().unwrap(), "main"]);
        (repo, wt)
    }

    #[tokio::test]
    async fn diff_shows_new_files_without_touching_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, wt) = repo_with_worktree(tmp.path());
        std::fs::write(wt.join("new.txt"), "hi\n").unwrap();
        std::fs::write(wt.join("a.txt"), "two\n").unwrap();

        let (files, patch) = diff(&wt, "main").await.unwrap();

        let mut paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["a.txt", "new.txt"]);
        assert!(patch.contains("+hi"));
        // The real index is unchanged: new.txt is still untracked, not intent-to-add.
        assert!(run(&wt, &["status", "--porcelain"]).contains("?? new.txt"));
    }

    #[tokio::test]
    async fn conflicting_merge_is_undone() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, wt) = repo_with_worktree(tmp.path());
        std::fs::write(wt.join("a.txt"), "task\n").unwrap();
        run(&wt, &["commit", "-q", "-am", "task"]);
        std::fs::write(repo.join("a.txt"), "main\n").unwrap();
        run(&repo, &["commit", "-q", "-am", "main"]);

        assert!(merge_into_base(&repo, "main", "ph/t").await.is_err());

        assert_eq!(run(&repo, &["status", "--porcelain"]), "", "checkout should be clean");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "main\n");
    }

    #[tokio::test]
    async fn merge_refuses_a_dirty_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, _wt) = repo_with_worktree(tmp.path());
        std::fs::write(repo.join("a.txt"), "local edit\n").unwrap();

        let err = merge_into_base(&repo, "main", "ph/t").await.unwrap_err();

        assert!(format!("{err:#}").contains("uncommitted"));
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "local edit\n");
    }

    #[tokio::test]
    async fn remove_works_even_if_the_repo_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, wt) = repo_with_worktree(tmp.path());
        std::fs::remove_dir_all(&repo).unwrap();

        let problems = remove(&repo, &wt, "ph/t", tmp.path()).await;

        assert!(!problems.is_empty(), "git should have complained");
        assert!(!wt.exists(), "the worktree folder should be gone anyway");
    }

    #[tokio::test]
    async fn remove_never_deletes_folders_it_does_not_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, wt) = repo_with_worktree(tmp.path());
        std::fs::remove_dir_all(&repo).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        remove(&repo, &wt, "ph/t", elsewhere.path()).await;

        assert!(wt.exists());
    }

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
