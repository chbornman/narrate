/**
 * Perf budgets (AUDIT 2026-07-07 T3b, guards F2): selection membership on
 * the grid's hot path. CONTRACT PINNED: `sel.isSelected` is O(1) per call
 * (a Set memoized on the order array's identity, logic/selection.ts) —
 * Grid.svelte asks it for EVERY mounted cell on EVERY scroll frame, so a
 * regression to the old linear `order.includes(hash)` turns
 * Select-All-then-scroll into O(selection x cells) per frame.
 *
 * CALIBRATION (2026-07-07, M1 Mac, node 25): this exact workload — 10k
 * selected, 150 cells x 120 frames = 18k membership tests — measured
 * ~0.7 ms with the Set memo and ~1360 ms with `.includes` (~1900x). The
 * 100 ms budget sits ~140x above the fixed path (generous for CI/jsdom
 * timer jitter) and ~13x below the regression, so it cannot flake green.
 */
import { describe, expect, it } from "vitest";
import * as sel from "../src/lib/logic/selection";

const N = 10_000;
const CELLS = 150;
const FRAMES = 120;
const BUDGET_MS = 100;

// Realistic hash shape: 64 hex chars, differing in the leading digits so a
// string compare cannot bail on the first character (the includes worst
// case the budget must catch is real, not synthetic).
const hashes = Array.from(
  { length: N },
  (_, i) => `${i.toString(16).padStart(8, "0")}${"ab".repeat(28)}`,
);

describe("selection membership budget (F2: per-cell per-frame hot path)", () => {
  it("Select-All 10k then 150 cells x 120 frames of isSelected stays under budget", () => {
    const s = sel.selectAll(sel.EMPTY, hashes);

    // Warm-up pass builds the memoized Set and JITs the path, like the
    // first rendered frame after Select-All would.
    sel.isSelected(s, hashes[0]);

    // The mounted window scrolls through the middle of the list — the
    // region where a linear scan pays ~N/2 compares per miss-free hit.
    const mid = N / 2;
    let hits = 0;
    const t0 = performance.now();
    for (let frame = 0; frame < FRAMES; frame++) {
      for (let cell = 0; cell < CELLS; cell++) {
        if (sel.isSelected(s, hashes[mid + cell])) hits++;
      }
    }
    const elapsed = performance.now() - t0;

    expect(hits).toBe(CELLS * FRAMES); // every probed cell IS selected
    expect(
      elapsed,
      `membership took ${elapsed.toFixed(1)}ms for ${CELLS * FRAMES} checks — O(selection) scan regression?`,
    ).toBeLessThan(BUDGET_MS);
  });

  it("memoization never goes stale: a REPLACED selection answers fresh", () => {
    // The memo is keyed on array identity, which is only sound because
    // selections are immutable-by-replacement — pin that the replacement
    // path yields correct answers (a stale-cache bug would surface here).
    const items = ["a", "b", "c", "d"];
    let s = sel.selectAll(sel.EMPTY, items);
    expect(sel.isSelected(s, "c")).toBe(true);
    s = sel.toggle(s, items, 2); // - c (fresh order array)
    expect(sel.isSelected(s, "c")).toBe(false);
    expect(sel.isSelected(s, "a")).toBe(true);
    s = sel.clear(s);
    expect(sel.isSelected(s, "a")).toBe(false);
    s = sel.marqueeMerge(s, items, [1, 3], false);
    expect(sel.isSelected(s, "b")).toBe(true);
    expect(sel.isSelected(s, "d")).toBe(true);
    expect(sel.isSelected(s, "a")).toBe(false);
  });
});
