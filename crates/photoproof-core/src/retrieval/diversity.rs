//! Representative-subset selection over the CLIP cosine k-NN graph — the
//! ENGINE behind the "duplication-tolerance / Diversify" view filter
//! (`docs/DESIGN-DEDUP-AND-SIMILARITY.md`, the duplication-tolerance slider).
//!
//! WHY this exists: a photo session produces bursts of near-identical frames.
//! Raising the tolerance slider should HIDE the redundant ones so the grid
//! shows fewer, MORE VARIED images — each tight similarity cluster collapses to
//! a single REPRESENTATIVE; the rest are hidden (non-destructively, a view
//! filter, never a delete). At tolerance 0 everything is shown.
//!
//! These are PURE functions over a similarity graph. They take no embedder, no
//! DB, no vector store — only the sparse k-NN graph the visualizer already
//! precomputes (`PpvecStore::knn_within`, shape `KnnGraph`) plus a per-image
//! quality score. That keeps the selection math fully unit-testable from small
//! synthetic fixtures (no real CLIP vectors needed) and lets the Tauri command
//! feed it the live graph.
//!
//! ## What we consume
//!
//! `KnnGraph = Vec<(image_hash, Vec<(neighbor_hash, cosine_sim)>)>` — for each
//! image, its top-k most-similar OTHER in-scope images, cosine in roughly
//! `[0, 1]` (negatives already clamped to 0 by `knn_within`). We treat this as
//! a SPARSE similarity matrix: an unlisted pair has similarity 0 (no edge). The
//! facility-location objective is therefore approximated on the k-NN edges
//! rather than a full O(n²) matrix — exactly the shape the design doc calls for
//! ("its naive O(n²) is approximated on a nearest-neighbor graph — directly
//! consumable from `knn_within`").
//!
//! ## The two objectives
//!
//! - [`facility_location_select`] — greedy facility-location, `f(X) = Σ_i
//!   max_{j∈X} s_ij`. "Cover every image with a representative." This is the
//!   RECOMMENDED CORE (arXiv 1805.11191) and the one the slider drives by
//!   default; submodular ⇒ the lazy-greedy pick is a `1 − 1/e` approximation.
//! - [`mmr_select`] — Maximal Marginal Relevance / max-sum diversification with
//!   a single `λ` quality-vs-diversity knob (arXiv 1203.6397). Offered as an
//!   alternate one-slider mapping.
//!
//! Both return the SAME [`Selection`] shape (which hashes are shown vs hidden),
//! so the command can switch objectives without changing its wire contract.
//!
//! ## Determinism
//!
//! Every tie is broken by hash, every input is sorted before iteration, and the
//! greedy loops never consult wall-clock or hash-map order. The same graph +
//! tolerance always yields the same selection (reproducible UI + exact tests),
//! mirroring `knn_within`'s own determinism contract.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::retrieval::KnnGraph;

/// The result of a representative-selection pass: which in-scope images are
/// REPRESENTATIVES (shown in the grid) and which are HIDDEN (collapsed into a
/// representative). `shown ∪ hidden` is exactly the scope's images; the two
/// sets are disjoint. Both are sorted by hash for a stable, testable order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The representative images — what the Diversify view SHOWS. Sorted by
    /// hash.
    pub shown: Vec<String>,
    /// The redundant images folded into a representative — what the view HIDES.
    /// Sorted by hash. Empty at tolerance 0 (nothing collapses).
    pub hidden: Vec<String>,
}

impl Selection {
    /// Everything shown, nothing hidden — the tolerance-0 / no-signal result.
    fn all_shown(mut images: Vec<String>) -> Self {
        images.sort();
        images.dedup();
        Selection {
            shown: images,
            hidden: Vec::new(),
        }
    }
}

/// A symmetric sparse similarity view over the scope, built ONCE from the k-NN
/// graph and reused by every objective. WHY symmetrize: `knn_within` is a
/// directed top-k (A may list B without B listing A, since each keeps only its
/// own k nearest). For "are these two redundant" we want an undirected relation,
/// so we keep the MAX of the two directed similarities for a pair — the more
/// generous, more stable signal (if either frame considers the other a near
/// neighbor, they are alike).
///
/// Stored as a `BTreeMap` keyed by hash so iteration order is deterministic and
/// independent of the input vector's order.
struct SimGraph {
    /// Every in-scope image hash (including isolated ones with no edges), sorted.
    nodes: Vec<String>,
    /// `node -> [(neighbor, sim)]`, symmetric, sim descending then hash. Only
    /// non-zero edges are stored (the sparse matrix); an absent pair is sim 0.
    adj: BTreeMap<String, Vec<(String, f32)>>,
}

impl SimGraph {
    /// Build the symmetric sparse graph from the directed k-NN graph. `extra`
    /// lists scope images that may have NO edges (un-embedded, or simply no
    /// near neighbor) so they still count as nodes — an image with no edges is
    /// its own singleton cluster and is always SHOWN, which is the correct
    /// "all-distinct images stay visible" behavior.
    fn from_knn(graph: &KnnGraph, extra: &[String]) -> Self {
        // Accumulate the max directed sim per undirected pair.
        let mut pairs: BTreeMap<(String, String), f32> = BTreeMap::new();
        let mut nodes: BTreeSet<String> = BTreeSet::new();
        for hash in extra {
            nodes.insert(hash.clone());
        }
        for (src, neighbors) in graph {
            nodes.insert(src.clone());
            for (dst, sim) in neighbors {
                nodes.insert(dst.clone());
                if src == dst {
                    continue; // never a self-edge (knn_within already excludes it)
                }
                // Order the pair so (A,B) and (B,A) collapse to one key, then
                // keep the stronger of the two directed similarities.
                let key = if src <= dst {
                    (src.clone(), dst.clone())
                } else {
                    (dst.clone(), src.clone())
                };
                let e = pairs.entry(key).or_insert(0.0);
                if *sim > *e {
                    *e = *sim;
                }
            }
        }

        let mut adj: BTreeMap<String, Vec<(String, f32)>> = BTreeMap::new();
        for node in &nodes {
            adj.entry(node.clone()).or_default();
        }
        for ((a, b), sim) in pairs {
            adj.get_mut(&a)
                .expect("node present")
                .push((b.clone(), sim));
            adj.get_mut(&b)
                .expect("node present")
                .push((a.clone(), sim));
        }
        // Deterministic neighbor order inside each list: sim desc, hash asc.
        for list in adj.values_mut() {
            list.sort_by(|x, y| {
                y.1.partial_cmp(&x.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| x.0.cmp(&y.0))
            });
        }

        SimGraph {
            nodes: nodes.into_iter().collect(),
            adj,
        }
    }

    /// Similarity of an undirected pair, 0 if there is no stored edge (the
    /// sparse-matrix convention).
    fn sim(&self, a: &str, b: &str) -> f32 {
        if a == b {
            return 1.0; // an image is identical to itself
        }
        self.adj
            .get(a)
            .and_then(|list| list.iter().find(|(n, _)| n == b))
            .map(|(_, s)| *s)
            .unwrap_or(0.0)
    }
}

/// The per-image QUALITY score the representative pick prefers: when several
/// frames are mutually redundant, the one with the highest quality becomes the
/// representative (the design's "representative = highest-rated / sharpest /
/// medoid"). The command supplies this from ratings / sharpness; the pure math
/// only needs the map. An image absent from the map scores 0 (neutral) — so an
/// un-scored library still diversifies, it just falls back to the deterministic
/// hash tie-break for which frame represents a cluster.
pub type QualityScores = BTreeMap<String, f32>;

/// Quality of `hash`, defaulting to 0.0 when unscored.
fn quality(scores: &QualityScores, hash: &str) -> f32 {
    scores.get(hash).copied().unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Tolerance → parameter mapping (the slider's behavior — DOCUMENTED here).
// ---------------------------------------------------------------------------

/// Map the user-facing `tolerance ∈ [0, 1]` to a cosine SIMILARITY CUTOFF `τ`:
/// two images are treated as REDUNDANT (one can represent the other) when their
/// cosine similarity ≥ τ. This is the knob facility-location and the cutoff in
/// MMR both read.
///
/// THE CURVE (and WHY): we want an intuitive, MONOTONIC slider —
///   - tolerance 0   → τ = `cutoff_high` (default 1.0): only an image identical
///     to its neighbor is redundant, so in practice NOTHING collapses and the
///     full scope shows. (Floats rarely hit exactly 1.0, and `knn_within`'s
///     dot-product cosine of distinct frames is < 1, so this is "show all".)
///   - tolerance 1   → τ = `cutoff_low` (default 0.55): even loosely-similar
///     frames (same scene / subject) count as redundant, collapsing hard.
///   - in between     → LINEAR interpolation: `τ = high − tolerance·(high−low)`.
///
/// WHY linear (and not, say, a steep curve): the load-bearing property is
/// MONOTONICITY (more tolerance ⇒ a lower cutoff ⇒ never fewer collapses ⇒ never
/// more images shown). Linear is the simplest curve with that property and the
/// most predictable feel; the founder can re-shape it later by moving the two
/// endpoints in `tuning.toml` (`cutoff_high` / `cutoff_low`) without touching
/// code. We deliberately do NOT start the high end below 1.0: a slider that
/// hides images at its zero position would violate "tolerance 0 shows
/// everything".
///
/// `cutoff_low < cutoff_high` is the caller's contract; we clamp defensively so
/// a mis-ordered pair can never invert the slider.
pub fn tolerance_to_cutoff(tolerance: f32, cutoff_high: f32, cutoff_low: f32) -> f32 {
    let t = tolerance.clamp(0.0, 1.0);
    let high = cutoff_high.clamp(0.0, 1.0);
    let low = cutoff_low.clamp(0.0, 1.0);
    // Guard against an inverted pair: a higher tolerance must never RAISE the
    // cutoff (that would SHOW more as you slide up — the opposite of intent).
    let (high, low) = if low <= high {
        (high, low)
    } else {
        (low, high)
    };
    high - t * (high - low)
}

/// Map `tolerance ∈ [0, 1]` to the MMR trade-off `λ ∈ [0, 1]`, where `λ`
/// weights QUALITY/relevance and `(1 − λ)` weights DIVERSITY (the standard MMR
/// convention, arXiv 1203.6397). Higher tolerance ⇒ we care MORE about variety
/// ⇒ LOWER λ. So `λ = 1 − tolerance·(1 − λ_floor)`:
///   - tolerance 0 → λ = 1.0: pure quality, diversity ignored ⇒ (with the
///     companion cutoff at 1.0) nothing is dropped, all shown.
///   - tolerance 1 → λ = `λ_floor` (default 0.3): diversity dominates ⇒ MMR
///     stops admitting near-dups aggressively.
///
/// `λ_floor > 0` keeps a little quality pull even at max diversity so the
/// representative of a cluster is still its best frame, not an arbitrary one.
pub fn tolerance_to_lambda(tolerance: f32, lambda_floor: f32) -> f32 {
    let t = tolerance.clamp(0.0, 1.0);
    let floor = lambda_floor.clamp(0.0, 1.0);
    1.0 - t * (1.0 - floor)
}

// ---------------------------------------------------------------------------
// Objective (a) — greedy facility-location (the recommended core / default).
// ---------------------------------------------------------------------------

/// Greedy facility-location representative selection over the k-NN similarity
/// graph, thresholded at cutoff `τ`. THE DEFAULT objective the Diversify slider
/// drives.
///
/// ## The objective
///
/// Facility-location: `f(X) = Σ_i max_{j∈X} s_ij` — every image `i` is
/// "covered" by its most-similar chosen representative `j`. Maximizing `f` picks
/// a small `X` that covers the whole scope well (≈ k-medoid). `f` is monotone
/// submodular, so greedily adding the element of largest MARGINAL gain is a
/// `1 − 1/e` approximation (Nemhauser 1978; the design's arXiv 1805.11191).
///
/// ## How the cutoff enters (the slider)
///
/// We only let a representative `j` COVER an image `i` when `s_ij ≥ τ`. So `τ`
/// is the "how alike is alike enough to be redundant" knob: at `τ = 1.0` an
/// image covers only itself ⇒ everything is its own representative ⇒ all shown;
/// lowering `τ` lets one frame cover its whole burst ⇒ fewer representatives ⇒
/// more hidden. We stop adding representatives once EVERY image is covered at
/// `τ` (its similarity to some chosen rep ≥ τ). Each leftover image is its own
/// representative — so an all-distinct scope (no pair ≥ τ) stays fully shown,
/// the required "distinct images are left visible" behavior.
///
/// ## Greedy with quality tie-break
///
/// Marginal gain = the extra coverage a candidate would add (how many currently
/// uncovered images it would cover at `τ`, weighted by similarity). Ties — and
/// the seed pick — break toward higher [`QualityScores`], then lower hash, so
/// the representative of a cluster is its best frame and the result is
/// deterministic.
///
/// Complexity: O(rounds · n · deg) over the SPARSE edges, not O(n²) — `deg` is
/// the k-NN fan-out. Fine for the scope sizes the lens runs over.
pub fn facility_location_select(
    graph: &KnnGraph,
    scope: &[String],
    quality_scores: &QualityScores,
    cutoff: f32,
) -> Selection {
    let sim = SimGraph::from_knn(graph, scope);
    if sim.nodes.len() <= 1 {
        // Zero or one image: nothing to diversify, show what there is.
        return Selection::all_shown(sim.nodes);
    }

    // "Covered" = within `cutoff` similarity of some already-chosen rep.
    // best_cover[i] = the strongest similarity from i to any chosen rep so far.
    let mut best_cover: BTreeMap<&str, f32> =
        sim.nodes.iter().map(|h| (h.as_str(), 0.0_f32)).collect();
    let mut chosen: BTreeSet<String> = BTreeSet::new();

    // An image is covered once some rep sits at/above the cutoff; an image is
    // ALWAYS able to cover itself (sim 1.0 ≥ any cutoff ≤ 1), so the loop always
    // terminates — in the worst case every image becomes its own rep.
    let covered = |best_cover: &BTreeMap<&str, f32>, h: &str, cutoff: f32| -> bool {
        best_cover.get(h).copied().unwrap_or(0.0) >= cutoff
    };

    loop {
        // Are all images covered? If so we are done.
        if sim.nodes.iter().all(|h| covered(&best_cover, h, cutoff)) {
            break;
        }

        // Pick the UNCHOSEN node whose addition covers the most still-uncovered
        // mass at `cutoff`. Marginal gain sums, over each currently-uncovered
        // image the candidate would newly cover, the IMPROVEMENT in that image's
        // best-cover similarity — so a candidate that covers more, and more
        // tightly, wins. A candidate always covers ITSELF (sim 1.0).
        let mut best: Option<(f32, f32, &str)> = None; // (gain, quality, hash)
        for cand in &sim.nodes {
            if chosen.contains(cand) {
                continue;
            }
            let mut gain = 0.0_f32;
            // Self-coverage: the candidate becomes covered if it wasn't.
            let self_prev = best_cover.get(cand.as_str()).copied().unwrap_or(0.0);
            if 1.0 > self_prev && !covered(&best_cover, cand, cutoff) {
                gain += 1.0 - self_prev;
            }
            // Neighbor coverage at the cutoff.
            if let Some(neigh) = sim.adj.get(cand) {
                for (n, s) in neigh {
                    if *s < cutoff {
                        // Neighbors are sorted sim-desc, so once below the
                        // cutoff every later neighbor is too — stop early.
                        break;
                    }
                    let prev = best_cover.get(n.as_str()).copied().unwrap_or(0.0);
                    if *s > prev {
                        gain += *s - prev;
                    }
                }
            }
            let q = quality(quality_scores, cand);
            // Maximize gain; ties → higher quality → lower hash (deterministic).
            let better = match best {
                None => true,
                Some((bg, bq, bh)) => {
                    gain > bg
                        || (gain == bg && q > bq)
                        || (gain == bg && q == bq && cand.as_str() < bh)
                }
            };
            if better {
                best = Some((gain, q, cand.as_str()));
            }
        }

        let Some((_, _, pick)) = best else {
            break; // no candidate left (shouldn't happen — self-cover guarantees one)
        };
        let pick = pick.to_owned();

        // Commit the pick: it covers itself and every neighbor at/above cutoff.
        if let Some(cell) = best_cover.get_mut(pick.as_str()) {
            *cell = cell.max(1.0);
        }
        if let Some(neigh) = sim.adj.get(&pick) {
            for (n, s) in neigh {
                if *s < cutoff {
                    break;
                }
                if let Some(cell) = best_cover.get_mut(n.as_str()) {
                    *cell = cell.max(*s);
                }
            }
        }
        chosen.insert(pick);
    }

    finalize(&sim.nodes, chosen)
}

// ---------------------------------------------------------------------------
// Objective (b) — MMR / max-sum diversification (one λ knob).
// ---------------------------------------------------------------------------

/// Maximal Marginal Relevance selection with a single quality-vs-diversity knob
/// `λ`, plus a redundancy `cutoff` that stops admission (arXiv 1203.6397). An
/// alternate one-slider mapping to facility-location.
///
/// ## The rule
///
/// Greedily build the shown set `S`. Start empty; repeatedly admit the
/// unadmitted image `i` maximizing
///   `mmr(i) = λ·quality(i) − (1 − λ)·max_{j∈S} sim(i, j)`,
/// i.e. reward quality, penalize being similar to something already shown. We
/// STOP admitting an image once its similarity to the closest already-shown
/// image is ≥ `cutoff` (it would be redundant) — those become HIDDEN. So `λ`
/// tunes the quality/diversity balance and `cutoff` is the hard redundancy gate
/// the slider also lowers.
///
/// At `λ = 1` (tolerance 0) the diversity penalty vanishes and — with the
/// companion cutoff at 1.0 — every image clears the gate ⇒ all shown. Raising
/// tolerance lowers BOTH λ and the cutoff, so near-dups get gated out.
///
/// Quality is the same [`QualityScores`] (unscored ⇒ 0). Deterministic: ties
/// break toward higher quality then lower hash; `S` is seeded by the global
/// best-quality / lowest-hash image.
pub fn mmr_select(
    graph: &KnnGraph,
    scope: &[String],
    quality_scores: &QualityScores,
    lambda: f32,
    cutoff: f32,
) -> Selection {
    let sim = SimGraph::from_knn(graph, scope);
    if sim.nodes.len() <= 1 {
        return Selection::all_shown(sim.nodes);
    }
    let lambda = lambda.clamp(0.0, 1.0);

    let mut shown: Vec<String> = Vec::new();
    let mut hidden: BTreeSet<String> = BTreeSet::new();
    let mut remaining: BTreeSet<String> = sim.nodes.iter().cloned().collect();

    // Seed: the highest-quality (then lowest-hash) image is always shown — a
    // non-empty result is guaranteed, and the best frame anchors the view.
    let seed = sim
        .nodes
        .iter()
        .max_by(|a, b| {
            let qa = quality(quality_scores, a);
            let qb = quality(quality_scores, b);
            qa.partial_cmp(&qb)
                .unwrap_or(std::cmp::Ordering::Equal)
                // higher quality wins; on a tie LOWER hash wins, so flip the
                // hash comparison (max_by keeps the greater).
                .then_with(|| b.cmp(a))
        })
        .cloned()
        .expect("non-empty");
    remaining.remove(&seed);
    shown.push(seed);

    // Admit images one at a time by descending MMR until none remain. An image
    // whose nearest shown image is already ≥ cutoff is REDUNDANT — hide it
    // instead of admitting (this is what collapses a cluster to its rep).
    while !remaining.is_empty() {
        let mut best: Option<(f32, f32, String)> = None; // (mmr, quality, hash)
        let mut redundant: Vec<String> = Vec::new();
        for cand in &remaining {
            // Closest already-shown image (the diversity penalty + the gate).
            let max_sim = shown
                .iter()
                .map(|s| sim.sim(cand, s))
                .fold(0.0_f32, f32::max);
            if max_sim >= cutoff {
                redundant.push(cand.clone());
                continue;
            }
            let q = quality(quality_scores, cand);
            let score = lambda * q - (1.0 - lambda) * max_sim;
            let better = match &best {
                None => true,
                Some((bs, bq, bh)) => {
                    score > *bs
                        || (score == *bs && q > *bq)
                        || (score == *bs && q == *bq && cand < bh)
                }
            };
            if better {
                best = Some((score, q, cand.clone()));
            }
        }
        // Everything still in `remaining` that is redundant gets hidden now: it
        // is already covered by a shown image and admitting it later cannot
        // help (the shown set only grows, so `max_sim` only rises).
        for r in &redundant {
            remaining.remove(r);
            hidden.insert(r.clone());
        }
        match best {
            Some((_, _, pick)) => {
                remaining.remove(&pick);
                shown.push(pick);
            }
            // No admissible candidate left — the rest were all redundant.
            None => break,
        }
    }

    let chosen: BTreeSet<String> = shown.into_iter().collect();
    let _ = hidden; // hidden is recomputed in finalize from the chosen set
    finalize(&sim.nodes, chosen)
}

// ---------------------------------------------------------------------------
// Objective (c) — farthest-point / k-center greedy (2-approx). [optional]
// ---------------------------------------------------------------------------

/// Farthest-point (k-center greedy) representative selection: repeatedly add the
/// image FARTHEST (least similar) from everything chosen so far, until no
/// remaining image is still "too similar" to a chosen one (every remaining image
/// is within `cutoff` of some rep). Gonzalez's 1985 greedy is a 2-approximation
/// for the min-max-radius k-center objective.
///
/// This maximizes the MINIMUM pairwise distance of the shown set — the
/// "maximally distinct subset" feel. We expose it for completeness / founder
/// experimentation; the slider does NOT drive it by default (facility-location
/// covers clusters more intuitively). Same `cutoff`-as-tolerance contract:
/// `cutoff = 1.0` shows everything, lower hides more.
///
/// Deterministic: the seed and every farthest-point tie break toward higher
/// quality then lower hash.
pub fn k_center_select(
    graph: &KnnGraph,
    scope: &[String],
    quality_scores: &QualityScores,
    cutoff: f32,
) -> Selection {
    let sim = SimGraph::from_knn(graph, scope);
    if sim.nodes.len() <= 1 {
        return Selection::all_shown(sim.nodes);
    }

    // nearest_sim[i] = max similarity from i to any already-chosen rep.
    let mut nearest_sim: BTreeMap<&str, f32> =
        sim.nodes.iter().map(|h| (h.as_str(), 0.0_f32)).collect();
    let mut chosen: BTreeSet<String> = BTreeSet::new();

    // Seed with the best-quality / lowest-hash image (deterministic anchor).
    let seed = pick_by_quality(&sim.nodes, quality_scores);
    commit_center(&sim, &seed, &mut nearest_sim);
    chosen.insert(seed);

    loop {
        // Stop once every image is within `cutoff` of some chosen rep — the
        // remaining ones are all "redundant" and will be hidden.
        let mut farthest: Option<(f32, f32, &str)> = None; // (1-nearest_sim, quality, hash)
        for cand in &sim.nodes {
            if chosen.contains(cand) {
                continue;
            }
            let near = nearest_sim.get(cand.as_str()).copied().unwrap_or(0.0);
            if near >= cutoff {
                continue; // already covered ⇒ not a new center
            }
            // "Distance" from the chosen set = 1 − nearest similarity; we want
            // the image FARTHEST from everything chosen.
            let dist = 1.0 - near;
            let q = quality(quality_scores, cand);
            let better = match farthest {
                None => true,
                Some((bd, bq, bh)) => {
                    dist > bd
                        || (dist == bd && q > bq)
                        || (dist == bd && q == bq && cand.as_str() < bh)
                }
            };
            if better {
                farthest = Some((dist, q, cand.as_str()));
            }
        }
        match farthest {
            Some((_, _, pick)) => {
                let pick = pick.to_owned();
                commit_center(&sim, &pick, &mut nearest_sim);
                chosen.insert(pick);
            }
            None => break, // every remaining image is covered at the cutoff
        }
    }

    finalize(&sim.nodes, chosen)
}

/// Update `nearest_sim` after choosing `center`: every node's nearest-rep
/// similarity rises to at least its similarity to the new center (the center
/// itself becomes 1.0).
fn commit_center(sim: &SimGraph, center: &str, nearest_sim: &mut BTreeMap<&str, f32>) {
    if let Some(cell) = nearest_sim.get_mut(center) {
        *cell = 1.0;
    }
    if let Some(neigh) = sim.adj.get(center) {
        for (n, s) in neigh {
            if let Some(cell) = nearest_sim.get_mut(n.as_str()) {
                *cell = cell.max(*s);
            }
        }
    }
}

/// The highest-quality (then lowest-hash) image among `nodes` — the shared
/// deterministic seed for the center-style objectives.
fn pick_by_quality(nodes: &[String], quality_scores: &QualityScores) -> String {
    nodes
        .iter()
        .max_by(|a, b| {
            let qa = quality(quality_scores, a);
            let qb = quality(quality_scores, b);
            qa.partial_cmp(&qb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.cmp(a)) // tie ⇒ lower hash (flip for max_by)
        })
        .cloned()
        .expect("non-empty nodes")
}

/// Split the full node set into the `chosen` representatives (shown) and the
/// rest (hidden), both sorted by hash. The single place a [`Selection`] is
/// built from a chosen set, so every objective produces the identical shape.
fn finalize(nodes: &[String], chosen: BTreeSet<String>) -> Selection {
    let mut shown: Vec<String> = chosen.iter().cloned().collect();
    let mut hidden: Vec<String> = nodes
        .iter()
        .filter(|h| !chosen.contains(*h))
        .cloned()
        .collect();
    shown.sort();
    hidden.sort();
    Selection { shown, hidden }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `KnnGraph` from explicit directed edges: `(src, dst, sim)`.
    /// Lets a test plant a similarity structure directly — no CLIP vectors.
    fn knn(edges: &[(&str, &str, f32)]) -> KnnGraph {
        let mut by_src: BTreeMap<String, Vec<(String, f32)>> = BTreeMap::new();
        for (s, d, w) in edges {
            by_src
                .entry((*s).to_owned())
                .or_default()
                .push(((*d).to_owned(), *w));
        }
        // knn_within emits neighbors sim-desc; mirror that so tests match live.
        for list in by_src.values_mut() {
            list.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
        }
        by_src.into_iter().collect()
    }

    fn scope(hashes: &[&str]) -> Vec<String> {
        hashes.iter().map(|s| (*s).to_owned()).collect()
    }

    fn no_quality() -> QualityScores {
        BTreeMap::new()
    }

    /// A tight 3-image cluster (all mutually ~0.95 similar) plus one distinct
    /// outlier. At a moderate cutoff, facility-location must collapse the
    /// cluster to ONE representative and keep the outlier — the core behavior.
    #[test]
    fn facility_location_collapses_a_tight_cluster() {
        let g = knn(&[
            ("a", "b", 0.96),
            ("a", "c", 0.95),
            ("b", "a", 0.96),
            ("b", "c", 0.94),
            ("c", "a", 0.95),
            ("c", "b", 0.94),
            // outlier d is far from everything (no edges ≥ cutoff).
            ("d", "a", 0.10),
            ("a", "d", 0.10),
        ]);
        let sel = facility_location_select(&g, &scope(&["a", "b", "c", "d"]), &no_quality(), 0.9);
        // One of {a,b,c} represents the cluster; d is its own rep ⇒ 2 shown.
        assert_eq!(sel.shown.len(), 2, "cluster collapses to 1 + outlier");
        assert!(
            sel.shown.contains(&"d".to_string()),
            "distinct outlier shown"
        );
        assert_eq!(sel.hidden.len(), 2, "two cluster frames hidden");
        // Every image is accounted for exactly once.
        assert_eq!(sel.shown.len() + sel.hidden.len(), 4);
    }

    /// The representative of a cluster is its HIGHEST-QUALITY frame (the
    /// "keep the sharpest / highest-rated" rule). In a burst the frames are
    /// near-identically similar, so each candidate covers the cluster equally —
    /// the coverage gain TIES and the quality tie-break decides which frame
    /// represents. (Where coverage differs materially, facility-location
    /// rightly prefers the better-covering frame; quality only breaks ties.)
    #[test]
    fn facility_location_picks_highest_quality_representative() {
        // Symmetric, equal-similarity cluster (a realistic burst): all three
        // mutually 0.96 ⇒ identical marginal gain ⇒ quality decides.
        let g = knn(&[
            ("a", "b", 0.96),
            ("b", "a", 0.96),
            ("a", "c", 0.96),
            ("c", "a", 0.96),
            ("b", "c", 0.96),
            ("c", "b", 0.96),
        ]);
        let mut q = QualityScores::new();
        q.insert("b".to_owned(), 5.0); // b is the sharpest frame
        let sel = facility_location_select(&g, &scope(&["a", "b", "c"]), &q, 0.9);
        assert_eq!(
            sel.shown,
            vec!["b".to_string()],
            "sharpest frame represents"
        );
    }

    /// All-distinct images (no pair similar enough) are left FULLY SHOWN at a
    /// moderate tolerance — the "don't hide variety" guarantee.
    #[test]
    fn all_distinct_images_stay_shown() {
        let g = knn(&[
            ("a", "b", 0.20),
            ("b", "a", 0.20),
            ("b", "c", 0.15),
            ("c", "b", 0.15),
            ("a", "c", 0.10),
            ("c", "a", 0.10),
        ]);
        // cutoff 0.85 (a moderate tolerance) — nothing is that similar.
        let sel = facility_location_select(&g, &scope(&["a", "b", "c"]), &no_quality(), 0.85);
        assert_eq!(sel.shown.len(), 3, "all distinct ⇒ all shown");
        assert!(sel.hidden.is_empty());
    }

    /// Tolerance MONOTONICITY (the load-bearing property): as tolerance rises
    /// (cutoff falls), the number of shown images never INCREASES. Swept over a
    /// mixed graph for facility-location.
    #[test]
    fn facility_location_is_monotone_in_tolerance() {
        let g = knn(&[
            ("a", "b", 0.98),
            ("b", "a", 0.98),
            ("c", "d", 0.90),
            ("d", "c", 0.90),
            ("a", "c", 0.70),
            ("c", "a", 0.70),
            ("e", "a", 0.30),
            ("a", "e", 0.30),
        ]);
        let s = scope(&["a", "b", "c", "d", "e"]);
        let q = no_quality();
        let mut prev_shown = usize::MAX;
        // Sweep tolerance 0 → 1 in steps; map to cutoff via the documented curve.
        for step in 0..=20 {
            let tol = step as f32 / 20.0;
            let cutoff = tolerance_to_cutoff(tol, 1.0, 0.55);
            let n = facility_location_select(&g, &s, &q, cutoff).shown.len();
            assert!(
                n <= prev_shown,
                "shown count rose at tolerance {tol} (cutoff {cutoff}): {n} > {prev_shown}"
            );
            prev_shown = n;
        }
    }

    /// MMR is also monotone in tolerance (both λ and cutoff move together).
    #[test]
    fn mmr_is_monotone_in_tolerance() {
        let g = knn(&[
            ("a", "b", 0.97),
            ("b", "a", 0.97),
            ("b", "c", 0.92),
            ("c", "b", 0.92),
            ("c", "d", 0.60),
            ("d", "c", 0.60),
            ("e", "a", 0.20),
            ("a", "e", 0.20),
        ]);
        let s = scope(&["a", "b", "c", "d", "e"]);
        let q = no_quality();
        let mut prev = usize::MAX;
        for step in 0..=20 {
            let tol = step as f32 / 20.0;
            let cutoff = tolerance_to_cutoff(tol, 1.0, 0.55);
            let lambda = tolerance_to_lambda(tol, 0.3);
            let n = mmr_select(&g, &s, &q, lambda, cutoff).shown.len();
            assert!(n <= prev, "MMR shown rose at tolerance {tol}: {n} > {prev}");
            prev = n;
        }
    }

    /// k-center is monotone too — every objective honors the slider.
    #[test]
    fn k_center_is_monotone_in_tolerance() {
        let g = knn(&[
            ("a", "b", 0.95),
            ("b", "a", 0.95),
            ("c", "d", 0.88),
            ("d", "c", 0.88),
            ("a", "c", 0.50),
            ("c", "a", 0.50),
        ]);
        let s = scope(&["a", "b", "c", "d"]);
        let q = no_quality();
        let mut prev = usize::MAX;
        for step in 0..=20 {
            let tol = step as f32 / 20.0;
            let cutoff = tolerance_to_cutoff(tol, 1.0, 0.55);
            let n = k_center_select(&g, &s, &q, cutoff).shown.len();
            assert!(
                n <= prev,
                "k-center shown rose at tolerance {tol}: {n} > {prev}"
            );
            prev = n;
        }
    }

    /// Tolerance 0 (cutoff 1.0) shows EVERYTHING for every objective — the
    /// "slider at zero shows all" contract.
    #[test]
    fn tolerance_zero_shows_everything() {
        let g = knn(&[
            ("a", "b", 0.999),
            ("b", "a", 0.999),
            ("a", "c", 0.97),
            ("c", "a", 0.97),
        ]);
        let s = scope(&["a", "b", "c"]);
        let q = no_quality();
        let cutoff = tolerance_to_cutoff(0.0, 1.0, 0.55); // == 1.0
        assert_eq!(cutoff, 1.0);
        assert_eq!(facility_location_select(&g, &s, &q, cutoff).shown.len(), 3);
        let lambda = tolerance_to_lambda(0.0, 0.3); // == 1.0
        assert_eq!(mmr_select(&g, &s, &q, lambda, cutoff).shown.len(), 3);
        assert_eq!(k_center_select(&g, &s, &q, cutoff).shown.len(), 3);
    }

    /// Determinism: the same inputs yield the identical selection, twice, for
    /// every objective (no hash-map-order leakage).
    #[test]
    fn selection_is_deterministic() {
        let g = knn(&[
            ("a", "b", 0.95),
            ("b", "a", 0.95),
            ("c", "d", 0.93),
            ("d", "c", 0.93),
            ("b", "c", 0.80),
            ("c", "b", 0.80),
            ("e", "f", 0.91),
            ("f", "e", 0.91),
        ]);
        let s = scope(&["a", "b", "c", "d", "e", "f"]);
        let q = no_quality();
        let cutoff = 0.9;
        let a1 = facility_location_select(&g, &s, &q, cutoff);
        let a2 = facility_location_select(&g, &s, &q, cutoff);
        assert_eq!(a1, a2);
        let m1 = mmr_select(&g, &s, &q, 0.5, cutoff);
        let m2 = mmr_select(&g, &s, &q, 0.5, cutoff);
        assert_eq!(m1, m2);
        let k1 = k_center_select(&g, &s, &q, cutoff);
        let k2 = k_center_select(&g, &s, &q, cutoff);
        assert_eq!(k1, k2);
    }

    /// An empty scope, and a single-image scope, both return a well-formed
    /// "all shown" selection rather than panicking — the graceful posture.
    #[test]
    fn empty_and_singleton_are_graceful() {
        let empty = facility_location_select(&knn(&[]), &scope(&[]), &no_quality(), 0.9);
        assert!(empty.shown.is_empty() && empty.hidden.is_empty());
        let one = facility_location_select(&knn(&[]), &scope(&["solo"]), &no_quality(), 0.9);
        assert_eq!(one.shown, vec!["solo".to_string()]);
        assert!(one.hidden.is_empty());
    }

    /// The symmetrization keeps the MAX of the two directed sims: if A lists B
    /// at 0.95 but B never lists A, the pair is still treated as 0.95-similar
    /// (either frame seeing the other as a near neighbor makes them redundant).
    #[test]
    fn asymmetric_edge_is_symmetrized_by_max() {
        let g = knn(&[
            // a sees b strongly, but b's own top-k never lists a:
            ("a", "b", 0.95),
            ("b", "x", 0.99),
            ("x", "b", 0.99),
        ]);
        let sel = facility_location_select(&g, &scope(&["a", "b", "x"]), &no_quality(), 0.9);
        // b and x collapse (0.99); a collapses into b via the symmetrized 0.95.
        // So exactly one representative covers all three at cutoff 0.9.
        assert_eq!(
            sel.shown.len(),
            1,
            "symmetrized edge lets one rep cover all"
        );
    }

    /// The tolerance→cutoff curve is itself monotone non-increasing and pinned
    /// at its documented endpoints (the contract the slider relies on).
    #[test]
    fn tolerance_to_cutoff_curve_is_monotone_and_pinned() {
        assert_eq!(tolerance_to_cutoff(0.0, 1.0, 0.55), 1.0);
        assert!((tolerance_to_cutoff(1.0, 1.0, 0.55) - 0.55).abs() < 1e-6);
        let mut prev = f32::INFINITY;
        for step in 0..=100 {
            let tol = step as f32 / 100.0;
            let c = tolerance_to_cutoff(tol, 1.0, 0.55);
            assert!(c <= prev + 1e-7, "cutoff rose at tol {tol}");
            prev = c;
        }
        // An inverted (low > high) pair is defended against, not honored.
        assert_eq!(tolerance_to_cutoff(0.0, 0.55, 1.0), 1.0);
    }

    /// `tolerance_to_lambda` lands at its documented endpoints and is monotone
    /// non-increasing (higher tolerance ⇒ lower λ ⇒ more diversity weight).
    #[test]
    fn tolerance_to_lambda_endpoints_and_monotonicity() {
        assert_eq!(tolerance_to_lambda(0.0, 0.3), 1.0);
        assert!((tolerance_to_lambda(1.0, 0.3) - 0.3).abs() < 1e-6);
        let mut prev = f32::INFINITY;
        for step in 0..=100 {
            let tol = step as f32 / 100.0;
            let l = tolerance_to_lambda(tol, 0.3);
            assert!(l <= prev + 1e-7);
            prev = l;
        }
    }
}
