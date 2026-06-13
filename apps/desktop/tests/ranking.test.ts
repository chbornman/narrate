/**
 * Ranking-signal toggles (search-as-scope Phase 3): the pure on/off ->
 * FusionWeights mapping the ⚙ popover ships. Founder decision D2 — on/off
 * checkboxes first; continuous sliders are Phase 4 — so the only mapping under
 * test is "checked = B75 default weight, unchecked = 0.0 (excluded)".
 */
import { describe, expect, it } from "vitest";
import {
  defaultToggles,
  isDefault,
  signalHint,
  SIGNALS,
  togglesToWeights,
} from "../src/lib/logic/ranking";

describe("ranking signal toggles -> FusionWeights", () => {
  it("defaults to all signals on (the B75 defaults)", () => {
    const t = defaultToggles();
    expect(t).toEqual({ s1: true, s2: true, s3_each: true, s4: true });
    expect(isDefault(t)).toBe(true);
  });

  it("all-on maps to the B75 default weights (1.0 / 1.0 / 0.5 / 1.0)", () => {
    expect(togglesToWeights(defaultToggles())).toEqual({
      s1: 1.0,
      s2: 1.0,
      s3_each: 0.5,
      s4: 1.0,
    });
  });

  it("an UNCHECKED signal maps to weight 0 (excluded from the fusion)", () => {
    // S4 (visual match) off: its weight is 0.0, the rest hold their defaults.
    const w = togglesToWeights({ s1: true, s2: true, s3_each: true, s4: false });
    expect(w.s4).toBe(0.0);
    expect(w.s1).toBe(1.0);
    expect(w.s2).toBe(1.0);
    expect(w.s3_each).toBe(0.5);
  });

  it("every signal can be excluded independently", () => {
    for (const sig of SIGNALS) {
      const off = { ...defaultToggles(), [sig.key]: false };
      expect(togglesToWeights(off)[sig.key]).toBe(0.0);
      expect(isDefault(off)).toBe(false);
    }
  });

  it("all-off maps every weight to 0", () => {
    const w = togglesToWeights({ s1: false, s2: false, s3_each: false, s4: false });
    expect(w).toEqual({ s1: 0.0, s2: 0.0, s3_each: 0.0, s4: 0.0 });
  });
});

describe("signalHint (the per-cell provenance breakdown)", () => {
  it("names the contributing signals as short codes, in S1..S4 order, de-duped", () => {
    // Backend formats the signal as its Rust enum name; S3 has two sub-lists.
    const per: [string, number | null, number][] = [
      ["S4ImageClip", 1, 0.9],
      ["S1AnnotationChunk", 2, 0.8],
      ["S3Summaries", 3, 0.4],
      ["S3Summaries", 5, 0.3],
    ];
    expect(signalHint(per)).toBe("S1 S3 S4");
  });

  it("is empty for no contributions", () => {
    expect(signalHint([])).toBe("");
  });
});
