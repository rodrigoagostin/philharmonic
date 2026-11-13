import { GROUPS } from "../api";
import { useStore } from "../store";
import { Dot } from "./ui";

export function Sidebar() {
  const s = useStore();
  const tasks = s.tasks.filter(s.shown);
  const activeId = s.view.name === "focus" || s.view.name === "review" ? s.view.id : undefined;

  return (
    <nav className="sidebar" aria-label="Tasks">
      {tasks.length === 0 && <p className="muted small">No tasks yet.</p>}
      {GROUPS.map((g) => {
        const list = tasks.filter((t) => g.statuses.includes(t.status));
        if (!list.length) return null;
        return (
          <section key={g.label} className="group">
            <h2>
              {g.label} <span className="muted">{list.length}</span>
            </h2>
            {list.map((t) => (
              <button
                key={t.id}
                className={`task-link${t.id === activeId ? " active" : ""}`}
                onClick={() => s.go({ name: "focus", id: t.id })}
              >
                <Dot status={t.status} />
                <span className="ellipsis grow">{t.title}</span>
                <span className="task-num">#{t.id}</span>
              </button>
            ))}
          </section>
        );
      })}
    </nav>
  );
}
