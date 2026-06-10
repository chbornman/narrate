/**
 * Pure side-panel math (primitives/panel.ts): clamp, resize-drag handed
 * by side, and width persistence (the Panel primitive's persistKey
 * storage — shared with prefs RAIL_WIDTH_KEY/INSPECTOR_WIDTH_KEY).
 */
import { beforeEach, describe, expect, it } from "vitest";
import { clampSize, dragSize, loadSize, saveSize } from "../src/lib/primitives/panel";

beforeEach(() => {
  localStorage.clear();
});

describe("clamp & drag", () => {
  it("clamps into [min, max] and rounds", () => {
    expect(clampSize(199.6, 160, 420)).toBe(200);
    expect(clampSize(100, 160, 420)).toBe(160);
    expect(clampSize(999, 160, 420)).toBe(420);
  });

  it("a LEFT panel grows as the pointer moves right", () => {
    const start = { size: 220, x: 500 };
    expect(dragSize(start, 560, "left", 160, 420)).toBe(280);
    expect(dragSize(start, 440, "left", 160, 420)).toBe(160); // clamped
  });

  it("a RIGHT panel grows as the pointer moves left", () => {
    const start = { size: 320, x: 900 };
    expect(dragSize(start, 840, "right", 260, 520)).toBe(380);
    expect(dragSize(start, 1200, "right", 260, 520)).toBe(260); // clamped
  });
});

describe("persistence", () => {
  it("round-trips a width and clamps on load", () => {
    saveSize("pp.railWidth", 300);
    expect(loadSize("pp.railWidth", 220, 160, 420)).toBe(300);
    saveSize("pp.railWidth", 9999);
    expect(loadSize("pp.railWidth", 220, 160, 420)).toBe(420);
  });

  it("falls back on missing or junk values", () => {
    expect(loadSize("pp.missing", 220, 160, 420)).toBe(220);
    localStorage.setItem("pp.junk", "not-a-number");
    expect(loadSize("pp.junk", 220, 160, 420)).toBe(220);
  });
});
