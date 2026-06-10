/**
 * The typed action system contracts (UI-ARCHITECTURE §3) — FROZEN BY
 * FOUNDATIONS. One registry of ActionDefs is the single source of truth
 * for verbs; the keyboard map (logic/keymap.ts), the four context-menu
 * seats (actions/menus.ts), the cheatsheet (actions/cheatsheet.ts), and
 * tooltips (primitives/tooltip.ts) are renderings of it.
 *
 * Enablement predicates live ONLY here (available/enabled on defs) —
 * component-level key/enable checks are forbidden (§8 guardrails).
 */
import type { Action } from "../logic/keymap";
import type { SortMode } from "../logic/sort";
import type { SurroundLevel } from "../theme/surround";

/**
 * One key binding. `ctrlOrMeta` undefined = the modifier must NOT be held;
 * `shift` undefined = don't care (shifted glyphs like "?" arrive
 * pre-resolved in KeyboardEvent.key). `arg` is an optional payload handed
 * to `toAction` when this chord matches (parametrized rows: rate 0–5,
 * arrows with a direction).
 */
export interface KeyChord {
  key: string;
  ctrlOrMeta?: boolean;
  shift?: boolean;
  arg?: unknown;
}

/** The four context-menu seats + the reserved M2a toolbar seat. */
export type MenuSeat = "thumb" | "gutter" | "rail-folder" | "look-backdrop" | "look-toolbar";

export interface MenuOption {
  arg: unknown;
  label: string;
  checked?: boolean;
}

export interface ActionContext {
  surface: "grid" | "look";
  searchOpen: boolean;
  /** Any text input focused (note input, search input, inline correction). */
  inputFocused: boolean;
  /** The focused input is the search input (overlay nav keys allowed). */
  searchInputFocused: boolean;
  /** Search query is empty (gates chip removal on Backspace, UI §11). */
  queryEmpty?: boolean;
  hasSelection: boolean;
  selectionCount: number;
  /** The ACTIVE (focused) image — Look, inspector, and R act on it (§1). */
  activeHash: string | null;
  activeIsPair: boolean;
  activePairCollapsed: boolean;
  railOpen: boolean;
  /** Rail keyboard focus — arrow routing gates on this, not railOpen. */
  railFocused: boolean;
  inspectorOpen: false | "metadata" | "journal";
  cheatsheetOpen: boolean;
  contextMenuOpen: boolean;
  chromeHidden: boolean;
  autoAdvance: boolean;
  /** Look is at fit zoom (Space close vs hold-to-pan gate). */
  lookAtFit: boolean;
  /** Compile-time debug builds only. */
  debugEnabled: boolean;
  /** ASR ready (never true in P4.2). */
  asrReady: boolean;
  // radio state for menus
  sort: SortMode;
  thumbStep: number;
  surround: SurroundLevel;
  filmstrip: boolean;
  // reserved (always falsy in P4.2; M2a/M2b light them up)
  pencilMode: boolean;
  micArmed: boolean;
}

export interface ActionDef {
  /** Ties the registry to the Action union. UNIQUE per (id, scope). */
  id: Action["kind"];
  /** Menu text. */
  verb: string;
  /** Cheatsheet long form (verb used when absent). */
  label?: string;
  /** [] = pointer-only (explicit allowlist in registry.test.ts). */
  keys: KeyChord[];
  scope: "global" | "grid" | "look" | "search" | "inspector";
  seats?: MenuSeat[];
  group: "contract" | "grid" | "look" | "panels" | "capture" | "search" | "system";
  /** Exists in this context (menu visibility). */
  available: (ctx: ActionContext) => boolean;
  /** Runnable now (graying, key gating). */
  enabled?: (ctx: ActionContext) => boolean;
  /** Toggle rows: current ON state, rendered as a menu check (display
   * only — like available/enabled, state predicates live ONLY on defs). */
  checked?: (ctx: ActionContext) => boolean;
  /** Exempt from the §11 single-letter suppression (chord-level non-typing
   * keys only — rule in match.ts). */
  worksInInput?: boolean;
  /** Parametrized rows. Returning null declines the match (e.g. a chord
   * that types in the current input). */
  toAction?: (ctx: ActionContext, arg?: unknown) => Action | null;
  /** Radio options for seat rendering (Rate ▸ / Sort ▸ / Size ▸ / Surround ▸). */
  options?: (ctx: ActionContext) => MenuOption[];
  /** Reserved seat (P/E/V, M, overlay-cycle): dispatches to nothing,
   * hidden from menus + cheatsheet until its packet. */
  reserved?: true;
}

/** "Modes are visible" (featureset §0) — by construction: every sticky
 * state is a ModeDef whose segment renders in the indicator. */
export interface ModeDef {
  id: "auto-advance" | "pencil" | "mic"; // M2a/M2b ids reserved now
  isOn(ctx: ActionContext): boolean;
  segment(ctx: ActionContext): { text: string; title: string } | null;
}
