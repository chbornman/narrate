//! Search: FTS5 query construction, structured filters, materialize-first
//! execution, result grouping + provenance (M1, packet P3.1); plus the §5
//! M3 hybrid pipeline — LLM query parse with the validation firewall,
//! four-signal candidate generation, weighted-RRF fusion — in [`hybrid`] /
//! [`parse`] (packet P7.2, mock-verified).
//!
//! Contract: spec/RETRIEVAL.md §4 (M1), §5 (M3 pipeline), §5.1 Filter AST,
//! §5.4 result contract, §6 provenance, §7 worked examples, §10.3
//! collection resolution, §13 acceptance criteria.
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
mod fuzzy;
mod hybrid;
mod parse;
mod query;
mod snippet;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, InterruptHandle};
use thiserror::Error;

use crate::id::{ContentHash, EventId, SessionId, UtcMillis};
use crate::store::StoreError;
use crate::store::schema;

pub use fuzzy::FuzzyField;
pub use hybrid::{
    FusionWeights, HybridOptions, HybridRig, NoModel, RRF_K, SIM_BLEND_BETA, keyword_only_rig,
};
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

/// One typed filter (RETRIEVAL §5.1). Chips and the P7.2 parse stage emit
/// the same AST. `Collection` executes once its name is resolved (§10.3 —
/// [`Searcher::hybrid_search`] resolves; an unresolved ref errors rather
/// than silently dropping). `Kind` has no executor yet
/// (`SearchError::UnsupportedFilter`) — the §5.1 LLM grammar never emits it.
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
    /// Resolved against the collections store, §10 (M3).
    Collection(CollectionRef),
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
pub struct CollectionRef {
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

/// Why a result matched (§5.4/§6).
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
    /// image_clip evidence only (S4, hybrid pipeline) — labeled honestly,
    /// NO fake quote (§6).
    VisualMatch,
    /// Pure structured-filter query.
    FilterOnly,
    /// Fuzzy (typo-tolerant) metadata match from the `~` quiet-toggle —
    /// camera/lens/filename widening (search-as-scope Phase 4). Carried ONLY by
    /// images appended AFTER the exact set; an exact hit keeps its real
    /// provenance. Labeled honestly as APPROXIMATE so the UI never presents a
    /// widened gear/filename match as an exact one (§6: a result never lies
    /// about why it matched).
    FuzzyMeta { field: FuzzyField },
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

/// Candidate-generation signals (§5.2). Plain [`Searcher::search`]
/// populates only `S2EventFts`; the hybrid pipeline names, per result,
/// every signal that ranked it (`S3Summaries` may appear twice — once per
/// §5.3 sub-list).
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
    /// Filters are hard constraints; a filter that cannot execute (`Kind`,
    /// or a `Collection` whose name was never resolved against the
    /// collections store) errors rather than being silently dropped. The
    /// §5.1 validation firewall — which DOES drop, with debug visibility —
    /// runs before execution in [`Searcher::hybrid_search`].
    #[error("filter not executable: {0}")]
    UnsupportedFilter(&'static str),
    /// A `Filter::Collection` chip whose name resolved below the §10.3
    /// threshold. Chips are typed user intent, not model output: the §5.1
    /// drop-with-debug-visibility firewall is scoped to LLM hallucinations,
    /// so an unresolvable chip is a hard error — never a silently broadened
    /// query (a hard constraint never guesses, and never vanishes).
    #[error("collection '{raw}' not found: {reason}")]
    UnresolvedCollection { raw: String, reason: String },
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

impl From<StoreError> for SearchError {
    fn from(e: StoreError) -> Self {
        // The Searcher opens the SAME db the EventStore already migrated, so a
        // schema error here is unexpected; preserve the sqlite/interrupt mapping
        // and carry any other StoreError (e.g. IncompatibleVersion) as its text.
        match e {
            StoreError::Sqlite(s) => s.into(),
            other => SearchError::Corrupt(other.to_string()),
        }
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
    /// The `~` quiet-toggle (search-as-scope Phase 4): when true, append a
    /// typo-tolerant camera/lens/filename metadata pass AFTER the exact FTS
    /// hits (additive widening, never reordering). Default false =
    /// byte-identical to today's exact-only behavior.
    pub fuzzy: bool,
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
        let result = exec::run_search(&tx, raw_query, filters, now, opts.include_debug, opts.fuzzy);
        drop(tx); // read-only; rollback
        result
    }

    /// The §5 M3 query pipeline: parse → candidates → weighted-RRF fusion →
    /// results. M1 behavior is the degenerate case, not a separate system:
    /// with every rig slot `None` ([`keyword_only_rig`]) this returns
    /// exactly what [`Searcher::search`] returns, plus §10.3 collection-name
    /// resolution for `Filter::Collection` chips (the collections store
    /// needs no model). Search never errors because a model did — parse
    /// failures fall back (§5.1), and a failing vector signal degrades to
    /// absent with a logged warning (B69: signals are additive).
    pub fn hybrid_search<L, TE, CE>(
        &self,
        raw_query: &str,
        chips: &[Filter],
        rig: &HybridRig<'_, L, TE, CE>,
        opts: &HybridOptions,
    ) -> Result<SearchResults, SearchError>
    where
        L: photoproof_connectors::LanguageModel,
        TE: photoproof_connectors::Embedder,
        CE: photoproof_connectors::Embedder,
    {
        let now = opts.now.unwrap_or_else(UtcMillis::now);
        let trimmed = raw_query.trim();

        // Stage 1 (the parse-LLM call) runs OUTSIDE the connection mutex: a
        // hung model would otherwise wedge every later search — the
        // search-as-you-type `sqlite3_interrupt` path can only cancel SQL —
        // and pin a WAL read snapshot for its duration. The grounding lists
        // it needs are read under a short lock of their own; they are tiny
        // and a keystroke-stale copy cannot mis-execute anything, because
        // resolution and filter execution re-read the store inside the
        // query snapshot below.
        let grounding = if rig.llm.is_some() && !trimmed.is_empty() {
            let mut conn = self.conn.lock().expect("searcher mutex poisoned");
            let tx = conn.transaction().map_err(SearchError::from)?;
            let g = parse::load_grounding(&tx)?;
            drop(tx); // read-only; rollback
            Some(g)
        } else {
            None
        };
        let parsed = hybrid::stage1_parse(
            grounding.as_ref(),
            trimmed,
            rig.llm,
            rig.any_vector_signal(),
            now,
            opts.parse_budget,
        );

        let mut conn = self.conn.lock().expect("searcher mutex poisoned");
        // One read snapshot for the statement sequence (WAL).
        let tx = conn.transaction().map_err(SearchError::from)?;
        let result = hybrid::run(&tx, raw_query, parsed, chips, rig, opts, now);
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
