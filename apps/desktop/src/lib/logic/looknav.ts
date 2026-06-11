/**
 * Look navigation logic (featureset §2/§5): the navigation set IS the
 * entry selection; R member flips over LookEntry pairs; and the Space
 * tap-vs-hold resolution (UI-ARCHITECTURE §6) — the packet's most fragile
 * interaction (Appendix B): tap closes at fit (the registry's look-close
 * row, enabled: lookAtFit), hold pans while zoomed, and a CLEAN tap
 * (down/up, no pan) while zoomed also closes. The machine here is pure;
 * LookStage feeds it key/pan events and performs the outcome.
 */
import type { DisplayUnit, LookEntry } from "../types/display";

/** Grid cell → Look entry (the frozen cross-stage seam, types/display.ts). */
export function toEntry(unit: DisplayUnit): LookEntry {
  return { display: unit.primary.hash, alt: unit.alt?.hash ?? null };
}

/**
 * Navigation set = entry selection (featureset §2): entering Look with a
 * multi-selection (≥2 units, entered unit included) cycles within it;
 * single-image entry — or an entry outside the selection, which narrows
 * scope to the viewed image anyway (CAPTURE §3) — cycles the folder.
 * Order is GRID order: ←/→ walk the wall the way it reads; selection
 * ORDER stays a write-scope concern (CAPTURE event_targets.position),
 * never a navigation one. Returns null when the entry hash is unknown.
 */
export function navigationSet(
  units: readonly DisplayUnit[],
  selected: readonly string[],
  entryHash: string,
): { order: LookEntry[]; index: number } | null {
  const sel = new Set(selected);
  const scoped =
    sel.size >= 2 && sel.has(entryHash)
      ? units.filter((u) => sel.has(u.primary.hash))
      : units;
  const index = scoped.findIndex((u) => u.primary.hash === entryHash);
  if (index < 0) return null;
  return { order: scoped.map(toEntry), index };
}

/** Hash an entry displays, honoring an R flip (featureset §5). */
export function displayedHash(entry: LookEntry, flips: ReadonlySet<string>): string {
  return flips.has(entry.display) && entry.alt !== null ? entry.alt : entry.display;
}

/** R: toggle the flip for a pair entry; lone entries no-op (FRV keeps the
 * key inert rather than erroring — quiet). */
export function toggleFlip(
  flips: ReadonlySet<string>,
  entry: LookEntry,
): ReadonlySet<string> {
  if (entry.alt === null) return flips;
  const next = new Set(flips);
  if (next.has(entry.display)) next.delete(entry.display);
  else next.add(entry.display);
  return next;
}

// ---------------------------------------------------------------------------
// Space tap-vs-hold (UI-ARCHITECTURE §6)
//
// At fit, Space never reaches this machine: the registry's look-close row
// (enabled: ctx.lookAtFit) consumes the keydown. While zoomed that row is
// disabled, so the raw key events flow here: a fresh keydown ENGAGES the
// hold (pan mode); any pan during the hold marks it; keyup resolves to
// "close" only for a clean tap. Pointer movement alone never marks the
// hold — panning requires a button-drag, so key-release jitter cannot
// cancel a close. Must survive M2a: the pencil claims the plain drag,
// Space-pan stays on this exact pipeline.
// ---------------------------------------------------------------------------

export interface SpaceHoldState {
  held: boolean;
  /** A pan happened during this hold (keyup must not close). */
  panned: boolean;
}

export const SPACE_IDLE: SpaceHoldState = { held: false, panned: false };

/**
 * Space keydown. Engages only on a FRESH press while zoomed:
 *  · auto-repeat during a hold keeps the hold (and its panned flag);
 *  · a repeat arriving on idle (the hold began before Look had the key —
 *    e.g. mid-hold surface change) never engages, so its keyup can never
 *    close;
 *  · at fit the registry row owns the tap — the machine stays idle.
 */
export function spaceDown(
  s: SpaceHoldState,
  e: { atFit: boolean; repeat: boolean },
): SpaceHoldState {
  if (s.held) return s;
  if (e.repeat || e.atFit) return s;
  return { held: true, panned: false };
}

/** A pan was applied while the hold was engaged: the tap is no longer clean. */
export function spacePanned(s: SpaceHoldState): SpaceHoldState {
  return s.held && !s.panned ? { held: true, panned: true } : s;
}

/** Space keyup: a clean engaged tap closes; a panned hold (or a stray
 * keyup on idle) resolves to nothing. The machine always returns to idle.
 *
 * With the pencil ON, Space is the PAN key at any zoom (UI §11: "Space
 * (hold) + drag · Look (pencil on) · Pan") — it must never close Look,
 * mid-mark least of all (during pen-down the overlay holds pointer
 * capture, so the stage never sees a pan and every tap looks clean). The
 * registry's look-close row carries the same `!ctx.pencilMode` gate at
 * fit; this is the zoomed (raw-pipeline) half of that ruling. */
export function spaceUp(
  s: SpaceHoldState,
  e: { pencilOn: boolean },
): {
  state: SpaceHoldState;
  outcome: "close" | "none";
} {
  if (!s.held) return { state: s, outcome: "none" };
  return {
    state: SPACE_IDLE,
    outcome: s.panned || e.pencilOn ? "none" : "close",
  };
}

/**
 * The shared raw Space-held fact (P5.1 review fix): ONE tracker, in
 * LookStage, writing the look slice (the eraserHeld precedent);
 * PencilOverlay only CONSUMES the slice field to yield the pointer for
 * Space-pan. Engagement is ownership-gated exactly like the pan machine
 * (LookStage's stageOwnsRawKeys: a Space pressed under the cheatsheet, a
 * context menu, search, or the rail must not make the overlay yield) —
 * so the two pipelines cannot desync. Release is UNCONDITIONAL: the
 * keyup (or window blur) may arrive after a menu opened mid-hold, and a
 * hold must never wedge.
 */
export function spaceHeldNext(
  held: boolean,
  evt: "down" | "up" | "blur",
  stageOwnsKeys: boolean,
): boolean {
  if (evt !== "down") return false;
  return stageOwnsKeys ? true : held;
}
