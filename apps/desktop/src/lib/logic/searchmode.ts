/**
 * Search lane state machine (M3 search-as-scope, Phase 2): the EXPLICIT,
 * centralized truth for the live-lexical / commit-semantic split.
 *
 * Phase 1 wired the BEHAVIOR (as-you-type runs the keyword lane; Enter runs
 * the hybrid lane) but left the user-facing MODE implicit — the detail row
 * showed one static hint regardless of what the grid was actually scoped by.
 * This module makes the transition legible:
 *
 *   - typing a keystroke      -> "lexical"  (never semantic; the <100 ms floor)
 *   - pressing Enter          -> "semantic" (the committed full-hybrid lane)
 *   - editing after a commit  -> back to "lexical" until the next Enter
 *   - clearing the query      -> the bar is no longer a scope (no lane shown)
 *
 * The state is a single discriminated value (`SearchLane`) the bar derives its
 * status copy from. Keeping the transitions in a PURE function (no Svelte
 * runes, no IPC) lets us unit-test every edge of the machine and lets the
 * store hold ONE flag instead of scattering the lexical/semantic decision
 * across the input/keydown/commit handlers.
 */

/** Which lane produced the query results the grid is CURRENTLY showing.
 * `"none"` = the bar is not a committed scope (empty, or sub-threshold). */
export type SearchLane = "none" | "lexical" | "semantic";

/** What just happened to the bar — the inputs that move the machine. */
export type SearchEvent =
  | "type" // a keystroke ran the as-you-type lexical lane
  | "commit" // Enter ran the semantic lane
  | "clear"; // the query scope was dropped (Esc / residue / source switch)

/**
 * The lane after `event`, given the current `lane`. PURE and total:
 *
 *   - "type"   -> always "lexical". Every keystroke is lexical, even right
 *                 after a semantic commit: editing drops you back to the live
 *                 keyword lane until you press Enter again (the design's
 *                 "re-typing drops back to lexical").
 *   - "commit" -> always "semantic". Enter is the only path to the hybrid lane.
 *   - "clear"  -> always "none". The bar stops being a scope.
 *
 * `lane` is accepted (not just ignored) so the signature stays a reducer the
 * store can call uniformly; the transition happens to be memoryless today, but
 * threading the prior lane keeps the door open for a future "stay semantic on
 * a no-op refresh" rule without touching call sites.
 */
export function nextLane(_lane: SearchLane, event: SearchEvent): SearchLane {
  switch (event) {
    case "type":
      return "lexical";
    case "commit":
      return "semantic";
    case "clear":
      return "none";
  }
}

/** The detail-row status copy for a lane. No em-dashes (the `check:emdash`
 * gate): the lexical hint uses " · " to point at the Enter upgrade; the
 * committed state is a single quiet word. `"none"` shows nothing (the bar is
 * not a scope, so there is no lane to name). */
export function laneStatus(lane: SearchLane): string {
  switch (lane) {
    case "lexical":
      return "lexical · Enter for semantic";
    case "semantic":
      return "semantic";
    case "none":
      return "";
  }
}
