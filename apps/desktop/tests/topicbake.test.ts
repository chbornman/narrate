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
