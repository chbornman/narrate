/**
 * Near-duplicate DISPLAY logic (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1") — the
 * pure, testable half of the Duplicates lens. The backend
 * (`find_near_duplicates`) returns transitive near-dup GROUPS as bare member
 * hashes; this module turns those into the shape the view renders:
 *
 *   · a stable group ORDER (biggest clusters first — the most redundancy to
 *     review sits at the top of the list),
 *   · a per-group REPRESENTATIVE (the one to keep) + the REDUNDANT rest (the
 *     non-destructive "select the others" affordance multi-selects these),
 *   · the summary COUNT copy ("N near-duplicate groups, M near-duplicates").
 *
 * It is display-only: NOTHING here deletes, archives, or writes a sidecar. The
 * keep/cull decision is sidecar truth, deliberately deferred to founder design.
 *
 * WHY a separate module: the grouping/representative-pick/copy are the only
 * pieces with branchy logic worth a unit test, and keeping them pure (no Svelte
 * runes, no IPC) lets the gate's vitest exercise them directly.
 */
import type { DuplicateGroupDto } from "../types/dto";

/** A group prepared for display: its members split into the representative to
 * keep and the redundant rest, plus the count carried through for the badge. */
export interface DupeCluster {
  /** The member to KEEP (highlighted in the view). */
  representative: string;
  /** The other members — the redundant copies the "select redundant" affordance
   * multi-selects. Never includes the representative; may be empty only in the
   * degenerate single-member case (which the backend never emits). */
  redundant: string[];
  /** Every member, representative first then the redundant rest — render order. */
  members: string[];
  /** Member count (== group.count); kept for the per-group "x{count}" badge so
   * the view never recomputes a `.length`. */
  count: number;
}

/** Optional per-hash signal used to PICK the representative. The design doc's
 * representative is "highest-rated / sharpest / medoid"; the backend ships bare
 * hashes, so the view supplies what it knows (the grid item's folded rating).
 * Absent for every hash ⇒ the representative is just the first member (the
 * backend already sorts members deterministically, so the pick is stable). */
export type RatingLookup = (hash: string) => number | null;

/**
 * Pick the representative member of a group: the highest available rating wins,
 * ties (and the no-ratings case) broken by the group's existing member order
 * (stable — the backend sorts members, so the choice is deterministic across
 * renders). Returns the member's INDEX so the caller can split cheaply.
 *
 * WHY rating-first: the founder reviews to KEEP the best frame; surfacing the
 * highest-rated copy as the one-to-keep matches the cull intent without us
 * guessing at sharpness we do not have on the wire yet.
 */
export function representativeIndex(
  members: readonly string[],
  rating?: RatingLookup,
): number {
  if (members.length === 0) return -1;
  if (rating === undefined) return 0;
  let bestIdx = 0;
  // -Infinity so any real rating (including 0) beats "no rating known"; an
  // unrated member stays the representative ONLY if nothing in the group is
  // rated, preserving the stable first-member fallback.
  let bestRating = Number.NEGATIVE_INFINITY;
  for (let i = 0; i < members.length; i++) {
    const r = rating(members[i]);
    const score = r === null ? Number.NEGATIVE_INFINITY : r;
    // Strictly greater keeps the FIRST of equal-rated members (stable tie-break).
    if (score > bestRating) {
      bestRating = score;
      bestIdx = i;
    }
  }
  return bestIdx;
}

/**
 * Turn the backend's raw groups into display clusters: drop any malformed
 * sub-pair group defensively, pick each representative, and ORDER the clusters
 * biggest-first (ties by representative hash for a stable render). Biggest-first
 * because the largest clusters carry the most redundancy to review — they belong
 * at the top of the list, mirroring the design doc's "142 near-dups in 38
 * groups" framing.
 */
export function toClusters(
  groups: readonly DuplicateGroupDto[],
  rating?: RatingLookup,
): DupeCluster[] {
  const clusters: DupeCluster[] = [];
  for (const g of groups) {
    // A near-dup group is >= 2 by definition; a 0/1-member group is a backend
    // contract violation, so skip it rather than render a lone "duplicate".
    if (g.imageHashes.length < 2) continue;
    const idx = representativeIndex(g.imageHashes, rating);
    const representative = g.imageHashes[idx];
    const redundant = g.imageHashes.filter((_, i) => i !== idx);
    clusters.push({
      representative,
      redundant,
      members: [representative, ...redundant],
      count: g.imageHashes.length,
    });
  }
  // Biggest cluster first; ties broken by representative hash (deterministic).
  clusters.sort(
    (a, b) =>
      b.count - a.count || a.representative.localeCompare(b.representative),
  );
  return clusters;
}

/** Every redundant (non-representative) hash across all clusters, de-duplicated
 * in render order — the exact set the "select redundant copies" affordance hands
 * to the grid's existing multi-selection. (A hash can only be redundant in one
 * group, but the de-dupe keeps the contract obvious and defensive.) */
export function redundantHashes(clusters: readonly DupeCluster[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const c of clusters) {
    for (const h of c.redundant) {
      if (!seen.has(h)) {
        seen.add(h);
        out.push(h);
      }
    }
  }
  return out;
}

/**
 * The summary line for the lens header: how many groups and how many redundant
 * copies they hide. NO em-dashes (gate: check:emdash). Empty groups ⇒ the calm
 * none-state copy. Singular/plural handled so it never reads "1 groups".
 *
 *   []                    -> "No near-duplicates found"
 *   1 group of 2          -> "1 near-duplicate group, 1 redundant copy"
 *   38 groups, 104 redund -> "38 near-duplicate groups, 104 redundant copies"
 */
export function summaryCopy(clusters: readonly DupeCluster[]): string {
  if (clusters.length === 0) return "No near-duplicates found";
  const groups = clusters.length;
  // Redundant copies = every member minus one kept representative per group.
  const redundant = clusters.reduce((n, c) => n + c.redundant.length, 0);
  const groupWord = groups === 1 ? "group" : "groups";
  const copyWord = redundant === 1 ? "copy" : "copies";
  return `${groups} near-duplicate ${groupWord}, ${redundant} redundant ${copyWord}`;
}

/**
 * A tiny trailing-edge debounce for the looseness slider: while the founder
 * drags the Hamming threshold, only the LAST value after `delayMs` of quiet
 * fires the (expensive, O(n^2)) re-scan. Pure + framework-free so the gate can
 * test it with fake timers; the component owns the timer handle's lifetime.
 *
 * Returns a `{ run, cancel }` pair: `run(arg)` (re)arms the timer with the
 * latest arg; `cancel()` drops a pending fire (lens close / unmount).
 */
export function debounce<A>(
  fn: (arg: A) => void,
  delayMs: number,
): { run: (arg: A) => void; cancel: () => void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    run(arg: A) {
      if (timer !== undefined) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = undefined;
        fn(arg);
      }, delayMs);
    },
    cancel() {
      if (timer !== undefined) {
        clearTimeout(timer);
        timer = undefined;
      }
    },
  };
}
