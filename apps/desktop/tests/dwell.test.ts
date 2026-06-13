/** Dwell tracker (DESIGN-ATTENTION-HEATMAP.md): the pure focus-episode state
 * machine — begin/end, the min-episode floor, the multi-select fan-out, and the
 * same-focus dedupe. */
import { describe, expect, it } from "vitest";
import {
  MIN_EPISODE_MS,
  beginEpisode,
  endEpisode,
  sameFocus,
} from "../src/lib/logic/dwell";

describe("dwell episodes", () => {
  it("begins an episode for a non-empty focus, null for empty", () => {
    expect(beginEpisode("look", ["a"], 1000)).toEqual({
      source: "look",
      hashes: ["a"],
      startMs: 1000,
    });
    expect(beginEpisode("grid", [], 1000)).toBeNull();
  });

  it("ends an episode as one flush per focused hash (multi-select fan-out)", () => {
    const ep = beginEpisode("grid", ["a", "b", "c"], 0);
    const flushes = endEpisode(ep, 1000);
    expect(flushes).toEqual([
      { hash: "a", source: "grid", elapsedMs: 1000 },
      { hash: "b", source: "grid", elapsedMs: 1000 },
      { hash: "c", source: "grid", elapsedMs: 1000 },
    ]);
  });

  it("drops an episode below the MIN_EPISODE_MS floor", () => {
    const ep = beginEpisode("look", ["a"], 0);
    expect(endEpisode(ep, MIN_EPISODE_MS - 1)).toEqual([]);
    expect(endEpisode(ep, MIN_EPISODE_MS).length).toBe(1);
  });

  it("ending a null episode (or a backwards clock) flushes nothing", () => {
    expect(endEpisode(null, 1000)).toEqual([]);
    const ep = beginEpisode("look", ["a"], 5000);
    expect(endEpisode(ep, 4000)).toEqual([]); // clock ran backwards
  });

  it("sameFocus compares tier + hash SET (order-independent)", () => {
    const a = { source: "grid" as const, hashes: ["a", "b"] };
    expect(sameFocus(a, { source: "grid", hashes: ["b", "a"] })).toBe(true);
    expect(sameFocus(a, { source: "grid", hashes: ["a"] })).toBe(false);
    expect(sameFocus(a, { source: "look", hashes: ["a", "b"] })).toBe(false);
    expect(sameFocus(a, null)).toBe(false);
    expect(sameFocus(null, null)).toBe(true);
  });
});
