/**
 * The progressive full-resolution swap threshold (logic/fullres.ts —
 * dogfood round 1): Look opens on the display preview and swaps to the
 * original once the zoom demands more pixels than the preview supplies,
 * i.e. rendered device pixels (zoom scale × preview dims × DPR) exceed the
 * preview's own pixel dims. The swap mechanics (request stickiness, no
 * flash until loaded, 404 → preview stands) are LookStage facts; the
 * protocol's stored-format allowlist is tested in Rust (protocol.rs).
 */
import { describe, expect, it } from "vitest";
import { needsOriginal } from "../src/lib/logic/fullres";
import { fitScale } from "../src/lib/logic/zoom";

const PREVIEW = { w: 2560, h: 1707 }; // the display-preview class

describe("needsOriginal — the swap-threshold predicate", () => {
  it("stays on the preview at fit (typical container is smaller than 2560)", () => {
    const scale = fitScale({ w: 1600, h: 1000 }, PREVIEW);
    expect(scale).toBeLessThan(1);
    expect(needsOriginal({ scale, preview: PREVIEW })).toBe(false);
  });

  it("stays on the preview at exactly 1:1 (epsilon headroom)", () => {
    expect(needsOriginal({ scale: 1, preview: PREVIEW })).toBe(false);
    expect(needsOriginal({ scale: 1.0005, preview: PREVIEW })).toBe(false);
  });

  it("swaps once the zoom outruns the preview's pixels", () => {
    expect(needsOriginal({ scale: 1.01, preview: PREVIEW })).toBe(true);
    expect(needsOriginal({ scale: 4, preview: PREVIEW })).toBe(true);
  });

  it("is the founder rule: zoom scale × preview dims exceeding the preview's pixel dims", () => {
    // scale·preview.w > preview.w ⇔ scale > 1 — the predicate and the
    // spec phrasing agree at the boundary.
    const justUnder = (PREVIEW.w - 1) / PREVIEW.w;
    const justOver = (PREVIEW.w + 26) / PREVIEW.w; // > 1 + epsilon
    expect(needsOriginal({ scale: justUnder, preview: PREVIEW })).toBe(false);
    expect(needsOriginal({ scale: justOver, preview: PREVIEW })).toBe(true);
  });

  it("HiDPI exhausts the preview sooner (device pixels, not CSS pixels)", () => {
    expect(needsOriginal({ scale: 0.6, preview: PREVIEW, devicePixelRatio: 2 })).toBe(
      true,
    );
    expect(needsOriginal({ scale: 0.4, preview: PREVIEW, devicePixelRatio: 2 })).toBe(
      false,
    );
    // DPR defaults to 1.
    expect(needsOriginal({ scale: 0.6, preview: PREVIEW })).toBe(false);
  });

  it("degenerate preview dims never request (nothing loaded yet)", () => {
    expect(needsOriginal({ scale: 8, preview: { w: 0, h: 0 } })).toBe(false);
    expect(needsOriginal({ scale: 8, preview: { w: 2560, h: 0 } })).toBe(false);
  });
});
