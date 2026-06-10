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
 */

export interface ScopeSource {
  surface: "grid" | "look";
  searchOpen: boolean;
  /** Grid selection order (hashes). */
  gridSelection: string[];
  /** Selection within search results (hashes). */
  searchSelection: string[];
  /** Hash shown in Look, null when not in Look. */
  lookHash: string | null;
}

export function scopeTargets(src: ScopeSource): string[] {
  if (src.searchOpen) return [...src.searchSelection];
  if (src.surface === "look") return src.lookHash ? [src.lookHash] : [];
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
