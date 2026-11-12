import { useState } from "react";
import { api, type Task, type TaskStatus } from "../api";
import { useChangedFiles } from "../hooks";
import { useStore } from "../store";
import { Composer } from "./Composer";
import { TerminalView } from "./TerminalView";
import { Dot, Icon, StatusPill } from "./ui";

const LIVE: TaskStatus[] = ["queued", "running", "needs_input", "review"];

export function Focus({ task }: { task: Task }) {
  const s = useStore();
  const live = LIVE.includes(task.status);
  const files = useChangedFiles(task.id);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const tabs = s.tasks.filter((t) => t.status !== "done");

  async function discard() {
    const ok = await s.run(() => api.discard(task.id));
    if (ok !== undefined) {
      s.toast(task.adopted ? `Removed task ${task.id} (your folder and branch are untouched)` : `Discarded task ${task.id}`);
      s.go({ name: "stage" });
    }
  }

  return (
    <section className="focus">
      <nav className="tabs" aria-label="Open tasks">
        <button className="tab" onClick={() => s.go({ name: "stage" })}>
          <Icon name="grid" /> Stage
        </button>
        {tabs.map((t, i) => (
          <button
            key={t.id}
            className={`tab${t.id === task.id ? " tab-active" : ""}`}
            onClick={() => s.go({ name: "focus", id: t.id })}
          >
            <Dot status={t.status} />
            <span className="ellipsis">{t.title}</span>
            {i < 9 && <kbd className="kbd">⌘{i + 1}</kbd>}
          </button>
        ))}
      </nav>

      <div className="focus-body">
        <div className="term-panel">
          <header className="term-head">
            <span className="task-num">#{task.id}</span>
            <strong className="ellipsis grow">{task.title}</strong>
            <StatusPill status={task.status} />
            <span className="muted small mono">{task.agent.kind === "claude" ? "claude" : "custom"}</span>
          </header>
          <TerminalView key={task.id} id={task.id} live={live} />
          {task.status === "needs_input" && (
            <div className="term-keys">
              <span className="small">The agent is waiting for you:</span>
              {[
                ["↑", "\x1b[A"],
                ["↓", "\x1b[B"],
                ["Enter", "\r"],
                ["Esc", "\x1b"],
              ].map(([label, key]) => (
                <button key={label} className="key" onClick={() => s.run(() => api.key(task.id, key))}>
                  {label}
                </button>
              ))}
            </div>
          )}
        </div>

        <aside className="side">
          <section className="panel">
            <h2>
              Changed files <span className="muted">{files.length}</span>
            </h2>
            {files.length === 0 && <p className="muted small">No changes yet.</p>}
            {files.map((f) => (
              <div key={f.path} className="file-row">
                <span className="ellipsis grow mono small">{f.path}</span>
                <span className="add small">+{f.added}</span>
                <span className="del small">−{f.removed}</span>
              </div>
            ))}
            <button className="link-btn small" onClick={() => s.go({ name: "review", id: task.id })}>
              Review diff →
            </button>
          </section>

          <section className="panel">
            <h2>Message the agent</h2>
            <Composer taskId={task.id} disabled={!live} />
          </section>

          <section className="panel">
            <h2>Task</h2>
            <dl className="info">
              <dt>Repo</dt>
              <dd className="mono selectable">{task.repo}</dd>
              <dt>Branch</dt>
              <dd className="mono selectable">
                {task.branch} ← {task.base}
              </dd>
              <dt>{task.adopted ? "Folder" : "Worktree"}</dt>
              <dd className="mono selectable">{task.worktree}</dd>
              <dt>Agent</dt>
              <dd>
                {task.agent.kind === "claude" ? "Claude Code" : task.agent.command}
                {task.model ? ` · ${task.model}` : ""}
                {task.auto_approve ? " · auto-approve edits" : ""}
              </dd>
              <dt>Prompt</dt>
              <dd className="selectable">{task.prompt}</dd>
            </dl>
          </section>

          <section className="panel">
            <h2>Actions</h2>
            <div className="actions">
              {live ? (
                <button className="btn" onClick={() => s.run(() => api.stop(task.id))}>
                  Stop agent
                </button>
              ) : (
                <button className="btn" onClick={() => s.run(() => api.resume(task.id))}>
                  Resume agent
                </button>
              )}
              <button
                className="btn"
                onClick={() =>
                  s.run(async () => {
                    await navigator.clipboard.writeText(task.worktree);
                    s.toast("Worktree path copied");
                  })
                }
              >
                Copy worktree path
              </button>
              <button className="btn" onClick={() => s.go({ name: "context", taskId: task.id })}>
                Context shelf
              </button>
              <button className="btn" onClick={() => s.run(() => api.markDone(task.id))} disabled={task.status === "done"}>
                Mark done
              </button>
              {confirmDiscard ? (
                <button className="btn btn-danger-solid" onClick={discard} onBlur={() => setConfirmDiscard(false)} autoFocus>
                  {task.adopted ? "Remove from Stage? (files stay)" : "Really delete worktree + branch?"}
                </button>
              ) : (
                <button className="btn btn-danger" onClick={() => setConfirmDiscard(true)}>
                  {task.adopted ? "Remove…" : "Discard…"}
                </button>
              )}
            </div>
          </section>
        </aside>
      </div>
    </section>
  );
}
