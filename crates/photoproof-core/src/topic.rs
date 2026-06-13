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
// topic_ranked_images — the single-topic ranked grid (Topics tab + the slider)
// ---------------------------------------------------------------------------

/// One in-scope image's blended affinity to ONE topic phrase, for the Topics
/// tab's ranked grid and the threshold slider. This is the single-topic
/// projection of [`AffinityReport`]: the tab selects a topic, this scores the
/// scope against just that phrase, and the grid renders the result descending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedImage {
    pub hash: String,
    /// Blended affinity `α·visual + (1−α)·annotation` (a cosine, roughly
    /// [-1, 1]). The slider thresholds on this value; the bake keeps images at
    /// or above the chosen threshold.
    pub score: f32,
}

/// `topic_ranked_images` (DESIGN-TOPICS-COLLECTIONS.md): the in-scope images
/// ranked by blended affinity to ONE topic phrase, descending. The Topics tab
/// selects a topic and shows THIS list in the grid; the slider thresholds on
/// `score`.
///
/// REUSES [`topic_affinities`] wholesale with a single-element topics slice
/// (one embed + one space scan per space, the cheap shape) — the topic is just
/// a different reference vector, exactly as the graph's multi-topic scoring is.
/// No second similarity definition.
///
/// GRACEFUL like the rest of the lens: an un-embedded scope, absent embedders,
/// or an empty scope all yield a well-formed (possibly all-zero, possibly
/// empty) ranked list, never an error. Ordering is STABLE: descending score,
/// then ascending hash as a deterministic tie-break so the grid never reshuffles
/// equal-score images between runs.
pub fn topic_ranked_images<TE: Embedder, CE: Embedder>(
    scope: &[String],
    phrase: &str,
    alpha: f64,
    vectors: &PpvecStore,
    text: Option<&TE>,
    clip: Option<&CE>,
) -> Vec<RankedImage> {
    // One topic → one column of the dense report; pull that column out as the
    // per-image score. (A single-element slice keeps the per-topic-not-per-image
    // cost shape — one embed + one scan, not one per image.)
    let report = topic_affinities(
        scope,
        std::slice::from_ref(&phrase.to_owned()),
        alpha,
        vectors,
        text,
        clip,
    );
    let mut ranked: Vec<RankedImage> = report
        .images
        .into_iter()
        .map(|img| RankedImage {
            // Exactly one topic was scored, so `scores[0]` is this phrase's
            // affinity for the image (the dense report always lists every topic).
            score: img.scores.first().map(|s| s.affinity).unwrap_or(0.0),
            hash: img.image_hash,
        })
        .collect();
    // Descending score; ascending hash tie-break for a fully deterministic order
    // (NaN cannot arise — score_images yields finite cosines — but total_cmp is
    // total regardless, so the sort is panic-free even on a degenerate value).
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.hash.cmp(&b.hash))
    });
    ranked
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
// suggest_collections — Phase 3 candidate GROUPINGS (DESIGN-TOPICS-COLLECTIONS.md)
// ---------------------------------------------------------------------------
//
// Three EXTRA candidate signals beyond the v2 cluster topics, all computed on
// the fly from data the app already has (annotation_events / sessions /
// event_targets / images.capture_ts / paths / image_journal_stats):
//
//   1. CO-ANNOTATION IN A SESSION — images annotated within the same session
//      co-occur; a session that touched a coherent set of photos is a candidate
//      grouping ("worked together <date>").
//   2. REPEATED PHRASES ACROSS NOTES — a salient note n-gram that recurs across
//      multiple images: the set of images whose notes share it is a candidate
//      ("images you called '<phrase>'"). Reuses the v2 `mine_ngrams` miner.
//   3. TIME + FOLDER BURSTS — images close in CAPTURE TIME within one folder (a
//      shoot): cluster by (folder, time gap) into bursts; a big burst is a
//      candidate ("burst, <folder>, <date>").
//
// K14: this PROPOSES, it never auto-creates. Quiet by construction — pure
// reducers over rows the command gathers, never a write. Empty/sparse libraries
// yield few/none, never an error. The caps below keep the rail short and the
// member lists sane; like the `suggest_topics` caps they shape a RAIL, not a
// ranking, so they are plain consts (deliberately not tuning.toml knobs).

/// At most this many candidates total across all three sources (the rail stays
/// short — the human skims and commits, they do not wade).
const MAX_COLLECTION_CANDIDATES: usize = 24;
/// Cap each candidate's member list: a grouping the human reviews before baking
/// should be glanceable, not a thousand-image dump. (A real shoot can exceed
/// this; the candidate is a STARTING POINT the human refines, not a final set.)
const MAX_CANDIDATE_MEMBERS: usize = 500;
/// A session must have touched at least this many DISTINCT images to be a
/// co-annotation candidate — one or two images is not a "set worked together".
const MIN_SESSION_IMAGES: usize = 3;
/// A note n-gram must span at least this many DISTINCT images to be a
/// repeated-phrase candidate — a phrase on a single image is not a grouping
/// (this is the cross-IMAGE recurrence floor, distinct from `suggest_topics`'
/// cross-NOTE one).
const MIN_PHRASE_IMAGES: usize = 3;
/// A capture-time burst must hold at least this many images to be a candidate —
/// a lone frame or a pair is not a "shoot".
const MIN_BURST_IMAGES: usize = 4;
/// The capture-time gap (ms) that splits one burst from the next within a
/// folder: > 30 minutes between consecutive frames starts a new burst. 30 min
/// mirrors the session idle boundary (CAPTURE §2.2) — the same "the
/// photographer stepped away" intuition, applied to capture time.
const BURST_GAP_MS: i64 = 30 * 60 * 1000;

/// A proposed candidate GROUPING the human might bake into a collection
/// (DESIGN-TOPICS-COLLECTIONS.md, autosuggest Phase 3). NOT a collection: it is
/// a suggestion the human commits via the existing bake (K14: the machine
/// proposes, the human authors). Computed on the fly; never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionCandidate {
    /// A human-readable name for the proposed grouping (no em-dashes — UI copy).
    pub label: String,
    /// The candidate member image hashes (lowercase hex), capped at
    /// `MAX_CANDIDATE_MEMBERS`. Sorted ascending for a deterministic order.
    pub members: Vec<String>,
    /// Which signal proposed this: `"co_annotation"` | `"repeated_phrase"` |
    /// `"time_folder"`. The rail can style each source differently.
    pub source: String,
    /// A simple coherence/size signal for ranking the rail (bigger / tighter
    /// groupings first). Not a probability — just a sortable strength.
    pub score: f64,
}

/// One annotation event's `(session_id, image_hash)` link, the raw input for the
/// co-annotation candidate source. The command gathers these from the journal
/// spine (live remark/rating/stroke/revision events joined to their targets).
pub type SessionImageLink = (String, String);

/// Co-annotation candidates: a session that touched >= `MIN_SESSION_IMAGES`
/// DISTINCT images is a candidate grouping (the photographer worked that set
/// together). `links` is `(session_id, image_hash)` for every live event/target
/// in scope; `session_started` maps a session id to its start timestamp (for the
/// "<date>" in the label) — a session absent from the map just gets no date.
///
/// PURE over its inputs (no DB) so it is unit-testable with planted links. The
/// score is the distinct-image count (bigger sessions rank higher).
fn co_annotation_candidates(
    links: &[SessionImageLink],
    session_started: &HashMap<String, i64>,
) -> Vec<CollectionCandidate> {
    // Group distinct images per session. A BTreeSet keeps members sorted +
    // deduped (a session can touch the same image across many events).
    let mut by_session: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    for (session, hash) in links {
        by_session
            .entry(session.clone())
            .or_default()
            .insert(hash.clone());
    }
    let mut out = Vec::new();
    for (session, images) in by_session {
        if images.len() < MIN_SESSION_IMAGES {
            continue;
        }
        let date = session_started
            .get(&session)
            .map(|ms| date_label(*ms))
            .unwrap_or_default();
        // Label without an em-dash (UI copy rule). With a known date: "worked
        // together 2026-06-13"; without: just "worked together".
        let label = if date.is_empty() {
            "worked together".to_owned()
        } else {
            format!("worked together {date}")
        };
        let score = images.len() as f64;
        out.push(make_candidate(label, images, "co_annotation", score));
    }
    out
}

/// One image's `(image_hash, note_text)` row, the raw input for the
/// repeated-phrase source. The command gathers these BY image (a multi-target
/// remark applies to each image it targets, so it is counted per image here, not
/// de-duped by event — exactly `scope_note_texts_by_hash`'s grouping).
///
/// Repeated-phrase candidates: a salient note n-gram (the v2 `mine_ngrams`
/// miner) that recurs across >= `MIN_PHRASE_IMAGES` DISTINCT images becomes a
/// candidate whose members are those images ("images you called '<phrase>'").
/// The score is the distinct-image span. Only the STRONGEST phrase per image set
/// is kept when several phrases pick out the same images, to avoid near-duplicate
/// rail chips.
fn repeated_phrase_candidates(
    notes_by_hash: &HashMap<String, Vec<String>>,
) -> Vec<CollectionCandidate> {
    // For each salient n-gram, the set of images whose notes contain it. Mine
    // each image's notes once (the same miner the v2 labels use) and union the
    // resulting phrases into per-phrase image sets.
    let mut images_by_phrase: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    for (hash, texts) in notes_by_hash {
        // mine_ngrams returns phrase->count within THIS image's notes; we only
        // care that the phrase appears at all for this image (cross-image
        // recurrence is the signal, not within-image repetition).
        for phrase in mine_ngrams(texts).into_keys() {
            images_by_phrase
                .entry(phrase)
                .or_default()
                .insert(hash.clone());
        }
    }
    // Keep phrases spanning enough distinct images, strongest (widest span)
    // first; a longer phrase breaks a span tie (more specific theme), then the
    // phrase text for determinism.
    let mut kept: Vec<(String, std::collections::BTreeSet<String>)> = images_by_phrase
        .into_iter()
        .filter(|(_, imgs)| imgs.len() >= MIN_PHRASE_IMAGES)
        .collect();
    kept.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| b.0.split(' ').count().cmp(&a.0.split(' ').count()))
            .then_with(|| a.0.cmp(&b.0))
    });

    // Drop a phrase whose member set is identical to one already emitted (two
    // phrases picking out the SAME images are one grouping; keep the strongest,
    // which sorts first). Cheap O(n^2) over the short kept list.
    let mut out: Vec<CollectionCandidate> = Vec::new();
    let mut emitted_sets: Vec<std::collections::BTreeSet<String>> = Vec::new();
    for (phrase, imgs) in kept {
        if emitted_sets.contains(&imgs) {
            continue;
        }
        let score = imgs.len() as f64;
        let label = format!("images you called \"{phrase}\"");
        emitted_sets.push(imgs.clone());
        out.push(make_candidate(label, imgs, "repeated_phrase", score));
    }
    out
}

/// One image's `(folder, capture_ms, image_hash)` row, the raw input for the
/// time+folder burst source. `folder` is a stable per-folder key the command
/// builds (root id + the file's parent dir), `capture_ms` is the EXIF capture
/// time in epoch millis (images with no capture time are omitted by the command).
pub type FolderTimeRow = (String, i64, String);

/// Time + folder burst candidates: within ONE folder, images whose consecutive
/// capture times are within `BURST_GAP_MS` form a burst (a shoot); a burst of at
/// least `MIN_BURST_IMAGES` is a candidate ("burst, folder, date"). A gap larger
/// than the threshold, or a different folder, SPLITS the burst.
///
/// PURE: sort each folder's rows by capture time, then walk splitting on the
/// gap. The score is the burst size. The `<folder>` shown is the row's folder
/// key's last path segment (a readable name, not the full key).
fn time_folder_candidates(rows: &[FolderTimeRow]) -> Vec<CollectionCandidate> {
    // Bucket by folder, then sort each bucket by capture time (then hash for a
    // deterministic order on identical timestamps — burst-mate frames often
    // share a second).
    let mut by_folder: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    for (folder, ms, hash) in rows {
        by_folder
            .entry(folder.clone())
            .or_default()
            .push((*ms, hash.clone()));
    }
    let mut out = Vec::new();
    // Deterministic folder order so the rail is stable across runs.
    let mut folders: Vec<String> = by_folder.keys().cloned().collect();
    folders.sort();
    for folder in folders {
        let mut frames = by_folder.remove(&folder).unwrap_or_default();
        frames.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        // Walk consecutive frames, breaking a burst when the gap exceeds the
        // threshold. Each closed burst >= the floor becomes a candidate.
        let mut burst: Vec<(i64, String)> = Vec::new();
        let folder_name = folder.rsplit('/').next().unwrap_or(&folder).to_owned();
        let flush = |burst: &mut Vec<(i64, String)>, out: &mut Vec<CollectionCandidate>| {
            if burst.len() >= MIN_BURST_IMAGES {
                let start_ms = burst.first().map(|(ms, _)| *ms).unwrap_or_default();
                let date = date_label(start_ms);
                let members: std::collections::BTreeSet<String> =
                    burst.iter().map(|(_, h)| h.clone()).collect();
                let score = members.len() as f64;
                let label = if date.is_empty() {
                    format!("burst, {folder_name}")
                } else {
                    format!("burst, {folder_name}, {date}")
                };
                out.push(make_candidate(label, members, "time_folder", score));
            }
            burst.clear();
        };
        for frame in frames {
            // A gap past the threshold from the previous frame closes the burst.
            if let Some((prev_ms, _)) = burst.last()
                && frame.0 - *prev_ms > BURST_GAP_MS
            {
                flush(&mut burst, &mut out);
            }
            burst.push(frame);
        }
        flush(&mut burst, &mut out);
    }
    out
}

/// Build a capped, deterministically-ordered candidate from a label, a sorted
/// member set, a source tag, and a score. Shared by all three sources so the
/// member cap + ordering are applied ONE way.
fn make_candidate(
    label: String,
    members: std::collections::BTreeSet<String>,
    source: &str,
    score: f64,
) -> CollectionCandidate {
    // BTreeSet already yields ascending order; the cap keeps the list glanceable
    // (the human refines a starting point, not a final set).
    let members: Vec<String> = members.into_iter().take(MAX_CANDIDATE_MEMBERS).collect();
    CollectionCandidate {
        label,
        members,
        source: source.to_owned(),
        score,
    }
}

/// The `YYYY-MM-DD` date portion of an epoch-ms instant, for candidate labels.
/// Empty string for a degenerate/zero instant (so the label drops the date
/// rather than printing a misleading epoch date).
fn date_label(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    // Reuse the canonical RFC3339 formatter and slice the date (first 10 bytes:
    // "YYYY-MM-DD"), so the label's date matches the rest of the app's clock.
    crate::UtcMillis::from_epoch_ms(ms)
        .to_rfc3339()
        .get(..10)
        .unwrap_or_default()
        .to_owned()
}

/// `suggest_collections` (DESIGN-TOPICS-COLLECTIONS.md, autosuggest Phase 3):
/// merge the three candidate sources into one ranked, capped rail. PURE over the
/// gathered rows (the desktop command does the DB reads + scope enumeration and
/// hands the rows in), so the whole proposal logic is unit-testable without a DB.
///
/// Ranking: by descending score (bigger / tighter groupings first), then source
/// then label for a deterministic order. Capped at `MAX_COLLECTION_CANDIDATES`.
/// Empty/sparse inputs yield few/none, never an error (K14 quiet posture).
pub fn suggest_collections(
    session_links: &[SessionImageLink],
    session_started: &HashMap<String, i64>,
    notes_by_hash: &HashMap<String, Vec<String>>,
    folder_time_rows: &[FolderTimeRow],
) -> Vec<CollectionCandidate> {
    let mut out = co_annotation_candidates(session_links, session_started);
    out.extend(repeated_phrase_candidates(notes_by_hash));
    out.extend(time_folder_candidates(folder_time_rows));
    // Strongest first, then a stable tie-break (source, then label) so the rail
    // order never reshuffles between identical runs.
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.label.cmp(&b.label))
    });
    out.truncate(MAX_COLLECTION_CANDIDATES);
    out
}

/// Pull `(session_id, image_hash)` for every LIVE event targeting any image in
/// `scope`, for the co-annotation candidate source. Unlike `scope_note_texts`
/// this counts ALL live event kinds (a rating or a stroke "touches" an image in
/// a session just as a remark does — the session worked that image), de-duped to
/// distinct `(session, image)` pairs. `redacted_by IS NULL` drops scrubbed
/// events. An empty scope yields an empty vec, never an error.
pub fn scope_session_image_links(
    conn: &Connection,
    scope: &[String],
) -> rusqlite::Result<Vec<SessionImageLink>> {
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; scope.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT e.session_id, t.image_hash
         FROM annotation_events e
         JOIN event_targets t ON t.event_id = e.id
         WHERE e.redacted_by IS NULL
           AND t.image_hash IN ({marks})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(scope.iter());
    let rows = stmt.query_map(params, |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// Map every session id that appears in `links` to its start instant (epoch ms),
/// for the co-annotation candidate labels' "<date>". Sessions whose `started_ts`
/// is absent/unparseable are simply omitted (the candidate then carries no date).
pub fn session_start_millis(
    conn: &Connection,
    links: &[SessionImageLink],
) -> rusqlite::Result<HashMap<String, i64>> {
    if links.is_empty() {
        return Ok(HashMap::new());
    }
    // Distinct session ids from the links (the set we need start times for).
    let ids: std::collections::BTreeSet<&str> = links.iter().map(|(s, _)| s.as_str()).collect();
    let ids: Vec<&str> = ids.into_iter().collect();
    let marks = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT id, started_ts FROM sessions WHERE id IN ({marks})");
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(ids.iter());
    let rows = stmt.query_map(params, |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, started) = row?;
        // started_ts is the canonical RFC3339; a parse failure just drops the
        // date (the candidate is still valid, it just has no "<date>" label).
        if let Ok(ms) = crate::UtcMillis::parse(&started) {
            out.insert(id, ms.epoch_ms());
        }
    }
    Ok(out)
}

/// Pull `(folder_key, capture_ms, image_hash)` for the in-scope images that have
/// BOTH an active path (a folder) AND a capture timestamp, for the time+folder
/// burst source. The folder key is `root_id` + the active path's PARENT
/// directory (so two images in the same shoot directory share a key; the
/// camera-DCIM-collision concern that GridItem.root_id guards against is handled
/// by keying on root_id too). Images with no capture time or no active path are
/// omitted (they cannot anchor a capture-time burst). Empty scope ⇒ empty vec.
pub fn scope_folder_time_rows(
    conn: &Connection,
    scope: &[String],
) -> rusqlite::Result<Vec<FolderTimeRow>> {
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; scope.len()].join(",");
    // Join images (capture_ts) to its active path (root_id + rel_path). One row
    // per image: a multi-path image picks its first active path (MIN(rel_path))
    // so the folder key is deterministic. capture_ts NOT NULL filters undated
    // images out of the burst math.
    let sql = format!(
        "SELECT i.image_hash, i.capture_ts,
                COALESCE(p.root_id, '') AS root_id, p.rel_path
         FROM images i
         JOIN paths p ON p.image_hash = i.image_hash AND p.state = 'active'
         WHERE i.capture_ts IS NOT NULL
           AND i.image_hash IN ({marks})
           AND p.rel_path = (
             SELECT MIN(p2.rel_path) FROM paths p2
             WHERE p2.image_hash = i.image_hash AND p2.state = 'active'
           )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(scope.iter());
    let rows = stmt.query_map(params, |r| {
        Ok((
            r.get::<_, String>(0)?, // image_hash
            r.get::<_, String>(1)?, // capture_ts (RFC3339)
            r.get::<_, String>(2)?, // root_id ('' if none)
            r.get::<_, String>(3)?, // rel_path
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (hash, capture_ts, root_id, rel_path) = row?;
        // A capture_ts that does not parse is treated as undated (skip it — it
        // cannot sit in a time-ordered burst).
        let Ok(ms) = crate::UtcMillis::parse(&capture_ts) else {
            continue;
        };
        // Folder key = root_id + the file's PARENT directory. The parent is
        // rel_path with its last '/'-segment (the filename) stripped; a file at
        // the root has an empty parent. Keying on root_id keeps identical camera
        // paths under different roots distinct (the GridItem.root_id concern).
        let parent = match rel_path.rsplit_once('/') {
            Some((dir, _file)) => dir,
            None => "",
        };
        let folder_key = format!("{root_id}/{parent}");
        out.push((folder_key, ms.epoch_ms(), hash));
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

    // ---- topic_ranked_images (the single-topic ranked grid) ---------------

    /// A fixed-direction embedder for the ranked-grid tests: every phrase maps
    /// to the SAME unit vector (the topic anchor sits at a known direction), so
    /// a planted image's affinity is purely a function of where IT sits. Mirrors
    /// the `NoEmbedder` test-double shape but actually returns a vector.
    struct FixedEmbedder {
        vector: Vec<f32>,
        model: String,
    }

    impl Embedder for FixedEmbedder {
        async fn embed_text(
            &self,
            _text: &str,
        ) -> photoproof_connectors::ConnectorResult<Embedding> {
            Ok(Embedding {
                vector: self.vector.clone(),
                model_id: self.model.clone(),
            })
        }
        async fn embed_image(
            &self,
            _img: &photoproof_connectors::embedder::DecodedImage,
        ) -> photoproof_connectors::ConnectorResult<Embedding> {
            unreachable!("ranked-grid tests embed text only")
        }
        fn dimensions(&self) -> usize {
            self.vector.len()
        }
        fn model_id(&self) -> &str {
            &self.model
        }
    }

    /// Plant an image_clip vector for `hash` in `store` under `model`.
    fn plant_clip(store: &PpvecStore, hash: &str, model: &str, vector: Vec<f32>) {
        use photoproof_connectors::vector_store::{VecKey, VecSpace, VecUnit, VectorStore};
        store
            .upsert(
                VecKey {
                    space: VecSpace {
                        vec_kind: VecKind::ImageClip,
                        model_id: model.to_owned(),
                    },
                    unit: VecUnit::Image {
                        image_hash: hash.to_owned(),
                    },
                },
                &Embedding {
                    vector,
                    model_id: model.to_owned(),
                },
            )
            .expect("plant clip vector");
    }

    /// `topic_ranked_images` ranks the scope by blended affinity descending,
    /// over PLANTED clip vectors: the topic anchor points along +x, and three
    /// images sit at decreasing cosine to it, so the ranked order is exactly
    /// that decreasing order. α = 1 isolates the visual half (the only space
    /// planted here), making the assertion about ranking, not blending.
    #[test]
    fn ranked_images_orders_by_descending_affinity() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            PpvecStore::open(dir.path().join("photoproof.db"), dir.path().join("vectors")).unwrap();
        let model = "clip-test";
        // Topic anchor along +x; images at angles 0° < 45° < 90° from it, so
        // cos affinity is 1 > ~0.707 > 0 — a strict ranking the sort must honor.
        plant_clip(&store, &"a".repeat(64), model, vec![1.0, 0.0]); // cos 1.0
        plant_clip(&store, &"b".repeat(64), model, vec![1.0, 1.0]); // cos ~.707
        plant_clip(&store, &"c".repeat(64), model, vec![0.0, 1.0]); // cos 0.0
        let scope = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];
        let clip = FixedEmbedder {
            vector: vec![1.0, 0.0],
            model: model.to_owned(),
        };
        // α = 1 → pure visual; no text embedder (annotation half is empty).
        let ranked = topic_ranked_images::<NoEmbedder, FixedEmbedder>(
            &scope,
            "anything",
            1.0,
            &store,
            None,
            Some(&clip),
        );
        let order: Vec<&str> = ranked.iter().map(|r| r.hash.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "a".repeat(64).as_str(),
                "b".repeat(64).as_str(),
                "c".repeat(64).as_str()
            ],
            "descending affinity: a (cos1) > b (cos.707) > c (cos0)"
        );
        // Scores are descending and finite.
        assert!(ranked[0].score > ranked[1].score);
        assert!(ranked[1].score > ranked[2].score);

        // THRESHOLD semantics (what the bake commits): the images at or above a
        // threshold are a prefix of this descending list. A 0.5 cut keeps a and
        // b (cos 1, .707) and drops c (cos 0) — the >= semantics the bake uses.
        let kept: Vec<&str> = ranked
            .iter()
            .filter(|r| r.score >= 0.5)
            .map(|r| r.hash.as_str())
            .collect();
        assert_eq!(kept, vec!["a".repeat(64).as_str(), "b".repeat(64).as_str()]);
    }

    /// Graceful: an un-embedded scope (vectors planted under a DIFFERENT model
    /// than the embedder reports) and absent embedders both yield a well-formed
    /// ranked list (every scope image at score 0), never an error. The order is
    /// still deterministic (ascending hash on the all-equal scores).
    #[test]
    fn ranked_images_unembedded_and_degraded_are_graceful() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            PpvecStore::open(dir.path().join("photoproof.db"), dir.path().join("vectors")).unwrap();
        let scope = vec!["b".repeat(64), "a".repeat(64)];
        // No embedders at all → every image scores 0, but the list is dense and
        // deterministically ordered (ascending hash).
        let ranked =
            topic_ranked_images::<NoEmbedder, NoEmbedder>(&scope, "x", 0.5, &store, None, None);
        assert_eq!(ranked.len(), 2);
        assert!(ranked.iter().all(|r| r.score == 0.0));
        assert_eq!(ranked[0].hash, "a".repeat(64), "tie-break ascending hash");
        // Empty scope → empty list, never an error.
        assert!(
            topic_ranked_images::<NoEmbedder, NoEmbedder>(&[], "x", 0.5, &store, None, None)
                .is_empty()
        );
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

    // ---- suggest_collections (Phase 3 candidate groupings) ----------------

    fn h(seed: &str) -> String {
        // A 64-hex content hash from a short seed (the candidate sources only
        // care about distinctness + ordering, not the bytes).
        seed.repeat(64 / seed.len().max(1))[..64].to_owned()
    }

    /// Co-annotation: a session that touched >= MIN_SESSION_IMAGES distinct
    /// images is a candidate; a smaller session is not. Members are exactly that
    /// session's distinct images; the label carries the start date.
    #[test]
    fn co_annotation_groups_a_multi_image_session() {
        let (s_big, s_small) = ("s_big", "s_small");
        let links = vec![
            (s_big.to_owned(), h("a")),
            (s_big.to_owned(), h("b")),
            (s_big.to_owned(), h("c")),
            // A repeated (session, image) pair must count once (distinct images).
            (s_big.to_owned(), h("c")),
            // A two-image session is below the floor → no candidate.
            (s_small.to_owned(), h("d")),
            (s_small.to_owned(), h("e")),
        ];
        let mut started = HashMap::new();
        // 2026-06-13T00:00:00.000Z in epoch ms.
        let ms = crate::UtcMillis::parse("2026-06-13T08:00:00.000Z")
            .unwrap()
            .epoch_ms();
        started.insert(s_big.to_owned(), ms);
        let cands = co_annotation_candidates(&links, &started);
        assert_eq!(cands.len(), 1, "only the >=3-image session is a candidate");
        let c = &cands[0];
        assert_eq!(c.source, "co_annotation");
        assert_eq!(c.members, vec![h("a"), h("b"), h("c")]);
        assert_eq!(c.score, 3.0);
        assert!(c.label.contains("2026-06-13"), "label: {}", c.label);
        assert!(!c.label.contains('—'), "no em-dash in UI copy: {}", c.label);
    }

    /// A session with no start time still yields a (dateless) candidate, never a
    /// misleading epoch date.
    #[test]
    fn co_annotation_dateless_session_has_no_date_in_label() {
        let links = vec![
            ("s".to_owned(), h("a")),
            ("s".to_owned(), h("b")),
            ("s".to_owned(), h("c")),
        ];
        let cands = co_annotation_candidates(&links, &HashMap::new());
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].label, "worked together");
    }

    /// Repeated phrase: an n-gram spanning >= MIN_PHRASE_IMAGES distinct images
    /// is a candidate whose members are those images; a phrase on too few images
    /// is dropped; identical-member phrases collapse to the strongest.
    #[test]
    fn repeated_phrase_groups_images_sharing_an_ngram() {
        let mut notes: HashMap<String, Vec<String>> = HashMap::new();
        // "the fog series" wording on three images → a candidate.
        notes.insert(h("a"), vec!["quiet fog series at dawn".to_owned()]);
        notes.insert(h("b"), vec!["more fog series light".to_owned()]);
        notes.insert(h("c"), vec!["fog series again".to_owned()]);
        // A phrase on a single image is below the cross-image floor.
        notes.insert(h("d"), vec!["a lonely tugboat".to_owned()]);
        let cands = repeated_phrase_candidates(&notes);
        // The "fog series" 2-gram (and "fog"/"series" 1-grams) all span a,b,c —
        // the same member set, so they collapse to ONE candidate.
        assert!(!cands.is_empty());
        let fog = cands
            .iter()
            .find(|c| c.label.contains("fog series"))
            .or_else(|| cands.first())
            .expect("a fog candidate");
        assert_eq!(fog.source, "repeated_phrase");
        assert_eq!(fog.members, vec![h("a"), h("b"), h("c")]);
        assert_eq!(fog.score, 3.0);
        // No member set with fewer than the floor's images escaped.
        assert!(cands.iter().all(|c| c.members.len() >= MIN_PHRASE_IMAGES));
        // Identical-member phrases collapsed (no two candidates share a set).
        for i in 0..cands.len() {
            for j in (i + 1)..cands.len() {
                assert_ne!(
                    cands[i].members, cands[j].members,
                    "identical member sets must collapse"
                );
            }
        }
    }

    /// Time + folder: frames close in time within one folder form a burst; a big
    /// time gap splits a burst; a different folder splits it; sub-floor bursts
    /// are dropped.
    #[test]
    fn time_folder_groups_a_burst_and_splits_on_gap_and_folder() {
        let base = crate::UtcMillis::parse("2026-06-13T08:00:00.000Z")
            .unwrap()
            .epoch_ms();
        let min = 60_000i64;
        // Folder F1: a 5-frame burst (1 min apart), then a >30min gap, then a
        // 2-frame tail (below the burst floor → not its own candidate).
        let mut rows: Vec<FolderTimeRow> = Vec::new();
        for (i, seed) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            rows.push(("F1".to_owned(), base + (i as i64) * min, h(seed)));
        }
        // 40-minute gap, then two more frames in F1 → a separate, too-small burst.
        rows.push(("F1".to_owned(), base + 45 * min, h("f")));
        rows.push(("F1".to_owned(), base + 46 * min, h("g")));
        // Folder F2: a 4-frame burst at the same times as F1's first burst —
        // SAME times, DIFFERENT folder must be its own candidate, not merged.
        for (i, seed) in ["p", "q", "r", "s"].iter().enumerate() {
            rows.push(("root2/F2".to_owned(), base + (i as i64) * min, h(seed)));
        }
        let cands = time_folder_candidates(&rows);
        // Two candidates: F1's 5-frame burst + F2's 4-frame burst. F1's 2-frame
        // tail is below MIN_BURST_IMAGES and the F2 set never merges with F1.
        assert_eq!(cands.len(), 2, "got: {:?}", cands);
        let f1 = cands
            .iter()
            .find(|c| c.members.contains(&h("a")))
            .expect("F1 burst");
        assert_eq!(f1.source, "time_folder");
        assert_eq!(f1.members.len(), 5, "the 5-frame burst, gap-split tail out");
        assert!(!f1.members.contains(&h("f")), "post-gap frame excluded");
        assert!(f1.label.starts_with("burst, F1"), "label: {}", f1.label);
        assert!(f1.label.contains("2026-06-13"));
        let f2 = cands
            .iter()
            .find(|c| c.members.contains(&h("p")))
            .expect("F2 burst");
        assert_eq!(
            f2.members.len(),
            4,
            "F2 is its own burst, not merged into F1"
        );
    }

    /// The whole rail: merged, ranked by descending score, capped, and an empty
    /// library yields an empty rail (never an error).
    #[test]
    fn suggest_collections_merges_ranks_and_caps() {
        // One co-annotation (score 4), one repeated-phrase (score 3), one burst
        // (score 5) → the burst ranks first.
        let session_links = vec![
            ("s".to_owned(), h("a")),
            ("s".to_owned(), h("b")),
            ("s".to_owned(), h("c")),
            ("s".to_owned(), h("d")),
        ];
        let mut notes: HashMap<String, Vec<String>> = HashMap::new();
        for seed in ["m", "n", "o"] {
            notes.insert(h(seed), vec!["the harbor mist again".to_owned()]);
        }
        let base = crate::UtcMillis::parse("2026-06-13T08:00:00.000Z")
            .unwrap()
            .epoch_ms();
        let mut rows: Vec<FolderTimeRow> = Vec::new();
        for (i, seed) in ["u", "v", "w", "x", "y"].iter().enumerate() {
            rows.push(("F".to_owned(), base + (i as i64) * 60_000, h(seed)));
        }
        let cands = suggest_collections(&session_links, &HashMap::new(), &notes, &rows);
        assert_eq!(cands.len(), 3);
        // Descending score: burst (5) > co-annotation (4) > phrase (3).
        assert_eq!(cands[0].source, "time_folder");
        assert_eq!(cands[0].score, 5.0);
        assert_eq!(cands[1].source, "co_annotation");
        assert_eq!(cands[2].source, "repeated_phrase");

        // Empty inputs → an empty rail, never an error.
        let empty = suggest_collections(&[], &HashMap::new(), &HashMap::new(), &[]);
        assert!(empty.is_empty());
    }

    /// The member cap holds: a burst larger than MAX_CANDIDATE_MEMBERS is
    /// truncated, the rest of the candidate intact.
    #[test]
    fn candidate_member_cap_holds() {
        let base = crate::UtcMillis::parse("2026-06-13T08:00:00.000Z")
            .unwrap()
            .epoch_ms();
        // MAX_CANDIDATE_MEMBERS + 50 frames 1 second apart (all one burst).
        let n = MAX_CANDIDATE_MEMBERS + 50;
        let mut rows: Vec<FolderTimeRow> = Vec::with_capacity(n);
        for i in 0..n {
            // Distinct 64-hex hashes from the index.
            let hash = format!("{i:064x}");
            rows.push(("F".to_owned(), base + (i as i64) * 1000, hash));
        }
        let cands = time_folder_candidates(&rows);
        assert_eq!(cands.len(), 1);
        assert_eq!(
            cands[0].members.len(),
            MAX_CANDIDATE_MEMBERS,
            "member list capped"
        );
        // The score still reflects the FULL burst size (the cap trims the member
        // list shown, not the strength signal).
        assert_eq!(cands[0].score, n as f64);
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
