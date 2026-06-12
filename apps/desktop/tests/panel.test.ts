/**
 * Pure edge-panel math (primitives/panel.ts): clamp and resize-drag
 * handed by EDGE (left/right on X, bottom on Y) — plus the GLOBAL
 * per-panel size persistence (state/prefs.ts pp.panel.<id>.size; one
 * remembered size per panel, founder June 12 2026), including the
 * one-time migration from the pre-refactor width keys.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { clampSize, dragSize } from "../src/lib/primitives/panel";
import {
  PANEL_SPECS,
  loadPanelSize,
  savePanelSize,
} from "../src/lib/state/prefs";

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
    const start = { size: 220, pos: 500 };
    expect(dragSize(start, 560, "left", 160, 420)).toBe(280);
    expect(dragSize(start, 440, "left", 160, 420)).toBe(160); // clamped
  });

  it("a RIGHT panel grows as the pointer moves left", () => {
    const start = { size: 320, pos: 900 };
    expect(dragSize(start, 840, "right", 260, 520)).toBe(380);
    expect(dragSize(start, 1200, "right", 260, 520)).toBe(260); // clamped
  });

  it("a BOTTOM panel grows as the pointer moves up (inner edge = top)", () => {
    const start = { size: 80, pos: 700 };
    expect(dragSize(start, 640, "bottom", 56, 200)).toBe(140);
    expect(dragSize(start, 900, "bottom", 56, 200)).toBe(56); // clamped
  });
});

describe("persistence (one GLOBAL size per panel — pp.panel.<id>.size)", () => {
  it("round-trips a size and clamps on load against the panel's spec", () => {
    savePanelSize("rail", 300);
    expect(loadPanelSize("rail")).toBe(300);
    savePanelSize("rail", 9999);
    expect(loadPanelSize("rail")).toBe(PANEL_SPECS.rail.maxSize);
  });

  it("falls back to the panel default on missing or junk values", () => {
    expect(loadPanelSize("inspector")).toBe(PANEL_SPECS.inspector.defaultSize);
    localStorage.setItem("pp.panel.filmstrip.size", "not-a-number");
    expect(loadPanelSize("filmstrip")).toBe(PANEL_SPECS.filmstrip.defaultSize);
  });

  it("migrates the pre-refactor width keys once, new key wins thereafter", () => {
    // An existing install keeps its widths across the layout round.
    localStorage.setItem("pp.railWidth", "300");
    localStorage.setItem("pp.inspectorWidth", "9999"); // junk-large: clamps
    expect(loadPanelSize("rail")).toBe(300);
    expect(loadPanelSize("inspector")).toBe(PANEL_SPECS.inspector.maxSize);
    // A save under the NEW key shadows the legacy value from then on.
    savePanelSize("rail", 200);
    expect(loadPanelSize("rail")).toBe(200);
  });

  it("the filmstrip (no legacy key) defaults cleanly", () => {
    expect(loadPanelSize("filmstrip")).toBe(PANEL_SPECS.filmstrip.defaultSize);
  });
});
