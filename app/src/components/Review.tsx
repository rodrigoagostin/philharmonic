import { useCallback, useEffect, useState } from "react";
import { api, type FileChange, type Task, type TaskStatus } from "../api";
import { useStore } from "../store";
import { Icon, StatusPill } from "./ui";

const LIVE: TaskStatus[] = ["queued", "running", "needs_input", "review"];

interface Line {
  kind: "add" | "del" | "ctx" | "hunk";
  text: string;
  oldNo?: number;
  newNo?: number;
}
interface FileDiff {
  path: string;
  lines: Line[];
  binary: boolean;
}
interface Comment {
  file: string;
  line: number;
  code: string;
  text: string;
}

/** Split a unified diff into files and numbered lines. */
export function parsePatch(patch: string): FileDiff[] {
  const files: FileDiff[] = [];
  let cur: FileDiff | null = null;
  let oldNo = 0;
  let newNo = 0;
  let inHeader = false;
  for (const raw of patch.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const m = / b\/(.+)$/.exec(raw);
      cur = { path: m ? m[1] : raw.slice(11), lines: [], binary: false };
      files.push(cur);
      inHeader = true;
      continue;
    }
    if (!cur) continue;
    if (raw.startsWith("@@")) {
      inHeader = false;
      const m = /@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(raw);
      oldNo = m ? Number(m[1]) : 0;
      newNo = m ? Number(m[2]) : 0;
      cur.lines.push({ kind: "hunk", text: raw });
      continue;
    }
    if (inHeader) {
      if (raw.startsWith("Binary files")) cur.binary = true;
      if (raw.startsWith("+++ b/")) cur.path = raw.slice(6);
      continue;
    }
    if (raw.startsWith("+")) cur.lines.push({ kind: "add", text: raw.slice(1), newNo: newNo++ });
    else if (raw.startsWith("-")) cur.lines.push({ kind: "del", text: raw.slice(1), oldNo: oldNo++ });
    else if (raw.startsWith(" ")) cur.lines.push({ kind: "ctx", text: raw.slice(1), oldNo: oldNo++, newNo: newNo++ });
  }
  return files;
}

export function Review({ task }: { task: Task }) {
  const s = useStore();
  const [files, setFiles] = useState<FileChange[]>([]);
  const [diffs, setDiffs] = useState<FileDiff[]>([]);
  const [loading, setLoading] = useState(true);
  const [active, setActive] = useState<string | null>(null);
  const [comments, setComments] = useState<Comment[]>([]);
  const [editing, setEditing] = useState<{ file: string; line: number; code: string } | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const live = LIVE.includes(task.status);

  const load = useCallback(async () => {
    setLoading(true);
    const r = await s.run(() => api.diff(task.id));
    if (r) {
      setFiles(r.files);
      setDiffs(parsePatch(r.patch));
    }
    setLoading(false);
  }, [task.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    load();
  }, [load]);

  const added = files.reduce((n, f) => n + f.added, 0);
  const removed = files.reduce((n, f) => n + f.removed, 0);

  function addComment() {
    if (!editing || !draft.trim()) return;
    setComments((c) => [...c, { ...editing, text: draft.trim() }]);
    setEditing(null);
    setDraft("");
  }

  async function sendComments() {
    const body = comments
      .map((c, i) => `${i + 1}. ${c.file}:${c.line} (\`${c.code.trim()}\`)\n   ${c.text.split("\n").join("\n   ")}`)
      .join("\n\n");
    const ok = await s.run(() => api.send(task.id, `Review comments on your changes. Please address each one:\n\n${body}`));
    if (ok !== undefined) {
      s.toast(`Sent ${comments.length} comment${comments.length === 1 ? "" : "s"} to the agent`);
      setComments([]);
      s.go({ name: "focus", id: task.id });
    }
  }

  async function ship(how: "pr" | "merge") {
    setBusy(true);
    const msg = await s.run(() => api.ship(task.id, how));
    setBusy(false);
    if (msg) s.toast(msg);
  }

  async function discard() {
    const ok = await s.run(() => api.discard(task.id));
    if (ok !== undefined) {
      s.toast(`Discarded task ${task.id}`);
      s.go({ name: "overview" });
    }
  }

  const shown = active ? diffs.filter((d) => d.path === active) : diffs;

  return (
    <section className="review">
      <header className="review-head">
        <button className="icon-btn" aria-label="Back" onClick={() => s.go({ name: "focus", id: task.id })}>
          <Icon name="back" />
        </button>
        <div className="grow">
          <div className="row">
            <h1 className="ellipsis">Review · {task.title}</h1>
            <StatusPill status={task.status} />
          </div>
          <p className="muted small mono">
            {task.branch} → {task.base} · {files.length} file{files.length === 1 ? "" : "s"} · <span className="add">+{added}</span>{" "}
            <span className="del">−{removed}</span>
          </p>
        </div>
        <button className="btn" onClick={load} disabled={loading}>
          Refresh
        </button>
        {confirmDiscard ? (
          <button className="btn btn-danger-solid" onClick={discard} onBlur={() => setConfirmDiscard(false)} autoFocus>
            Really discard?
          </button>
        ) : (
          <button className="btn btn-danger" onClick={() => setConfirmDiscard(true)}>
            Discard…
          </button>
        )}
        <button className="btn btn-attention" disabled={!comments.length || !live} onClick={sendComments}>
          Send {comments.length} comment{comments.length === 1 ? "" : "s"} to agent
        </button>
        <button className="btn" disabled={busy || !files.length} onClick={() => ship("merge")}>
          Merge locally
        </button>
        <button className="btn btn-primary" disabled={busy || !files.length} onClick={() => ship("pr")}>
          Create PR
        </button>
      </header>

      <div className="review-body">
        <nav className="review-files" aria-label="Changed files">
          <button className={`file-btn${active === null ? " active" : ""}`} onClick={() => setActive(null)}>
            All files
          </button>
          {files.map((f) => (
            <button key={f.path} className={`file-btn${active === f.path ? " active" : ""}`} onClick={() => setActive(f.path)}>
              <span className="ellipsis grow mono small">{f.path}</span>
              <span className="add small">+{f.added}</span>
              <span className="del small">−{f.removed}</span>
            </button>
          ))}
          {comments.length > 0 && (
            <section>
              <h2>Your comments</h2>
              {comments.map((c, i) => (
                <div key={i} className="comment-mini">
                  <span className="mono small">
                    {c.file}:{c.line}
                  </span>
                  <p className="small">{c.text}</p>
                  <button className="link-btn small" onClick={() => setComments((cs) => cs.filter((_, j) => j !== i))}>
                    Remove
                  </button>
                </div>
              ))}
            </section>
          )}
        </nav>

        <div className="diff-area">
          {loading && <p className="muted">Loading diff…</p>}
          {!loading && diffs.length === 0 && <p className="muted empty">No changes on this branch yet.</p>}
          {shown.map((d) => (
            <section key={d.path} className="diff-file">
              <h3 className="diff-file-head mono">{d.path}</h3>
              {d.binary && <p className="muted small">Binary file</p>}
              {d.lines.map((l, i) => {
                const lineNo = l.newNo ?? l.oldNo ?? 0;
                const lineComments = comments.filter((c) => c.file === d.path && c.line === lineNo && l.kind !== "hunk");
                const isEditing = editing && editing.file === d.path && editing.line === lineNo && l.kind !== "hunk";
                return (
                  <div key={i}>
                    <button
                      className={`dl dl-${l.kind}`}
                      disabled={l.kind === "hunk"}
                      title={l.kind === "hunk" ? undefined : "Comment on this line"}
                      onClick={() => {
                        setEditing({ file: d.path, line: lineNo, code: l.text });
                        setDraft("");
                      }}
                    >
                      <span className="ln">{l.oldNo ?? ""}</span>
                      <span className="ln">{l.newNo ?? ""}</span>
                      <span className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : ""}</span>
                      <span className="code">{l.text}</span>
                    </button>
                    {lineComments.map((c, j) => (
                      <div key={j} className="inline-comment small">
                        {c.text}
                      </div>
                    ))}
                    {isEditing && (
                      <div className="comment-box">
                        <textarea
                          autoFocus
                          rows={3}
                          placeholder="What should the agent change here? ⌘↵ adds, Esc cancels"
                          value={draft}
                          onChange={(e) => setDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" && e.metaKey) {
                              e.preventDefault();
                              addComment();
                            } else if (e.key === "Escape") {
                              e.stopPropagation();
                              setEditing(null);
                            }
                          }}
                        />
                        <div className="row">
                          <span className="spacer" />
                          <button className="btn" onClick={() => setEditing(null)}>
                            Cancel
                          </button>
                          <button className="btn btn-primary" onClick={addComment} disabled={!draft.trim()}>
                            Add comment
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
            </section>
          ))}
        </div>
      </div>
    </section>
  );
}
