/**
 * Escape = strictly "step back one layer" (spec/UI.md §2.2, DECISIONS I1),
 * in this order:
 *   (1) close any open transient (note input, sort menu, journal panel*,
 *       indicator popover, debug panel)        *journal panel is a later packet
 *   (2) leave Search back to the surface it was invoked from
 *   (3) leave Look back to Grid with the same image focused
 *   (4) in Grid with a selection, clear the selection
 * Escape NEVER quits the app.
 */

export interface EscapeContext {
  noteInputOpen: boolean;
  sortMenuOpen: boolean;
  indicatorPopoverOpen: boolean;
  debugPanelOpen: boolean;
  searchOpen: boolean;
  surface: "grid" | "look";
  hasSelection: boolean;
}

export type EscapeAction =
  | "close-note-input"
  | "close-sort-menu"
  | "close-indicator-popover"
  | "close-debug-panel"
  | "leave-search"
  | "leave-look"
  | "clear-selection"
  | "none";

export function escapeAction(ctx: EscapeContext): EscapeAction {
  // (1) transients — the note input is the innermost (it holds focus).
  if (ctx.noteInputOpen) return "close-note-input";
  if (ctx.sortMenuOpen) return "close-sort-menu";
  if (ctx.indicatorPopoverOpen) return "close-indicator-popover";
  if (ctx.debugPanelOpen) return "close-debug-panel";
  // (2) search back to its invoking surface.
  if (ctx.searchOpen) return "leave-search";
  // (3) Look back to Grid, same image focused.
  if (ctx.surface === "look") return "leave-look";
  // (4) clear grid selection.
  if (ctx.hasSelection) return "clear-selection";
  // Never quits.
  return "none";
}
