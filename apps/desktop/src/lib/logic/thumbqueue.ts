/**
 * Priority queue behind the graph thumbnail cache (logic/graphthumbs.ts). Pure +
 * DOM-free so it unit-tests the bounded-concurrency SCHEDULING in isolation: the
 * cache pumps real `Image()` loads, this decides WHICH hash loads next.
 *
 * The order matters for the lens at scale: nodes nearest the viewport (lowest
 * `priority`, e.g. squared distance to the canvas center) must load FIRST so the
 * visible region fills before off-screen work. A node that scrolls into view can
 * re-`push` with a smaller priority to jump the line; the queue keeps only the
 * SMALLEST priority seen for a hash and never queues a hash twice.
 *
 * It is a simple "scan for the min" queue, not a binary heap: the graph holds
 * dozens-to-hundreds of UNLOADED nodes at a time (loaded ones are never re-queued),
 * and a linear min-scan over that is trivially cheap and obviously correct —
 * clarity over cleverness, and no heap to get subtly wrong.
 */

/** A large constant added to an OFF-SCREEN node's priority so every node
 * currently inside the viewport sorts strictly AHEAD of every off-screen node,
 * regardless of distance-to-center (founder: initial preview loading should fill
 * what's VISIBLE first). Within each band the smaller value (nearer the center /
 * nearer the viewport edge) still wins, so the fill is visible-first then
 * nearest-first. Far larger than any realistic squared sim-distance. */
export const OFFSCREEN_PRIORITY_BASE = 1e12;

/**
 * The load priority for a node given its SCREEN position and the viewport box
 * (lower = loaded sooner). A node inside the viewport gets its squared distance
 * to the viewport CENTER (so the middle of the screen fills first); a node
 * outside gets that distance plus a large constant offset, so it always queues
 * behind every visible node. Pure (screen coords in, number out) so the
 * visible-first ordering unit-tests without a canvas.
 *
 * @param sx,sy   the node's screen position (px)
 * @param width,height the viewport size (px)
 * @param margin a slack band (px) around the viewport so a node just off the
 *   edge (about to scroll in) still counts as "visible" and preloads — a smooth
 *   fill as the user pans, no hard pop at the edge.
 */
export function viewportPriority(
  sx: number,
  sy: number,
  width: number,
  height: number,
  margin = 0,
): number {
  const cx = width / 2;
  const cy = height / 2;
  const dx = sx - cx;
  const dy = sy - cy;
  const distToCenter = dx * dx + dy * dy;
  const inView =
    sx >= -margin &&
    sx <= width + margin &&
    sy >= -margin &&
    sy <= height + margin;
  return inView ? distToCenter : OFFSCREEN_PRIORITY_BASE + distToCenter;
}

export class ThumbQueue {
  /** hash -> its best (smallest) priority while pending. A hash is "in the
   * queue" iff it is a key here. */
  private readonly pending = new Map<string, number>();

  /** Enqueue `hash` at `priority`, or LOWER an already-queued hash's priority
   * (so a node moving toward the viewport is served sooner). A higher priority
   * for an already-queued hash is ignored — it never demotes a more-urgent
   * request. */
  push(hash: string, priority: number): void {
    const existing = this.pending.get(hash);
    if (existing === undefined || priority < existing) {
      this.pending.set(hash, priority);
    }
  }

  /** Remove and return the lowest-priority (most urgent) hash, or null if empty.
   * Ties break on insertion order (Map iteration order) for determinism. */
  pop(): string | null {
    let bestHash: string | null = null;
    let bestPriority = Infinity;
    for (const [hash, priority] of this.pending) {
      if (priority < bestPriority) {
        bestPriority = priority;
        bestHash = hash;
      }
    }
    if (bestHash !== null) this.pending.delete(bestHash);
    return bestHash;
  }

  /** Whether a hash is currently queued (used by the cache to avoid re-enqueuing
   * and by tests). */
  has(hash: string): boolean {
    return this.pending.has(hash);
  }

  /** Pending count — the number of not-yet-started requests. */
  get size(): number {
    return this.pending.size;
  }

  /** Drop every pending request (a scope/topic change reseeds the node set). */
  clear(): void {
    this.pending.clear();
  }
}
