import { repoName } from "../api";
import { useStore, type Filter } from "../store";

const encode = (f: Filter) => (!f ? "" : f.kind === "repo" ? `repo:${f.repo}` : `col:${f.id}`);
const decode = (v: string): Filter =>
  v.startsWith("repo:") ? { kind: "repo", repo: v.slice(5) } : v.startsWith("col:") ? { kind: "collection", id: Number(v.slice(4)) } : null;
import { Icon } from "./ui";

export function TopBar() {
  const s = useStore();
  const repos = [...new Set(s.tasks.map((t) => t.repo))];
  const waiting = s.tasks.filter((t) => t.status === "needs_input");

  return (
    <header className="topbar">
      <button className="brand" onClick={() => s.go({ name: "overview" })}>
        <Icon name="baton" size={18} />
        <span>philharmonic</span>
      </button>
      <select
        aria-label="Show tasks from"
        title="Which tasks the sidebar, Overview and Stage show: a project, or a collection (e.g. one feature). With a task open, it follows that task."
        value={encode(s.filter)}
        onChange={(e) => s.setFilter(decode(e.target.value))}
      >
        <option value="">Show: everything</option>
        <optgroup label="Projects">
          {repos.map((r) => (
            <option key={r} value={`repo:${r}`}>
              Show: {repoName(r)}
            </option>
          ))}
        </optgroup>
        {s.collections.length > 0 && (
          <optgroup label="Collections">
            {s.collections.map((c) => (
              <option key={c.id} value={`col:${c.id}`}>
                Show: {c.name}
              </option>
            ))}
          </optgroup>
        )}
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
      <div className="segmented" role="tablist" aria-label="View">
        <button role="tab" aria-selected={s.view.name === "overview"} onClick={() => s.go({ name: "overview" })}>
          Overview
        </button>
        <button role="tab" aria-selected={s.view.name === "stage"} onClick={() => s.go({ name: "stage" })}>
          Stage
        </button>
      </div>
      <span className="spacer" />
      <button
        className="btn"
        title="Docs, screenshots, links and notes for your agents, grouped in collections"
        onClick={() => s.go({ name: "context", collectionId: s.filter?.kind === "collection" ? s.filter.id : undefined })}
      >
        <Icon name="clipboard" /> Context
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
