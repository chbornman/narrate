/**
 * Grease-pencil geometry (logic/stroke.ts + logic/pencil.ts) — the
 * CAPTURE §13.3 acceptance gate as property tests:
 *
 *   a stroke drawn at ANY zoom/pan, stored (quantized + clamped), and
 *   re-rendered at ANY OTHER zoom/pan lands within 1 SOURCE-image pixel
 *   of the original image-space path; p[]/t[] round-trip exactly; the
 *   payload validates against §8.2.
 *
 * Plus the §8.4 threshold truth table, the §8.6 hit-test radius (12-px
 * floor included), the long-edge-unit conversion trap on non-square
 * images in BOTH orientations, and §8.1 orientation compensation.
 */
import { describe, expect, it } from "vitest";
import type { Dims, Point, ZoomTransform } from "../src/lib/logic/zoom";
import * as stroke from "../src/lib/logic/stroke";
import {
  classifyPenDown,
  decimate,
  penDown,
  penMove,
  penUp,
  type PenState,
} from "../src/lib/logic/pencil";
import type { StrokePayloadWire } from "../src/lib/types/dto";

// ---- deterministic PRNG (registry.test.ts pattern) ---------------------------

function mulberry32(seed: number) {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ---- the simulated Look ------------------------------------------------------
//
// SOURCE image vs PREVIEW: Look draws over the cached display preview;
// normalized fractions are identical across the two (same aspect), and
// the §13.3 tolerance is measured in SOURCE pixels.

const SOURCE: Dims = { w: 6000, h: 4000 };
const PREVIEW: Dims = { w: 3000, h: 2000 };
const PORTRAIT_SOURCE: Dims = { w: 4000, h: 6000 };
const PORTRAIT_PREVIEW: Dims = { w: 2000, h: 3000 };

/** Original image-space path in normalized fractions (a hooked curve that
 * exercises both axes), with per-point pressure and time. */
function originalPath(n = 60): { frac: Point; pressure: number; t: number }[] {
  const out = [];
  for (let i = 0; i < n; i++) {
    const u = i / (n - 1);
    out.push({
      frac: {
        x: 0.2 + 0.55 * u,
        y: 0.3 + 0.35 * Math.sin(u * Math.PI * 1.5),
      },
      pressure: 0.3 + 0.5 * u,
      t: i * 8,
    });
  }
  return out;
}

/** "Draw" the path through the machine at a given transform: fractions →
 * preview px → screen px (T), sampled as pointer events. */
function draw(
  t: ZoomTransform,
  preview: Dims,
  pointerType: "pen" | "mouse" = "pen",
  orientation = 1,
): StrokePayloadWire {
  const path = originalPath();
  let state: PenState | null = null;
  for (const p of path) {
    const img = { x: p.frac.x * preview.w, y: p.frac.y * preview.h };
    const screen = stroke.imageToScreen(img, t);
    const sample = {
      x: screen.x,
      y: screen.y,
      pressure: p.pressure,
      pointerType,
      timeMs: 1000 + p.t,
    };
    if (state === null) state = penDown(sample, { t, image: preview });
    else penMove(state, sample, { t, image: preview });
  }
  // B41: pen-up at the last sample's position/time — the terminal sample
  // duplicates it (dedupe-exempt; one near-duplicate point per stroke).
  const last = path[path.length - 1];
  const img = { x: last.frac.x * preview.w, y: last.frac.y * preview.h };
  const screen = stroke.imageToScreen(img, t);
  const payload = penUp(
    state!,
    orientation,
    {
      x: screen.x,
      y: screen.y,
      pressure: last.pressure,
      pointerType,
      timeMs: 1000 + last.t,
    },
    { t, image: preview },
  );
  expect(payload).not.toBeNull();
  return payload!;
}

/** Distance (SOURCE px) from a point to the original polyline. */
function distanceToOriginal(p: Point, source: Dims): number {
  const path = originalPath().map(({ frac }) => ({
    x: frac.x * source.w,
    y: frac.y * source.h,
  }));
  let min = Infinity;
  for (let i = 1; i < path.length; i++) {
    const a = path[i - 1];
    const b = path[i];
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const lenSq = dx * dx + dy * dy;
    const u = lenSq === 0 ? 0 : Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / lenSq));
    min = Math.min(min, Math.hypot(p.x - (a.x + u * dx), p.y - (a.y + u * dy)));
  }
  return min;
}

/** Re-render stored points at transform t2 and map back to SOURCE px —
 * the full store → screen → image round-trip a renderer performs. */
function reRenderedSourcePoints(
  payload: StrokePayloadWire,
  preview: Dims,
  t2: ZoomTransform,
  source: Dims,
): Point[] {
  return payload.points.map((pt) => {
    const n = stroke.wireToNorm(pt);
    const screen = stroke.imageToScreen(stroke.denormalize(n, preview), t2);
    const back = stroke.normalize(stroke.screenToImage(screen, t2), preview);
    return { x: back.x * source.w, y: back.y * source.h };
  });
}

function assertRoundTrip(t1: ZoomTransform, t2: ZoomTransform, preview: Dims, source: Dims) {
  const payload = draw(t1, preview);
  expect(stroke.validatePayload(payload)).toEqual([]);
  for (const p of reRenderedSourcePoints(payload, preview, t2, source)) {
    expect(distanceToOriginal(p, source)).toBeLessThanOrEqual(1);
  }
}

describe("CAPTURE §13.3 — stroke round-trip fidelity", () => {
  it("named case: drawn at 100% zoom, re-rendered at an unrelated zoom/pan", () => {
    const at100: ZoomTransform = { scale: 1, tx: -800, ty: -200 };
    const other: ZoomTransform = { scale: 0.31, tx: 40, ty: 12 };
    assertRoundTrip(at100, other, PREVIEW, SOURCE);
  });

  it("named case: drawn at 400% panned to a corner, re-rendered at fit", () => {
    // 400% punched into the bottom-right corner of the preview.
    const at400: ZoomTransform = { scale: 4, tx: -3000 * 4 + 1200, ty: -2000 * 4 + 800 };
    const fit: ZoomTransform = { scale: 0.4, tx: 0, ty: 0 };
    assertRoundTrip(at400, fit, PREVIEW, SOURCE);
  });

  it("randomized zoom/pan pairs, landscape AND portrait non-square images", () => {
    const rnd = mulberry32(133);
    for (let i = 0; i < 40; i++) {
      const portrait = rnd() < 0.5;
      const preview = portrait ? PORTRAIT_PREVIEW : PREVIEW;
      const source = portrait ? PORTRAIT_SOURCE : SOURCE;
      const randT = (): ZoomTransform => ({
        scale: 0.25 + rnd() * 7.75, // fit-ish through 800%
        tx: (rnd() - 0.5) * 8000,
        ty: (rnd() - 0.5) * 8000,
      });
      assertRoundTrip(randT(), randT(), preview, source);
    }
  });

  it("wire-integer pin: known samples on a known transform store exact integers", () => {
    // Pins the quantizer's rounding mode (§8.2 says only "integer
    // ten-thousandths"; a silent round→floor drift stays inside the §13.3
    // tolerance, so only an absolute pin catches it) plus the overshoot
    // clamp. t = ×2 panned; image px = (screen − tx)/2.
    const t: ZoomTransform = { scale: 2, tx: -1000, ty: -500 };
    const opts = { t, image: PREVIEW };
    const samples = [
      { x: 501, y: 301, timeMs: 1000 }, // img (750.5, 400.5) → 2501.67→2502, 2002.5→2003
      { x: 2000, y: 1200, timeMs: 1010 }, // img (1500, 850) → exact 5000, 4250
      { x: -3001, y: 2801, timeMs: 1100 }, // img (−1000.5, 1650.5) → −3335 clamps to −2500, 8252.5→8253
    ].map((s) => ({ ...s, pressure: 0.5, pointerType: "pen" }));
    const state = penDown(samples[0], opts);
    penMove(state, samples[1], opts);
    penMove(state, samples[2], opts);
    // B41: the pointer-up sample is stored as the terminal point, exempt
    // from jitter dedupe — here a byte-for-byte duplicate of the last move.
    const payload = penUp(state, 1, samples[2], opts);
    expect(payload).toEqual({
      baseW: stroke.BASE_W_DEFAULT,
      orientation: 1,
      points: [
        [2502, 2003, 500, 0],
        [5000, 4250, 500, 10],
        [-2500, 8253, 500, 100],
        [-2500, 8253, 500, 100],
      ],
      tool: "pencil",
    });
  });

  it("p[] and t[] round-trip exactly (pen pressure mapped per-mille)", () => {
    const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    const payload = draw(t, PREVIEW, "pen");
    const path = originalPath();
    // Samples >0.5px apart all survive, plus B41's terminal pen-up sample.
    expect(payload.points.length).toBe(path.length + 1);
    for (const [i, [, , p, tMs]] of payload.points.entries()) {
      const j = Math.min(i, path.length - 1); // terminal duplicates the last
      expect(p).toBe(Math.round(path[j].pressure * 1000));
      expect(tMs).toBe(path[j].t);
    }
  });

  it("a mouse stroke records p = 1000 throughout (no-pressure rule, §8.2)", () => {
    const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    const payload = draw(t, PREVIEW, "mouse");
    expect(payload.points.every((pt) => pt[2] === 1000)).toBe(true);
  });

  it("overshoot clamps to −2500..12500; the payload still validates", () => {
    // A path circling past the frame edge (allowed: circling an edge
    // subject, §8.1) — quantization clamps, validation accepts.
    const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    let state: PenState | null = null;
    let lastSample = { x: 0, y: 0, pressure: 0.5, pointerType: "mouse", timeMs: 0 };
    for (const [i, fx] of [-0.4, -0.1, 0.5, 1.1, 1.4].entries()) {
      lastSample = {
        x: fx * PREVIEW.w,
        y: -0.3 * PREVIEW.h + i,
        pressure: 0.5,
        pointerType: "mouse",
        timeMs: 1000 + i * 20,
      };
      if (state === null) state = penDown(lastSample, { t, image: PREVIEW });
      else penMove(state, lastSample, { t, image: PREVIEW });
    }
    const payload = penUp(state!, 1, { ...lastSample, timeMs: 1100 }, { t, image: PREVIEW })!;
    expect(stroke.validatePayload(payload)).toEqual([]);
    expect(payload.points[0][0]).toBe(-2500);
    expect(payload.points[4][0]).toBe(12500);
    expect(payload.points[5][0]).toBe(12500); // B41 terminal sample, clamped too
    expect(payload.points.every((pt) => pt[1] === -2500)).toBe(true);
  });
});

describe("CAPTURE §8.4 — commit threshold truth table", () => {
  const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };

  function tap(spanPx: number, durationMs: number): StrokePayloadWire | null {
    const state = penDown(
      { x: 100, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 1000 },
      { t, image: PREVIEW },
    );
    const end = {
      x: 100 + spanPx,
      y: 100,
      pressure: 0.5,
      pointerType: "mouse",
      timeMs: 1000 + durationMs,
    };
    penMove(state, end, { t, image: PREVIEW });
    return penUp(state, 1, end, { t, image: PREVIEW });
  }

  it("tiny AND brief → discarded (a fleeting accidental tap)", () => {
    // 2 preview px on a 3000-px long edge ≈ 0.00067 LE < 0.003; 40 ms < 100.
    expect(tap(2, 40)).toBeNull();
  });

  it("tiny but SLOW → commits (a deliberate press-and-hold dot)", () => {
    expect(tap(2, 200)).not.toBeNull();
  });

  it("a held dot with NO movement at all commits (duration = pen-up time)", () => {
    const down = { x: 100, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 1000 };
    const state = penDown(down, { t, image: PREVIEW });
    const held = penUp(state, 1, { ...down, timeMs: 1250 }, { t, image: PREVIEW });
    expect(held).not.toBeNull(); // held 250 ms
    // B41: the dot's dwell is now IN the payload — ts − t_last is exact.
    expect(held!.points).toEqual([
      [333, 500, 1000, 0],
      [333, 500, 1000, 250],
    ]);
    expect(penUp(state, 1, { ...down, timeMs: 1040 }, { t, image: PREVIEW })).toBeNull(); // fleeting 40 ms
  });

  it("long but brief → commits (a fast flick is a real mark)", () => {
    // 0.003 LE on the 3000-px long edge = 9 px; 60 px is well past it.
    expect(tap(60, 50)).not.toBeNull();
  });
});

describe("§8.3 capture reduction — jitter dedupe and the 8192 bound", () => {
  const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };

  it("drops consecutive samples closer than 0.5 screen px", () => {
    const state = penDown(
      { x: 100, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 0 },
      { t, image: PREVIEW },
    );
    penMove(state, { x: 100.3, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 5 }, { t, image: PREVIEW });
    penMove(state, { x: 100.4, y: 100.2, pressure: 0.5, pointerType: "mouse", timeMs: 10 }, { t, image: PREVIEW });
    penMove(state, { x: 101, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 15 }, { t, image: PREVIEW });
    expect(state.points.length).toBe(2); // down + the 1-px move
  });

  it("recomputes the dedupe baseline when the transform changes mid-stroke", () => {
    // Pen-down at screen (100,100) under 1:1 → image (100,100). A wheel
    // zoom to ×2 mid-stroke moves that kept point to screen (200,200);
    // the baseline must follow it (the P5.1 review bug compared against
    // the stale pre-zoom screen point).
    const t1: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    const t2: ZoomTransform = { scale: 2, tx: 0, ty: 0 };
    const state = penDown(
      { x: 100, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 0 },
      { t: t1, image: PREVIEW },
    );
    // 0.3 px from the RE-PROJECTED baseline: jitter, must drop (the stale
    // baseline reads it as a ~141-px move and would keep it).
    penMove(
      state,
      { x: 200.3, y: 200, pressure: 0.5, pointerType: "mouse", timeMs: 10 },
      { t: t2, image: PREVIEW },
    );
    expect(state.points.length).toBe(1);
    // 0.3 px from the STALE baseline but a real ~50-image-px move under
    // ×2: must be kept (the stale baseline would drop it).
    penMove(
      state,
      { x: 100.3, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 20 },
      { t: t2, image: PREVIEW },
    );
    expect(state.points.length).toBe(2);
    expect(state.points[1].n.x).toBeCloseTo(50.15 / PREVIEW.w, 9);
    expect(state.points[1].n.y).toBeCloseTo(50 / PREVIEW.h, 9);
  });

  it("a mid-stroke PAN moves the baseline too (translation, not just scale)", () => {
    const t1: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    const panned: ZoomTransform = { scale: 1, tx: 5, ty: 0 }; // Space-pan mid-stroke
    const state = penDown(
      { x: 100, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 0 },
      { t: t1, image: PREVIEW },
    );
    // The kept point now sits at screen (105,100): 0.2 px away is jitter.
    penMove(
      state,
      { x: 105.2, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 10 },
      { t: panned, image: PREVIEW },
    );
    expect(state.points.length).toBe(1);
    // 0.2 px from the stale baseline is a real 4.8-px move now: kept.
    penMove(
      state,
      { x: 100.2, y: 100, pressure: 0.5, pointerType: "mouse", timeMs: 20 },
      { t: panned, image: PREVIEW },
    );
    expect(state.points.length).toBe(2);
    expect(state.points[1].n.x).toBeCloseTo(95.2 / PREVIEW.w, 9);
  });

  it("t offsets start at 0 and never decrease, even on misordered clocks", () => {
    const state = penDown(
      { x: 0, y: 0, pressure: 0.5, pointerType: "mouse", timeMs: 1000 },
      { t, image: PREVIEW },
    );
    penMove(state, { x: 10, y: 0, pressure: 0.5, pointerType: "mouse", timeMs: 1020 }, { t, image: PREVIEW });
    penMove(state, { x: 20, y: 0, pressure: 0.5, pointerType: "mouse", timeMs: 1015 }, { t, image: PREVIEW });
    expect(state.points.map((p) => p.t)).toEqual([0, 20, 20]);
  });

  it("over-8192 capture decimates (first/last kept, bound held, t ordered)", () => {
    const state = penDown(
      { x: 0, y: 0, pressure: 0.5, pointerType: "mouse", timeMs: 0 },
      { t, image: PREVIEW },
    );
    for (let i = 1; i < 9500; i++) {
      penMove(
        state,
        { x: i, y: (i * 7) % 1000, pressure: 0.5, pointerType: "mouse", timeMs: i },
        { t, image: PREVIEW },
      );
    }
    expect(state.points.length).toBeLessThanOrEqual(8192);
    expect(state.points[0].t).toBe(0);
    expect(state.points[state.points.length - 1].t).toBe(9499); // the tail survives
    for (let i = 1; i < state.points.length; i++)
      expect(state.points[i].t).toBeGreaterThanOrEqual(state.points[i - 1].t);
    const payload = penUp(
      state,
      1,
      { x: 9499, y: (9499 * 7) % 1000, pressure: 0.5, pointerType: "mouse", timeMs: 9500 },
      { t, image: PREVIEW },
    )!;
    expect(stroke.validatePayload(payload)).toEqual([]);
    expect(payload.points.length).toBeLessThanOrEqual(8192); // bound holds with the terminal sample
  });

  it("decimate keeps endpoints exactly", () => {
    const pts = Array.from({ length: 9 }, (_, i) => ({
      n: { x: i, y: i },
      p: 1000,
      t: i,
    }));
    const out = decimate(pts);
    expect(out[0]).toBe(pts[0]);
    expect(out[out.length - 1]).toBe(pts[8]);
    expect(out.length).toBe(5);
  });
});

describe("pointer-down intent — the button gate precedes eraser intent (P5.1 review)", () => {
  it("button 0 draws bare and erases under hold-E", () => {
    expect(classifyPenDown(0, 1, false)).toBe("draw");
    expect(classifyPenDown(0, 1, true)).toBe("erase");
  });

  it("the stylus eraser end always erases (button 5 / buttons bit 32 — B45)", () => {
    expect(classifyPenDown(5, 32, false)).toBe("erase");
    expect(classifyPenDown(5, 32, true)).toBe("erase");
    expect(classifyPenDown(0, 33, false)).toBe("erase"); // bit alongside contact
  });

  it("right/middle-click PASS to the backdrop menu seat — E held or not", () => {
    // The review bug: eraser intent evaluated first made right/middle
    // erase under hold-E, eating the look-backdrop menu's click.
    expect(classifyPenDown(2, 2, true)).toBe("pass");
    expect(classifyPenDown(2, 2, false)).toBe("pass");
    expect(classifyPenDown(1, 4, true)).toBe("pass");
    expect(classifyPenDown(1, 4, false)).toBe("pass");
    // X1/X2 side buttons are not erase gestures either.
    expect(classifyPenDown(3, 8, true)).toBe("pass");
    expect(classifyPenDown(4, 16, true)).toBe("pass");
  });
});

describe("long-edge-unit conversion (the per-axis trap, non-square both ways)", () => {
  it("converts per-axis fractions through W/longEdge and H/longEdge", () => {
    // Landscape 6000×4000: Δy = 0.5 of height = 2000 px = 1/3 long edge.
    expect(stroke.leDistance({ x: 0, y: 0 }, { x: 0, y: 0.5 }, SOURCE)).toBeCloseTo(1 / 3, 12);
    // Portrait 4000×6000: Δy = 0.5 of height = 3000 px = 1/2 long edge.
    expect(
      stroke.leDistance({ x: 0, y: 0 }, { x: 0, y: 0.5 }, PORTRAIT_SOURCE),
    ).toBeCloseTo(1 / 2, 12);
    // Δx = 0.5 of width: 1/2 LE landscape, 1/3 LE portrait.
    expect(stroke.leDistance({ x: 0, y: 0 }, { x: 0.5, y: 0 }, SOURCE)).toBeCloseTo(1 / 2, 12);
    expect(
      stroke.leDistance({ x: 0, y: 0 }, { x: 0.5, y: 0 }, PORTRAIT_SOURCE),
    ).toBeCloseTo(1 / 3, 12);
  });

  it("the same physical gesture commits on both orientations of one sensor", () => {
    // 10 px on the 3000-px long edge ≈ 0.0033 LE — just past the threshold
    // on landscape AND portrait previews (per-axis normalization differs;
    // long-edge length must not).
    for (const preview of [PREVIEW, PORTRAIT_PREVIEW]) {
      const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
      const state = penDown(
        { x: 50, y: 50, pressure: 0.5, pointerType: "mouse", timeMs: 0 },
        { t, image: preview },
      );
      const end = { x: 50, y: 60, pressure: 0.5, pointerType: "mouse", timeMs: 30 };
      penMove(state, end, { t, image: preview });
      expect(penUp(state, 1, end, { t, image: preview }), `preview ${preview.w}×${preview.h}`).not.toBeNull();
    }
  });
});

describe("CAPTURE §8.6 — eraser hit-test", () => {
  // A horizontal stroke across the middle, and an older overlapping twin.
  const line = (id: string): stroke.ErasableStroke => ({
    id,
    orientation: 1,
    points: [
      [2000, 5000, 1000, 0],
      [8000, 5000, 1000, 50],
    ],
  });
  const strokes = [line("01OLD"), line("01ZNEW")];

  it("hits within 0.01 long-edge units at working zooms", () => {
    // 0.008 LE below the line: tap y = 0.5 + 0.008·(longEdge/h).
    const tap = { x: 0.5, y: 0.5 + 0.008 * (6000 / 4000) };
    expect(stroke.pickEraserTarget(tap, strokes, SOURCE, 1, 1)).toBe("01ZNEW");
    // 0.013 LE away: miss.
    const far = { x: 0.5, y: 0.5 + 0.013 * (6000 / 4000) };
    expect(stroke.pickEraserTarget(far, strokes, SOURCE, 1, 1)).toBeNull();
  });

  it("the 12-screen-px floor takes over when zoomed far out", () => {
    // At s = 0.05 over a 6000-px long edge: 12/(0.05·6000) = 0.04 LE.
    const tap = { x: 0.5, y: 0.5 + 0.03 * (6000 / 4000) };
    expect(stroke.pickEraserTarget(tap, strokes, SOURCE, 0.05, 1)).toBe("01ZNEW");
    // The same tap at 100% is far outside the 0.01 radius.
    expect(stroke.pickEraserTarget(tap, strokes, SOURCE, 1, 1)).toBeNull();
    expect(stroke.eraserRadiusLE(SOURCE, 0.05)).toBeCloseTo(0.04, 12);
    expect(stroke.eraserRadiusLE(SOURCE, 1)).toBe(0.01);
  });

  it("among eligible strokes the most recent (latest event id) wins", () => {
    const tap = { x: 0.5, y: 0.5 };
    expect(stroke.pickEraserTarget(tap, strokes, SOURCE, 1, 1)).toBe("01ZNEW");
    expect(stroke.pickEraserTarget(tap, [line("01OLD")], SOURCE, 1, 1)).toBe("01OLD");
  });

  it("a single-point stroke (a dot) is erasable", () => {
    const dot: stroke.ErasableStroke = {
      id: "01DOT",
      orientation: 1,
      points: [[5000, 5000, 1000, 0]],
    };
    expect(stroke.pickEraserTarget({ x: 0.5, y: 0.5 }, [dot], SOURCE, 1, 1)).toBe("01DOT");
  });
});

describe("§8.1 orientation — recorded at draw time, compensated at render", () => {
  it("remapOrientation round-trips through every EXIF orientation", () => {
    const rnd = mulberry32(7);
    for (let o = 1; o <= 8; o++) {
      for (let i = 0; i < 20; i++) {
        const p = { x: rnd() * 1.5 - 0.25, y: rnd() * 1.5 - 0.25 }; // overshoot included
        const there = stroke.remapOrientation(p, 1, o);
        const back = stroke.remapOrientation(there, o, 1);
        expect(back.x).toBeCloseTo(p.x, 12);
        expect(back.y).toBeCloseTo(p.y, 12);
      }
    }
  });

  it("a stroke recorded under rotation 6 lands on the same raw pixels under 1", () => {
    // Raw sensor 6000×4000; orientation 6 display = 4000×6000 (90° CW):
    // raw (u,v) appears at display (1−v, u).
    const raw = { u: 0.25, v: 0.6 };
    const recordedUnder6 = { x: 1 - raw.v, y: raw.u };
    const under1 = stroke.remapOrientation(recordedUnder6, 6, 1);
    expect(under1.x).toBeCloseTo(raw.u, 12);
    expect(under1.y).toBeCloseTo(raw.v, 12);
    // And the screen-spec path applies the same compensation.
    const t: ZoomTransform = { scale: 1, tx: 0, ty: 0 };
    const spec = stroke.strokeScreenSpec(
      [[Math.round(recordedUnder6.x * 10000), Math.round(recordedUnder6.y * 10000), 1000, 0]],
      40,
      SOURCE, // orientation-1 display dims
      t,
      6,
      1,
    );
    expect(spec.pts[0].x).toBeCloseTo(raw.u * SOURCE.w, 0);
    expect(spec.pts[0].y).toBeCloseTo(raw.v * SOURCE.h, 0);
  });
});

describe("§8.2 width model", () => {
  it("w(i) = base_w × (0.4 + 0.6·p/1000); on-screen width scales with s", () => {
    expect(stroke.widthLE(1000, 40)).toBe(40); // no pressure = nominal
    expect(stroke.widthLE(0, 40)).toBeCloseTo(16, 12); // thins to 40%
    expect(stroke.widthLE(500, 40)).toBeCloseTo(28, 12);
    // 40/10000 of the 6000-px long edge = 24 image px; ×s on screen.
    expect(stroke.screenWidth(1000, 40, SOURCE, 1)).toBeCloseTo(24, 12);
    expect(stroke.screenWidth(1000, 40, SOURCE, 4)).toBeCloseTo(96, 12);
  });
});

describe("render-path generation (centripetal Catmull-Rom, render-only)", () => {
  it("interpolates THROUGH every stored point", () => {
    const pts: Point[] = [
      { x: 0, y: 0 },
      { x: 10, y: 8 },
      { x: 25, y: 2 },
      { x: 40, y: 14 },
    ];
    const segs = stroke.catmullRomBeziers(pts);
    expect(segs.length).toBe(3);
    for (const [i, s] of segs.entries()) {
      expect(s.p0).toEqual(pts[i]);
      expect(s.p1).toEqual(pts[i + 1]);
    }
  });

  it("degenerate (coincident) points do not produce NaN controls", () => {
    const segs = stroke.catmullRomBeziers([
      { x: 5, y: 5 },
      { x: 5, y: 5 },
      { x: 9, y: 9 },
    ]);
    for (const s of segs)
      for (const p of [s.c1, s.c2]) {
        expect(Number.isFinite(p.x)).toBe(true);
        expect(Number.isFinite(p.y)).toBe(true);
      }
  });

  it("svgPathFor renders a dot for a single point and curves otherwise", () => {
    expect(stroke.svgPathFor([{ x: 3, y: 4 }])).toBe("M 3.0 4.0 L 3.0 4.0");
    const d = stroke.svgPathFor([
      { x: 0, y: 0 },
      { x: 10, y: 10 },
      { x: 20, y: 0 },
    ]);
    expect(d.startsWith("M 0.0 0.0 C ")).toBe(true);
    expect(d.match(/C /g)?.length).toBe(2);
  });
});
