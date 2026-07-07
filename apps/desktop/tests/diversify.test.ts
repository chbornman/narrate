/**
 * The pure Diversify / duplication-tolerance filter math (logic/diversify.ts).
 * The grid's fold filter and the tolerance <-> percent mapping are the
 * load-bearing seams of the feature: the grid drops exactly the report's
 * `hidden` set (so a hash the pass never saw stays VISIBLE — the U1 mid-ingest
 * honesty), and the slider's percent must map to the backend's 0..1 tolerance
 * without an off-by-100. Tested in isolation, no Svelte, no IPC.
 */
import { describe, expect, it } from "vitest";
import {
  DEFAULT_TOLERANCE_PERCENT,
  filterDiversify,
  percentToTolerance,
  toleranceToPercent,
} from "../src/lib/logic/diversify";

const items = [
  { hash: "a", n: 1 },
  { hash: "b", n: 2 },
  { hash: "c", n: 3 },
  { hash: "d", n: 4 },
];

describe("filterDiversify", () => {
  it("null (filter OFF) is the identity: every item passes unchanged", () => {
    // Turning Diversify off restores the full set with no special-case at the
    // call site — the identity is what "off" reduces to.
    expect(filterDiversify(items, null)).toEqual(items);
  });

  it("drops exactly the folded hashes, preserving input order", () => {
    const hidden = new Set(["b", "d"]);
    expect(filterDiversify(items, hidden)).toEqual([
      { hash: "a", n: 1 },
      { hash: "c", n: 3 },
    ]);
  });

  it("an empty folded set shows everything (filter on, nothing redundant)", () => {
    expect(filterDiversify(items, new Set())).toEqual(items);
  });

  it("a hash the pass never saw PASSES THROUGH (mid-ingest honesty, U1)", () => {
    // The last pass scanned a,b,c and folded c; a re-list then introduced d.
    // d was never seen by the pass, so it must render — hiding new photos
    // until the next pass lands is the AUDIT-2026-07-07 U1 failure mode.
    const hidden = new Set(["c"]);
    expect(filterDiversify(items, hidden).map((i) => i.hash)).toEqual([
      "a",
      "b",
      "d",
    ]);
  });

  it("a folded hash that already left the scope drops nothing extra (no throw)", () => {
    // The inverse drift: an item the pass folded vanished from the scope
    // before the next re-list — the filter just has nothing to drop for it.
    expect(filterDiversify(items, new Set(["a", "zzz"]))).toEqual([
      { hash: "b", n: 2 },
      { hash: "c", n: 3 },
      { hash: "d", n: 4 },
    ]);
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
