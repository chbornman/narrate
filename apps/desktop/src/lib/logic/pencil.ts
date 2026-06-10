/**
 * The drawing state machine (CAPTURE §8.4) — the pure controller paired
 * with components/look/PencilOverlay.svelte (the thin pointer glue). One
 * pen-down → pen-up = one candidate event; pointer-cancel discards;
 * pen-up applies the commit threshold and quantizes to the §8.2 wire form.
 *
 * Raw points are stored UNSMOOTHED (C4): capture-side reduction is exactly
 * the 0.5-screen-px jitter dedupe, plus the explicit over-8192 overflow
 * policy below. Samples map through the transform CURRENT at their moment
 * (mid-stroke wheel zoom keeps every sample truthful).
 *
 * Over-8192 policy (the spec sets the bound, not the behavior; core's §3.3
 * doc reads "capture downsamples"): on overflow the accumulated points are
 * DECIMATED by stride 2 (first and last kept) and capture continues — the
 * whole gesture survives at half density rather than silently truncating
 * its tail. Flagged as an ambiguity ruling in the packet report.
 *
 * The undo stack (§8.5) lives here too: depth 10, in-memory, this process
 * only, holding only strokes authored here.
 */
import type { Dims, Point, ZoomTransform } from "./zoom";
import type { StrokePayloadWire, StrokeWirePoint } from "../types/dto";
import {
  BASE_W_DEFAULT,
  JITTER_MIN_SCREEN_PX,
  MAX_POINTS,
  normalize,
  pathLengthLE,
  pressurePerMille,
  quantizeCoord,
  screenToImage,
  shouldCommit,
  type NormPoint,
} from "./stroke";

/** What the pointer glue knows at each sample's moment. */
export interface PenFrame {
  /** THE view transform at this instant (image px → screen px). */
  t: ZoomTransform;
  /** Display-oriented image dimensions (the preview's pixel box). */
  image: Dims;
}

export interface PenSample {
  /** Container-relative screen px. */
  x: number;
  y: number;
  /** PointerEvent.pressure (mapped per §8.2 by pointer type). */
  pressure: number;
  pointerType: string;
  /** Event timestamp, ms (any monotonic origin shared across samples). */
  timeMs: number;
}

interface PenPoint {
  n: NormPoint;
  p: number;
  t: number;
}

/** In-flight stroke. Mutated in place by penMove (a stroke can carry
 * thousands of points; the machine stays pure in behavior — no IO, no
 * globals — and the tests treat it as a value). */
export interface PenState {
  points: PenPoint[];
  image: Dims;
  /** Last KEPT sample's screen point (jitter dedupe reference). */
  lastScreen: Point;
  downAtMs: number;
}

function toPenPoint(s: PenSample, f: PenFrame, t: number): PenPoint {
  return {
    n: normalize(screenToImage({ x: s.x, y: s.y }, f.t), f.image),
    p: pressurePerMille(s.pressure, s.pointerType),
    t,
  };
}

export function penDown(s: PenSample, f: PenFrame): PenState {
  return {
    points: [toPenPoint(s, f, 0)],
    image: f.image,
    lastScreen: { x: s.x, y: s.y },
    downAtMs: s.timeMs,
  };
}

/** Stride-2 decimation keeping the first and LAST points (overflow policy). */
export function decimate(points: PenPoint[]): PenPoint[] {
  const out: PenPoint[] = [];
  for (let i = 0; i < points.length; i += 2) out.push(points[i]);
  const last = points[points.length - 1];
  if (out[out.length - 1] !== last) out.push(last);
  return out;
}

export function penMove(state: PenState, s: PenSample, f: PenFrame): void {
  // Jitter dedupe: drop samples closer than 0.5 SCREEN px to the last
  // kept one (lossless at display resolution, §8.3).
  if (Math.hypot(s.x - state.lastScreen.x, s.y - state.lastScreen.y) < JITTER_MIN_SCREEN_PX)
    return;
  const prevT = state.points[state.points.length - 1].t;
  // t: integer ms offsets from pen-down, non-decreasing (§8.2).
  const t = Math.max(prevT, Math.round(s.timeMs - state.downAtMs));
  state.points.push(toPenPoint(s, f, t));
  state.lastScreen = { x: s.x, y: s.y };
  if (state.points.length > MAX_POINTS) state.points = decimate(state.points);
}

/**
 * Pen-up: apply the §8.4 threshold, quantize, and produce the wire
 * payload — or null for a discarded accident. Duration is pen-down →
 * pen-UP (a deliberate press-and-hold DOT commits even with one sample);
 * the event's ts is minted at append (pen-up) time by the backend, so
 * pen-down = ts − t_last (X1).
 */
export function penUp(
  state: PenState,
  orientation: number,
  upTimeMs: number,
): StrokePayloadWire | null {
  const norms = state.points.map((pt) => pt.n);
  const durationMs = Math.max(
    state.points[state.points.length - 1].t,
    Math.round(upTimeMs - state.downAtMs),
  );
  if (!shouldCommit(pathLengthLE(norms, state.image), durationMs)) return null;
  const points: StrokeWirePoint[] = state.points.map((pt) => [
    quantizeCoord(pt.n.x),
    quantizeCoord(pt.n.y),
    pt.p,
    pt.t,
  ]);
  return { baseW: BASE_W_DEFAULT, orientation, points, tool: "pencil" };
}

/** Live screen-space preview of the in-flight stroke under the CURRENT
 * transform (a later zoom re-anchors what was already drawn). */
export function livePoints(state: PenState, t: ZoomTransform): Point[] {
  return state.points.map((pt) => ({
    x: pt.n.x * state.image.w * t.scale + t.tx,
    y: pt.n.y * state.image.h * t.scale + t.ty,
  }));
}

export function livePressures(state: PenState): number[] {
  return state.points.map((pt) => pt.p);
}

// ---- undo stack (§8.5: depth 10, this-process, authored-here only) ----------

export const UNDO_DEPTH = 10;

export interface UndoEntry {
  /** Stroke event id (for the retraction target). */
  id: string;
  /** Image the stroke marked (journal refresh after the retraction). */
  hash: string;
}

export function pushUndo(stack: readonly UndoEntry[], entry: UndoEntry): UndoEntry[] {
  const next = [...stack, entry];
  return next.length > UNDO_DEPTH ? next.slice(next.length - UNDO_DEPTH) : next;
}

/** Drop an id wherever it sits (the eraser may retract a stacked stroke). */
export function removeUndo(stack: readonly UndoEntry[], id: string): UndoEntry[] {
  return stack.filter((e) => e.id !== id);
}
