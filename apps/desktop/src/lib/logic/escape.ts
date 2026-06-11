/**
 * Escape = strictly "step back one layer" (spec/UI.md §2.2 amended by the
 * P4.2 featureset §0/§3 — Esc is sacred), in this exact order, one flag
 * per layer, exhaustively tested:
 *
 *   1  first-run welcome card (BACKLOG "First-run welcome card": the
 *      storage-story modal — scrim-covered like the redaction modal and
 *      shown before anything else can open, so it peels first; every
 *      dismissal path, Esc included, honors the card's "don't show
 *      again" toggle)
 *   2  redaction modal (Cancel — the app's one DESTRUCTIVE modal, R5)
 *   3  drop-confirm sheet (INTEGRATION amendment: the drag-folder confirm
 *      is scrim-covered topmost chrome, so it peels first among transients;
 *      the Sheet contract promises Esc dismisses, and Esc routes only
 *      through this order — recorded in DECISIONS)
 *   4  context menu (any seat, incl. sort ▾)
 *   5  inline journal correction (text-edit exits first, §0)
 *   6  journal composer focus (polish round: the panel's inline remark
 *      input — Esc exits text-input focus first, §0; the draft survives)
 *   7  note input
 *   8  cheatsheet
 *   9  indicator popover
 *  10  debug panel
 *  11  search (back to the surface it was invoked from)
 *  12  Look → Grid (same image active; the INSPECTOR STAYS — founder,
 *      June 2026: "when we return to the grid, an image will still be
 *      selected", its panel content with it. This moved the inspector
 *      layer BELOW search and Look→Grid; relative order of every other
 *      layer is unchanged)
 *  13  inspector
 *  14  clear selection
 *  15  none — Escape NEVER quits
 */

export interface EscapeContext {
  welcomeCardOpen: boolean;
  redactionModalOpen: boolean;
  dropConfirmOpen: boolean;
  contextMenuOpen: boolean;
  journalEditOpen: boolean;
  journalComposerFocused: boolean;
  noteInputOpen: boolean;
  cheatsheetOpen: boolean;
  indicatorPopoverOpen: boolean;
  debugPanelOpen: boolean;
  inspectorOpen: boolean;
  searchOpen: boolean;
  surface: "grid" | "look";
  hasSelection: boolean;
}

export type EscapeAction =
  | "close-welcome-card"
  | "close-redaction-modal"
  | "close-drop-confirm"
  | "close-context-menu"
  | "close-journal-edit"
  | "blur-journal-composer"
  | "close-note-input"
  | "close-cheatsheet"
  | "close-indicator-popover"
  | "close-debug-panel"
  | "close-inspector"
  | "leave-search"
  | "leave-look"
  | "clear-selection"
  | "none";

export function escapeAction(ctx: EscapeContext): EscapeAction {
  if (ctx.welcomeCardOpen) return "close-welcome-card"; // 1
  if (ctx.redactionModalOpen) return "close-redaction-modal"; // 2
  if (ctx.dropConfirmOpen) return "close-drop-confirm"; // 3
  if (ctx.contextMenuOpen) return "close-context-menu"; // 4
  if (ctx.journalEditOpen) return "close-journal-edit"; // 5
  if (ctx.journalComposerFocused) return "blur-journal-composer"; // 6
  if (ctx.noteInputOpen) return "close-note-input"; // 7
  if (ctx.cheatsheetOpen) return "close-cheatsheet"; // 8
  if (ctx.indicatorPopoverOpen) return "close-indicator-popover"; // 9
  if (ctx.debugPanelOpen) return "close-debug-panel"; // 10
  if (ctx.searchOpen) return "leave-search"; // 11
  if (ctx.surface === "look") return "leave-look"; // 12 — inspector stays
  if (ctx.inspectorOpen) return "close-inspector"; // 13
  if (ctx.hasSelection) return "clear-selection"; // 14
  return "none"; // 15 — never quits
}
