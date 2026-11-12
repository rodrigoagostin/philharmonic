import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { api, listenToDaemon, type ContextItem, type Task, type TaskStatus } from "./api";

export type View =
  | { name: "stage" }
  | { name: "focus"; id: number }
  | { name: "review"; id: number }
  | { name: "context"; taskId?: number };

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

interface Store {
  tasks: Task[];
  task: (id: number) => Task | undefined;
  context: ContextItem[];
  connected: boolean;
  connectionError?: string;
  view: View;
  go: (view: View) => void;
  newTaskOpen: boolean;
  setNewTaskOpen: (open: boolean) => void;
  repoFilter: string | null;
  setRepoFilter: (repo: string | null) => void;
  search: string;
  setSearch: (s: string) => void;
  toasts: Toast[];
  toast: (text: string, kind?: Toast["kind"]) => void;
  /** Run an async action and show its error as a toast. */
  run: <T>(action: () => Promise<T>) => Promise<T | undefined>;
}

const Ctx = createContext<Store | null>(null);

export function useStore(): Store {
  const s = useContext(Ctx);
  if (!s) throw new Error("useStore outside StoreProvider");
  return s;
}

const ATTENTION: TaskStatus[] = ["needs_input", "review", "failed"];

export function StoreProvider({ children }: { children: ReactNode }) {
  const [tasks, setTasks] = useState<Map<number, Task>>(new Map());
  const [context, setContext] = useState<ContextItem[]>([]);
  const [connected, setConnected] = useState(false);
  const [connectionError, setConnectionError] = useState<string>();
  const [view, setView] = useState<View>({ name: "stage" });
  const [newTaskOpen, setNewTaskOpen] = useState(false);
  const [repoFilter, setRepoFilter] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);
  const notifyOk = useRef(false);
  // Last known status per task, to notify on changes. Kept outside React state so the
  // check happens once per event (state updaters can run twice in StrictMode).
  const statuses = useRef(new Map<number, TaskStatus>());

  const toast = useCallback((text: string, kind: Toast["kind"] = "info") => {
    const id = ++toastId.current;
    setToasts((t) => [...t, { id, text, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 8000 : 4000);
  }, []);

  const run = useCallback(
    async <T,>(action: () => Promise<T>) => {
      try {
        return await action();
      } catch (e) {
        toast(e instanceof Error ? e.message : String(e), "error");
        return undefined;
      }
    },
    [toast],
  );

  const reload = useCallback(async () => {
    try {
      const [list, items] = await Promise.all([api.listTasks(), api.listContext()]);
      statuses.current = new Map(list.map((t) => [t.id, t.status]));
      setTasks(new Map(list.map((t) => [t.id, t])));
      setContext(items);
    } catch {
      /* not connected yet; the connection event will retry */
    }
  }, []);

  useEffect(() => {
    isPermissionGranted()
      .then((ok) => (ok ? ok : requestPermission().then((p) => p === "granted")))
      .then((ok) => (notifyOk.current = ok))
      .catch(() => {});

    let unlisten: (() => void) | undefined;
    listenToDaemon({
      onConnection: (ok, error) => {
        setConnected(ok);
        setConnectionError(error);
        if (ok) reload();
      },
      onEvent: (e) => {
        if (e.kind === "task_updated") {
          const before = statuses.current.get(e.task.id);
          statuses.current.set(e.task.id, e.task.status);
          if (before && before !== e.task.status && ATTENTION.includes(e.task.status)) notify(e.task);
          setTasks((prev) => {
            const next = new Map(prev);
            next.set(e.task.id, e.task);
            return next;
          });
        } else if (e.kind === "task_removed") {
          statuses.current.delete(e.id);
          setTasks((prev) => {
            const next = new Map(prev);
            next.delete(e.id);
            return next;
          });
        } else if (e.kind === "context_changed") {
          api.listContext().then(setContext).catch(() => {});
        } else if (e.kind === "resync") {
          reload(); // we missed events
        }
      },
    }).then((u) => (unlisten = u));
    reload();
    return () => unlisten?.();
  }, [reload]);

  function notify(task: Task) {
    if (!notifyOk.current || document.hasFocus()) return;
    const what = task.status === "needs_input" ? "needs you" : task.status === "review" ? "is ready for review" : "failed";
    sendNotification({ title: `Task ${task.id} ${what}`, body: task.title });
  }

  const sorted = useMemo(() => [...tasks.values()].sort((a, b) => a.id - b.id), [tasks]);

  const store: Store = {
    tasks: sorted,
    task: (id) => tasks.get(id),
    context,
    connected,
    connectionError,
    view,
    go: setView,
    newTaskOpen,
    setNewTaskOpen,
    repoFilter,
    setRepoFilter,
    search,
    setSearch,
    toasts,
    toast,
    run,
  };
  return <Ctx.Provider value={store}>{children}</Ctx.Provider>;
}

// ---- recent repos (per-device convenience) ----

const RECENT_KEY = "ph.recentRepos";

export function recentRepos(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
  } catch {
    return [];
  }
}

export function rememberRepo(repo: string) {
  try {
    const list = [repo, ...recentRepos().filter((r) => r !== repo)].slice(0, 10);
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch {
    /* storage unavailable: fine */
  }
}
