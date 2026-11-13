import type { Terminal } from "@xterm/xterm";

// macOS composes a dead key (' " ` ~ ^ on Brazilian / US-International layouts) with the
// next key: ' + s = 's, ' + a = á, ' + space = '. In WebKit, typing ' then s gives (logged
// in the app):
//
//   composition…  compositionend "'"          the accent, committed
//   keydown/keypress key="'s"                 a leftover press of the same key: xterm sent "'" again
//   input insertText "s"                      the letter: xterm dropped it (it thought it had
//                                             handled that key already)
//
// so `let's` came out as `let''`. Here, composition never reaches xterm (we send the
// committed text), the leftover press is ignored, and the text that follows it is sent by us.

/** Named keys (Enter, Tab, ArrowUp, F1, Backspace…), as opposed to keys that type text. */
const NAMED = /^[A-Z][A-Za-z0-9]+$/;
/** The leftover press comes a few ms after the commit; a real keystroke can't be that fast. */
const LEFTOVER_MS = 30;

export interface Accents {
  /** For xterm's key handler: true = xterm must not handle this event. */
  blocks: (e: KeyboardEvent) => boolean;
  dispose: () => void;
}

/**
 * Take dead keys / accents away from xterm and `send` what they type, once. `el` is the
 * element xterm was opened in: listening there in the capture phase runs before xterm's own
 * listeners on its textarea.
 */
export function routeComposition(term: Terminal, el: HTMLElement, send: (text: string) => void): Accents {
  const ta = term.textarea!;
  let composing = false;
  let committed = { text: "", at: -Infinity };
  // The leftover press was ignored: the text it types arrives as an input event, for us.
  let ownNextInsert = false;

  const onComposition = (e: CompositionEvent) => {
    if (e.target !== ta) return;
    e.stopPropagation();
    if (e.type === "compositionstart") composing = true;
    if (e.type !== "compositionend") return;
    composing = false;
    committed = { text: e.data ?? "", at: performance.now() };
    if (e.data) send(e.data);
    setTimeout(() => (ta.value = ""), 0);
  };

  const onInput = (e: Event) => {
    if (e.target !== ta) return;
    const ev = e as InputEvent;
    if (composing || ev.isComposing || ev.inputType.includes("Composition")) {
      e.stopPropagation();
      return;
    }
    if (ownNextInsert && ev.inputType === "insertText") {
      ownNextInsert = false;
      e.stopPropagation();
      if (ev.data) send(ev.data);
      setTimeout(() => (ta.value = ""), 0);
    }
  };

  const blocks = (e: KeyboardEvent) => {
    // Part of a composition still in progress: the OS handles it.
    if (e.key === "Dead" || e.keyCode === 229 || (e.isComposing && !NAMED.test(e.key))) return true;
    // The leftover press right after a commit (Enter, arrows… still work normally).
    if ((e.type === "keydown" || e.type === "keypress") && !NAMED.test(e.key)) {
      if (performance.now() - committed.at < LEFTOVER_MS) {
        if (e.type === "keydown") ownNextInsert = true;
        return true;
      }
    }
    if (e.type === "keydown") ownNextInsert = false;
    return false;
  };

  const types = ["compositionstart", "compositionupdate", "compositionend"] as const;
  types.forEach((t) => el.addEventListener(t, onComposition, true));
  el.addEventListener("input", onInput, true);
  return {
    blocks,
    dispose: () => {
      types.forEach((t) => el.removeEventListener(t, onComposition, true));
      el.removeEventListener("input", onInput, true);
    },
  };
}
