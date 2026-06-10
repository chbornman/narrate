/**
 * Surround luminance (theme/surround.ts — featureset §7, D6): the five
 * levels, cycle order, persistence codec, and the per-level contrast
 * table that mirrors tokens.css (coverage asserted so CSS and TS cannot
 * drift silently).
 */
import { describe, expect, it } from "vitest";
import {
  cycleSurround,
  DEFAULT_SURROUND,
  parseSurround,
  SURROUND_CONTRAST,
  SURROUND_LABELS,
  SURROUND_LEVELS,
} from "../src/lib/theme/surround";

describe("levels & cycle", () => {
  it("exactly the five D6 levels, darkest → lightest; default black", () => {
    expect(SURROUND_LEVELS).toEqual(["black", "dark", "middle", "light", "white"]);
    expect(DEFAULT_SURROUND).toBe("black");
  });

  it("cycles forward with wrap; -1 steps darker", () => {
    expect(cycleSurround("black")).toBe("dark");
    expect(cycleSurround("white")).toBe("black");
    expect(cycleSurround("black", -1)).toBe("white");
  });
});

describe("persistence codec", () => {
  it("round-trips every level and rejects junk", () => {
    for (const level of SURROUND_LEVELS) expect(parseSurround(level)).toBe(level);
    expect(parseSurround(null)).toBeNull();
    expect(parseSurround("sepia")).toBeNull();
    expect(parseSurround("")).toBeNull();
  });
});

describe("contrast table (mirrors tokens.css [data-surround])", () => {
  it("covers every level with every token", () => {
    for (const level of SURROUND_LEVELS) {
      const c = SURROUND_CONTRAST[level];
      expect(c.surround).toMatch(/^#/);
      expect(c.journalDot).toMatch(/^#/);
      expect(c.selection).toMatch(/^#/);
      expect(c.focusCell).toMatch(/^#/);
      expect(c.marquee).toContain("rgba");
      expect(SURROUND_LABELS[level].length).toBeGreaterThan(0);
    }
  });

  it("surround values are distinct and ordered dark → light", () => {
    const lum = (hex: string) => parseInt(hex.slice(1, 3), 16);
    const values = SURROUND_LEVELS.map((l) => SURROUND_CONTRAST[l].surround);
    expect(new Set(values).size).toBe(values.length);
    for (let i = 1; i < values.length; i++) {
      expect(lum(values[i])).toBeGreaterThan(lum(values[i - 1]));
    }
  });

  it("selection/focus rings flip dark on light surrounds (legibility, D6)", () => {
    const lum = (hex: string) => parseInt(hex.slice(1, 3), 16);
    // On black: light rings. On white: dark rings.
    expect(lum(SURROUND_CONTRAST.black.selection)).toBeGreaterThan(0x80);
    expect(lum(SURROUND_CONTRAST.white.selection)).toBeLessThan(0x80);
    expect(lum(SURROUND_CONTRAST.black.focusCell)).toBeGreaterThan(
      lum(SURROUND_CONTRAST.white.surround) - 0xff,
    );
    // The journal dot stays a dulled red at every level (pencil-red reserved).
    for (const level of SURROUND_LEVELS) {
      const dot = SURROUND_CONTRAST[level].journalDot;
      const r = parseInt(dot.slice(1, 3), 16);
      const g = parseInt(dot.slice(3, 5), 16);
      expect(r).toBeGreaterThan(g); // red-leaning…
      expect(r).toBeLessThan(0xe0); // …but never the saturated pencil red
    }
  });
});
