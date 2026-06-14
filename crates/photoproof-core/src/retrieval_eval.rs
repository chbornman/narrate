//! Retrieval-quality evaluation — the IR metrics half of the M3
//! "Golden-query retrieval eval" gate (BACKLOG, founder). This module is the
//! INSTRUMENT, not the policy: it scores a ranked result list against a set
//! of known-relevant ids and reports the standard information-retrieval
//! numbers (precision@k, recall@k, MRR, nDCG@k). The gate it feeds is the
//! one named all over `search/hybrid.rs`: the §12 golden-set eval that "owns
//! retuning the weights and β" — i.e. it settles whether raising S4 (B69)
//! helps and whether a reranker earns its keep, by letting the founder SWEEP
//! `FusionWeights` and read the metric deltas off the same query set.
//!
//! WHY a pure module (modeled on `metrics.rs` / the voice-WER precedent): the
//! scoring math must be unit-testable WITHOUT a database, a corpus, or a
//! model. Given a ranked list of ids and a relevant set, every number here is
//! a deterministic function of those two inputs and `k` alone — so the unit
//! tests below pin hand-computed values (perfect / reversed / none-found /
//! partial / nDCG ideal-vs-actual) and those tests are what gate CI. The
//! wiring that turns a real search into a ranked list lives in the runner bin
//! (`pp_retrieval_eval`) and the sample integration test; this file never
//! touches `search/`.
//!
//! Identifier contract: the metrics are id-agnostic. They operate on opaque
//! `&str` ids; the CALLER decides what the stable key is. PhotoProof's stable
//! key is the image content hash (`ContentHash::as_str()`, a 64-char hex
//! BLAKE3) — that is what the search results carry and what a query-set file
//! lists as `relevant`. A synthetic-corpus fixture may instead use a stable
//! fixture id; the math does not care, only that the ranked list and the
//! relevant set draw from the SAME id space. See [`QuerySet`] for the file
//! format.
//!
//! Relevance model: BINARY (an id is relevant or it is not). nDCG uses gain
//! `1` for a relevant id and `0` otherwise, which collapses the gain term to
//! the standard binary form `1/log2(rank+1)`; graded relevance is a future
//! extension that would carry a per-id gain in the query-set file. Binary is
//! the right first cut because the founder's ground truth is "these images
//! are the answer to this query", not a graded preference order.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Query-set file format (the golden set)
// ---------------------------------------------------------------------------

/// A golden-query set: the founder's ground truth for the retrieval gate.
///
/// FORMAT (JSON; the runner loads it with `serde_json`). One object with a
/// `queries` array; each query carries its text, the set of relevant image
/// identifiers, and optional free-text notes:
///
/// ```json
/// {
///   "default_k": 10,
///   "queries": [
///     {
///       "query": "quiet melancholic harbor at dusk",
///       "relevant": [
///         "3a7f...64hex...",
///         "9c12...64hex..."
///       ],
///       "notes": "the slow-series candidates; clip-heavy, few notes"
///     }
///   ]
/// }
/// ```
///
/// IDENTIFIERS: each entry in `relevant` is a STABLE key shared by the corpus
/// and the search results. For a real library that key is the image content
/// hash (`ContentHash::as_str()` — 64-char hex BLAKE3), which is exactly what
/// `ImageResult::image_hash` carries, so a relevant id matches a result id by
/// plain string equality. A synthetic-corpus fixture may instead use a stable
/// fixture id; the scorer is id-agnostic (see the module header), it only
/// requires the ranked list and the relevant set to live in the same id
/// space.
///
/// WHY JSON over TOML: the relevant ids are long hex strings in arrays —
/// JSON arrays of strings are the least-friction shape for that, the crate
/// already depends on `serde_json`, and the runner's `--json` OUTPUT is JSON
/// too, so a sweep's input and output read with the same tools.
///
/// FOUNDER WORKFLOW: drop the real query set at a gitignored path under
/// `test-corpora/retrieval/` (see that directory's note) and point the runner
/// at it; the file never enters git because it encodes a private library's
/// hashes. The bundled SAMPLE set is the synthetic one the CI test builds in
/// process (see the `retrieval_eval_sample` integration test) — it proves the
/// pipe without shipping anyone's real hashes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySet {
    /// The default @k cutoff when the runner is not given one on the command
    /// line. `None` falls back to the runner's built-in default (10). Lives
    /// in the file so a query set can pin the k its relevant sets were
    /// curated for.
    #[serde(default)]
    pub default_k: Option<usize>,
    pub queries: Vec<GoldenQuery>,
}

/// One golden query: text + its relevant id set + optional notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenQuery {
    /// The raw query string, fed to the search rig exactly as a user typed.
    pub query: String,
    /// Stable ids of every image that SHOULD surface for this query — the
    /// binary ground truth. Order is irrelevant; it is a set.
    pub relevant: Vec<String>,
    /// Free-text rationale for the curator (why these are the answers, what
    /// signal the query is meant to stress). Never scored; kept for the
    /// report and for the next person to curate.
    #[serde(default)]
    pub notes: Option<String>,
}

impl GoldenQuery {
    /// The relevant ids as a set (the scorer's input). Built on demand so the
    /// on-disk shape can stay a plain JSON array.
    pub fn relevant_set(&self) -> HashSet<String> {
        self.relevant.iter().cloned().collect()
    }
}

/// One query's scored result, all metrics at a single cutoff `k`.
///
/// Every field is a pure function of (ranked ids, relevant set, k). `k` is
/// the cutoff the @k metrics were computed at; MRR is rank-of-first-relevant
/// and is NOT cutoff-bounded (a relevant item beyond `k` still contributes
/// its reciprocal rank — MRR answers "how deep to the first hit", which the
/// cutoff would hide).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryMetrics {
    /// The cutoff these @k numbers were computed at.
    pub k: usize,
    /// Relevant items in the top-k divided by `k` (NOT by hits returned):
    /// the standard precision@k denominator is the cutoff, so a short result
    /// list is penalized for the slots it left empty.
    pub precision_at_k: f64,
    /// Relevant items in the top-k divided by the TOTAL number of relevant
    /// items for the query. `0.0` when the query has no relevant items
    /// defined (an empty ground truth recalls nothing, vacuously).
    pub recall_at_k: f64,
    /// Reciprocal of the 1-based rank of the FIRST relevant item anywhere in
    /// the (full) ranked list; `0.0` if none is relevant. Not cutoff-bounded.
    pub mrr: f64,
    /// DCG@k / IDCG@k with the standard `log2(rank+1)` discount and binary
    /// gains. `1.0` for any ranking whose top-k is a perfect prefix of the
    /// relevant items; `0.0` when no relevant item falls in the top-k (or the
    /// query has no relevant items). Always in `[0, 1]`.
    pub ndcg_at_k: f64,
}

impl QueryMetrics {
    /// Score one ranked list against one relevant set at cutoff `k`.
    ///
    /// `ranked` is best-first (rank 1 = index 0); duplicate ids are treated
    /// as distinct list positions but only the FIRST occurrence of a relevant
    /// id counts toward precision/recall/DCG (a result list should not be
    /// able to inflate recall by repeating one true hit). `relevant` is the
    /// ground-truth set for the query.
    pub fn score(ranked: &[String], relevant: &HashSet<String>, k: usize) -> Self {
        // WHY guard k==0: a zero cutoff has no top-k, so every @k metric is a
        // degenerate 0.0 rather than a divide-by-zero. MRR still scans the
        // full list (it is not cutoff-bounded), so it is computed regardless.
        let total_relevant = relevant.len();

        // Walk the top-k ONCE, counting first-seen relevant hits and
        // accumulating DCG. `seen` dedupes so a repeated id cannot be
        // double-counted (see the dedupe rationale above).
        let mut seen: HashSet<&str> = HashSet::new();
        let mut hits_in_k: usize = 0;
        let mut dcg = 0.0_f64;
        for (idx, id) in ranked.iter().take(k).enumerate() {
            if !seen.insert(id.as_str()) {
                continue; // a duplicate position contributes nothing new
            }
            if relevant.contains(id) {
                hits_in_k += 1;
                // Binary-gain DCG: gain 1 at this rank, discounted by
                // log2(rank+1). rank is 1-based, so idx 0 -> log2(2) = 1.
                let rank = idx as f64 + 1.0;
                dcg += 1.0 / (rank + 1.0).log2();
            }
        }

        let precision_at_k = if k == 0 {
            0.0
        } else {
            hits_in_k as f64 / k as f64
        };
        let recall_at_k = if total_relevant == 0 {
            0.0
        } else {
            hits_in_k as f64 / total_relevant as f64
        };

        // IDCG@k: the best achievable DCG@k is every relevant item packed
        // into the top slots, but no more than `k` of them and no more than
        // exist. Same binary-gain discount summed over those ideal ranks.
        let ideal_hits = total_relevant.min(k);
        let mut idcg = 0.0_f64;
        for i in 0..ideal_hits {
            let rank = i as f64 + 1.0;
            idcg += 1.0 / (rank + 1.0).log2();
        }
        let ndcg_at_k = if idcg == 0.0 { 0.0 } else { dcg / idcg };

        // MRR: first relevant id anywhere in the FULL list (not just top-k),
        // deduped the same way so a repeat cannot move the first hit earlier.
        let mut mrr = 0.0_f64;
        let mut seen_mrr: HashSet<&str> = HashSet::new();
        for (idx, id) in ranked.iter().enumerate() {
            if !seen_mrr.insert(id.as_str()) {
                continue;
            }
            if relevant.contains(id) {
                mrr = 1.0 / (idx as f64 + 1.0);
                break;
            }
        }

        Self {
            k,
            precision_at_k,
            recall_at_k,
            mrr,
            ndcg_at_k,
        }
    }
}

/// Mean-over-queries aggregate plus the per-query breakdown — the report a
/// weight sweep diffs between runs.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalReport {
    /// The cutoff every @k number in this report was computed at.
    pub k: usize,
    /// Number of queries scored (the mean denominator).
    pub query_count: usize,
    /// Macro-averages (mean of the per-query metric, every query weighted
    /// equally regardless of how many relevant items it has). Macro, not
    /// micro, because the gate cares whether a weight change helps the
    /// TYPICAL query, not whether it helps the few queries with many
    /// relevant items.
    pub mean_precision_at_k: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub mean_ndcg_at_k: f64,
    /// One entry per scored query, in input order — so a sweep can show WHICH
    /// queries moved, not just the aggregate.
    pub per_query: Vec<NamedQueryMetrics>,
}

/// A per-query metric row tagged with the query it scored (for the report).
#[derive(Debug, Clone, PartialEq)]
pub struct NamedQueryMetrics {
    /// The query text (the human-readable label in the report).
    pub query: String,
    pub metrics: QueryMetrics,
}

impl EvalReport {
    /// Aggregate already-scored per-query rows into the mean-over-queries
    /// report. The caller scores each query (it owns the search rig); this
    /// only averages, so it stays as pure and testable as [`QueryMetrics`].
    pub fn from_scored(k: usize, per_query: Vec<NamedQueryMetrics>) -> Self {
        let query_count = per_query.len();
        // WHY a zero-query report reads all zeros, not NaN: an empty query
        // set is a valid (if useless) input — the runner should print zeros
        // and a "0 queries" line, never panic or emit NaN into the JSON.
        let mean = |sel: fn(&QueryMetrics) -> f64| -> f64 {
            if query_count == 0 {
                0.0
            } else {
                per_query.iter().map(|q| sel(&q.metrics)).sum::<f64>() / query_count as f64
            }
        };
        Self {
            k,
            query_count,
            mean_precision_at_k: mean(|m| m.precision_at_k),
            mean_recall_at_k: mean(|m| m.recall_at_k),
            mean_mrr: mean(|m| m.mrr),
            mean_ndcg_at_k: mean(|m| m.ndcg_at_k),
            per_query,
        }
    }
}

// ---------------------------------------------------------------------------
// Orchestration — run a query set through the real hybrid rig and score it.
//
// The scoring math above stays pure (no DB, no search). This thin driver is
// the ONE place the search rig and the scorer meet, so the runner bin
// (`pp-retrieval-eval`) and the sweep bin (`pp-sweep`) share it instead of
// each duplicating the run->score loop. It is the only part of this module
// that depends on `crate::search`; the metrics themselves never do.
// ---------------------------------------------------------------------------

use std::path::Path;
use std::sync::Arc;

use photoproof_connectors::OrtEmbedder;
use photoproof_connectors::vector_store::VectorStore;

use crate::retrieval::PpvecStore;
use crate::search::{FusionWeights, HybridOptions, HybridRig, NoModel, Searcher, keyword_only_rig};

// ---------------------------------------------------------------------------
// The model-backed eval rig (the app-vs-eval gap closer)
//
// `evaluate` used to run the KEYWORD-ONLY rig unconditionally: every vector
// signal (S1 annotation, S3 image-summary, S4 CLIP) was dark, so a sweep could
// only move S2 + the SQL-only S3 sub-list. That tuned a DIFFERENT search than
// the one the app ships (which embeds the QUERY with the real text + CLIP
// towers and ranks it against the stored `annotation_chunk` / `image_clip`
// vectors). `EvalRig` is the optional second arm: when models + the vector
// store are present it builds the SAME `HybridRig<NoModel, OrtEmbedder,
// OrtEmbedder>` the desktop's `run_search` builds (text + clip + vectors), so
// the eval embeds the query and exercises the FULL fusion. Absent models it is
// `None` and `evaluate` falls back to the keyword-only rig EXACTLY as before —
// the no-models CI path (the synthetic fixture, every unit test) is unchanged.
//
// WHY built once and reused across configs: embedding the query set is
// config-INDEPENDENT (the same towers, the same stored vectors); only the
// fusion WEIGHTS (`EvalConfig`) change per sweep config. The sweep builds the
// rig once and hands `&EvalRig` to `evaluate` per config, so the heavy ort
// load happens ONCE, not once per grid cell.
// ---------------------------------------------------------------------------

/// The real query-side embedders + the on-disk vector store, mirroring the
/// desktop search stack. Built once (the ort load is seconds) and shared by
/// every config in a sweep. `text` embeds the query into the annotation space
/// (S1) and the image-summary half of S3; `clip` embeds it into the CLIP space
/// (S4, B69 always-votes); `vectors` is the `.ppvec` store the app reads.
pub struct EvalRig {
    /// `None` for an image-only library (e.g. a COCO/Flickr benchmark with no
    /// notes): there are no `annotation_chunk` vectors, so the query is ranked by
    /// the CLIP visual signal (S4) alone - pure text->image retrieval, which is
    /// exactly what such a benchmark measures.
    text: Option<Arc<OrtEmbedder>>,
    clip: Arc<OrtEmbedder>,
    vectors: PpvecStore,
}

impl EvalRig {
    /// Build the rig from a models dir + the live library DB.
    ///
    /// `models_dir` is the app-data `models/` directory (`<id>/...` layout);
    /// `db_path` is the library SQLite and `vectors_dir` its sibling `vectors/`
    /// flat-file store. The text/clip model ids MUST match the ids the stored
    /// vectors were written under (the desktop pins `embeddinggemma-300m-q8` for
    /// text and `ViT-H-14-378-quickgelu__dfn5b` for clip): the search joins a
    /// query embedding to a stored space by `(vec_kind, model_id)`, so a
    /// mismatched id silently finds nothing. The caller resolves the ids off the
    /// `vectors` table (the model that produced the rows) so the rig can never
    /// embed into a space the library does not have.
    ///
    /// `Err` on a native load failure (missing/corrupt weights, an unknown id,
    /// a vector-store open failure) — the bin decides whether to surface it or
    /// degrade to keyword-only. The build reuses the SHARED connectors seam
    /// (`build_text_embedder` / `build_clip_embedder`), so the eval loads the
    /// exact same ort sessions the desktop host does.
    pub fn build(
        models_dir: &Path,
        text_model_id: Option<&str>,
        clip_model_id: &str,
        db_path: &Path,
        vectors_dir: &Path,
    ) -> Result<Self, String> {
        // The text embedder is only built when the library HAS annotation vectors
        // to rank against; an image-only benchmark skips it (S4-only retrieval).
        let text = match text_model_id {
            Some(id) => Some(Arc::new(
                photoproof_connectors::build_text_embedder(id, models_dir)
                    .map_err(|e| format!("loading text embedder {id}: {e}"))?,
            )),
            None => None,
        };
        let clip = photoproof_connectors::build_clip_embedder(clip_model_id, models_dir)
            .map_err(|e| format!("loading clip embedder {clip_model_id}: {e}"))?;
        let vectors = PpvecStore::open(db_path, vectors_dir)
            .map_err(|e| format!("opening vector store {}: {e}", vectors_dir.display()))?;
        Ok(Self {
            text,
            clip: Arc::new(clip),
            vectors,
        })
    }

    /// Build the rig, resolving the text + clip model ids off the live DB's
    /// `vectors` table (the model that actually produced the stored rows), and
    /// defaulting `vectors_dir` to the DB's sibling `vectors/`.
    ///
    /// WHY resolve from the DB and not hardcode the pinned ids: the search joins
    /// a query embedding to a stored space by `(vec_kind, model_id)`, so the rig
    /// MUST embed with the same model the library's vectors were written under.
    /// Reading the id off the table makes that impossible to get wrong (and makes
    /// the eval follow a library re-embedded under a different model for free).
    ///
    /// The text/annotation space is OPTIONAL: a library with no notes (a
    /// COCO/Flickr image benchmark) has no `annotation_chunk` vectors, and is
    /// ranked by the CLIP visual signal (S4) alone. `image_clip` is required -
    /// with neither space there is nothing to rank against.
    ///
    /// `Err` if the DB has no `image_clip` rows (nothing to rank against), or any
    /// build step fails.
    pub fn from_db(models_dir: &Path, db_path: &Path) -> Result<Self, String> {
        let text_model_id = resolve_vector_model_id_opt(db_path, "annotation_chunk")?;
        let clip_model_id = resolve_vector_model_id(db_path, "image_clip")?;
        let vectors_dir = crate::retrieval::default_vectors_dir(db_path)
            .ok_or_else(|| format!("cannot derive vectors dir from {}", db_path.display()))?;
        Self::build(
            models_dir,
            text_model_id.as_deref(),
            &clip_model_id,
            db_path,
            &vectors_dir,
        )
    }

    /// The model-backed `HybridRig` for one search — byte-identical in shape to
    /// the desktop `run_search` semantic rig (`llm: None`, text + clip + the
    /// vector store). Cheap to build per query (it only borrows the shared
    /// embedders + store), so `evaluate` can call it inside its per-query loop
    /// without re-loading anything.
    fn rig(&self) -> HybridRig<'_, NoModel, OrtEmbedder, OrtEmbedder> {
        HybridRig {
            llm: None,
            // None for an image-only library -> the annotation/S1 signals stay
            // dark and ranking is CLIP-visual (S4) only.
            text: self.text.as_deref(),
            clip: Some(self.clip.as_ref()),
            vectors: Some(&self.vectors as &dyn VectorStore),
        }
    }
}

/// The `model_id` the live DB stored its `vec_kind` vectors under (the model
/// that produced the rows). Read READ-ONLY — this is a pure lookup, it must
/// never migrate or write the live library. `Err` if the space is empty (no id
/// to read; nothing to rank against) or several ids coexist (an ambiguous
/// re-embed mid-flight — refuse rather than guess which space to query).
/// Like [`resolve_vector_model_id`] but an EMPTY space is `Ok(None)` rather than
/// an error - used for the OPTIONAL annotation space (an image-only benchmark
/// legitimately has none). An ambiguous space (several ids) is still an error.
fn resolve_vector_model_id_opt(db_path: &Path, vec_kind: &str) -> Result<Option<String>, String> {
    match resolve_vector_model_id(db_path, vec_kind) {
        Ok(id) => Ok(Some(id)),
        // The empty-space message is the one case we soften to None; a genuine
        // open/ambiguity error still propagates.
        Err(e) if e.contains("nothing to rank against") => Ok(None),
        Err(e) => Err(e),
    }
}

fn resolve_vector_model_id(db_path: &Path, vec_kind: &str) -> Result<String, String> {
    // Read-only open: no migration, no WAL checkpoint write — the live DB is
    // untouched (the K-constraint that the eval never mutates the library).
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("opening {} read-only: {e}", db_path.display()))?;
    let ids: Vec<String> = conn
        .prepare("SELECT DISTINCT model_id FROM vectors WHERE vec_kind = ?1 AND deleted = 0")
        .and_then(|mut stmt| {
            stmt.query_map([vec_kind], |row| row.get::<_, String>(0))?
                .collect()
        })
        .map_err(|e| format!("reading {vec_kind} model id: {e}"))?;
    match ids.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no {vec_kind} vectors in {} — nothing to rank against (re-embed the library first)",
            db_path.display()
        )),
        many => Err(format!(
            "ambiguous {vec_kind} model ids in {}: {many:?} — refusing to guess which space to query",
            db_path.display()
        )),
    }
}

/// The per-config knobs a single eval run varies: the four fusion weights plus
/// the two scalar dials (`rrf_k`, `beta`). One value object so the runner and
/// the sweep pass exactly the same surface to [`evaluate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvalConfig {
    /// §5.3 fusion weights (s1, s2, s3_each, s4).
    pub weights: FusionWeights,
    /// §5.3 RRF rank constant `k`.
    pub rrf_k: f64,
    /// §5.3 dense-signal similarity-tilt strength `β`.
    pub beta: f64,
}

impl Default for EvalConfig {
    /// The live baseline: fusion weights, `rrf_k`, and `β` from the active
    /// tuning config (file-overridable; code defaults absent a `tuning.toml`).
    fn default() -> Self {
        let search = &crate::tuning::tuning().search;
        Self {
            weights: search.fusion,
            rrf_k: search.rrf_k,
            beta: search.beta,
        }
    }
}

/// Run every query in `query_set` through the hybrid rig under `config`, score
/// each ranked result against its relevant set at cutoff `k`, and return the
/// mean-over-queries [`EvalReport`].
///
/// This is the shared entry point the `pp-retrieval-eval` runner and the
/// `pp-sweep` orchestrator both call: ONE definition of the run->score loop,
/// so a sweep can never silently diverge from the gate it tunes.
///
/// `rig` selects the search arm WITHOUT changing the loop:
/// - `None` -> the keyword-only rig (the no-models CI path, unchanged): every
///   vector signal is dark, so `FusionWeights`/`β`/`rrf_k` shape only S2 + the
///   SQL-only S3 sub-list. This is what the synthetic fixture and every headless
///   test exercise; it requires no models.
/// - `Some(rig)` -> the model-backed rig the desktop app ships (text + clip +
///   the vector store): the query is EMBEDDED into the annotation space (S1) and
///   the CLIP space (S4, B69) and ranked against the stored vectors. This is the
///   founder-machine run that exercises the FULL fusion. Same per-query scoring,
///   same metrics — only the candidate generation differs.
///
/// `rig` is built ONCE and passed per config, so a sweep reuses the same loaded
/// ort sessions + vectors across every grid cell (the embedding is
/// config-independent; only the fusion weights move).
pub fn evaluate(
    searcher: &Searcher,
    query_set: &QuerySet,
    config: EvalConfig,
    k: usize,
    rig: Option<&EvalRig>,
) -> Result<EvalReport, String> {
    // One options value per run; weights/β/rrf_k are the swept knobs, the rest
    // are the defaults (no `now` override -> wall-clock for any relative-date
    // filter, exactly as the runner did inline).
    let opts = HybridOptions {
        weights: config.weights,
        beta: config.beta,
        rrf_k: config.rrf_k,
        ..HybridOptions::default()
    };
    let mut scored: Vec<NamedQueryMetrics> = Vec::with_capacity(query_set.queries.len());
    for GoldenQuery {
        query, relevant, ..
    } in &query_set.queries
    {
        // Branch on the arm per query. The two rigs are DISTINCT generic types
        // (`HybridRig<NoModel,NoModel,NoModel>` vs `<NoModel,OrtEmbedder,
        // OrtEmbedder>`), so the match arms each call the monomorphized
        // `hybrid_search`; the `None` arm is byte-identical to the prior
        // keyword-only behavior the CI tests pin.
        let out = match rig {
            Some(eval_rig) => searcher.hybrid_search(query, &[], &eval_rig.rig(), &opts),
            None => searcher.hybrid_search(query, &[], &keyword_only_rig(), &opts),
        }
        .map_err(|e| format!("search failed for {query:?}: {e}"))?;
        let ranked: Vec<String> = out
            .images
            .iter()
            .map(|r| r.image_hash.as_str().to_owned())
            .collect();
        let relevant_set: HashSet<String> = relevant.iter().cloned().collect();
        scored.push(NamedQueryMetrics {
            query: query.clone(),
            metrics: QueryMetrics::score(&ranked, &relevant_set, k),
        });
    }
    Ok(EvalReport::from_scored(k, scored))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a relevant set from string literals — test ergonomics only.
    fn rel(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ranked(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Floor-compare a float to 6 decimals — the metrics are exact rationals
    /// of log2 terms, so we pin them to a tight tolerance rather than chase
    /// the last ULP. (Mirrors the floor6 discipline in retrieval_hybrid.rs.)
    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    // --- Perfect ranking: relevant items occupy the exact top slots. -------
    #[test]
    fn perfect_ranking_scores_all_ones() {
        // Two relevant items at ranks 1 and 2; k=2 so the top-k is exactly
        // the relevant set.
        let m = QueryMetrics::score(&ranked(&["a", "b", "c", "d"]), &rel(&["a", "b"]), 2);
        approx(m.precision_at_k, 1.0); // 2 hits / 2 slots
        approx(m.recall_at_k, 1.0); // 2 hits / 2 relevant
        approx(m.mrr, 1.0); // first relevant at rank 1
        // DCG = 1/log2(2) + 1/log2(3) = 1 + 0.6309298 ; IDCG identical ->
        // perfect prefix normalizes to exactly 1.0.
        approx(m.ndcg_at_k, 1.0);
    }

    // --- Reversed ranking: relevant items at the BOTTOM of the list. -------
    #[test]
    fn reversed_ranking_drops_precision_and_ndcg() {
        // Relevant {c, d} sit at ranks 3 and 4; k=4 so they are inside the
        // cutoff but maximally discounted.
        let m = QueryMetrics::score(&ranked(&["a", "b", "c", "d"]), &rel(&["c", "d"]), 4);
        approx(m.precision_at_k, 0.5); // 2 hits / 4 slots
        approx(m.recall_at_k, 1.0); // both relevant items are in top-4
        approx(m.mrr, 1.0 / 3.0); // first relevant at rank 3
        // DCG = 1/log2(4) + 1/log2(5) = 0.5 + 0.4306766 = 0.9306766
        // IDCG (ideal: ranks 1,2) = 1 + 0.6309298 = 1.6309298
        let dcg = 1.0 / 4.0_f64.log2() + 1.0 / 5.0_f64.log2();
        let idcg = 1.0 / 2.0_f64.log2() + 1.0 / 3.0_f64.log2();
        approx(m.ndcg_at_k, dcg / idcg);
    }

    // --- No relevant item found: everything zero, never NaN. ---------------
    #[test]
    fn no_relevant_found_is_all_zeros() {
        let m = QueryMetrics::score(&ranked(&["a", "b", "c"]), &rel(&["x", "y"]), 3);
        approx(m.precision_at_k, 0.0);
        approx(m.recall_at_k, 0.0);
        approx(m.mrr, 0.0);
        approx(m.ndcg_at_k, 0.0);
    }

    // --- Empty ground truth: recall/nDCG are vacuously zero, not NaN. ------
    #[test]
    fn empty_relevant_set_does_not_divide_by_zero() {
        let m = QueryMetrics::score(&ranked(&["a", "b"]), &rel(&[]), 2);
        approx(m.precision_at_k, 0.0); // 0 hits / 2 slots
        approx(m.recall_at_k, 0.0); // 0 relevant -> vacuous 0
        approx(m.mrr, 0.0);
        approx(m.ndcg_at_k, 0.0); // IDCG is 0 -> guarded to 0
    }

    // --- Partial: some relevant in top-k, some beyond it. ------------------
    #[test]
    fn partial_hits_split_precision_and_recall() {
        // Relevant {a, x, z}; ranked a(1) b(2) x(3) c(4) ...; cutoff k=3.
        // In top-3: a, x -> 2 hits. z never appears.
        let m = QueryMetrics::score(
            &ranked(&["a", "b", "x", "c", "d"]),
            &rel(&["a", "x", "z"]),
            3,
        );
        approx(m.precision_at_k, 2.0 / 3.0); // 2 hits / 3 slots
        approx(m.recall_at_k, 2.0 / 3.0); // 2 hits / 3 relevant total
        approx(m.mrr, 1.0); // a is relevant at rank 1
        // DCG@3 = 1/log2(2) [rank1 a] + 1/log2(4) [rank3 x] = 1 + 0.5 = 1.5
        // IDCG@3 = ideal 3 relevant but only k=3 slots & 3 exist:
        //   1/log2(2)+1/log2(3)+1/log2(4) = 1 + 0.6309298 + 0.5 = 2.1309298
        let dcg = 1.0 / 2.0_f64.log2() + 1.0 / 4.0_f64.log2();
        let idcg = 1.0 / 2.0_f64.log2() + 1.0 / 3.0_f64.log2() + 1.0 / 4.0_f64.log2();
        approx(m.ndcg_at_k, dcg / idcg);
    }

    // --- nDCG ideal-vs-actual: same hits, better order scores higher. ------
    #[test]
    fn ndcg_rewards_better_ordering_of_the_same_hits() {
        let relevant = rel(&["a", "b"]);
        // Both lists contain a and b in the top-4, but list_good ranks them
        // 1,2 (ideal) and list_bad ranks them 3,4.
        let good = QueryMetrics::score(&ranked(&["a", "b", "c", "d"]), &relevant, 4);
        let bad = QueryMetrics::score(&ranked(&["c", "d", "a", "b"]), &relevant, 4);
        // Same recall (both find both), but nDCG strictly prefers the better
        // order — this is the property the reranker go/no-go leans on.
        approx(good.recall_at_k, 1.0);
        approx(bad.recall_at_k, 1.0);
        approx(good.ndcg_at_k, 1.0);
        assert!(
            bad.ndcg_at_k < good.ndcg_at_k,
            "worse ordering must score lower nDCG: {} !< {}",
            bad.ndcg_at_k,
            good.ndcg_at_k
        );
    }

    // --- MRR is not cutoff-bounded: first hit beyond k still counts. -------
    #[test]
    fn mrr_sees_past_the_cutoff() {
        // Only relevant item is at rank 5, but k=2. precision/recall@2 are 0
        // (it is outside the cutoff) while MRR reports 1/5.
        let m = QueryMetrics::score(&ranked(&["a", "b", "c", "d", "z"]), &rel(&["z"]), 2);
        approx(m.precision_at_k, 0.0);
        approx(m.recall_at_k, 0.0);
        approx(m.ndcg_at_k, 0.0);
        approx(m.mrr, 1.0 / 5.0);
    }

    // --- Duplicate ids cannot inflate recall. ------------------------------
    #[test]
    fn duplicate_ids_count_once() {
        // "a" repeated three times; only one relevant item exists.
        let m = QueryMetrics::score(&ranked(&["a", "a", "a"]), &rel(&["a"]), 3);
        // One unique hit, k=3 slots -> precision 1/3 (NOT 3/3).
        approx(m.precision_at_k, 1.0 / 3.0);
        approx(m.recall_at_k, 1.0); // the single relevant item is found
        approx(m.ndcg_at_k, 1.0); // one hit at rank 1 == ideal for 1 relevant
        approx(m.mrr, 1.0);
    }

    // --- Aggregate: mean over queries weights each query equally. ----------
    #[test]
    fn report_means_each_query_equally() {
        let q1 = NamedQueryMetrics {
            query: "q1".into(),
            metrics: QueryMetrics::score(&ranked(&["a"]), &rel(&["a"]), 1), // all 1.0
        };
        let q2 = NamedQueryMetrics {
            query: "q2".into(),
            metrics: QueryMetrics::score(&ranked(&["b"]), &rel(&["z"]), 1), // all 0.0
        };
        let report = EvalReport::from_scored(1, vec![q1, q2]);
        approx(report.mean_precision_at_k, 0.5);
        approx(report.mean_recall_at_k, 0.5);
        approx(report.mean_mrr, 0.5);
        approx(report.mean_ndcg_at_k, 0.5);
        assert_eq!(report.query_count, 2);
        assert_eq!(report.per_query.len(), 2);
    }

    #[test]
    fn empty_report_reads_zeros_not_nan() {
        let report = EvalReport::from_scored(10, vec![]);
        approx(report.mean_ndcg_at_k, 0.0);
        assert_eq!(report.query_count, 0);
    }

    // --- Query-set format: parses the documented shape, optional fields. ---
    #[test]
    fn query_set_parses_documented_json() {
        let json = r#"{
            "default_k": 5,
            "queries": [
                { "query": "harbor at dusk", "relevant": ["aa", "bb"], "notes": "clip-heavy" },
                { "query": "fog", "relevant": ["cc"] }
            ]
        }"#;
        let qs: QuerySet = serde_json::from_str(json).unwrap();
        assert_eq!(qs.default_k, Some(5));
        assert_eq!(qs.queries.len(), 2);
        assert_eq!(qs.queries[0].query, "harbor at dusk");
        assert_eq!(qs.queries[0].relevant_set(), rel(&["aa", "bb"]));
        assert_eq!(qs.queries[0].notes.as_deref(), Some("clip-heavy"));
        // notes and default_k are optional and default cleanly.
        assert_eq!(qs.queries[1].notes, None);
    }

    #[test]
    fn query_set_default_k_is_optional() {
        let json = r#"{ "queries": [ { "query": "x", "relevant": [] } ] }"#;
        let qs: QuerySet = serde_json::from_str(json).unwrap();
        assert_eq!(qs.default_k, None);
        assert!(qs.queries[0].relevant_set().is_empty());
    }
}
