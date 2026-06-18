/**
 * The pure threshold -> selected-set / ranked-mapping math
 * (logic/topicbake.ts). This is the load-bearing bit of the signature bake
 * gesture: the graph's glow set, the live "N photos" count, and the bake's
 * membership all reduce to these functions, so they MUST agree. Tested in
 * isolation, no UI.
 */
import { describe, expect, it } from "vitest";
import {
  countAboveThreshold,
  rankedHashes,
  rankedToScored,
  resolveTopicIndex,
  selectedAboveThreshold,
  type ScoredImage,
} from "../src/lib/logic/topicbake";
import type { RankedImageDto } from "../src/lib/types/dto";

const scored: ScoredImage[] = [
  { hash: "a", score: 0.9 },
  { hash: "b", score: 0.6 },
  { hash: "c", score: 0.5 },
  { hash: "d", score: 0.2 },
];

describe("selectedAboveThreshold", () => {
  it("returns the hashes at or above the threshold, preserving order", () => {
    expect(selectedAboveThreshold(scored, 0.5)).toEqual(["a", "b", "c"]);
  });

  it("is INCLUSIVE at the boundary (a slider parked on a score grabs it)", () => {
    // c sits exactly at 0.5 -> included.
    expect(selectedAboveThreshold(scored, 0.5)).toContain("c");
    // Nudging just above the boundary drops it.
    expect(selectedAboveThreshold(scored, 0.51)).toEqual(["a", "b"]);
  });

  it("threshold 0 takes everything; threshold 1 takes nothing here", () => {
    expect(selectedAboveThreshold(scored, 0)).toEqual(["a", "b", "c", "d"]);
    expect(selectedAboveThreshold(scored, 1)).toEqual([]);
  });

  it("empty input is an empty selection", () => {
    expect(selectedAboveThreshold([], 0.5)).toEqual([]);
  });
});

describe("countAboveThreshold", () => {
  it("counts the same set selectedAboveThreshold returns (they must agree)", () => {
    for (const t of [0, 0.2, 0.5, 0.51, 0.9, 1]) {
      expect(countAboveThreshold(scored, t)).toBe(
        selectedAboveThreshold(scored, t).length,
      );
    }
  });
});

describe("rankedToScored / rankedHashes", () => {
  const ranked: RankedImageDto[] = [
    { hash: "x", score: 0.8 },
    { hash: "y", score: 0.4 },
  ];

  it("rankedToScored maps to ScoredImage preserving the (descending) order", () => {
    expect(rankedToScored(ranked)).toEqual([
      { hash: "x", score: 0.8 },
      { hash: "y", score: 0.4 },
    ]);
  });

  it("rankedHashes is the hashes in ranked order (the grid feed order)", () => {
    expect(rankedHashes(ranked)).toEqual(["x", "y"]);
  });
});

describe("resolveTopicIndex (identity-not-index for the bake selection)", () => {
  // The graph's `topics` is reverse-sorted from the store; the bake panel tracks
  // the selected topic by PHRASE and re-derives its index every frame via this.
  // These tests pin the §6b "selectedTopic off-by-one" fix: the selection follows
  // its identity through removals/reorders instead of dangling on a shifted index.
  const topics = ["alpha", "bravo", "charlie", "delta"];

  it("resolves a phrase to its current position", () => {
    expect(resolveTopicIndex(topics, "charlie")).toBe(2);
  });

  it("null selection resolves to -1 (no glow, no panel)", () => {
    expect(resolveTopicIndex(topics, null)).toBe(-1);
  });

  it("a phrase no longer present resolves to -1 (self-healing on removal)", () => {
    expect(resolveTopicIndex(topics, "echo")).toBe(-1);
  });

  it("THE BUG: selecting B then removing an EARLIER topic A keeps B selected, not B's old neighbor", () => {
    // Select "charlie" (index 2). The old index-based code stored 2.
    const selected = "charlie";
    const before = resolveTopicIndex(topics, selected);
    expect(before).toBe(2);

    // Remove an EARLIER topic ("alpha"). Later phrases shift DOWN by one. The old
    // raw index 2 would now name "delta" (the wrong topic) for the in-flight frame;
    // identity tracking re-derives "charlie" to its NEW index 1.
    const afterRemoval = topics.filter((t) => t !== "alpha"); // ["bravo","charlie","delta"]
    const after = resolveTopicIndex(afterRemoval, selected);
    expect(after).toBe(1);
    expect(afterRemoval[after]).toBe("charlie"); // still B, NOT "delta"
    // Prove the old behavior was wrong: the stale raw index would have lit "delta".
    expect(afterRemoval[before]).toBe("delta");
  });

  it("removing the SELECTED topic itself resolves to -1 (panel drops cleanly)", () => {
    const afterRemoval = topics.filter((t) => t !== "charlie");
    expect(resolveTopicIndex(afterRemoval, "charlie")).toBe(-1);
  });
});
