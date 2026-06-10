/**
 * Auto-advance decision (logic/advance.ts — featureset §4, D7): rating
 * advances; a note advances only when entered from Look; multi-select
 * rating NEVER advances; OFF means off.
 */
import { describe, expect, it } from "vitest";
import { afterCommit } from "../src/lib/logic/advance";

const on = { autoAdvance: true } as const;

describe("afterCommit", () => {
  it("OFF (the D7 default): never advances", () => {
    expect(
      afterCommit({ autoAdvance: false, surface: "look", commit: "rating", selectionCount: 1 }),
    ).toBeNull();
    expect(
      afterCommit({ autoAdvance: false, surface: "grid", commit: "note", selectionCount: 0 }),
    ).toBeNull();
  });

  it("a rating in Look advances within the navigation set", () => {
    expect(
      afterCommit({ ...on, surface: "look", commit: "rating", selectionCount: 1 }),
    ).toBe("look-next");
  });

  it("a single-selection rating in Grid advances the active image", () => {
    expect(
      afterCommit({ ...on, surface: "grid", commit: "rating", selectionCount: 1 }),
    ).toBe("grid-next");
  });

  it("a multi-select rating NEVER advances (selection preserved)", () => {
    expect(
      afterCommit({ ...on, surface: "grid", commit: "rating", selectionCount: 3 }),
    ).toBeNull();
    expect(
      afterCommit({ ...on, surface: "look", commit: "rating", selectionCount: 2 }),
    ).toBeNull();
  });

  it("a note advances only when entered from Look", () => {
    expect(afterCommit({ ...on, surface: "look", commit: "note", selectionCount: 1 })).toBe(
      "look-next",
    );
    expect(
      afterCommit({ ...on, surface: "grid", commit: "note", selectionCount: 1 }),
    ).toBeNull();
  });
});
