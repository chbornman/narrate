/**
 * The keyboard map — single source of truth is the typed action registry
 * (actions/registry.ts); this module is the INTERPRETER. `dispatch` keeps
 * its exact P3.2 signature and is now a thin wrapper over
 * actions/match.ts running against the registry; the executable key table
 * lives in actions/defs/*.
 *
 * The pencil band went live with P5.1 (B sticky toggle, hold-E eraser,
 * O overlay, Ctrl+Z undo — UI §4.4/§11; the P4.2 reserved P/E/V rows were
 * reconciled onto the spec's keys). The mic is live since P6.4 and
 * two-gesture since the June 2026 ruling: tap toggles, hold is
 * push-to-talk (logic/michold.ts) — and since June 12 2026 it sits on
 * SPACE ("like a Zoom call"); M returned to the reserved pool (unbound,
 * like the retired P/V keys). Space's old verbs moved aside for it:
 * open-look is Enter-only, Look closes on Esc alone, and the zoomed
 * Space-pan pipeline was deleted outright. Typing-key shortcuts (single
 * letters AND the space character) are suppressed while any text input is
 * focused (UI §11; rule owned by actions/match.ts).
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
  | { kind: "toggle-graph" }
  | { kind: "toggle-lights-out" }
  | { kind: "toggle-rail" }
  | { kind: "toggle-cheatsheet" }
  | { kind: "open-search" }
  | { kind: "summon-note" }
  | { kind: "toggle-auto-advance" }
  | { kind: "toggle-fullscreen" }
  // UI scale (desktop-conventions pass, June 2026): WEBVIEW zoom — the
  // whole chrome scales. Distinct from Look's IMAGE zoom band (zoom-step
  // et al. below): Cmd-modified chords are the UI, plain keys the image.
  | { kind: "ui-zoom"; delta: 1 | -1 }
  | { kind: "ui-zoom-reset" }
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
  // attention heatmap (DESIGN-ATTENTION-HEATMAP.md)
  | { kind: "toggle-heat" }
  | { kind: "toggle-attention-all-time" }
  | { kind: "stack-toggle-active" }
  | { kind: "stack-collapse-all" }
  | { kind: "stack-expand-all" }
  | { kind: "flip-stack-member" } // R: viewed member in Look, active in Grid (D1)
  // look
  | { kind: "look-nav"; delta: 1 | -1 }
  // look-close keeps its union seat (extended, never narrowed) though its
  // key binding is gone: Space became the mic (June 12 2026) and Esc
  // closes Look through the escape ladder ("leave-look") instead.
  | { kind: "look-close" }
  | { kind: "zoom-toggle" }
  | { kind: "zoom-step"; delta: 1 | -1 }
  | { kind: "zoom-fit" }
  | { kind: "zoom-100" }
  | { kind: "toggle-filmstrip" }
  | { kind: "toggle-histogram" }
  // panels
  | { kind: "rail-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "rail-enter" }
  | { kind: "rail-folder-open"; rootId: string; folder: string }
  | { kind: "rail-folder-reveal"; rootId: string; folder: string }
  | { kind: "rescan-root"; rootId: string }
  // recovery verb SEPARATE from Rescan (BACKLOG, dogfood round 3): re-pend
  // the preview pass for everything under the root; regeneration overwrites
  // artifacts idempotently (LIBRARY §9.8)
  | { kind: "rebuild-previews"; rootId: string }
  // archive lifecycle (folder-tree improvements): non-destructive hide a
  // root from the active rail (journal + memberships preserved), and restore
  | { kind: "archive-root"; rootId: string }
  | { kind: "unarchive-root"; rootId: string }
  // rail footer button + rail-folder seat: OS picker → register root
  | { kind: "add-root" }
  // collections (B71 — rail Collections tab, first slice): open a
  // collection's current members in the grid (the folder-open sibling)
  | { kind: "collection-open"; id: string }
  // thumb seat submenu: add the WHOLE selection to the named collection
  | { kind: "add-to-collection"; id: string }
  // thumb seat: mint a NEW collection AND add the WHOLE selection to it in
  // one evented step (founder, dogfood June 12 2026 — the zero-collections
  // dead end). Carries no id: the perform sink captures the targets, then
  // arms the rail's inline creator (the ONE create UX) to name + commit.
  | { kind: "new-collection-add" }
  // thumb seat submenu: close the WHOLE selection's open membership
  // intervals in the named collection (evented, never destructive —
  // RETRIEVAL §10.1; gathering must be reversible from the same menu)
  | { kind: "remove-from-collection"; id: string }
  | { kind: "open-inspector"; tab: "metadata" | "journal" }
  | { kind: "close-inspector" }
  // journal row verbs (pointer-seated; Stage C wires the flows)
  | { kind: "journal-correct"; eventId: string }
  | { kind: "journal-retract"; eventId: string }
  | { kind: "journal-redact"; eventId: string }
  | { kind: "journal-toggle-retracted" }
  // select-from-note (BACKLOG): jump home + select the event's FULL target
  // set in the grid, selection order = event_targets.position
  | { kind: "select-journal-targets"; targets: string[] }
  // stroke row click → flash that stroke on the Look overlay (UI §8.2)
  | { kind: "journal-flash-stroke"; eventId: string }
  // OS integration (D4 — no deletion verbs, D3)
  | { kind: "reveal-in-file-manager" }
  | { kind: "copy-file-path" }
  | { kind: "open-with-default" }
  // configurable external editor (BACKLOG "Configurable external editor, D4
  // revisit"): hand the original off to the user's editor (review here,
  // edit there). Pointer-seated only — no key.
  | { kind: "open-in-external-editor" }
  // "More like this" (B69 retrieval-stays-additive): nearest visual neighbors
  // of the active image, rendered as a `similar` grid scope. Pointer-seated
  // only — no key.
  | { kind: "find-similar" }
  // search
  | { kind: "search-nav"; dir: "up" | "down" | "left" | "right" }
  | { kind: "search-open-result" }
  | { kind: "remove-last-chip" }
  // grease pencil (P5.1 — keymap reconciliation: B toggle, hold-E eraser,
  // O overlay, Ctrl+Z undo; the reserved pencil-visibility row collapsed
  // into the single overlay toggle)
  | { kind: "pencil-pen" }
  | { kind: "pencil-eraser" }
  | { kind: "pencil-undo" }
  | { kind: "cycle-overlay" }
  // voice capture (M2b; two-gesture since the June 2026 ruling, on Space
  // since June 12 2026): mic-press is the Space KEYDOWN — it begins the
  // tap-vs-hold machine (logic/michold.ts); toggle-mic is the
  // instantaneous pointer form (indicator segment click — a click IS a
  // tap, so it toggles).
  | { kind: "toggle-mic" }
  | { kind: "mic-press" }
  // the station's info seat (pointer-only): pin the expansion open
  | { kind: "toggle-station-detail" };

/** The P3.2 KeyContext fields — still required, so existing fixtures and
 * callers compile unchanged. */
type LegacyContextKeys =
  | "viewMode"
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
  sort: DEFAULT_SORT,
  queryActive: false,
  heatOn: false,
  heatAllTime: false,
  thumbStep: DEFAULT_THUMB_STEP,
  surround: "black",
  filmstrip: false,
  histogram: false,
  pencilMode: false,
  overlayVisible: true, // tracing paper defaults ON (UI §4.4)
  pencilUndoable: false,
  micArmed: false,
  micState: "disarmed",
  asrUnavailable: true, // degraded until a supervised ASR reports Ready (P6.3)
  collections: [],
  activeMemberships: [],
};

export function withDefaults(ctx: KeyContext): ActionContext {
  return { ...CONTEXT_DEFAULTS, ...ctx };
}

/** Signature preserved from P3.2; internally match(e, ctx, REGISTRY). */
export function dispatch(e: KeyInput, ctx: KeyContext): Action | null {
  return match(e, withDefaults(ctx), REGISTRY);
}
