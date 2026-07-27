/**
 * Sim-interaction state-machine invariants (PLAN-SEAM1-SIMSTATE.md Part B;
 * STATE-MACHINE.md §6b). These guard the two failure modes we kept tripping in
 * the visualizer:
 *   1. DRAG FREEZE (c8087d9) — a drag must never count as "at rest", or the rAF
 *      loop stops and the canvas freezes under the cursor.
 *   2. RE-SEED JITTER (the expandSuper bug) — at cooled heat the per-step clamp
 *      is pinned to ANNEAL_FLOOR, so a re-seed that does NOT reheat only crawls
 *      sub-pixel and visibly jitters. reheat() (heat := REHEAT_START) is what
 *      frees the clamp; this proves that premise so reseedAndRestart's pairing
 *      is grounded in the physics, not folklore.
 * The predicate + clamp were extracted PURE into forcegraph.ts precisely so they
 * are unit-testable here (the component's isSettled now delegates to isAtRest).
 */
import { describe, expect, it } from "vitest";
import {
  ANNEAL_FLOOR,
  annealedMaxStep,
  DEFAULT_MAX_STEP,
  isAtRest,
  nextSettleCount,
  REHEAT_START,
  REST_ENERGY_PER_BODY,
  SETTLE_FRAMES,
  SETTLED_HEAT,
} from "../src/lib/logic/forcegraph";

describe("isAtRest — drag holds the sim awake (c8087d9)", () => {
  // A fully-quiet, fully-cooled, long-settled layout: at rest ONLY when no drag.
  const quiet = {
    energy: 0,
    bodies: 100,
    heat: SETTLED_HEAT,
    settleCount: SETTLE_FRAMES + 1,
  };

  it("is at rest when quiet and NOT dragging", () => {
    expect(isAtRest({ ...quiet, dragging: false })).toBe(true);
  });

  it("is NEVER at rest while dragging, even when otherwise fully settled", () => {
    // The whole point: the loop must keep ticking so the dragged node redraws.
    expect(isAtRest({ ...quiet, dragging: true })).toBe(false);
  });

  it("requires ALL of low energy, cooled heat, and enough quiet frames", () => {
    // Each condition alone must veto rest (no single lucky frame settles it).
    expect(
      isAtRest({ ...quiet, dragging: false, energy: REST_ENERGY_PER_BODY * quiet.bodies }),
    ).toBe(false); // energy at/over the per-body bar
    expect(isAtRest({ ...quiet, dragging: false, heat: REHEAT_START })).toBe(false); // still hot
    expect(isAtRest({ ...quiet, dragging: false, settleCount: SETTLE_FRAMES })).toBe(false); // one frame short
  });

  it("is scale-invariant — the per-body bar holds at 5 and 5000 bodies", () => {
    const perBody = { energy: 0, heat: SETTLED_HEAT, settleCount: SETTLE_FRAMES + 1, dragging: false };
    expect(isAtRest({ ...perBody, bodies: 5 })).toBe(true);
    expect(isAtRest({ ...perBody, bodies: 5000 })).toBe(true);
    // Energy just over the bar fails at either scale.
    expect(isAtRest({ ...perBody, bodies: 5, energy: 5 * REST_ENERGY_PER_BODY })).toBe(false);
    expect(isAtRest({ ...perBody, bodies: 5000, energy: 5000 * REST_ENERGY_PER_BODY })).toBe(false);
  });
});

describe("nextSettleCount — sustained motion, not elapsed frames", () => {
  const quiet = {
    energy: 0,
    bodies: 100,
    heat: SETTLED_HEAT,
    dragging: false,
  };

  it("increments only consecutive genuinely quiet frames", () => {
    expect(nextSettleCount({ ...quiet, settleCount: 7 })).toBe(8);
  });

  it("resets after visible motion instead of accumulating a time cutoff", () => {
    expect(
      nextSettleCount({
        ...quiet,
        settleCount: SETTLE_FRAMES,
        energy: REST_ENERGY_PER_BODY * quiet.bodies,
      }),
    ).toBe(0);
  });

  it("resets while hot or dragging", () => {
    expect(
      nextSettleCount({
        ...quiet,
        settleCount: 12,
        heat: REHEAT_START,
      }),
    ).toBe(0);
    expect(
      nextSettleCount({
        ...quiet,
        settleCount: 12,
        dragging: true,
      }),
    ).toBe(0);
  });
});

describe("annealedMaxStep — why a re-seed MUST reheat", () => {
  it("pins motion to ANNEAL_FLOOR at cooled heat (≈1) — the jitter cause", () => {
    // A re-seed at cooled heat: displaced nodes can only move ANNEAL_FLOOR/step.
    expect(annealedMaxStep(1, DEFAULT_MAX_STEP)).toBeCloseTo(ANNEAL_FLOOR, 10);
  });

  it("frees the full clamp at REHEAT_START — what reheat() restores", () => {
    // reheat() sets heat := REHEAT_START, so the freshly-displaced nodes settle
    // under the full step budget instead of crawling. This is the invariant
    // reseedAndRestart enforces by always reheating before restartLoop.
    expect(annealedMaxStep(REHEAT_START, DEFAULT_MAX_STEP)).toBeCloseTo(DEFAULT_MAX_STEP, 10);
  });

  it("interpolates monotonically between floor and full as heat rises", () => {
    const mid = annealedMaxStep((REHEAT_START + 1) / 2, DEFAULT_MAX_STEP);
    expect(mid).toBeGreaterThan(ANNEAL_FLOOR);
    expect(mid).toBeLessThan(DEFAULT_MAX_STEP);
  });

  it("keeps an unbounded clamp (Infinity) unbounded at every heat", () => {
    // The divergence-proof short-circuit: Infinity·0 would be NaN at rest.
    expect(annealedMaxStep(1, Infinity)).toBe(Infinity);
    expect(annealedMaxStep(REHEAT_START, Infinity)).toBe(Infinity);
  });

  it("reheat genuinely un-settles — REHEAT_START is well above the rest heat", () => {
    // So a reheat() after a re-seed reliably flips isAtRest back to false.
    expect(REHEAT_START).toBeGreaterThan(SETTLED_HEAT);
  });
});
