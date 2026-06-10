/**
 * The keyboard map — single source of truth is the typed action registry
 * (actions/registry.ts); this module is the INTERPRETER. `dispatch` keeps
 * its exact P3.2 signature and is now a thin wrapper over
 * actions/match.ts running against the registry; the executable key table
 * lives in actions/defs/*.
 *
 * P4.2 boundaries: pencil rows (P/E/V, overlay cycle) and the mic row (M)
 * are RESERVED registry rows — they dispatch to nothing here; the actions
 * arrive with their packets (M2a/M2b). Single-letter shortcuts are
 * suppressed while any text input is focused (UI §11; rule owned by
 * actions/match.ts).
 */
import { match } from "../actions/match";
import { REGISTRY } from "../actions/registry";
import type { ActionContext } from "../actions/types";
import type { SortMode } from "./sort";
import { DEFAULT_SORT, DEFAULT_THUMB_STEP } from "./sort";
import type { SurroundLevel } from "../theme/surround";

export interface KeyInput {
  key: string;
  ctrlOrMeta: boolean;
  shift: boolean;
}

/**
 * One Action type, one perform sink (state/app.svelte.ts), four renderings
 * of one table (keys, menus, cheatsheet, tooltips). The union is EXTENDED,
 * never narrowed — contract frozen by FOUNDATIONS.
 */
export type Action =
  // contract / global
  | { kind: "escape" }
  | { kind: "go-grid" }
  | { kind: "toggle-lights-out" }
  | { kind: "toggle-rail" }
  | { kind: "toggle-cheatsheet" }
  | { kind: "open-search" }
  | { kind: "summon-note" }
  | { kind: "toggle-auto-advance" }
  | { kind: "toggle-fullscreen" }
  | { kind: "open-settings" }
  | { kind: "quit" }
  | { kind: "toggle-debug-panel" }
  | { kind: "rate"; value: number }
  | { kind: "set-surround"; level: SurroundLevel }
  // grid
  | { kind: "open-look" }
  | { kind: "focus-move"; dir: "up" | "down" | "left" | "right"; extend: boolean }
  | { kind: "focus-edge"; edge: "home" | "end"; extend: boolean }
  | { kind: "focus-page"; dir: "up" | "down"; extend: boolean }
  | { kind: "toggle-select-focused" }
  | { kind: "select-all" }
  | { kind: "select-none" }
  | { kind: "open-sort-menu" }
  | { kind: "set-sort"; mode: SortMode }
  | { kind: "thumb-size"; delta: 1 | -1 }
  | { kind: "set-thumb-step"; step: number }
  | { kind: "cycle-cell-info" }
  | { kind: "stack-toggle-active" }
  | { kind: "stack-collapse-all" }
  | { kind: "stack-expand-all" }
  | { kind: "flip-stack-member" } // R: viewed member in Look, active in Grid (D1)
  // look
  | { kind: "look-nav"; delta: 1 | -1 }
  | { kind: "look-close" }
  | { kind: "zoom-toggle" }
  | { kind: "zoom-step"; delta: 1 | -1 }
  | { kind: "zoom-fit" }
  | { kind: "zoom-100" }
  | { kind: "toggle-filmstrip" }
  // panels
  | { kind: "rail-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "rail-enter" }
  | { kind: "rail-folder-open"; rootId: string; folder: string }
  | { kind: "rail-folder-reveal"; rootId: string; folder: string }
  | { kind: "rescan-root"; rootId: string }
  | { kind: "open-inspector"; tab: "metadata" | "journal" }
  | { kind: "close-inspector" }
  // journal row verbs (pointer-seated; Stage C wires the flows)
  | { kind: "journal-correct"; eventId: string }
  | { kind: "journal-retract"; eventId: string }
  | { kind: "journal-redact"; eventId: string }
  | { kind: "journal-toggle-retracted" }
  // OS integration (D4 — no deletion verbs, D3)
  | { kind: "reveal-in-file-manager" }
  | { kind: "copy-file-path" }
  | { kind: "open-with-default" }
  // search
  | { kind: "search-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "search-open-result" }
  | { kind: "remove-last-chip" }
  // reserved rows — dispatch to nothing in P4.2 (M2a/M2b packets)
  | { kind: "toggle-mic" }
  | { kind: "pencil-pen" }
  | { kind: "pencil-eraser" }
  | { kind: "pencil-visibility" }
  | { kind: "cycle-overlay" };

/** The P3.2 KeyContext fields — still required, so existing fixtures and
 * callers compile unchanged. */
type LegacyContextKeys =
  | "surface"
  | "searchOpen"
  | "inputFocused"
  | "searchInputFocused"
  | "hasSelection"
  | "railOpen"
  | "debugEnabled"
  | "asrReady";

/**
 * KeyContext = ActionContext with the P4.2 fields optional-with-defaults
 * (UI-ARCHITECTURE §3). New code should assemble a full ActionContext via
 * ui.actionContext(); this alias exists for signature stability.
 */
export type KeyContext = Pick<ActionContext, LegacyContextKeys> &
  Partial<Omit<ActionContext, LegacyContextKeys>>;

/** Defaults for the P4.2 ActionContext fields (reserved fields falsy). */
export const CONTEXT_DEFAULTS: Omit<ActionContext, LegacyContextKeys> = {
  selectionCount: 0,
  activeHash: null,
  activeIsPair: false,
  activePairCollapsed: false,
  railFocused: false,
  inspectorOpen: false,
  cheatsheetOpen: false,
  contextMenuOpen: false,
  chromeHidden: false,
  autoAdvance: false,
  lookAtFit: true,
  sort: DEFAULT_SORT,
  thumbStep: DEFAULT_THUMB_STEP,
  surround: "black",
  filmstrip: false,
  pencilMode: false,
  micArmed: false,
};

export function withDefaults(ctx: KeyContext): ActionContext {
  return { ...CONTEXT_DEFAULTS, ...ctx };
}

/** Signature preserved from P3.2; internally match(e, ctx, REGISTRY). */
export function dispatch(e: KeyInput, ctx: KeyContext): Action | null {
  return match(e, withDefaults(ctx), REGISTRY);
}
