/**
 * Integer-column snap layout + scroll anchors (logic/gridlayout.ts,
 * featureset §1): the slider sets a TARGET; the snapped cells always fill
 * the container width exactly, recomputed for any width (resize and panel
 * push are just new widths). Anchors are UNIT-based, so position survives
 * geometry changes (Look round-trips, column re-snaps, size steps).
 */
import { describe, expect, it } from "vitest";
import * as layout from "../src/lib/logic/gridlayout";

const GAP = 8;
const PAD = 10;

describe("integer-column snap", () => {
  it("rows fill the container width EXACTLY for any width/target", () => {
    for (let w = 320; w <= 2560; w += 73) {
      for (const target of [96, 160, 240, 320]) {
        const g = layout.snap(w, target, GAP, PAD);
        const filled = g.cols * g.cell + (g.cols - 1) * GAP + 2 * PAD;
        expect(Math.abs(filled - w), `w=${w} target=${target}`).toBeLessThan(0.001);
        expect(Number.isInteger(g.cols)).toBe(true);
        expect(g.cols).toBeGreaterThanOrEqual(1);
      }
    }
  });

  it("picks the column count nearest the target cell size", () => {
    // avail = 980; ideal at 240 → (980+8)/(240+8) ≈ 3.98 → 4 columns.
    const g = layout.snap(1000, 240, GAP, PAD);
    expect(g.cols).toBe(4);
    expect(g.cell).toBeCloseTo((980 - 3 * GAP) / 4, 5);
  });

  it("snapped cells deviate from the target less than one full step", () => {
    for (let w = 480; w <= 2000; w += 97) {
      const g = layout.snap(w, 160, GAP, PAD);
      // Never snaps to a wildly different size than asked for.
      expect(Math.abs(g.cell - 160)).toBeLessThan(160 / 2 + GAP);
    }
  });

  it("a too-narrow container still yields one usable column", () => {
    const g = layout.snap(40, 320, GAP, PAD);
    expect(g.cols).toBe(1);
    expect(g.cell).toBeGreaterThanOrEqual(1);
  });
});

describe("row math", () => {
  const g = layout.snap(1000, 240, GAP, PAD); // 4 cols

  it("position walks columns then wraps rows", () => {
    expect(layout.position(g, 0)).toEqual({ x: PAD, y: PAD });
    expect(layout.position(g, 3).x).toBeCloseTo(PAD + 3 * (g.cell + GAP), 5);
    expect(layout.position(g, 4)).toEqual({ x: PAD, y: PAD + g.rowH });
  });

  it("total rows/height cover a partial last row", () => {
    expect(layout.totalRows(g, 9)).toBe(3);
    expect(layout.totalHeight(g, 9)).toBeCloseTo(3 * g.rowH + 2 * PAD, 5);
    expect(layout.totalRows(g, 0)).toBe(0);
  });

  it("rowsPerPage is the PgUp/PgDn stride and never zero", () => {
    expect(layout.rowsPerPage(g, g.rowH * 3 + 10)).toBe(3);
    expect(layout.rowsPerPage(g, 1)).toBe(1);
  });
});

describe("virtualizer window (UI §3.3: +1 screen overscan)", () => {
  const g = layout.snap(1000, 240, GAP, PAD); // 4 cols
  const vh = 600;

  it("clamps at the ends and stays within the item count", () => {
    const top = layout.visibleRange(g, 0, vh, 100);
    expect(top.start).toBe(0);
    const bottom = layout.visibleRange(g, 1e9, vh, 100);
    expect(bottom.end).toBe(100);
    expect(bottom.start).toBeLessThanOrEqual(bottom.end);
  });

  it("includes one screen above and below the viewport", () => {
    const scrollTop = 10 * g.rowH;
    const r = layout.visibleRange(g, scrollTop, vh, 10_000);
    const firstRow = Math.floor(r.start / g.cols);
    const lastRow = Math.ceil(r.end / g.cols);
    expect(firstRow).toBeLessThanOrEqual(Math.floor((scrollTop - vh) / g.rowH));
    expect(lastRow).toBeGreaterThanOrEqual(Math.ceil((scrollTop + 2 * vh) / g.rowH));
  });

  it("the recycling pool always covers the window (ring stays collision-free)", () => {
    for (const target of [96, 160, 240, 320]) {
      const gg = layout.snap(1280, target, GAP, PAD);
      const pool = layout.poolSize(gg, vh);
      for (let st = 0; st < 50 * gg.rowH; st += 37) {
        const r = layout.visibleRange(gg, st, vh, 10_000);
        expect(r.end - r.start).toBeLessThanOrEqual(pool);
      }
    }
  });
});

describe("scroll anchors (position preserved — featureset §1)", () => {
  const g = layout.snap(1000, 240, GAP, PAD); // 4 cols
  const vh = 600;
  const count = 400;

  it("round-trips under the same geometry", () => {
    const scrollTop = 7 * g.rowH + 13;
    const a = layout.captureAnchor(g, scrollTop, count);
    expect(a).not.toBeNull();
    expect(layout.restoreScroll(g, a as layout.ScrollAnchor, count, vh)).toBeCloseTo(
      scrollTop,
      5,
    );
  });

  it("anchors the UNIT, not the pixel: the anchored unit stays at its offset after a re-snap", () => {
    const scrollTop = 12 * g.rowH + 5;
    const a = layout.captureAnchor(g, scrollTop, count) as layout.ScrollAnchor;
    const narrower = layout.snap(700, 240, GAP, PAD); // fewer columns
    const restored = layout.restoreScroll(narrower, a, count, vh);
    const unitTop = layout.position(narrower, a.index).y;
    expect(restored).toBeCloseTo(unitTop + Math.min(a.offset, narrower.rowH - 1), 5);
  });

  it("clamps to the scrollable range when the list shrank", () => {
    const a = { index: 399, offset: 0 };
    const restored = layout.restoreScroll(g, a, 8, vh);
    expect(restored).toBe(Math.max(0, layout.totalHeight(g, 8) - vh));
  });

  it("an empty grid anchors nothing", () => {
    expect(layout.captureAnchor(g, 100, 0)).toBeNull();
    expect(layout.restoreScroll(g, { index: 3, offset: 0 }, 0, vh)).toBe(0);
  });
});
