/**
 * Pure edge-panel math (paired with Panel.svelte): size clamping and the
 * resize drag, handed by EDGE — left/right panels resize horizontally,
 * the bottom panel vertically. Persistence lives in state/prefs.ts
 * (pp.panel.<id>.size — ONE global remembered size per panel; founder,
 * June 12 2026). Push panels participate in the shell layout, so the
 * photo grid re-snaps integer columns on every resize (featureset §1).
 */

export type PanelEdge = "left" | "right" | "bottom";

export function clampSize(size: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.round(size)));
}

/**
 * Resize drag from the panel's INNER edge: a left panel grows as the
 * pointer moves right; a right panel as it moves left; a bottom panel as
 * it moves up. `pos` is clientX for side panels, clientY for the bottom.
 */
export function dragSize(
  start: { size: number; pos: number },
  pos: number,
  edge: PanelEdge,
  min: number,
  max: number,
): number {
  const delta = pos - start.pos;
  return clampSize(start.size + (edge === "left" ? delta : -delta), min, max);
}
