/**
 * Marquee selection math (featureset §1): drag from EMPTY GUTTER space
 * rubber-bands; a drag that starts on a thumb never marquees (that origin
 * is the M3 drag-selection-to-rail branch — classified now so the split
 * needs no rework). Ctrl held = additive merge (selection.ts marqueeMerge
 * keeps CAPTURE §3 selection order truthful). Rendering is
 * grid/Marquee.svelte; pointer wiring is Grid.svelte; everything here is
 * pure and unit-tested.
 */
import type { GridGeometry } from "./gridlayout";

/** Movement below this (Chebyshev px) is a click, not a drag. */
export const DRAG_THRESHOLD = 4;

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Origin decides the gesture (§8: single click zone per cell — a thumb
 * drag is never a marquee). "item-drag" dispatches to nothing until M3. */
export function classifyDrag(originOnThumb: boolean): "marquee" | "item-drag" {
  return originOnThumb ? "item-drag" : "marquee";
}

export function isDrag(origin: Point, current: Point, threshold = DRAG_THRESHOLD): boolean {
  return (
    Math.max(Math.abs(current.x - origin.x), Math.abs(current.y - origin.y)) >= threshold
  );
}

/** Normalized rect between the drag endpoints (any drag direction). */
export function rectFrom(origin: Point, current: Point): Rect {
  return {
    x: Math.min(origin.x, current.x),
    y: Math.min(origin.y, current.y),
    w: Math.abs(current.x - origin.x),
    h: Math.abs(current.y - origin.y),
  };
}

/**
 * Rect → cell indices (canvas space). Candidate rows/columns come from the
 * stride; each candidate is then verified against the CELL box, so a rect
 * living entirely in a gap hits nothing. Returned ascending (grid order —
 * selection.ts marqueeMerge re-derives its own order rules).
 */
export function hitTest(rect: Rect, g: GridGeometry, count: number): number[] {
  if (count === 0 || rect.w < 0 || rect.h < 0) return [];
  const stride = g.cell + g.gap;
  const colLo = Math.max(0, Math.floor((rect.x - g.pad) / stride));
  const colHi = Math.min(g.cols - 1, Math.floor((rect.x + rect.w - g.pad) / stride));
  const rowLo = Math.max(0, Math.floor((rect.y - g.pad) / g.rowH));
  const rowHi = Math.floor((rect.y + rect.h - g.pad) / g.rowH);
  const hits: number[] = [];
  for (let row = rowLo; row <= rowHi; row++) {
    for (let col = colLo; col <= colHi; col++) {
      const idx = row * g.cols + col;
      if (idx >= count) break;
      const x = g.pad + col * stride;
      const y = g.pad + row * g.rowH;
      const overlapsX = rect.x <= x + g.cell && rect.x + rect.w >= x;
      const overlapsY = rect.y <= y + g.cell && rect.y + rect.h >= y;
      if (overlapsX && overlapsY) hits.push(idx);
    }
  }
  return hits;
}
