/**
 * Integer-column snap layout (featureset §1): the size slider sets a
 * TARGET cell size; the actual cell width snaps to container ÷ integer
 * column count so rows always fill the width exactly. Recompute on
 * resize/panel push is free by construction — Grid.svelte derives the
 * geometry from a reactive container width.
 *
 * Also the pure row math for the virtualizer (window + 1 screen of
 * overscan, UI §3.3), the page geometry the Home/End/PgUp/PgDn moves read
 * (logic/selection.ts does the index math), and the scroll anchor that
 * preserves position across Look round-trips, folder revisits, and
 * column-count re-snaps.
 */

export interface GridGeometry {
  /** Integer column count, ≥ 1. */
  cols: number;
  /** Snapped cell size (fractional px allowed — rows fill exactly). */
  cell: number;
  /** Row stride: cell + gap. */
  rowH: number;
  gap: number;
  pad: number;
}

/** Snap: pick the integer column count whose ideal cell is nearest the
 * target, then stretch cells so cols·cell + gaps + padding = container. */
export function snap(
  containerW: number,
  target: number,
  gap: number,
  pad: number,
): GridGeometry {
  const avail = containerW - pad * 2;
  const cols = Math.max(1, Math.round((avail + gap) / (target + gap)));
  const cell = Math.max(1, (avail - (cols - 1) * gap) / cols);
  return { cols, cell, rowH: cell + gap, gap, pad };
}

/** Canvas-space position of cell `index`. */
export function position(g: GridGeometry, index: number): { x: number; y: number } {
  return {
    x: g.pad + (index % g.cols) * (g.cell + g.gap),
    y: g.pad + Math.floor(index / g.cols) * g.rowH,
  };
}

export function totalRows(g: GridGeometry, count: number): number {
  return Math.ceil(count / g.cols);
}

export function totalHeight(g: GridGeometry, count: number): number {
  return totalRows(g, count) * g.rowH + g.pad * 2;
}

/** Whole rows per viewport — the PgUp/PgDn stride (× cols in selection.ts). */
export function rowsPerPage(g: GridGeometry, viewportH: number): number {
  return Math.max(1, Math.floor(viewportH / g.rowH));
}

/** Mounted index window: visible rows + 1 screen of overscan above and
 * below (UI §3.3). */
export function visibleRange(
  g: GridGeometry,
  scrollTop: number,
  viewportH: number,
  count: number,
): { start: number; end: number } {
  const rows = totalRows(g, count);
  const startRow = Math.min(rows, Math.max(0, Math.floor((scrollTop - viewportH) / g.rowH)));
  const endRow = Math.min(rows, Math.max(startRow, Math.ceil((scrollTop + 2 * viewportH) / g.rowH)));
  return { start: Math.min(count, startRow * g.cols), end: Math.min(count, endRow * g.cols) };
}

/** DOM-recycling pool size: always ≥ the maximal visibleRange window at
 * this geometry, so the idx → idx % poolSize ring stays collision-free. */
export function poolSize(g: GridGeometry, viewportH: number): number {
  return Math.max(1, (Math.ceil((3 * viewportH) / g.rowH) + 2) * g.cols);
}

/** First visible unit + its pixel offset within the viewport — survives
 * geometry changes (the unit, not the pixel, is what's anchored). */
export interface ScrollAnchor {
  index: number;
  offset: number;
}

export function captureAnchor(
  g: GridGeometry,
  scrollTop: number,
  count: number,
): ScrollAnchor | null {
  if (count === 0) return null;
  const row = Math.max(0, Math.floor((scrollTop - g.pad) / g.rowH));
  const index = Math.min(count - 1, row * g.cols);
  const top = g.pad + Math.floor(index / g.cols) * g.rowH;
  return { index, offset: scrollTop - top };
}

/** scrollTop that puts the anchored unit back at its offset under the
 * (possibly different) geometry, clamped to the scrollable range. The
 * offset is clamped to one row so a size change cannot skip the anchor. */
export function restoreScroll(
  g: GridGeometry,
  anchor: ScrollAnchor,
  count: number,
  viewportH: number,
): number {
  if (count === 0) return 0;
  const index = Math.min(Math.max(0, anchor.index), count - 1);
  const offset = Math.min(Math.max(0, anchor.offset), g.rowH - 1);
  const top = g.pad + Math.floor(index / g.cols) * g.rowH + offset;
  const max = Math.max(0, totalHeight(g, count) - viewportH);
  return Math.min(Math.max(0, top), max);
}
