//! Collections (spec/RETRIEVAL.md §10, DECISIONS B71): named collections
//! with append-only notes and EVENTED membership — interval rows with
//! `added_ts`/`removed_ts`, so full membership history stays queryable.
//! Owned by work packet P7.3.
//!
//! Collections are USER TRUTH, not index (the walk-away guarantee): every
//! mutation lands in SQLite first, then the single portability file
//! `appdata/collections.photoproof.json` is mirrored out through the same
//! debounced-writer mechanics as sidecars (SIDECARS §9: 2 s quiet window,
//! 5 s hard cap, atomic mtime-stable replace, backoff ladder on transient
//! failure). Merge is the §10.2 union family, implemented ONCE as the pure
//! `merge_docs` function; file import goes DB → doc → merge → DB so the
//! SQL layer never re-states a merge rule.
//!
//! - `doc`: the §10.2 document model, canonical bytes, strict parse
//! - `merge`: the pure union-merge + table-driven rule tests
//! - here: the engine — SQLite reads/writes, the debounced file writer,
//!   open-time reconcile (load-merge + converge), import/export

mod doc;
mod merge;

pub use doc::{
    COLLECTIONS_FILENAME, COLLECTIONS_VERSION, CollectionEntry, CollectionStatus, CollectionsDoc,
    MemberEntry, NoteEntry, ParsedCollections, parse_collections,
};
pub use merge::{NoteConflict, merge_docs, merge_docs_reporting};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use ulid::Ulid;

use crate::id::{ContentHash, UtcMillis};
use crate::sidecar::{DEBOUNCE_CAP_MS, DEBOUNCE_QUIET_MS, compact_timestamp, write_atomic};
use crate::store::StoreError;

#[derive(Debug, Error)]
pub enum CollectionsError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("collection not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("collections file parse error: {0}")]
    Parse(String),
    #[error(
        "collections file is version {0}, newer than this build; \
         left untouched (never merged, never overwritten)"
    )]
    NewerVersion(u64),
}

/// One collection as the command layer reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: CollectionStatus,
    pub created_ts: String,
    pub updated_ts: String,
    /// Current members (open intervals) only.
    pub member_count: usize,
    pub note_count: usize,
}

/// What an import did: whether anything changed, plus the note-conflict
/// losers (preserved, never silently discarded — SIDECARS §10.2 precedent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub changed: bool,
    pub note_conflicts: Vec<NoteConflict>,
}

/// §9.4-equivalent transient-failure backoff for the file writer.
const BACKOFF_MS: [i64; 4] = [1_000, 5_000, 30_000, 300_000];

/// Debounce state for the ONE file target. Same mechanics as the sidecar
/// `Debouncer` (SIDECARS §9.1/§9.4) without the per-image map: 2 s of
/// write-quiet, 5 s hard cap from the first unflushed mutation, immediate
/// triggers bypass both, transient failures climb the backoff ladder.
/// Pure: all methods take `now`; the caller supplies time.
#[derive(Debug, Default)]
struct FileDebounce {
    /// First unflushed mutation (drives the 5 s cap); `None` = clean.
    first: Option<i64>,
    /// Last mutation (drives the 2 s quiet window).
    last: i64,
    /// Monotonic mutation counter: a flush serializes at generation G and
    /// clears the dirty state only if still at G, so a mutation racing the
    /// file write is never silently dropped.
    generation: u64,
    attempts: u32,
    retry_at: Option<i64>,
    /// False when a newer-version file owns the path (never overwrite).
    enabled: bool,
    /// The version of the newer file that owns the path, when one does.
    /// Kept so the lockout is REPORTABLE (SIDECARS §5.3 requires the user
    /// learns about it): flush/export surface it as an error instead of
    /// silently leaving truth in SQLite only.
    newer_version: Option<u64>,
}

impl FileDebounce {
    fn note_dirty(&mut self, now: UtcMillis) {
        let now = now.epoch_ms();
        self.first.get_or_insert(now);
        self.last = now;
        self.generation += 1;
    }

    fn due(&self, now: UtcMillis) -> bool {
        let now = now.epoch_ms();
        let Some(first) = self.first else {
            return false;
        };
        if !self.enabled {
            return false;
        }
        if let Some(r) = self.retry_at
            && now < r
        {
            return false;
        }
        now - self.last >= DEBOUNCE_QUIET_MS || now - first >= DEBOUNCE_CAP_MS
    }

    fn clear_if(&mut self, generation: u64) {
        if self.generation == generation {
            self.first = None;
            self.attempts = 0;
            self.retry_at = None;
        }
    }

    fn note_failure(&mut self, now: UtcMillis) {
        let idx = (self.attempts as usize).min(BACKOFF_MS.len() - 1);
        self.retry_at = Some(now.epoch_ms() + BACKOFF_MS[idx]);
        self.attempts += 1;
    }
}

/// The collections engine: its own connection over the shared photoproof
/// database (the Library pattern), plus the debounced portability-file
/// writer.
pub struct Collections {
    conn: Mutex<Connection>,
    file_path: PathBuf,
    writer: Mutex<FileDebounce>,
    /// Serializes whole flushes (generation read, serialize, write, clear).
    /// The writer mutex alone cannot: two concurrent `flush_file` calls can
    /// interleave so a STALE serialization replaces a fresher write while
    /// the fresher call already cleared the dirty state — leaving the file
    /// behind the database with the writer marked clean.
    flush_lock: Mutex<()>,
}

impl Collections {
    /// Open over the shared database (creating/migrating if necessary) and
    /// reconcile the portability file: an existing file is union-merged
    /// into the database, then the file converges to the union (the
    /// mtime-stable write makes the already-converged case a no-op). This
    /// open-time reconcile is also the crash net — the in-memory dirty
    /// state needs no durable twin because the whole file is cheap to
    /// recompute and re-compare at every launch, unlike per-image sidecars.
    pub fn open(
        db_path: impl AsRef<Path>,
        app_data: impl Into<PathBuf>,
    ) -> Result<Self, CollectionsError> {
        let db_path = db_path.as_ref();
        let app_data = app_data.into();
        fs::create_dir_all(&app_data)?;
        // Schema + migrations (COLLECTIONS_SCHEMA_SQL, user_version 9) live
        // in store::schema; opening the EventStore runs them — the same
        // throwaway-open pattern Library::open documents.
        drop(crate::store::EventStore::open(db_path)?);
        let conn = crate::library::open_library_connection(db_path)?;
        let me = Self {
            conn: Mutex::new(conn),
            file_path: app_data.join(COLLECTIONS_FILENAME),
            writer: Mutex::new(FileDebounce {
                enabled: true,
                ..FileDebounce::default()
            }),
            flush_lock: Mutex::new(()),
        };
        me.reconcile_file_at_open(UtcMillis::now())?;
        Ok(me)
    }

    /// Absolute path of `appdata/collections.photoproof.json`.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    // -- mutations ----------------------------------------------------------

    /// Create a collection (status `active`).
    pub fn create(
        &self,
        name: &str,
        description: &str,
        now: UtcMillis,
    ) -> Result<CollectionRecord, CollectionsError> {
        self.create_with_images(name, description, &[], now)
    }

    /// Atomically create a collection and open its initial membership
    /// intervals. This is the topic/selection bake primitive: collection
    /// metadata and every member either commit together or leave no row.
    ///
    /// Returning the complete record from the committed inputs avoids a
    /// post-commit re-read that could turn a successful durable mutation into
    /// an apparent command failure with an unannounced collection.
    pub fn create_with_images(
        &self,
        name: &str,
        description: &str,
        hashes: &[ContentHash],
        now: UtcMillis,
    ) -> Result<CollectionRecord, CollectionsError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CollectionsError::Invalid("collection name is empty".into()));
        }
        let id = Ulid::new().to_string();
        let ts = now.to_rfc3339();
        let member_count = {
            let mut conn = self.conn.lock().expect("collections mutex");
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO collections (id, name, description, status, created_ts, updated_ts)
                 VALUES (?1, ?2, ?3, 'active', ?4, ?4)",
                params![id, name, description, ts],
            )?;
            let member_count = add_images_in_transaction(&tx, &id, hashes, now)?;
            tx.commit()?;
            member_count
        };
        self.mark_dirty(now);
        Ok(CollectionRecord {
            id,
            name: name.to_owned(),
            description: description.to_owned(),
            status: CollectionStatus::Active,
            created_ts: ts.clone(),
            updated_ts: ts,
            member_count,
            note_count: 0,
        })
    }

    /// Rename (metadata LWW surface: bumps `updated_ts`).
    pub fn rename(&self, id: &str, name: &str, now: UtcMillis) -> Result<(), CollectionsError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CollectionsError::Invalid("collection name is empty".into()));
        }
        self.update_metadata(id, "name", name, now)
    }

    /// Set status (metadata LWW surface: bumps `updated_ts`).
    pub fn set_status(
        &self,
        id: &str,
        status: CollectionStatus,
        now: UtcMillis,
    ) -> Result<(), CollectionsError> {
        self.update_metadata(id, "status", status.as_str(), now)
    }

    /// Set description (metadata LWW surface: bumps `updated_ts`).
    pub fn set_description(
        &self,
        id: &str,
        description: &str,
        now: UtcMillis,
    ) -> Result<(), CollectionsError> {
        self.update_metadata(id, "description", description, now)
    }

    fn update_metadata(
        &self,
        id: &str,
        column: &'static str,
        value: &str,
        now: UtcMillis,
    ) -> Result<(), CollectionsError> {
        let changed = {
            let conn = self.conn.lock().expect("collections mutex");
            // Column name is a compile-time constant from the three callers
            // above, never user input.
            conn.execute(
                &format!("UPDATE collections SET {column} = ?1, updated_ts = ?2 WHERE id = ?3"),
                params![value, now.to_rfc3339(), id],
            )?
        };
        if changed == 0 {
            return Err(CollectionsError::NotFound(id.to_owned()));
        }
        self.mark_dirty(now);
        Ok(())
    }

    /// Append a note (append-only — there is no edit or delete; §10.1).
    pub fn add_note(
        &self,
        collection_id: &str,
        text: &str,
        now: UtcMillis,
    ) -> Result<NoteEntry, CollectionsError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(CollectionsError::Invalid("note text is empty".into()));
        }
        let note = NoteEntry {
            id: Ulid::new().to_string(),
            ts: now.to_rfc3339(),
            text: text.to_owned(),
        };
        {
            let conn = self.conn.lock().expect("collections mutex");
            require_collection(&conn, collection_id)?;
            conn.execute(
                "INSERT INTO collection_notes (id, collection_id, ts, text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![note.id, collection_id, note.ts, note.text],
            )?;
        }
        self.mark_dirty(now);
        Ok(note)
    }

    /// Add images (explicit hash list). Images already current members are
    /// skipped; a re-add after removal opens a NEW interval row (§10.1).
    /// Returns the number of newly opened intervals.
    pub fn add_images(
        &self,
        collection_id: &str,
        hashes: &[ContentHash],
        now: UtcMillis,
    ) -> Result<usize, CollectionsError> {
        let added = {
            let mut conn = self.conn.lock().expect("collections mutex");
            let tx = conn.transaction()?;
            require_collection(&tx, collection_id)?;
            let added = add_images_in_transaction(&tx, collection_id, hashes, now)?;
            tx.commit()?;
            added
        };
        if added > 0 {
            self.mark_dirty(now);
        }
        Ok(added)
    }

    /// Remove images (explicit hash list): closes the open interval rows —
    /// evented, never destructive (§10.1). Returns the number of IMAGES
    /// whose membership was closed, not the row count: the §10.2 union can
    /// legitimately leave one image with several open interval rows (two
    /// replicas re-added it independently), and a remove closes them all.
    pub fn remove_images(
        &self,
        collection_id: &str,
        hashes: &[ContentHash],
        now: UtcMillis,
    ) -> Result<usize, CollectionsError> {
        let mut removed = 0;
        {
            let mut conn = self.conn.lock().expect("collections mutex");
            let tx = conn.transaction()?;
            require_collection(&tx, collection_id)?;
            for hash in hashes {
                // MAX(): a same-millisecond add+remove must not close the
                // interval before it opened (added_ts can sit 1 ms ahead of
                // the wall clock after the re-add bump above).
                let closed = tx.execute(
                    "UPDATE collection_members SET removed_ts = MAX(?1, added_ts)
                      WHERE collection_id = ?2 AND image_hash = ?3 AND removed_ts IS NULL",
                    params![now.to_rfc3339(), collection_id, hash.as_str()],
                )?;
                if closed > 0 {
                    removed += 1;
                }
            }
            tx.commit()?;
        }
        if removed > 0 {
            self.mark_dirty(now);
        }
        Ok(removed)
    }

    // -- reads --------------------------------------------------------------

    /// All collections (every status), id order (= creation order).
    pub fn list(&self) -> Result<Vec<CollectionRecord>, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        query_records(&conn, "ORDER BY c.id", &[])
    }

    /// One collection.
    pub fn get(&self, id: &str) -> Result<CollectionRecord, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        query_records(&conn, "WHERE c.id = ?1", &[&id])?
            .into_iter()
            .next()
            .ok_or_else(|| CollectionsError::NotFound(id.to_owned()))
    }

    /// Collections this image is CURRENTLY a member of (open intervals).
    pub fn collections_for_image(
        &self,
        hash: &ContentHash,
    ) -> Result<Vec<CollectionRecord>, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        query_records(
            &conn,
            "WHERE c.id IN (SELECT collection_id FROM collection_members
                             WHERE image_hash = ?1 AND removed_ts IS NULL)
             ORDER BY c.id",
            &[&hash.as_str()],
        )
    }

    /// Current members (open intervals), hash order. DISTINCT because
    /// membership is a SET and interval rows are history: the §10.2 union
    /// of two replicas that re-added the same image at different times
    /// holds several open rows for one image, which is still ONE member.
    pub fn current_members(&self, id: &str) -> Result<Vec<ContentHash>, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        require_collection(&conn, id)?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT image_hash FROM collection_members
              WHERE collection_id = ?1 AND removed_ts IS NULL ORDER BY image_hash",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for h in rows {
            out.push(
                ContentHash::from_hex(&h?)
                    .map_err(|e| CollectionsError::Invalid(format!("stored image_hash: {e}")))?,
            );
        }
        Ok(out)
    }

    /// Full membership history — every interval row, open and closed
    /// ("was in the series last winter, dropped in spring").
    pub fn membership_history(&self, id: &str) -> Result<Vec<MemberEntry>, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        require_collection(&conn, id)?;
        let mut stmt = conn.prepare(
            "SELECT image_hash, added_ts, removed_ts FROM collection_members
              WHERE collection_id = ?1 ORDER BY image_hash, added_ts",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(MemberEntry {
                image_hash: r.get(0)?,
                added_ts: r.get(1)?,
                removed_ts: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Notes, id order (ULID order = time order).
    pub fn notes(&self, id: &str) -> Result<Vec<NoteEntry>, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        require_collection(&conn, id)?;
        let mut stmt = conn.prepare(
            "SELECT id, ts, text FROM collection_notes
              WHERE collection_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(NoteEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                text: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -- portability --------------------------------------------------------

    /// The whole store as a §10.2 document (deterministic ordering).
    pub fn export_doc(&self) -> Result<CollectionsDoc, CollectionsError> {
        let conn = self.conn.lock().expect("collections mutex");
        read_doc(&conn)
    }

    /// Union-merge a document into the database (§10.2 rules via the pure
    /// `merge_docs_reporting`). A change marks the file dirty so the union
    /// mirrors back out. Note conflicts (same id, different content —
    /// producer corruption the merge resolved deterministically) ride the
    /// outcome so no caller can lose the losing copy silently.
    pub fn import_doc(
        &self,
        incoming: &CollectionsDoc,
        now: UtcMillis,
    ) -> Result<ImportOutcome, CollectionsError> {
        let outcome = {
            let mut conn = self.conn.lock().expect("collections mutex");
            let ours = read_doc(&conn)?;
            let (merged, note_conflicts) = merge_docs_reporting(&ours, incoming);
            let changed = merged != ours;
            if changed {
                apply_doc(&mut conn, &merged)?;
            }
            ImportOutcome {
                changed,
                note_conflicts,
            }
        };
        if outcome.changed {
            self.mark_dirty(now);
        }
        Ok(outcome)
    }

    /// Parse + union-merge a collections file (rebuild-from-export input).
    /// A newer-version file is an error, not silent skipping: the caller
    /// asked for THIS file's content and must learn it was untouchable.
    pub fn import_file(
        &self,
        path: &Path,
        now: UtcMillis,
    ) -> Result<ImportOutcome, CollectionsError> {
        let bytes = fs::read(path)?;
        match parse_collections(&bytes)? {
            ParsedCollections::Doc(doc) => self.import_doc(&doc, now),
            ParsedCollections::NewerVersion { version } => {
                Err(CollectionsError::NewerVersion(version))
            }
        }
    }

    /// One-click export (§10.2: "included in the full export beside the
    /// sidecar set + manifest"): flush the live file, then write the same
    /// canonical bytes into `dest`.
    ///
    /// While a newer-version file owns the app-data path this errors
    /// instead of writing: the v2 file's collections were never merged
    /// into this build's database, so a database-derived export would
    /// silently omit them — the user asked to "walk away with everything"
    /// and must learn this build cannot deliver that.
    pub fn export_to(&self, dest: &Path, now: UtcMillis) -> Result<PathBuf, CollectionsError> {
        if let Some(v) = self.newer_version_lockout() {
            return Err(CollectionsError::NewerVersion(v));
        }
        // Explicit export is an immediate-flush trigger (SIDECARS §9.1).
        self.flush(now)?;
        let bytes = self.export_doc()?.to_bytes();
        fs::create_dir_all(dest)?;
        let path = dest.join(COLLECTIONS_FILENAME);
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    // -- the debounced writer ------------------------------------------------

    /// Periodic tick (the shell's sidecar pump): flush when the §9.1
    /// windows say so. Returns true when a write happened.
    pub fn pump(&self, now: UtcMillis) -> Result<bool, CollectionsError> {
        if !self.writer.lock().expect("writer mutex").due(now) {
            return Ok(false);
        }
        self.flush_file(now)?;
        Ok(true)
    }

    /// Immediate flush (shutdown, export — the §9.1 bypass triggers).
    /// A clean writer no-ops. A DISABLED writer (newer-version file owns
    /// the path) never writes — and when mutations are pending it errors,
    /// because returning Ok would let a shutdown believe truth reached a
    /// file when it lives in SQLite only (SIDECARS §5.3 requires the
    /// condition to be surfaced, not swallowed).
    pub fn flush(&self, now: UtcMillis) -> Result<(), CollectionsError> {
        let (pending, lockout) = {
            let w = self.writer.lock().expect("writer mutex");
            (w.first.is_some(), w.newer_version)
        };
        if let Some(version) = lockout {
            if pending {
                return Err(CollectionsError::NewerVersion(version));
            }
            return Ok(());
        }
        if pending {
            self.flush_file(now)?;
        }
        Ok(())
    }

    /// Dirty = mutations not yet mirrored to the file (test seam).
    pub fn is_dirty(&self) -> bool {
        self.writer.lock().expect("writer mutex").first.is_some()
    }

    /// `Some(version)` while a newer-version file owns the app-data path:
    /// mirroring is off and mutations accumulate in SQLite only. The shell
    /// surfaces this; flush/export error on it rather than pretend.
    pub fn newer_version_lockout(&self) -> Option<u64> {
        self.writer.lock().expect("writer mutex").newer_version
    }

    fn mark_dirty(&self, now: UtcMillis) {
        self.writer.lock().expect("writer mutex").note_dirty(now);
    }

    fn flush_file(&self, now: UtcMillis) -> Result<(), CollectionsError> {
        // One flusher at a time across the whole read-serialize-write-clear
        // span: see `flush_lock` for the stale-overwrite interleaving this
        // prevents (pump tick racing a shutdown or export flush).
        let _flushing = self.flush_lock.lock().expect("flush mutex");
        // Capture the generation BEFORE serializing: a mutation landing
        // while the file is being written keeps the writer dirty.
        let generation = {
            let w = self.writer.lock().expect("writer mutex");
            // Belt-and-braces: every caller already gates on `enabled`,
            // but a write past this point would clobber a newer file.
            if !w.enabled {
                return Ok(());
            }
            w.generation
        };
        let bytes = self.export_doc()?.to_bytes();
        match write_atomic(&self.file_path, &bytes) {
            Ok(_) => {
                self.writer
                    .lock()
                    .expect("writer mutex")
                    .clear_if(generation);
                Ok(())
            }
            Err(e) => {
                // Transient failure: stay dirty, climb the backoff ladder
                // (SIDECARS §9.4); the pump retries.
                self.writer.lock().expect("writer mutex").note_failure(now);
                Err(e.into())
            }
        }
    }

    /// Open-time reconcile: merge an existing file into the database, then
    /// converge the file to the union. A corrupt file is renamed aside
    /// (preserved for inspection — SIDECARS §10.3 precedent), never
    /// overwritten in place, and every individually-intact collection in
    /// it is salvaged (one garbled field must not cost the user the whole
    /// store); a newer-version file disables the writer entirely
    /// (SIDECARS §5.3: opaque and inviolable).
    fn reconcile_file_at_open(&self, now: UtcMillis) -> Result<(), CollectionsError> {
        match fs::read(&self.file_path) {
            Ok(bytes) => match parse_collections(&bytes) {
                Ok(ParsedCollections::Doc(doc)) => {
                    let outcome = self.import_doc(&doc, now)?;
                    warn_note_conflicts(&outcome.note_conflicts);
                }
                Ok(ParsedCollections::NewerVersion { version }) => {
                    // Never silent: mirroring is OFF from here on, and
                    // every mutation lands in SQLite only until a build
                    // that understands the file runs. flush/export error
                    // while this holds so the shell can tell the user.
                    tracing::warn!(
                        "collections file at {} is version {version} (newer than this \
                         build's {COLLECTIONS_VERSION}); left untouched, mirroring disabled",
                        self.file_path.display()
                    );
                    let mut w = self.writer.lock().expect("writer mutex");
                    w.enabled = false;
                    w.newer_version = Some(version);
                    return Ok(());
                }
                Err(e) => {
                    let aside = self.file_path.with_file_name(format!(
                        "{COLLECTIONS_FILENAME}.corrupt-{}",
                        compact_timestamp(now)
                    ));
                    tracing::warn!(
                        "collections file unparseable ({e}); renamed aside to {}",
                        aside.display()
                    );
                    // Rename FIRST: the original bytes must be preserved
                    // before any salvage import triggers a rewrite.
                    fs::rename(&self.file_path, &aside)?;
                    let (salvaged, errors) = doc::salvage_collections(&bytes);
                    for err in &errors {
                        tracing::warn!("collections salvage skipped an entry: {err}");
                    }
                    if !salvaged.collections.is_empty() {
                        tracing::warn!(
                            "salvaged {} intact collection(s) from the corrupt file",
                            salvaged.collections.len()
                        );
                        let outcome = self.import_doc(&salvaged, now)?;
                        warn_note_conflicts(&outcome.note_conflicts);
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Converge: mtime-stable no-op when DB and file already agree. A
        // failure here must not block opening (the volume may be briefly
        // unwritable) — the writer stays dirty and the pump retries.
        self.mark_dirty(now);
        if let Err(e) = self.flush_file(now) {
            tracing::warn!("collections file converge failed at open (pump retries): {e}");
        }
        Ok(())
    }
}

/// The losing copy of a note conflict is preserved VERBATIM in the log
/// (the open path has no integrity report to carry it; the rebuild path
/// does and uses that instead).
fn warn_note_conflicts(conflicts: &[NoteConflict]) {
    for c in conflicts {
        tracing::warn!(
            "collections import resolved a note conflict in {}: losing copy preserved \
             verbatim: id={} ts={} text={:?}",
            c.collection_id,
            c.loser.id,
            c.loser.ts,
            c.loser.text
        );
    }
}

/// NotFound checks share one shape so every mutation/read agrees.
fn require_collection(conn: &Connection, id: &str) -> Result<(), CollectionsError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM collections WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(CollectionsError::NotFound(id.to_owned()));
    }
    Ok(())
}

fn add_images_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    collection_id: &str,
    hashes: &[ContentHash],
    now: UtcMillis,
) -> Result<usize, CollectionsError> {
    let mut added = 0;
    for hash in hashes {
        let open: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM collection_members
              WHERE collection_id = ?1 AND image_hash = ?2 AND removed_ts IS NULL)",
            params![collection_id, hash.as_str()],
            |r| r.get(0),
        )?;
        if open {
            continue;
        }
        // added_ts is part of the primary key: a remove + re-add within one
        // millisecond must still open a DISTINCT interval, so the new row lands
        // strictly after the last.
        let last: Option<String> = tx.query_row(
            "SELECT MAX(added_ts) FROM collection_members
              WHERE collection_id = ?1 AND image_hash = ?2",
            params![collection_id, hash.as_str()],
            |r| r.get(0),
        )?;
        let mut ts = now;
        if let Some(last) = last {
            let floor = UtcMillis::parse(&last)
                .map_err(|e| CollectionsError::Invalid(format!("stored added_ts: {e}")))?;
            if ts <= floor {
                ts = UtcMillis::from_epoch_ms(floor.epoch_ms() + 1);
            }
        }
        tx.execute(
            "INSERT INTO collection_members (collection_id, image_hash, added_ts, removed_ts)
             VALUES (?1, ?2, ?3, NULL)",
            params![collection_id, hash.as_str(), ts.to_rfc3339()],
        )?;
        added += 1;
    }
    Ok(added)
}

fn query_records(
    conn: &Connection,
    tail: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<CollectionRecord>, CollectionsError> {
    // COUNT(DISTINCT image_hash), not COUNT(*): the §10.2 union can leave
    // one image with several open interval rows; members are a set.
    let sql = format!(
        "SELECT c.id, c.name, c.description, c.status, c.created_ts, c.updated_ts,
                (SELECT COUNT(DISTINCT m.image_hash) FROM collection_members m
                  WHERE m.collection_id = c.id AND m.removed_ts IS NULL),
                (SELECT COUNT(*) FROM collection_notes n WHERE n.collection_id = c.id)
           FROM collections c {tail}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, description, status, created_ts, updated_ts, members, notes) = row?;
        out.push(CollectionRecord {
            id,
            name,
            description,
            status: CollectionStatus::parse(&status)?,
            created_ts,
            updated_ts,
            member_count: members.max(0) as usize,
            note_count: notes.max(0) as usize,
        });
    }
    Ok(out)
}

/// The whole store as a document, deterministically ordered.
fn read_doc(conn: &Connection) -> Result<CollectionsDoc, CollectionsError> {
    let mut collections = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, name, description, status, created_ts, updated_ts
           FROM collections ORDER BY id",
    )?;
    let heads = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut note_stmt = conn.prepare(
        "SELECT id, ts, text FROM collection_notes WHERE collection_id = ?1 ORDER BY id",
    )?;
    let mut member_stmt = conn.prepare(
        "SELECT image_hash, added_ts, removed_ts FROM collection_members
          WHERE collection_id = ?1 ORDER BY image_hash, added_ts",
    )?;
    for (id, name, description, status, created_ts, updated_ts) in heads {
        let notes = note_stmt
            .query_map(params![id], |r| {
                Ok(NoteEntry {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    text: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let members = member_stmt
            .query_map(params![id], |r| {
                Ok(MemberEntry {
                    image_hash: r.get(0)?,
                    added_ts: r.get(1)?,
                    removed_ts: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        collections.push(CollectionEntry {
            id,
            name,
            description,
            status: CollectionStatus::parse(&status)?,
            created_ts,
            updated_ts,
            notes,
            members,
        });
    }
    Ok(CollectionsDoc { collections })
}

/// Apply a merged document. Union laws guarantee `merged` is a superset of
/// the database rows, so this is upserts only — nothing is ever deleted.
fn apply_doc(conn: &mut Connection, merged: &CollectionsDoc) -> Result<(), CollectionsError> {
    let tx = conn.transaction()?;
    for c in &merged.collections {
        tx.execute(
            "INSERT INTO collections (id, name, description, status, created_ts, updated_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, description = excluded.description,
               status = excluded.status, created_ts = excluded.created_ts,
               updated_ts = excluded.updated_ts",
            params![
                c.id,
                c.name,
                c.description,
                c.status.as_str(),
                c.created_ts,
                c.updated_ts
            ],
        )?;
        for n in &c.notes {
            // DO UPDATE, not IGNORE: same-id/different-content is producer
            // corruption the merge already resolved deterministically; the
            // database must converge to the same resolution as the file.
            tx.execute(
                "INSERT INTO collection_notes (id, collection_id, ts, text)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET ts = excluded.ts, text = excluded.text",
                params![n.id, c.id, n.ts, n.text],
            )?;
        }
        for m in &c.members {
            tx.execute(
                "INSERT INTO collection_members (collection_id, image_hash, added_ts, removed_ts)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(collection_id, image_hash, added_ts)
                 DO UPDATE SET removed_ts = excluded.removed_ts",
                params![c.id, m.image_hash, m.added_ts, m.removed_ts],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
