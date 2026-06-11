/**
 * Key-hinted tooltip as a Svelte attachment: `{@attach tooltip({ actionId })}`.
 * Copy is RESOLVED FROM THE REGISTRY (verb + first chord) so tooltips can
 * never drift from the keymap — the fourth rendering of the one table.
 * The floating element is created imperatively (styled by .pp-tooltip in
 * app.css — token-only). Consumers: GridHeader controls, rail rows,
 * inspector controls.
 */
import type { Attachment } from "svelte/attachments";
import type { Action } from "../logic/keymap";
import { defById } from "../actions/registry";
import { isMac } from "../logic/platform";
import type { KeyChord } from "../actions/types";

const HOVER_DELAY_MS = 550;

function chordText(chord: KeyChord): string {
  const names: Record<string, string> = {
    " ": "Space",
    Escape: "Esc",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
  };
  const parts: string[] = [];
  if (chord.ctrlOrMeta === true) parts.push(isMac() ? "⌘" : "Ctrl");
  if (chord.shift === true) parts.push("Shift");
  parts.push(names[chord.key] ?? (chord.key.length === 1 ? chord.key.toUpperCase() : chord.key));
  return parts.join("+");
}

export function tooltip(opts: {
  actionId: Action["kind"];
  verb?: string;
  /** Parametrized defs: pick the chord whose arg matches (e.g. the
   * open-inspector def carries I→metadata and J→journal — a Journal
   * affordance hints J). Still resolved from the registry, never typed. */
  arg?: unknown;
}): Attachment {
  return (node) => {
    const el = node as HTMLElement;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let tip: HTMLDivElement | null = null;

    function show() {
      const def = defById(opts.actionId);
      const verb = opts.verb ?? def?.verb;
      if (verb === undefined || tip !== null) return;
      tip = document.createElement("div");
      tip.className = "pp-tooltip";
      const v = document.createElement("span");
      v.textContent = verb;
      tip.appendChild(v);
      const chord =
        opts.arg === undefined
          ? def?.keys[0]
          : def?.keys.find((k) => k.arg === opts.arg);
      if (chord !== undefined) {
        const k = document.createElement("span");
        k.className = "key";
        k.textContent = chordText(chord);
        tip.appendChild(k);
      }
      document.body.appendChild(tip);
      const r = el.getBoundingClientRect();
      const t = tip.getBoundingClientRect();
      tip.style.left = `${Math.max(4, Math.min(window.innerWidth - t.width - 4, r.left))}px`;
      tip.style.top = `${r.bottom + 6 + t.height > window.innerHeight ? r.top - t.height - 6 : r.bottom + 6}px`;
    }

    function hide() {
      clearTimeout(timer);
      timer = undefined;
      tip?.remove();
      tip = null;
    }

    function onEnter() {
      timer = setTimeout(show, HOVER_DELAY_MS);
    }

    el.addEventListener("pointerenter", onEnter);
    el.addEventListener("pointerleave", hide);
    el.addEventListener("pointerdown", hide);
    return () => {
      hide();
      el.removeEventListener("pointerenter", onEnter);
      el.removeEventListener("pointerleave", hide);
      el.removeEventListener("pointerdown", hide);
    };
  };
}
