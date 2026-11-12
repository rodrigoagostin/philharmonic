import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { api, b64ToBytes, onOutput } from "../api";

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

    // Subscribe first, buffer until the history is written, so nothing is lost or doubled.
    let ready = false;
    const pending: Uint8Array[] = [];
    const off = onOutput(id, (bytes) => (ready ? term.write(bytes) : pending.push(bytes)));
    let alive = true;
    api
      .screen(id)
      .then((r) => {
        if (!alive) return;
        term.write(b64ToBytes(r.history));
      })
      .catch(() => {})
      .finally(() => {
        if (!alive) return;
        pending.forEach((b) => term.write(b));
        pending.length = 0;
        ready = true;
      });

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
