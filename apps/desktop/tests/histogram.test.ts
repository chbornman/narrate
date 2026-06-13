/**
 * Histogram binning (logic/histogram.ts): known pixels land in known bins,
 * transparent pixels never spike bin 0, the working-size downsample caps
 * the long edge without upscaling, and normalize stays in [0,1] for both
 * scalings. The component (HistogramOverlay.svelte) owns only the canvas
 * getImageData glue; all the countable behavior is proven here.
 */
import { describe, expect, it } from "vitest";
import {
  BIN_COUNT,
  WORKING_LONG_EDGE,
  binRgba,
  normalize,
  peak,
  workingSize,
} from "../src/lib/logic/histogram";

/** Build a flat RGBA buffer of `n` pixels all the same color. */
const flat = (n: number, [r, g, b, a]: [number, number, number, number]) => {
  const buf = new Uint8ClampedArray(n * 4);
  for (let i = 0; i < n; i++) {
    buf[i * 4] = r;
    buf[i * 4 + 1] = g;
    buf[i * 4 + 2] = b;
    buf[i * 4 + 3] = a;
  }
  return buf;
};

describe("binRgba", () => {
  it("puts a solid color in exactly its bins", () => {
    const h = binRgba(flat(10, [12, 200, 255, 255]));
    expect(h.total).toBe(10);
    expect(h.r[12]).toBe(10);
    expect(h.g[200]).toBe(10);
    expect(h.b[255]).toBe(10);
    // Nothing anywhere else on those channels.
    expect(h.r.reduce((a, v) => a + v, 0)).toBe(10);
    expect(h.g.reduce((a, v) => a + v, 0)).toBe(10);
    expect(h.b.reduce((a, v) => a + v, 0)).toBe(10);
  });

  it("pure black and pure white land on bins 0 and 255", () => {
    const black = binRgba(flat(4, [0, 0, 0, 255]));
    expect(black.r[0]).toBe(4);
    expect(black.lum[0]).toBe(4);

    const white = binRgba(flat(4, [255, 255, 255, 255]));
    expect(white.r[255]).toBe(4);
    expect(white.b[255]).toBe(4);
    // Rec.709 of (255,255,255) is 255 → the top luminance bin.
    expect(white.lum[255]).toBe(4);
  });

  it("luminance uses Rec.709 weights (pure green is the brightest primary)", () => {
    const green = binRgba(flat(1, [0, 255, 0, 255]));
    const red = binRgba(flat(1, [255, 0, 0, 255]));
    const blue = binRgba(flat(1, [0, 0, 255, 255]));
    // 0.7152*255≈182, 0.2126*255≈54, 0.0722*255≈18.
    expect(green.lum[182]).toBe(1);
    expect(red.lum[54]).toBe(1);
    expect(blue.lum[18]).toBe(1);
  });

  it("skips fully-transparent pixels (no false bin-0 spike)", () => {
    // 3 opaque mid-grey + 5 transparent: only the 3 count, and bin 0 stays
    // empty (the letterbox / unpainted-canvas case).
    const buf = new Uint8ClampedArray(8 * 4);
    for (let i = 0; i < 3; i++) {
      buf[i * 4] = 128;
      buf[i * 4 + 1] = 128;
      buf[i * 4 + 2] = 128;
      buf[i * 4 + 3] = 255;
    }
    // pixels 3..7 left at all-zero (alpha 0) by construction.
    const h = binRgba(buf);
    expect(h.total).toBe(3);
    expect(h.r[128]).toBe(3);
    expect(h.r[0]).toBe(0);
    expect(h.lum[0]).toBe(0);
  });

  it("an empty buffer yields zero counts, not a crash", () => {
    const h = binRgba(new Uint8ClampedArray(0));
    expect(h.total).toBe(0);
    expect(peak(h.r)).toBe(0);
    expect(h.lum.length).toBe(BIN_COUNT);
  });

  it("sampleStep counts every Nth pixel (downsample correctness)", () => {
    // 100 pixels: even indices red=10, odd indices red=20. Step 2 starts at
    // pixel 0 and hits only the red=10 pixels.
    const buf = new Uint8ClampedArray(100 * 4);
    for (let i = 0; i < 100; i++) {
      buf[i * 4] = i % 2 === 0 ? 10 : 20;
      buf[i * 4 + 3] = 255;
    }
    const all = binRgba(buf, 1);
    expect(all.total).toBe(100);
    expect(all.r[10]).toBe(50);
    expect(all.r[20]).toBe(50);

    const stepped = binRgba(buf, 2);
    expect(stepped.total).toBe(50);
    expect(stepped.r[10]).toBe(50);
    expect(stepped.r[20]).toBe(0);
  });
});

describe("workingSize", () => {
  it("caps the long edge and preserves aspect", () => {
    const s = workingSize(4000, 3000);
    expect(Math.max(s.w, s.h)).toBe(WORKING_LONG_EDGE);
    expect(s.w).toBe(1024);
    expect(s.h).toBe(768); // 3000 * (1024/4000)
  });

  it("caps on the TALL edge for a portrait source", () => {
    const s = workingSize(3000, 4000);
    expect(Math.max(s.w, s.h)).toBe(WORKING_LONG_EDGE);
    expect(s.h).toBe(1024);
    expect(s.w).toBe(768);
  });

  it("never upscales a small source", () => {
    const s = workingSize(200, 150);
    expect(s).toEqual({ w: 200, h: 150 });
  });

  it("returns a drawable 1x1 for a degenerate source", () => {
    expect(workingSize(0, 0)).toEqual({ w: 1, h: 1 });
    expect(workingSize(-5, 10)).toEqual({ w: 1, h: 1 });
  });
});

describe("normalize", () => {
  it("scales the peak bin to 1 (linear)", () => {
    const bins = new Uint32Array(BIN_COUNT);
    bins[10] = 50;
    bins[20] = 100; // the peak
    const out = normalize(bins, "linear");
    expect(out[20]).toBeCloseTo(1);
    expect(out[10]).toBeCloseTo(0.5);
    expect(out[0]).toBe(0);
  });

  it("log scaling lifts small counts above their linear height", () => {
    const bins = new Uint32Array(BIN_COUNT);
    bins[0] = 1; // a sparse tail bin
    bins[100] = 1000; // a tall spike
    const lin = normalize(bins, "linear");
    const log = normalize(bins, "log");
    expect(log[100]).toBeCloseTo(1);
    // The lone shadow-tail pixel is invisible in linear, legible in log.
    expect(lin[0]).toBeLessThan(0.01);
    expect(log[0]).toBeGreaterThan(lin[0]);
  });

  it("an all-zero channel normalizes to all zeros (no NaN)", () => {
    const out = normalize(new Uint32Array(BIN_COUNT), "log");
    expect(out.every((v) => v === 0)).toBe(true);
    const lin = normalize(new Uint32Array(BIN_COUNT), "linear");
    expect(lin.every((v) => v === 0)).toBe(true);
  });

  it("every height stays within [0,1]", () => {
    const bins = new Uint32Array(BIN_COUNT);
    for (let i = 0; i < BIN_COUNT; i++) bins[i] = (i * 37) % 911;
    for (const scale of ["linear", "log"] as const) {
      const out = normalize(bins, scale);
      for (const v of out) {
        expect(v).toBeGreaterThanOrEqual(0);
        expect(v).toBeLessThanOrEqual(1);
      }
    }
  });
});
