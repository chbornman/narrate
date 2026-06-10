//! §4 execution: the materialize-first statement, grouping, and provenance
//! assembly.
//!
//! The statement shape is normative (RETRIEVAL §4): a MATERIALIZED CTE
//! resolves the MATCH before any join (the documented 650× planner trap);
//! `snippet()` is evaluated only over the LIMITed hit set; the joins run
//! through `fts_map` → `event_targets` → images/paths. Provenance text is
//! re-read from the truth tables at extraction time (§6 defense in depth) —
//! a stale index row cannot resurrect scrubbed bytes.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::event::Source;
use crate::id::{ContentHash, EventId, SessionId, UtcMillis};

use super::filter::{CompiledFilters, FilterMode, compile};
use super::query::{fts_match_query, highlight_terms};
use super::snippet::map_snippet;
use super::{
    DebugScores, Filter, ImageResult, ParsedQuery, PreviewRef, Provenance, QueryEcho, Quote,
    SearchError, SearchResults, SessionHit, SignalId,
};

/// §4 normative page bound.
const HIT_LIMIT: usize = 500;
/// Defensive IN-list chunking (mirrors the store's discipline).
const IN_LIST_CHUNK: usize = 30_000;

/// The §4 statement. `?1` is the MATCH string; filter params bind after it
/// in clause order. `ORDER BY rank` (default rank = bm25) keeps the ordering
/// inside FTS5's xBestIndex so `snippet()` stays lazy — see the module-level
/// note in `search/mod.rs`.
///
/// The MATCH is resolved exactly once per keystroke: when no image-scoped
/// filter is present, the join through `event_targets` is a LEFT JOIN and a
/// NULL `image_hash` row *is* a session-level hit (zero-target root) routed
/// to the separate `session_hits` list — re-running the materialized MATCH
/// in a second statement would double the dominant cost for broad prefixes.
/// With an image-scoped filter the inner join (the literal §4 form) runs and
/// session hits are skipped: a zero-target event cannot satisfy an image
/// constraint (R4).
pub(crate) fn image_hits_sql(cf: &CompiledFilters) -> String {
    let mut sql = String::from(
        "WITH hits AS MATERIALIZED (\n\
         \x20 SELECT event_fts.rowid AS fts_rowid,\n\
         \x20        bm25(event_fts) AS s,\n\
         \x20        snippet(event_fts, 0, '⟦', '⟧', '…', 12) AS snip\n\
         \x20 FROM event_fts\n\
         \x20 WHERE event_fts MATCH ?1\n\
         \x20 ORDER BY rank\n\
         \x20 LIMIT 500\n\
         )\n\
         SELECT m.root_event_id, t.image_hash, h.s, h.snip\n\
         FROM hits h\n\
         JOIN fts_map m       ON m.fts_rowid = h.fts_rowid\n",
    );
    if cf.image_scoped {
        sql.push_str("JOIN event_targets t ON t.event_id  = m.root_event_id\n");
    } else {
        sql.push_str("LEFT JOIN event_targets t ON t.event_id = m.root_event_id\n");
    }
    if cf.join_event {
        sql.push_str("JOIN annotation_events e ON e.id = m.root_event_id\n");
    }
    if cf.join_images {
        sql.push_str("JOIN images i ON i.image_hash = t.image_hash\n");
    }
    push_where(&mut sql, &cf.wheres);
    sql
}

/// Filter-only browse (§4): results ordered by capture date descending
/// (NULL capture dates last), image hash as the deterministic tiebreak.
fn browse_sql(cf: &CompiledFilters) -> String {
    let mut sql = String::from("SELECT i.image_hash FROM images i\n");
    push_where(&mut sql, &cf.wheres);
    sql.push_str("ORDER BY i.capture_ts DESC, i.image_hash ASC\nLIMIT 500");
    sql
}

fn push_where(sql: &mut String, wheres: &[String]) {
    let mut first = true;
    for w in wheres {
        sql.push_str(if first { "WHERE " } else { "  AND " });
        sql.push_str(w);
        sql.push('\n');
        first = false;
    }
}

// ---------------------------------------------------------------------------

struct RawHit {
    root: String,
    s: f64,
    snip: String,
}

pub(crate) fn run_search(
    conn: &Connection,
    raw: &str,
    filters: &[Filter],
    now: UtcMillis,
    include_debug: bool,
) -> Result<SearchResults, SearchError> {
    let match_q = fts_match_query(raw);
    let echo = QueryEcho {
        raw: raw.to_owned(),
        parsed: ParsedQuery {
            filters: filters.to_vec(),
            semantic: None,
            keywords: match_q.as_ref().map(|_| raw.trim().to_owned()),
            visual: false,
            dropped: Vec::new(),
            fallback: false,
        },
    };
    match match_q {
        Some(q) => fts_search(conn, echo, &q, raw, filters, now, include_debug),
        None => browse(conn, echo, filters, now),
    }
}

#[allow(clippy::too_many_arguments)]
fn fts_search(
    conn: &Connection,
    echo: QueryEcho,
    match_q: &str,
    raw: &str,
    filters: &[Filter],
    now: UtcMillis,
    include_debug: bool,
) -> Result<SearchResults, SearchError> {
    let cf = compile(filters, FilterMode::Hits, now)?;
    let sql = image_hits_sql(&cf);

    // One pass: image hits grouped per image, zero-target (NULL hash) rows
    // routed to the session-hit list (R4 — never attributed to an image).
    let mut per_image: HashMap<String, Vec<RawHit>> = HashMap::new();
    let mut session_raw: Vec<RawHit> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut params: Vec<Value> = Vec::with_capacity(1 + cf.params.len());
        params.push(Value::Text(match_q.to_owned()));
        params.extend(cf.params.iter().cloned());
        let mut rows = stmt.query(params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let root: String = row.get(0)?;
            let hash: Option<String> = row.get(1)?;
            let s: f64 = row.get(2)?;
            let snip: String = row.get(3)?;
            match hash {
                Some(hash) => per_image
                    .entry(hash)
                    .or_default()
                    .push(RawHit { root, s, snip }),
                None => session_raw.push(RawHit { root, s, snip }),
            }
        }
    }
    for hits in per_image.values_mut() {
        hits.sort_by(|a, b| a.s.total_cmp(&b.s));
    }
    session_raw.sort_by(|a, b| a.s.total_cmp(&b.s));

    // Provenance inputs for every root in the result set, batched: metadata
    // + liveness from the truth tables (§6 defense in depth), effective
    // (folded) text, linked strokes.
    let mut root_ids: Vec<String> = per_image
        .values()
        .flat_map(|v| v.iter().map(|h| h.root.clone()))
        .chain(session_raw.iter().map(|h| h.root.clone()))
        .collect();
    root_ids.sort();
    root_ids.dedup();
    let metas = fetch_root_meta(conn, &root_ids)?;
    let folded = fetch_effective_texts(conn, &root_ids, &metas)?;
    let linked = fetch_linked_strokes(conn, &root_ids, &metas)?;

    let (exact_terms, prefix_term) = highlight_terms(raw);

    // Group: image score = best (lowest) bm25 among its live hits; carry the
    // best live hit's snippet as the Quote.
    struct Grouped {
        hash: String,
        s: f64,
        quote: Quote,
    }
    let mut grouped: Vec<Grouped> = Vec::new();
    for (hash, hits) in &per_image {
        let best = hits.iter().find_map(|h| {
            build_quote(
                h,
                &metas,
                &folded,
                &linked,
                &exact_terms,
                prefix_term.as_deref(),
            )
        });
        if let Some((s, quote)) = best {
            grouped.push(Grouped {
                hash: hash.clone(),
                s,
                quote,
            });
        }
    }

    let hashes: Vec<String> = grouped.iter().map(|g| g.hash.clone()).collect();
    let last_ts = fetch_last_annotated(conn, &hashes)?;

    // Order: bm25 ascending; ties by last annotation recency (most recent
    // first, per the §5.3 tie-break family), then image hash (determinism).
    grouped.sort_by(|a, b| {
        a.s.total_cmp(&b.s)
            .then_with(|| {
                let la = last_ts.get(&a.hash);
                let lb = last_ts.get(&b.hash);
                lb.cmp(&la)
            })
            .then_with(|| a.hash.cmp(&b.hash))
    });

    let mut images = Vec::with_capacity(grouped.len());
    for (idx, g) in grouped.into_iter().enumerate() {
        let hash = ContentHash::from_hex(&g.hash).map_err(corrupt)?;
        let debug = include_debug.then(|| DebugScores {
            per_signal: vec![(SignalId::S2EventFts, Some(idx as u32 + 1), g.s as f32)],
            fused: g.s as f32,
        });
        images.push(ImageResult {
            preview: PreviewRef {
                image_hash: hash.clone(),
            },
            image_hash: hash,
            score: g.s as f32,
            provenance: Provenance::Quote(g.quote),
            last_annotated_ts: last_ts.get(&g.hash).copied(),
            debug,
        });
    }

    let mut session_hits = Vec::new();
    for h in &session_raw {
        if let Some((_, quote)) = build_quote(
            h,
            &metas,
            &folded,
            &linked,
            &exact_terms,
            prefix_term.as_deref(),
        ) {
            session_hits.push(SessionHit {
                session_id: quote.session_id.clone(),
                quote,
            });
        }
    }

    Ok(SearchResults {
        query: echo,
        images,
        session_hits,
    })
}

fn browse(
    conn: &Connection,
    echo: QueryEcho,
    filters: &[Filter],
    now: UtcMillis,
) -> Result<SearchResults, SearchError> {
    let cf = compile(filters, FilterMode::Browse, now)?;
    let sql = browse_sql(&cf);
    let mut hashes: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(params_from_iter(cf.params.iter().cloned()))?;
        while let Some(row) = rows.next()? {
            hashes.push(row.get(0)?);
        }
    }
    let last_ts = fetch_last_annotated(conn, &hashes)?;
    // Stroke evidence (§5.4/§6): a has-strokes chip means the image matched
    // via its strokes; reference the most recent live one.
    let strokes = if cf.stroke_evidence {
        fetch_best_strokes(conn, &hashes)?
    } else {
        HashMap::new()
    };

    let mut images = Vec::with_capacity(hashes.len().min(HIT_LIMIT));
    for h in &hashes {
        let hash = ContentHash::from_hex(h).map_err(corrupt)?;
        let provenance = match strokes.get(h) {
            Some((id, sid, ts)) => Provenance::Stroke {
                event_id: id.clone(),
                session_id: sid.clone(),
                ts: *ts,
            },
            None => Provenance::FilterOnly,
        };
        images.push(ImageResult {
            preview: PreviewRef {
                image_hash: hash.clone(),
            },
            image_hash: hash,
            score: 0.0,
            provenance,
            last_annotated_ts: last_ts.get(h).copied(),
            debug: None,
        });
    }
    Ok(SearchResults {
        query: echo,
        images,
        session_hits: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Provenance inputs (batched; truth tables only — §6)
// ---------------------------------------------------------------------------

struct RootMeta {
    session_id: SessionId,
    ts: UtcMillis,
    source: Source,
    text: Option<String>,
    linked_event: Option<String>,
    live: bool,
}

fn corrupt(e: impl std::fmt::Display) -> SearchError {
    SearchError::Corrupt(e.to_string())
}

/// Chunked `… IN (…)` execution.
fn for_in_chunks(
    conn: &Connection,
    ids: &[String],
    sql_for: impl Fn(&str) -> String,
    mut each: impl FnMut(&rusqlite::Row<'_>) -> Result<(), SearchError>,
) -> Result<(), SearchError> {
    for chunk in ids.chunks(IN_LIST_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = sql_for(&marks);
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(chunk.iter()))?;
        while let Some(row) = rows.next()? {
            each(row)?;
        }
    }
    Ok(())
}

/// Root metadata + liveness from the truth tables. A root that is scrubbed
/// or retracted (a stale index row — impossible under transactional
/// maintenance, checked anyway) is reported dead and its hit is skipped.
fn fetch_root_meta(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, RootMeta>, SearchError> {
    let mut out = HashMap::with_capacity(ids.len());
    for_in_chunks(
        conn,
        ids,
        |marks| {
            format!(
                "SELECT e.id, e.session_id, e.ts, e.source, e.text, e.linked_event, \
                        e.redacted_by IS NOT NULL, \
                        EXISTS (SELECT 1 FROM annotation_events r \
                                WHERE r.target_event = e.id AND r.kind = 'retraction') \
                 FROM annotation_events e WHERE e.id IN ({marks})"
            )
        },
        |row| {
            let id: String = row.get(0)?;
            let session: String = row.get(1)?;
            let ts: String = row.get(2)?;
            let source: String = row.get(3)?;
            let scrubbed: bool = row.get(6)?;
            let retracted: bool = row.get(7)?;
            out.insert(
                id,
                RootMeta {
                    session_id: SessionId::from_str_strict(&session).map_err(corrupt)?,
                    ts: UtcMillis::parse(&ts).map_err(corrupt)?,
                    source: Source::parse(&source)
                        .ok_or_else(|| corrupt(format!("source {source}")))?,
                    text: row.get(4)?,
                    linked_event: row.get(5)?,
                    live: !scrubbed && !retracted,
                },
            );
            Ok(())
        },
    )?;
    Ok(out)
}

/// Effective (folded) text overrides: the greatest-id live revision per
/// root chain (EVENTS §6.1), read from the truth tables — never from the
/// FTS index. Roots without a live revision quote their own text.
fn fetch_effective_texts(
    conn: &Connection,
    ids: &[String],
    metas: &HashMap<String, RootMeta>,
) -> Result<HashMap<String, String>, SearchError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    for_in_chunks(
        conn,
        ids,
        |marks| {
            format!(
                "WITH RECURSIVE chain(root_id, id) AS (\n\
                 \x20 SELECT e.id, e.id FROM annotation_events e WHERE e.id IN ({marks})\n\
                 \x20 UNION\n\
                 \x20 SELECT c.root_id, v.id\n\
                 \x20 FROM annotation_events v JOIN chain c ON v.target_event = c.id\n\
                 \x20 WHERE v.kind = 'revision'\n\
                 )\n\
                 SELECT c.root_id, v.text\n\
                 FROM chain c\n\
                 JOIN annotation_events v ON v.id = c.id\n\
                 WHERE v.kind = 'revision' AND v.redacted_by IS NULL AND v.text IS NOT NULL\n\
                 \x20 AND NOT EXISTS (SELECT 1 FROM annotation_events r\n\
                 \x20                 WHERE r.target_event = v.id AND r.kind = 'retraction')\n\
                 ORDER BY c.root_id ASC, v.id ASC"
            )
        },
        |row| {
            let root: String = row.get(0)?;
            let text: String = row.get(1)?;
            out.insert(root, text); // ascending id: last write wins
            Ok(())
        },
    )?;
    // Fill in roots that fold to their own text.
    for id in ids {
        if !out.contains_key(id)
            && let Some(m) = metas.get(id)
            && let Some(t) = &m.text
        {
            out.insert(id.clone(), t.clone());
        }
    }
    Ok(out)
}

/// `Quote.linked_stroke` (X2: the link lives on the later-committed event,
/// pointing backward — resolve both directions). Forward: the root's own
/// `linked_event` if it is a live stroke. Reverse: the earliest live stroke
/// linking back to the root.
fn fetch_linked_strokes(
    conn: &Connection,
    ids: &[String],
    metas: &HashMap<String, RootMeta>,
) -> Result<HashMap<String, EventId>, SearchError> {
    let mut out: HashMap<String, EventId> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }

    // Forward: remark → stroke.
    let forward: Vec<String> = metas
        .values()
        .filter_map(|m| m.linked_event.clone())
        .collect();
    let mut live_strokes: HashMap<String, EventId> = HashMap::new();
    if !forward.is_empty() {
        for_in_chunks(
            conn,
            &forward,
            |marks| {
                format!(
                    "SELECT e.id FROM annotation_events e \
                     WHERE e.id IN ({marks}) AND e.kind = 'stroke' \
                     AND e.redacted_by IS NULL \
                     AND NOT EXISTS (SELECT 1 FROM annotation_events r \
                                     WHERE r.target_event = e.id AND r.kind = 'retraction')"
                )
            },
            |row| {
                let id: String = row.get(0)?;
                live_strokes.insert(id.clone(), EventId::from_str_strict(&id).map_err(corrupt)?);
                Ok(())
            },
        )?;
    }
    for (root, m) in metas {
        if let Some(l) = &m.linked_event
            && let Some(stroke) = live_strokes.get(l)
        {
            out.insert(root.clone(), stroke.clone());
        }
    }

    // Reverse: stroke → remark (earliest linking stroke wins, matching the
    // fold's reverse-link resolution).
    for_in_chunks(
        conn,
        ids,
        |marks| {
            format!(
                "SELECT e.linked_event, e.id FROM annotation_events e \
                 WHERE e.linked_event IN ({marks}) AND e.kind = 'stroke' \
                 AND e.redacted_by IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM annotation_events r \
                                 WHERE r.target_event = e.id AND r.kind = 'retraction') \
                 ORDER BY e.id ASC"
            )
        },
        |row| {
            let root: String = row.get(0)?;
            let id: String = row.get(1)?;
            out.entry(root)
                .or_insert(EventId::from_str_strict(&id).map_err(corrupt)?);
            Ok(())
        },
    )?;
    Ok(out)
}

fn fetch_last_annotated(
    conn: &Connection,
    hashes: &[String],
) -> Result<HashMap<String, UtcMillis>, SearchError> {
    let mut out = HashMap::with_capacity(hashes.len());
    if hashes.is_empty() {
        return Ok(out);
    }
    for_in_chunks(
        conn,
        hashes,
        |marks| {
            format!(
                "SELECT image_hash, last_ts FROM image_journal_stats \
                 WHERE image_hash IN ({marks})"
            )
        },
        |row| {
            let h: String = row.get(0)?;
            let ts: String = row.get(1)?;
            out.insert(h, UtcMillis::parse(&ts).map_err(corrupt)?);
            Ok(())
        },
    )?;
    Ok(out)
}

/// Most recent live (non-retracted, non-scrubbed) stroke per image, for
/// browse-mode `Provenance::Stroke` (B4's has-strokes liveness).
fn fetch_best_strokes(
    conn: &Connection,
    hashes: &[String],
) -> Result<HashMap<String, (EventId, SessionId, UtcMillis)>, SearchError> {
    let mut out = HashMap::new();
    if hashes.is_empty() {
        return Ok(out);
    }
    for_in_chunks(
        conn,
        hashes,
        |marks| {
            format!(
                "SELECT t.image_hash, e.id, e.session_id, e.ts \
                 FROM event_targets t JOIN annotation_events e ON e.id = t.event_id \
                 WHERE t.image_hash IN ({marks}) AND e.kind = 'stroke' \
                 AND e.redacted_by IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM annotation_events r \
                                 WHERE r.target_event = e.id AND r.kind = 'retraction') \
                 ORDER BY e.id ASC"
            )
        },
        |row| {
            let h: String = row.get(0)?;
            let id: String = row.get(1)?;
            let sid: String = row.get(2)?;
            let ts: String = row.get(3)?;
            out.insert(
                h,
                (
                    EventId::from_str_strict(&id).map_err(corrupt)?,
                    SessionId::from_str_strict(&sid).map_err(corrupt)?,
                    UtcMillis::parse(&ts).map_err(corrupt)?,
                ),
            ); // ascending id: the last row per image is its newest stroke
            Ok(())
        },
    )?;
    Ok(out)
}

/// Build the Quote for one hit, or `None` when the root is dead (the hit is
/// skipped and the image falls back to its next-best hit). Returns the hit's
/// bm25 alongside, since a skipped best hit must not donate its score.
fn build_quote(
    hit: &RawHit,
    metas: &HashMap<String, RootMeta>,
    folded: &HashMap<String, String>,
    linked: &HashMap<String, EventId>,
    exact_terms: &[String],
    prefix_term: Option<&str>,
) -> Option<(f64, Quote)> {
    let meta = metas.get(&hit.root)?;
    if !meta.live {
        return None;
    }
    let folded_text = folded.get(&hit.root)?;
    let mapped = map_snippet(folded_text, &hit.snip, exact_terms, prefix_term);
    let event_id = EventId::from_str_strict(&hit.root).ok()?;
    Some((
        hit.s,
        Quote {
            event_id,
            session_id: meta.session_id.clone(),
            ts: meta.ts,
            source: meta.source,
            text: mapped.text,
            char_start: mapped.char_start,
            char_end: mapped.char_end,
            highlights: mapped.highlights,
            linked_stroke: linked.get(&hit.root).cloned(),
        },
    ))
}
