/**
 * Grease-pencil geometry (CAPTURE §8, EVENTS §3.3, DECISIONS X1/C4) — pure,
 * headless-testable. Everything here speaks three coordinate languages:
 *
 *  · SCREEN px — container-relative points through THE view transform
 *    (logic/zoom.ts: image px → screen px, uniform scale + translation).
 *  · NORMALIZED fractions — display-oriented image space, PER-AXIS:
 *    x = px / W_display, y = py / H_display (the cached preview's space).
 *  · LONG-EDGE units — the spec's distance currency. The commit threshold
 *    (0.003), the eraser radius (0.01), and base_w are all normalized by
 *    the LONG edge, so per-axis fractions must be rescaled (x by W/longEdge,
 *    y by H/longEdge) before ANY distance math. Getting this wrong is
 *    invisible on square images and ~1.5× wrong on 3:2 — stroke.test.ts
 *    pins it with non-square images in both orientations.
 *
 * The wire form is EVENTS §3.3's integer [x, y, p, t]: ten-thousandths of
 * the display-oriented extent clamped to −2500..12500, pressure per-mille
 * (1000 = device reports none), ms offsets from pen-down. photoproof-core
 * owns the canonical encoding (byte-exact since P1.1); this module only
 * produces/consumes the same integers.
 */
import type { Dims, Point, ZoomTransform } from "./zoom";
import type { StrokeWirePoint, StrokePayloadWire } from "../types/dto";

// ---- spec constants (CAPTURE §8.2/§8.4/§8.6, X1/C4) -------------------------

/** Integer quantization: ten-thousandths of the display-oriented extent. */
export const QUANT = 10000;
export const COORD_MIN = -2500;
export const COORD_MAX = 12500;
/** Stroke base width, ten-thousandths of the long edge (X1 default). */
export const BASE_W_DEFAULT = 40;
export const MAX_POINTS = 8192;
/** Commit threshold: discard iff length < 0.003 long-edge units AND
 * duration < 100 ms (a held dot commits; a fleeting tap does not). */
export const COMMIT_MIN_LENGTH_LE = 0.003;
export const COMMIT_MIN_DURATION_MS = 100;
/** Jitter dedupe: consecutive samples closer than this are dropped
 * (lossless at display resolution — §8.3). */
export const JITTER_MIN_SCREEN_PX = 0.5;
/** Eraser hit radius: max(0.01 long-edge units, 12 screen px / s). */
export const ERASER_RADIUS_LE = 0.01;
export const ERASER_RADIUS_SCREEN_PX = 12;

/** A display-oriented normalized point (PER-AXIS fractions of W/H). */
export interface NormPoint {
  x: number;
  y: number;
}

// ---- transform mapping (T and T⁻¹ — zoom.ts owns T's semantics) -------------

/** Screen (container) px → image px: T⁻¹. */
export function screenToImage(p: Point, t: ZoomTransform): Point {
  return { x: (p.x - t.tx) / t.scale, y: (p.y - t.ty) / t.scale };
}

/** Image px → screen (container) px: T. */
export function imageToScreen(p: Point, t: ZoomTransform): Point {
  return { x: p.x * t.scale + t.tx, y: p.y * t.scale + t.ty };
}

/** Image px → per-axis normalized fractions (§8.1). */
export function normalize(p: Point, image: Dims): NormPoint {
  return { x: p.x / image.w, y: p.y / image.h };
}

export function denormalize(n: NormPoint, image: Dims): Point {
  return { x: n.x * image.w, y: n.y * image.h };
}

// ---- long-edge distance math (THE per-axis → long-edge conversion) ----------

export function longEdge(image: Dims): number {
  return Math.max(image.w, image.h);
}

/** Distance between two per-axis-normalized points in LONG-EDGE units. */
export function leDistance(a: NormPoint, b: NormPoint, image: Dims): number {
  const le = longEdge(image);
  const dx = (a.x - b.x) * (image.w / le);
  const dy = (a.y - b.y) * (image.h / le);
  return Math.hypot(dx, dy);
}

/** Total polyline length in long-edge units. */
export function pathLengthLE(pts: readonly NormPoint[], image: Dims): number {
  let len = 0;
  for (let i = 1; i < pts.length; i++) len += leDistance(pts[i - 1], pts[i], image);
  return len;
}

// ---- quantization & pressure -------------------------------------------------

/** Fraction → integer ten-thousandths, clamped to the §8.2 overshoot range. */
export function quantizeCoord(fraction: number): number {
  const v = Math.round(fraction * QUANT);
  return Math.max(COORD_MIN, Math.min(COORD_MAX, v));
}

/**
 * PointerEvent pressure → per-mille (§8.2). Only a real pen reports
 * pressure: mouse pressure is a constant 0.5 while a button is down and
 * basic touch is indistinguishable from it, so every non-pen pointer
 * records 1000 ("device reports none" — the macOS norm, UI §4.4).
 */
export function pressurePerMille(pressure: number, pointerType: string): number {
  if (pointerType !== "pen") return 1000;
  return Math.max(0, Math.min(1000, Math.round(pressure * 1000)));
}

// ---- width model (§8.2) -------------------------------------------------------

/** w(i) = base_w × (0.4 + 0.6·p/1000), in ten-thousandths of the long edge. */
export function widthLE(p: number, baseW: number): number {
  return baseW * (0.4 + (0.6 * p) / 1000);
}

/** On-screen width = image-space width × s — marks zoom with the image. */
export function screenWidth(p: number, baseW: number, image: Dims, scale: number): number {
  return (widthLE(p, baseW) / QUANT) * longEdge(image) * scale;
}

// ---- commit threshold (§8.4) --------------------------------------------------

/** Discard IFF tiny AND brief; either alone commits. */
export function shouldCommit(lengthLE: number, durationMs: number): boolean {
  return !(lengthLE < COMMIT_MIN_LENGTH_LE && durationMs < COMMIT_MIN_DURATION_MS);
}

// ---- EXIF orientation compensation (§8.1) -------------------------------------
//
// A stroke records the orientation applied at draw time; if a later tool
// rewrites orientation metadata, the renderer remaps stored points into
// the CURRENT display space instead of letting the marks rotate out from
// under the user. Maps are affine in normalized space (overshoot values
// remap correctly); orientations 5–8 swap the axes.

/** Raw-sensor normalized (u,v) → display normalized under orientation o. */
function orientApply(o: number, u: number, v: number): NormPoint {
  switch (o) {
    case 2:
      return { x: 1 - u, y: v };
    case 3:
      return { x: 1 - u, y: 1 - v };
    case 4:
      return { x: u, y: 1 - v };
    case 5:
      return { x: v, y: u };
    case 6:
      return { x: 1 - v, y: u };
    case 7:
      return { x: 1 - v, y: 1 - u };
    case 8:
      return { x: v, y: 1 - u };
    default:
      return { x: u, y: v };
  }
}

/** Display normalized under orientation o → raw-sensor normalized. */
function orientInvert(o: number, p: NormPoint): { u: number; v: number } {
  switch (o) {
    case 2:
      return { u: 1 - p.x, v: p.y };
    case 3:
      return { u: 1 - p.x, v: 1 - p.y };
    case 4:
      return { u: p.x, v: 1 - p.y };
    case 5:
      return { u: p.y, v: p.x };
    case 6:
      return { u: p.y, v: 1 - p.x };
    case 7:
      return { u: 1 - p.y, v: 1 - p.x };
    case 8:
      return { u: 1 - p.y, v: p.x };
    default:
      return { u: p.x, v: p.y };
  }
}

/** Remap a point recorded under `from` into the `to` display space. */
export function remapOrientation(p: NormPoint, from: number, to: number): NormPoint {
  if (from === to) return p;
  const { u, v } = orientInvert(from, p);
  return orientApply(to, u, v);
}

// ---- wire helpers --------------------------------------------------------------

export function wireToNorm(pt: StrokeWirePoint): NormPoint {
  return { x: pt[0] / QUANT, y: pt[1] / QUANT };
}

/** Frontend mirror of the §8.2 ranges — the Rust side is authoritative;
 * this exists so the property tests can assert payload validity. */
export function validatePayload(p: StrokePayloadWire): string[] {
  const errors: string[] = [];
  if (p.tool !== "pencil") errors.push("tool must be \"pencil\"");
  if (!Number.isInteger(p.orientation) || p.orientation < 1 || p.orientation > 8)
    errors.push("orientation must be 1..=8");
  if (!Number.isInteger(p.baseW) || p.baseW < 1 || p.baseW > QUANT)
    errors.push("base_w must be 1..=10000");
  if (p.points.length < 1 || p.points.length > MAX_POINTS)
    errors.push("points must be 1..=8192");
  let prevT = 0;
  for (const [i, [x, y, pp, t]] of p.points.entries()) {
    if (![x, y, pp, t].every(Number.isInteger)) errors.push(`point ${i} not integer`);
    if (x < COORD_MIN || x > COORD_MAX || y < COORD_MIN || y > COORD_MAX)
      errors.push(`point ${i} out of coordinate range`);
    if (pp < 0 || pp > 1000) errors.push(`point ${i} pressure out of range`);
    if (i === 0 ? t !== 0 : t < prevT) errors.push(`point ${i} time not non-decreasing from 0`);
    prevT = t;
  }
  return errors;
}

// ---- eraser hit-test (§8.6) -----------------------------------------------------

export interface ErasableStroke {
  /** Event id — ULID order IS recency order; the greatest eligible wins. */
  id: string;
  orientation: number;
  points: readonly StrokeWirePoint[];
}

/** Point→segment distance in long-edge units. */
function segmentDistanceLE(p: NormPoint, a: NormPoint, b: NormPoint, image: Dims): number {
  const le = longEdge(image);
  const sx = image.w / le;
  const sy = image.h / le;
  const px = p.x * sx;
  const py = p.y * sy;
  const ax = a.x * sx;
  const ay = a.y * sy;
  const bx = b.x * sx;
  const by = b.y * sy;
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;
  const u = lenSq === 0 ? 0 : Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / lenSq));
  return Math.hypot(px - (ax + u * dx), py - (ay + u * dy));
}

/** Min distance from a display-space point to a stroke's polyline (LE units),
 * compensating a recorded-orientation mismatch first. */
export function strokeDistanceLE(
  tap: NormPoint,
  stroke: ErasableStroke,
  image: Dims,
  displayOrientation: number,
): number {
  const pts = stroke.points.map((pt) =>
    remapOrientation(wireToNorm(pt), stroke.orientation, displayOrientation),
  );
  if (pts.length === 1) return leDistance(tap, pts[0], image);
  let min = Infinity;
  for (let i = 1; i < pts.length; i++) {
    const d = segmentDistanceLE(tap, pts[i - 1], pts[i], image);
    if (d < min) min = d;
  }
  return min;
}

/** The §8.6 radius: at least a 12-screen-px target at any zoom. */
export function eraserRadiusLE(image: Dims, scale: number): number {
  return Math.max(ERASER_RADIUS_LE, ERASER_RADIUS_SCREEN_PX / (scale * longEdge(image)));
}

/** Topmost eligible stroke: among hits, the most recent (latest event id). */
export function pickEraserTarget(
  tap: NormPoint,
  strokes: readonly ErasableStroke[],
  image: Dims,
  scale: number,
  displayOrientation: number,
): string | null {
  const radius = eraserRadiusLE(image, scale);
  let best: string | null = null;
  for (const s of strokes) {
    if (strokeDistanceLE(tap, s, image, displayOrientation) > radius) continue;
    if (best === null || s.id > best) best = s.id;
  }
  return best;
}

// ---- render-path generation (§8.3 — render-only; stored points stay raw) --------

export interface BezierSegment {
  p0: Point;
  c1: Point;
  c2: Point;
  p1: Point;
}

/**
 * Centripetal Catmull-Rom (α = 0.5) through the points, as cubic Bézier
 * segments (endpoints duplicated for the open ends). Centripetal knots
 * never cusp or self-intersect between samples; degenerate (coincident)
 * knots fall back to straight control points.
 */
export function catmullRomBeziers(pts: readonly Point[]): BezierSegment[] {
  if (pts.length < 2) return [];
  const segs: BezierSegment[] = [];
  const knot = (a: Point, b: Point): number => Math.sqrt(Math.hypot(b.x - a.x, b.y - a.y));
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? pts[i + 1];
    const d1 = knot(p0, p1);
    const d2 = knot(p1, p2);
    const d3 = knot(p2, p3);
    let c1: Point;
    let c2: Point;
    if (d1 < 1e-6 || d2 < 1e-6) {
      c1 = { x: p1.x + (p2.x - p1.x) / 3, y: p1.y + (p2.y - p1.y) / 3 };
    } else {
      // Standard centripetal CR → Bézier control form.
      c1 = {
        x: (d1 * d1 * p2.x - d2 * d2 * p0.x + (2 * d1 * d1 + 3 * d1 * d2 + d2 * d2) * p1.x) / (3 * d1 * (d1 + d2)),
        y: (d1 * d1 * p2.y - d2 * d2 * p0.y + (2 * d1 * d1 + 3 * d1 * d2 + d2 * d2) * p1.y) / (3 * d1 * (d1 + d2)),
      };
    }
    if (d3 < 1e-6 || d2 < 1e-6) {
      c2 = { x: p2.x - (p2.x - p1.x) / 3, y: p2.y - (p2.y - p1.y) / 3 };
    } else {
      c2 = {
        x: (d3 * d3 * p1.x - d2 * d2 * p3.x + (2 * d3 * d3 + 3 * d3 * d2 + d2 * d2) * p2.x) / (3 * d3 * (d3 + d2)),
        y: (d3 * d3 * p1.y - d2 * d2 * p3.y + (2 * d3 * d3 + 3 * d3 * d2 + d2 * d2) * p2.y) / (3 * d3 * (d3 + d2)),
      };
    }
    segs.push({ p0: p1, c1, c2, p1: p2 });
  }
  return segs;
}

/** SVG path string over the same Catmull-Rom segments (micro-previews). */
export function svgPathFor(pts: readonly Point[]): string {
  if (pts.length === 0) return "";
  const f = (v: number): string => v.toFixed(1);
  if (pts.length === 1) {
    // A dot: zero-length segment; the round linecap renders it.
    const p = pts[0];
    return `M ${f(p.x)} ${f(p.y)} L ${f(p.x)} ${f(p.y)}`;
  }
  const segs = catmullRomBeziers(pts);
  let d = `M ${f(segs[0].p0.x)} ${f(segs[0].p0.y)}`;
  for (const s of segs)
    d += ` C ${f(s.c1.x)} ${f(s.c1.y)}, ${f(s.c2.x)} ${f(s.c2.y)}, ${f(s.p1.x)} ${f(s.p1.y)}`;
  return d;
}

// ---- stored stroke → screen-space render spec ------------------------------------

export interface StrokeScreenSpec {
  /** Screen-space points through the CURRENT transform. */
  pts: Point[];
  /** Per-point on-screen widths (the §8.2 width model × s). */
  widths: number[];
}

export function strokeScreenSpec(
  points: readonly StrokeWirePoint[],
  baseW: number,
  image: Dims,
  t: ZoomTransform,
  fromOrientation: number,
  displayOrientation: number,
): StrokeScreenSpec {
  const pts: Point[] = [];
  const widths: number[] = [];
  for (const pt of points) {
    const n = remapOrientation(wireToNorm(pt), fromOrientation, displayOrientation);
    pts.push(imageToScreen(denormalize(n, image), t));
    widths.push(screenWidth(pt[2], baseW, image, t.scale));
  }
  return { pts, widths };
}
