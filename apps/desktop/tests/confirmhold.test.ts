/**
 * Hold-to-confirm machine (logic/confirmhold.ts — UI §8.4 step two).
 * Pure: time is a parameter, so every path is asserted without fake
 * timers — idle, press, partial release (reset, no partial credit),
 * threshold via tick AND via release (missed-tick defense), the confirmed
 * latch, and the fraction rendering.
 */
import { describe, expect, it } from "vitest";
import {
  fraction,
  HOLD_IDLE,
  HOLD_TO_CONFIRM_MS,
  press,
  release,
  tick,
} from "../src/lib/logic/confirmhold";

const T0 = 10_000;

describe("press / release", () => {
  it("press starts the hold; a second press is ignored (no restart)", () => {
    const held = press(HOLD_IDLE, T0);
    expect(held.heldSince).toBe(T0);
    expect(held.confirmed).toBe(false);
    expect(press(held, T0 + 500)).toBe(held);
  });

  it("releasing before the threshold resets to idle — no partial credit", () => {
    const held = press(HOLD_IDLE, T0);
    const released = release(held, T0 + HOLD_TO_CONFIRM_MS - 1);
    expect(released).toEqual(HOLD_IDLE);
    // A new attempt starts from zero.
    const again = press(released, T0 + 2000);
    expect(fraction(again, T0 + 2000)).toBe(0);
  });

  it("release on idle stays idle", () => {
    expect(release(HOLD_IDLE, T0)).toEqual(HOLD_IDLE);
  });
});

describe("confirmation", () => {
  it("tick confirms exactly at the threshold", () => {
    const held = press(HOLD_IDLE, T0);
    expect(tick(held, T0 + HOLD_TO_CONFIRM_MS - 1).confirmed).toBe(false);
    expect(tick(held, T0 + HOLD_TO_CONFIRM_MS).confirmed).toBe(true);
  });

  it("release past the threshold also confirms (missed final tick)", () => {
    const held = press(HOLD_IDLE, T0);
    expect(release(held, T0 + HOLD_TO_CONFIRM_MS).confirmed).toBe(true);
  });

  it("confirmed is a latch: ticks, releases, and presses cannot re-arm", () => {
    const confirmed = tick(press(HOLD_IDLE, T0), T0 + HOLD_TO_CONFIRM_MS);
    expect(tick(confirmed, T0 + 99_999)).toBe(confirmed);
    expect(release(confirmed, T0 + 99_999)).toBe(confirmed);
    expect(press(confirmed, T0 + 99_999)).toBe(confirmed);
  });

  it("tick on idle is a no-op", () => {
    expect(tick(HOLD_IDLE, T0)).toBe(HOLD_IDLE);
  });
});

describe("fraction (the fill rendering)", () => {
  it("0 at idle, linear while holding, clamped to 1, 1 when confirmed", () => {
    expect(fraction(HOLD_IDLE, T0)).toBe(0);
    const held = press(HOLD_IDLE, T0);
    expect(fraction(held, T0)).toBe(0);
    expect(fraction(held, T0 + HOLD_TO_CONFIRM_MS / 2)).toBeCloseTo(0.5);
    expect(fraction(held, T0 + HOLD_TO_CONFIRM_MS * 2)).toBe(1);
    const confirmed = tick(held, T0 + HOLD_TO_CONFIRM_MS);
    expect(fraction(confirmed, T0)).toBe(1);
  });
});
