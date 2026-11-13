import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api, repoName, type Collection, type ContextItem, type Scope } from "../api";
import { recentRepos, useStore } from "../store";
import { contextFromDrop, contextFromPaste, Icon, KIND_ICON, StatusPill, URL_RE, type NewContext } from "./ui";

function sameScope(a: Scope, b: Scope) {
  return scopeKey(a) === scopeKey(b);
}

const scopeKey = (scope: Scope) => (scope.kind === "draft" ? "draft" : `${scope.kind}:${scope.id}`);
const parseScope = (key: string): Scope => {
  const [kind, id] = key.split(":");
  return { kind: kind === "task" ? "task" : "collection", id: Number(id) };
};

/**
 * Context for your agents, grouped in collections: one per project, per feature, per
 * product… whatever you work on. Tasks use the collections linked to their repo, plus any
 * you add. Each task also has its own items (what you paste into its message box).
 */
export function ContextShelf({ taskId, collectionId }: { taskId?: number; collectionId?: number }) {
  const s = useStore();
  const task = taskId !== undefined ? s.task(taskId) : undefined;
  const firstCollection = (task?.collections ?? []).find((id) => s.collection(id)) ?? s.collections[0]?.id;
  const [selected, setSelected] = useState<Scope | null>(
    collectionId !== undefined
      ? { kind: "collection", id: collectionId }
      : task
        ? { kind: "task", id: task.id }
        : firstCollection !== undefined
          ? { kind: "collection", id: firstCollection }
          : null,
  );
  // Nothing selected yet (collections load after the screen opens): pick the first. The
  // selected collection was deleted: same. A collection you just made isn't in the list for
  // a moment, so only fall back for one that was seen before and is gone now.
  const seen = useRef(new Set<number>());
  s.collections.forEach((c) => seen.current.add(c.id));
  const gone = selected?.kind === "collection" && seen.current.has(selected.id) && !s.collection(selected.id);
  useEffect(() => {
    if ((!selected || gone) && firstCollection !== undefined) setSelected({ kind: "collection", id: firstCollection });
  }, [selected, gone, firstCollection]);

  const count = (sc: Scope) => s.context.filter((c) => sameScope(c.scope, sc)).length;
  const used = task ? s.collections.filter((c) => task.collections.includes(c.id)) : [];
  const others = s.collections.filter((c) => !used.includes(c));

  return (
    <section className="shelf ctx">
      <aside className="ctx-list">
        <h1>Context</h1>
        <p className="small muted">
          Docs, screenshots, links and notes for your agents, grouped in collections: a project, a feature, a product… Agents
          only see the collections their task uses.
        </p>
        {task && (
          <>
            <h2 className="ctx-group">Task #{task.id}</h2>
            <ListRow
              active={selected?.kind === "task"}
              onClick={() => setSelected({ kind: "task", id: task.id })}
              title="Only this task"
              detail={task.title}
              count={count({ kind: "task", id: task.id })}
            />
            {used.map((c) => (
              <CollectionRow key={c.id} c={c} selected={selected} onSelect={setSelected} count={count} />
            ))}
          </>
        )}
        <h2 className="ctx-group">{task ? "Other collections" : "Collections"}</h2>
        {others.map((c) => (
          <CollectionRow key={c.id} c={c} selected={selected} onSelect={setSelected} count={count} />
        ))}
        {s.collections.length === 0 && <p className="small muted">No collections yet.</p>}
        <NewCollection
          defaultRepo={task?.repo ?? (s.filter?.kind === "repo" ? s.filter.repo : undefined)}
          onCreated={(c) => setSelected({ kind: "collection", id: c.id })}
        />
      </aside>

      <div className="grow ctx-main">
        {selected?.kind === "collection" && s.collection(selected.id) ? (
          <CollectionPanel key={selected.id} c={s.collection(selected.id)!} task={task} />
        ) : selected?.kind === "task" && task ? (
          <>
            <header className="ctx-head">
              <h2>Only task #{task.id}</h2>
              <p className="small muted">
                Things for this task alone, like screenshots pasted into its message box. Deleted with the task.
              </p>
            </header>
            <Items scope={{ kind: "task", id: task.id }} />
          </>
        ) : (
          <div className="ctx-empty">
            <h2>Make your first collection</h2>
            <p className="muted">
              For example one per project (“Quilora”, “Pawpers”), or one per feature (“Quilora · checkout”). Link it to a repo and
              new tasks there get it automatically.
            </p>
          </div>
        )}
      </div>
    </section>
  );
}

function ListRow(props: { active: boolean; onClick: () => void; title: string; detail: string; count: number }) {
  return (
    <button className={`ctx-row${props.active ? " active" : ""}`} onClick={props.onClick}>
      <span className="grow ellipsis">
        <strong>{props.title}</strong>
        <span className="small muted ellipsis">{props.detail}</span>
      </span>
      <span className="small muted">{props.count}</span>
    </button>
  );
}

function CollectionRow(props: { c: Collection; selected: Scope | null; onSelect: (s: Scope) => void; count: (s: Scope) => number }) {
  const { c } = props;
  const sc: Scope = { kind: "collection", id: c.id };
  return (
    <ListRow
      active={!!props.selected && sameScope(props.selected, sc)}
      onClick={() => props.onSelect(sc)}
      title={c.name}
      detail={c.repos.length ? c.repos.map(repoName).join(", ") : "not linked to a repo"}
      count={props.count(sc)}
    />
  );
}

function NewCollection({ defaultRepo, onCreated }: { defaultRepo?: string; onCreated: (c: Collection) => void }) {
  const s = useStore();
  const [name, setName] = useState("");
  const [link, setLink] = useState(true);
  async function create(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    const c = await s.run(() => api.saveCollection({ name: name.trim(), repos: link && defaultRepo ? [defaultRepo] : [] }));
    if (c) {
      setName("");
      onCreated(c);
    }
  }
  return (
    <form className="ctx-new" onSubmit={create}>
      <input placeholder="New collection" value={name} onChange={(e) => setName(e.target.value)} aria-label="New collection name" />
      {defaultRepo && name.trim() && (
        <label className="check small">
          <input type="checkbox" checked={link} onChange={(e) => setLink(e.target.checked)} />
          Link to {repoName(defaultRepo)}
        </label>
      )}
      <button className="btn" disabled={!name.trim()}>
        <Icon name="plus" /> Add
      </button>
    </form>
  );
}

/** A collection: its name, linked repos, items and the tasks using it. */
function CollectionPanel({ c, task }: { c: Collection; task?: ReturnType<ReturnType<typeof useStore>["task"]> }) {
  const s = useStore();
  const [name, setName] = useState(c.name);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const knownRepos = [...new Set([...s.tasks.map((t) => t.repo), ...recentRepos()])].filter((r) => !c.repos.includes(r));
  const usedBy = s.tasks.filter((t) => t.collections.includes(c.id));
  const save = (patch: Partial<Collection>) =>
    s.run(() => api.saveCollection({ id: c.id, name: patch.name ?? c.name, repos: patch.repos ?? c.repos }));
  const inTask = task ? task.collections.includes(c.id) : false;
  const live = task && ["queued", "running", "needs_input", "review"].includes(task.status);

  return (
    <>
      <header className="ctx-head">
        <div className="row">
          <input
            className="ctx-name grow"
            aria-label="Collection name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={() => name.trim() && name.trim() !== c.name && save({ name: name.trim() })}
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
          />
          {task && (
            <button
              className={`btn${inTask ? "" : " btn-primary"}`}
              title={live && !inTask ? "The agent hears about it with your next message" : undefined}
              onClick={() =>
                s.run(() =>
                  api.setTaskCollections(task.id, inTask ? task.collections.filter((x) => x !== c.id) : [...task.collections, c.id]),
                )
              }
            >
              {inTask ? `Stop using in #${task.id}` : `Use in #${task.id}`}
            </button>
          )}
          {confirmDelete ? (
            <>
              <button className="btn btn-danger" onClick={() => s.run(() => api.deleteCollection(c.id))}>
                Delete it and its items
              </button>
              <button className="btn" onClick={() => setConfirmDelete(false)}>
                Keep
              </button>
            </>
          ) : (
            <button className="btn" onClick={() => setConfirmDelete(true)}>
              Delete
            </button>
          )}
        </div>
        <div className="row ctx-repos">
          <span className="small muted">New tasks in</span>
          {c.repos.map((r) => (
            <span key={r} className="chip" title={r}>
              <span>{repoName(r)}</span>
              <span
                role="button"
                tabIndex={0}
                aria-label={`Unlink ${repoName(r)}`}
                className="chip-x"
                onClick={() => save({ repos: c.repos.filter((x) => x !== r) })}
              >
                <Icon name="close" size={12} />
              </span>
            </span>
          ))}
          {c.repos.length === 0 && <span className="small muted">no repo</span>}
          <select
            aria-label="Link a repo"
            value=""
            onChange={(e) => e.target.value && save({ repos: [...c.repos, e.target.value] })}
          >
            <option value="">+ Link a repo…</option>
            {knownRepos.map((r) => (
              <option key={r} value={r} title={r}>
                {repoName(r)}
              </option>
            ))}
          </select>
          <span className="small muted">use it automatically.</span>
        </div>
      </header>

      <Items scope={{ kind: "collection", id: c.id }} />

      <h3 className="ctx-tasks-head">
        Tasks using it <span className="muted">{usedBy.length}</span>
      </h3>
      {usedBy.length === 0 ? (
        <p className="small muted">None yet.</p>
      ) : (
        <div className="ctx-tasks">
          {usedBy.map((t) => (
            <button key={t.id} className="ctx-task" onClick={() => s.go({ name: "focus", id: t.id })}>
              <span className="task-num">#{t.id}</span>
              <span className="grow ellipsis">{t.title}</span>
              <span className="small muted">{repoName(t.repo)}</span>
              <StatusPill status={t.status} />
            </button>
          ))}
        </div>
      )}
    </>
  );
}

/** Add (paste, drop, type) and list the items of one collection or task. */
function Items({ scope }: { scope: Scope }) {
  const s = useStore();
  const [note, setNote] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  const items = s.context.filter((c) => sameScope(c.scope, scope));

  async function add(list: NewContext[]) {
    for (const c of list) await s.run(() => api.addContext({ scope, ...c }));
    if (list.length) s.toast(list.length === 1 ? `Added “${list[0].name}”` : `Added ${list.length} items`);
  }

  // Paste anywhere on the page (except into a text field) to add it here.
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

  return (
    <>
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
          <strong>⌘V anywhere</strong> to paste a screenshot, link or text, or drop files here.
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
            <ItemCard key={c.id} item={c} />
          ))}
        </div>
      )}
    </>
  );
}

function ItemCard({ item }: { item: ContextItem }) {
  const s = useStore();
  const [broken, setBroken] = useState(false);
  const here = scopeKey(item.scope);

  return (
    <article className="item">
      <div className="item-preview">
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
        <span className="ellipsis" title={item.kind === "link" ? item.text : item.title}>
          {item.title}
        </span>
      </div>
      <div className="item-actions">
        <select
          aria-label="Move to"
          value=""
          onChange={(e) => e.target.value && s.run(() => api.moveContext(item.id, parseScope(e.target.value)))}
        >
          <option value="">Move to…</option>
          {s.collections
            .filter((c) => scopeKey({ kind: "collection", id: c.id }) !== here)
            .map((c) => (
              <option key={c.id} value={scopeKey({ kind: "collection", id: c.id })}>
                {c.name}
              </option>
            ))}
          {s.tasks
            .filter((t) => t.status !== "done" && scopeKey({ kind: "task", id: t.id }) !== here)
            .map((t) => (
              <option key={t.id} value={scopeKey({ kind: "task", id: t.id })}>
                Only task #{t.id}: {t.title}
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

/** In a task: the collections it uses (add / remove), and how many items are its own. */
export function TaskContextPanel({ task }: { task: NonNullable<ReturnType<ReturnType<typeof useStore>["task"]>> }) {
  const s = useStore();
  const used = task.collections.map((id) => s.collection(id)).filter(Boolean) as Collection[];
  const others = s.collections.filter((c) => !task.collections.includes(c.id));
  const own = s.context.filter((c) => c.scope.kind === "task" && c.scope.id === task.id).length;
  const live = ["queued", "running", "needs_input", "review"].includes(task.status);
  const set = (ids: number[]) => s.run(() => api.setTaskCollections(task.id, ids));

  return (
    <section className="panel">
      <h2>
        Context <span className="muted">{used.length}</span>
      </h2>
      <div className="chips">
        {used.length === 0 && <span className="small muted">No collections.</span>}
        {used.map((c) => (
          <span key={c.id} className="chip chip-collection" title={`${s.context.filter((i) => i.scope.kind === "collection" && i.scope.id === c.id).length} items`}>
            <button className="chip-link" onClick={() => s.go({ name: "context", taskId: task.id, collectionId: c.id })}>
              {c.name}
            </button>
            <span
              role="button"
              tabIndex={0}
              aria-label={`Stop using ${c.name}`}
              className="chip-x"
              onClick={() => set(task.collections.filter((x) => x !== c.id))}
            >
              <Icon name="close" size={12} />
            </span>
          </span>
        ))}
      </div>
      <div className="row">
        {others.length > 0 && (
          <select aria-label="Use another collection" value="" onChange={(e) => e.target.value && set([...task.collections, Number(e.target.value)])}>
            <option value="">+ Use a collection…</option>
            {others.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        )}
        <span className="spacer" />
        <button className="link-btn small" onClick={() => s.go({ name: "context", taskId: task.id })}>
          {own ? `${own} item${own === 1 ? "" : "s"} only for this task · ` : ""}Open →
        </button>
      </div>
      {live && <p className="small muted">The agent hears about collections you add with your next message.</p>}
    </section>
  );
}
