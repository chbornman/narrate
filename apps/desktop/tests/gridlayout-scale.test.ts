/**
 * Windowing invariant at scale (AUDIT 2026-07-07 T3a). CONTRACT PINNED:
 * the virtualizer's mounted window (gridlayout.ts visibleRange) and its
 * DOM-recycling pool (poolSize) are bounded by VIEWPORT-derived geometry
 * alone — item count only clamps, never grows them. Grid.svelte mounts
 * exactly `visibleRange` cells, so this is the render-everything
 * regression tripwire: if a change ever makes the window scale with the
 * library (1k -> 50k items suddenly mounting 50k <img> nodes), these
 * assertions fail long before a dogfood session notices the jank.
 */
import { describe, expect, it } from "vitest";
import * as layout from "../src/lib/logic/gridlayout";

const GAP = 8;
const PAD = 10;
const VH = 900; // a tall desktop viewport — worst case for the bound
const COUNTS = [1_000, 10_000, 50_000];

/** The viewport-only bound poolSize encodes: viewport + one overscan
 * screen each side, plus the 2-row rounding margin, in cells. */
function viewportBound(g: layout.GridGeometry): number {
  return (Math.ceil((3 * VH) / g.rowH) + 2) * g.cols;
}

describe("virtualizer window is item-count independent (render-everything tripwire)", () => {
  it("window span and pool stay within the viewport-derived bound at 1k/10k/50k", () => {
    for (const target of [96, 160, 240, 320, 420, 512]) {
      const g = layout.snap(1440, target, GAP, PAD);
      const pool = layout.poolSize(g, VH);
      expect(pool).toBeLessThanOrEqual(viewportBound(g));
      for (const count of COUNTS) {
        const total = layout.totalHeight(g, count);
        // Top, a mid-list row, and pinned to the very bottom.
        for (const scrollTop of [0, total / 2, Math.max(0, total - VH)]) {
          const r = layout.visibleRange(g, scrollTop, VH, count);
          const span = r.end - r.start;
          expect(span, `target=${target} count=${count} top=${scrollTop}`).toBeLessThanOrEqual(
            pool,
          );
          // The tripwire itself: the window must never approach the list.
          expect(span).toBeLessThan(count);
        }
      }
    }
  });

  it("mid-scroll window span is IDENTICAL across item counts", () => {
    // Same geometry, same scroll position, 50x the items: the mounted
    // window must not change by a single cell.
    const g = layout.snap(1440, 160, GAP, PAD);
    const scrollTop = 40 * g.rowH; // deep enough that no count clamps it
    const spans = COUNTS.map((count) => {
      const r = layout.visibleRange(g, scrollTop, VH, count);
      return r.end - r.start;
    });
    expect(spans[1]).toBe(spans[0]);
    expect(spans[2]).toBe(spans[0]);
    expect(spans[0]).toBeGreaterThan(0);
  });

  it("poolSize takes no item count at all — the type pins it, the value confirms", () => {
    // poolSize(geometry, viewportH) has no count parameter by design; a
    // sanity ceiling keeps it honest as an absolute number too: even at
    // the smallest cells a 900px viewport pools a few hundred cells, not
    // thousands.
    const g = layout.snap(1440, 96, GAP, PAD);
    expect(layout.poolSize(g, VH)).toBeLessThan(600);
  });
});
