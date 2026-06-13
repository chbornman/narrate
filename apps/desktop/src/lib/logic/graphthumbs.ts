/**
 * Thumbnail image cache for the semantic topic-graph lens
 * (DESIGN-SEMANTIC-GRAPH.md). The graph draws each image node as a TINY preview
 * thumbnail instead of a plain dot, so a cluster reads as images, not an abstract
 * blob. This module owns the lazy-load + bounded-concurrency + per-hash cache so
 * TopicGraph.svelte stays a thin renderer: it asks `request(hash, priority)`,
 * draws the cached `HTMLImageElement` when `get(hash)` is ready, and repaints on
 * the cache's `onReady` notification.
 *
 * WHY a bounded queue: a full-library scope can hold thousands of image nodes.
 * Firing thousands of <img> loads at once floods the photoproof:// protocol
 * handler (the same flood the grid's previewReady gate avoids — founder, SMB,
 * June 2026) and starves the nodes actually on screen. Instead a small fixed
 * number load CONCURRENTLY, and pending requests are served NEAREST-FIRST (lowest
 * `priority` = most visible), so the viewport fills before off-screen nodes.
 *
 * The loading itself uses the DOM `Image()` element + the photoproof:// thumb URL
 * (the SMALL preview tier, ipc/urls.thumbUrl — same artifact the grid cell loads),
 * so the webview's immutable HTTP cache backs repeat views. The PURE pieces — the
 * priority queue's ordering + bounded-concurrency bookkeeping (logic/thumbqueue.ts)
 * and the node draw-size mapping (below) — are split out so they unit-test without
 * a DOM. The Image() plumbing here is the thin, hard-to-test shell.
 */

import { thumbUrl } from "../ipc/urls";
import { ThumbQueue } from "./thumbqueue";

/** How many thumb loads may be in flight at once. Small on purpose: enough to
 * keep the pipe full without flooding the protocol handler (mirrors the grid's
 * restraint on a network volume). A tunable const, not magic at the call site. */
const MAX_CONCURRENT = 6;

/** The per-hash load state. A node draws its placeholder dot until its entry
 * reaches "ready"; "error" settles permanently on the placeholder (no retry
 * storm — a missing graph thumb is cosmetic, unlike a grid cell). */
type Entry =
  | { state: "loading"; img: HTMLImageElement }
  | { state: "ready"; img: HTMLImageElement }
  | { state: "error" };

/**
 * A lazy, bounded thumbnail cache. One instance per graph view (created in
 * onMount, disposed on teardown). The render loop calls `request` for the nodes
 * it wants and `get` to fetch a ready image to draw; the cache calls back via
 * `onReady` when a load completes so the canvas repaints (off the render path).
 */
export class GraphThumbCache {
  private readonly entries = new Map<string, Entry>();
  private readonly queue = new ThumbQueue();
  private inFlight = 0;
  private disposed = false;

  /**
   * @param onReady Called (with the loaded hash) whenever a thumb finishes
   *   loading, so the host can request a repaint. Kept off the render path: a
   *   load completing schedules a draw, it does not happen mid-frame.
   * @param max Bounded concurrency (defaults to MAX_CONCURRENT) — injectable so
   *   tests can pin it.
   * @param load Image factory — injectable so tests run without a real DOM. In
   *   the app it builds a real `Image()`; a test can hand a fake.
   */
  constructor(
    private readonly onReady: (hash: string) => void,
    private readonly max: number = MAX_CONCURRENT,
    private readonly load: (hash: string) => HTMLImageElement = defaultLoad,
  ) {}

  /**
   * Ask for a hash's thumbnail at a given priority (lower = more urgent, e.g.
   * squared distance to the viewport center). A hash already loaded/erroring or
   * actively loading is a no-op; a hash already queued has its priority UPDATED
   * (so a node that scrolled into view jumps the line). New hashes enqueue, then
   * we pump up to the concurrency limit.
   */
  request(hash: string, priority: number): void {
    if (this.disposed) return;
    // Already resolved (ready/error) or actively loading: nothing to schedule.
    if (this.entries.has(hash)) return;
    this.queue.push(hash, priority);
    this.pump();
  }

  /** The ready thumbnail for a hash, or null if it is still loading, errored, or
   * never requested. The render loop draws the placeholder dot when this is
   * null and `drawImage`s the element when it is set. */
  get(hash: string): HTMLImageElement | null {
    const e = this.entries.get(hash);
    return e !== undefined && e.state === "ready" ? e.img : null;
  }

  /** Whether a hash has settled on "error" (a missing/undecodable thumb), so the
   * host can keep its placeholder without re-requesting. Exposed mostly for
   * tests + future telemetry; the render path only needs `get`. */
  hasErrored(hash: string): boolean {
    return this.entries.get(hash)?.state === "error";
  }

  /** Drop all pending (not-yet-started) requests — e.g. on a scope/topic change
   * that reseeds the node set, so stale off-screen loads don't crowd out the new
   * layout. In-flight loads finish (their result is still cached, harmless). */
  clearPending(): void {
    this.queue.clear();
  }

  /** Tear down: stop pumping and detach in-flight image handlers so a completing
   * load after teardown is a no-op (no repaint into a dead canvas). */
  dispose(): void {
    this.disposed = true;
    this.queue.clear();
    for (const e of this.entries.values()) {
      if (e.state === "loading") {
        e.img.onload = null;
        e.img.onerror = null;
      }
    }
  }

  /** Start loads up to the concurrency limit, draining the queue nearest-first.
   * Called after every enqueue and on every completion, so the pipe stays full
   * while pending work remains. */
  private pump(): void {
    while (!this.disposed && this.inFlight < this.max) {
      const hash = this.queue.pop();
      if (hash === null) return;
      // A hash can be re-popped if it was re-requested while queued; guard so we
      // never double-load the same entry.
      if (this.entries.has(hash)) continue;
      this.startLoad(hash);
    }
  }

  /** Kick off one image load, wiring its completion back into the cache. The
   * handlers are guarded against a post-dispose fire. */
  private startLoad(hash: string): void {
    const img = this.load(hash);
    this.entries.set(hash, { state: "loading", img });
    this.inFlight++;
    img.onload = () => this.settle(hash, img, true);
    img.onerror = () => this.settle(hash, img, false);
    // Set src LAST so the handlers are wired before a synchronous cache-hit can
    // fire (the webview's immutable HTTP cache can complete a repeat load fast).
    img.src = thumbUrl(hash);
  }

  /** Record a completed load (success or failure), free its slot, notify the
   * host on success, and keep pumping pending work. */
  private settle(hash: string, img: HTMLImageElement, ok: boolean): void {
    img.onload = null;
    img.onerror = null;
    this.inFlight--;
    if (this.disposed) return;
    if (ok && img.naturalWidth > 0) {
      this.entries.set(hash, { state: "ready", img });
      this.onReady(hash);
    } else {
      // A decode failure / 404 settles permanently on the placeholder. No retry
      // storm: a missing graph thumb is purely cosmetic (a dot stands in).
      this.entries.set(hash, { state: "error" });
    }
    this.pump();
  }
}

/** The real-DOM image factory (split out so the cache is DOM-free + testable). */
function defaultLoad(_hash: string): HTMLImageElement {
  // decoding=async keeps the decode off the main thread; the src is set by the
  // cache AFTER the handlers are wired so a cache-hit load can't fire first.
  const img = new Image();
  img.decoding = "async";
  return img;
}

// ---------------------------------------------------------------------------
// Node draw-size mapping — the pure geometry the canvas uses to size each
// thumbnail. Split from the draw call so it unit-tests without a 2D context.
// ---------------------------------------------------------------------------

/** The base thumbnail side (px) for a plain single-image node before any overlay
 * scaling. ~34px reads as a recognizable preview without crowding the layout. A
 * tunable const (founder tunes the lens by feel). */
export const THUMB_BASE_PX = 34;

/** A dragged/fixed single node draws slightly larger so the held node stands out
 * under the pointer (mirrors the old dot's `fixed` bump). */
export const THUMB_DRAG_PX = 40;

/** A LOD super-node's base side grows (sub-linearly) with its member count, so a
 * bigger cluster reads as a bigger stack. Clamped to [SUPER_MIN, SUPER_MAX] so a
 * one-member bin is still thumbnail-sized and a huge bin does not dominate. */
const SUPER_MIN_PX = THUMB_BASE_PX;
const SUPER_MAX_PX = 64;

/**
 * The drawn side length (px) of a node's thumbnail, BEFORE the overlay size
 * scale, given whether it is a LOD super-node (+ its member count) and whether it
 * is the dragged node. Pure: the canvas multiplies this by the overlay's
 * `sizeScale` and uses half of it as the hit radius, so the mapping is the single
 * source of truth for both draw and pick.
 */
export function nodeBaseSizePx(opts: {
  isSuper: boolean;
  memberCount: number;
  isDragged: boolean;
}): number {
  if (opts.isSuper) {
    // sqrt growth (same shape as the old super-node dot radius), mapped onto the
    // bigger thumbnail domain and clamped so a giant bin stays legible.
    const grown = SUPER_MIN_PX + Math.sqrt(Math.max(0, opts.memberCount)) * 3;
    return Math.min(SUPER_MAX_PX, Math.max(SUPER_MIN_PX, grown));
  }
  return opts.isDragged ? THUMB_DRAG_PX : THUMB_BASE_PX;
}
