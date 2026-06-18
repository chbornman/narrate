//! The event store: append / fold / retraction / redaction scrub / merge,
//! with transactional FTS + derived-table maintenance.
//!
//! Contract: spec/EVENTS.md §5–10.

mod fold;
pub(crate) mod schema;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use thiserror::Error;

use crate::canonical::{self, CanonicalParseError};
use crate::event::{Event, Kind, Payload, Source, StrokePayload, ValidationError};
use crate::id::{ContentHash, EventId, Minted, Minter, SessionId, UtcMillis, validate_device_id};

pub use fold::JournalEntry;
use fold::{FoldSet, fold_entries};
// Re-exported so out-of-crate connections to the events file (the shell's
// debug-panel raw-tail reader) name the one spec-pinned EVENTS §5.1 value
// instead of a same-valued local that could silently diverge.
pub use schema::BUSY_TIMEOUT_MS;

// ---------------------------------------------------------------------------
// Errors and reports
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("corrupt row: {0}")]
    Corrupt(String),
    #[error("unknown session: {0}")]
    SessionNotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The database's `user_version` is NEWER than this build understands: a
    /// downgrade (older app opening a DB a newer app upgraded). Opening it would
    /// let the old code read a schema it only partly knows and silently corrupt
    /// derived data, so we refuse instead. Recovery: run the newer app, or
    /// restore/clear the database. (STATE-INTEGRITY-AUDIT.md: newer-version DB.)
    #[error(
        "database schema version {found} is newer than this app supports (max {supported}); \
         update PhotoProof to open this library"
    )]
    IncompatibleVersion { found: i64, supported: i64 },
    /// `PRAGMA wal_checkpoint(TRUNCATE)` stayed blocked by a concurrent
    /// reader after bounded retries (§5.1, §7 step 8). The triggering write
    /// is committed and durable; only the WAL hygiene is incomplete — retry
    /// via `maintain()` once long-lived readers have released.
    #[error(
        "wal_checkpoint(TRUNCATE) blocked by a concurrent reader; \
         freed bytes linger in the WAL until the next successful checkpoint"
    )]
    CheckpointBlocked,
    /// The redaction committed (text scrubbed, vectors marked dead) but
    /// zeroing the PPVEC flat-file bytes failed (RETRIEVAL §13.5). The
    /// deleted marks are durable, so the next embedding drain's
    /// `sweep_dead` retries the physical zero.
    #[error("vector flat-file zeroing failed (the drain's sweep retries it): {0}")]
    VectorScrub(String),
}

#[derive(Debug, Error)]
pub enum AppendError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("target event not found: {0}")]
    TargetNotFound(EventId),
    #[error("invalid target kind {target_kind:?} for {meta:?}")]
    InvalidTargetKind { meta: Kind, target_kind: Kind },
    #[error("retraction of a retraction is not supported in v1; re-state instead")]
    RetractionOfRetraction,
    #[error("duplicate event id: {0}")]
    DuplicateId(EventId),
    /// The id is in the redactions registry: redaction supremacy (§7) — no
    /// insert path may write content for a condemned id, append included.
    #[error(
        "event id {0} is condemned by the redactions registry (§7); content may not be written"
    )]
    CondemnedId(EventId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for AppendError {
    fn from(e: rusqlite::Error) -> Self {
        AppendError::Store(StoreError::Sqlite(e))
    }
}

#[derive(Debug, Error)]
pub enum RedactError {
    #[error("target event not found: {0}")]
    NotFound(EventId),
    #[error("kind {0:?} cannot be redacted")]
    InvalidKind(Kind),
    #[error("target already scrubbed: {0}")]
    AlreadyScrubbed(EventId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for RedactError {
    fn from(e: rusqlite::Error) -> Self {
        RedactError::Store(StoreError::Sqlite(e))
    }
}

/// Result of a `merge()` (spec/EVENTS.md §8).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub inserted: usize,
    pub duplicates: usize,
    pub scrubbed_on_arrival: usize,
    pub newly_scrubbed: usize,
    /// (offset in the incoming slice, reason); never silently dropped.
    pub quarantined: Vec<(usize, String)>,
    /// Same id, structurally different content: corruption upstream.
    pub integrity_warnings: Vec<EventId>,
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

/// Draft of a new event (spec/EVENTS.md §10). There is no redaction draft:
/// redaction is not an append; it goes through `redact()`.
#[derive(Debug, Clone)]
pub enum EventDraft {
    Remark {
        source: RemarkSource,
        text: String,
        /// Empty = session-level remark.
        targets: Vec<ContentHash>,
    },
    Rating {
        /// 0..=5; 0 is an explicit zero, distinct from never-rated.
        value: u8,
        /// Non-empty.
        targets: Vec<ContentHash>,
    },
    Stroke {
        payload: StrokePayload,
        target: ContentHash,
        /// Backward link to an earlier utterance (X2).
        linked_event: Option<EventId>,
    },
    Revision {
        target: EventId,
        text: String,
    },
    Retraction {
        target: EventId,
        source: RetractionSource,
    },
}

#[derive(Debug, Clone)]
pub enum RemarkSource {
    Voice {
        /// Optional per CAPTURE §6.5: omitted when the model exposes no
        /// token log-probs.
        conf_pm: Option<u16>,
        dur_ms: u32,
        /// Backward link to an earlier stroke (X2).
        linked_event: Option<EventId>,
    },
    Typed,
}

#[derive(Debug, Clone, Copy)]
pub enum RetractionSource {
    Voice,
    System,
}

/// `sidecar_dirty` reasons (spec/EVENTS.md §5.3). `redaction` outranks and
/// overwrites other reasons for a hash, never vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirtyReason {
    Append,
    Fold,
    Redaction,
}

impl DirtyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DirtyReason::Append => "append",
            DirtyReason::Fold => "fold",
            DirtyReason::Redaction => "redaction",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "append" => DirtyReason::Append,
            "fold" => DirtyReason::Fold,
            "redaction" => DirtyReason::Redaction,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyImage {
    pub image: ContentHash,
    pub reason: DirtyReason,
    pub since_ts: UtcMillis,
}

/// Context for `open_session` (spec/EVENTS.md §9).
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Semver of the writing build.
    pub app_version: String,
    /// 32 lowercase hex, random per install.
    pub device_id: String,
    /// Opaque JSON written by CAPTURE; may be absent.
    pub root_context: Option<String>,
}

/// One `sessions` row (spec/EVENTS.md §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub started_ts: UtcMillis,
    pub ended_ts: Option<UtcMillis>,
    pub app_version: String,
    pub device_id: String,
    pub root_context: Option<String>,
}

/// One `image_journal_stats` row (P5, B4, B34): the grid-badge fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalStats {
    pub image: ContentHash,
    /// Live (non-retracted) content events targeting the image; scrubbed
    /// stubs count (B4).
    pub event_count: i64,
    /// Any live, non-scrubbed remark (B34) — words evidence.
    pub has_text: bool,
    /// Any live, non-scrubbed stroke (B4) — marks evidence.
    pub has_strokes: bool,
    /// Count of live, non-scrubbed strokes — the heatmap's small stroke factor
    /// (DESIGN-ATTENTION-HEATMAP.md). `has_strokes` carries presence; this
    /// carries the count, maintained in the same recompute transaction.
    pub stroke_count: i64,
    /// ts of the greatest-id live event.
    pub last_ts: UtcMillis,
}

impl JournalStats {
    /// UI §3.7 over B34: the dulled-red dot lights on words or marks only.
    pub fn has_journal(&self) -> bool {
        self.has_text || self.has_strokes
    }
}

/// Which focus tier a dwell episode was captured at (DESIGN-ATTENTION-HEATMAP.md):
/// `Look` = single-image view (full weight), `Grid` = grid select / multi-select
/// (a small fraction of the Look rate). The tier RATE is config (`tuning()`); the
/// store applies it in `record_dwell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwellSource {
    Look,
    Grid,
}

/// One image's normalized (0..1) engagement intensity in a scope
/// (DESIGN-ATTENTION-HEATMAP.md). Produced by [`EventStore::image_intensity`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImageIntensity {
    pub image: ContentHash,
    pub intensity: f64,
}

// ---------------------------------------------------------------------------
// Read instrumentation (I16): every SELECT issued by a read-pool connection
// on the current thread is counted, so tests can assert the §10.1 batched
// query plan (≤ 3 SELECTs) without trusting the implementation's word.
// ---------------------------------------------------------------------------

thread_local! {
    static READ_SELECTS: Cell<u64> = const { Cell::new(0) };
}

fn trace_read(sql: &str) {
    let t = sql.trim_start();
    if t.len() >= 6 && t.as_bytes()[..6].eq_ignore_ascii_case(b"select") {
        READ_SELECTS.with(|c| c.set(c.get() + 1));
    }
}

/// Number of SELECT statements executed by read-pool connections on this
/// thread since the process started. Test instrumentation for invariant I16.
pub fn read_select_count() -> u64 {
    READ_SELECTS.with(Cell::get)
}

// ---------------------------------------------------------------------------
// EventStore
// ---------------------------------------------------------------------------

const READ_POOL_SIZE: usize = 2;
const MERGE_CHUNK: usize = 10_000;
const IN_LIST_CHUNK: usize = 30_000;

/// The event store: one dedicated writer connection plus a small read pool
/// (spec/EVENTS.md §5.1); all writes serialize through the writer.
pub struct EventStore {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    minter: Minter,
    /// Where the PPVEC flat files live (the §1.3 layout: `{db
    /// parent}/vectors`). The redaction path zeroes vector bytes there
    /// synchronously (RETRIEVAL §13.5) — the metadata rows live in this
    /// same database, only the bytes are external.
    vectors_dir: Option<std::path::PathBuf>,
    /// Monotonic, in-memory journal version (Seam 1, sibling of
    /// `PpvecStore::vectors_version` and `Library::images_version`). Bumped on
    /// every committed journal MUTATION — a minted event (`append`), a
    /// redaction (`redact`), or a sidecar merge that landed events (`merge`) —
    /// so the library->view data-change contract lets the inspector/journal
    /// re-read when its slice advances instead of running an ad-hoc membership
    /// test. In-memory only: no schema column, no migration; views re-read on
    /// mount, so a counter that resets to 0 at process start is correct.
    journal_version: AtomicU64,
}

impl EventStore {
    /// Open (creating/migrating if necessary) the journal database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let writer = schema::open_connection(path)?;
        let from_version = schema::migrate(&writer)?;
        // Redaction-supremacy recovery (§7-step-8): a prior shutdown whose
        // checkpoint was blocked may have left scrubbed plaintext in `-wal`.
        // Truncate now — at open this is the FIRST connection to the db, so no
        // reader can block it, reliably flushing the committed (already-scrubbed)
        // state and dropping the pre-scrub frames. Best-effort: a rare
        // cross-process blocker just defers to the idle/shutdown checkpoints.
        if let Err(e) = checkpoint_truncate(&writer) {
            tracing::warn!(error = %e, "startup WAL checkpoint deferred (a sibling reader holds the db)");
        }
        let mut readers = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            let mut conn = schema::open_connection(path)?;
            conn.trace(Some(trace_read));
            readers.push(Mutex::new(conn));
        }
        let store = Self {
            writer: Mutex::new(writer),
            readers,
            next_reader: AtomicUsize::new(0),
            minter: Minter::new(),
            vectors_dir: crate::retrieval::default_vectors_dir(path),
            journal_version: AtomicU64::new(0),
        };
        // Two migrations added an `image_journal_stats` column with a
        // placeholder DEFAULT that the real fold must backfill: v5 added
        // `has_text` (B37) and v12 added `stroke_count` (the heatmap's stroke
        // factor). A database opened below either needs its derived tables
        // rebuilt. Fresh databases (from_version 0) and current ones skip this.
        if (1..12).contains(&from_version) {
            store.rebuild_derived()?;
        }
        Ok(store)
    }

    /// Monotonic per-process journal version (Seam 1, sibling of
    /// [`crate::retrieval::PpvecStore::vectors_version`] and
    /// [`crate::library::Library::images_version`]). Views compare it to the
    /// value they last read against and re-read only when it advances.
    pub fn journal_version(&self) -> u64 {
        self.journal_version.load(Ordering::Relaxed)
    }

    /// Bump the journal version (Seam 1) after a committed journal mutation.
    /// Centralized so every mutating path advances it the same way; callers
    /// invoke it only AFTER the writing transaction commits.
    fn bump_journal_version(&self) {
        self.journal_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Monotonic ULID + UTC ts, for pre-allocation at capture onset (§1.2).
    pub fn mint(&self) -> Minted {
        self.minter.mint()
    }

    /// [`EventStore::mint`] against a caller-supplied wall clock — CAPTURE's
    /// Clock seam mints utterance ids at VAD onset through THE SAME minter,
    /// so process-wide ULID monotonicity (I14) holds across capture-minted
    /// and store-minted ids.
    pub fn mint_at(&self, now: UtcMillis) -> Minted {
        self.minter.mint_at(now)
    }

    fn with_reader<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let idx = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        let conn = self.readers[idx].lock().expect("reader mutex poisoned");
        f(&conn)
    }

    // -- canonical JSON (§4) ------------------------------------------------

    /// The bytes sidecars embed. Round-trip is byte-exact (I5).
    pub fn canonical_json(event: &Event) -> Vec<u8> {
        canonical::canonical_json(event)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Event, CanonicalParseError> {
        canonical::parse_canonical(bytes)
    }

    // -- append (§10, §6.3) -------------------------------------------------

    /// Validates (matrix, target counts, target-kind rules), inserts the
    /// event and its `event_targets` rows, performs fold maintenance (§6.3)
    /// — one transaction.
    pub fn append(
        &self,
        session: &SessionId,
        draft: EventDraft,
        minted: Option<Minted>,
    ) -> Result<Event, AppendError> {
        let minted = minted.unwrap_or_else(|| self.minter.mint());
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let tx = w.transaction().map_err(StoreError::from)?;
        let event = build_draft_event(&tx, session, draft, minted)?;
        event.validate()?;
        if get_event(&tx, &event.id)?.is_some() {
            return Err(AppendError::DuplicateId(event.id));
        }
        // Redaction supremacy (§7): every insert path checks the registry
        // first. A dangling redaction learned via merge condemns the id even
        // before any row exists; appending content for it is corruption.
        if registry_lookup(&tx, &event.id)?.is_some() {
            return Err(AppendError::CondemnedId(event.id));
        }
        insert_event(&tx, &event)?;

        let mut roots: BTreeSet<EventId> = BTreeSet::new();
        let mut dirty: BTreeMap<ContentHash, DirtyReason> = BTreeMap::new();
        match event.kind {
            Kind::Remark => {
                roots.insert(event.id.clone());
                for h in &event.targets {
                    dirty.insert(h.clone(), DirtyReason::Append);
                }
            }
            Kind::Rating | Kind::Stroke => {
                for h in &event.targets {
                    dirty.insert(h.clone(), DirtyReason::Append);
                }
            }
            Kind::Revision => {
                if let Some(root) = db_root_event(&tx, &event)? {
                    for h in &root.targets {
                        dirty.insert(h.clone(), DirtyReason::Fold);
                    }
                    if root.kind == Kind::Remark {
                        roots.insert(root.id);
                    }
                }
            }
            Kind::Retraction => {
                let target_id = event.target_event.as_ref().expect("validated");
                if let Some(t) = get_event(&tx, target_id)? {
                    for h in db_effective_targets(&tx, &t)? {
                        dirty.insert(h, DirtyReason::Fold);
                    }
                    match t.kind {
                        Kind::Remark => {
                            roots.insert(t.id.clone());
                        }
                        Kind::Revision => {
                            if let Some(root) = db_root_event(&tx, &t)?
                                && root.kind == Kind::Remark
                            {
                                roots.insert(root.id);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Kind::Redaction => unreachable!("no redaction draft exists"),
        }
        recompute_derived(&tx, &roots, &dirty)?;
        tx.commit().map_err(StoreError::from)?;
        // Library->view data-version (Seam 1): a journal event was minted, so
        // the inspector's slice changed. Bump AFTER the commit (the `?` above
        // are errors) so views re-read over the existing event channel.
        self.bump_journal_version();
        Ok(event)
    }

    // -- folded reads (§6.2, §10.1) ------------------------------------------

    /// Folded journal of one image (§6.2): what the UI and context assembly
    /// consume. Executes the §10.1 batched plan, never per-event probes.
    pub fn folded_journal(&self, image: &ContentHash) -> Result<Vec<JournalEntry>, StoreError> {
        self.with_reader(|conn| {
            let set = image_closure(conn, image)?;
            let fs = FoldSet::new(set);
            Ok(fold_entries(&fs, |e| e.targets.contains(image)))
        })
    }

    /// Folded journal of one session, session-level remarks included.
    pub fn folded_session(&self, session: &SessionId) -> Result<Vec<JournalEntry>, StoreError> {
        self.with_reader(|conn| {
            let mut set = BTreeMap::new();
            collect_events(
                conn,
                &joined_sql("e.session_id = ?1"),
                &[session.as_str()],
                &mut set,
            )?;
            closure_descendants(conn, &mut set)?;
            let fs = FoldSet::new(set);
            Ok(fold_entries(&fs, |e| e.session_id == *session))
        })
    }

    /// Current rating per image = value of the greatest-id live rating
    /// (§3.2), answered from the derived `image_ratings` fold (I7 guarantees
    /// it equals the from-scratch fold).
    pub fn current_rating(&self, image: &ContentHash) -> Result<Option<u8>, StoreError> {
        self.with_reader(|conn| {
            let v: Option<i64> = conn
                .prepare_cached("SELECT rating FROM image_ratings WHERE image_hash = ?1")?
                .query_row([image.as_str()], |r| r.get(0))
                .optional()?;
            Ok(v.map(|v| v as u8))
        })
    }

    /// Batched journal-stats read for grid badges (UI §3.5, P5, B34): one
    /// SELECT per IN-list chunk over the derived `image_journal_stats` fold.
    /// Images with no live journal have no row and are absent from the
    /// result. Order: ascending image hash.
    pub fn journal_stats(&self, images: &[ContentHash]) -> Result<Vec<JournalStats>, StoreError> {
        self.with_reader(|conn| {
            let mut out = Vec::new();
            for chunk in images.chunks(IN_LIST_CHUNK) {
                let marks = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT image_hash, event_count, has_text, has_strokes, stroke_count, last_ts \
                     FROM image_journal_stats WHERE image_hash IN ({marks}) \
                     ORDER BY image_hash"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let params: Vec<&str> = chunk.iter().map(ContentHash::as_str).collect();
                let mut rows = stmt.query(params_from_iter(params.iter()))?;
                while let Some(row) = rows.next()? {
                    let hash: String = row.get(0)?;
                    let last_ts: String = row.get(5)?;
                    out.push(JournalStats {
                        image: ContentHash::from_hex(&hash).map_err(corrupt)?,
                        event_count: row.get(1)?,
                        has_text: row.get::<_, i64>(2)? != 0,
                        has_strokes: row.get::<_, i64>(3)? != 0,
                        stroke_count: row.get(4)?,
                        last_ts: UtcMillis::parse(&last_ts).map_err(corrupt)?,
                    });
                }
            }
            Ok(out)
        })
    }

    // -- attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md) -----------
    //
    // Dwell is machine-observed telemetry, kept in `image_dwell` SEPARATE from
    // the annotation event log (K14: the journal stays the user's own
    // words/marks). It is local-only and never enters a sidecar. The tier rate
    // and the per-episode cap are read from the heatmap tuning config here —
    // keeping them in the BACKEND keeps tuning authoritative; the frontend just
    // reports a raw episode (hash, source, elapsed_ms).

    /// Record one finished focus EPISODE for an image (DESIGN §"dwell
    /// capture"). `source` selects the tier: `Look` (single-image view, full
    /// weight) or `Grid` (grid select / multi-select, a small fraction). The
    /// tier rate is applied to `elapsed_ms` and the result is capped at
    /// `dwell_cap_ms` PER CALL (so a walk-away inside one episode can add at
    /// most the cap), then accumulated: `dwell_ms += min(rate*elapsed, cap)`,
    /// `focus_count += 1`, `last_ts = now`.
    pub fn record_dwell(
        &self,
        hash: &ContentHash,
        source: DwellSource,
        elapsed_ms: i64,
        now: UtcMillis,
    ) -> Result<(), StoreError> {
        let h = crate::tuning::tuning().heatmap;
        let rate = match source {
            DwellSource::Look => h.dwell_look_rate,
            DwellSource::Grid => h.dwell_grid_rate,
        };
        // Negative elapsed (a clock glitch) contributes nothing; the tier rate
        // and the cap together bound the credit at `dwell_cap_ms` per call.
        let raw = (rate * elapsed_ms.max(0) as f64).floor() as i64;
        let add = raw.clamp(0, h.dwell_cap_ms);
        let w = self.writer.lock().expect("writer mutex poisoned");
        w.prepare_cached(
            "INSERT INTO image_dwell(image_hash, dwell_ms, focus_count, last_ts) \
             VALUES (?1, ?2, 1, ?3) \
             ON CONFLICT(image_hash) DO UPDATE SET \
               dwell_ms = dwell_ms + excluded.dwell_ms, \
               focus_count = focus_count + 1, \
               last_ts = excluded.last_ts",
        )?
        .execute(params![hash.as_str(), add, now.to_rfc3339()])?;
        Ok(())
    }

    /// Per-image engagement intensity over a scope, normalized to 0..1
    /// (DESIGN-ATTENTION-HEATMAP.md). For each hash in `hashes`:
    ///
    /// ```text
    /// raw = w_dwell·dwell_ms + w_events·event_count + w_strokes·stroke_count
    /// ```
    ///
    /// (dwell from `image_dwell`, the counts from `image_journal_stats`; a hash
    /// absent from either contributes 0 for that family). When `all_time` is
    /// false (the DEFAULT — "what am I working on NOW"), `raw` is multiplied by
    /// a recency weight before normalization:
    ///
    /// ```text
    /// recency(age_days) = 0.5 ^ (age_days / recency_half_life_days)
    /// ```
    ///
    /// where `age_days` is the age of the image's most recent signal time
    /// (`max(image_dwell.last_ts, image_journal_stats.last_ts)`) relative to
    /// `now`. Recent attention burns hotter; the half-life is the tunable knob.
    /// When `all_time` is true the recency factor is omitted (flat all-time —
    /// "what mattered most ever"). The scaled scores are then NORMALIZED by the
    /// scope maximum so the hottest image is 1.0; an all-zero scope returns all
    /// zeros (no division by zero). The returned vector is in INPUT order.
    pub fn image_intensity(
        &self,
        hashes: &[ContentHash],
        all_time: bool,
        now: UtcMillis,
    ) -> Result<Vec<ImageIntensity>, StoreError> {
        let h = crate::tuning::tuning().heatmap;
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        self.with_reader(|conn| {
            // Pull the raw component rows for the scope in batched IN-list
            // chunks (the journal_stats plan), keyed by hash for assembly.
            struct Acc {
                dwell_ms: i64,
                event_count: i64,
                stroke_count: i64,
                last_epoch_ms: Option<i64>,
            }
            let mut acc: HashMap<String, Acc> = HashMap::new();
            for chunk in hashes.chunks(IN_LIST_CHUNK) {
                let marks = vec!["?"; chunk.len()].join(",");
                let params: Vec<&str> = chunk.iter().map(ContentHash::as_str).collect();

                // Annotation counts + their last_ts.
                let sql = format!(
                    "SELECT image_hash, event_count, stroke_count, last_ts \
                     FROM image_journal_stats WHERE image_hash IN ({marks})"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let mut rows = stmt.query(params_from_iter(params.iter()))?;
                while let Some(row) = rows.next()? {
                    let hash: String = row.get(0)?;
                    let last_ts: String = row.get(3)?;
                    let epoch = UtcMillis::parse(&last_ts).map_err(corrupt)?.epoch_ms();
                    let e = acc.entry(hash).or_insert(Acc {
                        dwell_ms: 0,
                        event_count: 0,
                        stroke_count: 0,
                        last_epoch_ms: None,
                    });
                    e.event_count = row.get(1)?;
                    e.stroke_count = row.get(2)?;
                    e.last_epoch_ms = Some(e.last_epoch_ms.map_or(epoch, |p| p.max(epoch)));
                }

                // Dwell telemetry + its last_ts.
                let sql = format!(
                    "SELECT image_hash, dwell_ms, last_ts \
                     FROM image_dwell WHERE image_hash IN ({marks})"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let mut rows = stmt.query(params_from_iter(params.iter()))?;
                while let Some(row) = rows.next()? {
                    let hash: String = row.get(0)?;
                    let last_ts: String = row.get(2)?;
                    let epoch = UtcMillis::parse(&last_ts).map_err(corrupt)?.epoch_ms();
                    let e = acc.entry(hash).or_insert(Acc {
                        dwell_ms: 0,
                        event_count: 0,
                        stroke_count: 0,
                        last_epoch_ms: None,
                    });
                    e.dwell_ms = row.get(1)?;
                    e.last_epoch_ms = Some(e.last_epoch_ms.map_or(epoch, |p| p.max(epoch)));
                }
            }

            // Scale each hash's raw composite, optionally recency-weighted.
            let half_life_ms = h.recency_half_life_days * 86_400_000.0;
            let mut scaled: Vec<(usize, f64)> = Vec::with_capacity(hashes.len());
            let mut max = 0.0_f64;
            for (i, hash) in hashes.iter().enumerate() {
                let s = match acc.get(hash.as_str()) {
                    None => 0.0,
                    Some(a) => {
                        let raw = h.w_dwell * a.dwell_ms as f64
                            + h.w_events * a.event_count as f64
                            + h.w_strokes * a.stroke_count as f64;
                        if all_time {
                            raw
                        } else {
                            // age in ms from the most recent signal to `now`;
                            // future timestamps (clock skew) clamp to age 0 =
                            // full weight, never a >1 boost.
                            let age_ms = a.last_epoch_ms.map_or(0, |e| (now.epoch_ms() - e).max(0));
                            let recency = 0.5_f64.powf(age_ms as f64 / half_life_ms);
                            raw * recency
                        }
                    }
                };
                if s > max {
                    max = s;
                }
                scaled.push((i, s));
            }

            // Normalize by the scope max (0..1). An all-zero scope stays zero.
            let mut out = vec![
                ImageIntensity {
                    image: hashes[0].clone(),
                    intensity: 0.0,
                };
                hashes.len()
            ];
            for (i, s) in scaled {
                out[i] = ImageIntensity {
                    image: hashes[i].clone(),
                    intensity: if max > 0.0 { s / max } else { 0.0 },
                };
            }
            Ok(out)
        })
    }

    /// Wipe ALL dwell telemetry (the "clear attention data" reset, DESIGN
    /// §"open decisions"). Annotation counts are untouched — those are the
    /// user's own journal, not telemetry. Returns the rows removed.
    pub fn clear_dwell(&self) -> Result<usize, StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        Ok(w.execute("DELETE FROM image_dwell", [])?)
    }

    // -- raw reads ------------------------------------------------------------

    /// Verbatim history: retracted included, scrubbed in scrubbed form.
    pub fn raw_event(&self, id: &EventId) -> Result<Option<Event>, StoreError> {
        self.with_reader(|conn| get_event(conn, id))
    }

    /// Effective-target closure for one image, ascending id — the sidecar
    /// feed (§2.3).
    pub fn events_for_image(&self, image: &ContentHash) -> Result<Vec<Event>, StoreError> {
        self.with_reader(|conn| {
            let set = image_closure(conn, image)?;
            Ok(set.into_values().collect())
        })
    }

    /// Zero-effective-target events per session — overflow/export feed (§2.3).
    pub fn sessionlevel_events(&self, session: &SessionId) -> Result<Vec<Event>, StoreError> {
        self.with_reader(|conn| {
            let mut set = BTreeMap::new();
            collect_events(
                conn,
                &joined_sql("e.session_id = ?1"),
                &[session.as_str()],
                &mut set,
            )?;
            // Ancestors to fixpoint, so effective_targets resolves fully.
            let mut tried: HashSet<EventId> = set.keys().cloned().collect();
            loop {
                let mut missing: Vec<String> = Vec::new();
                for t in set.values().filter_map(|e| e.target_event.clone()) {
                    if tried.insert(t.clone()) {
                        missing.push(t.as_str().to_owned());
                    }
                }
                if missing.is_empty() {
                    break;
                }
                fetch_joined_in(conn, "id", &missing, &mut set)?;
            }
            let fs = FoldSet::new(set);
            Ok(fs
                .events
                .values()
                .filter(|e| e.session_id == *session && fs.effective_targets(e).is_empty())
                .cloned()
                .collect())
        })
    }

    // -- redaction (§7) -------------------------------------------------------

    /// §7: all steps in one transaction. Returns the redaction event ids
    /// minted (one per chain member).
    pub fn redact(&self, target: &EventId) -> Result<Vec<EventId>, RedactError> {
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let tx = w.transaction().map_err(StoreError::from)?;
        // 1. Validate.
        let t = get_event(&tx, target)?.ok_or_else(|| RedactError::NotFound(target.clone()))?;
        if !matches!(t.kind, Kind::Remark | Kind::Revision | Kind::Stroke) {
            return Err(RedactError::InvalidKind(t.kind));
        }
        if t.scrubbed() {
            return Err(RedactError::AlreadyScrubbed(target.clone()));
        }
        // 2. Chain closure: C = chain(root(target)) for remark/revision;
        //    C = {target} for a stroke. When root(target) is undefined
        //    (dangling hop or cycle, §6.1) the chain closure rule still
        //    applies to everything locally reachable: anchor at the highest
        //    reachable ancestor and scrub it plus every local revision
        //    resolving to it — corrected copies of sensitive content are
        //    still sensitive (§3.6), known or not to be a full chain.
        let (anchor, members) = if t.kind == Kind::Stroke {
            (t.clone(), vec![t.clone()])
        } else {
            let anchor = db_chain_anchor(&tx, &t)?;
            let members = db_chain_members(&tx, &anchor)?;
            (anchor, members)
        };
        let session_id = redaction_session(&tx, &t)?;
        // 3.–6. Record the act, registry, scrub in place, purge indexes.
        let mut out = Vec::new();
        let mut scrubbed_ids = Vec::new();
        let mut newly_scrubbed: Vec<Event> = Vec::new();
        for c in &members {
            if c.scrubbed() {
                continue; // idempotence
            }
            let m = self.minter.mint();
            let r = Event {
                id: m.id,
                v: 1,
                session_id: session_id.clone(),
                ts: m.ts,
                source: Source::System,
                kind: Kind::Redaction,
                targets: Vec::new(),
                text: None,
                payload: None,
                target_event: Some(c.id.clone()),
                linked_event: None,
                redacted_by: None,
            };
            r.validate()
                .map_err(|e| StoreError::Invalid(e.to_string()))?;
            insert_event(&tx, &r)?;
            registry_upsert(&tx, &c.id, &r.id, r.ts)?;
            scrub_in_place(&tx, &c.id, &r.id)?;
            // RETRIEVAL §1.1: mark, never DELETE — the metadata row keeps
            // the flat-file pointer so the PPVEC scrub can physically zero
            // the stored bytes (deleting here would orphan un-zeroed bytes
            // in the file until compaction).
            tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                .execute([c.id.as_str()])
                .map_err(StoreError::from)?;
            scrubbed_ids.push(c.id.clone());
            newly_scrubbed.push(c.clone());
            out.push(r.id);
        }
        // RETRIEVAL §9.5/§1.1: derived rows that may PARAPHRASE the
        // scrubbed content — summaries, their summaries_fts rows, the
        // chain's sentiment rows — go in the same transaction as the
        // content scrub; the affected image_summary vectors are marked
        // dead here and physically zeroed below with the chunk vectors.
        let summary_images = purge_redacted_derived(&tx, &newly_scrubbed)?;
        // 6.–7. FTS purge + queue propagation via the scoped recompute.
        let mut roots = BTreeSet::new();
        if anchor.kind == Kind::Remark {
            roots.insert(anchor.id.clone());
        }
        let mut dirty = BTreeMap::new();
        for h in db_effective_targets(&tx, &anchor)? {
            dirty.insert(h, DirtyReason::Redaction);
        }
        recompute_derived(&tx, &roots, &dirty)?;
        tx.commit().map_err(StoreError::from)?;
        // RETRIEVAL §13.5: the PPVEC flat-file bytes are zeroed by the time
        // this call returns — never deferred to the next embedding drain.
        // Zeroing AFTER the commit is deliberate: the deleted=1 marks are
        // durable first, so a crash in this gap leaves marks the drain's
        // sweep_dead re-zeroes, rather than zeroed bytes whose metadata
        // still claims them live.
        self.zero_vector_bytes(&w, &scrubbed_ids)?;
        self.zero_summary_vector_bytes(&w, &summary_images)?;
        // 8. Physical hygiene: scrubbed plaintext must not linger in the WAL.
        //    The scrub above is already committed; a CheckpointBlocked error
        //    here means hygiene is incomplete, not that the redaction failed.
        checkpoint_truncate(&w)?;
        // Library->view data-version (Seam 1): a redaction mutated the journal,
        // so the inspector's slice changed. Bump after the durable commit.
        self.bump_journal_version();
        Ok(out)
    }

    /// Zero the flat-file bytes of every dead vector row of these events
    /// (RETRIEVAL §13.5 — redaction zeroing is synchronous). A store
    /// without a conventional vectors directory has no flat files to
    /// scrub; the metadata marks alone then carry the contract via the
    /// drain's sweep.
    fn zero_vector_bytes(
        &self,
        conn: &Connection,
        event_ids: &[EventId],
    ) -> Result<(), StoreError> {
        let Some(dir) = self.vectors_dir.as_deref() else {
            return Ok(());
        };
        if event_ids.is_empty() || !dir.exists() {
            return Ok(());
        }
        for id in event_ids {
            crate::retrieval::zero_deleted_rows_for_event(conn, dir, id.as_str())
                .map_err(|e| StoreError::VectorScrub(e.to_string()))?;
        }
        Ok(())
    }

    /// Zero the flat-file bytes of these images' dead image-keyed vectors —
    /// the `image_summary` rows that redaction propagation marked when it
    /// deleted their summary rows (RETRIEVAL §9.5: the vector follows its
    /// summary). Same synchronous posture and same crash story as
    /// [`Self::zero_vector_bytes`].
    fn zero_summary_vector_bytes(
        &self,
        conn: &Connection,
        image_hashes: &[ContentHash],
    ) -> Result<(), StoreError> {
        let Some(dir) = self.vectors_dir.as_deref() else {
            return Ok(());
        };
        if image_hashes.is_empty() || !dir.exists() {
            return Ok(());
        }
        for h in image_hashes {
            crate::retrieval::zero_deleted_rows_for_image(conn, dir, h.as_str())
                .map_err(|e| StoreError::VectorScrub(e.to_string()))?;
        }
        Ok(())
    }

    // -- merge (§8) -----------------------------------------------------------

    /// The one synchronization primitive: set-union by event id,
    /// order-independent, with redaction supremacy.
    pub fn merge(&self, incoming: &[Event]) -> Result<MergeReport, StoreError> {
        let mut report = MergeReport::default();

        // 0. Validate; quarantine invalid events (never silently dropped,
        //    never inserted). Dedupe within the batch (set semantics):
        //    same-id copies that differ structurally are corruption — keep
        //    the first, log an integrity warning, EXCEPT a scrubbed copy
        //    always beats an unscrubbed one (§2.3, §8) — so the kept copy is
        //    independent of input order (I6) and redacted plaintext can
        //    never be resurrected by a stale sibling sidecar in the batch.
        let mut valid: Vec<(usize, &Event)> = Vec::with_capacity(incoming.len());
        let mut seen: HashMap<&EventId, usize> = HashMap::new();
        for (i, e) in incoming.iter().enumerate() {
            if let Err(err) = e.validate() {
                report.quarantined.push((i, err.to_string()));
                continue;
            }
            match seen.get(&e.id) {
                Some(&kept_idx) => {
                    report.duplicates += 1;
                    let kept = valid[kept_idx].1;
                    if structural_mismatch(kept, e) {
                        report.integrity_warnings.push(e.id.clone());
                    }
                    if e.scrubbed() && !kept.scrubbed() {
                        valid[kept_idx].1 = e; // scrubbed beats unscrubbed
                    }
                }
                None => {
                    seen.insert(&e.id, valid.len());
                    valid.push((i, e));
                }
            }
        }
        let incoming_kinds: HashMap<&EventId, Kind> =
            valid.iter().map(|(_, e)| (&e.id, e.kind)).collect();

        // Batching discipline (§8): sort ascending by event id before insert.
        valid.sort_by(|a, b| a.1.id.cmp(&b.1.id));

        let large = valid.len() > MERGE_CHUNK;
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let mut touched: Vec<Event> = Vec::new();

        if !large {
            // One transaction: union + derived recompute.
            let tx = w.transaction()?;
            merge_chunk(&tx, &valid, &incoming_kinds, &mut report, &mut touched)?;
            let (roots, dirty) = merge_affected(&tx, &touched)?;
            recompute_derived(&tx, &roots, &dirty)?;
            // §9.5 propagation for merge-learned redactions, same
            // transaction as the scrub. Every scrubbed touched event is
            // covered (not just the newly scrubbed) — the set never
            // under-covers, and it lazily heals databases whose earlier
            // redactions predate this propagation.
            let summary_images = if report.newly_scrubbed > 0 {
                purge_redacted_derived(&tx, &scrubbed_events(&touched))?
            } else {
                Vec::new()
            };
            tx.commit()?;
            // §8 step 2 scrubs in place per §7 steps 5–8; step 8 is the WAL
            // truncate — mandatory on every merge path that newly scrubs,
            // the ≤10k path included, or the plaintext lingers in the WAL.
            if report.newly_scrubbed > 0 {
                self.zero_vector_bytes(&w, &scrubbed_event_ids(&touched))?;
                self.zero_summary_vector_bytes(&w, &summary_images)?;
                checkpoint_truncate(&w)?;
            }
        } else {
            // Commit in ~10k-event transactions; populate FTS and the derived
            // tables in a single pass after the union completes (§8).
            for chunk in valid.chunks(MERGE_CHUNK) {
                let tx = w.transaction()?;
                merge_chunk(&tx, chunk, &incoming_kinds, &mut report, &mut touched)?;
                tx.commit()?;
            }
            let tx = w.transaction()?;
            let (roots, dirty) = merge_affected(&tx, &touched)?;
            recompute_derived(&tx, &roots, &dirty)?;
            // §9.5 propagation — in the recompute transaction, matching the
            // large-merge FTS posture (the scrubs committed per chunk).
            let summary_images = if report.newly_scrubbed > 0 {
                purge_redacted_derived(&tx, &scrubbed_events(&touched))?
            } else {
                Vec::new()
            };
            tx.commit()?;
            // Newly learned redactions zero their flat-file vector bytes
            // before merge returns (RETRIEVAL §13.5), same as redact().
            if report.newly_scrubbed > 0 {
                self.zero_vector_bytes(&w, &scrubbed_event_ids(&touched))?;
                self.zero_summary_vector_bytes(&w, &summary_images)?;
            }
            schema::run_pragma(&w, "ANALYZE")?;
            w.execute("INSERT INTO event_fts(event_fts) VALUES('optimize')", [])?;
            checkpoint_truncate(&w)?;
        }
        // Library->view data-version (Seam 1): bump only when the merge actually
        // changed the journal — new events landed or a merge-learned redaction
        // scrubbed existing ones. A pure no-op merge (all duplicates) leaves the
        // version untouched so views do not re-read on nothing.
        if report.inserted > 0 || report.newly_scrubbed > 0 {
            self.bump_journal_version();
        }
        Ok(report)
    }

    /// Session union (§8 step 5): same id ⇒ immutable fields keep local on
    /// mismatch (returned as warnings); `ended_ts := max(non-NULL)`.
    pub fn merge_sessions(&self, incoming: &[SessionRecord]) -> Result<Vec<SessionId>, StoreError> {
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let tx = w.transaction()?;
        let mut warnings = Vec::new();
        for s in incoming {
            match get_session(&tx, &s.id)? {
                None => {
                    insert_session(&tx, s)?;
                }
                Some(local) => {
                    if local.started_ts != s.started_ts
                        || local.app_version != s.app_version
                        || local.device_id != s.device_id
                        || local.root_context != s.root_context
                    {
                        warnings.push(s.id.clone());
                    }
                    let ended = match (local.ended_ts, s.ended_ts) {
                        (None, None) => None,
                        (a, b) => std::cmp::max(a, b),
                    };
                    if ended != local.ended_ts {
                        tx.prepare_cached("UPDATE sessions SET ended_ts = ?1 WHERE id = ?2")?
                            .execute(params![ended.map(|t| t.to_rfc3339()), s.id.as_str()])?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(warnings)
    }

    // -- sidecar-writer interface (§5.3) ---------------------------------------

    /// The durable dirty set; `reason='redaction'` rows are first-priority.
    pub fn dirty_images(&self) -> Result<Vec<DirtyImage>, StoreError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT image_hash, reason, since_ts FROM sidecar_dirty \
                 ORDER BY (reason = 'redaction') DESC, since_ts ASC, image_hash ASC",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let hash: String = row.get(0)?;
                let reason: String = row.get(1)?;
                let since: String = row.get(2)?;
                out.push(DirtyImage {
                    image: ContentHash::from_hex(&hash)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    reason: DirtyReason::parse(&reason)
                        .ok_or_else(|| StoreError::Corrupt(format!("dirty reason {reason}")))?,
                    since_ts: UtcMillis::parse(&since)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                });
            }
            Ok(out)
        })
    }

    /// Rows are removed via ack only after a successful sidecar write.
    /// Pass the `since_ts` observed in the `dirty_images()` read that fed
    /// the write: the row is deleted only if no newer mark has landed since
    /// (`since_ts` advances strictly on every re-mark), so an ack can never
    /// lose dirtiness recorded after the writer's snapshot.
    pub fn ack_dirty(&self, image: &ContentHash, upto_ts: UtcMillis) -> Result<(), StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        w.prepare_cached("DELETE FROM sidecar_dirty WHERE image_hash = ?1 AND since_ts <= ?2")?
            .execute(params![image.as_str(), upto_ts.to_rfc3339()])?;
        Ok(())
    }

    // -- sessions (§9) ----------------------------------------------------------

    pub fn open_session(&self, ctx: SessionContext) -> Result<SessionId, StoreError> {
        validate_device_id(&ctx.device_id).map_err(|e| StoreError::Invalid(e.to_string()))?;
        if ctx.app_version.is_empty() {
            return Err(StoreError::Invalid("app_version must be non-empty".into()));
        }
        let m = self.minter.mint();
        let id = SessionId::from_str_strict(m.id.as_str()).expect("minted ULID is valid");
        let w = self.writer.lock().expect("writer mutex poisoned");
        insert_session(
            &w,
            &SessionRecord {
                id: id.clone(),
                started_ts: m.ts,
                ended_ts: None,
                app_version: ctx.app_version,
                device_id: ctx.device_id,
                root_context: ctx.root_context,
            },
        )?;
        Ok(id)
    }

    /// `ended_ts` set once on close — the single permitted UPDATE on
    /// `sessions`. Closing an already-closed session is a no-op.
    pub fn close_session(&self, id: &SessionId, ended: UtcMillis) -> Result<(), StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        let n = w
            .prepare_cached("UPDATE sessions SET ended_ts = ?1 WHERE id = ?2 AND ended_ts IS NULL")?
            .execute(params![ended.to_rfc3339(), id.as_str()])?;
        if n == 0 && get_session(&w, id)?.is_none() {
            return Err(StoreError::SessionNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Read one session row (storage only; lifecycle in CAPTURE).
    pub fn session(&self, id: &SessionId) -> Result<Option<SessionRecord>, StoreError> {
        self.with_reader(|conn| get_session(conn, id))
    }

    /// Sessions with no `ended_ts`, ascending id — the crash-recovery input
    /// (CAPTURE §2.4): sessions left open by a dead process. Recovery mints
    /// no events; callers close each at its last event's ts (else its start)
    /// via [`EventStore::last_event_ts`] + [`EventStore::close_session`].
    pub fn open_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        self.with_reader(|conn| {
            let ids: Vec<String> = {
                let mut stmt = conn
                    .prepare_cached("SELECT id FROM sessions WHERE ended_ts IS NULL ORDER BY id")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<_, _>>()?
            };
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                let sid = SessionId::from_str_strict(&id).map_err(corrupt)?;
                out.push(get_session(conn, &sid)?.expect("row just selected"));
            }
            Ok(out)
        })
    }

    // -- capture session bookkeeping (CAPTURE §2.3–2.5) ------------------------
    //
    // `(closed_clean, close_processing_done)` live in `capture_session_state`,
    // a capture-owned, index-only bookkeeping table (never in sidecars; the
    // EVENTS §5.2 `sessions` table stays exactly the spec's shape, so merge
    // semantics are untouched).

    /// Record a session's close bookkeeping (CAPTURE §2.3): `closed_clean`,
    /// with close processing pending. Idempotent: re-recording keeps the
    /// first `closed_clean` verdict and never resets a completed
    /// close-processing run (recovery may race a clean close).
    pub fn record_session_close(
        &self,
        id: &SessionId,
        closed_clean: bool,
    ) -> Result<(), StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        w.prepare_cached(
            "INSERT INTO capture_session_state (session_id, closed_clean, close_processing_done) \
             VALUES (?1, ?2, 0) ON CONFLICT(session_id) DO NOTHING",
        )?
        .execute(params![id.as_str(), closed_clean as i64])?;
        Ok(())
    }

    /// Mark a closed session's close processing complete (§2.5 step 3
    /// bookkeeping; processors are idempotent and resumable).
    pub fn mark_close_processing_done(&self, id: &SessionId) -> Result<(), StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        w.prepare_cached(
            "UPDATE capture_session_state SET close_processing_done = 1 WHERE session_id = ?1",
        )?
        .execute([id.as_str()])?;
        Ok(())
    }

    /// Closed sessions whose close processing has not completed, ascending
    /// id — re-enqueued on next launch (§2.5: `close_processing_done =
    /// false` survives a quit-before-done).
    pub fn close_processing_pending(&self) -> Result<Vec<SessionId>, StoreError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT session_id FROM capture_session_state \
                 WHERE close_processing_done = 0 ORDER BY session_id",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(SessionId::from_str_strict(&row?).map_err(corrupt)?);
            }
            Ok(out)
        })
    }

    /// `(closed_clean, close_processing_done)` for one session, if recorded.
    pub fn session_close_state(&self, id: &SessionId) -> Result<Option<(bool, bool)>, StoreError> {
        self.with_reader(|conn| {
            Ok(conn
                .prepare_cached(
                    "SELECT closed_clean, close_processing_done \
                     FROM capture_session_state WHERE session_id = ?1",
                )?
                .query_row([id.as_str()], |r| {
                    Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0))
                })
                .optional()?)
        })
    }

    /// `ts` of the greatest-id event in a session, `None` if the session has
    /// no events. Crash recovery closes a recovered session here (CAPTURE
    /// §2.2's no-dead-air rule: a session's span ends at its last activity).
    pub fn last_event_ts(&self, session: &SessionId) -> Result<Option<UtcMillis>, StoreError> {
        self.with_reader(|conn| {
            let ts: Option<String> = conn
                .prepare_cached(
                    "SELECT ts FROM annotation_events WHERE session_id = ?1 \
                     ORDER BY id DESC LIMIT 1",
                )?
                .query_row([session.as_str()], |r| r.get(0))
                .optional()?;
            ts.map(|t| UtcMillis::parse(&t).map_err(corrupt))
                .transpose()
        })
    }

    // -- rebuild (§5.3–5.4, I7) ---------------------------------------------------

    /// Drop + recompute all derived tables from the truth tables. Must equal
    /// incremental maintenance (I7). `sidecar_dirty` is preserved: it is a
    /// durable queue with ack history, not a pure fold of the log.
    pub fn rebuild_derived(&self) -> Result<(), StoreError> {
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let tx = w.transaction()?;
        tx.execute("DELETE FROM redactions", [])?;
        tx.execute("DELETE FROM image_ratings", [])?;
        tx.execute("DELETE FROM image_journal_stats", [])?;
        tx.execute("DELETE FROM event_fts", [])?;
        // RETRIEVAL §11: the rebuild wipes BOTH FTS tables. summaries_fts
        // is re-derived from derived_summaries below; the summary rows
        // themselves are kept — they were purged at redact/merge time in
        // the same transaction as the scrub (§9.5), so what remains is
        // live, and core cannot regenerate them (generation is a model
        // pass).
        tx.execute("DELETE FROM summaries_fts", [])?;

        // Registry = the fold of kind='redaction' events; deterministic
        // min-redaction-id per target (matches the incremental upsert rule).
        {
            let mut stmt = tx.prepare(
                "SELECT target_event, id, ts FROM annotation_events \
                 WHERE kind = 'redaction' ORDER BY id ASC",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let target: String = row.get(0)?;
                let rid: String = row.get(1)?;
                let ts: String = row.get(2)?;
                tx.prepare_cached(
                    "INSERT OR IGNORE INTO redactions(event_id, redaction_id, ts) \
                     VALUES (?1, ?2, ?3)",
                )?
                .execute(params![target, rid, ts])?;
            }
        }

        // Full fold for FTS bodies.
        let mut set = BTreeMap::new();
        collect_events(&tx, &joined_sql("1=1"), &[], &mut set)?;
        let fs = FoldSet::new(set);
        let retracted = fs.retracted_ids();
        let chains = fs.chains();
        let mut live_roots: HashSet<EventId> = HashSet::new();
        for e in fs.events.values() {
            if e.kind != Kind::Remark {
                continue;
            }
            tx.prepare_cached("INSERT OR IGNORE INTO fts_map(root_event_id) VALUES (?1)")?
                .execute([e.id.as_str()])?;
            let live = !retracted.contains(&e.id) && !e.scrubbed();
            if live {
                let rowid: i64 = tx
                    .prepare_cached("SELECT fts_rowid FROM fts_map WHERE root_event_id = ?1")?
                    .query_row([e.id.as_str()], |r| r.get(0))?;
                let (text, _) = fs.effective_text(e, &chains, &retracted);
                tx.prepare_cached("INSERT INTO event_fts(rowid, body) VALUES (?1, ?2)")?
                    .execute(params![rowid, text.unwrap_or_default()])?;
                live_roots.insert(e.id.clone());
            }
        }

        // One summaries_fts row per surviving summary (RETRIEVAL §11 "re-fold
        // every ... live summary"): like event_fts, the indexed text exists
        // nowhere else to rebuild from except its source-of-truth row.
        tx.execute(
            "INSERT INTO summaries_fts(text, summary_id) \
             SELECT text, id FROM derived_summaries",
            [],
        )?;

        // Vectors referencing dead roots are marked deleted (never DELETEd:
        // the metadata row keeps the flat-file pointer for the PPVEC zeroing
        // sweep — RETRIEVAL §1.1/§1.3); still-valid vectors are preserved
        // (core cannot re-embed — RETRIEVAL owns recreation).
        {
            let dead: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT event_id FROM vectors
                     WHERE event_id IS NOT NULL AND deleted = 0",
                )?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                ids.into_iter()
                    .filter(|s| {
                        EventId::from_str_strict(s)
                            .map(|id| !live_roots.contains(&id))
                            .unwrap_or(true)
                    })
                    .collect()
            };
            for id in dead {
                tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                    .execute([id])?;
            }
        }

        // Per-image folds.
        {
            let images: Vec<String> = {
                let mut stmt = tx.prepare("SELECT DISTINCT image_hash FROM event_targets")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            for h in images {
                recompute_image(&tx, &h)?;
            }
        }

        tx.commit()?;
        // §5.1/§5.4 operational rules after rebuild — both FTS tables
        // (RETRIEVAL §11).
        schema::run_pragma(&w, "ANALYZE")?;
        w.execute("INSERT INTO event_fts(event_fts) VALUES('optimize')", [])?;
        w.execute(
            "INSERT INTO summaries_fts(summaries_fts) VALUES('optimize')",
            [],
        )?;
        checkpoint_truncate(&w)?;
        Ok(())
    }

    // -- maintenance (§5.1 operational rules) -----------------------------------

    /// §5.1 operational rules for long-lived connections, to be run at app
    /// idle (RUNTIME owns the scheduling; this is the hook it calls):
    /// `PRAGMA optimize` on every connection, periodically, and
    /// `PRAGMA wal_checkpoint(TRUNCATE)` at idle.
    ///
    /// Returns [`StoreError::CheckpointBlocked`] if a concurrent reader kept
    /// the WAL from truncating after bounded retries; calling again at the
    /// next idle interval is the intended recovery.
    pub fn maintain(&self) -> Result<(), StoreError> {
        {
            let w = self.writer.lock().expect("writer mutex poisoned");
            schema::run_pragma(&w, "PRAGMA optimize").map_err(StoreError::from)?;
            checkpoint_truncate(&w)?;
        }
        for r in &self.readers {
            let c = r.lock().expect("reader mutex poisoned");
            schema::run_pragma(&c, "PRAGMA optimize").map_err(StoreError::from)?;
        }
        Ok(())
    }

    /// Explicit shutdown checkpoint (spec §7-step-8 redaction supremacy + §5.1):
    /// truncate the WAL with a LONGER retry budget (~2 s) than idle maintenance,
    /// so a reader still finishing at quit does not leave scrubbed plaintext
    /// recoverable in `-wal`. The shell calls this from the quit path AFTER every
    /// other subsystem has flushed and released the db (watchers stopped; the
    /// session, sidecars, and collections all flushed), maximizing the chance no
    /// reader blocks. Returns [`StoreError::CheckpointBlocked`] if it still could
    /// not truncate after the budget; the caller logs loudly and the next `open`
    /// recovers it.
    pub fn checkpoint_at_shutdown(&self) -> Result<(), StoreError> {
        let w = self.writer.lock().expect("writer mutex poisoned");
        checkpoint_truncate_budget(&w, 20, 100)
    }
}

impl Drop for EventStore {
    fn drop(&mut self) {
        // §5.1/§7-step-8: a LAST-RESORT checkpoint at close. The shell's quit
        // path already calls `checkpoint_at_shutdown` (longer budget) after every
        // other subsystem has released the db, so this is the backstop for drop
        // paths that bypass it. A still-blocked checkpoint here means scrubbed
        // plaintext lingers in `-wal`; the next `open` recovers it, so escalate
        // to ERROR (not warn) rather than failing — Drop cannot return.
        if let Ok(w) = self.writer.lock() {
            if matches!(checkpoint_truncate(&w), Err(StoreError::CheckpointBlocked)) {
                tracing::error!(
                    "wal_checkpoint(TRUNCATE) at close blocked by a concurrent reader; \
                     scrubbed plaintext may remain in -wal until the next open recovers it \
                     (spec/EVENTS.md §5.1, redaction supremacy §7)"
                );
            }
            let _ = schema::run_pragma(&w, "PRAGMA optimize");
        }
        for r in &self.readers {
            if let Ok(c) = r.lock() {
                let _ = schema::run_pragma(&c, "PRAGMA optimize");
            }
        }
    }
}

/// §5.1/§7-step-8 checkpoint with the busy result handled (not swallowed):
/// bounded retries with a short per-attempt busy wait, then a hard
/// [`StoreError::CheckpointBlocked`] so callers know the WAL still holds the
/// pre-checkpoint bytes. The connection's §5.1 `busy_timeout` is restored
/// afterwards regardless of outcome. The short default budget suits idle
/// maintenance + the Drop backstop; the shutdown path uses a longer one.
fn checkpoint_truncate(w: &Connection) -> Result<(), StoreError> {
    checkpoint_truncate_budget(w, 4, 50)
}

/// [`checkpoint_truncate`] with a caller-chosen retry budget. The shutdown path
/// passes a LONGER budget than idle maintenance: redaction supremacy (§7-step-8)
/// requires scrubbed plaintext to leave the `-wal` before exit, so it is worth
/// waiting out a reader that is still finishing at quit. Cost is paid only when
/// actually blocked.
fn checkpoint_truncate_budget(
    w: &Connection,
    attempts: usize,
    retry_sleep_ms: u64,
) -> Result<(), StoreError> {
    schema::run_pragma(w, "PRAGMA busy_timeout = 250").map_err(StoreError::from)?;
    let result = (|| {
        for attempt in 0..attempts {
            if schema::checkpoint_truncate_once(w)? {
                return Ok(true);
            }
            if attempt + 1 < attempts {
                std::thread::sleep(std::time::Duration::from_millis(retry_sleep_ms));
            }
        }
        Ok(false)
    })();
    let restore = schema::run_pragma(
        w,
        &format!("PRAGMA busy_timeout = {}", schema::BUSY_TIMEOUT_MS),
    );
    let truncated = result.map_err(StoreError::Sqlite)?;
    restore.map_err(StoreError::Sqlite)?;
    if truncated {
        Ok(())
    } else {
        Err(StoreError::CheckpointBlocked)
    }
}

// ---------------------------------------------------------------------------
// Row mapping + batched fetch (§10.1)
// ---------------------------------------------------------------------------

const EVENT_COLS: &str = "e.id, e.v, e.session_id, e.ts, e.source, e.kind, e.text, e.payload, \
                          e.target_event, e.linked_event, e.redacted_by, t.image_hash";

fn joined_sql(where_clause: &str) -> String {
    format!(
        "SELECT {EVENT_COLS} FROM annotation_events e \
         LEFT JOIN event_targets t ON t.event_id = e.id \
         WHERE {where_clause} ORDER BY e.id, t.position"
    )
}

fn corrupt(msg: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(msg.to_string())
}

fn event_from_row(row: &rusqlite::Row<'_>) -> Result<Event, StoreError> {
    let id: String = row.get(0)?;
    let v: i64 = row.get(1)?;
    let session: String = row.get(2)?;
    let ts: String = row.get(3)?;
    let source: String = row.get(4)?;
    let kind: String = row.get(5)?;
    let text: Option<String> = row.get(6)?;
    let payload: Option<String> = row.get(7)?;
    let target_event: Option<String> = row.get(8)?;
    let linked_event: Option<String> = row.get(9)?;
    let redacted_by: Option<String> = row.get(10)?;

    let kind = Kind::parse(&kind).ok_or_else(|| corrupt(format!("kind {kind}")))?;
    let source = Source::parse(&source).ok_or_else(|| corrupt(format!("source {source}")))?;
    let opt_id = |s: Option<String>| -> Result<Option<EventId>, StoreError> {
        s.map(|s| EventId::from_str_strict(&s).map_err(corrupt))
            .transpose()
    };
    Ok(Event {
        id: EventId::from_str_strict(&id).map_err(corrupt)?,
        v: u32::try_from(v).map_err(corrupt)?,
        session_id: SessionId::from_str_strict(&session).map_err(corrupt)?,
        ts: UtcMillis::parse(&ts).map_err(corrupt)?,
        source,
        kind,
        targets: Vec::new(),
        text,
        payload: payload
            .map(|p| canonical::parse_payload_json(kind, source, &p).map_err(corrupt))
            .transpose()?,
        target_event: opt_id(target_event)?,
        linked_event: opt_id(linked_event)?,
        redacted_by: opt_id(redacted_by)?,
    })
}

/// Run one joined SELECT and fold its rows (events × target rows) into `out`.
fn collect_events(
    conn: &Connection,
    sql: &str,
    params: &[&str],
    out: &mut BTreeMap<EventId, Event>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    while let Some(row) = rows.next()? {
        let id_s: String = row.get(0)?;
        let id = EventId::from_str_strict(&id_s).map_err(corrupt)?;
        if !out.contains_key(&id) {
            out.insert(id.clone(), event_from_row(row)?);
        }
        let hash: Option<String> = row.get(11)?;
        if let Some(h) = hash {
            // Rows arrive ordered by t.position; duplicates impossible (PK).
            out.get_mut(&id)
                .expect("just inserted")
                .targets
                .push(ContentHash::from_hex(&h).map_err(corrupt)?);
        }
    }
    Ok(())
}

/// One batched joined fetch over `e.{column} IN (...)` (chunked only past
/// the SQLite variable limit; a single statement in all realistic cases).
fn fetch_joined_in(
    conn: &Connection,
    column: &str,
    ids: &[String],
    out: &mut BTreeMap<EventId, Event>,
) -> Result<(), StoreError> {
    for chunk in ids.chunks(IN_LIST_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = joined_sql(&format!("e.{column} IN ({marks})"));
        let params: Vec<&str> = chunk.iter().map(String::as_str).collect();
        collect_events(conn, &sql, &params, out)?;
    }
    Ok(())
}

/// Only these kinds can be the target of a meta-event, so only they extend
/// the closure frontier — this is what makes one §10.1 round the norm.
fn can_be_meta_target(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Remark | Kind::Rating | Kind::Stroke | Kind::Revision
    )
}

/// §10.1 step 3: meta-closure to fixpoint; each round one batched query.
fn closure_descendants(
    conn: &Connection,
    set: &mut BTreeMap<EventId, Event>,
) -> Result<(), StoreError> {
    let mut frontier: Vec<String> = set
        .values()
        .filter(|e| can_be_meta_target(e.kind))
        .map(|e| e.id.as_str().to_owned())
        .collect();
    while !frontier.is_empty() {
        let mut found = BTreeMap::new();
        fetch_joined_in(conn, "target_event", &frontier, &mut found)?;
        frontier = Vec::new();
        for (id, e) in found {
            if let std::collections::btree_map::Entry::Vacant(slot) = set.entry(id) {
                if can_be_meta_target(e.kind) {
                    frontier.push(slot.key().as_str().to_owned());
                }
                slot.insert(e);
            }
        }
    }
    Ok(())
}

/// §10.1: covering range scan → one batched id fetch → meta-closure.
fn image_closure(
    conn: &Connection,
    image: &ContentHash,
) -> Result<BTreeMap<EventId, Event>, StoreError> {
    // 1. Covering range scan on idx_targets_image.
    let ids: Vec<String> = {
        let mut stmt = conn.prepare_cached(
            "SELECT event_id FROM event_targets WHERE image_hash = ?1 ORDER BY event_id",
        )?;
        let rows = stmt.query_map([image.as_str()], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut set = BTreeMap::new();
    if ids.is_empty() {
        return Ok(set);
    }
    // 2. One batched SELECT for those ids.
    fetch_joined_in(conn, "id", &ids, &mut set)?;
    // 3. Meta-closure to fixpoint.
    closure_descendants(conn, &mut set)?;
    Ok(set)
}

fn get_event(conn: &Connection, id: &EventId) -> Result<Option<Event>, StoreError> {
    let mut set = BTreeMap::new();
    collect_events(conn, &joined_sql("e.id = ?1"), &[id.as_str()], &mut set)?;
    Ok(set.into_values().next())
}

// ---------------------------------------------------------------------------
// Write internals
// ---------------------------------------------------------------------------

fn insert_event(conn: &Connection, e: &Event) -> Result<(), StoreError> {
    conn.prepare_cached(
        "INSERT INTO annotation_events \
         (id, v, session_id, ts, source, kind, text, payload, target_event, linked_event, redacted_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        e.id.as_str(),
        e.v as i64,
        e.session_id.as_str(),
        e.ts.to_rfc3339(),
        e.source.as_str(),
        e.kind.as_str(),
        e.text,
        e.payload.as_ref().map(canonical::payload_canonical_json),
        e.target_event.as_ref().map(EventId::as_str),
        e.linked_event.as_ref().map(EventId::as_str),
        e.redacted_by.as_ref().map(EventId::as_str),
    ])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO event_targets (event_id, image_hash, position) VALUES (?1, ?2, ?3)",
    )?;
    for (i, h) in e.targets.iter().enumerate() {
        stmt.execute(params![e.id.as_str(), h.as_str(), i as i64])?;
    }
    Ok(())
}

/// Walk revision hops to the chain root (§6.1); cycles/dangling ⇒ None.
fn db_root_event(conn: &Connection, e: &Event) -> Result<Option<Event>, StoreError> {
    let mut seen: HashSet<EventId> = HashSet::new();
    let mut cur = e.clone();
    while cur.kind == Kind::Revision {
        if !seen.insert(cur.id.clone()) {
            return Ok(None);
        }
        let Some(target) = cur.target_event.clone() else {
            return Ok(None);
        };
        match get_event(conn, &target)? {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// `effective_targets(e)` against the database (§2.3); dangling ⇒ [].
fn db_effective_targets(conn: &Connection, e: &Event) -> Result<Vec<ContentHash>, StoreError> {
    let mut seen: HashSet<EventId> = HashSet::new();
    let mut cur = e.clone();
    while cur.kind.is_meta() {
        if !seen.insert(cur.id.clone()) {
            return Ok(Vec::new());
        }
        let Some(target) = cur.target_event.clone() else {
            return Ok(Vec::new());
        };
        match get_event(conn, &target)? {
            Some(next) => cur = next,
            None => return Ok(Vec::new()),
        }
    }
    Ok(cur.targets)
}

/// Highest locally reachable chain ancestor of `e` (§7 step 2): the chain
/// root for an intact chain; the last reachable revision before a dangling
/// hop; the last revision before a repeat for a cyclic chain. Never `None`:
/// where `root(e)` is undefined (§6.1) the redaction closure still anchors
/// at the deepest event it can reach.
fn db_chain_anchor(conn: &Connection, e: &Event) -> Result<Event, StoreError> {
    let mut seen: HashSet<EventId> = HashSet::new();
    let mut cur = e.clone();
    while cur.kind == Kind::Revision {
        if !seen.insert(cur.id.clone()) {
            break; // cycle: corrupt data; anchor where the walk closed
        }
        let Some(target) = cur.target_event.clone() else {
            break;
        };
        match get_event(conn, &target)? {
            Some(next) if !(next.kind == Kind::Revision && seen.contains(&next.id)) => cur = next,
            _ => break, // dangling hop (or the next hop closes a cycle)
        }
    }
    Ok(cur)
}

/// `chain(anchor)` = {anchor} ∪ every local revision whose `target_event`
/// walk reaches `anchor`, ascending id (§6.1, §7 step 2). Resolution stops
/// at the anchor rather than requiring a full `root()`, so the closure of a
/// dangling or cyclic sub-chain still includes its reachable members.
fn db_chain_members(conn: &Connection, anchor: &Event) -> Result<Vec<Event>, StoreError> {
    let mut set = BTreeMap::new();
    set.insert(anchor.id.clone(), anchor.clone());
    closure_descendants(conn, &mut set)?;
    let mut members: Vec<Event> = vec![anchor.clone()];
    for e in set.values() {
        if e.kind != Kind::Revision || e.id == anchor.id {
            continue;
        }
        let mut seen: HashSet<&EventId> = HashSet::new();
        let mut cur = e;
        let reaches = loop {
            if !seen.insert(&cur.id) {
                break false; // cycle not passing through the anchor
            }
            let Some(t) = cur.target_event.as_ref() else {
                break false;
            };
            if *t == anchor.id {
                break true;
            }
            match set.get(t) {
                Some(next) if next.kind == Kind::Revision => cur = next,
                _ => break false, // dangling, or resolves to another root
            }
        };
        if reaches {
            members.push(e.clone());
        }
    }
    members.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(members)
}

fn scrub_in_place(conn: &Connection, victim: &EventId, marker: &EventId) -> Result<(), StoreError> {
    conn.prepare_cached(
        "UPDATE annotation_events SET text = NULL, payload = NULL, redacted_by = ?1 WHERE id = ?2",
    )?
    .execute(params![marker.as_str(), victim.as_str()])?;
    Ok(())
}

/// Registry upsert: deterministic min-redaction-id wins, so incremental
/// maintenance matches `rebuild_derived()` under any arrival order.
fn registry_upsert(
    conn: &Connection,
    victim: &EventId,
    redaction: &EventId,
    ts: UtcMillis,
) -> Result<(), StoreError> {
    conn.prepare_cached(
        "INSERT INTO redactions(event_id, redaction_id, ts) VALUES (?1, ?2, ?3) \
         ON CONFLICT(event_id) DO UPDATE SET redaction_id = excluded.redaction_id, ts = excluded.ts \
         WHERE excluded.redaction_id < redactions.redaction_id",
    )?
    .execute(params![victim.as_str(), redaction.as_str(), ts.to_rfc3339()])?;
    Ok(())
}

fn registry_lookup(conn: &Connection, id: &EventId) -> Result<Option<EventId>, StoreError> {
    let v: Option<String> = conn
        .prepare_cached("SELECT redaction_id FROM redactions WHERE event_id = ?1")?
        .query_row([id.as_str()], |r| r.get(0))
        .optional()?;
    v.map(|s| EventId::from_str_strict(&s).map_err(corrupt))
        .transpose()
}

/// Redaction events are recorded against the current session (§7 step 3):
/// the latest open session, else the latest session, else the target's own.
fn redaction_session(conn: &Connection, target: &Event) -> Result<SessionId, StoreError> {
    let open: Option<String> = conn
        .prepare_cached("SELECT id FROM sessions WHERE ended_ts IS NULL ORDER BY id DESC LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()?;
    let any = match open {
        Some(s) => Some(s),
        None => conn
            .prepare_cached("SELECT id FROM sessions ORDER BY id DESC LIMIT 1")?
            .query_row([], |r| r.get(0))
            .optional()?,
    };
    match any {
        Some(s) => SessionId::from_str_strict(&s).map_err(corrupt),
        None => Ok(target.session_id.clone()),
    }
}

fn build_draft_event(
    conn: &Connection,
    session: &SessionId,
    draft: EventDraft,
    minted: Minted,
) -> Result<Event, AppendError> {
    let base = |source: Source, kind: Kind| Event {
        id: minted.id.clone(),
        v: 1,
        session_id: session.clone(),
        ts: minted.ts,
        source,
        kind,
        targets: Vec::new(),
        text: None,
        payload: None,
        target_event: None,
        linked_event: None,
        redacted_by: None,
    };
    Ok(match draft {
        EventDraft::Remark {
            source,
            text,
            targets,
        } => {
            let (src, payload, linked) = match source {
                RemarkSource::Voice {
                    conf_pm,
                    dur_ms,
                    linked_event,
                } => (
                    Source::Voice,
                    Some(Payload::Voice { conf_pm, dur_ms }),
                    linked_event,
                ),
                RemarkSource::Typed => (Source::Typed, None, None),
            };
            let mut e = base(src, Kind::Remark);
            e.targets = targets;
            e.text = Some(text);
            e.payload = payload;
            e.linked_event = linked;
            e
        }
        EventDraft::Rating { value, targets } => {
            let mut e = base(Source::Typed, Kind::Rating);
            e.targets = targets;
            e.payload = Some(Payload::Rating { value });
            e
        }
        EventDraft::Stroke {
            payload,
            target,
            linked_event,
        } => {
            let mut e = base(Source::Pencil, Kind::Stroke);
            e.targets = vec![target];
            e.payload = Some(Payload::Stroke(payload));
            e.linked_event = linked_event;
            e
        }
        EventDraft::Revision { target, text } => {
            let t = get_event(conn, &target)?
                .ok_or_else(|| AppendError::TargetNotFound(target.clone()))?;
            if !matches!(t.kind, Kind::Remark | Kind::Revision) {
                return Err(AppendError::InvalidTargetKind {
                    meta: Kind::Revision,
                    target_kind: t.kind,
                });
            }
            let mut e = base(Source::Typed, Kind::Revision);
            e.target_event = Some(target);
            e.text = Some(text);
            e
        }
        EventDraft::Retraction { target, source } => {
            let t = get_event(conn, &target)?
                .ok_or_else(|| AppendError::TargetNotFound(target.clone()))?;
            match t.kind {
                Kind::Retraction => return Err(AppendError::RetractionOfRetraction),
                Kind::Redaction => {
                    return Err(AppendError::InvalidTargetKind {
                        meta: Kind::Retraction,
                        target_kind: t.kind,
                    });
                }
                _ => {}
            }
            let src = match source {
                RetractionSource::Voice => Source::Voice,
                RetractionSource::System => Source::System,
            };
            let mut e = base(src, Kind::Retraction);
            e.target_event = Some(target);
            e
        }
    })
}

fn structural_mismatch(a: &Event, b: &Event) -> bool {
    let core_differs = a.v != b.v
        || a.session_id != b.session_id
        || a.ts != b.ts
        || a.source != b.source
        || a.kind != b.kind
        || a.target_event != b.target_event
        || a.linked_event != b.linked_event
        || a.targets != b.targets;
    if core_differs {
        return true;
    }
    if !a.scrubbed() && !b.scrubbed() {
        return a.text != b.text || a.payload != b.payload;
    }
    false
}

/// Second-stage merge validation: target-kind rules, when the target's kind
/// is determinable from the batch or the database. Unknown targets are
/// dangling — inert, never invalid (§5.1, §8).
fn target_kind_violation(
    conn: &Connection,
    e: &Event,
    incoming_kinds: &HashMap<&EventId, Kind>,
) -> Result<Option<String>, StoreError> {
    let Some(target) = e.target_event.as_ref() else {
        return Ok(None);
    };
    let known = match incoming_kinds.get(target) {
        Some(k) => Some(*k),
        None => {
            let s: Option<String> = conn
                .prepare_cached("SELECT kind FROM annotation_events WHERE id = ?1")?
                .query_row([target.as_str()], |r| r.get(0))
                .optional()?;
            s.and_then(|s| Kind::parse(&s))
        }
    };
    let Some(k) = known else { return Ok(None) };
    let allowed = match e.kind {
        Kind::Revision => matches!(k, Kind::Remark | Kind::Revision),
        Kind::Retraction => matches!(
            k,
            Kind::Remark | Kind::Rating | Kind::Stroke | Kind::Revision
        ),
        Kind::Redaction => matches!(k, Kind::Remark | Kind::Revision | Kind::Stroke),
        _ => true,
    };
    Ok(if allowed {
        None
    } else {
        Some(format!("{} may not target {}", e.kind.as_str(), k.as_str()))
    })
}

/// §8 steps 0–3 for one chunk of the (sorted) incoming union.
fn merge_chunk(
    tx: &Connection,
    chunk: &[(usize, &Event)],
    incoming_kinds: &HashMap<&EventId, Kind>,
    report: &mut MergeReport,
    touched: &mut Vec<Event>,
) -> Result<(), StoreError> {
    let mut ok: Vec<&Event> = Vec::with_capacity(chunk.len());
    for (offset, e) in chunk {
        match target_kind_violation(tx, e, incoming_kinds)? {
            Some(reason) => report.quarantined.push((*offset, reason)),
            None => ok.push(e),
        }
    }

    // 1. Redactions first — supremacy in force before content lands.
    //    Duplicates also enter `touched`: re-merging events whose earlier
    //    merge was interrupted before the §8 step-4 recompute must heal the
    //    derived tables, so duplicates may not short-circuit recomputation.
    for e in ok.iter().filter(|e| e.kind == Kind::Redaction) {
        let target = e.target_event.as_ref().expect("validated");
        match get_event(tx, &e.id)? {
            None => {
                insert_event(tx, e)?;
                report.inserted += 1;
                touched.push((*e).clone());
            }
            Some(local) => {
                report.duplicates += 1;
                touched.push(local);
            }
        }
        registry_upsert(tx, target, &e.id, e.ts)?;
        // 2. Scrub local victims of newly learned redactions.
        if let Some(victim) = get_event(tx, target)?
            && !victim.scrubbed()
            && matches!(victim.kind, Kind::Remark | Kind::Revision | Kind::Stroke)
        {
            let marker = registry_lookup(tx, &victim.id)?.expect("just upserted");
            scrub_in_place(tx, &victim.id, &marker)?;
            // Mark, never DELETE: the row's file pointer feeds the PPVEC
            // physical-zero sweep (RETRIEVAL §1.1).
            tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                .execute([victim.id.as_str()])?;
            report.newly_scrubbed += 1;
            touched.push(victim.scrubbed_form(marker));
        }
    }

    // 3. Union the rest (ascending id; order is cosmetic).
    for e in ok.iter().filter(|e| e.kind != Kind::Redaction) {
        let condemned = registry_lookup(tx, &e.id)?;
        let arriving = match condemned {
            Some(marker) if !e.scrubbed() => {
                report.scrubbed_on_arrival += 1;
                e.scrubbed_form(marker)
            }
            _ => (*e).clone(),
        };
        match get_event(tx, &arriving.id)? {
            None => {
                insert_event(tx, &arriving)?;
                report.inserted += 1;
                touched.push(arriving);
            }
            Some(local) => {
                if local.scrubbed() && !arriving.scrubbed() {
                    // Supremacy: keep local.
                    report.duplicates += 1;
                    if structural_mismatch(&local, &arriving) {
                        report.integrity_warnings.push(arriving.id.clone());
                    }
                    touched.push(local);
                } else if arriving.scrubbed() && !local.scrubbed() {
                    // Learn it. (Mark, never DELETE — RETRIEVAL §1.1.)
                    let marker = arriving.redacted_by.clone().expect("scrubbed");
                    scrub_in_place(tx, &local.id, &marker)?;
                    tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                        .execute([local.id.as_str()])?;
                    report.newly_scrubbed += 1;
                    touched.push(local.scrubbed_form(marker));
                } else {
                    report.duplicates += 1;
                    if structural_mismatch(&local, &arriving) {
                        // Same id MUST mean same event; local wins, warn.
                        report.integrity_warnings.push(arriving.id.clone());
                    }
                    // Duplicates still recompute (interrupted-merge healing).
                    touched.push(local);
                }
            }
        }
    }
    Ok(())
}

/// Distinct ids of the scrubbed events a merge touched — the §13.5
/// flat-file zeroing set. Includes scrubbed-on-arrival copies (they never
/// had vectors; zeroing finds no rows) so the set never under-covers.
fn scrubbed_event_ids(touched: &[Event]) -> Vec<EventId> {
    let mut ids: Vec<EventId> = touched
        .iter()
        .filter(|e| e.scrubbed())
        .map(|e| e.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The scrubbed events a merge touched, deduped by id — the §9.5 derived-
/// row propagation set (same never-under-covers posture as
/// [`scrubbed_event_ids`], but the propagation needs the events' sessions
/// and targets, not just ids).
fn scrubbed_events(touched: &[Event]) -> Vec<Event> {
    let mut events: Vec<Event> = touched.iter().filter(|e| e.scrubbed()).cloned().collect();
    events.sort_by(|a, b| a.id.cmp(&b.id));
    events.dedup_by(|a, b| a.id == b.id);
    events
}

/// §8 step 4: affected chain roots and images for the post-union recompute.
fn merge_affected(
    tx: &Connection,
    touched: &[Event],
) -> Result<(BTreeSet<EventId>, BTreeMap<ContentHash, DirtyReason>), StoreError> {
    let mut roots: BTreeSet<EventId> = BTreeSet::new();
    let mut dirty: BTreeMap<ContentHash, DirtyReason> = BTreeMap::new();
    for e in touched {
        let reason = if e.scrubbed() || e.kind == Kind::Redaction {
            DirtyReason::Redaction
        } else {
            DirtyReason::Append
        };
        for h in db_effective_targets(tx, e)? {
            let entry = dirty.entry(h).or_insert(reason);
            if reason == DirtyReason::Redaction {
                *entry = DirtyReason::Redaction;
            }
        }
        match e.kind {
            Kind::Remark => {
                roots.insert(e.id.clone());
            }
            Kind::Revision => {
                if let Some(root) = db_root_event(tx, e)?
                    && root.kind == Kind::Remark
                {
                    roots.insert(root.id);
                }
            }
            Kind::Retraction | Kind::Redaction => {
                let target = e.target_event.as_ref().expect("validated");
                if let Some(t) = get_event(tx, target)? {
                    match t.kind {
                        Kind::Remark => {
                            roots.insert(t.id.clone());
                        }
                        Kind::Revision => {
                            if let Some(root) = db_root_event(tx, &t)?
                                && root.kind == Kind::Remark
                            {
                                roots.insert(root.id);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Kind::Rating | Kind::Stroke => {}
        }
    }
    Ok((roots, dirty))
}

// ---------------------------------------------------------------------------
// Derived maintenance (§6.3): same transaction as the triggering write;
// scoped recomputation from the truth tables, so incremental state always
// equals `rebuild_derived()` (I7) and merge results are order-free (§8).
// ---------------------------------------------------------------------------

/// RETRIEVAL §9.5 / §1.1 redaction propagation into derived rows, in the
/// same transaction as the content scrub: delete the scrubbed events'
/// sentiment rows and every derived summary whose inputs may have included
/// a scrubbed event, with the summaries' `summaries_fts` rows; mark the
/// affected images' `image_summary` vectors dead (the caller zeroes their
/// flat-file bytes after commit — the §13.5 posture). A summary may
/// paraphrase the scrubbed content, so it must go the moment the call
/// returns, not lazily.
///
/// The schema stores no per-summary input list (`inputs_hash` is an opaque
/// cache key), so input membership is resolved by SCOPE, conservatively:
/// image summaries are generated from the image's events (§9.1) — deleted
/// for every image a scrubbed event targets; session summaries from the
/// session's events (§9.3) — deleted for every scrubbed event's session;
/// folder rollups from member images' summaries (§9.2) — the folder-path
/// keying belongs to the not-yet-built generation pass, so the
/// conservative cut is every folder row once any image summary went.
/// Over-deletion is safe by design: derived rows are disposable fuel (E4)
/// and regenerate in the background.
///
/// Returns the affected image hashes — the `image_summary` zeroing set.
fn purge_redacted_derived(
    tx: &Connection,
    scrubbed: &[Event],
) -> Result<Vec<ContentHash>, StoreError> {
    if scrubbed.is_empty() {
        return Ok(Vec::new());
    }
    let mut images: BTreeSet<ContentHash> = BTreeSet::new();
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    for e in scrubbed {
        tx.prepare_cached("DELETE FROM sentiment_scores WHERE event_id = ?1")?
            .execute([e.id.as_str()])?;
        sessions.insert(e.session_id.clone());
        for h in db_effective_targets(tx, e)? {
            images.insert(h);
        }
    }
    let delete_scope = |scope: &str, key: &str| -> Result<(), StoreError> {
        // FTS rows first: the subquery needs the summary rows still present.
        tx.prepare_cached(
            "DELETE FROM summaries_fts WHERE summary_id IN \
             (SELECT id FROM derived_summaries WHERE scope = ?1 AND scope_key = ?2)",
        )?
        .execute(params![scope, key])?;
        tx.prepare_cached("DELETE FROM derived_summaries WHERE scope = ?1 AND scope_key = ?2")?
            .execute(params![scope, key])?;
        Ok(())
    };
    for h in &images {
        delete_scope("image", h.as_str())?;
        // Mark, never DELETE (RETRIEVAL §1.1): the metadata row keeps the
        // flat-file pointer for the physical zeroing.
        tx.prepare_cached(
            "UPDATE vectors SET deleted = 1 \
             WHERE vec_kind = 'image_summary' AND image_hash = ?1",
        )?
        .execute([h.as_str()])?;
    }
    for s in &sessions {
        delete_scope("session", s.as_str())?;
    }
    if !images.is_empty() {
        tx.prepare_cached(
            "DELETE FROM summaries_fts WHERE summary_id IN \
             (SELECT id FROM derived_summaries WHERE scope = 'folder')",
        )?
        .execute([])?;
        tx.prepare_cached("DELETE FROM derived_summaries WHERE scope = 'folder'")?
            .execute([])?;
    }
    Ok(images.into_iter().collect())
}

fn recompute_derived(
    tx: &Connection,
    roots: &BTreeSet<EventId>,
    dirty: &BTreeMap<ContentHash, DirtyReason>,
) -> Result<(), StoreError> {
    if !roots.is_empty() {
        let ids: Vec<String> = roots.iter().map(|r| r.as_str().to_owned()).collect();
        let mut set = BTreeMap::new();
        fetch_joined_in(tx, "id", &ids, &mut set)?;
        closure_descendants(tx, &mut set)?;
        let fs = FoldSet::new(set);
        let retracted = fs.retracted_ids();
        let chains = fs.chains();
        for rid in roots {
            let Some(root) = fs.get(rid) else { continue };
            if root.kind != Kind::Remark {
                continue;
            }
            // fts_map rows are created for every remark root touched and
            // never deleted (stable rowids, §6.3).
            tx.prepare_cached("INSERT OR IGNORE INTO fts_map(root_event_id) VALUES (?1)")?
                .execute([rid.as_str()])?;
            let rowid: i64 = tx
                .prepare_cached("SELECT fts_rowid FROM fts_map WHERE root_event_id = ?1")?
                .query_row([rid.as_str()], |r| r.get(0))?;
            let old_body: Option<String> = tx
                .prepare_cached("SELECT body FROM event_fts WHERE rowid = ?1")?
                .query_row([rowid], |r| r.get(0))
                .optional()?;
            let live = !retracted.contains(rid) && !root.scrubbed();
            if live {
                let (text, _) = fs.effective_text(root, &chains, &retracted);
                let body = text.unwrap_or_default();
                match &old_body {
                    None => {
                        tx.prepare_cached("INSERT INTO event_fts(rowid, body) VALUES (?1, ?2)")?
                            .execute(params![rowid, body])?;
                    }
                    Some(b) if *b != body => {
                        tx.prepare_cached("UPDATE event_fts SET body = ?1 WHERE rowid = ?2")?
                            .execute(params![body, rowid])?;
                        // Effective text changed: invalidate the root's
                        // vectors for re-embedding (§6.3). Mark, never
                        // DELETE — the metadata row keeps the flat-file
                        // pointer for the PPVEC zeroing sweep
                        // (RETRIEVAL §1.1/§1.3).
                        tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                            .execute([rid.as_str()])?;
                    }
                    _ => {}
                }
            } else {
                if old_body.is_some() {
                    tx.prepare_cached("DELETE FROM event_fts WHERE rowid = ?1")?
                        .execute([rowid])?;
                }
                tx.prepare_cached("UPDATE vectors SET deleted = 1 WHERE event_id = ?1")?
                    .execute([rid.as_str()])?;
            }
        }
    }

    let now = UtcMillis::now();
    for (hash, reason) in dirty {
        recompute_image(tx, hash.as_str())?;
        upsert_dirty(tx, hash, *reason, now)?;
        // RETRIEVAL §1.1 "enqueue embed": any journal change re-pends the
        // image's text-embedding pass so re-annotation re-embeds (L4 queue
        // mechanics — LIBRARY §10). UPDATE-only by design: pass rows are
        // created by the library's embedding backfill; the events engine
        // never invents queue rows for images the library has not ingested.
        // 'running' is covered too: a drain mid-pass snapshotted the OLD
        // folded text, so this change must force a re-run — pending wins
        // over the in-flight pass (ingest::mark_done only completes rows
        // still 'running', so the stale pass cannot clobber this re-pend).
        tx.prepare_cached(
            "UPDATE ingest_passes SET state = 'pending', not_before = NULL \
             WHERE image_hash = ?1 AND pass_name = 'text-embedding' \
               AND state IN ('running','done','error','skipped')",
        )?
        .execute([hash.as_str()])?;
    }
    Ok(())
}

/// Recompute `image_ratings` + `image_journal_stats` for one image from the
/// truth tables (one batched query; retraction checks answered from
/// `idx_events_target`, no row fetch).
fn recompute_image(tx: &Connection, hash: &str) -> Result<(), StoreError> {
    let mut stmt = tx.prepare_cached(
        "SELECT e.id, e.kind, e.ts, e.redacted_by IS NOT NULL, e.payload, \
                NOT EXISTS (SELECT 1 FROM annotation_events r \
                            WHERE r.target_event = e.id AND r.kind = 'retraction') \
         FROM event_targets t JOIN annotation_events e ON e.id = t.event_id \
         WHERE t.image_hash = ?1 AND e.kind IN ('remark','rating','stroke') \
         ORDER BY e.id",
    )?;
    let mut rows = stmt.query([hash])?;
    let mut event_count: i64 = 0;
    let mut has_text = false;
    let mut has_strokes = false;
    // Heatmap stroke factor (DESIGN-ATTENTION-HEATMAP.md): the COUNT of live,
    // non-scrubbed strokes, maintained in this same recompute transaction as
    // `has_strokes`. A redaction scrubs the stroke (drops it from the count);
    // a retraction makes it non-live (skipped above), so both retract and
    // redact lower the count, exactly like event_count.
    let mut stroke_count: i64 = 0;
    let mut last_ts: Option<String> = None;
    let mut winner: Option<(String, String, i64)> = None; // (event_id, ts, value)
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let ts: String = row.get(2)?;
        let scrubbed: bool = row.get(3)?;
        let payload: Option<String> = row.get(4)?;
        let live: bool = row.get(5)?;
        if !live {
            continue;
        }
        event_count += 1;
        last_ts = Some(ts.clone());
        match kind.as_str() {
            // B34: words evidence, symmetric with `has_strokes` — live AND
            // non-scrubbed. A redacted remark renders as a "[redacted]" stub
            // (Q2) but carries no words; it lights no dot.
            "remark" if !scrubbed => has_text = true,
            "stroke" if !scrubbed => {
                has_strokes = true;
                stroke_count += 1;
            }
            "rating" if !scrubbed => {
                if let Some(p) = payload
                    && let Ok(Payload::Rating { value }) =
                        canonical::parse_payload_json(Kind::Rating, Source::Typed, &p)
                {
                    winner = Some((id, ts, i64::from(value)));
                }
            }
            _ => {}
        }
    }
    match winner {
        Some((event_id, ts, value)) => {
            tx.prepare_cached(
                "INSERT INTO image_ratings(image_hash, rating, event_id, ts) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(image_hash) DO UPDATE SET \
                   rating = excluded.rating, event_id = excluded.event_id, ts = excluded.ts",
            )?
            .execute(params![hash, value, event_id, ts])?;
        }
        None => {
            tx.prepare_cached("DELETE FROM image_ratings WHERE image_hash = ?1")?
                .execute([hash])?;
        }
    }
    if event_count > 0 {
        tx.prepare_cached(
            "INSERT INTO image_journal_stats(image_hash, event_count, has_text, has_strokes, stroke_count, last_ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(image_hash) DO UPDATE SET \
               event_count = excluded.event_count, has_text = excluded.has_text, \
               has_strokes = excluded.has_strokes, stroke_count = excluded.stroke_count, \
               last_ts = excluded.last_ts",
        )?
        .execute(params![
            hash,
            event_count,
            has_text as i64,
            has_strokes as i64,
            stroke_count,
            last_ts.expect("count > 0")
        ])?;
    } else {
        tx.prepare_cached("DELETE FROM image_journal_stats WHERE image_hash = ?1")?
            .execute([hash])?;
    }
    Ok(())
}

/// `'redaction'` outranks and overwrites other reasons for a hash, never
/// vice versa (§5.3). `since_ts` is a strictly increasing high-water mark:
/// every re-mark advances it to `max(now, previous + 1 ms)`, so an
/// `ack_dirty(image, observed_since_ts)` computed from an earlier
/// `dirty_images()` read can never delete a mark made after that read —
/// acks lose no dirtiness, even within one wall-clock millisecond.
fn upsert_dirty(
    tx: &Connection,
    hash: &ContentHash,
    reason: DirtyReason,
    now: UtcMillis,
) -> Result<(), StoreError> {
    let existing: Option<(String, String)> = tx
        .prepare_cached("SELECT reason, since_ts FROM sidecar_dirty WHERE image_hash = ?1")?
        .query_row([hash.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let (reason, ts) = match existing {
        None => (reason, now),
        Some((old_reason, old_ts)) => {
            let old = UtcMillis::parse(&old_ts).map_err(corrupt)?;
            let bumped = UtcMillis::from_epoch_ms(now.epoch_ms().max(old.epoch_ms() + 1));
            let kept_reason = if DirtyReason::parse(&old_reason) == Some(DirtyReason::Redaction) {
                DirtyReason::Redaction // never coalesced away by later writes
            } else {
                reason
            };
            (kept_reason, bumped)
        }
    };
    tx.prepare_cached(
        "INSERT OR REPLACE INTO sidecar_dirty(image_hash, reason, since_ts) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![hash.as_str(), reason.as_str(), ts.to_rfc3339()])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Session rows
// ---------------------------------------------------------------------------

fn insert_session(conn: &Connection, s: &SessionRecord) -> Result<(), StoreError> {
    conn.prepare_cached(
        "INSERT INTO sessions (id, started_ts, ended_ts, app_version, device_id, root_context) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        s.id.as_str(),
        s.started_ts.to_rfc3339(),
        s.ended_ts.map(|t| t.to_rfc3339()),
        s.app_version,
        s.device_id,
        s.root_context,
    ])?;
    Ok(())
}

fn get_session(conn: &Connection, id: &SessionId) -> Result<Option<SessionRecord>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, started_ts, ended_ts, app_version, device_id, root_context \
         FROM sessions WHERE id = ?1",
    )?;
    let row = stmt
        .query_row([id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()?;
    row.map(
        |(id, started, ended, app_version, device_id, root_context)| {
            Ok(SessionRecord {
                id: SessionId::from_str_strict(&id).map_err(corrupt)?,
                started_ts: UtcMillis::parse(&started).map_err(corrupt)?,
                ended_ts: ended
                    .map(|t| UtcMillis::parse(&t).map_err(corrupt))
                    .transpose()?,
                app_version,
                device_id,
                root_context,
            })
        },
    )
    .transpose()
}
