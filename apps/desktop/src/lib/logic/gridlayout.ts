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
  /** Snapped cell size (fractional px allowed — rows fill exactly). This is
   * the square IMAGE box side; the info strip is added on TOP of it. */
  cell: number;
  /** Info-strip height (px) reserved ABOVE the image box — 0 when cell-info
   * is off. Global/all-cells, so every row reserves the same strip and rows
   * stay uniform (cellinfo.ts infoStripHeight). */
  info: number;
  /** Row stride: cell + info + gap. */
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
  info = 0,
): GridGeometry {
  const avail = containerW - pad * 2;
  const cols = Math.max(1, Math.round((avail + gap) / (target + gap)));
  const cell = Math.max(1, (avail - (cols - 1) * gap) / cols);
  // The strip lives inside the row stride ON TOP of the square image box, so
  // the cell (image side) is unchanged and rows stay uniform.
  return { cols, cell, info, rowH: cell + info + gap, gap, pad };
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

/** Overscan: one full screen of cells mounted above AND below the
 * viewport (UI §3.3). visibleRange and poolSize must encode the SAME
 * policy — poolSize's collision-free guarantee holds only while its span
 * covers the maximal window — so the value lives in exactly one place. */
const OVERSCAN_SCREENS = 1;

/** The cells that actually intersect the viewport — no overscan.
 *
 * Keep this separate from [`visibleRange`]: mounted cells include a screen of
 * runway on both sides for smooth scrolling, but preview generation and fetch
 * priority must describe what the user can see NOW. Treating the whole mounted
 * window as visible eagerly requests three screens and lets speculative work
 * compete with the final viewport after a fling.
 */
export function viewportRange(
  g: GridGeometry,
  scrollTop: number,
  viewportH: number,
  count: number,
): { start: number; end: number } {
  const rows = totalRows(g, count);
  const startRow = Math.min(rows, Math.max(0, Math.floor(scrollTop / g.rowH)));
  const endRow = Math.min(
    rows,
    Math.max(startRow, Math.ceil((scrollTop + viewportH) / g.rowH)),
  );
  return { start: Math.min(count, startRow * g.cols), end: Math.min(count, endRow * g.cols) };
}

/** Mounted index window: visible rows + OVERSCAN_SCREENS screens above
 * and below (UI §3.3). */
export function visibleRange(
  g: GridGeometry,
  scrollTop: number,
  viewportH: number,
  count: number,
): { start: number; end: number } {
  const rows = totalRows(g, count);
  const startRow = Math.min(
    rows,
    Math.max(0, Math.floor((scrollTop - OVERSCAN_SCREENS * viewportH) / g.rowH)),
  );
  const endRow = Math.min(
    rows,
    Math.max(startRow, Math.ceil((scrollTop + (1 + OVERSCAN_SCREENS) * viewportH) / g.rowH)),
  );
  return { start: Math.min(count, startRow * g.cols), end: Math.min(count, endRow * g.cols) };
}

/** DOM-recycling pool size: always ≥ the maximal visibleRange window at
 * this geometry, so the idx → idx % poolSize ring stays collision-free.
 * The span is the window's worst case — the viewport plus overscan on
 * both sides — with a 2-row margin for the floor/ceil rounding. */
export function poolSize(g: GridGeometry, viewportH: number): number {
  return Math.max(
    1,
    (Math.ceil(((1 + 2 * OVERSCAN_SCREENS) * viewportH) / g.rowH) + 2) * g.cols,
  );
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
