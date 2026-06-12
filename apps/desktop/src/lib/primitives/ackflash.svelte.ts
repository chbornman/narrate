/**
 * Fire-and-forget verbs acknowledge completion (founder dogfood, June
 * 2026: "Restart runtime ... I don't see anything. It doesn't even feel
 * like I clicked the button"). An AckFlash fronts ONE button's async verb:
 * `busy` while the await is in flight (the button disables), then `done`
 * for a brief window after it resolves (the button swaps to its past-tense
 * label), then back to rest. AckButton.svelte is the standard seat; this
 * class stays separate so the timing truth is headlessly testable.
 *
 * WHY not a toast: toasts are spec-capped at exactly three triggers
 * (UI §7.5 / R5), so — like the copy check (copyflash.svelte.ts) — the
 * acknowledgment lives AT the affordance. Per-button instances, unlike
 * copyFlash's one register: two verbs can land independently and each
 * ack vouches only for its own verb.
 *
 * The flash is truthful (the copyflash rule): `done` is set only after the
 * verb's promise RESOLVED — a rejected verb shows nothing rather than lying.
 */

export const ACK_FLASH_MS = 1500;

export class AckFlash {
  /** True while the verb's promise is in flight (disable the button). */
  busy = $state(false);
  /** True for the brief post-resolve window (show the done label). */
  done = $state(false);
  #timer: ReturnType<typeof setTimeout> | undefined;

  async run(verb: () => Promise<void>): Promise<void> {
    // Re-entrancy guard: `disabled` already blocks clicks mid-flight, but
    // Enter on a still-focused button can race the DOM update — a second
    // run would double-fire the verb.
    if (this.busy) return;
    this.busy = true;
    this.done = false;
    try {
      await verb();
      this.done = true;
      // Restart (not stack) the window: a re-click during the flash gives
      // the new completion its full window, and one timer means the label
      // can never flicker rest→done from a stale clear.
      clearTimeout(this.#timer);
      this.#timer = setTimeout(() => (this.done = false), ACK_FLASH_MS);
    } finally {
      this.busy = false;
    }
  }
}
