import { useEffect, useRef, useState } from "react";
import { api, onOutput, type FileChange } from "./api";

/** The task's visible terminal text, refreshed (at most every `everyMs`) when output arrives. */
export function useScreenText(id: number, everyMs = 1000): string {
  const [text, setText] = useState("");
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      timer = undefined;
      api
        .screen(id)
        .then((r) => alive && setText(r.text))
        .catch(() => {});
    };
    load();
    const off = onOutput(id, () => {
      if (!timer) timer = setTimeout(load, everyMs);
    });
    return () => {
      alive = false;
      off();
      if (timer) clearTimeout(timer);
    };
  }, [id, everyMs]);
  return text;
}

/** Changed files in the task's worktree, refreshed while the agent writes output. */
export function useChangedFiles(id: number): FileChange[] {
  const [files, setFiles] = useState<FileChange[]>([]);
  const busy = useRef(false);
  useEffect(() => {
    let alive = true;
    const load = () => {
      if (busy.current) return;
      busy.current = true;
      api
        .diff(id)
        .then((r) => alive && setFiles(r.files))
        .catch(() => {})
        .finally(() => (busy.current = false));
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
