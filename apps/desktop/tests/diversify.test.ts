/**
 * The pure Diversify / duplication-tolerance filter math (logic/diversify.ts).
 * The grid's shown-set filter, the "N hidden" count, and the tolerance <-> percent
 * mapping are the load-bearing seams of the feature: the grid renders exactly the
 * `shown` set, the header count must AGREE with it, and the slider's percent must
 * map to the backend's 0..1 tolerance without an off-by-100. Tested in isolation,
 * no Svelte, no IPC.
 */
import { describe, expect, it } from "vitest";
import {
  DEFAULT_TOLERANCE_PERCENT,
  filterToShown,
  hiddenCount,
  percentToTolerance,
  toleranceToPercent,
} from "../src/lib/logic/diversify";

const items = [
  { hash: "a", n: 1 },
  { hash: "b", n: 2 },
  { hash: "c", n: 3 },
  { hash: "d", n: 4 },
];

describe("filterToShown", () => {
  it("null (filter OFF) is the identity: every item passes unchanged", () => {
    // Turning Diversify off restores the full set with no special-case at the
    // call site — the identity is what "off" reduces to.
    expect(filterToShown(items, null)).toEqual(items);
  });

  it("keeps only items whose hash is in the shown set, preserving order", () => {
    const shown = new Set(["a", "c"]);
    expect(filterToShown(items, shown)).toEqual([
      { hash: "a", n: 1 },
      { hash: "c", n: 3 },
    ]);
  });

  it("an empty shown set hides everything (filter on, nothing representative)", () => {
    expect(filterToShown(items, new Set())).toEqual([]);
  });

  it("a shown hash not present in the items contributes nothing (no throw)", () => {
    // A shown set that drifted from the grid (e.g. an item left the scope) must
    // never inject a phantom row — only present items survive.
    expect(filterToShown(items, new Set(["a", "zzz"]))).toEqual([{ hash: "a", n: 1 }]);
  });
});

describe("hiddenCount", () => {
  it("null (filter off) hides nothing", () => {
    expect(hiddenCount(10, null)).toBe(0);
  });

  it("is scope size minus the shown set size", () => {
    expect(hiddenCount(10, new Set(["a", "b", "c"]))).toBe(7);
  });

  it("agrees with filterToShown: hidden = scope - shown.length", () => {
    const shown = new Set(["a", "c"]);
    const visible = filterToShown(items, shown);
    expect(hiddenCount(items.length, shown)).toBe(items.length - visible.length);
  });

  it("never goes negative when a stale shown set exceeds the current scope", () => {
    // Mid-relist the shown set can briefly carry more hashes than the (smaller)
    // current scope — the count must clamp at 0, not render "-2 hidden".
    expect(hiddenCount(2, new Set(["a", "b", "c", "d"]))).toBe(0);
  });
});

describe("percentToTolerance / toleranceToPercent", () => {
  it("maps 0..100% to the backend's 0..1 tolerance", () => {
    expect(percentToTolerance(0)).toBe(0);
    expect(percentToTolerance(50)).toBe(0.5);
    expect(percentToTolerance(100)).toBe(1);
  });

  it("clamps out-of-range percents to the nearest valid tolerance", () => {
    expect(percentToTolerance(-10)).toBe(0);
    expect(percentToTolerance(250)).toBe(1);
  });

  it("toleranceToPercent is the inverse, rounded to a whole percent", () => {
    expect(toleranceToPercent(0)).toBe(0);
    expect(toleranceToPercent(0.5)).toBe(50);
    expect(toleranceToPercent(1)).toBe(100);
    // A round-trip from the default percent is stable.
    expect(toleranceToPercent(percentToTolerance(DEFAULT_TOLERANCE_PERCENT))).toBe(
      DEFAULT_TOLERANCE_PERCENT,
    );
  });
});
