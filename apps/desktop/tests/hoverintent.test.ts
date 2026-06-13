/**
 * Hover-intent timing (primitives/hoverintent.ts): the cancelable HoverTimer
 * that opens a submenu after HOVER_OPEN_MS and defers a close by
 * HOVER_CLOSE_MS, where re-entering the child cancels the pending close.
 * Driven with vi.useFakeTimers — no DOM, no real waiting.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  HOVER_CLOSE_MS,
  HOVER_OPEN_MS,
  HoverTimer,
} from "../src/lib/primitives/hoverintent";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("HoverTimer — cancelable open/close intent", () => {
  it("fires the open intent after HOVER_OPEN_MS", () => {
    const open = vi.fn();
    const t = new HoverTimer();
    t.start(open, HOVER_OPEN_MS);
    expect(t.pending).toBe(true);
    vi.advanceTimersByTime(HOVER_OPEN_MS - 1);
    expect(open).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(open).toHaveBeenCalledTimes(1);
    expect(t.pending).toBe(false);
  });

  it("re-arming cancels the prior pending fire (only the latest wins)", () => {
    const a = vi.fn();
    const b = vi.fn();
    const t = new HoverTimer();
    t.start(a, HOVER_OPEN_MS);
    // Pointer moved to a sibling before the open fired: re-arm.
    t.start(b, HOVER_OPEN_MS);
    vi.advanceTimersByTime(HOVER_OPEN_MS);
    expect(a).not.toHaveBeenCalled();
    expect(b).toHaveBeenCalledTimes(1);
  });

  it("defers a close by HOVER_CLOSE_MS, canceled by re-entering the child", () => {
    const close = vi.fn();
    const t = new HoverTimer();
    // Leaving the parent row arms the deferred close.
    t.start(close, HOVER_CLOSE_MS);
    // The pointer reaches the child panel mid-travel and cancels it.
    vi.advanceTimersByTime(HOVER_CLOSE_MS - 1);
    t.cancel();
    vi.advanceTimersByTime(HOVER_CLOSE_MS);
    expect(close).not.toHaveBeenCalled();
    expect(t.pending).toBe(false);
  });

  it("a close NOT canceled in time still fires", () => {
    const close = vi.fn();
    const t = new HoverTimer();
    t.start(close, HOVER_CLOSE_MS);
    vi.advanceTimersByTime(HOVER_CLOSE_MS);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("the close window is longer than the open window (room to travel)", () => {
    expect(HOVER_CLOSE_MS).toBeGreaterThan(HOVER_OPEN_MS);
  });
});
