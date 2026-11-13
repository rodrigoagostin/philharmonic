# Philharmonic

Run and steer many coding agents in parallel. Each task gets its own git worktree,
branch and agent session; one place shows which agents are working, which need you,
and what they changed.

## Layout

```
crates/
  core/       philharmonic-core  shared types: Task, Request/Response/Event, paths, slugify
  daemon/     philharmonicd      owns every task: worktrees, agents in PTYs, state, git
  cli/        ph                 command-line client (also used by Claude Code hooks)
app/          desktop app: Tauri 2 (src-tauri/) + React + xterm.js (src/)
```

Clients talk to the daemon over a Unix socket (`~/.philharmonic/philharmonicd.sock`)
with one JSON message per line. Everything lives in `~/.philharmonic/`
(`state.json`, `wt/` worktrees, `ctx/` context shelf). Set `PHILHARMONIC_HOME` to use
another folder.

## Running it

### As a normal Mac app (no terminal needed afterwards)

```sh
scripts/build-app.sh --install   # builds ~/Applications/Philharmonic.app
```

Open it from Spotlight/Launchpad like any app. The daemon and `ph` are bundled inside the
`.app`; the app starts the daemon when it opens. The daemon keeps running when you close the
window, so agents aren't killed. Re-run the script after changing the code.

### While developing (one command, hot reload)

First time: `cd app && npm install`.

```sh
cd app && npm run tauri dev      # builds daemon + ph, opens the window, which starts the daemon
```

In Neovim, `:Run` (or `<leader>rr`) runs the same thing from `Procfile.dev`.
`<leader>ro` shows its output, `<leader>rx` stops it.

The app reuses a daemon that's already running. After changing daemon code, stop it
(`pkill philharmonicd`) so the app starts the new build.

CLI:

```sh
ph new "Add rate limiting to /auth" --repo ~/code/api --base main
ph ls
ph show 1                # what's on the agent's screen
ph send 1 "also add tests"
ph diff 1
ph ship 1 pr             # or: merge
ph rm 1                  # stop, delete worktree + branch
```

## How it works

- **Task** = a git worktree + branch `ph/<slug>` + an agent running in a PTY there.
- **Status** comes from Claude Code hooks, injected with `--settings` (nothing is written
  into your repo): `UserPromptSubmit`/`PostToolUse` → running, `Notification` → needs you,
  `Stop` → ready for review. The hooks call `ph notify`.
- **Context shelf**: pasted screenshots, links, files and notes are saved under
  `~/.philharmonic/ctx/`; agents get the file paths in the message.
- **Ship**: commit, then `gh pr create`, or merge into the base branch of your main
  checkout (only if it's on that branch).

## Learning path

- [x] **#1 `slugify`** (`crates/core/src/slug.rs`): strings, chars, iterators.
- [x] **#2 `worktree::create`** (`crates/daemon/src/git.rs`): processes, `Result`, `?`.
- [x] **#3 `new_task`** (`crates/daemon/src/handlers.rs`): ownership, locks and `.await`.
- [x] **#4 `print_tasks`** (`crates/cli/src/main.rs`): formatting.

## Not done yet

- Packaged `.app` (bundling `philharmonicd` and `ph` as sidecars). Dev mode only for now.
- Global ⌘⇧V "send clipboard to the shelf" from other apps.
- Cost / token tracking per task.
- Status for non-Claude agents beyond running/stopped.
- TUI client.
