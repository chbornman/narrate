//! Fuzzy metadata widening (search-as-scope Phase 4, the `~` quiet-toggle).
//!
//! WHAT: an OPT-IN, typo-tolerant pass over the small metadata column space —
//! camera (`images.camera_model` + `images.camera_make`), lens
//! (`images.lens_model`), and filename (the basename of `paths.rel_path`). It
//! runs ON the lexical lane only, AFTER the exact FTS hits, and only widens:
//! it appends images whose gear/filename FUZZILY matches a query token,
//! de-duplicated against the exact set, never reordering it.
//!
//! WHY it stays inside the <100 ms keystroke budget (RETRIEVAL §13.1): the
//! distinct metadata-value space is TINY and low-cardinality compared to the
//! full annotation-text corpus the FTS path scans. A real library has on the
//! order of dozens of distinct camera/lens strings (one per body/lens a
//! photographer owns) and a bounded set of filename stems. We pull the DISTINCT
//! values once (an indexed/grouped scan, not a per-row edit-distance), then run
//! Levenshtein in Rust over those few hundred SHORT strings — microseconds, not
//! the FTS path's full-corpus work. The expensive part (resolving a matched
//! value back to image hashes) is an indexed equality/`LIKE` lookup, and the
//! whole pass is skipped entirely unless the caller armed `fuzzy`.
//!
//! WHY edit distance (not a trigram index or SQLite extension): SQLite ships no
//! `editdist`/Levenshtein, and a trigram table would be a new index to maintain
//! for a feature that is off by default. `strsim` (already in the tree for
//! §10.3 collection-name resolution) gives us exactly the Levenshtein we need
//! over the tiny distinct-value set with ZERO new schema. Simplicity wins: the
//! whole feature is additive and reversible, so the cheapest correct widening
//! is the right one.

use std::collections::HashSet;

use rusqlite::Connection;
use rusqlite::types::Value;
use strsim::levenshtein;

use crate::id::UtcMillis;

use super::filter::{CompiledFilters, FilterMode, compile};
use super::{Filter, SearchError};

/// Which metadata field a fuzzy match came from — carried into the result's
/// provenance so the UI can label an approximate hit honestly (§6: a result
/// never lies about WHY it matched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzyField {
    Camera,
    Lens,
    Filename,
}

impl FuzzyField {
    /// Stable wire token for the IPC DTO + the UI's "approximate" provenance.
    pub fn as_str(self) -> &'static str {
        match self {
            FuzzyField::Camera => "camera",
            FuzzyField::Lens => "lens",
            FuzzyField::Filename => "filename",
        }
    }
}

/// One fuzzy metadata hit: the image and which field's value matched.
pub(crate) struct FuzzyHit {
    pub image_hash: String,
    pub field: FuzzyField,
}

/// Minimum query-token length to fuzzy-match on. A 1–2 char token is almost
/// all noise at any edit distance (every short gear string is "close" to "x5"),
/// so the widening would drown the exact set rather than help it.
const MIN_TOKEN_LEN: usize = 3;

/// Edit-distance budget as a function of token length: short tokens tolerate
/// one typo, longer ones two. Scaling with length keeps the widening tight —
/// "Levia" → "Leica" (dist 1) matches, but unrelated short strings do not.
fn distance_budget(token_len: usize) -> usize {
    match token_len {
        0..=4 => 1,
        _ => 2,
    }
}

/// Does `value` fuzzily contain `token`? True when ANY whitespace/punctuation
/// token of `value` is within the length-scaled edit-distance budget of the
/// query token (both lowercased). A direct substring is always accepted (a
/// distance-0 containment) so an exact partial like "leica" in "Leica M11"
/// still widens even when no single sub-token equals it.
fn token_matches_value(token: &str, value_lc: &str) -> bool {
    // Substring containment is the zero-cost common case (exact partial gear
    // names): accept it before paying for any edit-distance work.
    if value_lc.contains(token) {
        return true;
    }
    let budget = distance_budget(token.chars().count());
    value_lc
        // Split on non-alphanumerics so "RF 50mm" / "23mm f/2" tokenize sanely.
        .split(|c: char| !c.is_alphanumeric())
        .filter(|sub| !sub.is_empty())
        .any(|sub| {
            // Skip sub-tokens whose length is too far off to ever be within
            // budget — Levenshtein is bounded below by the length delta, so
            // this prunes the obvious misses before the O(n·m) DP runs.
            let (a, b) = (token.chars().count(), sub.chars().count());
            a.abs_diff(b) <= budget && levenshtein(token, sub) <= budget
        })
}

/// The query's fuzzy-eligible tokens: whitespace split, lowercased, short
/// tokens dropped. Empty when the query has nothing worth widening on.
fn fuzzy_tokens(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| t.chars().count() >= MIN_TOKEN_LEN)
        .collect()
}

/// The additive fuzzy-metadata pass. Returns image hashes that fuzzily match a
/// query token on camera/lens/filename, within the filtered universe (the same
/// hard constraints the exact lane applies, Browse mode). The caller appends
/// these AFTER the exact FTS images, de-duplicated against them — this function
/// neither orders against nor knows about the exact set.
///
/// `already` is the exact set's hashes: a hash already present is skipped here
/// so the exact result keeps its place and provenance (exact never demoted to
/// fuzzy). Returns at most [`super::exec::HIT_LIMIT`] hits — the same defensive
/// page bound the exact lane uses.
pub(crate) fn fuzzy_metadata_hits(
    conn: &Connection,
    raw: &str,
    filters: &[Filter],
    now: UtcMillis,
    already: &HashSet<String>,
) -> Result<Vec<FuzzyHit>, SearchError> {
    let tokens = fuzzy_tokens(raw);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    // Filters are hard constraints even in the widened lane: compile them once
    // (Browse mode — image-level constraints against `images i`) and reuse the
    // WHERE/params for every value-resolution query below.
    let cf = compile(filters, FilterMode::Browse, now)?;

    let mut hits: Vec<FuzzyHit> = Vec::new();
    let mut seen: HashSet<String> = already.clone();

    // Camera: distinct make+model strings (a handful per library). A token
    // matching EITHER make or model fuzzily counts as a camera match.
    fuzzy_field(
        conn,
        &cf,
        &tokens,
        FuzzyField::Camera,
        "SELECT DISTINCT i.camera_make, i.camera_model FROM images i\n",
        &["i.camera_make", "i.camera_model"],
        &mut seen,
        &mut hits,
    )?;
    // Lens: distinct lens_model strings.
    fuzzy_field(
        conn,
        &cf,
        &tokens,
        FuzzyField::Lens,
        "SELECT DISTINCT i.lens_model FROM images i\n",
        &["i.lens_model"],
        &mut seen,
        &mut hits,
    )?;
    // Filename: the basename of each active path. A path's directory portion is
    // a Folder-filter concern (filter.rs); the fuzzy widening is over the
    // filename a photographer would actually search by.
    fuzzy_filename(conn, &cf, &tokens, &mut seen, &mut hits)?;

    hits.truncate(super::exec::HIT_LIMIT);
    Ok(hits)
}

/// Generic camera/lens pass: scan the DISTINCT column values under the filter
/// WHERE, keep those that fuzzily match any token, then resolve each surviving
/// value back to its image hashes (indexed equality) under the same filter.
#[allow(clippy::too_many_arguments)]
fn fuzzy_field(
    conn: &Connection,
    cf: &CompiledFilters,
    tokens: &[String],
    field: FuzzyField,
    distinct_sql: &str,
    cols: &[&str],
    seen: &mut HashSet<String>,
    out: &mut Vec<FuzzyHit>,
) -> Result<(), SearchError> {
    // Collect the distinct (per-column) values that fuzzily match — small set.
    let mut matched: Vec<(usize, String)> = Vec::new();
    {
        let sql = with_filter(distinct_sql, cf);
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(cf.params.iter().cloned()))?;
        while let Some(row) = rows.next()? {
            for (ci, _col) in cols.iter().enumerate() {
                let v: Option<String> = row.get(ci)?;
                let Some(v) = v else { continue };
                let v_lc = v.to_lowercase();
                if tokens.iter().any(|t| token_matches_value(t, &v_lc)) {
                    matched.push((ci, v));
                }
            }
        }
    }
    // Resolve each matched value to its hashes. Equality on the column (NOCASE)
    // is an indexed lookup; the filter WHERE keeps the universe constrained.
    for (ci, value) in matched {
        let sql = with_filter(
            &format!(
                "SELECT i.image_hash FROM images i\nWHERE {} = ? COLLATE NOCASE\n",
                cols[ci]
            ),
            cf,
        );
        let mut params: Vec<Value> = vec![Value::Text(value)];
        params.extend(cf.params.iter().cloned());
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let hash: String = row.get(0)?;
            if seen.insert(hash.clone()) {
                out.push(FuzzyHit {
                    image_hash: hash,
                    field,
                });
            }
        }
    }
    Ok(())
}

/// Filename pass: scan distinct active-path basenames, keep fuzzy matches,
/// resolve each back to its image hashes. The basename is everything after the
/// last `/` (rel_path has no leading slash, §6 LIBRARY).
fn fuzzy_filename(
    conn: &Connection,
    cf: &CompiledFilters,
    tokens: &[String],
    seen: &mut HashSet<String>,
    out: &mut Vec<FuzzyHit>,
) -> Result<(), SearchError> {
    // Distinct (hash, basename) over active paths in the filtered universe. We
    // carry the hash alongside so a fuzzy basename hit resolves without a
    // second lookup — paths are 1:1 enough that the distinct set stays small,
    // and this avoids a LIKE re-scan per matched name.
    let sql = with_filter_paths(
        "SELECT DISTINCT p.image_hash, \
         CASE WHEN instr(p.rel_path, '/') = 0 THEN p.rel_path \
              ELSE replace(p.rel_path, rtrim(p.rel_path, replace(p.rel_path, '/', '')), '') END \
         FROM paths p\nWHERE p.state = 'active'\n",
        cf,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(cf.params.iter().cloned()))?;
    while let Some(row) = rows.next()? {
        let hash: String = row.get(0)?;
        if seen.contains(&hash) {
            continue;
        }
        let name: String = row.get(1)?;
        let name_lc = name.to_lowercase();
        if tokens.iter().any(|t| token_matches_value(t, &name_lc)) && seen.insert(hash.clone()) {
            out.push(FuzzyHit {
                image_hash: hash,
                field: FuzzyField::Filename,
            });
        }
    }
    Ok(())
}

/// Append the compiled filter's image-level WHERE clauses to a statement whose
/// driving table is `images i` (camera/lens passes). The filter compile already
/// emits `EXISTS (... img(Browse) ...)` against `i.image_hash`.
fn with_filter(base: &str, cf: &CompiledFilters) -> String {
    append_wheres(base, &cf.wheres)
}

/// Same, for the filename pass whose driving table is `paths p` but whose
/// filter WHERE clauses reference `i.image_hash` (Browse compile). They must
/// hang off a correlated `images i` for the image-level constraints to bind, so
/// wrap them as an `EXISTS` over the path's image.
fn with_filter_paths(base: &str, cf: &CompiledFilters) -> String {
    if cf.wheres.is_empty() {
        return base.to_owned();
    }
    let mut sql = base.to_owned();
    sql.push_str("  AND EXISTS (SELECT 1 FROM images i WHERE i.image_hash = p.image_hash\n");
    for w in &cf.wheres {
        sql.push_str("       AND ");
        sql.push_str(w);
        sql.push('\n');
    }
    sql.push_str("  )\n");
    sql
}

fn append_wheres(base: &str, wheres: &[String]) -> String {
    let mut sql = base.to_owned();
    let mut first = !base.to_ascii_uppercase().contains("WHERE");
    for w in wheres {
        sql.push_str(if first { "WHERE " } else { "  AND " });
        sql.push_str(w);
        sql.push('\n');
        first = false;
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_budget_scales_with_length() {
        assert_eq!(distance_budget(3), 1);
        assert_eq!(distance_budget(4), 1);
        assert_eq!(distance_budget(5), 2);
        assert_eq!(distance_budget(10), 2);
    }

    #[test]
    fn token_matches_tolerate_one_typo() {
        // "Levia" → "Leica" is a single transposition-as-two-edits? No: l-e-v-i-a
        // vs l-e-i-c-a is edits at positions 3/4 = distance 2, within a 5-char
        // budget. A single-char typo always matches.
        assert!(token_matches_value("leics", "leica m11")); // 1 edit
        assert!(token_matches_value("summilx", "summilux 35")); // 1 deletion
        // Exact substring always matches (distance-0 containment).
        assert!(token_matches_value("leica", "leica m11"));
        // Unrelated strings do not match.
        assert!(!token_matches_value("nikon", "leica m11"));
        assert!(!token_matches_value("canon", "summilux 35"));
    }

    #[test]
    fn short_tokens_are_dropped() {
        // "z8" is below MIN_TOKEN_LEN (2 < 3) and dropped; "m11" is exactly 3
        // and kept — a 1-2 char token is all noise at any edit distance.
        assert_eq!(fuzzy_tokens("z8 m11"), vec!["m11"]);
        assert!(fuzzy_tokens("z8  x").is_empty());
        assert_eq!(fuzzy_tokens("Leica zorp"), vec!["leica", "zorp"]);
    }
}
