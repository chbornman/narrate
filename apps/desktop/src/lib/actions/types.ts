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

/** The context-menu seats (journal-row joined with the select-from-note
 * polish) + the reserved M2a toolbar seat. */
export type MenuSeat =
  | "thumb"
  | "gutter"
  | "rail-folder"
  | "look-backdrop"
  | "journal-row"
  | "look-toolbar";

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
  /** ASR ready — LIVE off the runtime-status channel since P6.2
   * (RUNTIME §8.3): the EXISTENCE gate for mic surfaces (glyph, M row).
   * Never true before P6.3 vendors real binaries. */
  asrReady: boolean;
  // radio state for menus
  sort: SortMode;
  thumbStep: number;
  surround: SurroundLevel;
  filmstrip: boolean;
  // grease pencil (live since P5.1 — M2a)
  /** Sticky pencil mode (B; Look only, resets on return to Grid). */
  pencilMode: boolean;
  /** Tracing-paper overlay shown (O; hidden ⇒ pencil unavailable). */
  overlayVisible: boolean;
  /** Ctrl+Z has pencil work: a pen-down to cancel or a stacked stroke to
   * retract — otherwise the pencil layer must not swallow the chord. */
  pencilUndoable: boolean;
  // mic (CAPTURE §6.4/§11). ONE coherent readiness story since P6.2:
  // `asrReady` above answers "does ASR exist?" (RUNTIME §8.3 readiness —
  // surfaces appear); `asrUnavailable` answers "did it degrade under the
  // user?" (CAPTURE §11 indicator flag — the muted-mic glyph, quietly).
  micArmed: boolean;
  /** The §6.4 mic state machine, exactly the indicator contract's enum. */
  micState: MicState;
  /** §11 degraded flag: ASR down/absent — the muted-mic glyph, quietly. */
  asrUnavailable: boolean;
  /** Collections (B71), list order, for the thumb menu's Add-to-collection
   * submenu — id + name only: menus never need the full DTO. */
  collections: { id: string; name: string }[];
  /** Collection ids the ACTIVE image is CURRENTLY a member of (open
   * intervals) — the Add-to-collection checkmarks and the
   * Remove-from-collection submenu. With a multi-selection the marks
   * show the active image's truth (the Rate radio precedent). */
  activeMemberships: string[];
}

/** CAPTURE §6.4 mic states, as the wire spells them (types/dto.ts twin). */
export type MicState =
  | "disarmed"
  | "arming"
  | "armedIdle"
  | "armedSpeaking"
  | "disarmedError";

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
  /** This verb writes the clipboard (BACKLOG "Copy actions confirm
   * themselves"): menu rendering keys the row into the shared copy
   * register — the row flashes a check when the write lands and the
   * menu holds open just long enough to show it. Any future copy verb
   * joins the register by setting this flag. */
  copyConfirm?: true;
  /** Reserved seat (P/E/V, M, overlay-cycle): dispatches to nothing,
   * hidden from menus + cheatsheet until its packet. */
  reserved?: true;
}

/** "Modes are visible" (featureset §0) — by construction: every sticky
 * state is a ModeDef whose segment renders in the indicator. `tone` is
 * the quiet rendering hint (UI §7.3): dimmed glyphs for off/degraded
 * states, the breathing affordance while speech is detected. `icon`
 * names a Lucide glyph the indicator renders INSTEAD of `text` (no
 * emoji, ever — founder rule); `text` stays as the accessible label. */
export interface ModeDef {
  id: "auto-advance" | "pencil" | "mic"; // M2a/M2b ids reserved now
  isOn(ctx: ActionContext): boolean;
  segment(
    ctx: ActionContext,
  ): { text: string; title: string; tone?: SegmentTone; icon?: SegmentIcon } | null;
}

export type SegmentTone = "dim" | "live";
export type SegmentIcon = "mic" | "mic-off" | "pencil";
