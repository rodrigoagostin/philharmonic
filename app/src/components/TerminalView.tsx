import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { api, b64ToBytes, onOutput, onResync } from "../api";

/** The agent's real terminal: what you type goes straight to its PTY. */
export function TerminalView({ id, live }: { id: number; live: boolean }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current!;
    const term = new Terminal({
      fontFamily: '"JetBrains Mono", "SF Mono", Menlo, monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
      theme: { background: "#18181b", foreground: "#e4e4e7" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    // Dead keys (' ` ~ ^ " on Portuguese / US-International layouts): leave them to the
    // OS, which composes them with the next key (' + a = á, ' + space = '). If xterm also
    // handles the key press itself, WebKit gives you the character twice.
    term.attachCustomKeyEventHandler((e) => !(e.key === "Dead" || (e.type === "keydown" && e.isComposing)));

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

    function replay() {
      ready = false;
      pending = [];
      api
        .screen(id)
        .then((r) => {
          if (!alive) return;
          term.reset();
          term.write(b64ToBytes(r.history));
          pos = r.end;
        })
        .catch(() => {})
        .finally(() => {
          if (!alive) return;
          ready = true;
          const queued = pending;
          pending = [];
          queued.forEach((c) => writeChunk(c.bytes, c.offset));
        });
    }

    // Subscribe first, then load the history, buffering what arrives in between.
    const off = onOutput(id, (bytes, offset) => (ready ? writeChunk(bytes, offset) : pending.push({ bytes, offset })));
    const offResync = onResync(replay);
    replay();

    const enc = new TextEncoder();
    const d1 = term.onData((data) => api.input(id, enc.encode(data)).catch(() => {}));
    const d2 = term.onBinary((data) => {
      const bytes = new Uint8Array(data.length);
      for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i) & 0xff;
      api.input(id, bytes).catch(() => {});
    });

    let last = "";
    const ro = new ResizeObserver(() => {
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
    });
    ro.observe(el);
    term.focus();

    return () => {
      alive = false;
      off();
      offResync();
      ro.disconnect();
      d1.dispose();
      d2.dispose();
      term.dispose();
    };
  }, [id]);

  return (
    <div className="terminal-wrap">
      <div className="terminal" ref={ref} />
      {!live && <div className="terminal-overlay">The agent isn't running. Resume it to continue.</div>}
    </div>
  );
}
