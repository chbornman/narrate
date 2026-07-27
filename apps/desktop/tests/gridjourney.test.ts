import { describe, expect, it } from "vitest";
import { GridJourneyTracker } from "../src/lib/logic/gridjourney";

describe("GridJourneyTracker", () => {
  it("records first visible paint and full viewport settle without identities", () => {
    const tracker = new GridJourneyTracker();
    const generation = tracker.begin(["a", "b", "c"], 100);

    expect(tracker.loaded("outside", generation, 110)).toEqual([]);
    expect(tracker.loaded("b", generation, 112)).toEqual([
      {
        phase: "first-paint",
        durationMs: 12,
        ok: true,
        itemCount: 3,
      },
    ]);
    expect(tracker.loaded("b", generation, 115)).toEqual([]);
    expect(tracker.loaded("a", generation, 118)).toEqual([]);
    expect(tracker.loaded("c", generation, 125)).toEqual([
      {
        phase: "settle",
        durationMs: 25,
        ok: true,
        itemCount: 3,
      },
    ]);
  });

  it("supersedes stale viewports and records an honest timeout", () => {
    const tracker = new GridJourneyTracker();
    const stale = tracker.begin(["old"], 10);
    const current = tracker.begin(["new-a", "new-b"], 20);

    expect(tracker.loaded("old", stale, 25)).toEqual([]);
    expect(tracker.timeout(current, 220)).toEqual([
      {
        phase: "settle",
        durationMs: 200,
        ok: false,
        itemCount: 2,
      },
    ]);
    expect(tracker.loaded("new-a", current, 230)).toEqual([]);
  });

  it("does not manufacture samples for an empty viewport", () => {
    const tracker = new GridJourneyTracker();
    const generation = tracker.begin([], 10);
    expect(tracker.loaded("a", generation, 20)).toEqual([]);
    expect(tracker.timeout(generation, 30)).toEqual([]);
  });
});
