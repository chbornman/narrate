/**
 * The two-gesture mic machine (CAPTURE §6.4 — founder ruling, June 2026;
 * rebound June 12 2026: SPACE is 100% the microphone key, "like a Zoom
 * call" — M went back to the reserved pool). Two gestures on one key:
 *
 *  · TAP (released before MIC_HOLD_MS): toggle — arm when disarmed,
 *    disarm when armed;
 *  · PRESS-AND-HOLD (≥ MIC_HOLD_MS): push-to-talk — the mic arms ON THE
 *    PRESS (waiting out the threshold would eat the utterance's onset),
 *    stays armed for the hold, and the release disarms, shipping the
 *    utterance through the normal disarm drain (trailing finals, B52/B72
 *    — short PTT bursts still ship).
 *
 * Both gestures therefore do the same thing at keydown when disarmed
 * (arm now); only the RELEASE distinguishes them. From an already-armed
 * mic the hold gesture is deliberately inert: press does nothing and a
 * past-threshold release does nothing — PTT only applies from disarmed,
 * so an absent-minded hold can never tear down a mic the user armed on
 * purpose. A tap while armed disarms (that's the toggle half).
 *
 * Pure and timer-free like confirmhold.ts: time is a parameter (the sink
 * supplies Date.now), so michold.test.ts needs no fake timers. Engagement
 * arrives through the registry's mic-press row (keydown — §11 input
 * suppression and the asrReady gate apply there, on the def, like every
 * other key); release and window-blur are raw key facts handled in
 * App.svelte — the hold-E precedent.
 *
 * Resolved intents are EXPLICIT "arm"/"disarm", never "toggle": a PTT
 * release racing a device-failure disarm (§6.6) must end DISARMED — a
 * blind toggle could re-arm a mic the user believes is off.
 */

/** Tap/hold boundary. Short enough that a deliberate hold-to-talk never
 * reads as a mis-toggle, long enough that a quick toggle tap (typically
 * well under 200 ms) never trips PTT. */
export const MIC_HOLD_MS = 250;

export interface MicHoldState {
  /** Timestamp of the mic-key (Space) keydown; null = no gesture in flight. */
  readonly pressedAt: number | null;
  /** The press found the mic disarmed and armed it (the PTT half); false
   * = the mic was already armed when the gesture began. */
  readonly armedOnPress: boolean;
}

export const MIC_HOLD_IDLE: MicHoldState = { pressedAt: null, armedOnPress: false };

/** What the sink must drive through IPC (explicit, idempotent — see
 * module note on why never "toggle"). */
export type MicIntent = "arm" | "disarm" | "none";

/** Mic-key keydown (the registry's mic-press action). `armed` is the mic
 * truth at press time (armedIdle/armedSpeaking). */
export function micDown(
  s: MicHoldState,
  e: { armed: boolean; now: number },
): { state: MicHoldState; intent: "arm" | "none" } {
  // Auto-repeat: holding the key fires repeated keydowns, and a keydown while a
  // gesture is already in flight IS a repeat (the keyup would have
  // resolved the machine otherwise). Keep the original timestamp — a
  // restarted clock could keep a long hold forever under the threshold.
  if (s.pressedAt !== null) return { state: s, intent: "none" };
  return {
    state: { pressedAt: e.now, armedOnPress: !e.armed },
    // Disarmed → arm NOW: a tap wants armed anyway, and PTT must not lose
    // the utterance onset. Armed → nothing yet; the release decides.
    intent: e.armed ? "none" : "arm",
  };
}

/** Mic-key keyup: resolve tap vs hold. Always returns to idle — a hold must
 * never wedge — and a stray keyup (press suppressed while typing, or
 * consumed elsewhere) resolves to nothing. */
export function micUp(
  s: MicHoldState,
  now: number,
): { state: MicHoldState; intent: "disarm" | "none" } {
  if (s.pressedAt === null) return { state: s, intent: "none" };
  const held = now - s.pressedAt >= MIC_HOLD_MS;
  // From disarmed (we armed at press): a tap keeps the arm (toggle ON
  // landed at keydown); a hold is PTT — release ships by disarming.
  // From armed: a tap toggles OFF; a hold is inert (module note).
  const disarm = s.armedOnPress ? held : !held;
  return { state: MIC_HOLD_IDLE, intent: disarm ? "disarm" : "none" };
}

/** Window blur mid-gesture: the keyup will never arrive. A hot mic THIS
 * gesture opened must not wedge open — resolve as a PTT release (the
 * utterance so far still ships). A mic armed BEFORE the press stays
 * armed: that state predates the gesture, and blur must not kill a
 * deliberate toggle. */
export function micBlur(s: MicHoldState): {
  state: MicHoldState;
  intent: "disarm" | "none";
} {
  if (s.pressedAt === null) return { state: s, intent: "none" };
  return { state: MIC_HOLD_IDLE, intent: s.armedOnPress ? "disarm" : "none" };
}
