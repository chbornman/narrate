/**
 * The progressive full-resolution swap threshold (logic/fullres.ts —
 * dogfood round 1): Look opens on the display preview and swaps to the
 * original once the zoom demands more pixels than the preview supplies,
 * i.e. rendered device pixels (zoom scale × preview dims × DPR) exceed the
 * preview's own pixel dims. Dogfood round 2 adds the SOURCE LADDER below
 * the same predicate: /original, then the RAW's /embedded native JPEG.
 * The swap mechanics (request stickiness, no flash until loaded, 404 →
 * next rung → preview stands) are LookStage facts; the protocol's
 * allowlists and the embedded route's orientation/geometry agreement are
 * tested in Rust (protocol.rs, library_acceptance.rs).
 */
import { describe, expect, it } from "vitest";
import {
  FIRST_SOURCE,
  loadProvesPixels,
  needsOriginal,
  nextSource,
} from "../src/lib/logic/fullres";
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

describe("the source ladder — original, then embedded-native, then the preview stands", () => {
  it("a fresh request starts at the original", () => {
    expect(FIRST_SOURCE).toBe("original");
  });

  it("a refused original advances to the embedded-native rung (the RAW path)", () => {
    expect(nextSource("original")).toBe("embedded");
  });

  it("a refused embedded exhausts the ladder (preview stands; decoded 1:1 is M1.5)", () => {
    expect(nextSource("embedded")).toBeNull();
  });
});

describe("the swap gate — only a load that proved pixels supplants the preview", () => {
  // THE founder bug (June 2026): WKWebView fires `load` with natural dims
  // 0×0 — instead of `error` — when an <img>'s src was swapped inside its
  // own onerror handler and the new URL 404s. That is exactly a RAW's
  // ladder hop (/original refused → /embedded refused). Trusting the lying
  // load replaced the painted preview with the webview's broken-image
  // glyph and wedged the ladder. The gate makes a dimensionless load a
  // refusal: same path as onerror — next rung, or the preview stands.
  it("a RAW whose embedded rung 404s keeps the preview painted (0×0 load = refusal)", () => {
    expect(loadProvesPixels({ w: 0, h: 0 })).toBe(false);
  });

  it("a genuinely decoded source proves pixels and may swap in", () => {
    expect(loadProvesPixels({ w: 9504, h: 6336 })).toBe(true);
    expect(loadProvesPixels({ w: 1, h: 1 })).toBe(true);
  });

  it("a degenerate axis never proves (raster routes always have both)", () => {
    expect(loadProvesPixels({ w: 2560, h: 0 })).toBe(false);
    expect(loadProvesPixels({ w: 0, h: 1707 })).toBe(false);
  });
});
