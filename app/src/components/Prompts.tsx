import { useEffect, useMemo, useState, type RefObject } from "react";
import { api, clock, type Task } from "../api";
import { useStore } from "../store";
import { firstLine, useTaskActivity } from "./Activity";
import { norm, type TermControl } from "./TerminalView";

/** Everything you asked this task's agent, in order. Click one to scroll the terminal to it. */
const same = (a: string, b: string) => a.replace(/\s+/g, " ").trim() === b.replace(/\s+/g, " ").trim();

export function PromptsPanel({ task, term }: { task: Task; term: RefObject<TermControl | null> }) {
  const s = useStore();
  const activity = useTaskActivity(task.id);
  // Prompts the terminal shows (`❯ …` lines). Some were never logged (sent while an older
  // version of the service was running), so these fill the gaps. Re-read every few seconds.
  const [onScreen, setOnScreen] = useState<string[]>([]);
  useEffect(() => {
    const read = () => {
      const list = term.current?.prompts();
      if (list) setOnScreen((old) => (old.join("\n") === list.join("\n") ? old : list));
    };
    read();
    const t = setInterval(read, 2000);
    return () => clearInterval(t);
  }, [term, task.id]);

  const prompts = useMemo(() => {
    const logged: { at: number | null; text: string }[] = activity
      .filter((a) => a.kind === "prompt" && !a.agent)
      .map((a) => ({ at: a.at, text: a.text }));
    // The task's own prompt always comes first, even if it was sent before the log existed.
    if (!logged.length || !same(logged[0].text, task.prompt)) logged.unshift({ at: task.created_at, text: task.prompt });
    if (!onScreen.length) return logged;
    // Follow the terminal's order; take the time from the log when the prompt is there.
    const matches = (l: string, t: string) => {
      const a = norm(firstLine(l)), b = norm(t);
      return !!a && !!b && (a.startsWith(b) || b.startsWith(a));
    };
    const used = new Set<number>();
    const fromScreen = onScreen.map((text) => {
      const i = logged.findIndex((l, j) => !used.has(j) && matches(l.text, text));
      if (i < 0) return { at: null, text };
      used.add(i);
      return logged[i];
    });
    // Logged prompts no longer on screen are older ones that scrolled away: list them first.
    return [...logged.filter((_, j) => !used.has(j)), ...fromScreen];
  }, [activity, task.created_at, task.prompt, onScreen]);
  const [active, setActive] = useState<number | null>(null);
  const [fullScreen, setFullScreen] = useState(false);

  // Watch whether the agent draws in full-screen mode (then there's no scrollback to jump in).
  useEffect(() => {
    const check = () => {
      const c = term.current;
      if (!c) return false;
      setFullScreen(c.isFullScreen());
      c.onFullScreenChange = setFullScreen;
      return true;
    };
    if (check()) return;
    const t = setInterval(() => check() && clearInterval(t), 300); // the terminal mounts a moment later
    return () => clearInterval(t);
  }, [term, task.id]);
  const [missing, setMissing] = useState<Set<number>>(new Set());

  const [busy, setBusy] = useState(false);
  async function restartAgent() {
    // An old daemon would start it in full-screen mode again, so update the service first.
    if (s.daemonOutdated) return s.restartDaemon();
    setBusy(true);
    await s.run(async () => {
      await api.stop(task.id).catch(() => {});
      // Wait until the old process has exited (ask the daemon; React state here is a snapshot).
      for (let i = 0; i < 30; i++) {
        const t = (await api.listTasks()).find((x) => x.id === task.id);
        if (!t || !["queued", "running", "needs_input", "review"].includes(t.status)) break;
        await new Promise((r) => setTimeout(r, 150));
      }
      await api.resume(task.id);
    });
    setBusy(false);
  }

  function jump(i: number) {
    if (fullScreen) return;
    // The on-screen prompts are the last ones in the list; this is i's place among them.
    const onScreenIndex = onScreen.length ? i - (prompts.length - onScreen.length) : i;
    const ok = term.current?.jumpToPrompt(prompts[i].text, onScreenIndex) ?? false;
    if (ok) {
      setActive(i);
    } else {
      setMissing((m) => new Set(m).add(i));
      s.toast("That prompt isn't in the terminal any more (too old, or the agent restarted since).");
    }
  }

  return (
    <section className="panel prompts-panel">
      <h2>
        Your prompts <span className="muted">{prompts.length}</span>
        <span className="muted small prompts-hint"> · ⌘↑ ⌘↓ in the terminal</span>
      </h2>
      {fullScreen && (
        <div className="small fullscreen-note">
          <p>This agent was started in Claude Code's full-screen mode, so its conversation can't be scrolled or jumped to.</p>
          <button className="btn btn-primary" onClick={restartAgent} disabled={busy || s.restarting}>
            {busy || s.restarting ? "Restarting…" : "Restart agent"}
          </button>
          <span className="muted"> It keeps its conversation.</span>
        </div>
      )}
      {prompts.length === 0 ? (
        <p className="muted small">Nothing sent yet.</p>
      ) : (
        <ol className="prompts">
          {prompts.map((p, i) => (
            <li key={`${p.at}-${i}`}>
              <button
                className={`prompt-btn${active === i ? " active" : ""}${missing.has(i) || fullScreen ? " missing" : ""}`}
                disabled={fullScreen}
                onClick={() => jump(i)}
                title={p.text}
              >
                <span className="mono muted small">{p.at == null ? "—" : clock(p.at)}</span>
                <span className="ellipsis grow">{firstLine(p.text)}</span>
              </button>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
