import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, repoName, type ContextItem } from "../api";
import { recentRepos, rememberRepo, useStore } from "../store";
import { ContextChip } from "./ContextShelf";
import { contextFromDrop, contextFromPaste, Icon, type NewContext } from "./ui";

const MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"];

export function NewTaskModal() {
  const s = useStore();
  const repos = [...new Set([...recentRepos(), ...s.tasks.map((t) => t.repo)])];
  const [prompt, setPrompt] = useState("");
  const [repo, setRepo] = useState(repos[0] ?? "");
  const [branches, setBranches] = useState<string[]>([]);
  const [base, setBase] = useState("");
  const [agent, setAgent] = useState<"claude" | "custom">("claude");
  const [model, setModel] = useState("");
  const [command, setCommand] = useState("");
  const [autoApprove, setAutoApprove] = useState(false);
  const [attached, setAttached] = useState<number[]>([]);
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  const close = () => s.setNewTaskOpen(false);
  // Files pasted or dropped here go to the inbox right away. If you cancel, they go too.
  const uploaded = useRef<number[]>([]);
  const cancel = () => {
    uploaded.current.forEach((id) => api.removeContext(id).catch(() => {}));
    close();
  };

  useEffect(() => {
    if (!repo) return;
    // Switching repos quickly: ignore answers for a repo that's no longer selected.
    let current = true;
    api
      .branches(repo)
      .then((names) => {
        if (!current) return;
        setBranches(names);
        setBase((b) => (names.includes(b) ? b : (names.find((n) => n === "main" || n === "master") ?? names[0] ?? "")));
      })
      .catch(() => current && setBranches([]));
    return () => {
      current = false;
    };
  }, [repo]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && cancel();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const byId = new Map(s.context.map((c) => [c.id, c]));
  const items = attached.map((id) => byId.get(id)).filter(Boolean) as ContextItem[];
  const pickable = s.context.filter(
    (c) => !attached.includes(c.id) && (c.scope.kind === "inbox" || (c.scope.kind === "project" && c.scope.repo === repo)),
  );

  async function add(list: NewContext[]) {
    for (const c of list) {
      const item = await s.run(() => api.addContext({ scope: { kind: "inbox" }, ...c }));
      if (item) {
        uploaded.current.push(item.id);
        setAttached((a) => [...a, item.id]);
      }
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
        agent: agent === "claude" ? { kind: "claude" } : { kind: "custom", command: command.trim() },
        model: agent === "claude" && model.trim() ? model.trim() : null,
        auto_approve: agent === "claude" && autoApprove,
        context: attached,
      }),
    );
    setBusy(false);
    if (task) {
      rememberRepo(repo);
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
          <span className="small muted">Context</span>
          <div className="chips">
            {items.length === 0 && <span className="small muted">Drop files here or paste into the prompt.</span>}
            {items.map((c) => (
              <ContextChip key={c.id} item={c} onRemove={() => setAttached((a) => a.filter((x) => x !== c.id))} />
            ))}
          </div>
          <button className="link-btn small" onClick={() => setPicking((p) => !p)} disabled={!pickable.length}>
            From shelf… ({pickable.length})
          </button>
          {picking && (
            <div className="picker">
              {pickable.map((c) => (
                <button
                  key={c.id}
                  className="picker-row"
                  onClick={() => {
                    setAttached((a) => [...a, c.id]);
                    setPicking(false);
                  }}
                >
                  <ContextChip item={c} />
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
          <p className="small muted">
            The agent works in its own git worktree on a new branch <code>ph/…</code> off <code>{base || "the base branch"}</code>. Your
            checkout is never touched.
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
