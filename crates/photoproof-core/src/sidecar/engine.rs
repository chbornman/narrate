//! The sidecar engine: the single write chokepoint (§9.5) tying the
//! EventStore to the files on disk — flush routing (adjacent / overflow /
//! deferred), reconciliation and re-matching (§10), redaction propagation
//! (§11), and overflow migration (§8.2).
//!
//! Volume identity and writability detection belong to spec/LIBRARY.md
//! (packet P2.2). This packet stubs that dependency behind the narrow
//! [`ImageLocator`] trait; the library layer will implement it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use thiserror::Error;

use crate::event::{Event, Kind};
use crate::id::{ContentHash, EventId, SessionId, UtcMillis};
use crate::store::{EventStore, MergeReport, RedactError, SessionRecord, StoreError};

use super::doc::{
    ImageSnapshot, ParsedSidecar, SessionEntry, SessionMeta, SidecarDoc, SidecarEntry,
    SidecarParseError, huskify_opaque, opaque_sort_key, parse_sidecar,
};
use super::paths::{
    corrupt_aside_path, image_filename_of_sidecar, is_sidecar_filename, is_temp_orphan,
    overflow_dir, overflow_path, session_journal_path, sidecar_path_for_image,
};
use super::writer::{Debouncer, WriteOutcome, clean_orphan_temps, write_atomic};

/// §9.4: device id recorded for sessions learned from sidecars during
/// merge/rebuild. Sidecars deliberately carry no machine ids (§3.2), so the
/// original is unrecoverable; 32 zero-hex marks "unknown device".
pub const UNKNOWN_DEVICE_ID: &str = "00000000000000000000000000000000";

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Redact(#[from] RedactError),
    #[error("invalid: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------
// Volume stub (owned by this packet; implemented for real by P2.2 Library)
// ---------------------------------------------------------------------------

/// Where an image's adjacent sidecar location stands right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjacentLocation {
    /// The image resolves to a path on a writable volume.
    Writable { image_path: PathBuf },
    /// The image resolves, but its location is unwritable (§9.4 permanent:
    /// EROFS, EACCES, read-only mount) — the overflow store is the live
    /// write target (§8). The image itself is usually still *readable*
    /// (`image_path`), so the overflow entry records the real filename
    /// (§8.1).
    Unwritable {
        image_path: Option<PathBuf>,
        volume_id: Option<String>,
    },
    /// The volume is offline or the hash is unknown to the library — the
    /// dirty entry persists; flush happens at mount (§9.4).
    Offline { volume_id: Option<String> },
}

/// The narrow slice of spec/LIBRARY.md this packet needs: hash → current
/// adjacent location. LIBRARY (P2.2) owns volume identity, writability
/// probing, and path resolution.
pub trait ImageLocator {
    fn locate(&self, image: &ContentHash) -> AdjacentLocation;
}

impl<T: ImageLocator + ?Sized> ImageLocator for &T {
    fn locate(&self, image: &ContentHash) -> AdjacentLocation {
        (**self).locate(image)
    }
}

/// In-memory locator for tests and pre-P2.2 wiring. Unknown hashes are
/// `Offline { volume_id: None }`.
#[derive(Debug, Default)]
pub struct MemoryLocator {
    map: Mutex<HashMap<ContentHash, AdjacentLocation>>,
}

impl MemoryLocator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, image: ContentHash, loc: AdjacentLocation) {
        self.map.lock().expect("locator mutex").insert(image, loc);
    }

    pub fn remove(&self, image: &ContentHash) {
        self.map.lock().expect("locator mutex").remove(image);
    }
}

impl ImageLocator for MemoryLocator {
    fn locate(&self, image: &ContentHash) -> AdjacentLocation {
        self.map
            .lock()
            .expect("locator mutex")
            .get(image)
            .cloned()
            .unwrap_or(AdjacentLocation::Offline { volume_id: None })
    }
}

// ---------------------------------------------------------------------------
// Reports and outcomes
// ---------------------------------------------------------------------------

/// §12.3 step 7 / §10: the integrity report, accumulated across
/// reconciliation scans and rebuilds. Always produced, never silent.
#[derive(Debug, Default)]
pub struct IntegrityReport {
    pub files_scanned: usize,
    pub files_parsed: usize,
    /// Parse failures: (path, error). Quarantined, never aborting the run.
    pub failures: Vec<(PathBuf, String)>,
    pub events_imported: usize,
    pub duplicates_deduped: usize,
    /// Redactions enforced on arrival (content discarded unread) plus
    /// locally-intact events newly scrubbed by incoming markers.
    pub redactions_enforced: usize,
    /// Files rewritten because they held unscrubbed content for a registry
    /// id, or were subsets of the union (convergence).
    pub converged_rewrites: Vec<PathBuf>,
    /// §10.2 same-id-different-content conflicts.
    pub id_conflicts: Vec<String>,
    /// §10.2: losing copies preserved verbatim — (event id, canonical JSON).
    pub conflict_losers: Vec<(String, String)>,
    /// §5.3 newer-version files: opaque and inviolable.
    pub newer_version_files: Vec<PathBuf>,
    /// Opaque entries (unknown kinds / future event versions) preserved.
    pub unknown_kinds_preserved: usize,
    /// §2.3 collisions: sidecar-named files that are not sidecars.
    pub collisions: Vec<PathBuf>,
    /// Corrupt files renamed aside: (original, aside).
    pub corrupt_renamed: Vec<(PathBuf, PathBuf)>,
    /// §12.3 step 4: session metadata mismatches (first-seen wins).
    pub session_mismatches: Vec<String>,
    /// Merge quarantines: (path, offset, reason).
    pub quarantined: Vec<(PathBuf, usize, String)>,
    /// Manifest cross-check discrepancies (§12.3, advisory).
    pub manifest_discrepancies: Vec<String>,
}

impl IntegrityReport {
    fn absorb_merge(&mut self, path: &Path, m: &MergeReport) {
        self.events_imported += m.inserted;
        self.duplicates_deduped += m.duplicates;
        self.redactions_enforced += m.scrubbed_on_arrival + m.newly_scrubbed;
        for id in &m.integrity_warnings {
            self.id_conflicts.push(id.to_string());
        }
        for (offset, reason) in &m.quarantined {
            self.quarantined
                .push((path.to_path_buf(), *offset, reason.clone()));
        }
    }
}

/// Result of flushing one image's journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Written (or skipped identical) beside the image.
    Adjacent {
        path: PathBuf,
        outcome: WriteOutcome,
    },
    /// Routed to the overflow store (§8): unwritable location or §2.3
    /// collision or §5.3 newer-version file in the slot.
    Overflow {
        path: PathBuf,
        outcome: WriteOutcome,
    },
    /// Volume offline: the durable dirty entry persists (§9.4).
    Deferred,
    /// Nothing to write: no journal for this image.
    NoJournal,
}

/// Receipt of a redaction propagation (§11). `overflow_records` are durable
/// before this function returns — i.e. before any UI confirmation.
#[derive(Debug, Default)]
pub struct RedactionReceipt {
    /// Redaction event ids minted (one per chain member).
    pub markers: Vec<EventId>,
    /// §11 step 2: scrubbed overflow records for unreachable targets.
    pub overflow_records: Vec<PathBuf>,
    /// §11 step 3: adjacent sidecars rewritten immediately.
    pub adjacent_rewritten: Vec<PathBuf>,
    /// Session journals rewritten (session-level targets).
    pub session_journals: Vec<PathBuf>,
    /// §11 step 4: queued (event, image) pairs for offline volumes.
    pub queued: Vec<(EventId, ContentHash)>,
}

/// Journal owner: an image hash or a session ULID. Keys the preservation
/// tables (`sidecar_unknown_fields`, `sidecar_opaque_events`).
#[derive(Clone, Copy)]
pub(crate) enum Owner<'a> {
    Image(&'a ContentHash),
    Session(&'a str),
}

impl Owner<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Owner::Image(_) => "image",
            Owner::Session(_) => "session",
        }
    }

    fn key(&self) -> &str {
        match self {
            Owner::Image(h) => h.as_str(),
            Owner::Session(s) => s,
        }
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

pub struct SidecarEngine<'s, L: ImageLocator> {
    pub(crate) store: &'s EventStore,
    /// Own connection to the same database for the P2.1 tables (the
    /// EventStore's pool is private; WAL makes a sibling connection safe).
    pub(crate) conn: Mutex<Connection>,
    pub(crate) app_data: PathBuf,
    pub(crate) locator: L,
    pub(crate) debouncer: Mutex<Debouncer>,
}

impl<'s, L: ImageLocator> SidecarEngine<'s, L> {
    /// `db_path` must be the path the `EventStore` was opened on (its
    /// migrations create the P2.1 tables).
    pub fn new(
        store: &'s EventStore,
        db_path: impl AsRef<Path>,
        app_data: impl Into<PathBuf>,
        locator: L,
    ) -> Result<Self, SidecarError> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        Ok(Self {
            store,
            conn: Mutex::new(conn),
            app_data: app_data.into(),
            locator,
            debouncer: Mutex::new(Debouncer::new()),
        })
    }

    pub fn app_data(&self) -> &Path {
        &self.app_data
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, SidecarError>,
    ) -> Result<T, SidecarError> {
        let conn = self.conn.lock().expect("sidecar conn mutex poisoned");
        f(&conn)
    }

    // -- side tables ---------------------------------------------------------

    pub(crate) fn snapshot_get(
        &self,
        image: &ContentHash,
    ) -> Result<Option<ImageSnapshot>, SidecarError> {
        self.with_conn(|c| {
            let row: Option<(String, i64)> = c
                .prepare_cached(
                    "SELECT filename, byte_size FROM sidecar_image_snapshots WHERE image_hash = ?1",
                )?
                .query_row([image.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            Ok(row.map(|(filename, byte_size)| ImageSnapshot {
                hash: image.clone(),
                filename,
                byte_size: byte_size.max(0) as u64,
            }))
        })
    }

    /// Refresh the advisory snapshot (§3.1) from the file on disk: hash
    /// always wins; filename/byte_size drift is corrected on rewrite.
    pub(crate) fn refresh_snapshot_from_disk(
        &self,
        image: &ContentHash,
        image_path: &Path,
    ) -> Result<(), SidecarError> {
        if let Ok(meta) = fs::metadata(image_path) {
            let filename = image_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.snapshot_put(
                &ImageSnapshot {
                    hash: image.clone(),
                    filename,
                    byte_size: meta.len(),
                },
                true,
            )?;
        }
        Ok(())
    }

    /// `refresh = true` (image reachable on disk) overwrites; otherwise the
    /// first-seen advisory snapshot wins until a refresh.
    pub(crate) fn snapshot_put(
        &self,
        snap: &ImageSnapshot,
        refresh: bool,
    ) -> Result<(), SidecarError> {
        self.with_conn(|c| {
            let sql = if refresh {
                "INSERT INTO sidecar_image_snapshots(image_hash, filename, byte_size) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(image_hash) DO UPDATE SET \
                   filename = excluded.filename, byte_size = excluded.byte_size"
            } else {
                "INSERT OR IGNORE INTO sidecar_image_snapshots(image_hash, filename, byte_size) \
                 VALUES (?1, ?2, ?3)"
            };
            c.prepare_cached(sql)?.execute(params![
                snap.hash.as_str(),
                snap.filename,
                snap.byte_size as i64
            ])?;
            Ok(())
        })
    }

    fn unknown_put(
        &self,
        owner: Owner<'_>,
        scope: &str,
        key: &str,
        value: &Value,
    ) -> Result<(), SidecarError> {
        let compact = super::serialize::to_compact_canonical(value);
        self.with_conn(|c| {
            c.prepare_cached(
                "INSERT OR IGNORE INTO sidecar_unknown_fields \
                 (owner_kind, owner_key, scope, key, value_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                owner.kind(),
                owner.key(),
                scope,
                key,
                String::from_utf8(compact).expect("canonical JSON is UTF-8")
            ])?;
            Ok(())
        })
    }

    fn unknowns_get(
        &self,
        owner: Owner<'_>,
        scope: &str,
    ) -> Result<BTreeMap<String, Value>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT key, value_json FROM sidecar_unknown_fields \
                 WHERE owner_kind = ?1 AND owner_key = ?2 AND scope = ?3 ORDER BY key",
            )?;
            let mut rows = stmt.query(params![owner.kind(), owner.key(), scope])?;
            let mut out = BTreeMap::new();
            while let Some(row) = rows.next()? {
                let k: String = row.get(0)?;
                let v: String = row.get(1)?;
                let value: Value = serde_json::from_str(&v)
                    .map_err(|e| SidecarError::Invalid(format!("stored unknown field: {e}")))?;
                out.insert(k, value);
            }
            Ok(out)
        })
    }

    /// Returns `true` when the entry is new (drives the integrity report's
    /// "unknown kinds preserved" count without double-counting rereads).
    fn opaque_put(&self, owner: Owner<'_>, value: &Value) -> Result<bool, SidecarError> {
        let sort_key = opaque_sort_key(value);
        // Redaction supremacy on arrival: an opaque blob whose id is in the
        // registry is stored as a husk; its content is discarded unread.
        let stored = match self.registry_marker_for(&sort_key)? {
            Some(marker) => huskify_opaque(value, &marker),
            None => value.clone(),
        };
        let compact = super::serialize::to_compact_canonical(&stored);
        self.with_conn(|c| {
            let n = c
                .prepare_cached(
                    "INSERT OR IGNORE INTO sidecar_opaque_events \
                     (owner_kind, owner_key, sort_key, body_json) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![
                    owner.kind(),
                    owner.key(),
                    sort_key,
                    String::from_utf8(compact).expect("canonical JSON is UTF-8")
                ])?;
            Ok(n > 0)
        })
    }

    fn opaque_get(&self, owner: Owner<'_>) -> Result<Vec<Value>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT body_json FROM sidecar_opaque_events \
                 WHERE owner_kind = ?1 AND owner_key = ?2 ORDER BY sort_key",
            )?;
            let mut rows = stmt.query(params![owner.kind(), owner.key()])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let body: String = row.get(0)?;
                out.push(
                    serde_json::from_str(&body)
                        .map_err(|e| SidecarError::Invalid(format!("stored opaque event: {e}")))?,
                );
            }
            Ok(out)
        })
    }

    fn registry_marker_for(&self, id: &str) -> Result<Option<String>, SidecarError> {
        self.with_conn(|c| {
            Ok(
                c.prepare_cached("SELECT redaction_id FROM redactions WHERE event_id = ?1")?
                    .query_row([id], |r| r.get(0))
                    .optional()?,
            )
        })
    }

    /// §10.1 step 3 backstop for opaque blobs: scrub every stored opaque
    /// body whose id has entered the redaction registry.
    pub(crate) fn husk_condemned_opaques(&self) -> Result<usize, SidecarError> {
        let rows: Vec<(String, String, String, String, String)> = self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT o.owner_kind, o.owner_key, o.sort_key, o.body_json, r.redaction_id \
                 FROM sidecar_opaque_events o JOIN redactions r ON r.event_id = o.sort_key",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ));
            }
            Ok(out)
        })?;
        let mut n = 0;
        for (owner_kind, owner_key, sort_key, body, marker) in rows {
            let value: Value = serde_json::from_str(&body)
                .map_err(|e| SidecarError::Invalid(format!("stored opaque event: {e}")))?;
            let husk = huskify_opaque(&value, &marker);
            if husk != value {
                let compact = super::serialize::to_compact_canonical(&husk);
                self.with_conn(|c| {
                    c.prepare_cached(
                        "UPDATE sidecar_opaque_events SET body_json = ?1 \
                         WHERE owner_kind = ?2 AND owner_key = ?3 AND sort_key = ?4",
                    )?
                    .execute(params![
                        String::from_utf8(compact).expect("canonical JSON is UTF-8"),
                        owner_kind,
                        owner_key,
                        sort_key
                    ])?;
                    Ok(())
                })?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Does any journal truth exist for this image (events, opaque entries,
    /// or preserved unknown fields)?
    pub fn has_journal(&self, image: &ContentHash) -> Result<bool, SidecarError> {
        if !self.store.events_for_image(image)?.is_empty() {
            return Ok(true);
        }
        self.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT (SELECT COUNT(*) FROM sidecar_opaque_events \
                          WHERE owner_kind = 'image' AND owner_key = ?1) \
                      + (SELECT COUNT(*) FROM sidecar_unknown_fields \
                          WHERE owner_kind = 'image' AND owner_key = ?1)",
                [image.as_str()],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }

    // -- document building (serialize from the index) --------------------------

    /// The canonical sidecar document for one image, from the index — the
    /// merge invariant makes the index the union of every journal ever read,
    /// so this is also the export serialization (§12.1).
    pub fn image_doc(&self, image: &ContentHash) -> Result<SidecarDoc, SidecarError> {
        let events = self.store.events_for_image(image)?;
        let snapshot = self.snapshot_get(image)?.unwrap_or_else(|| ImageSnapshot {
            hash: image.clone(),
            filename: String::new(),
            byte_size: 0,
        });
        let mut doc = SidecarDoc::new(Some(snapshot));
        let owner = Owner::Image(image);

        let mut session_ids: BTreeSet<String> = BTreeSet::new();
        for e in &events {
            session_ids.insert(e.session_id.as_str().to_owned());
        }
        doc.events = events.into_iter().map(SidecarEntry::Canonical).collect();
        for v in self.opaque_get(owner)? {
            doc.events.push(SidecarEntry::Opaque(v));
        }
        doc.events.sort_by_key(SidecarEntry::sort_key);

        for sid in session_ids {
            let meta = self.session_meta(owner, &sid)?;
            doc.sessions.insert(sid, SessionEntry::Meta(meta));
        }
        for (k, v) in self.unknowns_get(owner, "sessions")? {
            doc.sessions.entry(k).or_insert(SessionEntry::Opaque(v));
        }
        doc.unknown_top = self.unknowns_get(owner, "top")?;
        doc.unknown_image = self.unknowns_get(owner, "image")?;
        Ok(doc)
    }

    /// The session journal document (§7.2), or `None` when the session has
    /// no zero-target events (no file).
    pub fn session_doc(&self, session: &SessionId) -> Result<Option<SidecarDoc>, SidecarError> {
        let events = self.store.sessionlevel_events(session)?;
        let owner_key = session.as_str().to_owned();
        let owner = Owner::Session(&owner_key);
        let opaque = self.opaque_get(owner)?;
        let unknown_top = self.unknowns_get(owner, "top")?;
        if events.is_empty() && opaque.is_empty() && unknown_top.is_empty() {
            return Ok(None);
        }
        let mut doc = SidecarDoc::new(None);
        doc.events = events.into_iter().map(SidecarEntry::Canonical).collect();
        for v in opaque {
            doc.events.push(SidecarEntry::Opaque(v));
        }
        doc.events.sort_by_key(SidecarEntry::sort_key);
        let meta = self.session_meta(owner, session.as_str())?;
        doc.sessions
            .insert(session.as_str().to_owned(), SessionEntry::Meta(meta));
        for (k, v) in self.unknowns_get(owner, "sessions")? {
            doc.sessions.entry(k).or_insert(SessionEntry::Opaque(v));
        }
        doc.unknown_top = unknown_top;
        Ok(Some(doc))
    }

    fn session_meta(&self, owner: Owner<'_>, sid: &str) -> Result<SessionMeta, SidecarError> {
        let unknown = self.unknowns_get(owner, &format!("session:{sid}"))?;
        if let Ok(session_id) = SessionId::from_str_strict(sid)
            && let Some(rec) = self.store.session(&session_id)?
        {
            return Ok(SessionMeta {
                started_at: rec.started_ts,
                app_version: rec.app_version,
                unknown,
            });
        }
        // Session row unknown (events merged without their session's
        // sidecar context). Deterministic fallback: the ULID's embedded
        // timestamp; provenance honestly marked unknown.
        let ms = ulid_epoch_ms(sid).unwrap_or(0);
        Ok(SessionMeta {
            started_at: UtcMillis::from_epoch_ms(ms),
            app_version: "unknown".to_owned(),
            unknown,
        })
    }

    // -- ingest (reading is always the same operation, §10.1) -------------------

    /// Union a parsed document into the index: events (set-union with
    /// redaction supremacy via `EventStore::merge`), sessions, snapshot,
    /// unknown fields, opaque entries.
    pub(crate) fn ingest_doc(
        &self,
        doc: &SidecarDoc,
        source: &Path,
        report: &mut IntegrityReport,
    ) -> Result<MergeReport, SidecarError> {
        let events = doc.canonical_events();
        let mrep = self.store.merge(&events)?;
        report.absorb_merge(source, &mrep);
        self.ingest_side_tables(doc, report)?;
        Ok(mrep)
    }

    /// Everything `ingest_doc` does *except* the event merge: sessions
    /// (first-seen wins, mismatches reported, §12.3 step 4), the advisory
    /// snapshot, unknown-field preservation, opaque entries. Rebuild (§12.3)
    /// batches the event merge separately per the §8 insertion discipline
    /// and calls this per document.
    pub(crate) fn ingest_side_tables(
        &self,
        doc: &SidecarDoc,
        report: &mut IntegrityReport,
    ) -> Result<(), SidecarError> {
        let events = doc.canonical_events();
        let owner_key_storage;
        let owner = match &doc.image {
            Some(snap) => Owner::Image(&snap.hash),
            None => {
                owner_key_storage = doc
                    .sessions
                    .keys()
                    .next()
                    .cloned()
                    .or_else(|| events.first().map(|e| e.session_id.as_str().to_owned()))
                    .unwrap_or_default();
                Owner::Session(&owner_key_storage)
            }
        };

        for (ulid, entry) in &doc.sessions {
            match entry {
                SessionEntry::Meta(m) => {
                    match SessionId::from_str_strict(ulid) {
                        Ok(sid) => {
                            match self.store.session(&sid)? {
                                None => {
                                    self.store.merge_sessions(&[SessionRecord {
                                        id: sid,
                                        started_ts: m.started_at,
                                        ended_ts: None,
                                        app_version: m.app_version.clone(),
                                        device_id: UNKNOWN_DEVICE_ID.to_owned(),
                                        root_context: None,
                                    }])?;
                                }
                                Some(local) => {
                                    // §12.3 step 4: identical ids must agree;
                                    // mismatches reported, first-seen wins.
                                    if local.started_ts != m.started_at
                                        || local.app_version != m.app_version
                                    {
                                        report.session_mismatches.push(ulid.clone());
                                    }
                                }
                            }
                            for (k, v) in &m.unknown {
                                self.unknown_put(owner, &format!("session:{ulid}"), k, v)?;
                            }
                        }
                        Err(_) => {
                            // Non-ULID session key: preserve whole entry.
                            let mut obj = serde_json::Map::new();
                            obj.insert(
                                "started_at".to_owned(),
                                Value::String(m.started_at.to_rfc3339()),
                            );
                            obj.insert(
                                "app_version".to_owned(),
                                Value::String(m.app_version.clone()),
                            );
                            for (k, v) in &m.unknown {
                                obj.insert(k.clone(), v.clone());
                            }
                            self.unknown_put(owner, "sessions", ulid, &Value::Object(obj))?;
                        }
                    }
                }
                SessionEntry::Opaque(v) => {
                    self.unknown_put(owner, "sessions", ulid, v)?;
                }
            }
        }

        for (k, v) in &doc.unknown_top {
            self.unknown_put(owner, "top", k, v)?;
        }
        for (k, v) in &doc.unknown_image {
            self.unknown_put(owner, "image", k, v)?;
        }
        if let Some(snap) = &doc.image {
            self.snapshot_put(snap, false)?;
        }
        for entry in &doc.events {
            if let SidecarEntry::Opaque(v) = entry
                && self.opaque_put(owner, v)?
            {
                report.unknown_kinds_preserved += 1;
            }
        }
        // Newly learned redaction markers may condemn stored opaque blobs.
        self.husk_condemned_opaques()?;
        Ok(())
    }

    // -- flush (§9) --------------------------------------------------------------

    /// Capture-layer hook: an event was appended for this image (the SQLite
    /// commit already happened; this schedules the sidecar mirror).
    pub fn note_event(&self, image: &ContentHash, now: UtcMillis) {
        self.debouncer
            .lock()
            .expect("debouncer mutex")
            .note_event(image, now);
    }

    /// One debounce tick: sync the durable dirty queue into the debouncer,
    /// flush everything due. Drives both normal capture mirroring and crash
    /// recovery.
    pub fn pump(&self, now: UtcMillis) -> Result<Vec<(ContentHash, FlushOutcome)>, SidecarError> {
        let rows = self.store.dirty_images()?;
        let due = {
            let mut deb = self.debouncer.lock().expect("debouncer mutex");
            deb.sync_from_queue(&rows, now);
            deb.due(now)
        };
        self.flush_set(&due, now)
    }

    /// Immediate full drain (§9.1: shutdown, export).
    pub fn flush_all(
        &self,
        now: UtcMillis,
    ) -> Result<Vec<(ContentHash, FlushOutcome)>, SidecarError> {
        let rows = self.store.dirty_images()?;
        let all = {
            let mut deb = self.debouncer.lock().expect("debouncer mutex");
            deb.sync_from_queue(&rows, now);
            deb.drain_all()
        };
        self.flush_set(&all, now)
    }

    fn flush_set(
        &self,
        images: &[ContentHash],
        now: UtcMillis,
    ) -> Result<Vec<(ContentHash, FlushOutcome)>, SidecarError> {
        let mut out = Vec::new();
        for h in images {
            match self.flush_image(h, now) {
                Ok(outcome) => {
                    self.debouncer.lock().expect("debouncer mutex").clear(h);
                    out.push((h.clone(), outcome));
                }
                Err(e) => {
                    // §9.4 transient: stays in the durable queue; backoff.
                    self.debouncer
                        .lock()
                        .expect("debouncer mutex")
                        .note_failure(h, now, e.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Flush one image's journal to its live write target (§7.1, §9).
    pub fn flush_image(
        &self,
        image: &ContentHash,
        now: UtcMillis,
    ) -> Result<FlushOutcome, SidecarError> {
        if !self.has_journal(image)? {
            self.ack_dirty_row(image)?;
            return Ok(FlushOutcome::NoJournal);
        }
        match self.locator.locate(image) {
            AdjacentLocation::Writable { image_path } => {
                let mut report = IntegrityReport::default();
                self.flush_adjacent(image, &image_path, now, &mut report)
            }
            AdjacentLocation::Unwritable { image_path, .. } => {
                // §8.1: the overflow entry still records the real filename.
                if let Some(p) = image_path {
                    self.refresh_snapshot_from_disk(image, &p)?;
                }
                let (path, outcome) = self.write_overflow(image)?;
                self.ack_dirty_row(image)?;
                Ok(FlushOutcome::Overflow { path, outcome })
            }
            AdjacentLocation::Offline { .. } => Ok(FlushOutcome::Deferred),
        }
    }

    /// The adjacent write path: §1.5 never-destroy (existing file content is
    /// unioned first), §2.3 collision, §5.3 newer-version, §10.3 corrupt and
    /// hash-mismatch handling, §9.2 atomic mtime-stable replace, defensive
    /// re-verify around `ack_dirty`.
    pub(crate) fn flush_adjacent(
        &self,
        image: &ContentHash,
        image_path: &Path,
        now: UtcMillis,
        report: &mut IntegrityReport,
    ) -> Result<FlushOutcome, SidecarError> {
        // Refresh the advisory snapshot from disk (§3.1).
        self.refresh_snapshot_from_disk(image, image_path)?;

        let target = sidecar_path_for_image(image_path);
        if target.exists() {
            let bytes = fs::read(&target)?;
            match parse_sidecar(&bytes) {
                Ok(ParsedSidecar::Doc(doc)) => {
                    match &doc.image {
                        Some(snap) if snap.hash == *image => {
                            // Union before replace: never destroy (§1.5).
                            self.ingest_doc(&doc, &target, report)?;
                        }
                        Some(snap) => {
                            // §10.3 swap row: preserve the embedded-hash
                            // journal first, verifiably, then own the slot.
                            let other = snap.hash.clone();
                            self.ingest_doc(&doc, &target, report)?;
                            self.rehome_journal(&other, report)?;
                        }
                        None => {
                            // A session journal in an adjacent slot: import
                            // its truth, then own the slot.
                            self.ingest_doc(&doc, &target, report)?;
                        }
                    }
                }
                Ok(ParsedSidecar::NewerVersion { .. }) => {
                    // §5.3: opaque and inviolable; new events route to
                    // overflow under the current schema version.
                    report.newer_version_files.push(target.clone());
                    let (path, outcome) = self.write_overflow(image)?;
                    self.ack_dirty_row(image)?;
                    return Ok(FlushOutcome::Overflow { path, outcome });
                }
                Ok(ParsedSidecar::Manifest) => {
                    // A manifest in the slot is not ours to claim: treat as
                    // a §2.3 collision (never overwrite or rename).
                    report.collisions.push(target.clone());
                    let (path, outcome) = self.write_overflow(image)?;
                    self.ack_dirty_row(image)?;
                    return Ok(FlushOutcome::Overflow { path, outcome });
                }
                Err(e) => match classify_unparseable(&bytes, &e) {
                    Unparseable::Collision => {
                        // §2.3 collision: MUST NOT overwrite or rename.
                        report.collisions.push(target.clone());
                        let (path, outcome) = self.write_overflow(image)?;
                        self.ack_dirty_row(image)?;
                        return Ok(FlushOutcome::Overflow { path, outcome });
                    }
                    Unparseable::Corrupt => {
                        // §10.3: corrupt — rename aside, never delete.
                        let aside = corrupt_aside_path(&target, now);
                        fs::rename(&target, &aside)?;
                        report.corrupt_renamed.push((target.clone(), aside));
                    }
                },
            }
        }

        // Write/converge with a defensive loop around ack: re-serialize
        // after acking and rewrite if the journal moved underneath us
        // (never assume clean after ack).
        let mut outcome = WriteOutcome::SkippedIdentical;
        for _ in 0..3 {
            let bytes = self.image_doc(image)?.to_bytes();
            outcome = write_atomic(&target, &bytes)?;
            self.ack_dirty_row(image)?;
            let again = self.image_doc(image)?.to_bytes();
            if again == bytes {
                break;
            }
        }
        self.case_rename_cleanup(&target, image)?;
        Ok(FlushOutcome::Adjacent {
            path: target,
            outcome,
        })
    }

    /// §2.2: a case-only rename of the image leaves an old-case sidecar
    /// behind; after the new name is written and verified, delete the old.
    fn case_rename_cleanup(&self, target: &Path, image: &ContentHash) -> Result<(), SidecarError> {
        let Some(dir) = target.parent() else {
            return Ok(());
        };
        let target_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Verify the new name parses before deleting any old name.
        let Ok(new_bytes) = fs::read(target) else {
            return Ok(());
        };
        if !matches!(parse_sidecar(&new_bytes), Ok(ParsedSidecar::Doc(_))) {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != target_name
                && name.eq_ignore_ascii_case(&target_name)
                && is_sidecar_filename(&name)
                && let Ok(b) = fs::read(entry.path())
                && let Ok(ParsedSidecar::Doc(doc)) = parse_sidecar(&b)
                && doc.image.as_ref().map(|s| &s.hash) == Some(image)
            {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// §10.3 swap step (a): preserve the displaced journal, verifiably,
    /// before the caller replaces the mismatched slot. The journal is
    /// written to the overflow store — always reachable, and immune to the
    /// mutual-rehoming recursion two swapped images would otherwise cause —
    /// and the same scan's overflow migration (§8.2) then places it adjacent
    /// to its writable image. Verified by re-read (parse + hash match).
    fn rehome_journal(
        &self,
        image: &ContentHash,
        report: &mut IntegrityReport,
    ) -> Result<(), SidecarError> {
        let (path, _) = self.write_overflow(image)?;
        let bytes = fs::read(&path)?;
        match parse_sidecar(&bytes) {
            Ok(ParsedSidecar::Doc(doc)) if doc.image.as_ref().map(|s| &s.hash) == Some(image) => {
                report.converged_rewrites.push(path);
                Ok(())
            }
            _ => Err(SidecarError::Invalid(format!(
                "rehomed journal failed verification: {}",
                path.display()
            ))),
        }
    }

    /// Write one image's journal to the overflow store (§8.1): identical
    /// format, hash-named fan-out, real `image.filename` in the snapshot.
    pub(crate) fn write_overflow(
        &self,
        image: &ContentHash,
    ) -> Result<(PathBuf, WriteOutcome), SidecarError> {
        let path = overflow_path(&self.app_data, image);
        let bytes = self.image_doc(image)?.to_bytes();
        let outcome = write_atomic(&path, &bytes)?;
        Ok((path, outcome))
    }

    /// Write/refresh a session journal (§7.2). `None` when the session has
    /// no zero-target events (no file).
    pub fn flush_session(
        &self,
        session: &SessionId,
    ) -> Result<Option<(PathBuf, WriteOutcome)>, SidecarError> {
        match self.session_doc(session)? {
            None => Ok(None),
            Some(doc) => {
                let path = session_journal_path(&self.app_data, session);
                let outcome = write_atomic(&path, &doc.to_bytes())?;
                Ok(Some((path, outcome)))
            }
        }
    }

    pub(crate) fn ack_dirty_row(&self, image: &ContentHash) -> Result<(), SidecarError> {
        let rows = self.store.dirty_images()?;
        if let Some(row) = rows.iter().find(|r| r.image == *image) {
            self.store.ack_dirty(image, row.since_ts)?;
        }
        Ok(())
    }

    // -- redaction propagation (§11) ---------------------------------------------

    /// §11: index scrub (synchronous, via `EventStore::redact`), durable
    /// scrubbed overflow record for every unreachable target *before this
    /// returns* (= before any UI confirmation), immediate adjacent rewrite,
    /// queue rows for offline/unwritable copies.
    pub fn redact(
        &self,
        target: &EventId,
        now: UtcMillis,
    ) -> Result<RedactionReceipt, SidecarError> {
        // Step 1: index scrub, registry, FTS/vector purge, dirty queue.
        let markers = self.store.redact(target)?;
        let mut receipt = RedactionReceipt {
            markers: markers.clone(),
            ..RedactionReceipt::default()
        };
        let victim = self
            .store
            .raw_event(target)?
            .ok_or_else(|| SidecarError::Invalid(format!("redacted event vanished: {target}")))?;
        let targets = self.effective_targets_db(&victim)?;
        let mut members: Vec<EventId> = Vec::new();
        for m in &markers {
            if let Some(r) = self.store.raw_event(m)?
                && let Some(t) = r.target_event
            {
                members.push(t);
            }
        }

        if targets.is_empty() {
            // Session-level: the session journal is the home (§7.2); it
            // lives in app data and is always reachable.
            if let Some((path, _)) = self.flush_session(&victim.session_id)? {
                receipt.session_journals.push(path);
            }
            return Ok(receipt);
        }

        // Step 2: durable scrubbed records for unreachable targets, before
        // anything else (and before the caller confirms to the user).
        let mut unreachable: Vec<(ContentHash, Option<String>)> = Vec::new();
        let mut reachable: Vec<(ContentHash, PathBuf)> = Vec::new();
        for h in &targets {
            match self.locator.locate(h) {
                AdjacentLocation::Writable { image_path } => {
                    reachable.push((h.clone(), image_path));
                }
                AdjacentLocation::Unwritable { volume_id, .. }
                | AdjacentLocation::Offline { volume_id } => {
                    let (path, _) = self.write_overflow(h)?;
                    receipt.overflow_records.push(path);
                    unreachable.push((h.clone(), volume_id));
                }
            }
        }

        // Step 3: adjacent rewrite, immediate (bypasses debounce).
        let mut report = IntegrityReport::default();
        for (h, image_path) in &reachable {
            if let FlushOutcome::Adjacent { path, .. } =
                self.flush_adjacent(h, image_path, now, &mut report)?
            {
                receipt.adjacent_rewritten.push(path);
            }
            self.debouncer.lock().expect("debouncer mutex").clear(h);
        }

        // Step 4: queue for offline/unwritable volumes.
        for (h, volume_id) in &unreachable {
            for member in &members {
                self.queue_redaction(member, h, volume_id.as_deref(), now)?;
                receipt.queued.push((member.clone(), h.clone()));
            }
        }
        Ok(receipt)
    }

    fn queue_redaction(
        &self,
        event: &EventId,
        image: &ContentHash,
        volume_id: Option<&str>,
        now: UtcMillis,
    ) -> Result<(), SidecarError> {
        self.with_conn(|c| {
            c.prepare_cached(
                "INSERT OR IGNORE INTO redaction_queue \
                 (event_id, image_hash, volume_id, queued_at) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![
                event.as_str(),
                image.as_str(),
                volume_id.unwrap_or(""),
                now.to_rfc3339()
            ])?;
            Ok(())
        })
    }

    /// All pending propagation rows.
    pub fn redaction_queue(&self) -> Result<Vec<(EventId, ContentHash)>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT event_id, image_hash FROM redaction_queue ORDER BY image_hash, event_id",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let e: String = row.get(0)?;
                let h: String = row.get(1)?;
                out.push((
                    EventId::from_str_strict(&e)
                        .map_err(|er| SidecarError::Invalid(er.to_string()))?,
                    ContentHash::from_hex(&h)
                        .map_err(|er| SidecarError::Invalid(er.to_string()))?,
                ));
            }
            Ok(out)
        })
    }

    /// §11 step 4 drain (at volume mount and each reconciliation scan):
    /// rewrite, verify by re-read — the event must appear only as a marker
    /// — then dequeue. Failures leave the row for the next attempt.
    pub fn drain_redaction_queue(&self, now: UtcMillis) -> Result<usize, SidecarError> {
        let rows = self.redaction_queue()?;
        let mut by_image: BTreeMap<ContentHash, Vec<EventId>> = BTreeMap::new();
        for (e, h) in rows {
            by_image.entry(h).or_default().push(e);
        }
        let mut drained = 0;
        for (image, events) in by_image {
            let AdjacentLocation::Writable { image_path } = self.locator.locate(&image) else {
                continue;
            };
            let mut report = IntegrityReport::default();
            let outcome = self.flush_adjacent(&image, &image_path, now, &mut report)?;
            let path = match outcome {
                FlushOutcome::Adjacent { path, .. } => path,
                // Routed to overflow (collision/newer file): the durable
                // scrubbed record there is the live target; the adjacent
                // copy stays queued.
                _ => continue,
            };
            let bytes = fs::read(&path)?;
            let Ok(ParsedSidecar::Doc(doc)) = parse_sidecar(&bytes) else {
                continue;
            };
            for event in events {
                if sidecar_event_is_scrubbed(&doc, &event) {
                    self.with_conn(|c| {
                        c.prepare_cached(
                            "DELETE FROM redaction_queue WHERE event_id = ?1 AND image_hash = ?2",
                        )?
                        .execute(params![event.as_str(), image.as_str()])?;
                        Ok(())
                    })?;
                    drained += 1;
                }
            }
        }
        Ok(drained)
    }

    /// `effective_targets(e)` against the store (§2.3); dangling ⇒ [].
    fn effective_targets_db(&self, e: &Event) -> Result<Vec<ContentHash>, SidecarError> {
        let mut seen: BTreeSet<EventId> = BTreeSet::new();
        let mut cur = e.clone();
        while cur.kind.is_meta() {
            if !seen.insert(cur.id.clone()) {
                return Ok(Vec::new());
            }
            let Some(target) = cur.target_event.clone() else {
                return Ok(Vec::new());
            };
            match self.store.raw_event(&target)? {
                Some(next) => cur = next,
                None => return Ok(Vec::new()),
            }
        }
        Ok(cur.targets)
    }

    // -- overflow migration (§8.2) --------------------------------------------

    /// At volume mount and at every reconciliation scan: union-merge each
    /// overflow entry whose image now resolves writable, write adjacent,
    /// read back and re-validate, only then delete the overflow entry.
    /// Idempotent; failures leave the entry for the next scan.
    pub fn migrate_overflow(
        &self,
        now: UtcMillis,
        report: &mut IntegrityReport,
    ) -> Result<usize, SidecarError> {
        let dir = overflow_dir(&self.app_data);
        let mut migrated = 0;
        for path in walk_sidecar_files(&dir)? {
            let bytes = fs::read(&path)?;
            let Ok(ParsedSidecar::Doc(doc)) = parse_sidecar(&bytes) else {
                continue;
            };
            let Some(snap) = &doc.image else { continue };
            let hash = snap.hash.clone();
            let AdjacentLocation::Writable { image_path } = self.locator.locate(&hash) else {
                continue;
            };
            // (1) union the overflow entry; (2) flush_adjacent also unions
            // any existing adjacent sidecar before writing the merged result.
            self.ingest_doc(&doc, &path, report)?;
            let outcome = self.flush_adjacent(&hash, &image_path, now, report)?;
            let adjacent = match outcome {
                FlushOutcome::Adjacent { path, .. } => path,
                _ => continue, // collision etc.: overflow stays the target
            };
            // (3) read back and re-validate: parse + hash match.
            let back = fs::read(&adjacent)?;
            let ok = matches!(
                parse_sidecar(&back),
                Ok(ParsedSidecar::Doc(d)) if d.image.as_ref().map(|s| &s.hash) == Some(&hash)
            );
            if ok {
                // (4) only then delete the overflow entry.
                fs::remove_file(&path)?;
                migrated += 1;
            }
        }
        Ok(migrated)
    }

    /// Convenience for "a volume mounted": migrate overflow entries, drain
    /// the redaction queue, flush anything dirty that is now reachable.
    pub fn on_volume_mount(&self, now: UtcMillis) -> Result<IntegrityReport, SidecarError> {
        let mut report = IntegrityReport::default();
        self.migrate_overflow(now, &mut report)?;
        self.drain_redaction_queue(now)?;
        self.flush_all(now)?;
        Ok(report)
    }

    // -- reconciliation (§9.5, §10) ----------------------------------------------

    /// Reconcile one `*.photoproof.json` file: validate, bind by embedded
    /// hash, union, converge bytes, handle every §10.3 row.
    pub fn reconcile_file(
        &self,
        path: &Path,
        now: UtcMillis,
        report: &mut IntegrityReport,
    ) -> Result<(), SidecarError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_temp_orphan(&name) || !is_sidecar_filename(&name) {
            return Ok(());
        }
        report.files_scanned += 1;
        let bytes = fs::read(path)?;
        match parse_sidecar(&bytes) {
            Ok(ParsedSidecar::Manifest) => Ok(()),
            Ok(ParsedSidecar::NewerVersion { .. }) => {
                // §5.3: opaque and inviolable — never rewritten, renamed,
                // or imported; reported.
                report.newer_version_files.push(path.to_path_buf());
                Ok(())
            }
            Err(e) => match classify_unparseable(&bytes, &e) {
                Unparseable::Collision => {
                    // §2.3: a user file wearing our suffix. Never touched;
                    // re-checked at every scan.
                    report.collisions.push(path.to_path_buf());
                    Ok(())
                }
                Unparseable::Corrupt => {
                    // §10.3: corrupt — rename aside, regenerate from the
                    // index if a journal exists, report.
                    report.failures.push((path.to_path_buf(), e.to_string()));
                    let aside = corrupt_aside_path(path, now);
                    fs::rename(path, &aside)?;
                    report.corrupt_renamed.push((path.to_path_buf(), aside));
                    if let Some(prefix) = image_filename_of_sidecar(&name) {
                        let image_path = path.with_file_name(prefix);
                        if image_path.exists() {
                            let h = hash_file(&image_path)?;
                            if self.has_journal(&h)? {
                                self.flush_adjacent(&h, &image_path, now, report)?;
                            }
                        }
                    }
                    Ok(())
                }
            },
            Ok(ParsedSidecar::Doc(doc)) => {
                report.files_parsed += 1;
                self.ingest_doc(&doc, path, report)?;
                self.converge_file(&doc, path, &bytes, &name, now, report)
            }
        }
    }

    /// Post-union placement/convergence for one parsed file.
    fn converge_file(
        &self,
        doc: &SidecarDoc,
        path: &Path,
        original: &[u8],
        name: &str,
        now: UtcMillis,
        report: &mut IntegrityReport,
    ) -> Result<(), SidecarError> {
        match &doc.image {
            None => {
                // Session journal: converge in place.
                let sid = doc
                    .sessions
                    .keys()
                    .next()
                    .cloned()
                    .or_else(|| {
                        doc.canonical_events()
                            .first()
                            .map(|e| e.session_id.as_str().to_owned())
                    })
                    .unwrap_or_default();
                if let Ok(session) = SessionId::from_str_strict(&sid)
                    && let Some(canon) = self.session_doc(&session)?
                {
                    let cb = canon.to_bytes();
                    if cb != original && write_atomic(path, &cb)? == WriteOutcome::Written {
                        report.converged_rewrites.push(path.to_path_buf());
                    }
                }
                Ok(())
            }
            Some(snap) => {
                let h = &snap.hash;
                let prefix = image_filename_of_sidecar(name).unwrap_or_default();
                if prefix == h.as_str() {
                    // Hash-named copy (overflow/export): converge in place.
                    return self.converge_in_place(h, path, original, report);
                }
                let image_path = path.with_file_name(prefix);
                if !image_path.exists() {
                    // §10.3 row 1: sidecar found, image absent — journal
                    // imported; the orphan file is left in place untouched.
                    return Ok(());
                }
                let actual = hash_file(&image_path)?;
                if actual == *h {
                    // §10.3 stale/foreign/snapshot-drift rows: union already
                    // done; flush_adjacent refreshes the snapshot, converges
                    // to the superset bytes (mtime-stable when identical),
                    // and acks the dirty row with a defensive re-read.
                    if let FlushOutcome::Adjacent {
                        path: written,
                        outcome: WriteOutcome::Written,
                    } = self.flush_adjacent(h, &image_path, now, report)?
                    {
                        report.converged_rewrites.push(written);
                    }
                    Ok(())
                } else {
                    // §10.3 swap row: never bind by adjacency. (a) the
                    // embedded journal was imported above; rehome it,
                    // verified. (b) only then rewrite the slot for the image
                    // actually present, if it has a journal.
                    self.rehome_journal(h, report)?;
                    if self.has_journal(&actual)? {
                        self.flush_adjacent(&actual, &image_path, now, report)?;
                        report.converged_rewrites.push(path.to_path_buf());
                    }
                    Ok(())
                }
            }
        }
    }

    fn converge_in_place(
        &self,
        image: &ContentHash,
        path: &Path,
        original: &[u8],
        report: &mut IntegrityReport,
    ) -> Result<(), SidecarError> {
        let cb = self.image_doc(image)?.to_bytes();
        if cb != original && write_atomic(path, &cb)? == WriteOutcome::Written {
            report.converged_rewrites.push(path.to_path_buf());
        }
        Ok(())
    }

    /// Reconciliation scan over directory roots: reconcile every sidecar,
    /// clean orphan temps older than one hour (§9.3), migrate overflow,
    /// drain the redaction queue, retry the dirty queue once (§9.4), and
    /// regenerate missing/stale adjacent sidecars and session journals from
    /// the index (§9.5, §10.3 row 2). Every write is mtime-stable, so a
    /// converged library is left byte- and mtime-untouched (§13.11).
    pub fn scan(&self, roots: &[PathBuf], now: UtcMillis) -> Result<IntegrityReport, SidecarError> {
        let mut report = IntegrityReport::default();
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for root in roots {
            for dir in walk_dirs(root)? {
                let _ = clean_orphan_temps(&dir, cutoff);
            }
            for file in walk_sidecar_files(root)? {
                self.reconcile_file(&file, now, &mut report)?;
            }
        }
        for dir in [
            overflow_dir(&self.app_data),
            super::paths::sessions_dir(&self.app_data),
        ] {
            if dir.is_dir() {
                for d in walk_dirs(&dir)? {
                    let _ = clean_orphan_temps(&d, cutoff);
                }
            }
        }
        self.migrate_overflow(now, &mut report)?;
        self.drain_redaction_queue(now)?;
        // §9.4: transients past the backoff ladder retry once per scan.
        self.flush_all(now)?;
        // §9.5/§10.3 row 2: an image whose adjacent sidecar is missing or
        // stale relative to the index (user deletion, restored old backup,
        // unscrubbed registry id, merge superset) is rewritten from the
        // index. The writer is the single chokepoint.
        for image in self.annotated_images()? {
            if let AdjacentLocation::Writable { image_path } = self.locator.locate(&image)
                && let FlushOutcome::Adjacent {
                    path,
                    outcome: WriteOutcome::Written,
                } = self.flush_adjacent(&image, &image_path, now, &mut report)?
            {
                report.converged_rewrites.push(path);
            }
        }
        // §7.2: session journals are written by the same writer and covered
        // by the same reconciliation.
        for sid in self.session_journal_owners()? {
            if let Some((path, WriteOutcome::Written)) = self.flush_session(&sid)? {
                report.converged_rewrites.push(path);
            }
        }
        Ok(report)
    }

    /// Sessions that own (or may own) a session journal: every session known
    /// to the store plus orphan owners learned from preserved entries.
    pub(crate) fn session_journal_owners(&self) -> Result<Vec<SessionId>, SidecarError> {
        let mut out = self.list_sessions()?;
        for owner in self.orphan_session_owners()? {
            if let Ok(sid) = SessionId::from_str_strict(&owner)
                && !out.contains(&sid)
            {
                out.push(sid);
            }
        }
        out.sort();
        Ok(out)
    }

    /// All session ids known to the store (session journals + export feed).
    pub(crate) fn list_sessions(&self) -> Result<Vec<SessionId>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached("SELECT id FROM sessions ORDER BY id")?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                out.push(
                    SessionId::from_str_strict(&id)
                        .map_err(|e| SidecarError::Invalid(e.to_string()))?,
                );
            }
            Ok(out)
        })
    }

    /// Session ULIDs owning preserved opaque/unknown entries but absent
    /// from the sessions table (rebuild completeness).
    pub(crate) fn orphan_session_owners(&self) -> Result<Vec<String>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT DISTINCT owner_key FROM (\
                   SELECT owner_key, owner_kind FROM sidecar_opaque_events \
                   UNION SELECT owner_key, owner_kind FROM sidecar_unknown_fields) \
                 WHERE owner_kind = 'session' \
                   AND owner_key NOT IN (SELECT id FROM sessions) ORDER BY owner_key",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row.get::<_, String>(0)?);
            }
            Ok(out)
        })
    }

    /// Distinct annotated image hashes (export/rebuild feed): every hash
    /// with target rows, plus opaque/unknown-only owners.
    pub(crate) fn annotated_images(&self) -> Result<Vec<ContentHash>, SidecarError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT DISTINCT image_hash FROM (\
                   SELECT image_hash FROM event_targets \
                   UNION SELECT owner_key AS image_hash FROM sidecar_opaque_events \
                     WHERE owner_kind = 'image' \
                   UNION SELECT owner_key AS image_hash FROM sidecar_unknown_fields \
                     WHERE owner_kind = 'image') \
                 ORDER BY image_hash",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let h: String = row.get(0)?;
                out.push(
                    ContentHash::from_hex(&h).map_err(|e| SidecarError::Invalid(e.to_string()))?,
                );
            }
            Ok(out)
        })
    }

    pub(crate) fn count(&self, sql: &str) -> Result<i64, SidecarError> {
        self.with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
    }
}

/// §2.3 vs §10.3 disambiguation for a file that fails JSON parsing: only a
/// file that positively self-identifies as ours (the literal format-marker
/// string appears in its bytes — e.g. a truncated or bit-rotted sidecar) is
/// treated as a *corrupt sidecar* (renamed aside, §10.3). Anything else is a
/// user file wearing our suffix: a §2.3 collision, never modified or renamed
/// (§1 principle 5 — never destroy user data).
pub(crate) fn classify_unparseable(bytes: &[u8], err: &SidecarParseError) -> Unparseable {
    match err {
        // Parsed as JSON but the format marker is missing/wrong: not ours.
        SidecarParseError::NotASidecar => Unparseable::Collision,
        // Had our format marker, failed structural validation: corrupt.
        SidecarParseError::Invalid(..) => Unparseable::Corrupt,
        // Not valid JSON (or not an object): ours only if the marker string
        // survives in the bytes.
        SidecarParseError::Json(_) | SidecarParseError::NotObject => {
            let marker = super::doc::FORMAT_SIDECAR.as_bytes();
            if bytes.windows(marker.len()).any(|w| w == marker) {
                Unparseable::Corrupt
            } else {
                Unparseable::Collision
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unparseable {
    /// §2.3: a user file — never overwritten, never renamed.
    Collision,
    /// §10.3: a damaged Photoproof sidecar — renamed aside, never deleted.
    Corrupt,
}

/// Is the event present in this document only as a scrubbed husk (or as the
/// redaction marker itself)? Drain-time verification (§11 step 4).
fn sidecar_event_is_scrubbed(doc: &SidecarDoc, event: &EventId) -> bool {
    for entry in &doc.events {
        match entry {
            SidecarEntry::Canonical(e) if e.id == *event => {
                if e.kind == Kind::Redaction {
                    return true;
                }
                if !(e.scrubbed() && e.text.is_none() && e.payload.is_none()) {
                    return false;
                }
            }
            SidecarEntry::Opaque(v)
                if v.get("id").and_then(Value::as_str) == Some(event.as_str())
                    && (v.get("text").is_some()
                        || v.get("payload").is_some()
                        || v.get("redacted_by").is_none()) =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

/// BLAKE3 of a file's bytes (re-match binding; LIBRARY owns the real
/// hashing pipeline).
pub(crate) fn hash_file(path: &Path) -> Result<ContentHash, SidecarError> {
    Ok(ContentHash::from_bytes_of(&fs::read(path)?))
}

/// Recursively collect `*.photoproof.json` files (case-insensitive suffix,
/// §12.3 step 1), skipping `*.tmp-*` orphans. Missing roots yield nothing.
pub(crate) fn walk_sidecar_files(root: &Path) -> Result<Vec<PathBuf>, SidecarError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_sidecar_filename(&name) && !is_temp_orphan(&name) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn walk_dirs(root: &Path) -> Result<Vec<PathBuf>, SidecarError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        out.push(dir);
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(out)
}

/// Decode the 48-bit millisecond timestamp from a ULID string (Crockford
/// base32, first 10 characters).
fn ulid_epoch_ms(s: &str) -> Option<i64> {
    if s.len() != 26 {
        return None;
    }
    let mut v: i64 = 0;
    for &b in &s.as_bytes()[..10] {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'A'..=b'H' => b - b'A' + 10,
            b'J' | b'K' => b - b'J' + 18,
            b'M' | b'N' => b - b'M' + 20,
            b'P'..=b'T' => b - b'P' + 22,
            b'V'..=b'Z' => b - b'V' + 27,
            _ => return None,
        };
        v = (v << 5) | i64::from(d);
    }
    Some(v)
}
