/**
 * Escape = strictly "step back one layer" (spec/UI.md §2.2 amended by the
 * P4.2 featureset §0/§3 — Esc is sacred), in this exact order, one flag
 * per layer, exhaustively tested:
 *
 *   1  redaction modal (Cancel — the app's one modal, R5)
 *   2  drop-confirm sheet (INTEGRATION amendment: the drag-folder confirm
 *      is scrim-covered topmost chrome, so it peels first among transients;
 *      the Sheet contract promises Esc dismisses, and Esc routes only
 *      through this order — recorded in DECISIONS)
 *   3  context menu (any seat, incl. sort ▾)
 *   4  inline journal correction (text-edit exits first, §0)
 *   5  journal composer focus (polish round: the panel's inline remark
 *      input — Esc exits text-input focus first, §0; the draft survives)
 *   6  note input
 *   7  cheatsheet
 *   8  indicator popover
 *   9  debug panel
 *  10  search (back to the surface it was invoked from)
 *  11  Look → Grid (same image active; the INSPECTOR STAYS — founder,
 *      June 2026: "when we return to the grid, an image will still be
 *      selected", its panel content with it. This moved the inspector
 *      layer BELOW search and Look→Grid; relative order of every other
 *      layer is unchanged)
 *  12  inspector
 *  13  clear selection
 *  14  none — Escape NEVER quits
 */

export interface EscapeContext {
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
  if (ctx.redactionModalOpen) return "close-redaction-modal"; // 1
  if (ctx.dropConfirmOpen) return "close-drop-confirm"; // 2
  if (ctx.contextMenuOpen) return "close-context-menu"; // 3
  if (ctx.journalEditOpen) return "close-journal-edit"; // 4
  if (ctx.journalComposerFocused) return "blur-journal-composer"; // 5
  if (ctx.noteInputOpen) return "close-note-input"; // 6
  if (ctx.cheatsheetOpen) return "close-cheatsheet"; // 7
  if (ctx.indicatorPopoverOpen) return "close-indicator-popover"; // 8
  if (ctx.debugPanelOpen) return "close-debug-panel"; // 9
  if (ctx.searchOpen) return "leave-search"; // 10
  if (ctx.surface === "look") return "leave-look"; // 11 — inspector stays
  if (ctx.inspectorOpen) return "close-inspector"; // 12
  if (ctx.hasSelection) return "clear-selection"; // 13
  return "none"; // 14 — never quits
}
