/**
 * Hover-intent timing for cascading submenus (paired with Menu.svelte). The
 * SIMPLE delay approach, NOT a geometric safe-triangle: opening a submenu
 * waits HOVER_OPEN_MS (so a passing pointer does not flicker it open);
 * closing the open child is deferred HOVER_CLOSE_MS (so the diagonal travel
 * from parent row to child panel — across the GUTTER overlap — does not slam
 * it shut). Entering the child panel cancels the pending close.
 *
 * WHY a pure helper: the timer lifecycle (start/cancel, all cleared on
 * destroy) is the same discipline as Menu.svelte's copy-close timer, and is
 * testable with vi.useFakeTimers without a DOM.
 */

export const HOVER_OPEN_MS = 110;
export const HOVER_CLOSE_MS = 280;

/** A single cancelable timer: starting again cancels any prior pending fire,
 * so one HoverTimer models "the one open-intent" or "the one close-intent". */
export class HoverTimer {
  private handle: ReturnType<typeof setTimeout> | undefined;

  /** Arm the timer; replaces any pending fire. */
  start(fn: () => void, ms: number): void {
    this.cancel();
    this.handle = setTimeout(() => {
      this.handle = undefined;
      fn();
    }, ms);
  }

  /** Cancel a pending fire (no-op if none). */
  cancel(): void {
    if (this.handle !== undefined) {
      clearTimeout(this.handle);
      this.handle = undefined;
    }
  }

  /** Whether a fire is currently pending. */
  get pending(): boolean {
    return this.handle !== undefined;
  }
}
