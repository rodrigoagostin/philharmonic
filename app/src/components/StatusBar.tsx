import { useStore } from "../store";

export function StatusBar() {
  const s = useStore();
  const count = (...st: string[]) => s.tasks.filter((t) => st.includes(t.status)).length;
  return (
    <footer className="statusbar">
      <span>{count("running", "queued")} running</span>
      <span>{count("needs_input")} waiting</span>
      <span>{count("review")} in review</span>
      <span>{count("done")} done</span>
      <span className="spacer" />
      <span className="muted">⌘N new · ⌘0 overview · ⌘J stage · ⌘K search · ⌘1–9 tasks</span>
      {s.connected ? (
        <span className="ok">● Daemon connected</span>
      ) : (
        <span className="bad" title={s.connectionError}>
          ● Daemon offline, reconnecting…
        </span>
      )}
    </footer>
  );
}
