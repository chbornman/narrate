/**
 * Histogram binning (the reviewing aid, not an editing tool): pure math
 * over an RGBA pixel buffer, paired with HistogramOverlay.svelte the way
 * zoom.ts pairs with LookStage. The component owns the offscreen <canvas>
 * + getImageData (a DOM fact); everything countable lives here so it is
 * unit-testable against known pixels.
 *
 * WHY a separate working size (logic/histogram, not the component): a
 * full-res decode is millions of pixels and we recompute on every image
 * change. A histogram's SHAPE is statistical, not per-pixel exact, so we
 * sample a bounded working buffer (the component scales the source into a
 * small canvas; sampleStep here lets a test feed a full buffer and skip).
 * 256 bins is the photo-standard 8-bit channel resolution.
 */

/** The four 256-bin channels a standard photo histogram shows. */
export interface Histogram {
  /** 256-bin counts per channel. */
  r: Uint32Array;
  g: Uint32Array;
  b: Uint32Array;
  /** Rec. 709 luminance bin counts (the combined-value curve). */
  lum: Uint32Array;
  /** Pixels actually counted (post-sampling, fully-opaque-weighted). */
  total: number;
}

export const BIN_COUNT = 256;

/** The longest-edge cap for the working buffer the component samples into.
 * Big enough that the curve shape is stable, small enough that binning is
 * a sub-millisecond pass even on a 50MP develop. Exported so the component
 * and its test agree on one number. */
export const WORKING_LONG_EDGE = 1024;

/**
 * The integer dimensions to draw the source into before binning: the
 * source scaled to fit within WORKING_LONG_EDGE on its long edge, never
 * upscaled (a small preview is already cheap to bin). Returns at least
 * 1x1 so a degenerate source still yields a drawable canvas.
 */
export function workingSize(
  srcW: number,
  srcH: number,
  longEdge: number = WORKING_LONG_EDGE,
): { w: number; h: number } {
  if (srcW <= 0 || srcH <= 0) return { w: 1, h: 1 };
  const longest = Math.max(srcW, srcH);
  // Never upscale: scaling a 200px preview UP to 1024 invents no detail and
  // only makes binning slower; cap the factor at 1.
  const scale = Math.min(1, longEdge / longest);
  return {
    w: Math.max(1, Math.round(srcW * scale)),
    h: Math.max(1, Math.round(srcH * scale)),
  };
}

/**
 * Bin an RGBA buffer (Uint8ClampedArray from getImageData, or any array
 * of 4-byte RGBA pixels) into the four channels.
 *
 * - `sampleStep` (default 1) counts every Nth pixel; the component leaves
 *   it 1 because it already downsampled via the working canvas, but it
 *   keeps the pure module honest (a test can feed a full buffer + step).
 * - Fully-transparent pixels (alpha 0) are skipped: a letterboxed or
 *   not-yet-painted canvas region must not pile a spike onto bin 0. Any
 *   nonzero alpha counts at full weight (we review opaque photographs;
 *   partial alpha is not a real case here).
 *
 * Luminance uses Rec. 709 coefficients (the sRGB display primaries the
 * webview renders), rounded to the nearest bin.
 */
export function binRgba(
  data: ArrayLike<number>,
  sampleStep: number = 1,
): Histogram {
  const r = new Uint32Array(BIN_COUNT);
  const g = new Uint32Array(BIN_COUNT);
  const b = new Uint32Array(BIN_COUNT);
  const lum = new Uint32Array(BIN_COUNT);
  let total = 0;

  const step = Math.max(1, Math.floor(sampleStep)) * 4;
  for (let i = 0; i + 3 < data.length; i += step) {
    const a = data[i + 3];
    if (a === 0) continue; // skip transparent (letterbox / unpainted)
    const rv = data[i];
    const gv = data[i + 1];
    const bv = data[i + 2];
    r[rv]++;
    g[gv]++;
    b[bv]++;
    // Rec. 709 luma; the +0.5 rounds to nearest bin rather than truncating.
    const y = (0.2126 * rv + 0.7152 * gv + 0.0722 * bv + 0.5) | 0;
    lum[y < 255 ? y : 255]++;
    total++;
  }
  return { r, g, b, lum, total };
}

/** A channel's tallest bin (the normalization denominator for plotting). */
export function peak(bins: Uint32Array): number {
  let m = 0;
  for (let i = 0; i < bins.length; i++) if (bins[i] > m) m = bins[i];
  return m;
}

/**
 * Normalize a channel's bins to [0,1] plot heights for ONE image, with a
 * choice of vertical scaling:
 *
 * - "linear": height = count / peak. Honest, but a single huge spike (a
 *   blown sky, a black surround) flattens everything else to invisibility.
 * - "log": height = log(1 + count) / log(1 + peak). The photo-standard
 *   readability choice: it lifts the sparse shadow/highlight tails into
 *   view so clipping at the ends is legible, without a separate axis the
 *   reviewer has to read. We default to "log" for exactly that reason and
 *   document the trade so the founder can flip it.
 *
 * Returns a fresh Float32Array of BIN_COUNT heights in [0,1]. An empty or
 * all-zero channel returns all zeros (no division by zero).
 */
export function normalize(
  bins: Uint32Array,
  scale: "linear" | "log" = "log",
): Float32Array {
  const out = new Float32Array(bins.length);
  const p = peak(bins);
  if (p === 0) return out; // empty image: flat baseline, no NaNs
  const denom = scale === "log" ? Math.log1p(p) : p;
  for (let i = 0; i < bins.length; i++) {
    const v = scale === "log" ? Math.log1p(bins[i]) : bins[i];
    out[i] = v / denom;
  }
  return out;
}
