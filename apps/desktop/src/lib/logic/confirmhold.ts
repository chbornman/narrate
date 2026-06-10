/**
 * Hold-to-confirm state machine (UI §8.4 step two) — pure, no timers.
 * The redaction modal's "Redact permanently" button drives it: press
 * starts the hold, periodic ticks advance it, releasing before the
 * threshold resets to idle, and reaching the threshold confirms exactly
 * once. The §8.4 alternative (type-to-confirm) is deliberately not built —
 * one confirm affordance, one modal (R5).
 *
 * Time is a parameter everywhere (the component supplies Date.now), so
 * confirmhold.test.ts needs no fake timers.
 */

/** Hold duration. Long enough to be deliberate, short enough to not read
 * as broken — destructive ambiguity must be impossible (featureset §8). */
export const HOLD_TO_CONFIRM_MS = 1500;

export interface HoldState {
  /** Timestamp of the press; null = not holding. */
  readonly heldSince: number | null;
  /** Latched once: a confirmed hold stays confirmed (the action fired). */
  readonly confirmed: boolean;
}

export const HOLD_IDLE: HoldState = { heldSince: null, confirmed: false };

/** Pointer/key down. Ignored while already holding or after confirmation
 * (the modal is closing; a re-press must not re-fire). */
export function press(state: HoldState, now: number): HoldState {
  if (state.confirmed || state.heldSince !== null) return state;
  return { heldSince: now, confirmed: false };
}

/** Periodic tick while holding: confirms when the threshold is reached. */
export function tick(state: HoldState, now: number): HoldState {
  if (state.confirmed || state.heldSince === null) return state;
  if (now - state.heldSince >= HOLD_TO_CONFIRM_MS)
    return { heldSince: state.heldSince, confirmed: true };
  return state;
}

/** Pointer/key up. Past the threshold this confirms (defensive against a
 * missed final tick); before it, the hold resets to idle — no partial
 * credit across attempts. */
export function release(state: HoldState, now: number): HoldState {
  if (state.confirmed) return state;
  if (state.heldSince !== null && now - state.heldSince >= HOLD_TO_CONFIRM_MS)
    return { heldSince: state.heldSince, confirmed: true };
  return HOLD_IDLE;
}

/** Progress in [0, 1] for the fill rendering. */
export function fraction(state: HoldState, now: number): number {
  if (state.confirmed) return 1;
  if (state.heldSince === null) return 0;
  return Math.max(0, Math.min(1, (now - state.heldSince) / HOLD_TO_CONFIRM_MS));
}
