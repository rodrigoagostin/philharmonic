import { useState } from "react";
import { api, type ContextItem } from "../api";
import { useStore } from "../store";
import { ContextChip } from "./ContextShelf";
import { contextFromDrop, contextFromPaste, type NewContext } from "./ui";

/** Message box for a running agent. Paste a screenshot or a link and it's attached. */
export function Composer({ taskId, disabled }: { taskId: number; disabled?: boolean }) {
  const s = useStore();
  const [text, setText] = useState("");
  const [attached, setAttached] = useState<number[]>([]);
  const [picking, setPicking] = useState(false);
  const task = s.task(taskId);

  const byId = new Map(s.context.map((c) => [c.id, c]));
  const items = attached.map((id) => byId.get(id)).filter(Boolean) as ContextItem[];
  const pickable = s.context.filter(
    (c) =>
      !attached.includes(c.id) &&
      ((c.scope.kind === "task" && c.scope.id === taskId) ||
        c.scope.kind === "inbox" ||
        (c.scope.kind === "project" && c.scope.repo === task?.repo)),
  );

  async function add(list: NewContext[]) {
    for (const c of list) {
      const item = await s.run(() => api.addContext({ scope: { kind: "task", id: taskId }, ...c }));
      if (item) setAttached((a) => [...a, item.id]);
    }
  }

  async function send() {
    if (disabled || (!text.trim() && !attached.length)) return;
    const ok = await s.run(() => api.send(taskId, text.trim() || "See the attached context.", attached));
    if (ok !== undefined) {
      setText("");
      setAttached([]);
      s.toast("Sent to the agent");
    }
  }

  return (
    <div
      className="composer"
      onDragOver={(e) => e.preventDefault()}
      onDrop={async (e) => {
        e.preventDefault();
        add(await contextFromDrop(e));
      }}
    >
      <textarea
        id={`composer-${taskId}`}
        rows={4}
        disabled={disabled}
        placeholder={disabled ? "The agent isn't running." : "Type a message. Paste screenshots or links to attach them. ⌘↵ sends."}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onPaste={async (e) => {
          const hasFiles = e.clipboardData.files.length > 0;
          const plain = e.clipboardData.getData("text/plain").trim();
          const isLink = /^https?:\/\/\S+$/i.test(plain);
          if (!hasFiles && !isLink) return; // plain text: paste normally
          e.preventDefault();
          add((await contextFromPaste(e)).filter((c) => c.kind !== "note"));
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.metaKey) {
            e.preventDefault();
            send();
          }
        }}
      />
      {items.length > 0 && (
        <div className="chips">
          {items.map((c) => (
            <ContextChip key={c.id} item={c} onRemove={() => setAttached((a) => a.filter((x) => x !== c.id))} />
          ))}
        </div>
      )}
      <div className="row">
        <button className="link-btn small" onClick={() => setPicking((p) => !p)} disabled={!pickable.length}>
          Attach from shelf ({pickable.length})
        </button>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={send} disabled={disabled || (!text.trim() && !attached.length)}>
          Send <kbd className="kbd kbd-dark">⌘↵</kbd>
        </button>
      </div>
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
              <span className="muted small">{c.scope.kind}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
