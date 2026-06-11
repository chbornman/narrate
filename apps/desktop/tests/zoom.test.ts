/**
 * THE Look transform (logic/zoom.ts) — featureset §2 acceptance: the
 * cursor point is invariant under zoom on BOTH axes (the legacy anchor
 * bug drifted vertically: it anchored on event offsets relative to the
 * transformed <img>, not the container), letterboxed edges included; and
 * the session carryOver re-derives the transform per image so zoom
 * persistence across ←/→ survives DIMENSION CHANGES (Appendix B names
 * naive transform reuse on mismatched aspect ratios as the risk).
 */
import { describe, expect, it } from "vitest";
import {
  MAX_SCALE,
  carryOver,
  clampOffsets,
  clampScale,
  fitScale,
  fitTransform,
  panBy,
  stepScale,
  toSession,
  wheelScale,
  zoomAtPoint,
  type Dims,
  type Point,
  type ZoomTransform,
} from "../src/lib/logic/zoom";

const dims = (w: number, h: number): Dims => ({ w, h });

/** Image point sitting under a container point for a transform. */
const imagePointUnder = (t: ZoomTransform, p: Point): Point => ({
  x: (p.x - t.tx) / t.scale,
  y: (p.y - t.ty) / t.scale,
});

/** Where an image point lands on screen for a transform. */
const screenPoint = (t: ZoomTransform, q: Point): Point => ({
  x: q.x * t.scale + t.tx,
  y: q.y * t.scale + t.ty,
});

// A landscape photo, height-limited in this container → pillarboxed
// (gaps left/right); and a wide photo, width-limited → letterboxed
// (gaps top/bottom). Both letterbox axes are exercised.
const C = dims(1200, 800);
const LANDSCAPE = dims(4000, 3000); // fit 4/15 ≈ 0.2667, gaps left/right
const WIDE = dims(4000, 1000); // fit 0.3, gaps top/bottom
const PORTRAIT = dims(3000, 4500);

describe("fit", () => {
  it("scales to contain and centers the letterbox on the limited axis", () => {
    const t = fitTransform(C, LANDSCAPE);
    expect(t.scale).toBeCloseTo(800 / 3000, 12);
    expect(t.ty).toBeCloseTo(0, 12);
    expect(t.tx).toBeCloseTo((1200 - 4000 * (800 / 3000)) / 2, 12);

    const w = fitTransform(C, WIDE);
    expect(w.scale).toBeCloseTo(0.3, 12);
    expect(w.tx).toBeCloseTo(0, 12);
    expect(w.ty).toBeCloseTo((800 - 1000 * 0.3) / 2, 12);
  });

  it("degenerate dimensions fall back to scale 1", () => {
    expect(fitScale(dims(0, 0), LANDSCAPE)).toBe(1);
    expect(fitScale(C, dims(0, 0))).toBe(1);
  });
});

describe("zoomAtPoint — cursor invariance on both axes", () => {
  const anchors: [string, Point][] = [
    ["image center", { x: 600, y: 400 }],
    ["off-center inside the image", { x: 700, y: 250 }],
    ["container corner", { x: 0, y: 0 }],
    ["right edge", { x: 1200, y: 400 }],
  ];

  for (const [name, p] of anchors) {
    it(`keeps the point under "${name}" stationary zooming in and out`, () => {
      let t = fitTransform(C, LANDSCAPE);
      for (const next of [1, 2.5, 0.5, 0.2]) {
        const q = imagePointUnder(t, p);
        t = zoomAtPoint(t, C, LANDSCAPE, p, next);
        const s = screenPoint(t, q);
        expect(s.x).toBeCloseTo(p.x, 9);
        expect(s.y).toBeCloseTo(p.y, 9);
      }
    });
  }

  it("holds in the HORIZONTAL letterbox band (pillarbox gap)", () => {
    const t0 = fitTransform(C, LANDSCAPE);
    const p = { x: t0.tx / 2, y: 400 }; // inside the left gap
    const q = imagePointUnder(t0, p); // outside the image — math unchanged
    const t1 = zoomAtPoint(t0, C, LANDSCAPE, p, 1);
    expect(screenPoint(t1, q).x).toBeCloseTo(p.x, 9);
    expect(screenPoint(t1, q).y).toBeCloseTo(p.y, 9);
  });

  it("holds in the VERTICAL letterbox band — the legacy drift axis", () => {
    const t0 = fitTransform(C, WIDE); // gaps top/bottom
    const p = { x: 600, y: t0.ty / 2 }; // above the image's top edge
    const q = imagePointUnder(t0, p);
    const t1 = zoomAtPoint(t0, C, WIDE, p, 1.5);
    expect(screenPoint(t1, q).x).toBeCloseTo(p.x, 9);
    expect(screenPoint(t1, q).y).toBeCloseTo(p.y, 9);
  });

  it("pins the image's top-left corner exactly when anchored on it", () => {
    // The legacy offsetX/offsetY bug is largest here: image-relative
    // offsets lose the letterbox shift entirely.
    const t0 = fitTransform(C, LANDSCAPE);
    const corner = { x: t0.tx, y: t0.ty }; // image (0,0) on screen
    const t1 = zoomAtPoint(t0, C, LANDSCAPE, corner, 2);
    expect(screenPoint(t1, { x: 0, y: 0 }).x).toBeCloseTo(corner.x, 9);
    expect(screenPoint(t1, { x: 0, y: 0 }).y).toBeCloseTo(corner.y, 9);
  });

  it("zoom in then out at the same anchor returns to the start", () => {
    const t0 = fitTransform(C, LANDSCAPE);
    const p = { x: 832, y: 117 };
    const t1 = zoomAtPoint(t0, C, LANDSCAPE, p, 1);
    const t2 = zoomAtPoint(t1, C, LANDSCAPE, p, t0.scale);
    expect(t2.scale).toBeCloseTo(t0.scale, 12);
    expect(t2.tx).toBeCloseTo(t0.tx, 9);
    expect(t2.ty).toBeCloseTo(t0.ty, 9);
  });

  it("already at the ceiling: a further zoom-in is an exact no-op", () => {
    const p = { x: 600, y: 400 };
    const t1 = zoomAtPoint(fitTransform(C, LANDSCAPE), C, LANDSCAPE, p, MAX_SCALE);
    const t2 = zoomAtPoint(t1, C, LANDSCAPE, p, MAX_SCALE * 2);
    expect(t2).toEqual(t1);
  });
});

describe("scale clamping", () => {
  it("clamps to [half-of-fit, 800%] for ordinary photos", () => {
    const fit = fitScale(C, LANDSCAPE); // < 1
    expect(clampScale(100, C, LANDSCAPE)).toBe(MAX_SCALE);
    expect(clampScale(1e-6, C, LANDSCAPE)).toBeCloseTo(fit / 2, 12);
  });

  it("keeps BOTH fit and 100% reachable for tiny images (fit > 800%)", () => {
    const tiny = dims(100, 80); // fit 10 in this container
    expect(fitScale(C, tiny)).toBe(10);
    expect(clampScale(1, C, tiny)).toBe(1); // Ctrl+1 still means 100%
    expect(clampScale(25, C, tiny)).toBe(10); // ceiling raised to fit
    expect(clampScale(0.2, C, tiny)).toBeCloseTo(0.5, 12); // half of 100%
  });
});

describe("wheel and step factors", () => {
  it("wheel: negative deltaY zooms in, positive out, symmetrically", () => {
    expect(wheelScale(1, -100)).toBeGreaterThan(1);
    expect(wheelScale(1, 100)).toBeLessThan(1);
    expect(wheelScale(wheelScale(0.7, 240), -240)).toBeCloseTo(0.7, 12);
  });

  it("wheel is continuous: small deltas make small changes", () => {
    expect(wheelScale(1, -2)).toBeCloseTo(1, 2);
    expect(wheelScale(1, -2)).not.toBe(1);
  });

  it("step is ×1.25 per press, symmetric", () => {
    expect(stepScale(1, 1)).toBeCloseTo(1.25, 12);
    expect(stepScale(stepScale(2, 1), -1)).toBeCloseTo(2, 12);
  });
});

describe("pan", () => {
  it("offsets translation only", () => {
    const t = panBy({ scale: 1, tx: 10, ty: 20 }, -4, 7);
    expect(t).toEqual({ scale: 1, tx: 6, ty: 27 });
  });
});

describe("session carryOver — ←/→ persistence (featureset §2)", () => {
  it("null session is the entry default: Fit", () => {
    expect(carryOver(null, C, LANDSCAPE)).toEqual(fitTransform(C, LANDSCAPE));
  });

  it("fit mode refits against the NEW image's dimensions", () => {
    const s = toSession(fitTransform(C, LANDSCAPE), C, LANDSCAPE, "fit");
    expect(s.centerFrac.x).toBeCloseTo(0.5, 12);
    expect(s.centerFrac.y).toBeCloseTo(0.5, 12);
    expect(carryOver(s, C, PORTRAIT)).toEqual(fitTransform(C, PORTRAIT));
  });

  it("round-trips exactly on the same image (no per-gesture drift)", () => {
    let t = fitTransform(C, LANDSCAPE);
    t = zoomAtPoint(t, C, LANDSCAPE, { x: 1000, y: 150 }, 1);
    t = zoomAtPoint(t, C, LANDSCAPE, { x: 300, y: 700 }, 2.3);
    const back = carryOver(toSession(t, C, LANDSCAPE, "free"), C, LANDSCAPE);
    expect(back.scale).toBeCloseTo(t.scale, 12);
    expect(back.tx).toBeCloseTo(t.tx, 9);
    expect(back.ty).toBeCloseTo(t.ty, 9);
  });

  it("actual (1:1) carries across a LANDSCAPE → PORTRAIT aspect change", () => {
    // Punch into the image center at 100%, then "arrow" to an image of
    // opposite aspect: still 100%, and the SAME image fraction sits at the
    // container center — the Appendix-B regression. (Interior anchor: no
    // edge constraint binds.)
    let t = fitTransform(C, LANDSCAPE);
    t = zoomAtPoint(t, C, LANDSCAPE, { x: 600, y: 400 }, 1);
    const s = toSession(t, C, LANDSCAPE, "actual");
    const next = carryOver(s, C, PORTRAIT);
    expect(next.scale).toBe(1);
    const centered = screenPoint(next, {
      x: s.centerFrac.x * PORTRAIT.w,
      y: s.centerFrac.y * PORTRAIT.h,
    });
    expect(centered.x).toBeCloseTo(C.w / 2, 9);
    expect(centered.y).toBeCloseTo(C.h / 2, 9);

    // A near-corner anchor cannot sit at center: clampOffsets pins the
    // image edges to the container edges instead (same rule as panning).
    const corner = toSession(
      zoomAtPoint(fitTransform(C, LANDSCAPE), C, LANDSCAPE, { x: 1100, y: 750 }, 1),
      C,
      LANDSCAPE,
      "actual",
    );
    const clamped = carryOver(corner, C, PORTRAIT);
    expect(clamped.tx).toBe(C.w - PORTRAIT.w); // x pins to the right edge
    // y stays interior on this geometry: the fraction still sits at center.
    const cy = screenPoint(clamped, { x: 0, y: corner.centerFrac.y * PORTRAIT.h }).y;
    expect(cy).toBeCloseTo(C.h / 2, 9);
  });

  it("free zoom keeps the ABSOLUTE magnification across images", () => {
    const s = toSession(
      zoomAtPoint(fitTransform(C, LANDSCAPE), C, LANDSCAPE, { x: 600, y: 400 }, 0.9),
      C,
      LANDSCAPE,
      "free",
    );
    expect(carryOver(s, C, PORTRAIT).scale).toBeCloseTo(0.9, 12);
    expect(carryOver(s, C, WIDE).scale).toBeCloseTo(0.9, 12);
  });

  it("free zoom re-clamps per image (a deep zoom-out can't strand a small fit)", () => {
    const s = { mode: "free" as const, scale: 0.05, centerFrac: { x: 0.5, y: 0.5 } };
    const small = dims(600, 400); // fit 2 here → floor is min(fit,1)/2 = 0.5
    expect(carryOver(s, C, small).scale).toBeCloseTo(0.5, 12);
  });

  it("centerFrac 0.5/0.5 centers any image at any scale", () => {
    const s = { mode: "free" as const, scale: 1, centerFrac: { x: 0.5, y: 0.5 } };
    const t = carryOver(s, C, PORTRAIT);
    expect(t.tx).toBeCloseTo((C.w - PORTRAIT.w) / 2, 9);
    expect(t.ty).toBeCloseTo((C.h - PORTRAIT.h) / 2, 9);
  });
});

describe("clampOffsets — centering + pan clamp (founder dogfood round 1)", () => {
  const container = { w: 1000, h: 800 };
  const image = { w: 2000, h: 1000 }; // fit scale = 0.5 (width-bound)

  it("centers both axes once the whole image fits", () => {
    // scale 0.3: 600×300 inside 1000×800 — wherever panning left it.
    const t = clampOffsets({ scale: 0.3, tx: -120, ty: 999 }, container, image);
    expect(t.tx).toBeCloseTo((1000 - 600) / 2);
    expect(t.ty).toBeCloseTo((800 - 300) / 2);
  });

  it("centers per axis: height fits while width overflows", () => {
    // scale 0.6: 1200×600 — x overflows (clamped), y fits (centered).
    const t = clampOffsets({ scale: 0.6, tx: 50, ty: -500 }, container, image);
    expect(t.tx).toBe(0); // clamp: offsets sit in [container - scaled, 0]
    expect(t.ty).toBeCloseTo((800 - 600) / 2);
  });

  it("clamps panning so edges never detach while zoomed in", () => {
    // scale 1: 2000×1000 overflows both. Legal tx ∈ [-1000, 0], ty ∈ [-200, 0].
    expect(clampOffsets({ scale: 1, tx: 80, ty: 10 }, container, image)).toEqual({
      scale: 1,
      tx: 0,
      ty: 0,
    });
    expect(
      clampOffsets({ scale: 1, tx: -1500, ty: -900 }, container, image),
    ).toEqual({ scale: 1, tx: -1000, ty: -200 });
    // In-range offsets pass through untouched.
    expect(
      clampOffsets({ scale: 1, tx: -400, ty: -100 }, container, image),
    ).toEqual({ scale: 1, tx: -400, ty: -100 });
  });

  it("carryOver output is always normalized (zoom out below fill re-centers)", () => {
    // A free session aimed at the image corner at a scale where it all fits.
    const t = carryOver(
      { mode: "free", scale: 0.25, centerFrac: { x: 0.05, y: 0.05 } },
      container,
      image,
    );
    expect(t.tx).toBeCloseTo((1000 - 500) / 2);
    expect(t.ty).toBeCloseTo((800 - 250) / 2);
  });
});

describe("bottom-edge resize — the filmstrip PUSHES the viewport (polish)", () => {
  // Toggling the filmstrip shrinks the stage container by the strip's
  // height (it is an edge panel now, never an overlay). The session is
  // canonical and dimension-independent, so LookStage's derived transform
  // re-runs THIS carryOver against the new container — fit refits, a live
  // zoom keeps its magnification, and clampOffsets (U13) re-normalizes;
  // PencilOverlay redraws because its draw effect keys on (t, container).
  const SHRUNK = dims(C.w, C.h - 80);

  it("fit recomputes against the shrunk container, and back on untoggle", () => {
    expect(carryOver(null, SHRUNK, LANDSCAPE)).toEqual(fitTransform(SHRUNK, LANDSCAPE));
    expect(fitScale(SHRUNK, LANDSCAPE)).toBeLessThan(fitScale(C, LANDSCAPE));
    expect(carryOver(null, C, LANDSCAPE)).toEqual(fitTransform(C, LANDSCAPE));
  });

  it("an in-progress zoom session survives: same scale, centerFrac at the new center", () => {
    const t = zoomAtPoint(fitTransform(C, LANDSCAPE), C, LANDSCAPE, { x: 700, y: 250 }, 2.3);
    const s = toSession(t, C, LANDSCAPE, "free");
    const resized = carryOver(s, SHRUNK, LANDSCAPE);
    expect(resized.scale).toBeCloseTo(2.3, 12); // the resize never re-zooms
    // The image fraction that sat at the OLD container center sits at the
    // NEW one (interior point: no edge constraint binds here).
    const p = screenPoint(resized, {
      x: s.centerFrac.x * LANDSCAPE.w,
      y: s.centerFrac.y * LANDSCAPE.h,
    });
    expect(p.x).toBeCloseTo(SHRUNK.w / 2, 9);
    expect(p.y).toBeCloseTo(SHRUNK.h / 2, 9);
  });

  it("a session punched into the bottom edge re-clamps: no gap opens under the image", () => {
    const s = { mode: "free" as const, scale: 1, centerFrac: { x: 0.5, y: 0.99 } };
    const resized = carryOver(s, SHRUNK, LANDSCAPE);
    // Unclamped ty would strand the bottom edge above the container's;
    // clampOffsets pins it (the U13 overflow rule, same as panning).
    expect(resized.ty).toBe(SHRUNK.h - LANDSCAPE.h * resized.scale);
    expect(resized.tx).toBeCloseTo(SHRUNK.w / 2 - 0.5 * LANDSCAPE.w, 9);
  });
});
