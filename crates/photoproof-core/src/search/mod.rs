//! M1 search: FTS5 query construction, structured filters, materialize-first
//! execution, result grouping + provenance.
//!
//! Contract: spec/RETRIEVAL.md §4 (M1), §5.1 Filter AST, §5.4 result contract
//! (M1 subset), §6 provenance, §7.2 worked example, §13 acceptance criteria.
//! Owned by work packet P3.1.
//!
//! Boundaries: the `event_fts`/`fts_map` construction is EVENTS §5.4's and is
//! maintained by the store; this module only queries it. The M1 subset of the
//! §5.4 result contract is implemented with the crate's identity primitives
//! standing in for the spec sketch's `Ulid`/`DateTime<Utc>`:
//! [`EventId`]/[`SessionId`]/[`UtcMillis`]/[`ContentHash`].
//!
//! Flagged readings (build-loop discipline; see the packet report):
//! - The §4 statement's `ORDER BY s` (alias of `bm25(event_fts)`) is not
//!   consumed by FTS5's `xBestIndex`, which forces a temp B-tree sort and
//!   evaluates `snippet()` for every candidate row pre-LIMIT — the exact
//!   failure §4 and §13.11 forbid (measured: 20 000 snippet calls vs 500).
//!   The executed statement therefore orders by the FTS5 `rank` column,
//!   whose default rank function *is* bm25 — identical ordering, snippet
//!   bounded by the LIMIT. Everything else in the statement is verbatim.
//! - Empty query + a `HasStrokes(true)` chip yields `Provenance::Stroke`
//!   (the image matched via stroke evidence, §5.4/§6); all other filter-only
//!   browses yield `Provenance::FilterOnly`.

mod exec;
mod filter;
mod query;
mod snippet;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, InterruptHandle};
use thiserror::Error;

use crate::id::{ContentHash, EventId, SessionId, UtcMillis};
use crate::store::schema;

pub use query::fts_match_query;
pub use snippet::render_with_sentinels;

/// Spec name for the event source in filter/result contracts
/// (RETRIEVAL §5.1, §5.4): voice | typed | pencil | system.
pub use crate::event::Source as EventSource;

/// Spec name for the event kind in the filter AST (RETRIEVAL §5.1).
pub use crate::event::Kind as EventKind;

// ---------------------------------------------------------------------------
// §5.1 — the Filter AST (normative Rust; full type, M1 chip-subset execution)
// ---------------------------------------------------------------------------

/// Parsed query: typed filters + semantic remainder (RETRIEVAL §5.1). In M1
/// there is no parse LLM; chips construct `Filter` values directly and the
/// raw string is the FTS `keywords` remainder.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedQuery {
    /// Hard WHERE constraints — filters filter, never rank.
    pub filters: Vec<Filter>,
    /// Remainder for embedding search (M3; always `None` in M1).
    pub semantic: Option<String>,
    /// Remainder for FTS; not in the LLM JSON — set = semantic post-parse.
    pub keywords: Option<String>,
    /// Query asks about image content, not words about it (M3; false in M1).
    pub visual: bool,
    /// Validation rejects, for the debug panel (M3; empty in M1).
    pub dropped: Vec<DroppedClause>,
    /// True if parse failed/timed out (§5.1 fallback; false in M1).
    pub fallback: bool,
}

/// One typed filter (RETRIEVAL §5.1). M1 executes the chip subset: date
/// range, camera, lens, folder/root, rating, has-strokes, source,
/// online/offline. `Project` and `Kind` are M3 (`SearchError::UnsupportedFilter`).
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Date {
        field: DateField,
        range: DateRange,
    },
    Camera(StringMatch),
    Lens(StringMatch),
    /// Subtree of a watched root.
    Folder(PathMatch),
    /// Watched-root name.
    Root(String),
    /// Folded current rating, 0..=5 (reads `image_ratings`; E4: `value:0`
    /// is an explicit zero — unrated images match no rating filter).
    Rating(Comparison),
    /// Resolved against the project store, §10 (M3).
    Project(ProjectRef),
    Volume(VolumeFilter),
    /// Reads `image_journal_stats.has_strokes` — never a stroke-event fold
    /// at query time (§4, P5).
    HasStrokes(bool),
    /// voice | typed | pencil | system.
    Source(Vec<EventSource>),
    /// remark | rating | stroke | … (M3).
    Kind(Vec<EventKind>),
}

/// EXIF capture ts vs. event ts; default Captured (RETRIEVAL §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Captured,
    Annotated,
}

/// Half-open `[start, end)`; relative ranges resolve against `now` at
/// execution time (RETRIEVAL §5.1).
#[derive(Debug, Clone, PartialEq)]
pub enum DateRange {
    Absolute {
        start: Option<UtcMillis>,
        end: Option<UtcMillis>,
    },
    Relative(RelativeRange),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelativeRange {
    LastDays(u32),
    LastWeeks(u32),
    LastMonths(u32),
    LastYears(u32),
    /// "last winter" = `Winter, years_ago: 1`.
    Season {
        season: Season,
        years_ago: u32,
    },
    /// "in March", "March 2024".
    Month {
        month: u8,
        year: Option<i32>,
    },
    Year(i32),
}

/// N-hemisphere months; Winter spans the year boundary (Dec 1 – Mar 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

/// Case-insensitive (RETRIEVAL §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringMatch {
    Exact(String),
    Contains(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathMatch {
    Subtree(String),
    NameContains(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Eq(u8),
    Gte(u8),
    Lte(u8),
    Between(u8, u8),
}

/// §10.3 fuzzy resolution (M3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    pub raw: String,
    pub resolved: Option<ulid::Ulid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeFilter {
    Online,
    Offline,
    Named(String),
}

/// A clause the validation firewall rejected (RETRIEVAL §5.1; M3 — empty in
/// M1, present in the type so the contract is stable).
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedClause {
    pub raw: serde_json::Value,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// §5.4 — the result contract (M1 subset; UI renders this, never raw rows)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResults {
    /// Raw string + `ParsedQuery` (incl. dropped, fallback).
    pub query: QueryEcho,
    /// M1 order: best (lowest) bm25 first; filter-only browse: capture date
    /// descending.
    pub images: Vec<ImageResult>,
    /// Session-level remark matches — a separate list, never attributed to
    /// images (R4).
    pub session_hits: Vec<SessionHit>,
}

/// The query as echoed back to the UI (§5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryEcho {
    pub raw: String,
    pub parsed: ParsedQuery,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageResult {
    pub image_hash: ContentHash,
    /// Cache key (LIBRARY.md §9.8); the shell resolves it to artifact paths.
    pub preview: PreviewRef,
    /// M1: the image's best (lowest) bm25 among its hits — ascending is
    /// best-first. Filter-only browse rows carry 0.0. (M3 replaces this with
    /// the fused RRF score.)
    pub score: f32,
    /// §6 — REQUIRED, never absent.
    pub provenance: Provenance,
    pub last_annotated_ts: Option<UtcMillis>,
    /// Dev builds only; `None` unless requested via [`SearchOptions`].
    pub debug: Option<DebugScores>,
}

/// Preview-cache key (LIBRARY.md): previews are keyed by image hash; the
/// artifact kind (`thumb`/`display`) is the renderer's choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRef {
    pub image_hash: ContentHash,
}

/// Why a result matched (§5.4/§6). M1 subset: no `VisualMatch` (S4 is M3).
#[derive(Debug, Clone, PartialEq)]
pub enum Provenance {
    /// The BEST matching span of the user's own words.
    Quote(Quote),
    /// Image matched via has-strokes / stroke-only evidence.
    Stroke {
        event_id: EventId,
        session_id: SessionId,
        ts: UtcMillis,
    },
    /// Pure structured-filter query.
    FilterOnly,
}

/// The `Provenance::Quote` fields (§5.4), shared with [`SessionHit`].
/// Offsets are Unicode-scalar (char) counts, per RETRIEVAL §1.2.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub ts: UtcMillis,
    /// voice | typed (remark roots only).
    pub source: EventSource,
    /// Exact folded-text span (snippet window) — verbatim user words.
    pub text: String,
    /// Span within the event's folded text.
    pub char_start: u32,
    pub char_end: u32,
    /// Matched-term ranges within `text` (FTS hits, sentinel-mapped).
    pub highlights: Vec<(u32, u32)>,
    /// Stroke event drawn with these words, if any (X2: resolved in both
    /// link directions).
    pub linked_stroke: Option<EventId>,
}

/// A session-level remark match (zero image targets) — §5.4.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionHit {
    pub session_id: SessionId,
    pub quote: Quote,
}

/// Dev-build debug panel only (§5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct DebugScores {
    /// (signal, 1-based rank within the signal, raw score).
    pub per_signal: Vec<(SignalId, Option<u32>, f32)>,
    pub fused: f32,
}

/// Candidate-generation signals (§5.2). M1 populates only `S2EventFts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalId {
    S1AnnotationChunk,
    S2EventFts,
    S3Summaries,
    S4ImageClip,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("sqlite error: {0}")]
    Sqlite(rusqlite::Error),
    /// The in-flight query was cancelled via [`Searcher::interrupt`]
    /// (`sqlite3_interrupt`) — expected during search-as-you-type.
    #[error("search interrupted")]
    Interrupted,
    /// Filters are hard constraints; a filter that cannot execute in M1
    /// (`Project`, `Kind`) errors rather than being silently dropped.
    #[error("filter not executable in M1: {0}")]
    UnsupportedFilter(&'static str),
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

impl From<rusqlite::Error> for SearchError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, _) = &e
            && f.code == rusqlite::ErrorCode::OperationInterrupted
        {
            return SearchError::Interrupted;
        }
        SearchError::Sqlite(e)
    }
}

// ---------------------------------------------------------------------------
// Searcher
// ---------------------------------------------------------------------------

/// Per-search knobs. `now` anchors relative date ranges (§5.1: resolved
/// against `now` at execution time); `include_debug` populates
/// [`ImageResult::debug`] (dev builds only — the shell must not expose it in
/// release, §5.4).
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub now: Option<UtcMillis>,
    pub include_debug: bool,
}

/// M1 search over the shared photoproof database (RETRIEVAL §4): one
/// read connection; queries cancellable from any thread via
/// [`Searcher::interrupt`] (`sqlite3_interrupt`) for search-as-you-type.
/// The 100 ms debounce is UI-side.
pub struct Searcher {
    conn: Mutex<Connection>,
    interrupt: InterruptHandle,
}

impl Searcher {
    /// Open (creating/migrating if necessary) the shared database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SearchError> {
        let conn = schema::open_connection(path.as_ref())?;
        schema::migrate(&conn)?;
        let interrupt = conn.get_interrupt_handle();
        Ok(Self {
            conn: Mutex::new(conn),
            interrupt,
        })
    }

    /// Cancel the in-flight query, if any (`sqlite3_interrupt`). Safe from
    /// any thread; the cancelled `search` returns
    /// [`SearchError::Interrupted`]. A call with no query in flight is a
    /// no-op (it does not affect later queries).
    pub fn interrupt(&self) {
        self.interrupt.interrupt();
    }

    /// §4 search: query string + filter chips. Empty/whitespace query with
    /// filters = filter-only browse (capture date descending).
    pub fn search(
        &self,
        raw_query: &str,
        filters: &[Filter],
    ) -> Result<SearchResults, SearchError> {
        self.search_with(raw_query, filters, &SearchOptions::default())
    }

    /// [`Searcher::search`] with explicit options.
    pub fn search_with(
        &self,
        raw_query: &str,
        filters: &[Filter],
        opts: &SearchOptions,
    ) -> Result<SearchResults, SearchError> {
        let now = opts.now.unwrap_or_else(UtcMillis::now);
        let mut conn = self.conn.lock().expect("searcher mutex poisoned");
        // One read snapshot for the statement sequence (WAL).
        let tx = conn.transaction().map_err(SearchError::from)?;
        let result = exec::run_search(&tx, raw_query, filters, now, opts.include_debug);
        drop(tx); // read-only; rollback
        result
    }
}

/// The §4 image-hit statement (and its filter parameters, which bind after
/// the `?1` MATCH string) exactly as [`Searcher::search`] executes it.
/// Test surface for the §13.11 plan-shape gate; not part of the public API.
#[doc(hidden)]
pub fn image_hits_sql_for_tests(
    filters: &[Filter],
    now: UtcMillis,
) -> Result<(String, Vec<rusqlite::types::Value>), SearchError> {
    let cf = filter::compile(filters, filter::FilterMode::Hits, now)?;
    Ok((exec::image_hits_sql(&cf), cf.params))
}
