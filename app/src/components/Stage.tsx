import { useState } from "react";
import { ago, api, repoName, type Task, type TaskStatus } from "../api";
import { useScreenText } from "../hooks";
import { useStore } from "../store";
import { Icon, StatusPill, tail } from "./ui";

const ORDER: TaskStatus[] = ["needs_input", "review", "running", "queued", "failed", "stopped", "done"];

export function Stage() {
  const s = useStore();
  const [showDone, setShowDone] = useState(false);
  const q = s.search.trim().toLowerCase();

  const scoped = s.tasks.filter((t) => !s.repoFilter || t.repo === s.repoFilter);
  const doneCount = scoped.filter((t) => t.status === "done").length;
  const tasks = scoped
    .filter((t) => showDone || t.status !== "done")
    .filter((t) => !q || [t.title, t.branch, t.prompt, t.repo].some((f) => f.toLowerCase().includes(q)))
    .sort((a, b) => ORDER.indexOf(a.status) - ORDER.indexOf(b.status) || b.id - a.id);
  const repos = new Set(tasks.map((t) => t.repo)).size;

  return (
    <section className="stage">
      <header className="row">
        <h1>
          Stage <span className="muted">{tasks.length}</span>
        </h1>
        {repos > 0 && <span className="muted small">across {repos} repo{repos === 1 ? "" : "s"}</span>}
        <span className="spacer" />
        <label className="check small">
          <input type="checkbox" checked={showDone} onChange={(e) => setShowDone(e.target.checked)} />
          Show done ({doneCount})
        </label>
      </header>
      {tasks.length === 0 ? (
        <div className="empty">
          <p>{q ? "No tasks match your search." : "Nothing on stage. Start a task and the agent gets its own worktree."}</p>
          <button className="btn btn-primary" onClick={() => s.setNewTaskOpen(true)}>
            <Icon name="plus" /> New task
          </button>
        </div>
      ) : (
        <div className="cards">
          {tasks.map((t) => (
            <TaskCard key={t.id} task={t} />
          ))}
        </div>
      )}
    </section>
  );
}

const LIVE: TaskStatus[] = ["queued", "running", "needs_input", "review"];

function TaskCard({ task }: { task: Task }) {
  const s = useStore();
  const text = useScreenText(task.id);
  const [reply, setReply] = useState("");
  const waiting = task.status === "needs_input";
  const live = LIVE.includes(task.status);
  const open = () => s.go({ name: "focus", id: task.id });

  async function send(e: React.FormEvent) {
    e.preventDefault();
    if (!reply.trim()) return;
    const ok = await s.run(() => api.send(task.id, reply.trim()));
    if (ok !== undefined) setReply("");
  }

  return (
    <article className={`card${waiting ? " card-waiting" : ""}`}>
      <header className="row">
        <button className="link-btn ellipsis grow" onClick={open} title={task.title}>
          <span className="task-num">#{task.id}</span> {task.title}
        </button>
        <StatusPill status={task.status} />
      </header>
      <p className="muted small ellipsis mono">
        {task.agent.kind === "claude" ? "claude" : "custom"} · {repoName(task.repo)} · {task.branch}
      </p>
      <pre className="tail" onClick={open}>
        {tail(text, 6) || " "}
      </pre>
      {waiting && (
        <div className="quick-keys">
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
      {live && (
        <form className="reply" onSubmit={send}>
          <input
            aria-label={`Reply to task ${task.id}`}
            placeholder={waiting ? "Answer the agent…" : "Message the agent…"}
            value={reply}
            onChange={(e) => setReply(e.target.value)}
          />
          <button className={`btn ${waiting ? "btn-attention-solid" : ""}`} disabled={!reply.trim()}>
            Send
          </button>
        </form>
      )}
      <footer className="card-foot">
        <span className="muted small">{ago(task.created_at)}</span>
        <span className="spacer" />
        {(task.status === "stopped" || task.status === "failed") && (
          <button className="btn" onClick={() => s.run(() => api.resume(task.id))}>
            Resume
          </button>
        )}
        {task.status !== "done" && (
          <button className="btn" onClick={() => s.go({ name: "review", id: task.id })}>
            <Icon name="diff" /> Review
          </button>
        )}
        <button className="btn" onClick={open}>
          Open
        </button>
      </footer>
    </article>
  );
}
