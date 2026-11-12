import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { ago, api, repoName, type FoundSession } from "../api";
import { recentRepos, rememberRepo, useStore } from "../store";
import { Icon } from "./ui";

/** Pick a repo and bring the Claude Code threads already started there onto the Stage. */
export function ImportModal({ initialRepo }: { initialRepo?: string }) {
  const s = useStore();
  const repos = [...new Set([...recentRepos(), ...s.tasks.map((t) => t.repo)])];
  const [repo, setRepo] = useState(initialRepo ?? repos[0] ?? "");
  const [found, setFound] = useState<FoundSession[] | null>(null);
  const [error, setError] = useState<string>();
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const close = () => s.setImportOpen(false);

  useEffect(() => {
    if (!repo) return;
    let current = true;
    setFound(null);
    setError(undefined);
    api
      .discover(repo)
      .then((list) => {
        if (!current) return;
        setFound(list);
        // "Load them all": everything that isn't on the Stage yet starts ticked.
        setPicked(new Set(list.filter((f) => f.task === null).map((f) => f.id)));
      })
      .catch((e) => current && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      current = false;
    };
  }, [repo]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  async function browse() {
    const dir = await s.run(() => open({ directory: true, title: "Choose a project folder" }));
    if (typeof dir === "string") setRepo(dir);
  }

  const importable = (found ?? []).filter((f) => f.task === null);
  const allPicked = importable.length > 0 && importable.every((f) => picked.has(f.id));
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  async function doImport() {
    if (!picked.size) return;
    setBusy(true);
    const tasks = await s.run(() => api.importSessions(repo, [...picked]));
    setBusy(false);
    if (tasks) {
      rememberRepo(tasks[0]?.repo ?? repo); // the main checkout, even if you picked a subfolder
      s.toast(`Imported ${tasks.length} thread${tasks.length === 1 ? "" : "s"}. Resume one to pick up where it left off.`);
      close();
      s.go({ name: "stage" });
    }
  }

  // Where a thread runs, relative to the repo: the main checkout, a worktree, a subfolder.
  const where = (f: FoundSession) => {
    if (f.cwd === repo) return "main checkout";
    if (f.cwd.startsWith(repo + "/")) return f.cwd.slice(repo.length + 1);
    return f.cwd.split("/").slice(-2).join("/");
  };

  return (
    <div className="backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="import-title">
        <header className="modal-head">
          <h1 id="import-title">Open existing threads</h1>
          <button className="icon-btn" aria-label="Close" onClick={close}>
            <Icon name="close" />
          </button>
        </header>

        <p className="small muted">
          Claude Code conversations you already started in this project (in its folder, its worktrees or their
          subfolders). Imported threads stay where they are: same folder, same branch. Resuming one continues that exact
          conversation.
        </p>

        <label className="field">
          <span>Project</span>
          <div className="row">
            <select className="grow" value={repo} onChange={(e) => setRepo(e.target.value)}>
              {!repo && <option value="">Choose a project folder</option>}
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

        {error && <p className="small bad">{error}</p>}
        {repo && !error && found === null && <p className="small muted">Looking for threads…</p>}
        {found && found.length === 0 && (
          <p className="small muted empty">No Claude Code threads found for this project yet.</p>
        )}

        {found && found.length > 0 && (
          <>
            <div className="row">
              <label className="check small">
                <input
                  type="checkbox"
                  checked={allPicked}
                  disabled={!importable.length}
                  onChange={() => setPicked(allPicked ? new Set() : new Set(importable.map((f) => f.id)))}
                />
                Select all ({importable.length})
              </label>
            </div>
            <div className="thread-list">
              {found.map((f) => (
                <label key={f.id} className={`thread-row${f.task !== null ? " thread-done" : ""}`}>
                  <input
                    type="checkbox"
                    checked={f.task !== null || picked.has(f.id)}
                    disabled={f.task !== null}
                    onChange={() => toggle(f.id)}
                  />
                  <div className="grow thread-main">
                    <div className="row">
                      <strong className="ellipsis grow" title={f.prompt}>
                        {f.title}
                      </strong>
                      {f.task !== null && <span className="pill pill-done">On stage · #{f.task}</span>}
                      {f.task === null && f.open_elsewhere && (
                        <span
                          className="pill pill-needs_input"
                          title="It's running in another terminal. You can import it now, but quit it there before resuming it here."
                        >
                          Open elsewhere
                        </span>
                      )}
                    </div>
                    <p className="muted small ellipsis mono">
                      {f.branch} · {where(f)} · {f.prompts} prompt{f.prompts === 1 ? "" : "s"} · {ago(f.updated_at)}
                    </p>
                  </div>
                </label>
              ))}
            </div>
          </>
        )}

        <footer className="modal-foot">
          <span className="small muted grow">
            {found?.some((f) => f.task === null && f.open_elsewhere)
              ? "Threads open in another terminal: quit them there before resuming them here."
              : ""}
          </span>
          <button className="btn" onClick={close}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={doImport} disabled={busy || !picked.size}>
            {busy ? "Importing…" : `Import ${picked.size || ""} thread${picked.size === 1 ? "" : "s"}`}
          </button>
        </footer>
      </div>
    </div>
  );
}
