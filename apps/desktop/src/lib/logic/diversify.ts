/**
 * The Diversify / duplication-tolerance view filter, pure
 * (DESIGN-DEDUP-AND-SIMILARITY.md, the duplication-tolerance slider). The
 * backend's `diversify_scope` reports which in-scope images are redundant
 * (`hidden`); the grid drops those and folds them behind a "N hidden"
 * affordance.
 *
 * WHY a module: the fold filter is the load-bearing bit of the feature (it
 * must AGREE with the backend's report and survive a slow IPC racing a scope
 * change), and the 0..1 tolerance <-> 0..100% slider mapping is the kind of
 * off-by-one math that wants one tested home rather than inline in the chrome.
 * Keeping it pure lets the seam be unit-tested without any Svelte or IPC.
 */

/** Filter a grid item-list by DROPPING the report's `hidden` (redundant) set,
 * preserving the input order (the grid's own sort runs downstream over the
 * result). A `null` hidden-set means "the filter is OFF" -> pass everything
 * through unchanged, so the caller can hold one nullable field for the whole
 * on/off + filter state.
 *
 * WHY the filter keys on `hidden` rather than the report's `shown` twin: a
 * hash the pass never saw PASSES THROUGH. Mid-ingest re-lists introduce
 * new photos between passes, and keeping-what's-in-`shown` silently hid every
 * one of them until the next pass landed (AUDIT-2026-07-07 U1). Dropping
 * what the pass explicitly FOLDED makes "unknown -> visible" the structural
 * default, which is the honest failure mode for a view filter.
 *
 * Generic over `{ hash }` so it filters GridItems without importing the DTO (the
 * grid slice owns that type); the only field it touches is `hash`. */
export function filterDiversify<T extends { hash: string }>(
  items: readonly T[],
  hidden: ReadonlySet<string> | null,
): T[] {
  // OFF (null) is the identity: returning the same items unfiltered is what
  // "turning Diversify off restores the full set" reduces to, with no special
  // case at the call site.
  if (hidden === null) return items as T[];
  return items.filter((i) => !hidden.has(i.hash));
}

/** The slider works in whole PERCENT (0..100, the dupeGuru/digiKam idiom — see
 * the design's "UX template"); the backend wants a tolerance in 0..1. One place
 * for the mapping so the readout label and the IPC payload can never disagree by
 * a factor of 100. */
export function percentToTolerance(percent: number): number {
  // Defensive clamp: a stray slider value outside 0..100 maps to the nearest
  // valid tolerance rather than sending the backend an out-of-range f32.
  const clamped = Math.max(0, Math.min(100, percent));
  return clamped / 100;
}

/** The inverse, for seeding the slider from a stored/restored tolerance. Rounded
 * to a whole percent (the slider's step) so a round-trip is stable. */
export function toleranceToPercent(tolerance: number): number {
  const clamped = Math.max(0, Math.min(1, tolerance));
  return Math.round(clamped * 100);
}

/** The default tolerance the slider opens at when Diversify is first turned on.
 * A MIDDLE strength (50%): high enough that the founder immediately SEES the
 * feature collapse near-dup bursts (the whole point), low enough that it does
 * not aggressively hide distinct-but-similar frames before they can react. The
 * slider is the direct manipulation from here. */
export const DEFAULT_TOLERANCE_PERCENT = 50;
