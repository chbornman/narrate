/**
 * Write-scope target derivation (CAPTURE §3): the UI's only scope job is to
 * REPORT the selection/view-derived target list to the core and render the
 * echoed scope. The mapping below is CAPTURE §3's table, verbatim:
 *
 *   Single-image view        → the viewed image
 *   Grid, selection of 1     → that image
 *   Grid, selection of N ≥ 2 → selected images, in selection order
 *   Grid, no selection       → session (zero targets)
 *   Search results           → same rules over result selection
 *
 * P4.2 stacks (D1): this module consumes PRE-EXPANDED target lists — stack
 * expansion happens upstream (grid.selectionTargets via stacks.ts, display
 * member first, JPEG then RAW; the Look slice does the same for a viewed
 * collapsed pair). A collapsed RAW+JPEG stack is one cell but TWO targets:
 * the indicator truthfully reads "● 2" (coordinator ruling; the backend is
 * untouched — K13).
 */

export interface ScopeSource {
  surface: "grid" | "look";
  searchOpen: boolean;
  /** Stack-expanded grid selection targets, in selection order. */
  gridSelection: string[];
  /** Selection within search results (hashes). */
  searchSelection: string[];
  /** Hashes shown in Look — [display] or [display, alt] for a collapsed
   * pair; empty when not in Look. */
  lookTargets: string[];
}

export function scopeTargets(src: ScopeSource): string[] {
  if (src.searchOpen) return [...src.searchSelection];
  if (src.surface === "look") return [...src.lookTargets];
  return [...src.gridSelection];
}

/** Indicator scope text (UI §7.2): `● N` or `● session`; `● 0` never renders. */
export function scopeLabel(kind: string, count: number): string {
  return kind === "session" ? "session" : String(count);
}

/** Typed-note placeholder copy (UI §6: scope echoed as dimmed placeholder). */
export function notePlaceholder(kind: string, count: number): string {
  if (kind === "session") return "note on this session";
  if (count === 1) return "note on 1 image";
  return `note on ${count} images`;
}
