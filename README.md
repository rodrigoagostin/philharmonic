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

## Context and collections

Agents get docs, screenshots, links and notes from **collections**: named groups you make in
the app's Context screen for whatever you work on, such as a project (`Quilora`), one
feature of it (`Quilora · checkout`) or a product split over several repos. Make as many as
you like.

- Link a collection to repos and new tasks there use it automatically. The New task window
  shows them ticked, and you can add or remove collections there or later in the task.
- A task only sees its own collections, so working on several projects or features at once
  never mixes their context. Each task also has items of its own (what you paste into its
  message box), deleted with the task.
- When the agent starts, it's told what its collections contain (`--append-system-prompt`)
  and can read their folders without asking (`--add-dir`). A collection added to a running
  task is announced with your next message. Custom agents get `$PH_CONTEXT` and
  `$PH_CONTEXT_DIRS`.
- `ph collections` lists them; `ph new "…" -c "Quilora · checkout"` picks them by name.

Team rules and conventions still belong in `CLAUDE.md` in the repo, where every tool and
teammate gets them. Collections are for material that isn't code.

## Branches and your Claude setup

Each task gets its own worktree (a second checkout of your repo in `~/.philharmonic/wt/`)
on a new branch. The branch is `ph/<from the prompt>` unless you name it: the New task window
fills it in when the prompt has a ticket key (`PAY-482 add retry…` →
`PAY-482-add-retry…`, with the `feature/`-style prefix you used last in that repo), and
`ph new --branch feature/PAY-482-retry` does the same from the CLI.

The worktree has every committed file, so `CLAUDE.md` (in any folder), `.claude/settings.json`,
rules, skills, agents, commands, `.mcp.json` and project plugins all apply as usual. Your
user-level setup (`~/.claude`, installed plugins and marketplaces) applies everywhere anyway.
What git doesn't track is handled when the task starts:

- `.claude/settings.local.json` and `CLAUDE.local.md` are copied from your checkout.
- Anything matching `.worktreeinclude` (gitignore syntax, the same file Claude Code's own
  `--worktree` uses) is copied too, e.g. `.env`.
- MCP servers you added for that project only, and the `.mcp.json` servers you approved there,
  are passed to the agent (Claude Code stores those by folder path).

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
ph open ~/code/site          # Claude right in that folder (git or not): no worktree, no branch
ph new "Add rate limiting to /auth" --repo ~/code/api --base main
ph ls
ph show 1                # what's on the agent's screen
ph send 1 "also add tests"
ph log 1                 # what the agent (and its subagents) did
ph log                   # latest actions of every agent
ph diff 1
ph ship 1 pr             # or: merge
ph rm 1                  # stop, delete worktree + branch
```

## Watching your agents

The **Overview** (home screen, ⌘0) is the conductor's view: how many agents and subagents are
working, who is waiting for you, tokens used, and for every task what it's doing right now, its
plan, each subagent with its own actions and result, and what it said when it finished. On the
right, a live feed of every action. Click a task to filter the feed to it.

This comes from Claude Code hooks (`PreToolUse`, `PostToolUse`, `SubagentStart`/`SubagentStop`,
`Notification`, `Stop`, `UserPromptSubmit`), which `ph notify` forwards to the daemon. The log is
kept per task in `~/.philharmonic/activity/<id>.jsonl`; tokens are counted from Claude Code's
session transcripts. Custom (non-Claude) agents only report start/stop and your messages.

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
