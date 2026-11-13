import { memo, useCallback, useMemo, useState } from "react";
import { api, compact, repoName, usageTotal, type Task, type TaskStatus } from "../api";
import { useStore } from "../store";
import { ActivityFeed, Elapsed, firstLine, NowLine, PlanList, SubagentList } from "./Activity";
import { Dot, Icon, StatusPill } from "./ui";

const LIVE: TaskStatus[] = ["queued", "running", "needs_input", "review"];
const ORDER: TaskStatus[] = ["needs_input", "running", "queued", "review", "failed", "stopped", "done"];

/** The conductor's view: every agent, what it's doing now, and what each one has done. */
export function Overview() {
  const s = useStore();
  const [showAll, setShowAll] = useState(false);
  const [picked, setPicked] = useState<number | null>(null);
  const togglePick = useCallback((id: number) => setPicked((p) => (p === id ? null : id)), []);

  const scoped = s.tasks.filter(s.shown);
  const shown = scoped
    .filter((t) => showAll || t.status !== "done")
    .sort((a, b) => ORDER.indexOf(a.status) - ORDER.indexOf(b.status) || b.last_activity - a.last_activity);
  const hidden = scoped.filter((t) => t.status === "done").length;

  const working = scoped.filter((t) => t.status === "running" || t.status === "queued").length;
  const subagents = scoped.reduce((n, t) => n + t.subagents.filter((x) => x.running).length, 0);
  const waiting = scoped.filter((t) => t.status === "needs_input").length;
  const review = scoped.filter((t) => t.status === "review").length;
  const tokens = scoped.reduce((n, t) => n + usageTotal(t.usage), 0);

  const byId = useMemo(() => new Map(s.tasks.map((t) => [t.id, t])), [s.tasks]);
  const feed = useMemo(() => {
    const ids = new Set(scoped.map((t) => t.id));
    return s.activity.filter((a) => ids.has(a.task) && (picked === null || a.task === picked)).slice(-300).reverse();
  }, [s.activity, scoped, picked]);

  return (
    <section className="overview">
      <header className="ov-stats" aria-label="Summary">
        <Stat value={working} label={working === 1 ? "agent working" : "agents working"} tone="running" />
        <Stat value={subagents} label={subagents === 1 ? "subagent" : "subagents"} tone="running" />
        <Stat value={waiting} label="waiting for you" tone="waiting" />
        <Stat value={review} label="ready for review" tone="review" />
        <Stat value={compact(tokens)} label="tokens used" />
        <span className="spacer" />
        <label className="check small">
          <input type="checkbox" checked={showAll} onChange={(e) => setShowAll(e.target.checked)} />
          Show done ({hidden})
        </label>
      </header>

      <div className="ov-body">
        <div className="ov-agents">
          {shown.length === 0 ? (
            <div className="empty">
              <p>No tasks yet. Start one and watch its agents here.</p>
              <button className="btn btn-primary" onClick={() => s.setNewTaskOpen(true)}>
                <Icon name="plus" /> New task
              </button>
            </div>
          ) : (
            shown.map((t) => (
              <AgentBlock
                key={t.id}
                task={t}
                picked={picked === t.id}
                onPick={togglePick}
              />
            ))
          )}
        </div>

        <aside className="ov-feed panel" aria-label="Live activity">
          <h2 className="row">
            <span className="grow">
              Live activity
              {picked !== null && <span className="muted"> · task #{picked}</span>}
            </span>
            {picked !== null && (
              <button className="link-btn small" onClick={() => setPicked(null)}>
                Show all
              </button>
            )}
          </h2>
          <div className="feed-scroll">
            <ActivityFeed items={feed} tasks={byId} showTask onPickTask={setPicked} />
          </div>
        </aside>
      </div>
    </section>
  );
}

function Stat({ value, label, tone }: { value: number | string; label: string; tone?: string }) {
  return (
    <div className={`stat${tone && value !== 0 ? ` stat-${tone}` : ""}`}>
      <strong>{value}</strong>
      <span className="small muted">{label}</span>
    </div>
  );
}

const AgentBlock = memo(function AgentBlock({ task, picked, onPick }: { task: Task; picked: boolean; onPick: (id: number) => void }) {
  const s = useStore();
  const live = LIVE.includes(task.status);
  const subRunning = task.subagents.filter((x) => x.running).length;
  const [open, setOpen] = useState(true);

  return (
    <article className={`agent${picked ? " agent-picked" : ""}${task.status === "needs_input" ? " card-waiting" : ""}`}>
      <header className="row">
        <button className="icon-btn agent-toggle" aria-expanded={open} aria-label={open ? "Collapse" : "Expand"} onClick={() => setOpen(!open)}>
          {open ? "▾" : "▸"}
        </button>
        <Dot status={task.status} />
        <button className="link-btn ellipsis grow agent-title" onClick={() => onPick(task.id)} title="Show only this task's activity">
          <span className="task-num">#{task.id}</span> {task.title}
        </button>
        <StatusPill status={task.status} />
      </header>
      <p className="muted small mono ellipsis agent-meta">
        {repoName(task.repo)}
        {task.collections.length > 0 &&
          ` (${task.collections.map((id) => s.collection(id)?.name).filter(Boolean).join(", ")})`}{" "}
        · {task.branch} · {task.model ?? (task.agent.kind === "claude" ? "claude" : "custom")} · <Elapsed since={task.created_at} until={LIVE.includes(task.status) ? null : task.last_activity} /> ·{" "}
        {task.tool_calls} actions · {compact(usageTotal(task.usage))} tokens
        {task.subagents.length > 0 && ` · ${task.subagents.length} subagent${task.subagents.length === 1 ? "" : "s"}${subRunning ? ` (${subRunning} running)` : ""}`}
      </p>
      <NowLine task={task} />

      {open && (
        <div className="agent-detail">
          <PlanList plan={task.plan} />
          <SubagentList subagents={task.subagents} />
          {task.summary && task.status !== "review" && (
            <p className="small muted ellipsis" title={task.summary}>
              Last said: {firstLine(task.summary)}
            </p>
          )}
        </div>
      )}

      <footer className="card-foot">
        {task.status === "needs_input" && (
          <button className="btn btn-attention-solid" onClick={() => s.go({ name: "focus", id: task.id })}>
            Answer
          </button>
        )}
        <span className="spacer" />
        {live ? (
          <button className="btn" onClick={() => s.run(() => api.stop(task.id))}>
            Stop
          </button>
        ) : (
          task.status !== "done" && (
            <button className="btn btn-primary" onClick={() => s.run(() => api.resume(task.id))}>
              Resume
            </button>
          )
        )}
        {task.status !== "done" && (
          <button className="btn" onClick={() => s.go({ name: "review", id: task.id })}>
            <Icon name="diff" /> Review
          </button>
        )}
        <button className="btn" onClick={() => s.go({ name: "focus", id: task.id })}>
          Open
        </button>
      </footer>
    </article>
  );
});
