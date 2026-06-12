//! Stages 2–4 of the §5 pipeline: candidate generation over the four
//! signals, weighted-RRF fusion (k = 60), and the §5.4 result contract
//! with best-quote provenance.
//!
//! Contract: spec/RETRIEVAL.md §5.2–5.4, §6, §7.1 worked example, §13.
//! M1 is the degenerate case: with no models in the rig (or every vector
//! signal empty) the output is exactly the M1 [`exec::run_search`] result,
//! so the shipping no-models posture is unchanged by construction.
//!
//! B69 (founder, binding): `image_clip` (S4) votes on EVERY semantic query
//! — own-words identity is protected by WEIGHT (0.5 vs 1.0), not by the
//! §5.2 activation gate, which B69 demotes to a latency lever this packet
//! does not need. Annotating an image only ever ADDS ways to find it.
//!
//! Signal failure posture: a vector signal that errors (store IO, embedder
//! down) degrades to "absent" with a logged warning — search never errors
//! because a model did (§5.1 extends to every model in the pipeline).
//! Filters are the exception: they are hard constraints and still error
//! loudly when unexecutable.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use photoproof_connectors::embedder::{Embedder, Embedding};
use photoproof_connectors::llm::LanguageModel;
use photoproof_connectors::vector_store::{VecHit, VecKind, VecSpace, VectorStore};
use rusqlite::Connection;
use rusqlite::types::Value;
use serde_json::json;

use crate::id::{ContentHash, UtcMillis};
use crate::retrieval::instruct_query;

use super::filter::{FilterMode, compile};
use super::query::fts_match_query;
use super::{
    DebugScores, DroppedClause, Filter, ImageResult, ParsedQuery, PreviewRef, Provenance,
    QueryEcho, SearchError, SearchResults, SignalId, exec, parse,
};

/// §5.3 RRF constant.
pub const RRF_K: f64 = 60.0;

/// §5.2 candidate depth for the vector signals.
const VECTOR_K: usize = 200;

/// §5.3: top 100 fused images proceed to results.
const FUSED_LIMIT: usize = 100;

/// §5.1 parse latency budget.
const PARSE_BUDGET: Duration = Duration::from_millis(1500);

/// §5.3 fusion weights. Defaults are the spec's — and explicitly defaults,
/// not findings: the §12 golden-set eval is the named gate for tuning them
/// (which is why they are data, not constants).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusionWeights {
    /// S1 `annotation_chunk` vectors — primary.
    pub s1: f64,
    /// S2 `event_fts` (FTS5).
    pub s2: f64,
    /// Each S3 sub-list (`summaries_fts`, `image_summary` vectors):
    /// summaries are derived prose and must never outvote the
    /// photographer's actual words.
    pub s3_each: f64,
    /// S4 `image_clip` (B69: always votes on semantic queries).
    pub s4: f64,
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self {
            s1: 1.0,
            s2: 1.0,
            s3_each: 0.5,
            s4: 0.5,
        }
    }
}

/// Per-search knobs for [`super::Searcher::hybrid_search`].
#[derive(Debug, Clone)]
pub struct HybridOptions {
    /// Anchors relative date ranges (§5.1) and the parse prompt's "today".
    pub now: Option<UtcMillis>,
    /// Populates [`ImageResult::debug`] (dev builds only).
    pub include_debug: bool,
    pub weights: FusionWeights,
    /// §5.1: the parse must complete in < 1.5 s; a slower one is discarded
    /// in favor of the fallback even though it answered.
    pub parse_budget: Duration,
}

impl Default for HybridOptions {
    fn default() -> Self {
        Self {
            now: None,
            include_debug: false,
            weights: FusionWeights::default(),
            parse_budget: PARSE_BUDGET,
        }
    }
}

/// The pipeline's model collaborators, all optional — `None` slots degrade
/// that stage, never error it (the M1 posture that already ships). Mirrors
/// the library's `EmbeddingRig` precedent.
pub struct HybridRig<'a, L, TE, CE> {
    /// The parse model (§5.1). `None` = no NL parse requested: chips +
    /// keywords run the degenerate parse with `fallback: false`.
    pub llm: Option<&'a L>,
    /// The text embedder (S1 + the `image_summary` half of S3).
    pub text: Option<&'a TE>,
    /// The CLIP embedder (S4 query-side text tower; bare text, no instruct
    /// template — §3).
    pub clip: Option<&'a CE>,
    /// The vector store behind the §1.3 trait seam. `VecHit` joins back to
    /// the `vectors` metadata table by rowid (§1.3), which is unchanged by
    /// any backend swap.
    pub vectors: Option<&'a dyn VectorStore>,
}

/// Type-level placeholder for absent connectors: the rig stores
/// `Option<&T>` and a `None` slot still needs a concrete `T`. Uninhabited,
/// so the impls below are unreachable by construction.
pub enum NoModel {}

impl LanguageModel for NoModel {
    async fn complete(
        &self,
        _req: photoproof_connectors::llm::ChatRequest,
    ) -> photoproof_connectors::ConnectorResult<photoproof_connectors::llm::ChatResponse> {
        match *self {}
    }

    async fn caption_image(
        &self,
        _img: &photoproof_connectors::embedder::DecodedImage,
        _prompt: &str,
    ) -> photoproof_connectors::ConnectorResult<String> {
        match *self {}
    }

    fn model_id(&self) -> &str {
        match *self {}
    }
}

impl Embedder for NoModel {
    async fn embed_text(&self, _text: &str) -> photoproof_connectors::ConnectorResult<Embedding> {
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

/// The no-models rig — the shipping M1 posture. Keyword search plus
/// collection-chip resolution; every model stage degrades to absent.
pub fn keyword_only_rig() -> HybridRig<'static, NoModel, NoModel, NoModel> {
    HybridRig {
        llm: None,
        text: None,
        clip: None,
        vectors: None,
    }
}

// ---------------------------------------------------------------------------
// Stage 1 — parse (LLM when present; degenerate/fallback otherwise)
// ---------------------------------------------------------------------------

struct Parse {
    filters: Vec<Filter>,
    dropped: Vec<DroppedClause>,
    semantic: Option<String>,
    keywords: Option<String>,
    visual: bool,
    fallback: bool,
}

impl Parse {
    /// The degenerate parse (§5: "M1 behavior is the degenerate case of
    /// this pipeline"): chips are the filters, the raw string is the FTS
    /// remainder, and it doubles as the embedding text only when a vector
    /// signal could actually consume it (keeps the no-models echo
    /// identical to M1's).
    fn degenerate(trimmed: &str, any_vector_signal: bool) -> Self {
        let keywords = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        Self {
            filters: Vec::new(),
            dropped: Vec::new(),
            semantic: keywords.clone().filter(|_| any_vector_signal),
            keywords,
            visual: false,
            fallback: false,
        }
    }

    /// §5.1 fallback: the whole raw query as both FTS string and embedding
    /// text, zero filters, `fallback: true`. Dropped clauses (if the parse
    /// got far enough to produce any) stay visible in the debug panel.
    fn fallback(trimmed: &str, dropped: Vec<DroppedClause>) -> Self {
        let keywords = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        Self {
            filters: Vec::new(),
            dropped,
            semantic: keywords.clone(),
            keywords,
            visual: false,
            fallback: true,
        }
    }
}

fn stage1_parse<L: LanguageModel>(
    conn: &Connection,
    trimmed: &str,
    llm: Option<&L>,
    any_vector_signal: bool,
    now: UtcMillis,
    budget: Duration,
) -> Result<Parse, SearchError> {
    let Some(llm) = llm else {
        return Ok(Parse::degenerate(trimmed, any_vector_signal));
    };
    if trimmed.is_empty() {
        // Nothing to parse; filter chips alone are already typed.
        return Ok(Parse::degenerate(trimmed, any_vector_signal));
    }
    let grounding = parse::load_grounding(conn)?;
    let request = parse::build_request(trimmed, &grounding, now);
    let started = Instant::now();
    // block_on mirrors the embedding drain's posture: model IO is one call
    // at a time and a full async runtime would be dead weight here. The
    // budget check is post-hoc — wall-clock preemption belongs to the
    // serving layer's transport timeouts (RUNTIME); what §5.1 requires is
    // that a late parse never shapes the results.
    let response = pollster::block_on(llm.complete(request));
    let elapsed = started.elapsed();
    match response {
        Ok(resp) if elapsed <= budget => match parse::validate_response(&resp.text, &grounding) {
            Some(v) if !(v.filters.is_empty() && v.semantic.is_none()) => Ok(Parse {
                keywords: v.semantic.clone(), // §5.1: keywords = semantic post-parse
                semantic: v.semantic,
                filters: v.filters,
                dropped: v.dropped,
                visual: v.visual,
                fallback: false,
            }),
            Some(v) => {
                // Everything dropped and no remainder (§5.1) — fall back,
                // keeping the rejects visible.
                Ok(Parse::fallback(trimmed, v.dropped))
            }
            None => {
                tracing::warn!("query parse returned undeserializable JSON; falling back");
                Ok(Parse::fallback(trimmed, Vec::new()))
            }
        },
        Ok(_) => {
            tracing::warn!(
                ?elapsed,
                "query parse exceeded the latency budget; falling back"
            );
            Ok(Parse::fallback(trimmed, Vec::new()))
        }
        Err(e) => {
            tracing::warn!(error = %e, "query parse model unavailable; falling back");
            Ok(Parse::fallback(trimmed, Vec::new()))
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 2 — candidate generation
// ---------------------------------------------------------------------------

/// One image's S1 evidence: its best (max-similarity) chunk.
struct S1Best {
    event_id: String,
    char_start: u32,
    char_end: u32,
}

/// Image hashes in rank order (1-based by index) with the signal's raw
/// score — one signal's candidate list.
type RankedImages = Vec<(String, f32)>;

/// A ranked candidate list plus which §5.2 signal produced it.
struct RankedList {
    signal: SignalId,
    weight: f64,
    images: RankedImages,
}

/// Resolve S1 chunk hits to a per-image ranked list (max per signal — best
/// chunk per image) within the filtered universe. Multi-target events rank
/// all their surviving targets (§1.2: the remark is about all of them);
/// per-image filters exclude individual targets through the join.
fn resolve_s1(
    conn: &Connection,
    hits: &[VecHit],
    filters: &[Filter],
    now: UtcMillis,
) -> Result<(RankedImages, HashMap<String, S1Best>), SearchError> {
    if hits.is_empty() {
        return Ok((Vec::new(), HashMap::new()));
    }
    let cf = compile(filters, FilterMode::Hits, now)?;
    let marks = vec!["?"; hits.len()].join(",");
    let mut sql = String::from(
        "SELECT v.id, v.event_id, v.char_start, v.char_end, t.image_hash\n\
         FROM vectors v\n\
         JOIN event_targets t ON t.event_id = v.event_id\n",
    );
    if cf.join_event {
        sql.push_str("JOIN annotation_events e ON e.id = v.event_id\n");
    }
    if cf.join_images {
        sql.push_str("JOIN images i ON i.image_hash = t.image_hash\n");
    }
    sql.push_str(&format!("WHERE v.id IN ({marks})\n"));
    for w in &cf.wheres {
        sql.push_str("  AND ");
        sql.push_str(w);
        sql.push('\n');
    }
    let mut params: Vec<Value> = hits.iter().map(|h| Value::Integer(h.vector_id)).collect();
    params.extend(cf.params.iter().cloned());

    struct Row {
        event_id: String,
        char_start: u32,
        char_end: u32,
        image_hash: String,
    }
    let mut by_vector: HashMap<i64, Vec<Row>> = HashMap::new();
    {
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            by_vector.entry(row.get(0)?).or_default().push(Row {
                event_id: row.get(1)?,
                char_start: row.get::<_, Option<u32>>(2)?.unwrap_or(0),
                char_end: row.get::<_, Option<u32>>(3)?.unwrap_or(0),
                image_hash: row.get(4)?,
            });
        }
    }
    // Hits arrive best-first from the store; the first appearance of an
    // image is its max — its rank within the signal and its best chunk.
    let mut ranked: Vec<(String, f32)> = Vec::new();
    let mut best: HashMap<String, S1Best> = HashMap::new();
    for hit in hits {
        let Some(rows) = by_vector.get_mut(&hit.vector_id) else {
            continue; // filtered out, or a foreign/stale row
        };
        // Same-score multi-target rows order by image hash (determinism).
        rows.sort_by(|a, b| a.image_hash.cmp(&b.image_hash));
        for row in rows.iter() {
            if best.contains_key(&row.image_hash) {
                continue;
            }
            best.insert(
                row.image_hash.clone(),
                S1Best {
                    event_id: row.event_id.clone(),
                    char_start: row.char_start,
                    char_end: row.char_end,
                },
            );
            ranked.push((row.image_hash.clone(), hit.score));
        }
    }
    Ok((ranked, best))
}

/// Resolve image-keyed vector hits (`image_summary`, `image_clip`) to a
/// ranked image list within the filtered universe (browse-mode compile:
/// image-level signal, image-level constraints).
fn resolve_image_hits(
    conn: &Connection,
    hits: &[VecHit],
    filters: &[Filter],
    now: UtcMillis,
) -> Result<Vec<(String, f32)>, SearchError> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let cf = compile(filters, FilterMode::Browse, now)?;
    let marks = vec!["?"; hits.len()].join(",");
    let mut sql = format!(
        "SELECT v.id, v.image_hash FROM vectors v\n\
         JOIN images i ON i.image_hash = v.image_hash\n\
         WHERE v.id IN ({marks})\n"
    );
    for w in &cf.wheres {
        sql.push_str("  AND ");
        sql.push_str(w);
        sql.push('\n');
    }
    let mut params: Vec<Value> = hits.iter().map(|h| Value::Integer(h.vector_id)).collect();
    params.extend(cf.params.iter().cloned());
    let mut by_vector: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            by_vector.insert(row.get(0)?, row.get(1)?);
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut ranked = Vec::new();
    for hit in hits {
        if let Some(hash) = by_vector.get(&hit.vector_id)
            && seen.insert(hash.clone())
        {
            ranked.push((hash.clone(), hit.score));
        }
    }
    Ok(ranked)
}

/// S3's FTS sub-list: `summaries_fts` ranked by bm25, mapped to images
/// through `derived_summaries` (scope = 'image'), filtered universe.
fn s3_summaries_fts(
    conn: &Connection,
    match_q: &str,
    filters: &[Filter],
    now: UtcMillis,
) -> Result<Vec<(String, f32)>, SearchError> {
    let cf = compile(filters, FilterMode::Browse, now)?;
    // Same materialize-first discipline as the §4 statement.
    let mut sql = String::from(
        "WITH shits AS MATERIALIZED (\n\
         \x20 SELECT summaries_fts.summary_id AS sid, bm25(summaries_fts) AS s\n\
         \x20 FROM summaries_fts\n\
         \x20 WHERE summaries_fts MATCH ?1\n\
         \x20 ORDER BY rank\n\
         \x20 LIMIT 500\n\
         )\n\
         SELECT ds.scope_key, h.s\n\
         FROM shits h\n\
         JOIN derived_summaries ds ON ds.id = h.sid\n\
         JOIN images i ON i.image_hash = ds.scope_key\n\
         WHERE ds.scope = 'image'\n",
    );
    for w in &cf.wheres {
        sql.push_str("  AND ");
        sql.push_str(w);
        sql.push('\n');
    }
    sql.push_str("ORDER BY h.s ASC, ds.scope_key ASC");
    let mut params: Vec<Value> = vec![Value::Text(match_q.to_owned())];
    params.extend(cf.params.iter().cloned());
    let mut seen: HashSet<String> = HashSet::new();
    let mut ranked = Vec::new();
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    while let Some(row) = rows.next()? {
        let hash: String = row.get(0)?;
        let s: f64 = row.get(1)?;
        if seen.insert(hash.clone()) {
            ranked.push((hash, s as f32));
        }
    }
    Ok(ranked)
}

/// One vector-space search through the trait seam, degrading to empty on
/// failure (the signal goes absent; search never errors because a model
/// or its storage did).
fn vector_search(
    vectors: &dyn VectorStore,
    query: &Embedding,
    vec_kind: VecKind,
    model_id: &str,
) -> Vec<VecHit> {
    let space = VecSpace {
        vec_kind,
        model_id: model_id.to_owned(),
    };
    match vectors.search(query, space, VECTOR_K) {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = %e, ?vec_kind, "vector signal failed; degrading to absent");
            Vec::new()
        }
    }
}

/// Embed through the seam, degrading to `None` on failure.
fn embed_text<E: Embedder>(embedder: &E, text: &str, what: &str) -> Option<Embedding> {
    match pollster::block_on(embedder.embed_text(text)) {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!(error = %e, "{what} query embedding failed; signal degrades to absent");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The pipeline
// ---------------------------------------------------------------------------

pub(crate) fn run<L, TE, CE>(
    conn: &Connection,
    raw: &str,
    chips: &[Filter],
    rig: &HybridRig<'_, L, TE, CE>,
    opts: &HybridOptions,
    now: UtcMillis,
) -> Result<SearchResults, SearchError>
where
    L: LanguageModel,
    TE: Embedder,
    CE: Embedder,
{
    let trimmed = raw.trim();
    let any_vector_signal = rig.vectors.is_some() && (rig.text.is_some() || rig.clip.is_some());

    // Stage 1 — parse.
    let mut parsed = stage1_parse(
        conn,
        trimmed,
        rig.llm,
        any_vector_signal,
        now,
        opts.parse_budget,
    )?;

    // Chips merge in as-is (already typed; the same AST the parser emits).
    parsed.filters.extend(chips.iter().cloned());

    // §10.3 — resolve collection names (no model required; the LLM-parsed
    // clauses already resolved inside validation, this covers chips).
    let needs_resolution = parsed
        .filters
        .iter()
        .any(|f| matches!(f, Filter::Collection(c) if c.resolved.is_none()));
    if needs_resolution {
        let rows = {
            let g = parse::load_grounding(conn)?;
            g.collections
        };
        let mut resolved = Vec::with_capacity(parsed.filters.len());
        for f in parsed.filters.drain(..) {
            match f {
                Filter::Collection(c) if c.resolved.is_none() => {
                    match parse::resolve_collection(&rows, &c.raw) {
                        Ok(r) => resolved.push(Filter::Collection(r)),
                        Err(reason) => parsed.dropped.push(DroppedClause {
                            raw: json!({ "type": "collection", "name": c.raw }),
                            reason,
                        }),
                    }
                }
                other => resolved.push(other),
            }
        }
        parsed.filters = resolved;
    }

    let echo = |parsed: &Parse| QueryEcho {
        raw: raw.to_owned(),
        parsed: ParsedQuery {
            filters: parsed.filters.clone(),
            semantic: parsed.semantic.clone(),
            keywords: parsed.keywords.clone(),
            visual: parsed.visual,
            dropped: parsed.dropped.clone(),
            fallback: parsed.fallback,
        },
    };

    // Stage 2 — candidates. S2 (and the filter-only browse degenerate
    // case) reuse the M1 executor wholesale: same statement shape, same
    // grouping, same provenance.
    let keywords = parsed.keywords.clone().unwrap_or_default();
    let mut base = exec::run_search(conn, &keywords, &parsed.filters, now, opts.include_debug)?;

    let semantic = parsed.semantic.clone().filter(|s| !s.is_empty());
    let match_q = fts_match_query(&keywords);

    // The text query embeds at most once (§5.2): it serves S1 and the
    // image_summary half of S3 — same space, same model.
    let text_query = match (&semantic, rig.text, rig.vectors) {
        (Some(s), Some(te), Some(_)) => embed_text(te, &instruct_query(s), "text"),
        _ => None,
    };
    let (s1_ranked, s1_best) = match (&text_query, rig.vectors, rig.text) {
        (Some(q), Some(vs), Some(te)) => {
            let hits = vector_search(vs, q, VecKind::AnnotationChunk, te.model_id());
            resolve_s1(conn, &hits, &parsed.filters, now)?
        }
        _ => (Vec::new(), HashMap::new()),
    };
    let s3_vec = match (&text_query, rig.vectors, rig.text) {
        (Some(q), Some(vs), Some(te)) => {
            let hits = vector_search(vs, q, VecKind::ImageSummary, te.model_id());
            resolve_image_hits(conn, &hits, &parsed.filters, now)?
        }
        _ => Vec::new(),
    };
    let s3_fts = match &match_q {
        Some(q) => s3_summaries_fts(conn, q, &parsed.filters, now)?,
        None => Vec::new(),
    };
    // S4 — B69: image_clip votes on every semantic query (bare text on the
    // CLIP tower; no instruct template, §3).
    let s4_ranked = match (&semantic, rig.clip, rig.vectors) {
        (Some(s), Some(ce), Some(vs)) => match embed_text(ce, s, "clip") {
            Some(q) => {
                let hits = vector_search(vs, &q, VecKind::ImageClip, ce.model_id());
                resolve_image_hits(conn, &hits, &parsed.filters, now)?
            }
            None => Vec::new(),
        },
        _ => Vec::new(),
    };

    // Degenerate case: nothing beyond S2 voted — the M1 result IS the
    // answer (identical scores, provenance, ordering), with the hybrid
    // echo carrying any resolution/drop bookkeeping.
    if s1_ranked.is_empty() && s3_vec.is_empty() && s3_fts.is_empty() && s4_ranked.is_empty() {
        base.query = echo(&parsed);
        return Ok(base);
    }

    // Stage 3 — weighted RRF, k = 60 (§5.3).
    let s2_ranked: Vec<(String, f32)> = base
        .images
        .iter()
        .map(|i| (i.image_hash.to_string(), i.score))
        .collect();
    let s2_results: HashMap<String, ImageResult> = base
        .images
        .iter()
        .map(|i| (i.image_hash.to_string(), i.clone()))
        .collect();

    let w = &opts.weights;
    let lists: Vec<RankedList> = vec![
        RankedList {
            signal: SignalId::S1AnnotationChunk,
            weight: w.s1,
            images: s1_ranked,
        },
        RankedList {
            signal: SignalId::S2EventFts,
            weight: w.s2,
            images: s2_ranked,
        },
        RankedList {
            signal: SignalId::S3Summaries,
            weight: w.s3_each,
            images: s3_fts,
        },
        RankedList {
            signal: SignalId::S3Summaries,
            weight: w.s3_each,
            images: s3_vec,
        },
        RankedList {
            signal: SignalId::S4ImageClip,
            weight: w.s4,
            images: s4_ranked,
        },
    ];

    let mut fused: HashMap<String, f64> = HashMap::new();
    // Which signals ranked each image — the per-result signal provenance
    // (§5.4 DebugScores): (signal, 1-based rank, raw score) in list order.
    let mut signals: HashMap<String, Vec<(SignalId, Option<u32>, f32)>> = HashMap::new();
    for list in &lists {
        for (idx, (hash, score)) in list.images.iter().enumerate() {
            let rank = idx as f64 + 1.0;
            *fused.entry(hash.clone()).or_insert(0.0) += list.weight / (RRF_K + rank);
            signals.entry(hash.clone()).or_default().push((
                list.signal,
                Some(idx as u32 + 1),
                *score,
            ));
        }
    }

    let union: Vec<String> = fused.keys().cloned().collect();
    let last_ts = exec::fetch_last_annotated(conn, &union)?;
    let mut order: Vec<(String, f64)> = fused.into_iter().collect();
    // §5.3 tie-break: equal fused scores order by the image's last live
    // annotation recency (most recent first), then image hash.
    order.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| last_ts.get(&b.0).cmp(&last_ts.get(&a.0)))
            .then_with(|| a.0.cmp(&b.0))
    });
    order.truncate(FUSED_LIMIT);

    // Stage 4 — results with best-quote provenance (§5.4 selection chain:
    // S1 chunk span → S2 snippet → S3 re-run against the image's own
    // events → S4 visual match; a summary is never quoted).
    let s1_events: Vec<String> = order
        .iter()
        .filter_map(|(hash, _)| s1_best.get(hash).map(|b| b.event_id.clone()))
        .collect();
    let provenance_inputs = exec::provenance_inputs(conn, &s1_events)?;
    let s3_ranked_set: HashSet<&String> = lists[2]
        .images
        .iter()
        .chain(lists[3].images.iter())
        .map(|(h, _)| h)
        .collect();
    let s4_ranked_set: HashSet<&String> = lists[4].images.iter().map(|(h, _)| h).collect();

    let mut images = Vec::with_capacity(order.len());
    for (hash, fused_score) in &order {
        let s1_quote = s1_best
            .get(hash)
            .and_then(|b| provenance_inputs.quote_span(&b.event_id, b.char_start, b.char_end));
        let provenance = if let Some(q) = s1_quote {
            Some(Provenance::Quote(q))
        } else if let Some(r) = s2_results.get(hash) {
            Some(r.provenance.clone())
        } else if s3_ranked_set.contains(hash) {
            let requeried = match &match_q {
                Some(mq) => exec::best_quote_for_image(conn, mq, &keywords, hash)?,
                None => None,
            };
            requeried.map(Provenance::Quote).or_else(|| {
                s4_ranked_set
                    .contains(hash)
                    .then_some(Provenance::VisualMatch)
            })
        } else if s4_ranked_set.contains(hash) {
            Some(Provenance::VisualMatch)
        } else {
            None
        };
        let Some(provenance) = provenance else {
            // §13.2: a result without a human-readable explanation must
            // not render. An image ranked only by derived prose with no
            // recoverable event-level evidence is dropped, not faked.
            tracing::debug!(image = %hash, "fused candidate dropped: no quotable provenance");
            continue;
        };
        let content_hash =
            ContentHash::from_hex(hash).map_err(|e| SearchError::Corrupt(e.to_string()))?;
        let debug = opts.include_debug.then(|| DebugScores {
            per_signal: signals.get(hash).cloned().unwrap_or_default(),
            fused: *fused_score as f32,
        });
        images.push(ImageResult {
            preview: PreviewRef {
                image_hash: content_hash.clone(),
            },
            image_hash: content_hash,
            score: *fused_score as f32,
            provenance,
            last_annotated_ts: last_ts.get(hash).copied(),
            debug,
        });
    }

    Ok(SearchResults {
        query: echo(&parsed),
        images,
        session_hits: base.session_hits,
    })
}
