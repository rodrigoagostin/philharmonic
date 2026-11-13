import { useEffect, useRef, type RefObject } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { api, b64ToBytes, onOutput, onResync } from "../api";
import { routeComposition } from "../deadKeys";

/** Lets the Focus view move around the terminal (jump to a prompt, previous/next prompt). */
export interface TermControl {
  /** Scroll to the prompt that starts with `text` (or the `index`-th prompt). False if it's not in the scrollback. */
  jumpToPrompt: (text: string, index: number) => boolean;
  /** The prompts visible in the scrollback, oldest first (first line of each). */
  prompts: () => string[];
  prevPrompt: () => void;
  nextPrompt: () => void;
  /** The program switched to the full-screen buffer (no scrollback to jump through). */
  isFullScreen: () => boolean;
  /** Called when the program enters or leaves full-screen mode. */
  onFullScreenChange?: (fullScreen: boolean) => void;
}

/** The first characters we compare: enough to tell prompts apart, short enough to fit one line. */
export const norm = (s: string) => s.replace(/\s+/g, " ").trim().slice(0, 40);

/**
 * Lines where Claude Code shows a prompt you sent: `❯ your text`. The input box at the bottom
 * also starts with `❯`, but it sits between two `───` rules, so it's skipped.
 */
function promptLines(term: Terminal): { y: number; text: string }[] {
  const buf = term.buffer.active;
  const line = (y: number) => buf.getLine(y)?.translateToString(true) ?? "";
  const found: { y: number; text: string }[] = [];
  for (let y = 0; y < buf.length; y++) {
    const text = line(y);
    if (!text.startsWith("❯ ") || !text.slice(2).trim()) continue;
    const isInputBox = line(y - 1).startsWith("─") && line(y + 1).startsWith("─");
    if (!isInputBox) found.push({ y, text: text.slice(2) });
  }
  return found;
}

/** The agent's real terminal: what you type goes straight to its PTY. */
export function TerminalView({
  id,
  live,
  onResume,
  control,
}: {
  id: number;
  live: boolean;
  onResume?: () => void;
  control?: RefObject<TermControl | null>;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current!;
    const term = new Terminal({
      fontFamily: '"JetBrains Mono", "SF Mono", Menlo, monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      theme: { background: "#18181b", foreground: "#e4e4e7" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    // Draw with the GPU: far less CPU and smooth scrolling. If WebGL isn't available or the
    // context is lost, xterm falls back to its (slower) DOM renderer.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* DOM renderer */
    }

    // Every output chunk carries its position in the task's output stream, and the history
    // says where it ends. `pos` is how far this terminal has written, so each byte is
    // written exactly once: chunks we already have are skipped, and a gap (missed events)
    // triggers a fresh replay.
    let alive = true;
    let ready = false;
    let pos = 0;
    let pending: { bytes: Uint8Array; offset: number }[] = [];

    const writeChunk = (bytes: Uint8Array, offset: number) => {
      const end = offset + bytes.length;
      if (end <= pos) return; // already written
      if (offset > pos) return replay(); // missed something
      term.write(offset < pos ? bytes.subarray(pos - offset) : bytes);
      pos = end;
    };

    // Replay at the size the output was drawn at, *then* fit to the window: the agent redraws
    // with relative cursor moves, which only land in the right place at the original size.
    // The resize afterwards makes it redraw cleanly at the new size.
    function replay() {
      ready = false;
      pending = [];
      const done = () => {
        if (!alive) return;
        ready = true;
        const queued = pending;
        pending = [];
        queued.forEach((c) => writeChunk(c.bytes, c.offset));
        fitAndResize();
      };
      api
        .screen(id)
        .then((r) => {
          if (!alive) return;
          term.reset();
          if (r.cols > 0 && r.rows > 0) term.resize(r.cols, r.rows);
          pos = r.end;
          term.write(b64ToBytes(r.history), done);
        })
        .catch(done);
    }

    // Subscribe first, then load the history, buffering what arrives in between.
    const off = onOutput(id, (bytes, offset) => (ready ? writeChunk(bytes, offset) : pending.push({ bytes, offset })));
    const offResync = onResync(replay);
    replay();

    // Typing/pasting can produce many tiny chunks; send them together (one request per ~10ms).
    // Plain mouse-movement reports (SGR button 35) are dropped: some programs ask for every
    // movement, which would mean a request per pixel the mouse moves.
    const enc = new TextEncoder();
    let queued = "";
    let flushTimer: ReturnType<typeof setTimeout> | undefined;
    const flush = () => {
      flushTimer = undefined;
      if (queued) api.input(id, enc.encode(queued)).catch(() => {});
      queued = "";
    };
    const d1 = term.onData((data) => {
      const filtered = data.replace(/\x1b\[<35;\d+;\d+M/g, "");
      if (!filtered) return;
      queued += filtered;
      if (!flushTimer) flushTimer = setTimeout(flush, 10);
    });
    // Accents / dead keys: the OS composes them, we send the result once (see deadKeys.ts).
    const accents = routeComposition(term, el, (text) => {
      queued += text;
      if (!flushTimer) flushTimer = setTimeout(flush, 10);
    });

    const d2 = term.onBinary((data) => {
      const bytes = new Uint8Array(data.length);
      for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i) & 0xff;
      api.input(id, bytes).catch(() => {});
    });

    // ---- jumping between prompts ----
    let clearSelection: ReturnType<typeof setTimeout> | undefined;
    const show = (y: number) => {
      term.scrollToLine(Math.max(0, y - 1)); // one line of context above
      term.selectLines(y, y); // highlight it for a moment
      if (clearSelection) clearTimeout(clearSelection);
      clearSelection = setTimeout(() => term.clearSelection(), 1500);
    };
    const step = (dir: 1 | -1) => {
      const top = term.buffer.active.viewportY + 1; // the line just under the context line
      const lines = promptLines(term).map((p) => p.y);
      const target = dir === 1 ? lines.find((y) => y > top) : lines.reverse().find((y) => y < top);
      if (target !== undefined) show(target);
      else if (dir === 1) term.scrollToBottom();
    };
    if (control) {
      control.current = {
        jumpToPrompt: (text, index) => {
          const lines = promptLines(term);
          const want = norm(text.split("\n").find((l) => l.trim()) ?? "");
          // Match by text; long or multi-line pastes are shown collapsed ("[Pasted text #1]"),
          // so fall back to position when every prompt is on screen.
          const byText = want ? lines.find((l) => norm(l.text).startsWith(want) || want.startsWith(norm(l.text))) : undefined;
          const target = byText ?? lines[index];
          if (!target) return false;
          show(target.y);
          return true;
        },
        prompts: () => promptLines(term).map((p) => p.text.trimEnd()),
        prevPrompt: () => step(-1),
        nextPrompt: () => step(1),
        isFullScreen: () => term.buffer.active.type === "alternate",
      };
    }
    const d3 = term.buffer.onBufferChange((b) => control?.current?.onFullScreenChange?.(b.type === "alternate"));
    // xterm takes ONE key handler (a second call replaces the first), so everything is here.
    term.attachCustomKeyEventHandler((e) => {
      // Keys that are part of a dead-key composition: the OS handles them (see deadKeys.ts).
      if (accents.blocks(e)) return false;
      // ⌘↑ / ⌘↓ inside the terminal: previous / next prompt (instead of sending them to the agent).
      if (e.type === "keydown" && e.metaKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
        step(e.key === "ArrowUp" ? -1 : 1);
        return false;
      }
      return true;
    });

    let last = "";
    function fitAndResize() {
      try {
        fit.fit();
      } catch {
        return;
      }
      const size = `${term.cols}x${term.rows}`;
      if (size !== last) {
        last = size;
        api.resize(id, term.cols, term.rows).catch(() => {});
      }
    }
    // Until the history is replayed, keep the original size (see above).
    const ro = new ResizeObserver(() => ready && fitAndResize());
    ro.observe(el);
    term.focus();

    return () => {
      alive = false;
      if (control) control.current = null;
      if (clearSelection) clearTimeout(clearSelection);
      if (flushTimer) clearTimeout(flushTimer);
      flush();
      off();
      offResync();
      ro.disconnect();
      accents.dispose();
      d1.dispose();
      d2.dispose();
      d3.dispose();
      term.dispose();
    };
  }, [id, control]);

  return (
    <div className="terminal-wrap">
      <div className="terminal" ref={ref} />
      {!live && (
        <div className="terminal-overlay">
          <div className="overlay-box">
            <p>The agent isn't running.</p>
            {onResume && (
              <button className="btn btn-primary" onClick={onResume}>
                Resume agent
              </button>
            )}
            <p className="small">It continues the same conversation, in the same worktree.</p>
          </div>
        </div>
      )}
    </div>
  );
}
