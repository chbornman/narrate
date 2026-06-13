/**
 * Filmstrip windowing math (pure — the component feeds scroll/size facts).
 *
 * The strip renders the WHOLE order virtually: the scroller's content is
 * sized for every cell (lead/tail spacers), but only the cells inside the
 * viewport ± overscan exist in the DOM. This replaced the fixed
 * 17-neighbor window (founder, June 12 2026: "filmstrip only loads 17
 * images… when squished small it doesn't fill the width") — a fixed
 * radius both capped the strip and, at small panel heights, couldn't
 * produce enough cells to fill the row.
 *
 * One pitch = cell + gap. Cells are square (cell = inner panel height),
 * so the pitch — and therefore the whole window — re-derives when the
 * panel is drag-resized.
 */

export interface StripWindow {
  /** Absolute index of the first rendered cell. */
  first: number;
  /** How many cells to render. */
  count: number;
  /** Spacer widths (px) standing in for the unrendered runs. */
  leadPx: number;
  tailPx: number;
}

/** DOM cells beyond each viewport edge — enough that flick-scrolling
 * shows pixels, small enough that a 50k-image folder stays light. */
export const STRIP_OVERSCAN = 8;

export function stripWindow(
  scrollLeft: number,
  viewportPx: number,
  pitch: number,
  total: number,
  overscan: number = STRIP_OVERSCAN,
): StripWindow {
  if (total <= 0 || pitch <= 0 || viewportPx <= 0) {
    return { first: 0, count: total > 0 ? total : 0, leadPx: 0, tailPx: 0 };
  }
  const first = Math.max(0, Math.floor(scrollLeft / pitch) - overscan);
  const visible = Math.ceil(viewportPx / pitch) + overscan * 2;
  const count = Math.max(0, Math.min(total - first, visible));
  return {
    first,
    count,
    leadPx: first * pitch,
    tailPx: Math.max(0, (total - first - count) * pitch),
  };
}

/** Where to put scrollLeft so cell `index` sits centered — clamped to the
 * scroll range so edge cells never force a useless rubber-band. The
 * component re-applies this on EVERY index or geometry change (founder,
 * June 12 2026: the selected photo stays horizontally centered through
 * sidebar toggles and window resizes, not just navigation). */
export function centerOn(
  index: number,
  viewportPx: number,
  pitch: number,
  total: number,
): number {
  const max = Math.max(0, total * pitch - viewportPx);
  return Math.min(max, Math.max(0, index * pitch - viewportPx / 2 + pitch / 2));
}
