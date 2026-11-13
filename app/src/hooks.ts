import { useEffect, useRef, useState } from "react";
import { api, onOutput, type FileChange } from "./api";

/**
 * The task's visible terminal text (a few KB), re-read every `everyMs` while the agent is live.
 * Polling a small snapshot costs the same however much the agent prints; following its raw
 * output would mean receiving every byte of every agent on the Stage.
 */
export function useScreenText(id: number, live: boolean, everyMs = 1500): string {
  const [text, setText] = useState("");
  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .screenText(id)
        .then((t) => alive && setText((old) => (old === t ? old : t)))
        .catch(() => {});
    load();
    const timer = live ? setInterval(load, everyMs) : undefined;
    return () => {
      alive = false;
      if (timer) clearInterval(timer);
    };
  }, [id, live, everyMs]);
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
        .changedFiles(id)
        .then((f) => alive && setFiles(f))
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
        }, 5000);
    });
    return () => {
      alive = false;
      off();
      if (timer) clearTimeout(timer);
    };
  }, [id]);
  return files;
}

/**
 * Attachments of a message you're writing. Files you paste or drop are saved to the shelf
 * right away (so the agent can open them by path); this remembers which ones *this* draft
 * uploaded, so removing one from the draft deletes it instead of leaving it on the shelf.
 * Items you picked from the shelf only go back to it.
 */
export interface Attachments {
  ids: number[];
  /** Attach an item that's already on the shelf. */
  pick: (id: number) => void;
  /** Attach an item this draft just saved. */
  uploaded: (id: number) => void;
  /** Take it off the draft (and delete it, if this draft saved it). */
  remove: (id: number) => void;
  /** The message was sent: keep everything, start a fresh draft. */
  sent: () => void;
  /** Throw the draft away: delete what it saved. */
  discard: () => void;
}

/** Drafts survive switching tasks: text and attachments, per key. */
const drafts = new Map<string, { text: string; ids: number[]; mine: number[] }>();

export function useDraft(key: string): [string, (t: string) => void, Attachments] {
  const saved = drafts.get(key);
  const [text, setText] = useState(saved?.text ?? "");
  const [ids, setIds] = useState<number[]>(saved?.ids ?? []);
  const mine = useRef(new Set<number>(saved?.mine ?? []));
  useEffect(() => {
    drafts.set(key, { text, ids, mine: [...mine.current] });
  }, [key, text, ids]);

  const drop = (id: number) => api.removeContext(id).catch(() => {});
  const attachments: Attachments = {
    ids,
    pick: (id) => setIds((a) => (a.includes(id) ? a : [...a, id])),
    uploaded: (id) => {
      mine.current.add(id);
      setIds((a) => [...a, id]);
    },
    remove: (id) => {
      setIds((a) => a.filter((x) => x !== id));
      if (mine.current.delete(id)) drop(id);
    },
    sent: () => {
      mine.current.clear();
      setIds([]);
      setText("");
    },
    discard: () => {
      mine.current.forEach(drop);
      mine.current.clear();
      setIds([]);
      setText("");
      drafts.delete(key);
    },
  };
  return [text, setText, attachments];
}
