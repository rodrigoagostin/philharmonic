import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api, repoName, type ContextItem, type Scope, type TaskStatus } from "../api";
import { useStore } from "../store";
import { contextFromDrop, contextFromPaste, Icon, KIND_ICON, URL_RE, type NewContext } from "./ui";

const LIVE: TaskStatus[] = ["queued", "running", "needs_input", "review"];

function sameScope(a: Scope, b: Scope) {
  if (a.kind !== b.kind) return false;
  if (a.kind === "task" && b.kind === "task") return a.id === b.id;
  if (a.kind === "project" && b.kind === "project") return a.repo === b.repo;
  return true;
}

function scopeKey(scope: Scope) {
  return scope.kind === "task" ? `task:${scope.id}` : scope.kind === "project" ? `project:${scope.repo}` : "inbox";
}

function parseScope(key: string): Scope {
  if (key.startsWith("task:")) return { kind: "task", id: Number(key.slice(5)) };
  if (key.startsWith("project:")) return { kind: "project", repo: key.slice(8) };
  return { kind: "inbox" };
}

/** The transfer area: paste screenshots, links and notes here, then hand them to an agent. */
export function ContextShelf({ taskId }: { taskId?: number }) {
  const s = useStore();
  const repos = [...new Set(s.tasks.map((t) => t.repo))];
  const task = taskId !== undefined ? s.task(taskId) : undefined;
  const [tab, setTab] = useState<"task" | "project" | "inbox">(task ? "task" : "inbox");
  const [repo, setRepo] = useState(task?.repo ?? repos[0] ?? "");
  const [note, setNote] = useState("");
  const [selected, setSelected] = useState<number[]>([]);
  const [target, setTarget] = useState<number | "">(task && LIVE.includes(task.status) ? task.id : "");
  const [message, setMessage] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);

  // Tasks may load after the shelf opens: pick a project once there is one, so the
  // Project tab never quietly shows (and adds to) the inbox.
  useEffect(() => {
    if (!repo && repos.length) setRepo(repos[0]);
  }, [repo, repos]);

  const scope: Scope =
    tab === "task" && task ? { kind: "task", id: task.id } : tab === "project" && repo ? { kind: "project", repo } : { kind: "inbox" };
  const items = s.context.filter((c) => sameScope(c.scope, scope));
  const count = (sc: Scope) => s.context.filter((c) => sameScope(c.scope, sc)).length;
  const liveTasks = s.tasks.filter((t) => LIVE.includes(t.status));
  const byId = new Map(s.context.map((c) => [c.id, c]));
  const chosen = selected.map((id) => byId.get(id)).filter(Boolean) as ContextItem[];

  async function add(list: NewContext[]) {
    for (const c of list) await s.run(() => api.addContext({ scope, ...c }));
    if (list.length) s.toast(list.length === 1 ? `Added “${list[0].name}”` : `Added ${list.length} items`);
  }

  // Paste anywhere on the page (except into a text field) to drop it on the shelf.
  const addRef = useRef(add);
  addRef.current = add;
  useEffect(() => {
    async function onPaste(e: ClipboardEvent) {
      const el = e.target as HTMLElement | null;
      if (el?.closest("textarea, input")) return;
      e.preventDefault();
      addRef.current(await contextFromPaste(e));
    }
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, []);

  async function addNote(e: React.FormEvent) {
    e.preventDefault();
    const text = note.trim();
    if (!text) return;
    await add([URL_RE.test(text) ? { kind: "link", name: text, text } : { kind: "note", name: text.split("\n")[0].slice(0, 60), text }]);
    setNote("");
  }

  async function sendToAgent() {
    if (target === "") return;
    const ok = await s.run(() => api.send(target, message.trim() || "See the attached context.", selected));
    if (ok !== undefined) {
      s.toast(`Sent ${selected.length} item${selected.length === 1 ? "" : "s"} to task ${target}`);
      setSelected([]);
      setMessage("");
    }
  }

  // Mirrors context_store::describe in the daemon.
  const describe = (c: ContextItem) =>
    c.kind === "image"
      ? `- Screenshot/image: ${c.path}`
      : c.kind === "file"
        ? `- File "${c.title}": ${c.path}`
        : c.kind === "link"
          ? `- Link: ${c.text.trim()}`
          : `- Note (${c.path}): ${c.text.trim()}`;
  const preview =
    (message.trim() || "See the attached context.") +
    (chosen.length ? `\n\nAttached context (open these to see them):\n${chosen.map(describe).join("\n")}` : "");

  return (
    <section className="shelf">
      <header className="row">
        <h1>Context shelf</h1>
        <div className="segmented" role="tablist">
          {task && (
            <button role="tab" aria-selected={tab === "task"} onClick={() => setTab("task")}>
              This task <span className="muted">{count({ kind: "task", id: task.id })}</span>
            </button>
          )}
          <button role="tab" aria-selected={tab === "project"} onClick={() => setTab("project")}>
            Project
          </button>
          <button role="tab" aria-selected={tab === "inbox"} onClick={() => setTab("inbox")}>
            Inbox <span className="muted">{count({ kind: "inbox" })}</span>
          </button>
        </div>
        {tab === "project" && (
          <select aria-label="Project" value={repo} onChange={(e) => setRepo(e.target.value)}>
            {repos.length === 0 && <option value="">No projects yet</option>}
            {repos.map((r) => (
              <option key={r} value={r}>
                {repoName(r)} ({count({ kind: "project", repo: r })})
              </option>
            ))}
          </select>
        )}
      </header>

      <div className="shelf-body">
        <div className="grow">
          <div
            className="dropzone"
            onDragOver={(e) => e.preventDefault()}
            onDrop={async (e) => {
              e.preventDefault();
              add(await contextFromDrop(e));
            }}
          >
            <Icon name="clipboard" size={22} />
            <p>
              <strong>⌘V anywhere</strong> to paste a screenshot, link or text — or drop files here.
            </p>
            <button className="btn" onClick={() => fileInput.current?.click()}>
              Choose files
            </button>
            <input
              ref={fileInput}
              type="file"
              multiple
              hidden
              onChange={async (e) => {
                const dt = new DataTransfer();
                [...(e.target.files ?? [])].forEach((f) => dt.items.add(f));
                add(await contextFromDrop({ dataTransfer: dt }));
                e.target.value = "";
              }}
            />
          </div>

          <form className="note-form" onSubmit={addNote}>
            <input placeholder="Add a note or a link" value={note} onChange={(e) => setNote(e.target.value)} />
            <button className="btn" disabled={!note.trim()}>
              Add
            </button>
          </form>

          {items.length === 0 ? (
            <p className="muted empty">Nothing here yet.</p>
          ) : (
            <div className="items">
              {items.map((c) => (
                <ItemCard
                  key={c.id}
                  item={c}
                  selected={selected.includes(c.id)}
                  onToggle={() => setSelected((sel) => (sel.includes(c.id) ? sel.filter((x) => x !== c.id) : [...sel, c.id]))}
                  repos={repos}
                />
              ))}
            </div>
          )}
        </div>

        <aside className="shelf-send panel">
          <h2>Send to agent</h2>
          <label className="field">
            <span>Task</span>
            <select value={target} onChange={(e) => setTarget(e.target.value ? Number(e.target.value) : "")}>
              <option value="">Choose a running task</option>
              {liveTasks.map((t) => (
                <option key={t.id} value={t.id}>
                  #{t.id} {t.title}
                </option>
              ))}
            </select>
          </label>
          <div className="chips">
            {chosen.length === 0 && <span className="muted small">Tick items on the left to attach them.</span>}
            {chosen.map((c) => (
              <ContextChip key={c.id} item={c} onRemove={() => setSelected((sel) => sel.filter((x) => x !== c.id))} />
            ))}
          </div>
          <label className="field">
            <span>Message</span>
            <textarea rows={4} value={message} onChange={(e) => setMessage(e.target.value)} placeholder="What should the agent do with this?" />
          </label>
          <span className="small muted">What the agent receives</span>
          <pre className="preview">{preview}</pre>
          <button className="btn btn-primary" onClick={sendToAgent} disabled={target === "" || (!chosen.length && !message.trim())}>
            Send
          </button>
        </aside>
      </div>
    </section>
  );
}

function ItemCard({ item, selected, onToggle, repos }: { item: ContextItem; selected: boolean; onToggle: () => void; repos: string[] }) {
  const s = useStore();
  const [broken, setBroken] = useState(false);

  return (
    <article className={`item${selected ? " item-selected" : ""}`}>
      <div className="item-preview" onClick={onToggle}>
        {item.kind === "image" && !broken ? (
          <img src={convertFileSrc(item.path)} alt={item.title} onError={() => setBroken(true)} />
        ) : item.kind === "note" ? (
          <p className="item-note">{item.text}</p>
        ) : (
          <span className="item-icon">
            <Icon name={KIND_ICON[item.kind]} size={28} />
          </span>
        )}
      </div>
      <div className="item-meta">
        <label className="check">
          <input type="checkbox" checked={selected} onChange={onToggle} />
          <span className="ellipsis" title={item.title}>
            {item.title}
          </span>
        </label>
      </div>
      <div className="item-actions">
        <select
          aria-label="Move to"
          value=""
          onChange={(e) => e.target.value && s.run(() => api.moveContext(item.id, parseScope(e.target.value)))}
        >
          <option value="">Move to…</option>
          <option value={scopeKey({ kind: "inbox" })}>Inbox</option>
          {repos.map((r) => (
            <option key={r} value={scopeKey({ kind: "project", repo: r })}>
              Project: {repoName(r)}
            </option>
          ))}
          {s.tasks
            .filter((t) => t.status !== "done")
            .map((t) => (
              <option key={t.id} value={scopeKey({ kind: "task", id: t.id })}>
                Task #{t.id}: {t.title}
              </option>
            ))}
        </select>
        <button className="btn btn-danger" onClick={() => s.run(() => api.removeContext(item.id))}>
          Delete
        </button>
      </div>
    </article>
  );
}

export function ContextChip({ item, onRemove }: { item: ContextItem; onRemove?: () => void }) {
  return (
    <span className="chip" title={item.kind === "link" ? item.text : item.path}>
      <Icon name={KIND_ICON[item.kind]} size={14} />
      <span className="ellipsis">{item.title}</span>
      {onRemove && (
        <span
          role="button"
          tabIndex={0}
          aria-label={`Remove ${item.title}`}
          className="chip-x"
          onClick={(e) => {
            e.stopPropagation();
            onRemove();
          }}
          onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && onRemove()}
        >
          <Icon name="close" size={12} />
        </span>
      )}
    </span>
  );
}
