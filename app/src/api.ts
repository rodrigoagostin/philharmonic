// Types mirror crates/core (protocol.rs, task.rs, context.rs). Keep them in sync.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type TaskStatus = "queued" | "running" | "needs_input" | "review" | "stopped" | "done" | "failed";

export type Agent = { kind: "claude" } | { kind: "custom"; command: string };

export interface Task {
  id: number;
  title: string;
  prompt: string;
  repo: string;
  base: string;
  branch: string;
  worktree: string;
  agent: Agent;
  model: string | null;
  auto_approve: boolean;
  status: TaskStatus;
  created_at: number;
}

export type Scope = { kind: "task"; id: number } | { kind: "project"; repo: string } | { kind: "inbox" };
export type ContextKind = "image" | "link" | "note" | "file";

export interface ContextItem {
  id: number;
  scope: Scope;
  kind: ContextKind;
  title: string;
  path: string;
  text: string;
  created_at: number;
}

export interface FileChange {
  path: string;
  added: number;
  removed: number;
}

export type DaemonEvent =
  | { kind: "task_updated"; task: Task }
  | { kind: "task_removed"; id: number }
  | { kind: "output"; id: number; data: string; offset: number }
  | { kind: "context_changed" }
  | { kind: "resync" };

/** Send a request; returns the response, or throws the daemon's error message. */
export async function request<T = any>(req: Record<string, unknown>): Promise<T> {
  const res = await invoke<any>("daemon", { request: req });
  if (res?.type === "error") throw new Error(res.message);
  return res as T;
}

// Keystrokes and resizes must reach the agent in the order they happened. Each request
// travels on its own connection and the daemon handles connections in parallel, so they
// are sent one after another: the next leaves when the previous has been answered.
// One queue per task, so a stuck agent doesn't hold up typing into the others.
const terminalQueues = new Map<number, Promise<unknown>>();
function inOrder<T>(id: number, send: () => Promise<T>): Promise<T> {
  const result = (terminalQueues.get(id) ?? Promise.resolve()).then(send);
  terminalQueues.set(id, result.catch(() => {}));
  return result;
}

export const api = {
  listTasks: async () => (await request<{ tasks: Task[] }>({ type: "list_tasks" })).tasks,
  newTask: async (args: {
    prompt: string;
    repo: string;
    base: string;
    agent: Agent;
    model: string | null;
    auto_approve: boolean;
    context: number[];
  }) => (await request<{ task: Task }>({ type: "new_task", ...args })).task,
  stop: (id: number) => request({ type: "stop", id }),
  resume: (id: number) => request({ type: "resume", id }),
  discard: (id: number) => request({ type: "discard", id }),
  markDone: (id: number) => request({ type: "mark_done", id }),
  input: (id: number, bytes: Uint8Array) => inOrder(id, () => request({ type: "input", id, data: bytesToB64(bytes) })),
  key: (id: number, key: string) =>
    inOrder(id, () => request({ type: "input", id, data: bytesToB64(new TextEncoder().encode(key)) })),
  resize: (id: number, cols: number, rows: number) => inOrder(id, () => request({ type: "resize", id, cols, rows })),
  /** Full output history (to replay) + visible text. `end` = stream offset after the history. */
  screen: (id: number) => request<{ history: string; text: string; end: number }>({ type: "screen", id }),
  /** Just the visible text: cheap, for previews. */
  screenText: async (id: number) => (await request<{ text: string }>({ type: "screen_text", id })).text,
  send: (id: number, text: string, context: number[] = []) => request({ type: "send", id, text, context }),
  diff: (id: number) => request<{ files: FileChange[]; patch: string }>({ type: "diff", id }),
  ship: async (id: number, how: "pr" | "merge") =>
    (await request<{ message: string }>({ type: "ship", id, how })).message,
  listContext: async () => (await request<{ items: ContextItem[] }>({ type: "list_context" })).items,
  addContext: async (args: { scope: Scope; kind: ContextKind; name: string; data?: string; text?: string }) =>
    (await request<{ item: ContextItem }>({ type: "add_context", ...args })).item,
  moveContext: (id: number, scope: Scope) => request({ type: "move_context", id, scope }),
  removeContext: (id: number) => request({ type: "remove_context", id }),
  branches: async (repo: string) => (await request<{ names: string[] }>({ type: "branches", repo })).names,
};

// ---- events ----

/** `offset` is where `bytes` start in the task's output stream (see `api.screen().end`). */
type OutputListener = (bytes: Uint8Array, offset: number) => void;
const outputListeners = new Map<number, Set<OutputListener>>();
const resyncListeners = new Set<() => void>();

/** Called when the app may have missed events (the daemon said so, or it reconnected). */
export function onResync(fn: () => void): () => void {
  resyncListeners.add(fn);
  return () => resyncListeners.delete(fn);
}

function resync() {
  resyncListeners.forEach((fn) => fn());
}

/** Terminal output arrives very often; components subscribe per task instead of via React state. */
export function onOutput(id: number, fn: OutputListener): () => void {
  if (!outputListeners.has(id)) outputListeners.set(id, new Set());
  outputListeners.get(id)!.add(fn);
  return () => outputListeners.get(id)?.delete(fn);
}

export async function listenToDaemon(handlers: {
  onEvent: (e: DaemonEvent) => void;
  onConnection: (connected: boolean, error?: string) => void;
}) {
  const un1 = await listen<DaemonEvent>("ph-event", ({ payload }) => {
    if (payload.kind === "output") {
      const bytes = b64ToBytes(payload.data);
      outputListeners.get(payload.id)?.forEach((fn) => fn(bytes, payload.offset));
    } else if (payload.kind === "resync") {
      resync();
    }
    handlers.onEvent(payload);
  });
  let wasConnected = false;
  const un2 = await listen<{ connected: boolean; error?: string }>("ph-connection", ({ payload }) => {
    handlers.onConnection(payload.connected, payload.error);
    // Back after a disconnect (maybe a daemon restart): everything may have changed.
    if (payload.connected && !wasConnected) resync();
    wasConnected = payload.connected;
  });
  return () => {
    un1();
    un2();
  };
}

// ---- helpers ----

export function bytesToB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

export function b64ToBytes(b64: string): Uint8Array {
  const s = atob(b64);
  const bytes = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i);
  return bytes;
}

export function fileToB64(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",")[1] ?? "");
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

export const repoName = (repo: string) => repo.split("/").filter(Boolean).pop() ?? repo;

export const STATUS_LABEL: Record<TaskStatus, string> = {
  queued: "Queued",
  running: "Running",
  needs_input: "Needs you",
  review: "Ready for review",
  stopped: "Stopped",
  done: "Done",
  failed: "Failed",
};

/** Sidebar groups, in display order. */
export const GROUPS: { label: string; statuses: TaskStatus[] }[] = [
  { label: "Needs you", statuses: ["needs_input"] },
  { label: "Running", statuses: ["running", "queued"] },
  { label: "Ready for review", statuses: ["review"] },
  { label: "Stopped", statuses: ["stopped", "failed"] },
  { label: "Done", statuses: ["done"] },
];

export function ago(unixSeconds: number): string {
  const s = Math.max(0, Date.now() / 1000 - unixSeconds);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
