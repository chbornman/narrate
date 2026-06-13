/**
 * Pure flyout placement math (primitives/flyout.ts): submenu panels prefer
 * the parent row's right edge, flip left only when right clips AND left
 * fully fits, otherwise clamp on-screen; top-aligns with the parent row and
 * clamps up on bottom overflow. Mirrors panel.test.ts style.
 */
import { describe, expect, it } from "vitest";
import { flyoutPos, type Rect, type Viewport } from "../src/lib/primitives/flyout";

const GUTTER = 4;
const MARGIN = 8;
const vp: Viewport = { width: 1000, height: 800 };

/** A parent submenu-row rect at (left, top) sized 200x28 (a menu row). */
function row(left: number, top: number, width = 200, height = 28): Rect {
  return { left, top, right: left + width, bottom: top + height, width, height };
}

describe("flyoutPos — edge-aware side placement", () => {
  it("opens to the RIGHT by default, overlapping the parent by GUTTER", () => {
    const p = row(300, 100);
    const pos = flyoutPos(p, { width: 200, height: 240 }, vp);
    expect(pos.side).toBe("right");
    expect(pos.left).toBe(p.right - GUTTER); // 500 - 4
    expect(pos.top).toBe(100); // top-aligned
  });

  it("flips LEFT near the right edge when the left side fully fits", () => {
    // Parent near the right edge: right-flown child would clip; its left
    // (parent.left - child.width) clears MARGIN, so it flips.
    const p = row(700, 100); // right = 900
    const child = { width: 220, height: 240 };
    // right: 900 - 4 + 220 + 8 = 1124 > 1000 → overflow; left fits: 700-220-8=472>=0
    const pos = flyoutPos(p, child, vp);
    expect(pos.side).toBe("left");
    expect(pos.left).toBe(700 - 220 + GUTTER); // parent.left - child.width + GUTTER
  });

  it("stays RIGHT and clamps on-screen when NEITHER side fully fits", () => {
    // Pathological: a child wider than fits on either side. Right overflows
    // and left does NOT fit, so it stays right and clamps flush to MARGIN.
    const p = row(40, 100); // left small → left side cannot fit a wide child
    const child = { width: 980, height: 240 };
    // right: 240-4 + 980 + 8 = 1224 > 1000 overflow; left fits? 40-980-8<0 → no
    const pos = flyoutPos(p, child, vp);
    expect(pos.side).toBe("right");
    expect(pos.left).toBe(vp.width - child.width - MARGIN); // 1000-980-8 = 12
    expect(pos.left).toBeGreaterThanOrEqual(0); // never off-screen
  });

  it("clamps the TOP up when the panel overflows the bottom", () => {
    const p = row(300, 700); // near the bottom
    const child = { width: 200, height: 240 };
    // top: 700 + 240 + 8 = 948 > 800 → clamp to 800-240-8 = 552
    const pos = flyoutPos(p, child, vp);
    expect(pos.top).toBe(vp.height - child.height - MARGIN);
  });

  it("never clamps the top above MARGIN even for an over-tall panel", () => {
    const p = row(300, 50);
    const child = { width: 200, height: 900 }; // taller than the viewport
    const pos = flyoutPos(p, child, vp);
    expect(pos.top).toBe(MARGIN); // max(MARGIN, 800-900-8) = max(8, -108)
  });

  it("top-aligns with the parent row when the panel fits below", () => {
    const p = row(300, 120);
    const pos = flyoutPos(p, { width: 200, height: 200 }, vp);
    expect(pos.top).toBe(120);
  });
});
