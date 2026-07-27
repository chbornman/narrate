/**
 * A26/T4 headless scripted-scroll receipt.
 *
 * This is deliberately a request-accounting harness rather than another
 * layout unit test: it drives a 50k-item library through a sequence of large
 * fling jumps, records every newly mounted thumbnail as a request fired, and
 * records the final viewport as cells settled. The bound is derived from the
 * real virtualizer pool, so a render-everything or unbounded-overscan
 * regression fails with useful counts.
 */
import { describe, expect, it } from "vitest";
import * as layout from "../src/lib/logic/gridlayout";

const ITEMS = 50_000;
const VIEWPORT_W = 1_440;
const VIEWPORT_H = 900;
const FRAMES = 60;
const geometry = layout.snap(VIEWPORT_W, 160, 8, 10);

interface FlingReceipt {
  frames: number;
  poolCells: number;
  requestsFired: number;
  cellsSettled: number;
  peakMounted: number;
}

function scriptedFling(): FlingReceipt {
  const poolCells = layout.poolSize(geometry, VIEWPORT_H);
  const total = layout.totalHeight(geometry, ITEMS);
  let requestsFired = 0;
  let peakMounted = 0;
  let previous = new Set<number>();
  let final = new Set<number>();

  for (let frame = 0; frame < FRAMES; frame++) {
    // Ease-out curve: early frames make large jumps, later frames converge on
    // the final viewport like a real trackpad fling.
    const progress = 1 - (1 - frame / (FRAMES - 1)) ** 3;
    const top = progress * Math.max(0, total - VIEWPORT_H);
    const range = layout.visibleRange(geometry, top, VIEWPORT_H, ITEMS);
    const mounted = new Set<number>();
    for (let index = range.start; index < range.end; index++) mounted.add(index);
    for (const index of mounted) {
      if (!previous.has(index)) requestsFired++;
    }
    peakMounted = Math.max(peakMounted, mounted.size);
    previous = mounted;
    final = mounted;
  }

  return {
    frames: FRAMES,
    poolCells,
    requestsFired,
    cellsSettled: final.size,
    peakMounted,
  };
}

describe("scripted grid fling request budget", () => {
  it("reports bounded requests and a fully settled final window", () => {
    const receipt = scriptedFling();

    expect(receipt.peakMounted).toBeLessThanOrEqual(receipt.poolCells);
    expect(receipt.cellsSettled).toBeGreaterThan(0);
    expect(receipt.cellsSettled).toBeLessThanOrEqual(receipt.poolCells);
    // At most one new URL per pool seat per synthetic scroll frame. This is
    // intentionally a hard numeric ceiling, not a timing assertion.
    expect(receipt.requestsFired).toBeLessThanOrEqual(
      receipt.frames * receipt.poolCells,
    );
    // A fast trip through a 50k library must not request the whole library.
    expect(receipt.requestsFired).toBeLessThan(ITEMS / 2);
  });
});
