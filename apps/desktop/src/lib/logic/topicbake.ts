/**
 * The topic -> collection bake, pure (DESIGN-TOPICS-COLLECTIONS.md). A topic is
 * fuzzy/computed: a phrase with a per-image affinity SCORE. A collection is the
 * committed snapshot. The bake gesture is "drag a threshold -> the images
 * scoring at or above it become a selection -> commit that selection into an
 * evented collection". This module is the ONE place the threshold->selected-set
 * math + the ranked->order mapping live, so the graph slider and the Topics tab
 * share identical semantics (and the seam is unit-tested without any UI).
 *
 * WHY a module: the threshold geometry is the load-bearing bit of the signature
 * gesture (the "nearby set lighting up"), and the graph glow + the tab count +
 * the bake-command members must agree exactly. One pure function guarantees they
 * do.
 */
import type { RankedImageDto } from "../types/dto";

/** One image's blended affinity to a selected topic phrase. The Topics-tab grid
 * (RankedImageDto) and the graph (per-node blended affinity) both reduce to this
 * shape, so the threshold math is shared across both surfaces. */
export interface ScoredImage {
  hash: string;
  score: number;
}

/** The hashes whose affinity is AT OR ABOVE the threshold, preserving the input
 * order (the ranked grid feeds descending-affinity, so the selection keeps that
 * order). Inclusive at the boundary so a slider parked exactly on a score still
 * grabs that image. The single source of truth for: the graph's glow set, the
 * live "N photos" count, and the bake's membership preview. */
export function selectedAboveThreshold(
  scored: readonly ScoredImage[],
  threshold: number,
): string[] {
  return scored.filter((s) => s.score >= threshold).map((s) => s.hash);
}

/** The live count the slider readout shows ("142 photos"): how many images are
 * at or above the threshold. Cheap twin of selectedAboveThreshold for the count
 * label without materializing the hash array. */
export function countAboveThreshold(
  scored: readonly ScoredImage[],
  threshold: number,
): number {
  let n = 0;
  for (const s of scored) if (s.score >= threshold) n++;
  return n;
}

/** Map the backend's ranked-images reply to ScoredImage, dropping nothing: the
 * backend already returns descending `score`, so this preserves the ranked
 * (highest-affinity-first) order the Topics tab grid wants. */
export function rankedToScored(ranked: readonly RankedImageDto[]): ScoredImage[] {
  return ranked.map((r) => ({ hash: r.hash, score: r.score }));
}

/** The ranked hashes in order (highest affinity first) — the order the Topics
 * tab grid feeds into list_images so the cells render most-on-topic first. */
export function rankedHashes(ranked: readonly RankedImageDto[]): string[] {
  return ranked.map((r) => r.hash);
}

/** A sane DEFAULT threshold for a fresh selection: high enough that the bake
 * starts from the strongly-on-topic core, not the whole fuzzy tail. 0.5 on the
 * 0..1 slider (affinities are cosines; the strong matches cluster well above
 * the midpoint). The user drags from here. */
export const DEFAULT_BAKE_THRESHOLD = 0.5;
