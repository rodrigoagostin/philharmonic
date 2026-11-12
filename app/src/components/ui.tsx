import type { ClipboardEvent, DragEvent } from "react";
import { fileToB64, STATUS_LABEL, type ContextKind, type TaskStatus } from "../api";

const PATHS: Record<string, string> = {
  grid: "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
  bell: "M6 16V11a6 6 0 1 1 12 0v5l2 2H4zM10 20a2 2 0 0 0 4 0",
  plus: "M12 5v14M5 12h14",
  back: "M15 5l-7 7 7 7",
  close: "M6 6l12 12M18 6L6 18",
  image: "M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M15 9h.01",
  link: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1",
  note: "M5 4h10l4 4v12H5zM15 4v4h4M8 12h8M8 16h6",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  clipboard: "M9 4h6v3H9zM7 5H5v16h14V5h-2",
  baton: "M4 20L18 6M16 4l4 4M3 21l2-2",
  diff: "M7 4v10M2 9h10M14 18h8M17 4v6M14 7h6",
  import: "M12 4v11M7 10l5 5 5-5M5 20h14",
};

export function Icon({ name, size = 16 }: { name: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[name] ?? ""} />
    </svg>
  );
}

export const KIND_ICON: Record<ContextKind, string> = { image: "image", link: "link", note: "note", file: "file" };

export function StatusPill({ status }: { status: TaskStatus }) {
  return <span className={`pill pill-${status}`}>{STATUS_LABEL[status]}</span>;
}

export function Dot({ status }: { status: TaskStatus }) {
  return <span className={`dot dot-${status}`} title={STATUS_LABEL[status]} />;
}

/** Something to put on the context shelf, before the daemon has stored it. */
export interface NewContext {
  kind: ContextKind;
  name: string;
  data?: string;
  text?: string;
}

export const URL_RE = /^https?:\/\/\S+$/i;

export function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}.${p(d.getMinutes())}.${p(d.getSeconds())}`;
}

export async function fromFiles(files: File[]): Promise<NewContext[]> {
  return Promise.all(
    files.map(async (f) => {
      const image = f.type.startsWith("image/");
      const ext = f.type.split("/")[1] ?? "png";
      const name = image && (!f.name || f.name === "image.png") ? `Screenshot ${stamp()}.${ext}` : f.name || `File ${stamp()}`;
      return { kind: image ? "image" : "file", name, data: await fileToB64(f) } as NewContext;
    }),
  );
}

/** What a paste contains: files (screenshots!), else a link, else a note. */
export async function contextFromPaste(e: ClipboardEvent | globalThis.ClipboardEvent): Promise<NewContext[]> {
  const dt = e.clipboardData;
  if (!dt) return [];
  const files = [...dt.files];
  if (files.length) return fromFiles(files);
  const text = dt.getData("text/plain").trim();
  if (!text) return [];
  if (URL_RE.test(text)) return [{ kind: "link", name: text, text }];
  return [{ kind: "note", name: text.split("\n")[0].slice(0, 60), text }];
}

export async function contextFromDrop(e: DragEvent | { dataTransfer: DataTransfer | null }): Promise<NewContext[]> {
  const dt = e.dataTransfer;
  if (!dt) return [];
  const files = [...dt.files];
  if (files.length) return fromFiles(files);
  const text = (dt.getData("text/uri-list") || dt.getData("text/plain")).trim();
  if (!text) return [];
  if (URL_RE.test(text)) return [{ kind: "link", name: text, text }];
  return [{ kind: "note", name: text.split("\n")[0].slice(0, 60), text }];
}

/** The last `n` meaningful lines of terminal text (skips blank lines and box-drawing rules). */
export function tail(text: string, n: number): string {
  return text
    .split("\n")
    .map((l) => l.trimEnd())
    .filter((l) => l.trim() && !/^[\s─━]+$/.test(l))
    .slice(-n)
    .join("\n");
}
