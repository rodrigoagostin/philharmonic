import type { Editor, FileChange } from "../api";

/** Which agent's files to show: an agent id ("" = the main agent), "?" = unknown, null = all. */
export type EditorPick = string | null;

/** The agents that edited these files, main agent first, plus "?" if some files have no known editor. */
export function editorsOf(files: FileChange[]): { agent: string; name: string; files: number }[] {
  const byId = new Map<string, { agent: string; name: string; files: number }>();
  let unknown = 0;
  for (const f of files) {
    if (!f.by.length) unknown++;
    for (const e of f.by) {
      const cur = byId.get(e.agent) ?? { agent: e.agent, name: e.name, files: 0 };
      cur.files++;
      byId.set(e.agent, cur);
    }
  }
  const list = [...byId.values()].sort((a, b) => (a.agent === "" ? -1 : b.agent === "" ? 1 : a.name.localeCompare(b.name)));
  if (unknown && list.length) list.push({ agent: "?", name: "unknown", files: unknown });
  return list;
}

export function editedBy(f: FileChange, pick: EditorPick): boolean {
  if (pick === null) return true;
  if (pick === "?") return f.by.length === 0;
  return f.by.some((e) => e.agent === pick);
}

/** "All · main 12 · Write the tests 3": shown only when more than one agent edited files. */
export function EditorFilter({ files, pick, onPick }: { files: FileChange[]; pick: EditorPick; onPick: (p: EditorPick) => void }) {
  const editors = editorsOf(files);
  if (editors.length < 2) return null;
  return (
    <div className="editor-filter" role="group" aria-label="Show files edited by">
      <button className={`editor-chip${pick === null ? " active" : ""}`} onClick={() => onPick(null)}>
        All
      </button>
      {editors.map((e) => (
        <button
          key={e.agent}
          className={`editor-chip${pick === e.agent ? " active" : ""}`}
          title={e.agent === "?" ? "Changed some other way: a shell command, you, another thread…" : e.name}
          onClick={() => onPick(pick === e.agent ? null : e.agent)}
        >
          <span className="ellipsis">{e.agent === "" ? "Main agent" : e.name}</span> <span className="muted">{e.files}</span>
        </button>
      ))}
    </div>
  );
}

/** Small "main · Write the tests" under a file, when subagents were involved. */
export function EditedByLabel({ by, show }: { by: Editor[]; show: boolean }) {
  if (!show || !by.length) return null;
  return <span className="edited-by ellipsis">{by.map((e) => (e.agent === "" ? "main" : e.name)).join(" · ")}</span>;
}
