/**
 * Auto-advance (featureset §4, D7 default OFF): after a committed rating —
 * and after a note submitted from Look — advance to the next image in the
 * navigation set. Pure decision; the root wires the outcome
 * (look.next() / grid.advanceActive()).
 *
 * Multi-select rating NEVER advances or destroys the selection
 * (UI-ARCHITECTURE §4).
 */

export interface AdvanceInput {
  autoAdvance: boolean;
  /** Surface the commit happened on (note inputs keep their summon surface). */
  surface: "grid" | "look";
  commit: "rating" | "note";
  /** Scope target count at commit time (multi = no advance). */
  selectionCount: number;
}

export function afterCommit(input: AdvanceInput): "look-next" | "grid-next" | null {
  if (!input.autoAdvance) return null;
  if (input.commit === "note") {
    // Notes advance only when entered from Look (featureset §4).
    return input.surface === "look" ? "look-next" : null;
  }
  // A collapsed pair is ONE displayed image with two targets — still a
  // single-image rating, still advances (count > 2 is a real multi-select;
  // count 2 from a pair is handled upstream: the slice reports the
  // DISPLAY-unit count here, not the expanded target count).
  if (input.selectionCount > 1) return null;
  return input.surface === "look" ? "look-next" : "grid-next";
}
