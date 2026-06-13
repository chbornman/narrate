/**
 * Graph thumbnail cache (logic/graphthumbs.ts + logic/thumbqueue.ts) — the PURE
 * pieces behind drawing each topic-graph image node as a tiny preview thumbnail
 * instead of a plain dot (founder: "tiny previews as the markers"). The canvas
 * draw itself is hard to unit-test; these pin the testable helpers:
 *   - the priority queue's nearest-first ordering + dedupe/priority-lowering;
 *   - the cache's bounded concurrency + lazy load + ready/error settling, with an
 *     INJECTED fake image so it runs without a DOM;
 *   - the node draw-size mapping (single source of truth for draw + hit-test).
 */
import { describe, expect, it, vi } from "vitest";
import {
  GraphThumbCache,
  nodeBaseSizePx,
  nodeDrawRect,
  THUMB_BASE_PX,
  THUMB_DRAG_PX,
} from "../src/lib/logic/graphthumbs";
import {
  OFFSCREEN_PRIORITY_BASE,
  ThumbQueue,
  viewportPriority,
} from "../src/lib/logic/thumbqueue";

describe("ThumbQueue — nearest-first scheduling", () => {
  it("pops in ascending priority (most urgent / nearest first)", () => {
    const q = new ThumbQueue();
    q.push("far", 100);
    q.push("near", 1);
    q.push("mid", 10);
    expect(q.pop()).toBe("near");
    expect(q.pop()).toBe("mid");
    expect(q.pop()).toBe("far");
    expect(q.pop()).toBeNull();
  });

  it("dedupes a hash and keeps only its SMALLEST priority (line-jump on scroll-in)", () => {
    const q = new ThumbQueue();
    q.push("a", 50);
    q.push("a", 5); // scrolled into view: jump the line
    q.push("b", 10);
    expect(q.size).toBe(2);
    expect(q.pop()).toBe("a"); // 5 < 10
    expect(q.pop()).toBe("b");
  });

  it("never demotes a hash to a worse priority", () => {
    const q = new ThumbQueue();
    q.push("a", 5);
    q.push("a", 99); // ignored: 99 is worse than 5
    q.push("b", 10);
    expect(q.pop()).toBe("a"); // still 5 < 10
  });

  it("clear() drops every pending request", () => {
    const q = new ThumbQueue();
    q.push("a", 1);
    q.push("b", 2);
    q.clear();
    expect(q.size).toBe(0);
    expect(q.pop()).toBeNull();
  });
});

// A controllable fake image: records its assigned src and lets the test fire the
// load/error completion, so the cache's concurrency + settling are deterministic.
interface FakeImg {
  src: string;
  naturalWidth: number;
  onload: (() => void) | null;
  onerror: (() => void) | null;
  fireLoad(): void;
  fireError(): void;
}
function fakeImageFactory() {
  const made: FakeImg[] = [];
  const factory = (): HTMLImageElement => {
    const img: FakeImg = {
      src: "",
      naturalWidth: 0,
      onload: null,
      onerror: null,
      fireLoad() {
        this.naturalWidth = 64; // a decoded thumb
        this.onload?.();
      },
      fireError() {
        this.onerror?.();
      },
    };
    made.push(img);
    return img as unknown as HTMLImageElement;
  };
  return { factory, made };
}

describe("GraphThumbCache — bounded concurrency + lazy load", () => {
  it("never exceeds the concurrency limit; pending loads start as slots free", () => {
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(() => {}, 2, factory);
    // Request four nodes; only 2 may load at once.
    cache.request("a", 1);
    cache.request("b", 2);
    cache.request("c", 3);
    cache.request("d", 4);
    expect(made.length).toBe(2); // bounded

    made[0].fireLoad(); // free one slot -> the next pending (nearest) starts
    expect(made.length).toBe(3);
    made[1].fireLoad();
    expect(made.length).toBe(4);
  });

  it("serves pending loads NEAREST-FIRST regardless of request order", () => {
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(() => {}, 1, factory);
    cache.request("far", 100);
    cache.request("near", 1); // queued behind the in-flight 'far'
    cache.request("mid", 10);
    expect(made[0].src).toContain("far"); // started immediately (slot was free)
    made[0].fireLoad();
    // The nearest pending (smallest priority) starts next.
    expect(made[1].src).toContain("near");
    made[1].fireLoad();
    expect(made[2].src).toContain("mid");
  });

  it("a ready load is gettable + notifies onReady; an error settles on placeholder", () => {
    const ready = vi.fn();
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(ready, 4, factory);
    cache.request("ok", 1);
    cache.request("bad", 2);

    made[0].fireLoad();
    expect(cache.get("ok")).not.toBeNull();
    expect(ready).toHaveBeenCalledWith("ok");

    made[1].fireError();
    expect(cache.get("bad")).toBeNull(); // never a ready image
    expect(cache.hasErrored("bad")).toBe(true);
    expect(ready).toHaveBeenCalledTimes(1); // error does NOT notify
  });

  it("requesting an already-loaded/in-flight hash is a no-op (no double load)", () => {
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(() => {}, 4, factory);
    cache.request("a", 1);
    cache.request("a", 1); // in-flight: ignored
    expect(made.length).toBe(1);
    made[0].fireLoad();
    cache.request("a", 1); // already ready: ignored
    expect(made.length).toBe(1);
  });

  it("clearPending() drops queued loads but keeps in-flight + cached results", () => {
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(() => {}, 1, factory);
    cache.request("inflight", 1);
    cache.request("queued", 2);
    cache.clearPending(); // drop the queued one
    made[0].fireLoad(); // the in-flight completes -> no pending to start
    expect(made.length).toBe(1);
    expect(cache.get("inflight")).not.toBeNull();
  });

  it("dispose() drops the queue and ignores a post-dispose completion", () => {
    const ready = vi.fn();
    const { factory, made } = fakeImageFactory();
    const cache = new GraphThumbCache(ready, 4, factory);
    cache.request("a", 1);
    cache.dispose();
    made[0].onload?.(); // a late load event after teardown
    expect(ready).not.toHaveBeenCalled();
    expect(cache.get("a")).toBeNull();
  });
});

describe("nodeBaseSizePx — draw-size mapping (draw + hit-test source of truth)", () => {
  it("a plain single node is the base thumbnail side", () => {
    expect(
      nodeBaseSizePx({ isSuper: false, memberCount: 0, isDragged: false }),
    ).toBe(THUMB_BASE_PX);
  });

  it("a dragged single node draws larger", () => {
    expect(
      nodeBaseSizePx({ isSuper: false, memberCount: 0, isDragged: true }),
    ).toBe(THUMB_DRAG_PX);
  });

  it("a super-node grows with member count (monotone, sub-linear)", () => {
    const small = nodeBaseSizePx({ isSuper: true, memberCount: 4, isDragged: false });
    const big = nodeBaseSizePx({ isSuper: true, memberCount: 100, isDragged: false });
    expect(big).toBeGreaterThan(small);
    // sub-linear: 25x the members is far less than 25x the size.
    expect(big).toBeLessThan(small * 25);
  });

  it("a super-node is never smaller than a plain thumb, and is clamped at the top", () => {
    const one = nodeBaseSizePx({ isSuper: true, memberCount: 1, isDragged: false });
    const huge = nodeBaseSizePx({
      isSuper: true,
      memberCount: 1_000_000,
      isDragged: false,
    });
    expect(one).toBeGreaterThanOrEqual(THUMB_BASE_PX);
    expect(huge).toBeLessThanOrEqual(64); // SUPER_MAX_PX
  });
});

describe("nodeDrawRect — native aspect ratio (founder: not forced square)", () => {
  it("a square image draws exactly side×side (the prior behavior)", () => {
    expect(nodeDrawRect(34, 100, 100)).toEqual({ w: 34, h: 34 });
  });

  it("a landscape image is a WIDE rect: long edge = side, height scales down", () => {
    // 200x100 (2:1): width caps at side, height is half.
    expect(nodeDrawRect(40, 200, 100)).toEqual({ w: 40, h: 20 });
  });

  it("a portrait image is a TALL rect: long edge = side, width scales down", () => {
    // 100x200 (1:2): height caps at side, width is half.
    expect(nodeDrawRect(40, 100, 200)).toEqual({ w: 20, h: 40 });
  });

  it("the long edge is capped at side and the short edge never exceeds it", () => {
    const wide = nodeDrawRect(50, 300, 100);
    expect(wide.w).toBe(50); // long edge capped at side
    expect(wide.h).toBeLessThan(50); // short edge scaled down
    const tall = nodeDrawRect(50, 100, 300);
    expect(tall.h).toBe(50);
    expect(tall.w).toBeLessThan(50);
  });

  it("unknown/zero natural dims fall back to a neutral square (placeholder)", () => {
    // Before decode the natural dims are 0; the box stays square so there is no
    // layout jump when the aspect later resolves.
    expect(nodeDrawRect(34, 0, 0)).toEqual({ w: 34, h: 34 });
    expect(nodeDrawRect(34, 0, 100)).toEqual({ w: 34, h: 34 });
    expect(nodeDrawRect(34, 100, 0)).toEqual({ w: 34, h: 34 });
  });
});

describe("viewportPriority — visible-first thumb fill", () => {
  const W = 800;
  const H = 600;

  it("an in-viewport node sorts strictly AHEAD of any off-screen node", () => {
    const inView = viewportPriority(400, 300, W, H); // dead center
    const farInView = viewportPriority(10, 10, W, H); // a corner, still on screen
    const offScreen = viewportPriority(-500, 300, W, H); // off the left edge
    expect(inView).toBeLessThan(offScreen);
    expect(farInView).toBeLessThan(offScreen);
    expect(offScreen).toBeGreaterThanOrEqual(OFFSCREEN_PRIORITY_BASE);
  });

  it("within the viewport, the center fills first (nearest-to-center wins)", () => {
    const center = viewportPriority(400, 300, W, H);
    const edge = viewportPriority(780, 580, W, H);
    expect(center).toBeLessThan(edge);
    expect(center).toBe(0); // dead center: zero distance
  });

  it("the preload margin keeps a node just off the edge in the visible band", () => {
    // 50px past the right edge, within a 200px margin: still treated as visible.
    const justOff = viewportPriority(W + 50, 300, W, H, 200);
    const wayOff = viewportPriority(W + 500, 300, W, H, 200);
    expect(justOff).toBeLessThan(OFFSCREEN_PRIORITY_BASE);
    expect(wayOff).toBeGreaterThanOrEqual(OFFSCREEN_PRIORITY_BASE);
  });

  it("feeds the queue so visible nodes pop before off-screen ones", () => {
    const q = new ThumbQueue();
    q.push("offscreen", viewportPriority(-500, 300, W, H));
    q.push("edge", viewportPriority(780, 580, W, H));
    q.push("center", viewportPriority(400, 300, W, H));
    expect(q.pop()).toBe("center"); // visible, nearest center
    expect(q.pop()).toBe("edge"); // visible, farther
    expect(q.pop()).toBe("offscreen"); // off-screen, last
  });
});
