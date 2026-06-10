/**
 * Marquee math (logic/marquee.ts, featureset §1): gutter-origin only (a
 * thumb drag is the M3 item-drag branch), click-vs-drag threshold, rect
 * normalization, rect → indices over the snapped geometry (gaps miss),
 * and the additive merge through selection.ts (selection ORDER is the
 * write-scope order — CAPTURE §3).
 */
import { describe, expect, it } from "vitest";
import * as marquee from "../src/lib/logic/marquee";
import * as layout from "../src/lib/logic/gridlayout";
import * as sel from "../src/lib/logic/selection";

// 4 columns: avail 980, cell 239, rowH 247 (cells at x 10/257/504/751…).
const g = layout.snap(1000, 240, 8, 10);

describe("drag classification (§8: a thumb drag never marquees)", () => {
  it("gutter origin = marquee; thumb origin = item-drag (M3 branch)", () => {
    expect(marquee.classifyDrag(false)).toBe("marquee");
    expect(marquee.classifyDrag(true)).toBe("item-drag");
  });

  it("movement under the threshold is a click, not a drag", () => {
    const o = { x: 100, y: 100 };
    expect(marquee.isDrag(o, { x: 103, y: 97 })).toBe(false);
    expect(marquee.isDrag(o, { x: 104, y: 100 })).toBe(true);
    expect(marquee.isDrag(o, { x: 100, y: 104 })).toBe(true);
  });
});

describe("rect normalization", () => {
  it("any drag direction yields the same rect", () => {
    const r = { x: 10, y: 20, w: 30, h: 40 };
    expect(marquee.rectFrom({ x: 10, y: 20 }, { x: 40, y: 60 })).toEqual(r);
    expect(marquee.rectFrom({ x: 40, y: 60 }, { x: 10, y: 20 })).toEqual(r);
    expect(marquee.rectFrom({ x: 10, y: 60 }, { x: 40, y: 20 })).toEqual(r);
  });
});

describe("rect → indices hit-test", () => {
  it("covers exactly the intersected cells", () => {
    // Spans cells 0 and 1 horizontally on the first row.
    const hits = marquee.hitTest({ x: 200, y: 20, w: 100, h: 10 }, g, 100);
    expect(hits).toEqual([0, 1]);
  });

  it("a rect living entirely in a gap selects nothing", () => {
    // The vertical gap between col 0 (ends at 249) and col 1 (starts 257).
    const x = 10 + g.cell + 1;
    const hits = marquee.hitTest({ x, y: 20, w: 5, h: 10 }, g, 100);
    expect(hits).toEqual([]);
  });

  it("spans rows in grid order", () => {
    // Column 1 area, from row 0 into row 1.
    const x = 10 + (g.cell + 8) + 5;
    const hits = marquee.hitTest({ x, y: 20, w: 10, h: g.rowH }, g, 100);
    expect(hits).toEqual([1, 5]);
  });

  it("clips to the item count (partial last row)", () => {
    const huge = { x: 0, y: 0, w: 5000, h: 5000 };
    expect(marquee.hitTest(huge, g, 6)).toEqual([0, 1, 2, 3, 4, 5]);
    expect(marquee.hitTest(huge, g, 0)).toEqual([]);
  });

  it("a zero-area point on a cell hits that cell", () => {
    const p = layout.position(g, 5);
    const hits = marquee.hitTest({ x: p.x + 2, y: p.y + 2, w: 0, h: 0 }, g, 100);
    expect(hits).toEqual([5]);
  });
});

describe("merge into the selection (Ctrl = additive — featureset §1)", () => {
  const items = ["a", "b", "c", "d", "e", "f"];

  it("replace: the hit set becomes the selection, last hit active", () => {
    const s0 = sel.click(sel.EMPTY, items, 0);
    const rect = { x: 200, y: 20, w: 100, h: 10 }; // cells 0,1
    const s = sel.marqueeMerge(s0, items, marquee.hitTest(rect, g, items.length), false);
    expect(s.order).toEqual(["a", "b"]);
    expect(s.focus).toBe(1);
  });

  it("additive: existing SELECTION ORDER preserved, new hits append in grid order", () => {
    let s = sel.click(sel.EMPTY, items, 5); // f first — order matters
    const rect = { x: 200, y: 20, w: 100, h: 10 }; // cells 0,1
    s = sel.marqueeMerge(s, items, marquee.hitTest(rect, g, items.length), true);
    expect(s.order).toEqual(["f", "a", "b"]);
  });

  it("an empty (gutter click) result clears, unless additive", () => {
    const s0 = sel.click(sel.EMPTY, items, 2);
    expect(sel.marqueeMerge(s0, items, [], false).order).toEqual([]);
    expect(sel.marqueeMerge(s0, items, [], true).order).toEqual(["c"]);
  });
});
