import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, repoName, type ContextItem } from "../api";
import { useDraft } from "../hooks";
import { recentRepos, rememberRepo, useStore } from "../store";
import { ContextChip } from "./ContextShelf";
import { contextFromDrop, contextFromPaste, Icon, type NewContext } from "./ui";

/** A ticket key in the prompt, like ABC-123 (Jira, Linear…). */
const TICKET = /\b([A-Z][A-Z0-9]+-\d+)\b/;

function slug(text: string, max: number): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, max).replace(/-+$/, "");
}

/** The part before the last `/` of the branch you used last time in this repo (e.g. `feature/`). */
const prefixKey = (repo: string) => `ph.branchPrefix:${repo}`;
function lastPrefix(repo: string): string {
  try {
    return localStorage.getItem(prefixKey(repo)) ?? "";
  } catch {
    return "";
  }
}
function rememberPrefix(repo: string, branch: string) {
  try {
    localStorage.setItem(prefixKey(repo), branch.includes("/") ? branch.slice(0, branch.lastIndexOf("/") + 1) : "");
  } catch {
    /* not important */
  }
}

/** `feature/ABC-123-fix-login` from "ABC-123 fix login", or "" when there's no ticket. */
export function suggestBranch(prompt: string, prefix: string): string {
  const m = prompt.match(TICKET);
  if (!m) return "";
  const rest = slug(prompt.split("\n")[0].replace(m[1], " "), 40);
  return `${prefix}${m[1]}${rest ? `-${rest}` : ""}`;
}

const MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"];

export function NewTaskModal() {
  const s = useStore();
  const repos = [...new Set([...recentRepos(), ...s.tasks.map((t) => t.repo)])];
  const [prompt, setPrompt, files] = useDraft("new-task");
  const [repo, setRepo] = useState(repos[0] ?? "");
  const [branches, setBranches] = useState<string[]>([]);
  const [base, setBase] = useState("");
  const [typedBranch, setTypedBranch] = useState<string | null>(null);
  const branch = typedBranch ?? suggestBranch(prompt, lastPrefix(repo));
  const [agent, setAgent] = useState<"claude" | "custom">("claude");
  const [model, setModel] = useState("");
  const [command, setCommand] = useState("");
  const [autoApprove, setAutoApprove] = useState(false);
  const attached = files.ids;
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  const close = () => s.setNewTaskOpen(false);
  // Closing without starting: files pasted here are deleted, so they don't pile up on the shelf.
  const cancel = () => {
    files.discard();
    close();
  };

  useEffect(() => {
    if (!repo) return;
    api
      .branches(repo)
      .then((names) => {
        setBranches(names);
        setBase((b) => (names.includes(b) ? b : (names.find((n) => n === "main" || n === "master") ?? names[0] ?? "")));
      })
      .catch(() => setBranches([]));
  }, [repo]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && cancel();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const byId = new Map(s.context.map((c) => [c.id, c]));
  const items = attached.map((id) => byId.get(id)).filter(Boolean) as ContextItem[];
  // Collections: the ones linked to the repo, unless you changed the selection.
  const [pickedCollections, setPickedCollections] = useState<number[] | null>(null);
  useEffect(() => setPickedCollections(null), [repo]);
  const linked = s.collections.filter((c) => c.repos.includes(repo)).map((c) => c.id);
  const collections = (pickedCollections ?? linked).filter((id) => s.collection(id));
  const otherCollections = s.collections.filter((c) => !collections.includes(c.id));

  async function newCollectionForRepo() {
    const c = await s.run(() => api.saveCollection({ name: repoName(repo), repos: [repo] }));
    if (c) setPickedCollections([...collections, c.id]);
  }

  // Items you can single out in the first prompt: from the chosen collections.
  const pickable = s.context.filter(
    (c) => !attached.includes(c.id) && c.scope.kind === "collection" && collections.includes(c.scope.id),
  );

  async function add(list: NewContext[]) {
    for (const c of list) {
      const item = await s.run(() => api.addContext({ scope: { kind: "draft" }, ...c }));
      if (item) files.uploaded(item.id);
    }
  }

  async function browse() {
    const dir = await s.run(() => open({ directory: true, title: "Choose a git repository" }));
    if (typeof dir === "string") setRepo(dir);
  }

  const canStart = prompt.trim() && repo && base && (agent === "claude" || command.trim()) && !busy;

  async function start() {
    if (!canStart) return;
    setBusy(true);
    const task = await s.run(() =>
      api.newTask({
        prompt: prompt.trim(),
        repo,
        base,
        branch: branch.trim() || null,
        agent: agent === "claude" ? { kind: "claude" } : { kind: "custom", command: command.trim() },
        model: agent === "claude" && model.trim() ? model.trim() : null,
        auto_approve: agent === "claude" && autoApprove,
        context: attached,
        collections,
      }),
    );
    setBusy(false);
    if (task) {
      files.sent(); // they now belong to the task
      rememberRepo(repo);
      if (branch.trim()) rememberPrefix(repo, branch.trim());
      close();
      s.go({ name: "focus", id: task.id });
    }
  }

  return (
    <div className="backdrop" onMouseDown={(e) => e.target === e.currentTarget && cancel()}>
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="new-task-title">
        <header className="modal-head">
          <h1 id="new-task-title">New task</h1>
          <button className="icon-btn" aria-label="Close" onClick={cancel}>
            <Icon name="close" />
          </button>
        </header>

        <label className="field">
          <span>What should the agent do?</span>
          <textarea
            autoFocus
            rows={5}
            value={prompt}
            placeholder="Describe the task. Paste screenshots or links to attach them."
            onChange={(e) => setPrompt(e.target.value)}
            onPaste={async (e) => {
              const hasFiles = e.clipboardData.files.length > 0;
              const plain = e.clipboardData.getData("text/plain").trim();
              if (!hasFiles && !/^https?:\/\/\S+$/i.test(plain)) return;
              e.preventDefault();
              add((await contextFromPaste(e)).filter((c) => c.kind !== "note"));
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && e.metaKey) {
                e.preventDefault();
                start();
              }
            }}
          />
        </label>

        <div
          className="context-drop"
          onDragOver={(e) => e.preventDefault()}
          onDrop={async (e) => {
            e.preventDefault();
            add(await contextFromDrop(e));
          }}
        >
          <span className="small muted">Collections</span>
          <div className="chips">
            {collections.map((id) => (
              <span key={id} className="chip chip-collection" title="The agent gets this collection's docs, screenshots, links and notes">
                <Icon name="clipboard" size={14} />
                <span className="ellipsis">{s.collection(id)?.name}</span>
                <span
                  role="button"
                  tabIndex={0}
                  aria-label={`Don't use ${s.collection(id)?.name}`}
                  className="chip-x"
                  onClick={() => setPickedCollections(collections.filter((x) => x !== id))}
                >
                  <Icon name="close" size={12} />
                </span>
              </span>
            ))}
            {otherCollections.length > 0 && (
              <select
                aria-label="Add a collection"
                value=""
                onChange={(e) => e.target.value && setPickedCollections([...collections, Number(e.target.value)])}
              >
                <option value="">+ Add a collection…</option>
                {otherCollections.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            )}
            {repo && linked.length === 0 && (
              <button className="link-btn small" onClick={newCollectionForRepo}>
                + New collection “{repoName(repo)}”
              </button>
            )}
          </div>
          <span className="small muted">Attached to this task</span>
          <div className="chips">
            {items.length === 0 && <span className="small muted">Drop files here or paste into the prompt.</span>}
            {items.map((c) => (
              <ContextChip key={c.id} item={c} onRemove={() => files.remove(c.id)} />
            ))}
          </div>
          <button className="link-btn small" onClick={() => setPicking((p) => !p)} disabled={!pickable.length}>
            Point at an item… ({pickable.length})
          </button>
          {picking && pickable.length > 0 && (
            <div className="picker">
              {pickable.map((c) => (
                <button
                  key={c.id}
                  className="picker-row"
                  onClick={() => {
                    files.pick(c.id);
                    setPicking(false);
                  }}
                >
                  {/* ✕ deletes it from its collection (attaching is the row itself). */}
              <ContextChip item={c} onRemove={() => s.run(() => api.removeContext(c.id))} />
                </button>
              ))}
            </div>
          )}
        </div>

        <div className="grid2">
          <label className="field">
            <span>Repository</span>
            <div className="row">
              <select className="grow" value={repo} onChange={(e) => setRepo(e.target.value)}>
                {!repo && <option value="">Choose a repository</option>}
                {repo && !repos.includes(repo) && <option value={repo}>{repoName(repo)}</option>}
                {repos.map((r) => (
                  <option key={r} value={r} title={r}>
                    {repoName(r)}
                  </option>
                ))}
              </select>
              <button className="btn" onClick={browse}>
                Browse…
              </button>
            </div>
          </label>
          <label className="field">
            <span>Base branch</span>
            <select value={base} onChange={(e) => setBase(e.target.value)} disabled={!branches.length}>
              {branches.length === 0 && <option value="">—</option>}
              {branches.map((b) => (
                <option key={b} value={b}>
                  {b}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>Agent</span>
            <select value={agent} onChange={(e) => setAgent(e.target.value as "claude" | "custom")}>
              <option value="claude">Claude Code</option>
              <option value="custom">Custom command</option>
            </select>
          </label>
          {agent === "claude" ? (
            <label className="field">
              <span>Model</span>
              <input list="models" value={model} placeholder="Default" onChange={(e) => setModel(e.target.value)} />
              <datalist id="models">
                {MODELS.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            </label>
          ) : (
            <label className="field">
              <span>Command</span>
              <input className="mono" value={command} placeholder='codex "$PH_PROMPT"' onChange={(e) => setCommand(e.target.value)} />
            </label>
          )}
        </div>

        <fieldset className="iso">
          <legend>Isolation</legend>
          <label className="field">
            <span>New branch</span>
            <input
              className="mono"
              value={branch}
              placeholder={`ph/${slug(prompt.split("\n")[0], 40) || "…"}`}
              onChange={(e) => setTypedBranch(e.target.value)}
              spellCheck={false}
            />
          </label>
          <p className="small muted">
            The agent works in its own git worktree on this branch, off <code>{base || "the base branch"}</code>. Your checkout is
            never touched. A ticket key in the prompt (like <code>ABC-123</code>) goes into the name; leave it empty for{" "}
            <code>ph/…</code>.
          </p>
          {agent === "claude" && (
            <label className="check small">
              <input type="checkbox" checked={autoApprove} onChange={(e) => setAutoApprove(e.target.checked)} />
              Auto-approve file edits (still asks before running commands)
            </label>
          )}
        </fieldset>

        <footer className="modal-foot">
          <span className="small muted grow">
            The first time, Claude Code asks whether you trust the new folder. Answer it from the task card.
          </span>
          <button className="btn" onClick={cancel}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={start} disabled={!canStart}>
            {busy ? "Starting…" : "Start task"} <kbd className="kbd kbd-dark">⌘↵</kbd>
          </button>
        </footer>
      </div>
    </div>
  );
}
