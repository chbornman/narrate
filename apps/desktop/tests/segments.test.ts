/**
 * The segmented indicator model (logic/segments.ts — featureset §4):
 * fixed ordering — ingest hairline · scope (the pulse target) · n-of-m ·
 * mode segments — with the mic and query-residue seats reserved by
 * construction. "Modes are visible" (§0) is asserted here.
 */
import { describe, expect, it } from "vitest";
import { segments, type SegmentInput } from "../src/lib/logic/segments";
import { MODES } from "../src/lib/actions/modes";
import { withDefaults } from "../src/lib/logic/keymap";

const ctx = (over: Partial<Parameters<typeof withDefaults>[0]> = {}) =>
  withDefaults({
    surface: "grid",
    searchOpen: false,
    inputFocused: false,
    searchInputFocused: false,
    hasSelection: false,
    railOpen: false,
    debugEnabled: false,
    asrReady: false,
    ...over,
  });

const base: SegmentInput = {
  ingest: { running: false, done: 0, total: 0 },
  scope: { kind: "session", count: 0 },
  lookPosition: null,
  ctx: ctx(),
};

describe("ordering and seats", () => {
  it("quiet default: just the scope segment (● session; ● 0 never renders)", () => {
    const segs = segments(base);
    expect(segs.map((s) => s.id)).toEqual(["scope"]);
    expect(segs[0].text).toBe("● session");
    expect(segs[0].pulse).toBe(true);
  });

  it("full house keeps the fixed order: ingest · scope · n-of-m · modes", () => {
    const segs = segments({
      ingest: { running: true, done: 50, total: 200 },
      scope: { kind: "multi", count: 3 },
      lookPosition: { index: 4, total: 12 },
      ctx: ctx({ autoAdvance: true }),
    });
    expect(segs.map((s) => s.id)).toEqual([
      "ingest",
      "scope",
      "position",
      "mode:auto-advance",
    ]);
  });

  it("the ingest hairline carries a 0..1 fraction and the hover copy", () => {
    const segs = segments({
      ...base,
      ingest: { running: true, done: 12402, total: 48377 },
    });
    const hairline = segs[0];
    expect(hairline.id).toBe("ingest");
    expect(hairline.fraction).toBeCloseTo(12402 / 48377);
    expect(hairline.title).toContain("12,402");
    expect(hairline.title).toContain("48,377");
  });

  it("n-of-m renders 1-based in Look", () => {
    const segs = segments({ ...base, lookPosition: { index: 0, total: 8 } });
    expect(segs.find((s) => s.id === "position")?.text).toBe("1 of 8");
  });

  it("a collapsed pair reads ● 2 — target truth, not cell count", () => {
    const segs = segments({ ...base, scope: { kind: "multi", count: 2 } });
    expect(segs.find((s) => s.id === "scope")?.text).toBe("● 2");
  });
});

describe("modes are visible (featureset §0) — by construction", () => {
  it("auto-advance ON lights its segment; OFF leaves no trace", () => {
    const off = segments({ ...base, ctx: ctx({ autoAdvance: false }) });
    expect(off.some((s) => s.id === "mode:auto-advance")).toBe(false);
    const onSegs = segments({ ...base, ctx: ctx({ autoAdvance: true }) });
    expect(onSegs.some((s) => s.id === "mode:auto-advance")).toBe(true);
  });

  it("the pencil and mic ModeDefs are RESERVED now (M2a/M2b ids exist, falsy ctx)", () => {
    expect(MODES.map((m) => m.id)).toEqual(["auto-advance", "pencil", "mic"]);
    // Their ctx fields are always false in P4.2, so no segment renders…
    const segs = segments(base);
    expect(segs.some((s) => s.id === "mode:pencil" || s.id === "mode:mic")).toBe(false);
    // …but the seat exists: a truthy ctx field lights it with zero new code.
    const lit = segments({ ...base, ctx: { ...ctx(), micArmed: true } });
    expect(lit.some((s) => s.id === "mode:mic")).toBe(true);
  });
});
