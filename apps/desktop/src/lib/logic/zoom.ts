/**
 * THE Look transform (featureset §2; UI-ARCHITECTURE §1) — pure math over
 * a top-left-origin view transform { scale, tx, ty } in container pixels
 * (CSS: translate(tx,ty) scale(s), transform-origin 0 0).
 *
 * THE ANCHOR BUG, fixed here: the legacy stage anchored wheel zoom on
 * event offsetX/offsetY — coordinates relative to the EVENT TARGET, which
 * over the transformed <img> is the image's own (scaled, letterbox-
 * shifted) space. The anchor therefore drifted, worst on the letterboxed
 * (typically vertical) axis. Every anchor point in this module is
 * CONTAINER-relative; zoomAtPoint keeps that point invariant on both axes
 * by construction, letterbox included (zoom.test.ts proves it).
 *
 * Zoom persistence across ←/→ ("zoom never resets per image" — §8
 * guardrail) travels as a SESSION { mode, scale, centerFrac }, not as a
 * raw transform: carryOver re-derives the transform from the session
 * against EACH image's own dimensions. Reusing tx/ty across mismatched
 * aspect ratios is exactly the drift class the risk register names
 * (Appendix B); the session never stores container or image pixels.
 */

export interface Dims {
  w: number;
  h: number;
}

export interface Point {
  x: number;
  y: number;
}

/** The live view transform: screen px per image px + container-px offsets. */
export interface ZoomTransform {
  scale: number;
  tx: number;
  ty: number;
}

export type ZoomMode = "fit" | "actual" | "free";

/**
 * The cross-image zoom session (mirrors the look slice's frozen
 * ZoomSessionState): `scale` is absolute (screen px per image px — punch
 * in at 1:1, arrow through to compare at the same magnification);
 * `centerFrac` is the image-FRACTION point sitting at the container
 * center, so it lands on the analogous spot of a differently-sized image.
 */
export interface ZoomSession {
  mode: ZoomMode;
  scale: number;
  centerFrac: Point;
}

/** Zoom floor: half of fit (or of 100% when fit is larger than 1:1). */
export const MIN_SCALE_FACTOR = 0.5;
/** Zoom ceiling: 800% — raised to fit when fit itself exceeds it. */
export const MAX_SCALE = 8;
/** `+`/`-` keyboard step factor (legacy behavior kept). */
export const STEP_FACTOR = 1.25;
/** Continuous wheel rate: scale × e^(−deltaY·rate) — ~×1.16 per 100 px. */
export const WHEEL_RATE = 0.0015;

/** Scale-to-contain ("fit"); 1 when either box is degenerate. */
export function fitScale(container: Dims, image: Dims): number {
  if (container.w <= 0 || container.h <= 0 || image.w <= 0 || image.h <= 0) return 1;
  return Math.min(container.w / image.w, container.h / image.h);
}

/** Clamp so fit AND 100% are always reachable, whichever is smaller. */
export function clampScale(scale: number, container: Dims, image: Dims): number {
  const fit = fitScale(container, image);
  const min = Math.min(fit, 1) * MIN_SCALE_FACTOR;
  const max = Math.max(fit, MAX_SCALE);
  return Math.max(min, Math.min(max, scale));
}

/** The fit transform: contained and centered (letterboxed) on both axes. */
export function fitTransform(container: Dims, image: Dims): ZoomTransform {
  const s = fitScale(container, image);
  return {
    scale: s,
    tx: (container.w - image.w * s) / 2,
    ty: (container.h - image.h * s) / 2,
  };
}

/**
 * Zoom toward a CONTAINER-relative point, keeping the image point under it
 * stationary on screen — on both axes, including anchors in the letterbox
 * (where the invariant image point lies outside the image; the math holds
 * unchanged). No translation snap: invariance is the prime directive;
 * Ctrl+0 always recovers.
 */
export function zoomAtPoint(
  t: ZoomTransform,
  container: Dims,
  image: Dims,
  p: Point,
  nextScale: number,
): ZoomTransform {
  const s1 = clampScale(nextScale, container, image);
  const ix = (p.x - t.tx) / t.scale; // image point under the anchor
  const iy = (p.y - t.ty) / t.scale;
  return { scale: s1, tx: p.x - ix * s1, ty: p.y - iy * s1 };
}

/** Drag / Space-hold pan: container-px deltas, scale untouched. */
export function panBy(t: ZoomTransform, dx: number, dy: number): ZoomTransform {
  return { scale: t.scale, tx: t.tx + dx, ty: t.ty + dy };
}

/** Continuous wheel zoom (trackpad-smooth, notch-friendly), unclamped —
 * zoomAtPoint clamps. Negative deltaY (wheel up) zooms in. */
export function wheelScale(scale: number, deltaY: number): number {
  return scale * Math.exp(-deltaY * WHEEL_RATE);
}

/** `+` / `-` step, unclamped — zoomAtPoint clamps. */
export function stepScale(scale: number, delta: 1 | -1): number {
  return delta === 1 ? scale * STEP_FACTOR : scale / STEP_FACTOR;
}

/** Snapshot the session from a live transform: which image fraction sits
 * at the container center (the only dimension-independent anchor). */
export function toSession(
  t: ZoomTransform,
  container: Dims,
  image: Dims,
  mode: ZoomMode,
): ZoomSession {
  return {
    mode,
    scale: t.scale,
    centerFrac: {
      x: (container.w / 2 - t.tx) / (t.scale * image.w),
      y: (container.h / 2 - t.ty) / (t.scale * image.h),
    },
  };
}

/**
 * Re-derive the transform from a session against (possibly new) image
 * dimensions — the ←/→ persistence rule (featureset §2): `fit` refits,
 * `actual` re-clamps 1:1, `free` keeps the absolute magnification; the
 * centerFrac point of the NEW image lands at the container center. A null
 * session is the default entry state: Fit.
 */
export function carryOver(
  session: ZoomSession | null,
  container: Dims,
  image: Dims,
): ZoomTransform {
  if (session === null || session.mode === "fit") return fitTransform(container, image);
  const scale = clampScale(session.mode === "actual" ? 1 : session.scale, container, image);
  return clampOffsets(
    {
      scale,
      tx: container.w / 2 - session.centerFrac.x * image.w * scale,
      ty: container.h / 2 - session.centerFrac.y * image.h * scale,
    },
    container,
    image,
  );
}

/**
 * Offset normalization, applied to every derived transform (carryOver is
 * the single render path, so wheel/pan/resize all pass through here): an
 * axis whose scaled image FITS the container centers on that axis (zoom
 * out below fill → the image returns to center, per axis — width can fit
 * while height still overflows); an axis that OVERFLOWS clamps so the
 * image edge never detaches from the container edge while panning.
 * (Founder dogfood, round 1.)
 */
export function clampOffsets(
  t: ZoomTransform,
  container: Dims,
  image: Dims,
): ZoomTransform {
  const clampAxis = (offset: number, c: number, i: number): number => {
    const scaled = i * t.scale;
    if (scaled <= c) return (c - scaled) / 2;
    return Math.max(c - scaled, Math.min(0, offset));
  };
  return {
    scale: t.scale,
    tx: clampAxis(t.tx, container.w, image.w),
    ty: clampAxis(t.ty, container.h, image.h),
  };
}
