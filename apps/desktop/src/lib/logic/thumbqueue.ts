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
