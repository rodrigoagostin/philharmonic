import { useEffect } from "react";
import { useStore } from "./store";
import { TopBar } from "./components/TopBar";
import { Sidebar } from "./components/Sidebar";
import { Stage } from "./components/Stage";
import { Focus } from "./components/Focus";
import { Review } from "./components/Review";
import { ContextShelf } from "./components/ContextShelf";
import { NewTaskModal } from "./components/NewTaskModal";
import { StatusBar } from "./components/StatusBar";
import { Toasts } from "./components/Toasts";

export function App() {
  const s = useStore();

  // Global shortcuts. Typing inside the terminal is left alone, except ⌘-combos.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const inTerminal = (document.activeElement as HTMLElement | null)?.closest(".xterm");
      if (e.metaKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        s.setNewTaskOpen(true);
      } else if (e.metaKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        s.go({ name: "stage" });
        setTimeout(() => document.getElementById("search")?.focus(), 0);
      } else if (e.metaKey && /^[1-9]$/.test(e.key)) {
        const live = s.tasks.filter((t) => t.status !== "done");
        const task = live[Number(e.key) - 1];
        if (task) {
          e.preventDefault();
          s.go({ name: "focus", id: task.id });
        }
      } else if (e.metaKey && e.key === "0") {
        e.preventDefault();
        s.go({ name: "stage" });
      } else if (e.key === "Escape" && !inTerminal && !s.newTaskOpen && s.view.name !== "stage") {
        s.go({ name: "stage" });
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [s]);

  // A view for a task that no longer exists falls back to the Stage.
  const viewTaskId = s.view.name === "focus" || s.view.name === "review" ? s.view.id : undefined;
  const missing = viewTaskId !== undefined && !s.task(viewTaskId) && s.connected;
  useEffect(() => {
    if (missing) s.go({ name: "stage" });
  }, [missing, s]);

  return (
    <div className="app">
      <TopBar />
      <div className="body">
        <Sidebar />
        <main className="main">
          {s.view.name === "stage" && <Stage />}
          {s.view.name === "focus" && s.task(s.view.id) && <Focus task={s.task(s.view.id)!} />}
          {s.view.name === "review" && s.task(s.view.id) && <Review task={s.task(s.view.id)!} />}
          {s.view.name === "context" && <ContextShelf taskId={s.view.taskId} />}
        </main>
      </div>
      <StatusBar />
      {s.newTaskOpen && <NewTaskModal />}
      <Toasts />
    </div>
  );
}
