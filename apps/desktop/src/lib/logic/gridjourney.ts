/**
 * Low-cardinality viewport performance bookkeeping.
 *
 * Hashes exist only in this short-lived frontend set. Emitted events contain
 * durations and counts only, so no photo identity reaches performance logs.
 */
export interface GridJourneyEvent {
  phase: "first-paint" | "settle";
  durationMs: number;
  ok: boolean;
  itemCount: number;
}

export class GridJourneyTracker {
  private generation = 0;
  private startedAt = 0;
  private expected = 0;
  private pending = new Set<string>();
  private painted = false;

  begin(hashes: readonly string[], startedAt: number): number {
    this.generation += 1;
    this.startedAt = startedAt;
    this.pending = new Set(hashes);
    this.expected = this.pending.size;
    this.painted = false;
    return this.generation;
  }

  loaded(hash: string, generation: number, now: number): GridJourneyEvent[] {
    if (
      generation !== this.generation ||
      this.expected === 0 ||
      !this.pending.delete(hash)
    ) {
      return [];
    }
    const durationMs = Math.max(0, now - this.startedAt);
    const events: GridJourneyEvent[] = [];
    if (!this.painted) {
      this.painted = true;
      events.push({
        phase: "first-paint",
        durationMs,
        ok: true,
        itemCount: this.expected,
      });
    }
    if (this.pending.size === 0) {
      events.push({
        phase: "settle",
        durationMs,
        ok: true,
        itemCount: this.expected,
      });
      this.expected = 0;
    }
    return events;
  }

  timeout(generation: number, now: number): GridJourneyEvent[] {
    if (
      generation !== this.generation ||
      this.expected === 0 ||
      this.pending.size === 0
    ) {
      return [];
    }
    const event: GridJourneyEvent = {
      phase: "settle",
      durationMs: Math.max(0, now - this.startedAt),
      ok: false,
      itemCount: this.expected,
    };
    this.expected = 0;
    this.pending.clear();
    return [event];
  }

  cancel(): void {
    this.generation += 1;
    this.expected = 0;
    this.pending.clear();
  }
}
