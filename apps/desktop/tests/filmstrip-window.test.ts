/**
 * Filmstrip virtual-window math (logic/filmstrip.ts) — born from the
 * June 12 2026 dogfood: the fixed 17-neighbor window capped the strip
 * and left a squished panel half-empty. The strip now scrolls the whole
 * order; these pin the window/spacer arithmetic and the follow rules.
 */
import { describe, expect, it } from "vitest";
import { centerOn, stripWindow, STRIP_OVERSCAN } from "../src/lib/logic/filmstrip";

const PITCH = 72; // 68px cell + 4px gap

describe("stripWindow", () => {
  it("renders enough cells to FILL the viewport plus overscan — never a fixed cap", () => {
    // A wide strip with a small cell: the 17-cell era would starve this.
    const w = stripWindow(0, 1400, PITCH, 10_000);
    expect(w.first).toBe(0);
    expect(w.count).toBe(Math.ceil(1400 / PITCH) + STRIP_OVERSCAN * 2);
    expect(w.count).toBeGreaterThan(17);
    expect(w.leadPx).toBe(0);
  });

  it("spacers stand in for every unrendered cell, so the scrollbar spans the set", () => {
    const total = 5_000;
    const w = stripWindow(50 * PITCH, 720, PITCH, total);
    expect(w.first).toBe(50 - STRIP_OVERSCAN);
    expect(w.leadPx).toBe(w.first * PITCH);
    expect(w.leadPx + w.count * PITCH + w.tailPx).toBe(total * PITCH);
  });

  it("clamps at both ends of the order", () => {
    const head = stripWindow(0, 720, PITCH, 5);
    expect(head).toMatchObject({ first: 0, count: 5, leadPx: 0, tailPx: 0 });
    const tail = stripWindow(1_000 * PITCH, 720, PITCH, 1_000);
    expect(tail.first + tail.count).toBe(1_000);
    expect(tail.tailPx).toBe(0);
  });

  it("degenerate inputs (no images, unmeasured panel) stay inert", () => {
    expect(stripWindow(0, 0, PITCH, 100).leadPx).toBe(0);
    expect(stripWindow(0, 720, 0, 100).count).toBe(100);
    expect(stripWindow(0, 720, PITCH, 0).count).toBe(0);
  });
});

describe("centerOn (the snap-back target)", () => {
  it("clamps to the scroll range at both edges", () => {
    expect(centerOn(0, 720, PITCH, 1_000)).toBe(0);
    const max = 1_000 * PITCH - 720;
    expect(centerOn(999, 720, PITCH, 1_000)).toBe(max);
    // Mid-strip: the cell's center sits at the viewport's center.
    const mid = centerOn(500, 720, PITCH, 1_000);
    expect(mid + 720 / 2).toBeCloseTo(500 * PITCH + PITCH / 2);
  });

  it("a strip narrower than the viewport never scrolls", () => {
    expect(centerOn(4, 720, PITCH, 5)).toBe(0);
  });
});
