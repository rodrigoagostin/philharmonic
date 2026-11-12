import { repoName } from "../api";
import { useStore } from "../store";
import { Icon } from "./ui";

export function TopBar() {
  const s = useStore();
  const repos = [...new Set(s.tasks.map((t) => t.repo))];
  const waiting = s.tasks.filter((t) => t.status === "needs_input");

  return (
    <header className="topbar">
      <button className="brand" onClick={() => s.go({ name: "stage" })}>
        <Icon name="baton" size={18} />
        <span>philharmonic</span>
      </button>
      <select
        aria-label="Project"
        value={s.repoFilter ?? ""}
        onChange={(e) => s.setRepoFilter(e.target.value || null)}
      >
        <option value="">All projects</option>
        {repos.map((r) => (
          <option key={r} value={r}>
            {repoName(r)}
          </option>
        ))}
      </select>
      <label className="search">
        <Icon name="search" />
        <span className="sr-only">Search tasks</span>
        <input
          id="search"
          placeholder="Search tasks"
          value={s.search}
          onChange={(e) => {
            s.setSearch(e.target.value);
            if (s.view.name !== "stage") s.go({ name: "stage" });
          }}
        />
        <kbd className="kbd">⌘K</kbd>
      </label>
      <span className="spacer" />
      <button className="btn" onClick={() => s.go({ name: "context" })}>
        <Icon name="clipboard" /> Shelf
      </button>
      {waiting.length > 0 && (
        <button className="btn btn-attention" onClick={() => s.go({ name: "focus", id: waiting[0].id })}>
          <Icon name="bell" /> {waiting.length} need{waiting.length === 1 ? "s" : ""} you
        </button>
      )}
      <button className="btn btn-primary" onClick={() => s.setNewTaskOpen(true)}>
        <Icon name="plus" /> New task <kbd className="kbd kbd-dark">⌘N</kbd>
      </button>
    </header>
  );
}
