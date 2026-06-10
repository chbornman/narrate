/**
 * The keyboard map — single source of truth is spec/UI.md §11; this module
 * is its executable form. Anything not produced here does not exist.
 *
 * M1 boundaries: `J` (journal panel) and `B`/`E`/`O` (pencil) belong to
 * later packets; `M` exists only once ASR is ready (never in M1). They
 * dispatch to nothing here — the rows stay in the spec, the actions arrive
 * with their packets.
 *
 * Single-letter shortcuts are suppressed while any text input is focused
 * (§11). Search-overlay navigation keys (arrows/Enter/Escape) still work
 * with the search input focused — they never insert text.
 */

export interface KeyInput {
  key: string;
  ctrlOrMeta: boolean;
  shift: boolean;
}

export interface KeyContext {
  surface: "grid" | "look";
  searchOpen: boolean;
  /** Any text input focused (note input, search input, settings fields). */
  inputFocused: boolean;
  /** The focused input is the search input (overlay nav keys allowed). */
  searchInputFocused: boolean;
  hasSelection: boolean;
  railOpen: boolean;
  /** Compile-time debug builds only. */
  debugEnabled: boolean;
  /** ASR ready (never true in M1). */
  asrReady: boolean;
}

export type Action =
  | { kind: "escape" }
  | { kind: "open-search" }
  | { kind: "summon-note" }
  | { kind: "open-settings" }
  | { kind: "quit" }
  | { kind: "toggle-debug-panel" }
  | { kind: "rate"; value: number }
  | { kind: "open-look" } // Enter on focused thumb / search result
  | { kind: "toggle-rail" }
  | { kind: "rail-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "rail-enter" }
  | { kind: "focus-move"; dir: "up" | "down" | "left" | "right"; extend: boolean }
  | { kind: "toggle-select-focused" }
  | { kind: "select-all" }
  | { kind: "open-sort-menu" }
  | { kind: "thumb-size"; delta: 1 | -1 }
  | { kind: "look-nav"; delta: 1 | -1 }
  | { kind: "zoom-toggle" }
  | { kind: "zoom-step"; delta: 1 | -1 }
  | { kind: "zoom-fit" }
  | { kind: "toggle-filmstrip" }
  | { kind: "search-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "search-open-result" }
  | { kind: "remove-last-chip" };

export function dispatch(e: KeyInput, ctx: KeyContext): Action | null {
  const k = e.key;

  // ---- always-on chords -----------------------------------------------
  if (e.ctrlOrMeta && (k === "q" || k === "Q")) return { kind: "quit" };
  if (e.ctrlOrMeta && k === ",") return { kind: "open-settings" };
  if (e.ctrlOrMeta && (k === "f" || k === "F")) return { kind: "open-search" };
  if (k === "F12") return ctx.debugEnabled ? { kind: "toggle-debug-panel" } : null;
  if (k === "Escape") return { kind: "escape" };

  // ---- search overlay: navigation works with the input focused ---------
  if (ctx.searchOpen) {
    if (k === "Enter") return { kind: "search-open-result" };
    if (k === "ArrowUp") return { kind: "search-nav", dir: "up" };
    if (k === "ArrowDown") return { kind: "search-nav", dir: "down" };
    if (k === "ArrowLeft" && !ctx.searchInputFocused)
      return { kind: "search-nav", dir: "left" };
    if (k === "ArrowRight" && !ctx.searchInputFocused)
      return { kind: "search-nav", dir: "right" };
    if (k === "Backspace" && ctx.searchInputFocused)
      return { kind: "remove-last-chip" }; // only on empty input — caller gates
    if (ctx.inputFocused) return null;
    if (/^[0-5]$/.test(k) && ctx.hasSelection)
      return { kind: "rate", value: Number(k) };
    return null;
  }

  // ---- suppression rule (§11) ------------------------------------------
  if (ctx.inputFocused) {
    // Note input owns Enter/Shift+Enter itself; nothing global fires.
    return null;
  }

  // ---- global single keys ----------------------------------------------
  if (k === "/") return { kind: "open-search" };
  if (k === "n" || k === "N") return { kind: "summon-note" };
  if ((k === "m" || k === "M") && ctx.asrReady) return null; // M2b packet
  if (e.ctrlOrMeta && k === "0" && ctx.surface === "look") return { kind: "zoom-fit" };
  if (/^[0-5]$/.test(k) && !e.ctrlOrMeta) {
    // Grid needs a selection; Look always rates the viewed image (CAPTURE §10).
    if (ctx.surface === "look" || ctx.hasSelection)
      return { kind: "rate", value: Number(k) };
    return null; // session scope: rating keys do nothing
  }

  // ---- grid -------------------------------------------------------------
  if (ctx.surface === "grid") {
    if (k === "Tab") return { kind: "toggle-rail" };
    if (k === "Enter") return ctx.railOpen ? { kind: "rail-enter" } : { kind: "open-look" };
    if (k === "s" || k === "S") return { kind: "open-sort-menu" };
    if (k === "-") return { kind: "thumb-size", delta: -1 };
    if (k === "=") return { kind: "thumb-size", delta: 1 };
    if (k === " ") return { kind: "toggle-select-focused" };
    if (e.ctrlOrMeta && (k === "a" || k === "A")) return { kind: "select-all" };
    const dir =
      k === "ArrowUp"
        ? ("up" as const)
        : k === "ArrowDown"
          ? ("down" as const)
          : k === "ArrowLeft"
            ? ("left" as const)
            : k === "ArrowRight"
              ? ("right" as const)
              : null;
    if (dir) {
      if (ctx.railOpen) return { kind: "rail-nav", dir };
      return { kind: "focus-move", dir, extend: e.shift };
    }
    return null;
  }

  // ---- look ---------------------------------------------------------------
  if (k === "ArrowLeft") return { kind: "look-nav", delta: -1 };
  if (k === "ArrowRight") return { kind: "look-nav", delta: 1 };
  if (k === "z" || k === "Z") return { kind: "zoom-toggle" };
  if (k === "+" || (k === "=" && e.shift)) return { kind: "zoom-step", delta: 1 };
  if (k === "-") return { kind: "zoom-step", delta: -1 };
  if (k === "f" || k === "F") return { kind: "toggle-filmstrip" };
  // B / E / O / Cmd+Z: grease pencil — M2a packet (P5.1).
  return null;
}
