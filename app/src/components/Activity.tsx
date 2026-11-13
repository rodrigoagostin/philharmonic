import { useEffect, useMemo, useState } from "react";
import { api, clock, compact, duration, usageTotal, type Activity, type PlanItem, type Subagent, type Task } from "../api";
import { useStore } from "../store";

/** Seconds since 1970, refreshed every second (for live durations). */
export function useNow(ticking = true): number {
  const [now, setNow] = useState(Date.now() / 1000);
  useEffect(() => {
    if (!ticking) return;
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, [ticking]);
  return now;
}

/** A duration that counts up by itself (only this text re-renders every second). */
export function Elapsed({ since, until }: { since: number; until?: number | null }) {
  const now = useNow(until == null);
  return <>{duration((until ?? now) - since)}</>;
}

/** "now: Editing src/main.rs" / "thinking…" / "waiting for you", one line. */
export function NowLine({ task }: { task: Task }) {
  const running = task.subagents.filter((s) => s.running).length;
  let text: string | null = null;
  if (task.status === "needs_input") text = "waiting for you";
  else if (task.status === "running" || task.status === "queued") text = task.doing ?? "thinking…";
  else if (task.status === "review" && task.summary) text = `finished: ${firstLine(task.summary)}`;
  if (!text) return null;
  return (
    <p className={`now now-${task.status} small ellipsis`} title={task.status === "review" ? task.summary ?? "" : text}>
      {task.status === "review" ? "" : "now: "}
      {text}
      {running > 0 && <span className="muted"> · {running} subagent{running === 1 ? "" : "s"} working</span>}
    </p>
  );
}

export function firstLine(text: string): string {
  return text.split("\n").find((l) => l.trim())?.trim() ?? "";
}

export function PlanList({ plan }: { plan: PlanItem[] }) {
  if (!plan.length) return null;
  const done = plan.filter((p) => p.status === "completed").length;
  return (
    <div className="plan">
      <div className="small muted">
        Plan · {done}/{plan.length} done
      </div>
      <ul>
        {plan.map((p) => (
          <li key={p.id} className={`plan-${p.status}`}>
            <span className="plan-box" aria-hidden="true">
              {p.status === "completed" ? "✓" : p.status === "in_progress" ? "▸" : ""}
            </span>
            <span className="sr-only">{p.status.replace("_", " ")}: </span>
            {p.text}
          </li>
        ))}
      </ul>
    </div>
  );
}

export function SubagentList({ subagents }: { subagents: Subagent[] }) {
  if (!subagents.length) return null;
  return (
    <ul className="subagents">
      {subagents.map((s) => (
        <li key={s.id} className={s.running ? "sub-running" : "sub-done"}>
          <div className="row">
            <span className={`dot ${s.running ? "dot-running" : "dot-done"}`} />
            <strong className="ellipsis grow" title={s.description}>
              {s.description}
            </strong>
            <span className="muted small mono">{s.kind}</span>
          </div>
          <div className="small muted sub-meta">
            {s.running ? (s.doing ?? "thinking…") : "done"} · <Elapsed since={s.started_at} until={s.running ? null : (s.ended_at ?? s.started_at)} /> · {s.tool_calls} action
            {s.tool_calls === 1 ? "" : "s"}
          </div>
          {s.result && (
            <details className="sub-result small">
              <summary>Result</summary>
              <p className="selectable">{s.result}</p>
            </details>
          )}
        </li>
      ))}
    </ul>
  );
}

const KIND_MARK: Record<Activity["kind"], string> = {
  lifecycle: "◆",
  prompt: "›",
  tool: "·",
  subagent: "⑂",
  waiting: "!",
  finished: "✓",
};

/** A list of activity lines. With `showTask`, each line says which task it belongs to. */
export function ActivityFeed({
  items,
  tasks,
  showTask,
  onPickTask,
}: {
  items: Activity[];
  tasks: Map<number, Task>;
  showTask?: boolean;
  onPickTask?: (id: number) => void;
}) {
  if (!items.length) return <p className="muted small">Nothing yet.</p>;
  return (
    <ol className="feed">
      {items.map((a, i) => {
        const task = tasks.get(a.task);
        const sub = a.agent ? task?.subagents.find((s) => s.id === a.agent) : undefined;
        return (
          <li key={`${a.at}-${i}`} className={`feed-${a.kind}${a.agent ? " feed-sub" : ""}`}>
            <time className="mono muted">{clock(a.at)}</time>
            {showTask && (
              <button className="task-num feed-task" onClick={() => onPickTask?.(a.task)} title={task?.title}>
                #{a.task}
              </button>
            )}
            <span className="feed-mark" aria-hidden="true">
              {KIND_MARK[a.kind]}
            </span>
            <span className="feed-text selectable">
              {a.agent && <span className="feed-agent">{sub?.description ?? "subagent"} › </span>}
              {a.kind === "prompt" ? <em>“{firstLine(a.text)}”</em> : a.kind === "finished" ? firstLine(a.text) : a.text}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

/** A task's activity, oldest first: the full history from disk once, then live entries. */
export function useTaskActivity(taskId: number): Activity[] {
  const s = useStore();
  const [history, setHistory] = useState<Activity[]>([]);
  useEffect(() => {
    api.activity(taskId, 2000).then(setHistory).catch(() => {});
  }, [taskId]);
  return useMemo(() => {
    const last = history.at(-1)?.at ?? 0;
    const live = s.activity.filter((a) => a.task === taskId && a.at >= last);
    const seen = new Set(history.map((a) => `${a.at}|${a.text}`));
    return [...history, ...live.filter((a) => !seen.has(`${a.at}|${a.text}`))];
  }, [history, s.activity, taskId]);
}

/** Focus view side panel: what this task's agents are doing and have done. */
export function ActivityPanel({ task }: { task: Task }) {
  const all = useTaskActivity(task.id);
  const items = useMemo(() => all.slice(-300), [all]);
  const tasks = useMemo(() => new Map([[task.id, task]]), [task]);

  return (
    <section className="panel activity-panel">
      <h2>
        Activity <span className="muted">{task.tool_calls} actions · {compact(usageTotal(task.usage))} tokens</span>
      </h2>
      <NowLine task={task} />
      <PlanList plan={task.plan} />
      <SubagentList subagents={task.subagents} />
      <div className="feed-scroll">
        <ActivityFeed items={[...items].reverse()} tasks={tasks} />
      </div>
    </section>
  );
}
