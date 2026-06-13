/**
 * T cell-info cycle (logic/cellinfo.ts, featureset §1): none → minimal →
 * annotated-state → none. The default level shows nothing — bare cells
 * stay the quiet baseline (UI §3.5); the levels are an explicit opt-in.
 */
import { describe, expect, it } from "vitest";
import {
  CELL_INFO_CYCLE,
  infoLine,
  infoStripHeight,
  nextLevel,
} from "../src/lib/logic/cellinfo";

describe("the T cycle", () => {
  it("walks none → minimal → annotated and wraps", () => {
    expect(nextLevel("none")).toBe("minimal");
    expect(nextLevel("minimal")).toBe("annotated");
    expect(nextLevel("annotated")).toBe("none");
    expect(CELL_INFO_CYCLE).toEqual(["none", "minimal", "annotated"]);
  });
});

describe("info-strip height (the cell grows by this — founder)", () => {
  it("none reserves no strip; minimal < annotated (one line vs two)", () => {
    expect(infoStripHeight("none")).toBe(0);
    expect(infoStripHeight("minimal")).toBeLessThan(infoStripHeight("annotated"));
    expect(infoStripHeight("minimal")).toBeGreaterThan(0);
  });
});

describe("per-level content", () => {
  const facts = { fileName: "IMG_4471.jpg", rating: 3, hasJournal: true };

  it("none renders nothing (the quiet baseline)", () => {
    expect(infoLine("none", facts)).toEqual({ name: null, state: null });
  });

  it("minimal renders the filename only", () => {
    expect(infoLine("minimal", facts)).toEqual({ name: "IMG_4471.jpg", state: null });
  });

  it("annotated renders filename + rating fold + journal presence", () => {
    expect(infoLine("annotated", facts)).toEqual({
      name: "IMG_4471.jpg",
      state: "★ 3 · journaled",
    });
  });

  it("annotated states cover the unrated/unjournaled corners", () => {
    expect(
      infoLine("annotated", { fileName: "a.jpg", rating: null, hasJournal: false }).state,
    ).toBe("unrated");
    expect(
      infoLine("annotated", { fileName: "a.jpg", rating: null, hasJournal: true }).state,
    ).toBe("unrated · journaled");
    expect(
      infoLine("annotated", { fileName: "a.jpg", rating: 0, hasJournal: false }).state,
    ).toBe("★ 0"); // an explicit clear is a rating fold, not "unrated"
  });
});
