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

    // Mine recurring n-grams from the notes.
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

    let mut grams: Vec<(String, usize)> = counts
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
