import { useEffect, useState } from "react";
import { api, onOutput, onResync, type FileChange } from "./api";

/** The task's visible terminal text, refreshed (at most every `everyMs`) when output arrives. */
export function useScreenText(id: number, everyMs = 1000): string {
  const [text, setText] = useState("");
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      timer = undefined;
      // Only the text: the full history can be megabytes, and this runs every second.
      api
        .screenText(id)
        .then((t) => alive && setText(t))
        .catch(() => {});
    };
    load();
    const schedule = () => {
      if (!timer) timer = setTimeout(load, everyMs);
    };
    const off = onOutput(id, schedule);
    const offResync = onResync(schedule);
    return () => {
      alive = false;
      off();
      offResync();
      if (timer) clearTimeout(timer);
    };
  }, [id, everyMs]);
  return text;
}

/** Changed files in the task's worktree, refreshed while the agent writes output. */
export function useChangedFiles(id: number): FileChange[] {
  const [files, setFiles] = useState<FileChange[]>([]);
  useEffect(() => {
    // Never show another task's files while this one loads.
    setFiles([]);
    let alive = true;
    // Per task (not shared across tasks), so the new task's first load is never skipped
    // because the old task's request is still running.
    let busy = false;
    const load = () => {
      if (busy) return;
      busy = true;
      api
        .diff(id)
        .then((r) => alive && setFiles(r.files))
        .catch(() => {})
        .finally(() => (busy = false));
    };
    load();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const off = onOutput(id, () => {
      if (!timer)
        timer = setTimeout(() => {
          timer = undefined;
          load();
        }, 3000);
    });
    return () => {
      alive = false;
      off();
      if (timer) clearTimeout(timer);
    };
  }, [id]);
  return files;
}
