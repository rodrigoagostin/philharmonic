import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { api, listenToDaemon, type Activity, type Collection, type ContextItem, type Task, type TaskStatus } from "./api";

export type View =
  | { name: "overview" }
  | { name: "stage" }
  | { name: "focus"; id: number }
  | { name: "review"; id: number }
  /** The Context screen: a task's own items, or a collection. */
  | { name: "context"; taskId?: number; collectionId?: number };

/** Which tasks the sidebar, Overview and Stage show. */
export type Filter = { kind: "repo"; repo: string } | { kind: "collection"; id: number } | null;

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

interface Store {
  tasks: Task[];
  task: (id: number) => Task | undefined;
  context: ContextItem[];
  collections: Collection[];
  collection: (id: number) => Collection | undefined;
  /** Latest activity of every agent, oldest first. */
  activity: Activity[];
  connected: boolean;
  connectionError?: string;
  /** The running daemon is an older build than the installed one. */
  daemonOutdated: boolean;
  /** Restart the daemon on the new build; running agents are stopped and resumed. */
  restartDaemon: () => Promise<void>;
  restarting: boolean;
  view: View;
  go: (view: View) => void;
  newTaskOpen: boolean;
  setNewTaskOpen: (open: boolean) => void;
  /** Follows the open task (its project, unless the filter you picked already includes it). */
  filter: Filter;
  setFilter: (f: Filter) => void;
  /** Does the filter show this task? */
  shown: (t: Task) => boolean;
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
  const [collections, setCollections] = useState<Collection[]>([]);
  const [activity, setActivity] = useState<Activity[]>([]);
  const [connected, setConnected] = useState(false);
  const [connectionError, setConnectionError] = useState<string>();
  const [daemonOutdated, setDaemonOutdated] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const tasksRef = useRef<Map<number, Task>>(new Map());
  const [view, setView] = useState<View>({ name: "overview" });
  const [newTaskOpen, setNewTaskOpen] = useState(false);
  // The project you picked yourself. While a task is open, the filter follows that task's
  // project instead; your own pick comes back when you leave the task.
  const [chosen, setChosen] = useState<Filter>(null);
  const [search, setSearch] = useState("");
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);
  const notifyOk = useRef(false);

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
      const [list, items, recent, cols] = await Promise.all([
        api.listTasks(),
        api.listContext(),
        api.activity(undefined, 500).catch(() => []),
        api.listCollections().catch(() => []),
      ]);
      setTasks(new Map(list.map((t) => [t.id, t])));
      setContext(items);
      setCollections(cols);
      setActivity(recent);
      setConnected(true); // a request worked, so we're connected (the event may have come before we listened)
      api.daemonOutdated().then(setDaemonOutdated).catch(() => {});
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
          setTasks((prev) => {
            const before = prev.get(e.task.id);
            if (before && before.status !== e.task.status && ATTENTION.includes(e.task.status)) {
              notify(e.task);
            }
            const next = new Map(prev);
            next.set(e.task.id, e.task);
            return next;
          });
        } else if (e.kind === "task_removed") {
          setTasks((prev) => {
            const next = new Map(prev);
            next.delete(e.id);
            return next;
          });
        } else if (e.kind === "activity") {
          setActivity((a) => (a.length >= 1000 ? [...a.slice(-800), e.item] : [...a, e.item]));
        } else if (e.kind === "context_changed") {
          api.listContext().then(setContext).catch(() => {});
          api.listCollections().then(setCollections).catch(() => {});
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
  tasksRef.current = tasks;

  const restartDaemon = useCallback(async () => {
    const live: TaskStatus[] = ["queued", "running", "needs_input", "review"];
    const resume = [...tasksRef.current.values()].filter((t) => live.includes(t.status)).map((t) => t.id);
    setRestarting(true);
    try {
      const failed = await api.restartDaemon(resume);
      setDaemonOutdated(false);
      if (failed.length) toast(`Restarted, but couldn't resume: ${failed.join("; ")}`, "error");
      else toast(resume.length ? `Service restarted · resumed ${resume.length} agent${resume.length === 1 ? "" : "s"}` : "Service restarted");
      await reload();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setRestarting(false);
    }
  }, [toast, reload]);

  const matches = (f: Filter, t: Task) =>
    !f || (f.kind === "repo" ? t.repo === f.repo : (t.collections ?? []).includes(f.id));
  const openTaskId = view.name === "focus" || view.name === "review" ? view.id : undefined;
  const openTask = openTaskId === undefined ? undefined : tasks.get(openTaskId);
  // With a task open, show its neighbours: keep your pick if the task is in it (e.g. a
  // feature's collection), otherwise switch to the task's project.
  const filter: Filter = openTask && !(chosen && matches(chosen, openTask)) ? { kind: "repo", repo: openTask.repo } : chosen;
  const setFilter = (f: Filter) => {
    setChosen(f);
    // Picking something the open task isn't part of: show those tasks instead.
    if (openTask && f && !matches(f, openTask)) setView({ name: "overview" });
  };

  const store: Store = {
    tasks: sorted,
    task: (id) => tasks.get(id),
    context,
    collections,
    collection: (id) => collections.find((c) => c.id === id),
    activity,
    connected,
    connectionError,
    daemonOutdated,
    restartDaemon,
    restarting,
    view,
    go: setView,
    newTaskOpen,
    setNewTaskOpen,
    filter,
    setFilter,
    shown: (t) => matches(filter, t),
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
