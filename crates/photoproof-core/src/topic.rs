//! Semantic topic-graph backend (DESIGN-SEMANTIC-GRAPH.md, v1).
//!
//! The force-directed lens generalizes "more like this" from an IMAGE anchor
//! to a TOPIC PHRASE anchor: every in-scope image is scored against each named
//! topic in BOTH the CLIP-text space (what it LOOKS like) and the text-embedder
//! space (what you SAID about it), then blended:
//!
//!   affinity = α·cos(image_clip, clip_text_topic)
//!            + (1−α)·cos(annotation/summary, text_topic)
//!
//! This REUSES the existing retrieval machinery wholesale (RETRIEVAL §1.3): the
//! same `Embedder` seam the hybrid search uses to embed a query, and the same
//! PPVEC brute-force cosine kernel (`PpvecStore::score_images`) the S4/S1 paths
//! score with. Nothing here is a second similarity definition — the topic is
//! just a different reference vector.
//!
//! GRACEFUL POSTURE (founder-binding, like the whole M1 product): an image with
//! no vector, an un-embedded library, or an absent embedder all yield zeros /
//! empty rather than an error. The mechanism must be CORRECT before an embed
//! pass and must NOT crash when models aren't loaded — so every "no signal"
//! path returns 0.0 affinity, never `Err`.
//!
//! v1 deliberately keeps suggestions cheap (note n-grams + collection names);
//! the cluster auto-labels (v2) and LLM topic extraction (v3) wait on their
//! design phases. The force LAYOUT itself is frontend (logic/forcegraph.ts).

use std::collections::HashMap;

use photoproof_connectors::embedder::{Embedder, Embedding};
use photoproof_connectors::vector_store::{VecKind, VecSpace};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::retrieval::{PpvecStore, instruct_query};

/// One image's blended affinity to one topic. The frontend turns this into a
/// pull force toward that topic's anchor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TopicScore {
    /// Index into the `topics` slice the caller passed (stable ordering, so the
    /// frontend never has to re-match phrases).
    pub topic: usize,
    /// Blended affinity `α·visual + (1−α)·annotation`, in roughly [-1, 1] (a
    /// cosine). Higher = stronger pull. A space with no stored vector for this
    /// image contributes 0 to its side of the blend.
    pub affinity: f32,
}

/// One in-scope image and its affinity to every topic. Topics the image has no
/// signal for are still listed at affinity 0 (the frontend wants a dense
/// per-topic row so the layout is stable as topics come and go).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageAffinities {
    pub image_hash: String,
    pub scores: Vec<TopicScore>,
}

/// The `topic_affinities` result plus the honesty flags the UI surfaces (so the
/// founder can SEE when the lens is running degraded rather than guessing why
/// everything sits at the center).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffinityReport {
    /// One row per in-scope image (every image, even all-zero ones — the
    /// frontend places them at the center).
    pub images: Vec<ImageAffinities>,
    /// True when the CLIP text tower embedded the topics (the visual half of
    /// the blend carried signal). False ⇒ every visual term was 0.
    pub visual_ready: bool,
    /// True when the text embedder embedded the topics (the annotation half).
    pub annotation_ready: bool,
}

/// Blend the two cosines: `α·visual + (1−α)·annotation`. A space that produced
/// no score for an image contributes 0 on its side (an honest "no signal", not
/// a guess). `α` is clamped to [0, 1] so a bad caller value can never invert a
/// term. Pure + total so the blend is unit-testable at α = 0 / 1 / 0.5.
fn blend(visual: Option<f32>, annotation: Option<f32>, alpha: f64) -> f32 {
    let a = alpha.clamp(0.0, 1.0) as f32;
    a * visual.unwrap_or(0.0) + (1.0 - a) * annotation.unwrap_or(0.0)
}

/// Embed one topic phrase in BOTH spaces (whichever embedders are present) and
/// score every in-scope image against it. `None` embedder ⇒ that half is empty
/// (graceful — the blend just uses the other half).
///
/// The text side embeds with the §3 query instruct template (matching how the
/// S1/S3 query path embeds, so the topic sits in the same region of the space
/// as a real search would); the CLIP side embeds the BARE phrase on the text
/// tower (no template — §3 / B69, exactly the S4 query path).
fn score_topic<TE: Embedder, CE: Embedder>(
    topic: &str,
    scope: &[String],
    vectors: &PpvecStore,
    text: Option<&TE>,
    clip: Option<&CE>,
) -> (HashMap<String, f32>, HashMap<String, f32>) {
    // Visual half — CLIP image space vs the CLIP-text topic embedding (bare
    // phrase, B69). Any failure (embed error, empty space) degrades to empty.
    let visual = clip
        .and_then(|ce| embed_then_score(ce, topic, VecKind::ImageClip, vectors, scope))
        .unwrap_or_default();
    // Annotation half — the image's annotation/summary vector vs the text-topic
    // embedding. v1 scores the `image_summary` space (one image-keyed row per
    // image, exactly what the lens needs); annotation_chunk is event-keyed, so
    // the per-image summary is the natural image-level annotation signal here.
    let annotation = text
        .and_then(|te| {
            embed_then_score(
                te,
                &instruct_query(topic),
                VecKind::ImageSummary,
                vectors,
                scope,
            )
        })
        .unwrap_or_default();
    (visual, annotation)
}

/// Embed `text` through `embedder` and score the scope's images in that space.
/// Returns `None` on any embed failure (degrade to "no signal"); an embedded
/// query over an empty space simply yields an empty map (not `None`).
fn embed_then_score<E: Embedder>(
    embedder: &E,
    text: &str,
    vec_kind: VecKind,
    vectors: &PpvecStore,
    scope: &[String],
) -> Option<HashMap<String, f32>> {
    let q: Embedding = pollster::block_on(embedder.embed_text(text))
        .map_err(|e| {
            // A model that errors degrades that half to absent — the lens must
            // not crash because an embedder hiccuped (the §5.1 search posture).
            tracing::warn!(error = %e, ?vec_kind, "topic embed failed; that half degrades to absent");
        })
        .ok()?;
    let space = VecSpace {
        vec_kind,
        model_id: embedder.model_id().to_owned(),
    };
    match vectors.score_images(&q, space, scope) {
        Ok(scores) => Some(scores),
        Err(e) => {
            tracing::warn!(error = %e, ?vec_kind, "topic scoring failed; that half degrades to absent");
            None
        }
    }
}

/// `topic_affinities` (DESIGN-SEMANTIC-GRAPH.md): for every image in `scope`,
/// its blended affinity to every topic in `topics`, at blend `alpha`.
///
/// REUSES `PpvecStore::score_images` (the brute-force cosine kernel) and the
/// `Embedder` seam — the topic is just a different reference vector. Always
/// returns a dense report (every scope image, every topic) so the frontend's
/// layout is stable; a topic with no embedder signal contributes 0 affinity.
///
/// Never errors on a degraded rig: an empty scope, empty topics, an
/// un-embedded index, or absent embedders all yield a well-formed report with
/// zeros and the readiness flags set honestly.
pub fn topic_affinities<TE: Embedder, CE: Embedder>(
    scope: &[String],
    topics: &[String],
    alpha: f64,
    vectors: &PpvecStore,
    text: Option<&TE>,
    clip: Option<&CE>,
) -> AffinityReport {
    // Score each topic once across the whole scope, then transpose into the
    // per-image rows the frontend consumes. Computing per-topic (not per-image)
    // means one embed + one space scan per topic, not per image — the cheap
    // shape DESIGN asks for ("compute once per topic-set/alpha change").
    let mut per_topic: Vec<(HashMap<String, f32>, HashMap<String, f32>)> =
        Vec::with_capacity(topics.len());
    let mut visual_ready = false;
    let mut annotation_ready = false;
    for topic in topics {
        let (visual, annotation) = score_topic(topic, scope, vectors, text, clip);
        // "Ready" = this half produced at least one non-empty score map for
        // some topic — i.e. the embedder ran AND the space had a vector.
        visual_ready |= !visual.is_empty();
        annotation_ready |= !annotation.is_empty();
        per_topic.push((visual, annotation));
    }

    let images = scope
        .iter()
        .map(|hash| {
            let scores = per_topic
                .iter()
                .enumerate()
                .map(|(topic, (visual, annotation))| TopicScore {
                    topic,
                    affinity: blend(
                        visual.get(hash).copied(),
                        annotation.get(hash).copied(),
                        alpha,
                    ),
                })
                .collect();
            ImageAffinities {
                image_hash: hash.clone(),
                scores,
            }
        })
        .collect();

    AffinityReport {
        images,
        visual_ready,
        annotation_ready,
    }
}

// ---------------------------------------------------------------------------
// suggest_topics — cheap v1 candidates (note n-grams + collection names)
// ---------------------------------------------------------------------------

/// `suggest_topics` v1 caps: keep the rail short and the n-gram scan cheap. NOT
/// tuning knobs the founder re-feels (they shape a suggestion RAIL, not a
/// ranking) — plain caps, deliberately not in tuning.toml.
const MAX_SUGGESTIONS: usize = 24;
/// An n-gram must appear at least this many times across the scope's notes to
/// be a suggestion — a one-off phrase is noise, not a theme.
const MIN_NGRAM_COUNT: usize = 2;
/// n-gram widths to mine (1 = salient single words, 2/3 = short phrases).
const NGRAM_WIDTHS: [usize; 3] = [1, 2, 3];

/// English stopwords dropped from single-word suggestions and from the EDGES of
/// phrases (a phrase may contain them internally — "edge of night" is fine, but
/// "the" alone is not a topic). Small, hard-coded, ASCII-folded: v1 is cheap by
/// design (no tokenizer, no model — that is v3).
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for", "with", "is", "it",
    "this", "that", "was", "are", "be", "as", "by", "i", "im", "its", "my", "we", "so", "up",
    "out", "if", "no", "not", "just", "very", "really", "got", "get", "had", "has", "into",
];

/// A suggested topic candidate plus where it came from (the rail can style a
/// collection-name chip differently from a mined note phrase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicSuggestion {
    pub phrase: String,
    /// `"note"` (mined from the scope's annotation text) or `"collection"` (a
    /// collection name the scope's images belong to).
    pub source: String,
    /// How many times the phrase recurred (note n-grams) — the rail can rank by
    /// it. Collection names carry their member-overlap count.
    pub count: usize,
}

/// `suggest_topics` (DESIGN-SEMANTIC-GRAPH.md, v1): cheap candidate topics for
/// the suggestion rail — frequent note n-grams over the scope's annotation text
/// plus the names of collections the scope's images belong to. NO LLM (that is
/// v3); NO clustering (v2). Pure string mining + a name lookup.
///
/// `note_texts` is the scope's live annotation prose (the caller pulls it from
/// the journal — see the desktop command); `collection_names` are
/// `(name, overlap_count)` pairs for collections the scope intersects. Both are
/// passed in so this stays a pure, unit-testable reducer with no DB coupling.
///
/// Returns up to `MAX_SUGGESTIONS`, collection names first (they are
/// human-authored, the strongest signal), then note n-grams by descending
/// frequency. De-duplicated case-insensitively so a collection named "Harbor"
/// and a frequent "harbor" n-gram surface once.
pub fn suggest_topics(
    note_texts: &[String],
    collection_names: &[(String, usize)],
) -> Vec<TopicSuggestion> {
    let mut out: Vec<TopicSuggestion> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Collection names first — authored intent beats mined phrases.
    for (name, overlap) in collection_names {
        let key = name.to_lowercase();
        let trimmed = name.trim();
        if trimmed.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(TopicSuggestion {
            phrase: trimmed.to_owned(),
            source: "collection".to_owned(),
            count: *overlap,
        });
    }

    // Mine recurring n-grams from the notes (the shared salient-phrase miner the
    // v2 cluster labeling reuses).
    let mut grams: Vec<(String, usize)> = mine_ngrams(note_texts)
        .into_iter()
        .filter(|(_, c)| *c >= MIN_NGRAM_COUNT)
        .collect();
    // Descending frequency, then alphabetical for a deterministic tie-break (so
    // the rail order is stable across runs — tests can pin it).
    grams.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    for (phrase, count) in grams {
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
        if !seen.insert(phrase.clone()) {
            continue;
        }
        out.push(TopicSuggestion {
            phrase,
            source: "note".to_owned(),
            count,
        });
    }
    out.truncate(MAX_SUGGESTIONS);
    out
}

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Mine salient n-grams (widths 1/2/3) from a slice of note prose, returning
/// `phrase -> occurrence count`. The shared phrase miner: `suggest_topics` ranks
/// these by frequency for the v1 rail, and the v2 cluster labeling runs it over
/// just ONE cluster's note text to pick that cluster's most representative
/// phrase. Same stopword-edge + bare-word rules as v1 so both surfaces speak the
/// same phrase vocabulary. NOT frequency-floored here (a single cluster's notes
/// may each mention its theme once) — the caller applies whatever floor it wants.
fn mine_ngrams(note_texts: &[String]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for text in note_texts {
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect();
        for &width in &NGRAM_WIDTHS {
            if words.len() < width {
                continue;
            }
            for window in words.windows(width) {
                // Reject a gram whose first OR last word is a stopword (keeps
                // "harbor at dusk" but drops "at dusk and" / bare "the").
                let first = window.first().map(String::as_str).unwrap_or("");
                let last = window.last().map(String::as_str).unwrap_or("");
                if is_stopword(first) || is_stopword(last) {
                    continue;
                }
                // Single bare words must also be substantive (>2 chars): "ok",
                // "go" are not topics.
                if width == 1 && window[0].len() <= 2 {
                    continue;
                }
                *counts.entry(window.join(" ")).or_insert(0) += 1;
            }
        }
    }
    counts
}

// ---------------------------------------------------------------------------
// Scope enumeration — the in-scope annotation text for suggest_topics
// ---------------------------------------------------------------------------

/// Pull the live remark/revision text of every event targeting any image in
/// `scope`, for `suggest_topics`' n-gram mining. Reads the journal spine
/// directly (read-only): live (non-redacted) `remark`/`revision` rows joined to
/// `event_targets`. An empty scope or a scope with no notes yields an empty
/// vec — never an error.
///
/// WHY a `&Connection` rather than a higher-level API: this is a read-only
/// projection the desktop command runs on its own read connection; keeping it
/// here (next to its only consumer) avoids threading a new method through the
/// journal API for one scan.
pub fn scope_note_texts(conn: &Connection, scope: &[String]) -> rusqlite::Result<Vec<String>> {
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; scope.len()].join(",");
    // DISTINCT on the event id: a multi-target remark must count once, not once
    // per targeted image (it is one phrase the photographer wrote). `redacted_by
    // IS NULL` drops scrubbed text; `text IS NOT NULL` skips ratings/strokes.
    let sql = format!(
        "SELECT DISTINCT e.id, e.text
         FROM annotation_events e
         JOIN event_targets t ON t.event_id = e.id
         WHERE e.kind IN ('remark','revision')
           AND e.redacted_by IS NULL
           AND e.text IS NOT NULL
           AND t.image_hash IN ({marks})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(scope.iter());
    let rows = stmt.query_map(params, |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// Like [`scope_note_texts`] but grouped BY image hash, for `cluster_topics`'
/// per-cluster labeling: each cluster mines only ITS members' note text, so the
/// label is grounded in those specific images. A multi-target remark legitimately
/// attaches to each image it targets (the phrase applies to each), so here we
/// key on `image_hash` rather than de-duping by event. An image with no notes is
/// simply absent from the map (its cluster just contributes no phrases from it).
pub fn scope_note_texts_by_hash(
    conn: &Connection,
    scope: &[String],
) -> rusqlite::Result<HashMap<String, Vec<String>>> {
    if scope.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = vec!["?"; scope.len()].join(",");
    let sql = format!(
        "SELECT t.image_hash, e.text
         FROM annotation_events e
         JOIN event_targets t ON t.event_id = e.id
         WHERE e.kind IN ('remark','revision')
           AND e.redacted_by IS NULL
           AND e.text IS NOT NULL
           AND t.image_hash IN ({marks})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(scope.iter());
    let rows = stmt.query_map(params, |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (hash, text) = row?;
        out.entry(hash).or_default().push(text);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// cluster_topics — v2 cluster auto-labels (note-grounded auto-topics)
// ---------------------------------------------------------------------------
//
// v1's suggestions are frequent n-grams over the whole scope (above); v2 adds
// SMARTER suggestions grounded in the embedding structure: cluster the in-scope
// image vectors with a small k-means, then LABEL each cluster by the note phrase
// most representative of it. These become note-grounded auto-topics on the same
// suggestion rail (DESIGN §topics step 2: "cluster the embeddings; label each
// cluster by its nearest note phrase").
//
// The math is deliberately small + self-contained (no heavy clustering dep) and
// DETERMINISTIC (farthest-first seeding by index, fixed iteration order) so the
// labels are reproducible and the tests can plant clusters and assert them.

/// One labeled cluster: a note-grounded auto-topic for the suggestion rail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterTopic {
    /// The chosen label — the cluster's most representative note phrase, or a
    /// generic `"Group N"` fallback when the cluster's images carry no notes.
    pub label: String,
    /// How many images fell in this cluster.
    pub size: usize,
    /// Mean cosine of the cluster's members to their centroid in [-1, 1]: how
    /// TIGHT the cluster is (a coherent theme scores high; a loose grab-bag
    /// low). The rail can rank or de-emphasize by it.
    pub centroid_affinity: f32,
}

/// Pick a cluster count from the scope size: `clamp(round(sqrt(n/2)), k_min,
/// k_max)` (DESIGN heuristic). A handful of images yields `k_min`; a large
/// scope is capped at `k_max` so the rail stays short and the k-means cheap.
fn pick_k(n: usize, k_min: usize, k_max: usize) -> usize {
    let raw = ((n as f64) / 2.0).sqrt().round() as usize;
    raw.clamp(k_min.max(1), k_max.max(k_min.max(1)))
}

/// Cosine similarity of two equal-length vectors. Zero-norm (an all-zero vector)
/// yields 0 — an honest "no direction", never a NaN.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Squared Euclidean distance (k-means assigns by nearest centroid; the squared
/// form avoids a sqrt in the hot assignment loop).
fn dist2(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// Deterministic k-means over `points` into `k` clusters: returns each point's
/// cluster assignment (index into 0..k). DETERMINISTIC by construction so a
/// fixture clusters the same way every run (the test plants clusters and pins
/// the assignment):
///   - SEEDING is farthest-first from `points[0]` (no RNG): centroid 0 is the
///     first point, then each next centroid is the point farthest from all
///     chosen so far — a spread, reproducible init that separates planted
///     clusters cleanly (a seeded k-means++ without the randomness).
///   - Lloyd iterations run a FIXED number of passes (or until assignments
///     stop changing); ties in the nearest-centroid pick go to the lower index.
///
/// An empty `points`, `k == 0`, or `k >= n` is handled gracefully (each point
/// its own cluster when k >= n).
fn kmeans(points: &[Vec<f32>], k: usize, max_iters: usize) -> Vec<usize> {
    let n = points.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    let dims = points[0].len();
    let k = k.min(n);

    // Farthest-first seeding (deterministic): start at point 0, then repeatedly
    // add the point with the maximum distance to its nearest chosen centroid.
    let mut centroid_idx: Vec<usize> = vec![0];
    let mut nearest_d2: Vec<f32> = points.iter().map(|p| dist2(p, &points[0])).collect();
    while centroid_idx.len() < k {
        // Pick the farthest point (lowest index wins a tie for determinism).
        let mut best = 0usize;
        let mut best_d = -1.0f32;
        for (i, &d) in nearest_d2.iter().enumerate() {
            if d > best_d {
                best_d = d;
                best = i;
            }
        }
        centroid_idx.push(best);
        // Refresh each point's distance to its NEAREST chosen centroid.
        for (i, p) in points.iter().enumerate() {
            let d = dist2(p, &points[best]);
            if d < nearest_d2[i] {
                nearest_d2[i] = d;
            }
        }
    }
    let mut centroids: Vec<Vec<f32>> = centroid_idx.iter().map(|&i| points[i].clone()).collect();

    let mut assign = vec![0usize; n];
    for _ in 0..max_iters.max(1) {
        // Assignment step: each point to its nearest centroid (low index wins).
        let mut changed = false;
        for (i, p) in points.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = f32::INFINITY;
            for (c, centroid) in centroids.iter().enumerate() {
                let d = dist2(p, centroid);
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            if assign[i] != best {
                changed = true;
            }
            assign[i] = best;
        }
        // Update step: each centroid is the mean of its members. An EMPTY
        // cluster keeps its previous centroid (it simply attracts nothing — no
        // re-seed jitter, which would break determinism).
        let mut sums = vec![vec![0.0f32; dims]; centroids.len()];
        let mut counts = vec![0usize; centroids.len()];
        for (i, p) in points.iter().enumerate() {
            let c = assign[i];
            counts[c] += 1;
            for (s, &v) in sums[c].iter_mut().zip(p) {
                *s += v;
            }
        }
        for (c, centroid) in centroids.iter_mut().enumerate() {
            if counts[c] > 0 {
                for (slot, &sum) in centroid.iter_mut().zip(&sums[c]) {
                    *slot = sum / counts[c] as f32;
                }
            }
        }
        if !changed {
            break;
        }
    }
    assign
}

/// `cluster_topics` (DESIGN-SEMANTIC-GRAPH.md, v2): cluster the in-scope image
/// vectors and label each cluster by its most representative note phrase, for
/// the suggestion rail's smarter, note-grounded auto-topics.
///
/// PURE over its inputs so it is unit-testable without a DB or an embedder (the
/// command pulls the vectors + notes and calls this):
///   - `vectors` — `(image_hash, vector)` for the in-scope images THAT HAVE a
///     stored vector in the clustering space (image_summary by default, or
///     image_clip). An un-embedded scope passes an empty slice → empty result.
///   - `notes_by_hash` — `image_hash -> [note prose]` for labeling (an image
///     with no notes simply contributes no phrases to its cluster).
///   - `k` — explicit cluster count, or `None` to pick by the size heuristic
///     within `[k_min, k_max]`.
///
/// Returns `[{ label, size, centroid_affinity }]`, one per non-empty cluster,
/// sorted by descending size then label (deterministic order). Each cluster's
/// LABEL is the most frequent salient n-gram across its members' notes (the
/// shared `mine_ngrams` miner), tie-broken alphabetically; a cluster whose
/// images carry no notes falls back to a generic `"Group N"`.
///
/// GRACEFUL (the whole-product posture): empty/un-embedded input returns an
/// empty vec, never an error.
#[allow(clippy::too_many_arguments)]
pub fn cluster_topics(
    vectors: &[(String, Vec<f32>)],
    notes_by_hash: &HashMap<String, Vec<String>>,
    k: Option<usize>,
    k_min: usize,
    k_max: usize,
) -> Vec<ClusterTopic> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let points: Vec<Vec<f32>> = vectors.iter().map(|(_, v)| v.clone()).collect();
    let chosen_k = k
        .map(|k| k.clamp(1, vectors.len()))
        .unwrap_or_else(|| pick_k(vectors.len(), k_min, k_max));
    let assign = kmeans(&points, chosen_k, 50);

    // Gather members + recompute each final centroid (the mean of its members),
    // so centroid_affinity reflects the SETTLED layout, not the seed.
    let n_clusters = assign.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let dims = points[0].len();
    let mut centroids = vec![vec![0.0f32; dims]; n_clusters];
    let mut counts = vec![0usize; n_clusters];
    for (i, &c) in assign.iter().enumerate() {
        counts[c] += 1;
        for (s, &v) in centroids[c].iter_mut().zip(&points[i]) {
            *s += v;
        }
    }
    for (c, centroid) in centroids.iter_mut().enumerate() {
        if counts[c] > 0 {
            for slot in centroid.iter_mut() {
                *slot /= counts[c] as f32;
            }
        }
    }

    let mut out: Vec<ClusterTopic> = Vec::new();
    for c in 0..n_clusters {
        if counts[c] == 0 {
            continue;
        }
        // Tightness: mean cosine of members to the cluster centroid.
        let mut affinity_sum = 0.0f32;
        // The cluster's note text, for labeling.
        let mut cluster_notes: Vec<String> = Vec::new();
        for (i, &assigned) in assign.iter().enumerate() {
            if assigned != c {
                continue;
            }
            affinity_sum += cosine(&points[i], &centroids[c]);
            if let Some(notes) = notes_by_hash.get(&vectors[i].0) {
                cluster_notes.extend(notes.iter().cloned());
            }
        }
        let centroid_affinity = affinity_sum / counts[c] as f32;

        // Label by the most representative salient n-gram in the cluster's
        // notes: most FREQUENT first, then the LONGER phrase on a tie (a 2-3
        // word phrase is a more specific topic than a bare word that recurs
        // equally often), then alphabetical for a fully deterministic pick. No
        // notes → a generic "Group N" fallback.
        let grams = mine_ngrams(&cluster_notes);
        let label = grams
            .into_iter()
            .max_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| a.0.split(' ').count().cmp(&b.0.split(' ').count()))
                    .then_with(|| b.0.cmp(&a.0))
            })
            .map(|(phrase, _)| phrase)
            .unwrap_or_else(|| format!("Group {}", c + 1));

        out.push(ClusterTopic {
            label,
            size: counts[c],
            centroid_affinity,
        });
    }
    // Deterministic, useful order: biggest clusters first, label as tie-break.
    out.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.label.cmp(&b.label)));
    out
}

// ---------------------------------------------------------------------------
// suggest_topics_llm — v3 SEAM (LLM topic suggestion), scaffold only
// ---------------------------------------------------------------------------
//
// DESIGN §topics step 3: once Gemma is wired, extract N themes from the scope's
// notes/summaries as suggested topics. The LLM connector is spec'd but NOT yet
// wired (mocked in M1), so this is a SEAM, not a fake: it returns an explicit
// "LLM not available" state and the rail degrades to the cluster + n-gram
// suggestions meanwhile. The suggestion rail shows LLM suggestions ONLY when the
// connector becomes real.

/// The state of the v3 LLM-suggestion path. Until the Gemma connector lands this
/// is always `Unavailable` — an HONEST "not wired", never fabricated themes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LlmSuggestions {
    /// The LLM connector is not wired on this build/machine. The UI shows the
    /// cluster + n-gram suggestions instead and hides the LLM section.
    Unavailable { reason: String },
    /// The LLM extracted these theme phrases (only ever reached once the
    /// connector is real — see the TODO below).
    Ready { topics: Vec<String> },
}

/// The seam an LLM topic-suggester implements. Kept as a trait so the real Gemma
/// connector drops in behind it without touching the command surface (the same
/// shape the embedder/ASR seams use).
pub trait TopicLlm {
    /// Extract up to `max` theme phrases from the scope's note/summary prose.
    fn suggest(&self, note_texts: &[String], max: usize) -> LlmSuggestions;
}

/// `suggest_topics_llm` (DESIGN-SEMANTIC-GRAPH.md, v3 SEAM): the LLM-grounded
/// suggestion path. With NO connector (`None`) it returns the explicit
/// `Unavailable` state so the rail degrades cleanly to the cluster + n-gram
/// suggestions. This is a SCAFFOLD: it does not fabricate LLM output.
///
// TODO(v3): wire when the LLM connector lands (the Gemma connector is mocked in
// M1). Pass a real `TopicLlm` and the rail will surface its themes.
pub fn suggest_topics_llm<L: TopicLlm>(
    llm: Option<&L>,
    note_texts: &[String],
    max: usize,
) -> LlmSuggestions {
    match llm {
        None => LlmSuggestions::Unavailable {
            reason: "LLM connector not wired (mocked in M1); using cluster and note suggestions"
                .to_owned(),
        },
        Some(l) => l.suggest(note_texts, max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- blend (the looks-vs-said math, α = 0 / 1 / 0.5) ------------------

    #[test]
    fn blend_alpha_extremes_and_midpoint() {
        // α = 1 → pure visual; α = 0 → pure annotation; α = 0.5 → mean.
        assert_eq!(blend(Some(0.8), Some(0.2), 1.0), 0.8);
        assert_eq!(blend(Some(0.8), Some(0.2), 0.0), 0.2);
        assert!((blend(Some(0.8), Some(0.2), 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn blend_missing_half_contributes_zero() {
        // No visual vector at α = 0.5 → only half the annotation lands.
        assert!((blend(None, Some(0.6), 0.5) - 0.3).abs() < 1e-6);
        // No signal at all → exactly zero (the centered-image case).
        assert_eq!(blend(None, None, 0.5), 0.0);
    }

    #[test]
    fn blend_clamps_out_of_range_alpha() {
        // A caller passing α > 1 must not invert the annotation term.
        assert_eq!(blend(Some(0.9), Some(0.1), 5.0), 0.9); // clamps to 1.0
        assert_eq!(blend(Some(0.9), Some(0.1), -1.0), 0.1); // clamps to 0.0
    }

    // ---- topic_affinities over a degraded rig (no embedders) --------------

    /// The founder-binding graceful posture: with NO embedders the report is
    /// well-formed — every scope image present, every topic at affinity 0, both
    /// readiness flags false — and NEVER an error. This is the pre-embed-pass
    /// correctness DESIGN demands.
    #[test]
    fn affinities_degraded_rig_returns_zeros_never_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            PpvecStore::open(dir.path().join("photoproof.db"), dir.path().join("vectors")).unwrap();
        let scope = vec!["aa".repeat(32), "bb".repeat(32)];
        let topics = vec!["harbor at dusk".to_owned(), "snow".to_owned()];
        let report =
            topic_affinities::<NoEmbedder, NoEmbedder>(&scope, &topics, 0.5, &store, None, None);
        assert_eq!(report.images.len(), 2);
        for img in &report.images {
            assert_eq!(img.scores.len(), 2, "every topic listed even at zero");
            assert!(img.scores.iter().all(|s| s.affinity == 0.0));
            // Topic indices are stable 0..n.
            assert_eq!(img.scores[0].topic, 0);
            assert_eq!(img.scores[1].topic, 1);
        }
        assert!(!report.visual_ready);
        assert!(!report.annotation_ready);
    }

    /// An empty scope / empty topics is still a well-formed (empty/dense) report.
    #[test]
    fn affinities_empty_inputs_are_well_formed() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            PpvecStore::open(dir.path().join("photoproof.db"), dir.path().join("vectors")).unwrap();
        let none = topic_affinities::<NoEmbedder, NoEmbedder>(&[], &[], 0.5, &store, None, None);
        assert!(none.images.is_empty());
        // Topics present, scope empty → no rows.
        let no_imgs = topic_affinities::<NoEmbedder, NoEmbedder>(
            &[],
            &["x".to_owned()],
            0.5,
            &store,
            None,
            None,
        );
        assert!(no_imgs.images.is_empty());
    }

    // ---- suggest_topics (cheap candidates) --------------------------------

    #[test]
    fn suggestions_mine_recurring_ngrams_and_prepend_collections() {
        let notes = vec![
            "Quiet harbor at dusk, the fog rolling in".to_owned(),
            "harbor at dusk again, lovely fog".to_owned(),
            "the fog never lifted".to_owned(),
        ];
        let collections = vec![("Iceland Trip".to_owned(), 3usize)];
        let out = suggest_topics(&notes, &collections);
        // Collection name leads.
        assert_eq!(out[0].phrase, "Iceland Trip");
        assert_eq!(out[0].source, "collection");
        // "fog" recurs 3×, "harbor at dusk" 2× — both surface as note grams.
        let phrases: Vec<&str> = out.iter().map(|s| s.phrase.as_str()).collect();
        assert!(phrases.contains(&"fog"), "{phrases:?}");
        assert!(phrases.contains(&"harbor at dusk"), "{phrases:?}");
        // Stopword-edged grams and bare stopwords never appear.
        assert!(!phrases.iter().any(|p| *p == "the" || p.starts_with("the ")));
        // Every note suggestion cleared the recurrence floor.
        assert!(
            out.iter()
                .filter(|s| s.source == "note")
                .all(|s| s.count >= MIN_NGRAM_COUNT)
        );
    }

    #[test]
    fn suggestions_dedupe_collection_and_ngram_case_insensitively() {
        let notes = vec!["harbor scene".to_owned(), "harbor light".to_owned()];
        let collections = vec![("Harbor".to_owned(), 2usize)];
        let out = suggest_topics(&notes, &collections);
        let harbor_count = out
            .iter()
            .filter(|s| s.phrase.to_lowercase() == "harbor")
            .count();
        assert_eq!(
            harbor_count, 1,
            "collection and n-gram 'harbor' merge to one"
        );
    }

    #[test]
    fn suggestions_empty_inputs_are_empty() {
        assert!(suggest_topics(&[], &[]).is_empty());
        // A single-occurrence phrase is below the floor → nothing.
        assert!(suggest_topics(&["a lone thought".to_owned()], &[]).is_empty());
    }

    // ---- cluster_topics (v2) ----------------------------------------------

    /// A 2D point on the unit-ish plane, for planting separable clusters.
    fn pt(x: f32, y: f32) -> Vec<f32> {
        vec![x, y]
    }

    /// k-means converges DETERMINISTICALLY on planted clusters: two tight blobs
    /// far apart split cleanly, the same way every run (farthest-first seeding +
    /// fixed iteration order — no RNG). The exact assignment values are an
    /// implementation detail, but the PARTITION (which points group together)
    /// and reproducibility are the contract.
    #[test]
    fn kmeans_converges_deterministically_on_planted_clusters() {
        // Blob A near (0,0); blob B near (10,10).
        let points = vec![
            pt(0.0, 0.0),
            pt(0.1, -0.1),
            pt(-0.1, 0.05),
            pt(10.0, 10.0),
            pt(9.9, 10.1),
            pt(10.1, 9.95),
        ];
        let a = kmeans(&points, 2, 50);
        let b = kmeans(&points, 2, 50);
        // Deterministic: identical inputs → identical assignment.
        assert_eq!(a, b);
        // The three A-points share a label; the three B-points share the OTHER.
        assert_eq!(a[0], a[1]);
        assert_eq!(a[1], a[2]);
        assert_eq!(a[3], a[4]);
        assert_eq!(a[4], a[5]);
        assert_ne!(a[0], a[3], "the two blobs must not collapse into one label");
    }

    /// k >= n gives each point its own cluster (no panic, no empty-mean NaN).
    #[test]
    fn kmeans_handles_k_at_or_above_n() {
        let points = vec![pt(0.0, 0.0), pt(5.0, 5.0)];
        let a = kmeans(&points, 5, 50);
        assert_eq!(a.len(), 2);
        assert_ne!(a[0], a[1]);
        // Empty input is empty, never a panic.
        assert!(kmeans(&[], 3, 50).is_empty());
    }

    /// `pick_k` follows the sqrt(n/2) heuristic, clamped to [k_min, k_max].
    #[test]
    fn pick_k_follows_heuristic_within_bounds() {
        // sqrt(2/2)=1 → clamped up to k_min (2).
        assert_eq!(pick_k(2, 2, 12), 2);
        // sqrt(50/2)=5.
        assert_eq!(pick_k(50, 2, 12), 5);
        // sqrt(800/2)=20 → clamped down to k_max (12).
        assert_eq!(pick_k(800, 2, 12), 12);
    }

    /// cluster_topics labels each cluster by the NOTE PHRASE most representative
    /// of its members: two planted blobs, each blob's images mentioning a
    /// distinct theme, produce two clusters labeled by those themes.
    #[test]
    fn cluster_labeling_picks_the_right_note_phrase() {
        // Two blobs in DIFFERENT directions from the origin so cosine (angular)
        // separates them and each blob is internally tight (high centroid
        // affinity): blob A points roughly along +x, blob B along +y.
        let vectors = vec![
            ("a".to_owned(), pt(5.0, 0.0)),
            ("b".to_owned(), pt(5.0, 0.1)),
            ("c".to_owned(), pt(4.9, -0.1)),
            ("d".to_owned(), pt(0.0, 5.0)),
            ("e".to_owned(), pt(0.1, 5.0)),
            ("f".to_owned(), pt(-0.1, 4.9)),
        ];
        let mut notes: HashMap<String, Vec<String>> = HashMap::new();
        // Blob A images all talk about "harbor fog".
        for h in ["a", "b", "c"] {
            notes.insert(h.to_owned(), vec!["quiet harbor fog drifting".to_owned()]);
        }
        // Blob B images all talk about "snow ridge".
        for h in ["d", "e", "f"] {
            notes.insert(h.to_owned(), vec!["cold snow ridge at dawn".to_owned()]);
        }
        let clusters = cluster_topics(&vectors, &notes, Some(2), 2, 12);
        assert_eq!(clusters.len(), 2);
        let labels: Vec<&str> = clusters.iter().map(|c| c.label.as_str()).collect();
        // Each cluster is labeled by a phrase from ITS blob's notes (the most
        // frequent salient n-gram), and the two labels differ.
        assert!(
            labels.iter().any(|l| l.contains("harbor")),
            "labels: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("snow")),
            "labels: {labels:?}"
        );
        // Each cluster carried 3 images; the tight blobs score high affinity.
        for c in &clusters {
            assert_eq!(c.size, 3);
            assert!(
                c.centroid_affinity > 0.9,
                "tight blob: {}",
                c.centroid_affinity
            );
        }
    }

    /// A cluster whose images carry NO notes falls back to a generic label
    /// rather than vanishing or erroring.
    #[test]
    fn cluster_with_no_notes_gets_generic_label() {
        let vectors = vec![
            ("a".to_owned(), pt(1.0, 0.0)),
            ("b".to_owned(), pt(1.1, 0.0)),
        ];
        let notes: HashMap<String, Vec<String>> = HashMap::new();
        let clusters = cluster_topics(&vectors, &notes, Some(1), 2, 12);
        assert_eq!(clusters.len(), 1);
        assert!(
            clusters[0].label.starts_with("Group "),
            "{}",
            clusters[0].label
        );
    }

    /// Empty / un-embedded scope returns an empty rail, never an error (the
    /// graceful pre-embed-pass posture).
    #[test]
    fn cluster_empty_or_unembedded_returns_empty() {
        let notes: HashMap<String, Vec<String>> = HashMap::new();
        assert!(cluster_topics(&[], &notes, None, 2, 12).is_empty());
    }

    // ---- suggest_topics_llm (v3 seam) -------------------------------------

    /// The v3 seam degrades cleanly: with NO connector it returns the explicit
    /// Unavailable state (so the rail shows cluster + n-gram suggestions),
    /// NEVER fabricated themes.
    #[test]
    fn llm_seam_unavailable_without_connector() {
        // A never-constructed concrete L satisfies the generic.
        enum NoLlm {}
        impl TopicLlm for NoLlm {
            fn suggest(&self, _: &[String], _: usize) -> LlmSuggestions {
                match *self {}
            }
        }
        let out = suggest_topics_llm::<NoLlm>(None, &["some notes".to_owned()], 5);
        match out {
            LlmSuggestions::Unavailable { reason } => {
                assert!(reason.to_lowercase().contains("not wired"));
            }
            LlmSuggestions::Ready { .. } => panic!("must NOT fabricate LLM output"),
        }
    }

    /// Uninhabited embedder for the degraded-rig tests: the topic_affinities
    /// generic needs a concrete `TE`/`CE` even when both slots are `None`.
    /// Mirrors `search::hybrid::NoModel`.
    enum NoEmbedder {}

    impl Embedder for NoEmbedder {
        async fn embed_text(
            &self,
            _text: &str,
        ) -> photoproof_connectors::ConnectorResult<Embedding> {
            match *self {}
        }
        async fn embed_image(
            &self,
            _img: &photoproof_connectors::embedder::DecodedImage,
        ) -> photoproof_connectors::ConnectorResult<Embedding> {
            match *self {}
        }
        fn dimensions(&self) -> usize {
            match *self {}
        }
        fn model_id(&self) -> &str {
            match *self {}
        }
    }
}
