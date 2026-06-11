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

  it("the pencil and mic seats exist; mic renders from micState (P6.1 fills the M2b seat)", () => {
    // Amended by P6.1: the mic seat reserved since P4.2 is now live —
    // it renders CAPTURE §6.4's state machine via ctx.micState, no longer
    // the boolean placeholder.
    expect(MODES.map((m) => m.id)).toEqual(["auto-advance", "pencil", "mic"]);
    const segs = segments(base);
    expect(segs.some((s) => s.id === "mode:pencil" || s.id === "mode:mic")).toBe(false);
    const lit = segments({ ...base, ctx: { ...ctx(), micState: "armedIdle" } });
    expect(lit.some((s) => s.id === "mode:mic")).toBe(true);
  });
});

describe("the mic segment renders CAPTURE §6.4/§11 (P6.1)", () => {
  const micSeg = (over: Partial<ReturnType<typeof ctx>>) =>
    segments({ ...base, ctx: { ...ctx(), ...over } }).find((s) => s.id === "mode:mic");

  it("absent until ASR exists; dimmed glyph when disarmed-but-ready (UI §7.3)", () => {
    expect(micSeg({ micState: "disarmed", asrReady: false })).toBeUndefined();
    const dimmed = micSeg({ micState: "disarmed", asrReady: true });
    expect(dimmed?.tone).toBe("dim");
  });

  it("armed = solid glyph with the one-line privacy claim; speaking breathes", () => {
    const idle = micSeg({ micState: "armedIdle" });
    expect(idle?.tone).toBeUndefined();
    expect(idle?.title).toBe(
      "Listening — audio is transcribed on this device and never written to disk.",
    );
    const speaking = micSeg({ micState: "armedSpeaking" });
    expect(speaking?.tone).toBe("live");
  });

  it("degraded = the muted-mic glyph + the §7.3 hover line; never a toast seat", () => {
    const degraded = micSeg({ micState: "disarmedError", asrUnavailable: true });
    expect(degraded?.tone).toBe("dim");
    expect(degraded?.title).toBe(
      "Voice capture unavailable — typed notes and pencil still work.",
    );
    // No text content ever rides the indicator (§11): the segment carries
    // a glyph and a title, nothing transcript-shaped.
    expect(degraded?.text.length).toBeLessThan(4);
  });
});

describe("the streaming-utterance tether (CAPTURE §5.4/§11, UI §7.3)", () => {
  it("a bound scope differing from the live selection tethers the scope segment", () => {
    const segs = segments({
      ...base,
      scope: { kind: "single", count: 1 }, // selection is now B
      streaming: { kind: "single", count: 1, differs: true }, // words land on A
    });
    const scope = segs.find((s) => s.id === "scope");
    expect(scope?.text).toBe("● 1 ⇠");
    expect(scope?.title).toContain("Words in flight land on the earlier selection");
    expect(scope?.pulse).toBe(true);
  });

  it("same-scope streaming and no streaming render the plain scope", () => {
    for (const streaming of [null, { kind: "single", count: 1, differs: false }]) {
      const segs = segments({
        ...base,
        scope: { kind: "single", count: 1 },
        streaming,
      });
      expect(segs.find((s) => s.id === "scope")?.text).toBe("● 1");
    }
  });

  it("a multi-bound utterance tethers with its own count, not the live one", () => {
    const segs = segments({
      ...base,
      scope: { kind: "session", count: 0 },
      streaming: { kind: "multi", count: 3, differs: true },
    });
    expect(segs.find((s) => s.id === "scope")?.text).toBe("● 3 ⇠");
  });
});
