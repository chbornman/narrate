/**
 * Key-hinted tooltip as a Svelte attachment: `{@attach tooltip({ actionId })}`.
 * Copy is RESOLVED FROM THE REGISTRY so tooltips can never drift from the
 * keymap - the fourth rendering of the one table. It surfaces the def's
 * EXPLANATORY `label` (the one-line "what this does") when present, falling
 * back to its `verb`, plus the first key chord chip. The floating element is
 * created imperatively (styled by .pp-tooltip in app.css - token-only).
 *
 * Chrome buttons NOT tied to a registry row (e.g. the fuzzy / ranking-signals
 * toggles) pass a plain `text` instead of an `actionId`, so they get the SAME
 * delayed, token-styled tooltip and hover affordance rather than an ad-hoc
 * browser `title=`. Consumers: titlebar, GridHeader controls, rail rows,
 * station seats, inspector controls.
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
  /** The registry row this affordance dispatches. Omit for chrome buttons
   * with no action (then `text` carries the explanation). */
  actionId?: Action["kind"];
  /** Override the resolved copy. With an `actionId` it replaces the def's
   * label/verb (e.g. a seat that hints the Journal tab specifically);
   * without one it IS the tooltip (plain chrome buttons). */
  verb?: string;
  /** Plain explanatory copy for buttons not tied to a registry row. Alias of
   * `verb` for readability at call sites - both mean "the words to show". */
  text?: string;
  /** Parametrized defs: pick the chord whose arg matches (e.g. the
   * open-inspector def carries I->metadata and J->journal - a Journal
   * affordance hints J). Still resolved from the registry, never typed. */
  arg?: unknown;
}): Attachment {
  return (node) => {
    const el = node as HTMLElement;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let tip: HTMLDivElement | null = null;

    function show() {
      const def = opts.actionId === undefined ? undefined : defById(opts.actionId);
      // Prefer the registry's explanatory `label` (the one-line "what it
      // does"), then its `verb`; an explicit verb/text override wins over
      // both. This is the single source of truth - the words can never drift
      // from the keymap or the cheatsheet.
      const copy = opts.verb ?? opts.text ?? def?.label ?? def?.verb;
      if (copy === undefined || tip !== null) return;
      tip = document.createElement("div");
      tip.className = "pp-tooltip";
      const v = document.createElement("span");
      v.textContent = copy;
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
