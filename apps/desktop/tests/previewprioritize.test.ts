/**
 * Viewport-first preview generation (OD-2) — the FRONTEND filter + debounce.
 *
 * The grid sends the hashes the user is scrolled to whose thumbnail isn't made
 * yet, so the backend generates those previews first. These tests prove the
 * three guards that keep it cheap and non-fighting:
 *   - only previews-missing hashes go out (a drawable thumb is never re-asked);
 *   - both pair members count and the set is deduped;
 *   - a fast scroll coalesces into ONE call, and an unchanged window is skipped.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import {
  visibleMissingHashes,
  PreviewPrioritizer,
} from "../src/lib/logic/previewprioritize";
import type { DisplayUnit } from "../src/lib/types/display";
import type { GridItem } from "../src/lib/types/dto";

// A grid item with just the fields this logic reads; `previewReady` is the
// gate (false = no thumb artifact yet).
function gi(hash: string, previewReady: boolean | undefined): GridItem {
  return {
    hash,
    fileName: `${hash}.jpg`,
    relPath: `a/${hash}.jpg`,
    captureTs: null,
    addedTs: "2026-06-01T00:00:00Z",
    hasJournal: false,
    rating: null,
    offline: false,
    previewReady,
  };
}

function unit(
  primary: GridItem,
  alt: GridItem | null = null,
): DisplayUnit {
  return { primary, alt };
}

describe("visibleMissingHashes", () => {
  it("returns only previews-missing hashes, skipping ready/absent", () => {
    const units = [
      unit(gi("missing1", false)),
      unit(gi("ready", true)),
      unit(gi("absent", undefined)), // fixture predates the flag = treat as ready
      unit(gi("missing2", false)),
    ];
    expect(visibleMissingHashes(units)).toEqual(["missing1", "missing2"]);
  });

  it("includes BOTH pair members and dedupes", () => {
    const units = [
      unit(gi("jpeg", false), gi("raw", false)), // both missing
      unit(gi("jpeg2", false), gi("jpeg", false)), // "jpeg" repeats -> once
    ];
    expect(visibleMissingHashes(units)).toEqual(["jpeg", "raw", "jpeg2"]);
  });

  it("empty when nothing visible is missing", () => {
    expect(visibleMissingHashes([unit(gi("a", true))])).toEqual([]);
    expect(visibleMissingHashes([])).toEqual([]);
  });
});

describe("PreviewPrioritizer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("debounces a fast scroll into one send of the final window", () => {
    const send = vi.fn();
    const p = new PreviewPrioritizer(send, 120);

    p.notify([unit(gi("a", false))]);
    vi.advanceTimersByTime(50);
    p.notify([unit(gi("b", false))]); // re-scroll before settle
    vi.advanceTimersByTime(50);
    p.notify([unit(gi("c", false))]);
    expect(send).not.toHaveBeenCalled(); // still settling

    vi.advanceTimersByTime(120);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(["c"]); // only the final window
  });

  it("sends only the visible-missing hashes, not already-ready ones", () => {
    const send = vi.fn();
    const p = new PreviewPrioritizer(send, 120);
    p.notify([
      unit(gi("missing", false)),
      unit(gi("ready", true)),
    ]);
    vi.advanceTimersByTime(120);
    expect(send).toHaveBeenCalledExactlyOnceWith(["missing"]);
  });

  it("skips an unchanged window since the last send", () => {
    const send = vi.fn();
    const p = new PreviewPrioritizer(send, 120);

    p.notify([unit(gi("a", false))]);
    vi.advanceTimersByTime(120);
    expect(send).toHaveBeenCalledTimes(1);

    // Same visible-missing set (an idle re-render) -> no second call.
    p.notify([unit(gi("a", false))]);
    vi.advanceTimersByTime(120);
    expect(send).toHaveBeenCalledTimes(1);

    // A genuinely new window -> sends again.
    p.notify([unit(gi("a", false)), unit(gi("b", false))]);
    vi.advanceTimersByTime(120);
    expect(send).toHaveBeenCalledTimes(2);
    expect(send).toHaveBeenLastCalledWith(["a", "b"]);
  });

  it("never sends when nothing is missing", () => {
    const send = vi.fn();
    const p = new PreviewPrioritizer(send, 120);
    p.notify([unit(gi("a", true))]);
    vi.advanceTimersByTime(120);
    expect(send).not.toHaveBeenCalled();
  });

  it("reset cancels a pending send", () => {
    const send = vi.fn();
    const p = new PreviewPrioritizer(send, 120);
    p.notify([unit(gi("a", false))]);
    p.reset();
    vi.advanceTimersByTime(120);
    expect(send).not.toHaveBeenCalled();
  });
});
