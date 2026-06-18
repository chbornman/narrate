/**
 * Near-duplicate DISPLAY helpers (logic/dedup.ts): grouping order,
 * representative pick, summary copy, and the slider debounce. These are the
 * branchy bits of the Duplicates lens; the view itself is thin glue over them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  debounce,
  redundantHashes,
  representativeIndex,
  summaryCopy,
  toClusters,
} from "../src/lib/logic/dedup";
import type { DuplicateGroupDto } from "../src/lib/types/dto";

const group = (...hashes: string[]): DuplicateGroupDto => ({
  imageHashes: hashes,
  count: hashes.length,
});

describe("representativeIndex", () => {
  it("with no rating lookup keeps the first member (stable, backend-sorted)", () => {
    expect(representativeIndex(["a", "b", "c"])).toBe(0);
  });

  it("picks the highest-rated member", () => {
    const rating = (h: string) => ({ a: 2, b: 5, c: 3 })[h] ?? null;
    expect(representativeIndex(["a", "b", "c"], rating)).toBe(1);
  });

  it("breaks rating ties by first member (stable)", () => {
    const rating = (h: string) => ({ a: 4, b: 4 })[h] ?? null;
    expect(representativeIndex(["a", "b"], rating)).toBe(0);
  });

  it("treats a rating of 0 as a real rating, beating an unrated member", () => {
    // -Infinity floor: an explicit 0 must outrank "no rating known".
    const rating = (h: string) => ({ a: null, b: 0 })[h] ?? null;
    expect(representativeIndex(["a", "b"], rating)).toBe(1);
  });

  it("falls back to the first member when nothing is rated", () => {
    const rating = () => null;
    expect(representativeIndex(["a", "b"], rating)).toBe(0);
  });

  it("returns -1 for an empty member list", () => {
    expect(representativeIndex([])).toBe(-1);
  });
});

describe("toClusters", () => {
  it("orders clusters biggest-first (cluster order, not member order)", () => {
    const clusters = toClusters([group("b", "a"), group("e", "d", "c")]);
    expect(clusters.map((c) => c.count)).toEqual([3, 2]);
    // Members keep the backend's order (representative first, then the rest as
    // given) — toClusters orders the CLUSTERS, never re-sorts within one.
    expect(clusters[0].members).toEqual(["e", "d", "c"]);
    expect(clusters[1].members).toEqual(["b", "a"]);
  });

  it("breaks equal-size cluster ties by representative hash (deterministic)", () => {
    // Two size-2 groups; representatives are the first members "b" and "a"
    // (no ratings). Tie-break sorts by representative hash, so "a" comes first.
    const clusters = toClusters([group("b", "z"), group("a", "y")]);
    expect(clusters.map((c) => c.representative)).toEqual(["a", "b"]);
  });

  it("splits each group into representative + redundant rest", () => {
    const [c] = toClusters([group("a", "b", "c")]);
    expect(c.representative).toBe("a");
    expect(c.redundant).toEqual(["b", "c"]);
    expect(c.members).toEqual(["a", "b", "c"]);
  });

  it("honors the rating lookup for the representative and keeps the rest redundant", () => {
    const rating = (h: string) => ({ a: 1, b: 5, c: 2 })[h] ?? null;
    const [c] = toClusters([group("a", "b", "c")], rating);
    expect(c.representative).toBe("b");
    // Redundant preserves the remaining members in their original order.
    expect(c.redundant).toEqual(["a", "c"]);
    expect(c.members).toEqual(["b", "a", "c"]);
  });

  it("drops malformed sub-pair groups (a near-dup is always >= 2)", () => {
    expect(toClusters([group("a"), group()])).toEqual([]);
  });
});

describe("redundantHashes", () => {
  it("collects every non-representative hash across clusters in render order", () => {
    const clusters = toClusters([group("a", "b"), group("c", "d", "e")]);
    // Biggest first: c's redundant (d,e) then a's redundant (b).
    expect(redundantHashes(clusters)).toEqual(["d", "e", "b"]);
  });

  it("is empty when there are no clusters", () => {
    expect(redundantHashes([])).toEqual([]);
  });
});

describe("summaryCopy (no em-dashes; singular/plural)", () => {
  it("reads the calm none-state when empty", () => {
    expect(summaryCopy([])).toBe("No near-duplicates found");
  });

  it("uses singular for one group / one redundant copy", () => {
    expect(summaryCopy(toClusters([group("a", "b")]))).toBe(
      "1 near-duplicate group, 1 redundant copy",
    );
  });

  it("uses plural and counts redundant copies across groups", () => {
    const clusters = toClusters([group("a", "b", "c"), group("d", "e")]);
    // 2 groups; redundant = (3-1) + (2-1) = 3.
    expect(summaryCopy(clusters)).toBe(
      "2 near-duplicate groups, 3 redundant copies",
    );
  });

  it("never contains an em-dash (gate: check:emdash)", () => {
    const clusters = toClusters([group("a", "b")]);
    expect(summaryCopy(clusters)).not.toContain("—");
  });
});

describe("debounce", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("fires once, with the LAST arg, after the quiet window", () => {
    const fn = vi.fn();
    const d = debounce(fn, 100);
    d.run(1);
    d.run(2);
    d.run(3);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(3);
  });

  it("cancel() drops a pending fire", () => {
    const fn = vi.fn();
    const d = debounce(fn, 100);
    d.run(1);
    d.cancel();
    vi.advanceTimersByTime(100);
    expect(fn).not.toHaveBeenCalled();
  });
});
