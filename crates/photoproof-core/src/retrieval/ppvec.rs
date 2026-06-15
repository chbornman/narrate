//! PPVEC v2 flat-file vector storage behind the `VectorStore` seam.
//!
//! Contract: spec/RETRIEVAL.md §1.2 (the `vectors` metadata table, owned in
//! full by RETRIEVAL) and §1.3 (the on-disk format, lifecycle, scrub and
//! compaction). One file per `(vec_kind, model_id)` pair under
//! `appdata/vectors/`; SQLite holds only metadata + the `file_row` pointer.
//!
//! Stored encoding: int8 scalar quantization at MRL-truncated 512 dims —
//! f32 exists only transiently at embed time; nothing f32 touches disk.
//! Redaction (`scrub`) physically zeroes the stored int8 row bytes so a
//! byte-scan of the file proves absence (§13.12, mirroring EVENTS I8).
//!
//! Read path (§1.3): the file is memory-mapped and brute-force scanned with
//! a tight multiply-add kernel (autovectorizable), in parallel via rayon
//! once a space is large enough to pay for the fan-out. [`PpvecStore::prewarm`]
//! is the §1.3 page-cache warmer; until the caller wires it, the first
//! search per cold space runs at disk speed and sits outside the §13
//! latency budget. Hand-written SIMD intrinsics and the usearch escape
//! hatch sit behind this same trait if profiling demands them.
//!
//! Concurrency: SQLite metadata is serialized by the store's connection
//! mutex; every path that pairs a `file_row` pointer with flat-file bytes
//! additionally holds the process-wide [`file_io_lock`] so a compaction
//! remap can never interleave with a read, scrub or upsert (lock order is
//! always connection first, file lock second).
//!
//! Crash safety: appends truncate a torn tail (a partial row can never have
//! committed metadata); compaction is two-phase — the `file_row` remap
//! commits together with a `ppvec_compactions` marker, then the rewritten
//! file is renamed into place and the marker cleared; `open` completes or
//! discards any compaction the crash interrupted, so remapped pointers are
//! never paired with the pre-compaction file.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::vector_store::{
    VecHit, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError, VectorStoreResult,
};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use crate::id::UtcMillis;

/// MRL truncation target (§1.3 default; the runtime spike may revisit
/// 512 vs 1024 — a constant here, not a format property: the header
/// records whatever dims a space was created with).
pub const MRL_DIMS: usize = 512;

/// Compaction thresholds (§1.3): dead rows > 20% of a file or 10,000,
/// whichever first.
pub const COMPACT_DEAD_FRACTION: f64 = 0.20;
pub const COMPACT_DEAD_ROWS: u64 = 10_000;

/// Below this many live rows the scan stays single-threaded: rayon's
/// fan-out costs more than scanning a few MB of page-cached bytes.
const PARALLEL_SCAN_MIN_ROWS: usize = 4096;

const MAGIC: &[u8; 6] = b"PPVEC\x02";
/// magic(6) + dims u32 LE (4) + dtype u8 (1) + reserved zero padding (5).
const HEADER_LEN: u64 = 16;

pub const DTYPE_F32: u8 = 0;
pub const DTYPE_INT8: u8 = 1;

/// Process-wide flat-file IO lock. The events engine zeroes redacted rows
/// through [`zero_deleted_rows_for_event`] without holding any
/// `PpvecStore` mutex, so pointer-read + file-write critical sections need
/// a lock both sides share; one process owns the files (EVENTS §5.1 single
///-process model), so a single global mutex is the simple correct choice.
/// Always acquired AFTER a SQLite connection lock, never before.
static FILE_IO: Mutex<()> = Mutex::new(());

fn file_io_lock() -> MutexGuard<'static, ()> {
    FILE_IO.lock().expect("ppvec file lock poisoned")
}

/// Decoded PPVEC v2 header + quantization parameters (§13.12 requires the
/// header to round-trip dtype/dims/scale/offset exactly).
#[derive(Debug, Clone, PartialEq)]
pub struct PpvecHeader {
    pub dims: u32,
    pub dtype: u8,
    /// Per-dimension scale, frozen at file creation.
    pub scale: Vec<f32>,
    /// Per-dimension offset, frozen at file creation.
    pub offset: Vec<f32>,
}

impl PpvecHeader {
    fn data_offset(&self) -> u64 {
        // int8 files carry scale + offset (two f32 per dimension) after
        // the fixed header.
        HEADER_LEN + 8 * u64::from(self.dims)
    }

    fn row_len(&self) -> u64 {
        u64::from(self.dims)
    }
}

/// Metadata that travels with one vector row beyond the embedding itself
/// (RETRIEVAL §1.2 columns the bare trait `upsert` cannot carry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VecMeta {
    /// blake3 of embedded text (or preview bytes) + prefix scheme version
    /// + instruct template version — the staleness check.
    pub inputs_hash: String,
    /// Char offsets (Unicode scalars) into the folded text, for quote
    /// extraction; annotation chunks only.
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
}

/// The sparse semantic k-NN graph [`PpvecStore::knn_within`] returns: per source
/// image hash, its ordered `(neighbor_hash, similarity)` edges. Named so the
/// nested shape reads clearly at the call sites (and clears clippy's
/// type-complexity lint on the signature).
pub type KnnGraph = Vec<(String, Vec<(String, f32)>)>;

/// The PPVEC v2 store: SQLite metadata + one flat file per space.
pub struct PpvecStore {
    db: Mutex<Connection>,
    dir: PathBuf,
}

/// Why the startup doctor acted on one vector space (STATE-INTEGRITY-AUDIT).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceReconcileReason {
    /// The ACTIVE model's space has rows but its `.ppvec` file is GONE (the
    /// vectors dir was mangled outside the app). The rows are dangling pointers;
    /// re-embedding would take the upsert UPDATE branch and write at the stale
    /// `file_row` offsets into a freshly-created sparse file (silent corruption,
    /// every other row reads as zeros). So the rows are deleted and the embedding
    /// pass is re-pended to rebuild a dense file from row 0.
    DanglingActiveFileMissing,
    /// A live space for a SUPERSEDED model: a newer ACTIVE model's space for the
    /// same `vec_kind` exists AND is populated, so this one is stale duplicate
    /// data (e.g. `dfn5b` lingering after the swap to `dfn5b-fp16`). Rows + file
    /// dropped; no re-pend (the active space already carries the embeddings).
    SupersededByActiveModel,
    /// A `.ppvec` FILE on disk with NO live rows pointing at it: pure orphaned
    /// derived bytes (a crash between metadata delete and file removal, or a
    /// dropped space's leftover file). The file is removed.
    OrphanFile,
}

/// One space the startup doctor reconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledSpace {
    pub vec_kind: VecKind,
    pub model_id: String,
    /// Live rows the space held before reconciliation (0 for an orphan file).
    pub rows: u64,
    pub reason: SpaceReconcileReason,
}

/// What [`PpvecStore::reconcile_spaces`] found + did. Empty when the on-disk
/// vector spaces already matched the DB and the active models.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceReconcileReport {
    pub reconciled: Vec<ReconciledSpace>,
    /// `(vec_kind, model_id)` whose rows were dropped because the ACTIVE model's
    /// file was missing: the shell must re-pend the matching embedding pass so
    /// the space rebuilds. Superseded/orphan cleanups never need a re-pend.
    pub repend: Vec<(VecKind, String)>,
}

impl SpaceReconcileReport {
    pub fn is_empty(&self) -> bool {
        self.reconciled.is_empty()
    }
}

impl PpvecStore {
    /// Open over the shared photoproof database; `dir` hosts the flat
    /// files (`appdata/vectors/`), created if missing. Opening the
    /// EventStore first runs schema migrations, mirroring `Library::open`.
    /// Completes (or discards) any compaction a crash interrupted before
    /// returning, so no later read can pair stale pointers with a
    /// half-compacted space.
    pub fn open(db_path: impl AsRef<Path>, dir: impl Into<PathBuf>) -> VectorStoreResult<Self> {
        let db_path = db_path.as_ref();
        drop(
            crate::store::EventStore::open(db_path)
                .map_err(|e| VectorStoreError::Metadata(format!("schema: {e}")))?,
        );
        let conn = crate::library::open_library_connection(db_path).map_err(db_err)?;
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        recover_pending_compactions(&conn, &dir)?;
        Ok(Self {
            db: Mutex::new(conn),
            dir,
        })
    }

    /// `appdata/vectors/{vec_kind}.{model_id_sanitized}.ppvec` (§1.3).
    pub fn file_path(&self, space: &VecSpace) -> PathBuf {
        space_file_path(&self.dir, vec_kind_str(space.vec_kind), &space.model_id)
    }

    /// Decoded header of a space's file, `None` when the space has no file
    /// yet.
    pub fn header(&self, space: &VecSpace) -> VectorStoreResult<Option<PpvecHeader>> {
        let path = self.file_path(space);
        if !path.exists() {
            return Ok(None);
        }
        let mut f = File::open(&path)?;
        Ok(Some(read_header(&mut f)?))
    }

    /// §1.3 prewarm: sequentially read a space's file to pull it into the
    /// OS page cache, so the first scan runs at memory speed instead of
    /// disk speed. Returns the bytes touched. No locks: the bytes are
    /// discarded, so racing a concurrent write is harmless.
    pub fn prewarm(&self, space: &VecSpace) -> VectorStoreResult<u64> {
        let path = self.file_path(space);
        if !path.exists() {
            return Ok(0);
        }
        let mut f = File::open(&path)?;
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        Ok(total)
    }

    /// `(live_rows, total_file_rows)` for a space — the compaction
    /// threshold inputs. Total comes from the file length, so orphaned
    /// rows (crash between file append and metadata commit) count as dead.
    pub fn space_stats(&self, space: &VecSpace) -> VectorStoreResult<(u64, u64)> {
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let path = self.file_path(space);
        if !path.exists() {
            return Ok((0, 0));
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        let total = (f.metadata()?.len().saturating_sub(header.data_offset())) / header.row_len();
        let live: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vectors
                 WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 0",
                params![vec_kind_str(space.vec_kind), space.model_id],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        Ok((live, total))
    }

    /// Compact when the §1.3 thresholds are met. The caller (the
    /// background-pass scheduler / the embedding drain) invokes this; the
    /// trait's `compact` is the unconditional execution.
    pub fn compact_if_needed(&self, space: &VecSpace) -> VectorStoreResult<bool> {
        let (live, total) = self.space_stats(space)?;
        let dead = total.saturating_sub(live);
        if dead == 0 {
            return Ok(false);
        }
        if dead >= COMPACT_DEAD_ROWS || (dead as f64) > (total as f64) * COMPACT_DEAD_FRACTION {
            self.compact(space.clone())?;
            return Ok(true);
        }
        Ok(false)
    }

    /// The stored `inputs_hash` for a key plus its deleted flag — the
    /// embedding passes' staleness check (skip when fresh).
    pub fn row_inputs_hash(&self, key: &VecKey) -> VectorStoreResult<Option<(String, bool)>> {
        let conn = self.db.lock().expect("poisoned");
        let (sql, p1, p2) = unit_filter(&key.unit);
        conn.query_row(
            &format!(
                "SELECT inputs_hash, deleted FROM vectors
                 WHERE vec_kind = ?1 AND model_id = ?2 AND {sql}"
            ),
            params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .map_err(db_err)
    }

    /// Insert or replace the row at `key`, carrying the §1.2 metadata the
    /// bare trait `upsert` cannot (inputs_hash, char offsets). Replacing
    /// un-deletes and overwrites the file row in place.
    pub fn upsert_with_meta(
        &self,
        key: &VecKey,
        v: &Embedding,
        meta: &VecMeta,
    ) -> VectorStoreResult<()> {
        if v.model_id != key.space.model_id {
            return Err(VectorStoreError::ModelMismatch {
                expected: key.space.model_id.clone(),
                got: v.model_id.clone(),
            });
        }
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        // REDACTION RACE GUARD (RETRIEVAL §13.5/§13.12, review L4-host). The
        // embedding drain reads a chunk's body, releases every lock, runs the
        // ort embed (tens of ms to ~3 s), then lands here. A redaction can
        // commit in that window: it scrubs the event (redacted_by set, text
        // NULLed), marks the vector rows deleted=1, and zeroes the flat-file
        // bytes synchronously before `redact()` returns. Without this guard
        // the UPDATE below would flip deleted back to 0 and write an embedding
        // of the now-redacted text — permanently, since the scrubbed event is
        // gone from event_fts so neither the per-image pass nor the
        // session-level sweep ever revisits it, and sweep_dead only zeroes
        // deleted=1 rows. So: for an annotation-chunk row, re-check under THIS
        // connection lock (atomic with the write) whether the source event is
        // redacted; if so, skip the write entirely and leave the row dead.
        // A plain revision does NOT set redacted_by (it appends a new event),
        // so legitimate re-embeds are unaffected — only redaction is blocked.
        if let VecUnit::AnnotationChunk { event_id, .. } = &key.unit {
            // Block ONLY when the source event exists AND is scrubbed
            // (redacted_by set). Absence is NOT a block: PpvecStore is a
            // standalone derived store — a vector may legitimately be upserted
            // for an event row not present in THIS database (and the unit
            // tests plant synthetic ids), so a missing row means "no redaction
            // evidence", allow. A plain revision leaves redacted_by NULL, so
            // re-embeds still land; only an actual scrub refuses.
            let scrubbed: Option<bool> = conn
                .query_row(
                    "SELECT redacted_by IS NOT NULL FROM annotation_events WHERE id = ?1",
                    [event_id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .map_err(db_err)?
                .map(|flag| flag != 0);
            if scrubbed == Some(true) {
                return Ok(());
            }
        }
        // One source-dims per space: mixing embedder dimensions in one
        // space is corruption, never a soft fallback.
        let existing_dims: Option<i64> = conn
            .query_row(
                "SELECT dims FROM vectors WHERE vec_kind = ?1 AND model_id = ?2 LIMIT 1",
                params![vec_kind_str(key.space.vec_kind), key.space.model_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(dims) = existing_dims
            && dims as usize != v.vector.len()
        {
            return Err(VectorStoreError::DimensionMismatch {
                space: key.space.clone(),
                expected: dims as usize,
                got: v.vector.len(),
            });
        }

        let processed = mrl_truncate_normalize(&v.vector);
        let path = self.file_path(&key.space);
        let header = ensure_file(&path, processed.len())?;
        if header.dims as usize != processed.len() {
            return Err(VectorStoreError::Corrupt(format!(
                "{}: header dims {} != processed dims {}",
                path.display(),
                header.dims,
                processed.len()
            )));
        }
        let quantized = quantize(&processed, &header);

        let (filter_sql, p1, p2) = unit_filter(&key.unit);
        let existing_row: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND {filter_sql}"
                ),
                params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;

        let now = UtcMillis::now().to_rfc3339();
        match existing_row {
            Some(file_row) => {
                // File first, then SQLite: a crash in between leaves the
                // old metadata pointing at the new bytes — same key, same
                // space, refreshed on the next pass run via inputs_hash.
                write_row(&path, &header, file_row as u64, &quantized)?;
                conn.execute(
                    &format!(
                        "UPDATE vectors SET deleted = 0, dims = ?5, inputs_hash = ?6,
                                char_start = ?7, char_end = ?8, created_ts = ?9
                         WHERE vec_kind = ?1 AND model_id = ?2 AND {filter_sql}"
                    ),
                    params![
                        vec_kind_str(key.space.vec_kind),
                        key.space.model_id,
                        p1,
                        p2,
                        v.vector.len() as i64,
                        meta.inputs_hash,
                        meta.char_start,
                        meta.char_end,
                        now,
                    ],
                )
                .map_err(db_err)?;
            }
            None => {
                // Append at file end: write + fsync the file FIRST, then
                // commit the SQLite row — an orphaned file row is
                // unreachable garbage cleaned by compaction; the reverse
                // order would be a dangling pointer (§1.3).
                let file_row = append_row(&path, &header, &quantized)?;
                let (event_id, image_hash, chunk_index) = unit_columns(&key.unit);
                conn.execute(
                    "INSERT INTO vectors
                       (vec_kind, model_id, dims, event_id, image_hash, chunk_index,
                        char_start, char_end, file_row, inputs_hash, created_ts, deleted)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
                    params![
                        vec_kind_str(key.space.vec_kind),
                        key.space.model_id,
                        v.vector.len() as i64,
                        event_id,
                        image_hash,
                        chunk_index,
                        meta.char_start,
                        meta.char_end,
                        file_row as i64,
                        meta.inputs_hash,
                        now,
                    ],
                )
                .map_err(db_err)?;
            }
        }
        Ok(())
    }

    /// Physically zero + drop every `deleted = 1` metadata row, across all
    /// spaces. The events engine marks rows deleted on revision, retraction
    /// and redaction (RETRIEVAL §1.1); redaction additionally zeroes the
    /// file bytes synchronously via [`zero_deleted_rows_for_event`] before
    /// the redact call returns (§13.5). This drain-time sweep is the
    /// idempotent backstop (crash between the redaction commit and its
    /// zero write) and the reclaim path for revision/retraction marks.
    /// Zeroing every dead row (not just redactions) is deliberate: core
    /// cannot cheaply distinguish causes, and zeroing is always safe,
    /// strictly stronger than the contract.
    pub fn sweep_dead(&self) -> VectorStoreResult<usize> {
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let dead: Vec<(i64, String, String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT id, vec_kind, model_id, file_row FROM vectors WHERE deleted = 1")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };
        for (id, vec_kind, model_id, file_row) in &dead {
            let path = space_file_path(&self.dir, vec_kind, model_id);
            if path.exists() {
                let header = {
                    let mut f = File::open(&path)?;
                    read_header(&mut f)?
                };
                write_row(
                    &path,
                    &header,
                    *file_row as u64,
                    &vec![0i8; header.dims as usize],
                )?;
            }
            // Metadata row dropped after the bytes are gone; the file row
            // stays as dead space until compaction reclaims it (counted by
            // `space_stats` from the file length).
            conn.execute("DELETE FROM vectors WHERE id = ?1", [id])
                .map_err(db_err)?;
        }
        Ok(dead.len())
    }

    /// Zero + drop chunk rows of `event_id` at `chunk_index >= keep` — the
    /// re-chunking tail when a re-embedded event produced fewer chunks.
    pub fn drop_chunks_from(
        &self,
        space: &VecSpace,
        event_id: &str,
        keep: u32,
    ) -> VectorStoreResult<usize> {
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let stale: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND event_id = ?3
                       AND chunk_index >= ?4",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(
                    params![vec_kind_str(space.vec_kind), space.model_id, event_id, keep],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };
        if stale.is_empty() {
            return Ok(0);
        }
        let path = self.file_path(space);
        let header = {
            let mut f = File::open(&path)?;
            read_header(&mut f)?
        };
        for (id, file_row) in &stale {
            write_row(
                &path,
                &header,
                *file_row as u64,
                &vec![0i8; header.dims as usize],
            )?;
            conn.execute("DELETE FROM vectors WHERE id = ?1", [id])
                .map_err(db_err)?;
        }
        Ok(stale.len())
    }

    /// Read back the stored vector for one key, dequantized to f32, or
    /// `None` when no live row exists. WHY this exists: the bare trait
    /// `search()` takes a query Embedding but the store has no "give me the
    /// vector I already have" accessor, and "more like this" needs exactly
    /// that — the query image's OWN stored CLIP vector to search from. The
    /// returned vector is the int8 round-trip of the original (the only form
    /// on disk; §1.3 keeps nothing f32), which is precisely what `search()`
    /// re-quantizes anyway, so the self-as-query path carries no extra loss.
    /// Mirrors `search()`'s critical-section discipline (connection lock
    /// then file lock) so a compaction remap can't pair a stale `file_row`
    /// with rewritten bytes.
    pub fn fetch(&self, key: &VecKey) -> VectorStoreResult<Option<Embedding>> {
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let (sql, p1, p2) = unit_filter(&key.unit);
        let file_row: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND {sql} AND deleted = 0"
                ),
                params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let Some(file_row) = file_row else {
            return Ok(None);
        };
        let path = self.file_path(&key.space);
        if !path.exists() {
            return Ok(None);
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        let dims = header.dims as usize;
        let mmap = unsafe { memmap2::Mmap::map(&f)? };
        let data = &mmap[header.data_offset() as usize..];
        let start = file_row as usize * dims;
        let Some(row) = data.get(start..start + dims) else {
            return Err(VectorStoreError::Corrupt(format!(
                "{}: file_row {file_row} beyond file end",
                path.display()
            )));
        };
        let vector: Vec<f32> = row
            .iter()
            .enumerate()
            .map(|(i, &b)| dequantize(b as i8, i, &header))
            .collect();
        Ok(Some(Embedding {
            vector,
            model_id: key.space.model_id.clone(),
        }))
    }

    /// The `image_clip` model id under which `image_hash` has a live stored
    /// vector, or `None` when it has none. WHY query the table rather than
    /// take the active embedder's model id: "more like this" reads vectors
    /// that ALREADY exist on disk, so it must work even when the CLIP model
    /// is not loaded into memory on this machine (the embedder is a write-
    /// side concern; retrieval over stored vectors is not). One image carries
    /// at most one live `image_clip` row (the §1.2 `vectors_image` unique
    /// index), so a single row is the answer.
    pub fn image_clip_model_id(&self, image_hash: &str) -> VectorStoreResult<Option<String>> {
        let conn = self.db.lock().expect("poisoned");
        conn.query_row(
            "SELECT model_id FROM vectors
             WHERE vec_kind = ?1 AND image_hash = ?2 AND deleted = 0
             LIMIT 1",
            params![vec_kind_str(VecKind::ImageClip), image_hash],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)
    }

    /// The model id of ANY live row in an image-keyed space, or `None` when the
    /// space is empty. WHY: the topic-graph clustering (v2) reads stored
    /// vectors and so needs the model id of the space to read, but it must work
    /// even when the embedder is NOT loaded into memory (clustering over
    /// vectors that already exist on disk is a read-side concern, like
    /// `image_clip_model_id`). The active embedder's id is preferred when it IS
    /// loaded (it matches the live write path); this is the graceful fallback so
    /// the lens clusters an embedded-but-models-unloaded library.
    pub fn any_model_id(&self, vec_kind: VecKind) -> VectorStoreResult<Option<String>> {
        let conn = self.db.lock().expect("poisoned");
        conn.query_row(
            "SELECT model_id FROM vectors
             WHERE vec_kind = ?1 AND deleted = 0
             LIMIT 1",
            params![vec_kind_str(vec_kind)],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)
    }

    /// Startup-doctor disk-vs-DB reconciliation of the vector spaces
    /// (STATE-INTEGRITY-AUDIT.md; founder: "fully robust"). `active` maps each
    /// `vec_kind` to the model id the live embedder writes today (from config /
    /// the loaded embedder). Three silent-failure classes are healed:
    ///
    /// 1. ACTIVE model, file MISSING ⇒ the rows are dangling pointers; delete
    ///    them and ask the shell to re-pend so the space rebuilds dense (a
    ///    re-embed over dangling rows would corrupt the file via stale offsets).
    /// 2. SUPERSEDED model (a populated active-model space exists for the same
    ///    kind) ⇒ stale duplicate; drop its rows so its file becomes an orphan.
    /// 3. ORPHAN `.ppvec` FILE (no live row maps to it, including the files the
    ///    superseded drop just orphaned) ⇒ remove the bytes.
    ///
    /// CONSERVATIVE: a superseded space is dropped ONLY when the active model's
    /// space is populated (`> 0` rows), so a half-finished re-embed never loses
    /// the only copy of a library's vectors. A kind with no active model entry
    /// (embedder unloaded + nothing to compare against) is left untouched.
    /// Idempotent: a second run over a healthy store reports nothing.
    pub fn reconcile_spaces(
        &self,
        active: &HashMap<VecKind, String>,
    ) -> VectorStoreResult<SpaceReconcileReport> {
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let mut report = SpaceReconcileReport::default();

        // Live spaces grouped by kind: (model_id, live_row_count) per kind.
        let live: Vec<(String, String, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT vec_kind, model_id, COUNT(*) FROM vectors
                     WHERE deleted = 0 GROUP BY vec_kind, model_id",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };
        let mut by_kind: HashMap<VecKind, Vec<(String, i64)>> = HashMap::new();
        for (kind_str, model_id, count) in live {
            let Some(kind) = vec_kind_from_str(&kind_str) else {
                continue; // unknown kind string is not ours to touch
            };
            by_kind.entry(kind).or_default().push((model_id, count));
        }

        // Pass 1 + 2: dangling-active and superseded row drops.
        for (kind, spaces) in &by_kind {
            let active_model = active.get(kind);
            let active_rows = active_model
                .and_then(|am| spaces.iter().find(|(m, _)| m == am))
                .map(|(_, c)| *c)
                .unwrap_or(0);
            for (model_id, count) in spaces {
                let is_active = active_model == Some(model_id);
                let path = space_file_path(&self.dir, vec_kind_str(*kind), model_id);
                if is_active {
                    if !path.exists() {
                        delete_space_rows(&conn, *kind, model_id)?;
                        report.repend.push((*kind, model_id.clone()));
                        report.reconciled.push(ReconciledSpace {
                            vec_kind: *kind,
                            model_id: model_id.clone(),
                            rows: *count as u64,
                            reason: SpaceReconcileReason::DanglingActiveFileMissing,
                        });
                    }
                } else if active_model.is_some() && active_rows > 0 {
                    // Safe to retire the superseded space: the active one is
                    // ready. Drop rows AND the file here so the orphan sweep
                    // below does not re-report the same space as a stray file.
                    delete_space_rows(&conn, *kind, model_id)?;
                    if path.exists() {
                        fs::remove_file(&path)?;
                    }
                    report.reconciled.push(ReconciledSpace {
                        vec_kind: *kind,
                        model_id: model_id.clone(),
                        rows: *count as u64,
                        reason: SpaceReconcileReason::SupersededByActiveModel,
                    });
                }
                // else: no active model to compare against, or it is not yet
                // populated -> leave this space alone (cannot judge safely).
            }
        }

        // Pass 3: orphan-file sweep. The set of filenames any REMAINING live row
        // maps to is the keep-set; every other `.ppvec` file is orphaned bytes
        // (this also sweeps the files the superseded drop above just orphaned).
        let keep: HashSet<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT vec_kind, model_id FROM vectors WHERE deleted = 0")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| {
                    let k: String = r.get(0)?;
                    let m: String = r.get(1)?;
                    Ok((k, m))
                })
                .map_err(db_err)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(db_err)?
                .into_iter()
                .filter_map(|(k, m)| {
                    space_file_path(&self.dir, &k, &m)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_owned())
                })
                .collect()
        };
        if self.dir.exists() {
            for entry in fs::read_dir(&self.dir)? {
                let p = entry?.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // Only judge committed space files; compaction temp files are the
                // recover_pending_compactions sweep's business, not ours.
                if !name.ends_with(".ppvec") || keep.contains(name) {
                    continue;
                }
                fs::remove_file(&p)?;
                report.reconciled.push(ReconciledSpace {
                    // The kind/model are not recoverable from the sanitized
                    // filename; record what we can identify (the kind prefix).
                    vec_kind: vec_kind_from_prefix(name).unwrap_or(VecKind::ImageClip),
                    model_id: name.to_owned(),
                    rows: 0,
                    reason: SpaceReconcileReason::OrphanFile,
                });
            }
        }
        // Deterministic order (HashMap iteration is not): the report feeds a log
        // line + tests, both of which want a stable shape.
        report.reconciled.sort_by(|a, b| {
            (vec_kind_str(a.vec_kind), &a.model_id).cmp(&(vec_kind_str(b.vec_kind), &b.model_id))
        });
        report
            .repend
            .sort_by(|a, b| (vec_kind_str(a.0), &a.1).cmp(&(vec_kind_str(b.0), &b.1)));
        Ok(report)
    }

    /// Score a SPECIFIC set of image hashes against a topic query embedding,
    /// over one image-keyed space (`ImageClip` or `ImageSummary`). Returns a
    /// `(image_hash -> cosine)` map carrying only the hashes that HAVE a live
    /// stored vector in that space; a hash with no row is simply absent (its
    /// affinity is "unknown", which the topic-graph treats as zero pull).
    ///
    /// WHY a sibling of `search()` rather than `search()` itself: the topic
    /// graph scores a KNOWN in-scope set (collection members / the whole
    /// library), not "the global top-k". A top-k search would silently drop
    /// in-scope images past rank k and waste work ranking out-of-scope ones.
    /// This walks exactly the requested rows through the SAME quantize /
    /// dequantize / fused-multiply-add kernel the brute-force `search()` uses,
    /// so the cosine numbers are identical to what fusion would compute — no
    /// second similarity definition. The query and every stored row carry the
    /// same int8 quantization error (§1.3), exactly as `search()`.
    ///
    /// Graceful by construction (DESIGN-SEMANTIC-GRAPH.md: "absent embedders
    /// return zeros / empty, never error"): an empty space, a model mismatch,
    /// or an empty hash set all yield an empty map, never an error — the
    /// mechanism is correct before any embedding pass has run.
    pub fn score_images(
        &self,
        query: &Embedding,
        space: VecSpace,
        image_hashes: &[String],
    ) -> VectorStoreResult<HashMap<String, f32>> {
        if image_hashes.is_empty() || query.model_id != space.model_id {
            return Ok(HashMap::new());
        }
        let path = self.file_path(&space);
        // ONE critical section (connection + file lock), mirroring `search()`:
        // a compaction remap between the pointer reads and the byte snapshot
        // would pair dense new file_rows with the old bytes.
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        let processed = mrl_truncate_normalize(&query.vector);
        if processed.len() != header.dims as usize {
            // A dimension mismatch is a model/space mismatch the graph treats
            // as "no signal", never a hard error (it must not crash the lens).
            return Ok(HashMap::new());
        }
        // SAFETY: identical to `search()` — one process owns the files and we
        // hold the FILE_IO lock for the mapping's lifetime (this function).
        let mmap = unsafe { memmap2::Mmap::map(&f)? };
        let data = &mmap[header.data_offset() as usize..];

        // Same hoisted quant kernel as `search()`: dequant(b)·q == b·(scale·q)
        // + offset·q, so each row is a fused multiply-add.
        let q: Vec<f32> = quantize(&processed, &header)
            .iter()
            .enumerate()
            .map(|(i, &v)| dequantize(v, i, &header))
            .collect();
        let weights: Vec<f32> = header.scale.iter().zip(&q).map(|(s, qf)| s * qf).collect();
        let bias: f32 = header.offset.iter().zip(&q).map(|(o, qf)| o * qf).sum();
        let dims = header.dims as usize;

        // Resolve the requested hashes to their live file rows in ONE query.
        // De-dup is harmless (the map keys on hash); a hash with no live row
        // simply does not come back.
        let marks = vec!["?"; image_hashes.len()].join(",");
        let rows: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT image_hash, file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 0
                       AND image_hash IN ({marks})"
                ))
                .map_err(db_err)?;
            let mut params: Vec<Value> = vec![
                Value::Text(vec_kind_str(space.vec_kind).to_owned()),
                Value::Text(space.model_id.clone()),
            ];
            params.extend(image_hashes.iter().map(|h| Value::Text(h.clone())));
            let mapped = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(db_err)?;
            mapped.collect::<Result<_, _>>().map_err(db_err)?
        };

        let mut out = HashMap::with_capacity(rows.len());
        for (hash, file_row) in rows {
            let start = file_row as usize * dims;
            let Some(row) = data.get(start..start + dims) else {
                return Err(VectorStoreError::Corrupt(format!(
                    "{}: file_row {file_row} beyond file end",
                    path.display()
                )));
            };
            let mut acc = 0.0f32;
            for (b, w) in row.iter().zip(&weights) {
                acc += f32::from(*b as i8) * w;
            }
            out.insert(hash, acc + bias);
        }
        Ok(out)
    }

    /// Read the RAW (dequantized) stored vectors for a SPECIFIC set of image
    /// hashes over one image-keyed space, in ONE critical section. Returns
    /// `(image_hash, vector)` pairs carrying only the hashes that HAVE a live
    /// stored vector; a hash with no row is simply absent.
    ///
    /// WHY a bulk sibling of `fetch` (which reads ONE row, re-locking and
    /// re-mmapping per call): the topic-graph clustering (v2) needs the actual
    /// vectors for thousands of in-scope images to run k-means over. Fetching
    /// them one at a time would re-acquire the connection mutex, the file lock,
    /// and a fresh mmap per image. This walks exactly the requested rows under a
    /// SINGLE lock/mmap pair, dequantizing each through the same kernel as
    /// `fetch`, so the clustering sees the same int8-quantized vectors retrieval
    /// scores with (§1.3) — no second vector definition.
    ///
    /// Graceful by construction (DESIGN-SEMANTIC-GRAPH.md): an empty space, a
    /// missing file, or an empty hash set all yield an empty vec, never an error
    /// — clustering over an un-embedded scope returns nothing, not a crash.
    pub fn read_image_vectors(
        &self,
        space: VecSpace,
        image_hashes: &[String],
    ) -> VectorStoreResult<Vec<(String, Vec<f32>)>> {
        if image_hashes.is_empty() {
            return Ok(Vec::new());
        }
        let path = self.file_path(&space);
        // One critical section (connection + file lock), mirroring `score_images`:
        // a compaction remap between the pointer reads and the byte snapshot
        // would pair dense new file_rows with the old bytes.
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        let dims = header.dims as usize;
        // SAFETY: identical to `score_images` — one process owns the files and we
        // hold the FILE_IO lock for the mapping's lifetime (this function).
        let mmap = unsafe { memmap2::Mmap::map(&f)? };
        let data = &mmap[header.data_offset() as usize..];

        // Resolve the requested hashes to their live file rows in ONE query.
        let marks = vec!["?"; image_hashes.len()].join(",");
        let rows: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT image_hash, file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 0
                       AND image_hash IN ({marks})"
                ))
                .map_err(db_err)?;
            let mut params: Vec<Value> = vec![
                Value::Text(vec_kind_str(space.vec_kind).to_owned()),
                Value::Text(space.model_id.clone()),
            ];
            params.extend(image_hashes.iter().map(|h| Value::Text(h.clone())));
            let mapped = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(db_err)?;
            mapped.collect::<Result<_, _>>().map_err(db_err)?
        };

        let mut out = Vec::with_capacity(rows.len());
        for (hash, file_row) in rows {
            let start = file_row as usize * dims;
            let Some(row) = data.get(start..start + dims) else {
                return Err(VectorStoreError::Corrupt(format!(
                    "{}: file_row {file_row} beyond file end",
                    path.display()
                )));
            };
            let vector: Vec<f32> = row
                .iter()
                .enumerate()
                .map(|(i, &b)| dequantize(b as i8, i, &header))
                .collect();
            out.push((hash, vector));
        }
        Ok(out)
    }

    /// Sparse semantic k-NN graph over an in-scope image set: for each image
    /// that HAS a stored vector, its top-`k` most similar OTHER in-scope images
    /// by cosine, descending. Returns `(image_hash, [(neighbor_hash, sim), ..])`.
    ///
    /// WHY this exists: the visualizer's force layout needs a "these photos are
    /// alike" attraction so semantically similar images pull together. That is a
    /// one-shot precompute over a known scope (not a per-frame query, and not a
    /// global top-k like `search`), so a brute-force O(N^2) pass over exactly the
    /// requested rows is the simple correct shape — at the scope sizes the lens
    /// runs over, the cost is dominated by the read, not the dot products.
    ///
    /// Cosine == dot product here: every stored vector is already
    /// MRL-truncated + L2-normalized at write time (§1.3), so the dequantized
    /// rows are unit vectors and their dot product IS the cosine. Negative
    /// similarities are clamped to 0 so a dissimilar pair becomes "no pull" for
    /// the layout, never a repulsion edge (repulsion is the sim's global term).
    ///
    /// Deterministic: neighbors sort by similarity descending, ties broken by
    /// neighbor hash ascending, so the same scope always yields the same graph
    /// (reproducible layout + stable tests).
    ///
    /// Graceful by construction (DESIGN-SEMANTIC-GRAPH.md): empty hashes, an
    /// empty/un-embedded space, or `k == 0` all yield an empty result, never an
    /// error. An image with no stored vector is simply omitted (it gets no
    /// semantic edges, which the layout treats as no pull).
    pub fn knn_within(
        &self,
        space: VecSpace,
        hashes: &[String],
        k: usize,
    ) -> VectorStoreResult<KnnGraph> {
        if hashes.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        // Read exactly the in-scope rows once (one lock + mmap); absent hashes
        // (no stored vector) simply do not come back, so they get no edges.
        let present = self.read_image_vectors(space, hashes)?;
        if present.len() < 2 {
            // A single (or zero) embedded image has no OTHER image to pull
            // toward: an honest empty graph, not a self-edge.
            return Ok(Vec::new());
        }

        // Stable input order so the O(N^2) pass and its tie-breaks are
        // reproducible regardless of how the DB returned the rows.
        let mut present = present;
        present.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out: Vec<(String, Vec<(String, f32)>)> = Vec::with_capacity(present.len());
        for (i, (hash_i, vec_i)) in present.iter().enumerate() {
            let mut neighbors: Vec<(String, f32)> = Vec::with_capacity(present.len() - 1);
            for (j, (hash_j, vec_j)) in present.iter().enumerate() {
                if i == j {
                    continue; // never an edge to self
                }
                // Both rows are unit-length (§1.3), so the dot IS the cosine.
                let dot: f32 = vec_i.iter().zip(vec_j).map(|(a, b)| a * b).sum();
                // Clamp negatives to 0: a dissimilar pair is "no pull" for the
                // layout, never a (sim-driven) repulsion.
                neighbors.push((hash_j.clone(), dot.max(0.0)));
            }
            // Most-similar first; equal similarities ordered by neighbor hash so
            // the graph is deterministic (reproducible layout + tests).
            neighbors.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            neighbors.truncate(k);
            out.push((hash_i.clone(), neighbors));
        }
        Ok(out)
    }

    /// "More like this": the nearest OTHER images to `image_hash` by cosine
    /// similarity over the `image_clip` space, in descending-similarity order
    /// with the query image itself excluded. Returns `(image_hash, score)`
    /// pairs, at most `limit` of them.
    ///
    /// Reuses the existing brute-force `search()` the S4 hybrid path uses —
    /// no second kNN implementation. The query is the image's OWN stored
    /// vector (via `fetch`), so the top hit is always self at cosine ~1.0;
    /// we over-fetch by one and drop the self row to keep `limit` neighbors.
    /// Graceful on an un-embedded machine: an image with no stored vector,
    /// or an empty space, yields an empty list rather than an error — the
    /// mechanism is correct even before any embedding pass has run.
    pub fn similar_images(
        &self,
        image_hash: &str,
        limit: usize,
    ) -> VectorStoreResult<Vec<(String, f32)>> {
        // No stored vector for this image (not yet embedded, or scrubbed):
        // an honest empty result, not a failure.
        let Some(model_id) = self.image_clip_model_id(image_hash)? else {
            return Ok(Vec::new());
        };
        let space = VecSpace {
            vec_kind: VecKind::ImageClip,
            model_id: model_id.clone(),
        };
        let key = VecKey {
            space: space.clone(),
            unit: VecUnit::Image {
                image_hash: image_hash.to_owned(),
            },
        };
        let Some(query) = self.fetch(&key)? else {
            return Ok(Vec::new());
        };
        // Over-fetch by one: the query image ranks first against itself, so
        // we drop it below and still return up to `limit` true neighbors.
        let hits = self.search(&query, space, limit.saturating_add(1))?;
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        // Resolve hit rowids → image hashes in one statement, then walk the
        // hits in similarity order (the map is unordered; `search` already
        // sorted) excluding self. WHY exclude by hash, not by rowid: robust
        // even if a future re-embed changes the self row's id between fetch
        // and search.
        let conn = self.db.lock().expect("poisoned");
        let marks = vec!["?"; hits.len()].join(",");
        let mut by_id: HashMap<i64, String> = HashMap::new();
        {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT id, image_hash FROM vectors WHERE id IN ({marks})"
                ))
                .map_err(db_err)?;
            let params: Vec<i64> = hits.iter().map(|h| h.vector_id).collect();
            let mut rows = stmt
                .query(rusqlite::params_from_iter(params))
                .map_err(db_err)?;
            while let Some(row) = rows.next().map_err(db_err)? {
                let id: i64 = row.get(0).map_err(db_err)?;
                let hash: Option<String> = row.get(1).map_err(db_err)?;
                if let Some(hash) = hash {
                    by_id.insert(id, hash);
                }
            }
        }
        let mut out = Vec::with_capacity(limit.min(hits.len()));
        for hit in &hits {
            if out.len() >= limit {
                break;
            }
            if let Some(hash) = by_id.get(&hit.vector_id)
                && hash != image_hash
            {
                out.push((hash.clone(), hit.score));
            }
        }
        Ok(out)
    }
}

impl VectorStore for PpvecStore {
    fn upsert(&self, key: VecKey, v: &Embedding) -> VectorStoreResult<()> {
        // The bare trait call carries no chunk metadata; hash the raw
        // vector bytes so staleness still has an honest input. Embedding
        // passes use `upsert_with_meta` with the §1.2 recipe instead.
        let mut bytes = Vec::with_capacity(v.vector.len() * 4);
        for x in &v.vector {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        let meta = VecMeta {
            inputs_hash: blake3::hash(&bytes).to_hex().to_string(),
            char_start: None,
            char_end: None,
        };
        self.upsert_with_meta(&key, v, &meta)
    }

    fn search(
        &self,
        query: &Embedding,
        space: VecSpace,
        k: usize,
    ) -> VectorStoreResult<Vec<VecHit>> {
        if query.model_id != space.model_id {
            return Err(VectorStoreError::ModelMismatch {
                expected: space.model_id.clone(),
                got: query.model_id.clone(),
            });
        }
        let path = self.file_path(&space);
        // Pointer reads and the byte snapshot are ONE critical section:
        // without it a compaction remap landing in between pairs dense new
        // file_rows with the old bytes — silently wrong neighbors.
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        // §1.3 Read: memory-mapped — no per-query copy of the data region.
        // SAFETY: one process owns the PPVEC files (EVENTS §5.1 single-
        // process model) and every in-process mutation path holds the
        // FILE_IO lock we hold here, so the mapping cannot observe a
        // concurrent write for its lifetime (this function).
        let mmap = unsafe { memmap2::Mmap::map(&f)? };
        // read_header consumed exactly data_offset bytes, so the file (and
        // the map) is at least that long.
        let data = &mmap[header.data_offset() as usize..];

        // The query goes through the same quantize/dequantize as every
        // stored vector (§1.3: "every query is quantized with these same
        // parameters"), so both sides carry identical quantization error.
        let processed = mrl_truncate_normalize(&query.vector);
        if processed.len() != header.dims as usize {
            return Err(VectorStoreError::DimensionMismatch {
                space,
                expected: header.dims as usize,
                got: processed.len(),
            });
        }
        let q: Vec<f32> = quantize(&processed, &header)
            .iter()
            .enumerate()
            .map(|(i, &v)| dequantize(v, i, &header))
            .collect();
        // Hoist the per-dimension quant parameters out of the row loop:
        // dequant(b)*q  ==  b*(scale*q) + offset*q, so the row kernel is a
        // single fused multiply-add per byte — the shape autovectorizers
        // turn into SIMD (§1.3 makes the scan kernel a requirement).
        let weights: Vec<f32> = header.scale.iter().zip(&q).map(|(s, qf)| s * qf).collect();
        let bias: f32 = header.offset.iter().zip(&q).map(|(o, qf)| o * qf).sum();

        let live: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 0",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![vec_kind_str(space.vec_kind), space.model_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };

        let dims = header.dims as usize;
        let score_one = |&(vector_id, file_row): &(i64, i64)| -> VectorStoreResult<VecHit> {
            let start = file_row as usize * dims;
            let Some(row) = data.get(start..start + dims) else {
                return Err(VectorStoreError::Corrupt(format!(
                    "{}: file_row {file_row} beyond file end",
                    path.display()
                )));
            };
            let mut acc = 0.0f32;
            for (b, w) in row.iter().zip(&weights) {
                acc += f32::from(*b as i8) * w;
            }
            Ok(VecHit {
                vector_id,
                score: acc + bias,
            })
        };
        let mut hits: Vec<VecHit> = if live.len() >= PARALLEL_SCAN_MIN_ROWS {
            // §1.3: multithreaded scanning is a requirement at scale.
            use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
            live.par_iter().map(score_one).collect::<Result<_, _>>()?
        } else {
            live.iter().map(score_one).collect::<Result<_, _>>()?
        };
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.vector_id.cmp(&b.vector_id))
        });
        hits.truncate(k);
        Ok(hits)
    }

    fn mark_deleted(&self, key: VecKey) -> VectorStoreResult<()> {
        let conn = self.db.lock().expect("poisoned");
        let (sql, p1, p2) = unit_filter(&key.unit);
        let n = conn
            .execute(
                &format!(
                    "UPDATE vectors SET deleted = 1
                     WHERE vec_kind = ?1 AND model_id = ?2 AND {sql}"
                ),
                params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
            )
            .map_err(db_err)?;
        if n == 0 {
            return Err(VectorStoreError::NotFound(key));
        }
        Ok(())
    }

    fn scrub(&self, key: VecKey) -> VectorStoreResult<()> {
        // Physical zero of the stored int8 row bytes + logical delete —
        // the redaction contract (§1.3; byte-scan acceptance §13.12).
        // Locate + zero + mark are ONE critical section: a compaction
        // remap between locate and the write would make the zeros land on
        // a different, live row while the redacted bytes survive.
        let conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        let (sql, p1, p2) = unit_filter(&key.unit);
        let file_row: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND {sql}"
                ),
                params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let Some(file_row) = file_row else {
            return Err(VectorStoreError::NotFound(key));
        };
        let path = self.file_path(&key.space);
        let header = {
            let mut f = File::open(&path)?;
            read_header(&mut f)?
        };
        write_row(
            &path,
            &header,
            file_row as u64,
            &vec![0i8; header.dims as usize],
        )?;
        conn.execute(
            &format!(
                "UPDATE vectors SET deleted = 1
                 WHERE vec_kind = ?1 AND model_id = ?2 AND {sql}"
            ),
            params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
        )
        .map_err(db_err)?;
        Ok(())
    }

    fn compact(&self, space: VecSpace) -> VectorStoreResult<()> {
        let path = self.file_path(&space);
        // Both locks BEFORE the snapshot: an upsert landing between the
        // snapshot and the rename would be silently reverted by the
        // rename (fresh metadata over stale bytes, never re-embedded).
        let mut conn = self.db.lock().expect("poisoned");
        let _io = file_io_lock();
        if !path.exists() {
            return Ok(());
        }
        let (header, data) = {
            let mut f = File::open(&path)?;
            let header = read_header(&mut f)?;
            f.seek(SeekFrom::Start(header.data_offset()))?;
            let mut data = Vec::new();
            f.read_to_end(&mut data)?;
            (header, data)
        };
        let dims = header.dims as usize;

        let live: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, file_row FROM vectors
                     WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 0
                     ORDER BY file_row",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![vec_kind_str(space.vec_kind), space.model_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };

        // Rewrite dropping dead rows into a temp file in the same
        // directory (§1.3).
        let tmp = compact_tmp_path(&path);
        {
            let mut out = File::create(&tmp)?;
            let mut head = Vec::new();
            write_header_bytes(&mut head, &header);
            out.write_all(&head)?;
            for (_, file_row) in &live {
                let start = *file_row as usize * dims;
                let row = data.get(start..start + dims).ok_or_else(|| {
                    VectorStoreError::Corrupt(format!(
                        "{}: file_row {file_row} beyond file end",
                        path.display()
                    ))
                })?;
                out.write_all(row)?;
            }
            out.sync_all()?;
        }

        // Two-phase commit: the remap and a pending-compaction marker
        // commit atomically, THEN the rename happens, then the marker
        // clears. The rename cannot live inside the SQLite transaction, so
        // a crash in either gap leaves the marker for `open` to finish the
        // rename (or discard a pre-commit temp) — remapped pointers are
        // never left over the pre-compaction file, which would silently
        // read wrong vectors and let the next sweep zero live rows.
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "DELETE FROM vectors WHERE vec_kind = ?1 AND model_id = ?2 AND deleted = 1",
            params![vec_kind_str(space.vec_kind), space.model_id],
        )
        .map_err(db_err)?;
        // Ascending old file_row: every row moves down (or stays), so the
        // (vec_kind, model_id, file_row) unique index never sees a
        // transient collision.
        for (new_row, (id, _)) in live.iter().enumerate() {
            tx.execute(
                "UPDATE vectors SET file_row = ?1 WHERE id = ?2",
                params![new_row as i64, id],
            )
            .map_err(db_err)?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO ppvec_compactions (vec_kind, model_id) VALUES (?1, ?2)",
            params![vec_kind_str(space.vec_kind), space.model_id],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;

        fs::rename(&tmp, &path)?;
        sync_dir(&self.dir)?;
        conn.execute(
            "DELETE FROM ppvec_compactions WHERE vec_kind = ?1 AND model_id = ?2",
            params![vec_kind_str(space.vec_kind), space.model_id],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// Zero the flat-file bytes of every `deleted = 1` row of `event_id`,
/// leaving the metadata rows for the drain's sweep/compaction to reclaim.
/// The events engine calls this right after committing a redaction scrub,
/// so the §13.5 guarantee — flat-file bytes zeroed by the time the
/// redaction call returns — holds without waiting for an embedding drain;
/// [`PpvecStore::sweep_dead`] stays the idempotent backstop for a crash
/// between that commit and this write. Returns the rows zeroed.
pub(crate) fn zero_deleted_rows_for_event(
    conn: &Connection,
    dir: &Path,
    event_id: &str,
) -> VectorStoreResult<usize> {
    zero_deleted_rows(conn, dir, "event_id", event_id)
}

/// Image-keyed sibling of [`zero_deleted_rows_for_event`]: zeroes the dead
/// `image_summary`/`image_clip` rows of one image. The events engine calls
/// it when redaction propagation deletes an image's derived summary
/// (RETRIEVAL §9.5 — the summary's vector follows its row), with the same
/// synchronous timing as the event-chunk zeroing.
pub(crate) fn zero_deleted_rows_for_image(
    conn: &Connection,
    dir: &Path,
    image_hash: &str,
) -> VectorStoreResult<usize> {
    zero_deleted_rows(conn, dir, "image_hash", image_hash)
}

/// `key_column` is one of the two compile-time constants above — never
/// user input — so the formatted SQL cannot be injected into.
fn zero_deleted_rows(
    conn: &Connection,
    dir: &Path,
    key_column: &str,
    key: &str,
) -> VectorStoreResult<usize> {
    let _io = file_io_lock();
    let rows: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT vec_kind, model_id, file_row FROM vectors
                 WHERE {key_column} = ?1 AND deleted = 1"
            ))
            .map_err(db_err)?;
        let found = stmt
            .query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(db_err)?;
        found.collect::<Result<_, _>>().map_err(db_err)?
    };
    let mut zeroed = 0usize;
    for (vec_kind, model_id, file_row) in &rows {
        let path = space_file_path(dir, vec_kind, model_id);
        if !path.exists() {
            continue;
        }
        let header = {
            let mut f = File::open(&path)?;
            read_header(&mut f)?
        };
        write_row(
            &path,
            &header,
            *file_row as u64,
            &vec![0i8; header.dims as usize],
        )?;
        zeroed += 1;
    }
    Ok(zeroed)
}

/// The spec layout (§1.3): `{db parent}/vectors`. The events engine derives
/// its zeroing target from the database path with this, so stores opened at
/// the conventional location get synchronous redaction zeroing.
pub fn default_vectors_dir(db_path: &Path) -> Option<PathBuf> {
    db_path.parent().map(|p| p.join("vectors"))
}

fn space_file_path(dir: &Path, vec_kind: &str, model_id: &str) -> PathBuf {
    dir.join(format!("{vec_kind}.{}.ppvec", sanitize_model_id(model_id)))
}

fn compact_tmp_path(path: &Path) -> PathBuf {
    path.with_extension("ppvec.compact-tmp")
}

/// Finish (or discard) compactions interrupted by a crash. A marker row
/// committed with the remap but whose temp file still exists means the
/// rename never happened: complete it. A marker without a temp file means
/// the crash hit between rename and marker delete: just clear it. Temp
/// files WITHOUT a marker are pre-commit garbage (metadata still matches
/// the original file) and are removed.
fn recover_pending_compactions(conn: &Connection, dir: &Path) -> VectorStoreResult<()> {
    let pending: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT vec_kind, model_id FROM ppvec_compactions")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)?
    };
    for (vec_kind, model_id) in &pending {
        let path = space_file_path(dir, vec_kind, model_id);
        let tmp = compact_tmp_path(&path);
        if tmp.exists() {
            fs::rename(&tmp, &path)?;
            sync_dir(dir)?;
        }
        conn.execute(
            "DELETE FROM ppvec_compactions WHERE vec_kind = ?1 AND model_id = ?2",
            params![vec_kind, model_id],
        )
        .map_err(db_err)?;
    }
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".compact-tmp"))
        {
            fs::remove_file(&p)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Quantization (§1.3)
// ---------------------------------------------------------------------------

/// MRL truncation to 512 dims + L2 renormalization (truncation shortens
/// the vector, so cosine needs a renorm). Inputs shorter than the target
/// pass through (their own length), normalized.
pub fn mrl_truncate_normalize(vector: &[f32]) -> Vec<f32> {
    let mut v: Vec<f32> = vector[..vector.len().min(MRL_DIMS)].to_vec();
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
    v
}

fn quantize(v: &[f32], header: &PpvecHeader) -> Vec<i8> {
    v.iter()
        .enumerate()
        .map(|(i, &x)| {
            let q = ((x - header.offset[i]) / header.scale[i]).round();
            q.clamp(-127.0, 127.0) as i8
        })
        .collect()
}

fn dequantize(q: i8, dim: usize, header: &PpvecHeader) -> f32 {
    f32::from(q) * header.scale[dim] + header.offset[dim]
}

// ---------------------------------------------------------------------------
// File format
// ---------------------------------------------------------------------------

fn write_header_bytes(out: &mut Vec<u8>, header: &PpvecHeader) {
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&header.dims.to_le_bytes());
    out.push(header.dtype);
    out.extend_from_slice(&[0u8; 5]); // reserved padding to 16-byte alignment
    for s in &header.scale {
        out.extend_from_slice(&s.to_le_bytes());
    }
    for o in &header.offset {
        out.extend_from_slice(&o.to_le_bytes());
    }
}

fn read_header(f: &mut File) -> VectorStoreResult<PpvecHeader> {
    let mut fixed = [0u8; HEADER_LEN as usize];
    f.read_exact(&mut fixed)
        .map_err(|_| VectorStoreError::Corrupt("truncated PPVEC header".into()))?;
    if &fixed[..6] != MAGIC {
        return Err(VectorStoreError::Corrupt("bad PPVEC magic".into()));
    }
    let dims = u32::from_le_bytes(fixed[6..10].try_into().expect("4 bytes"));
    let dtype = fixed[10];
    if dtype != DTYPE_INT8 {
        // The format reserves dtype 0 = f32; v1 never writes it (§1.3:
        // nothing f32 touches disk), so reading one is unexpected state.
        return Err(VectorStoreError::Corrupt(format!(
            "unsupported PPVEC dtype {dtype} (v1 stores int8 only)"
        )));
    }
    let mut params = vec![0u8; 8 * dims as usize];
    f.read_exact(&mut params)
        .map_err(|_| VectorStoreError::Corrupt("truncated PPVEC quant params".into()))?;
    let scale: Vec<f32> = params[..4 * dims as usize]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().expect("4 bytes")))
        .collect();
    let offset: Vec<f32> = params[4 * dims as usize..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().expect("4 bytes")))
        .collect();
    Ok(PpvecHeader {
        dims,
        dtype,
        scale,
        offset,
    })
}

/// Create the space's file if missing; return its header either way.
///
/// Calibration (§1.3: per-dimension scale/offset frozen at file creation):
/// v1 freezes the full representable range of an L2-normalized vector —
/// scale 1/127, offset 0, per dimension. Deterministic by construction
/// (no data-dependent calibration sample), which §13.8 rebuild
/// byte-equality needs; a real calibration pass can land later behind the
/// same header without a format change.
///
/// NAMED DEVIATION (interim): this is NOT the §1.3 "calibration sample of
/// the space". Components of an L2-normalized 512-d vector cluster near
/// 1/sqrt(512) ~ 0.044, so the full-range constant uses only ~±6 of the
/// ±127 int8 codes (~3.5 effective bits) — the cited ~1–2% quality cost of
/// int8 was measured for calibrated ranges and does NOT transfer to this
/// scheme. The §12 eval harness (a later packet) gates the real number; a
/// data-derived calibration replaces these constants behind the same
/// header, re-embedding via the inputs_hash recipe.
///
/// A torn header (crash during creation, before the fsync completed) is
/// recreated in place: the metadata row for an append commits only after
/// `ensure_file` + the row write succeed, so a file that cannot produce a
/// complete header has never had a committed row — recreating loses
/// nothing, while erroring would wedge the space (and the whole embedding
/// drain) forever.
fn ensure_file(path: &Path, dims: usize) -> VectorStoreResult<PpvecHeader> {
    if path.exists() {
        let mut f = File::open(path)?;
        match read_header(&mut f) {
            Ok(h) => return Ok(h),
            // Both truncation cases mean the data region cannot exist
            // (the file ends inside the header/params): recreate.
            Err(VectorStoreError::Corrupt(msg)) if msg.starts_with("truncated PPVEC") => {}
            Err(e) => return Err(e),
        }
    }
    let header = PpvecHeader {
        dims: dims as u32,
        dtype: DTYPE_INT8,
        scale: vec![1.0 / 127.0; dims],
        offset: vec![0.0; dims],
    };
    let mut bytes = Vec::new();
    write_header_bytes(&mut bytes, &header);
    let mut f = File::create(path)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    if let Some(dir) = path.parent() {
        sync_dir(dir)?;
    }
    Ok(header)
}

/// Append one row at file end; write + fsync (§1.3). Returns the row index.
fn append_row(path: &Path, header: &PpvecHeader, row: &[i8]) -> VectorStoreResult<u64> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    let data_len = len.saturating_sub(header.data_offset());
    let whole_rows = data_len / header.row_len();
    if data_len % header.row_len() != 0 {
        // A torn tail can only be the remainder of a crashed append: the
        // metadata row commits strictly after the file write + fsync
        // return, so no committed pointer can reference a partial row.
        // Truncating to the last whole row is therefore always safe —
        // erroring instead would wedge the space (and abort every future
        // drain) behind one torn write.
        f.set_len(header.data_offset() + whole_rows * header.row_len())?;
        f.sync_all()?;
    }
    f.seek(SeekFrom::End(0))?;
    f.write_all(&as_bytes(row))?;
    f.sync_all()?;
    Ok(whole_rows)
}

/// Overwrite one row in place (replacement upsert; zeroing for scrub).
fn write_row(
    path: &Path,
    header: &PpvecHeader,
    file_row: u64,
    row: &[i8],
) -> VectorStoreResult<()> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    f.seek(SeekFrom::Start(
        header.data_offset() + file_row * header.row_len(),
    ))?;
    f.write_all(&as_bytes(row))?;
    f.sync_all()?;
    Ok(())
}

fn as_bytes(row: &[i8]) -> Vec<u8> {
    // A copy instead of an unsafe reinterpretation: rows are tiny (512
    // bytes) and this is the write path, not the scan path.
    row.iter().map(|&b| b as u8).collect()
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    // Directory fsync makes the create/rename durable on POSIX; harmless
    // where unsupported.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SQL plumbing
// ---------------------------------------------------------------------------

fn vec_kind_str(kind: VecKind) -> &'static str {
    match kind {
        VecKind::AnnotationChunk => "annotation_chunk",
        VecKind::ImageSummary => "image_summary",
        VecKind::ImageClip => "image_clip",
    }
}

/// Inverse of [`vec_kind_str`] for the `vec_kind` strings stored in the DB.
/// `None` for an unrecognized string (forward-compat / corruption guard).
fn vec_kind_from_str(s: &str) -> Option<VecKind> {
    match s {
        "annotation_chunk" => Some(VecKind::AnnotationChunk),
        "image_summary" => Some(VecKind::ImageSummary),
        "image_clip" => Some(VecKind::ImageClip),
        _ => None,
    }
}

/// Best-effort kind from a `.ppvec` FILENAME (`{vec_kind}.{model}.ppvec`): the
/// model id is sanitized + irrecoverable, but the kind prefix is intact. Used
/// only to label an orphan-file cleanup in the report.
fn vec_kind_from_prefix(file_name: &str) -> Option<VecKind> {
    let prefix = file_name.split('.').next()?;
    vec_kind_from_str(prefix)
}

/// Drop ALL metadata rows of one space (live or already-deleted). The flat file
/// is left to the orphan-file sweep, which removes any `.ppvec` no live row maps
/// to. Caller holds the connection lock + the file IO lock.
fn delete_space_rows(conn: &Connection, kind: VecKind, model_id: &str) -> VectorStoreResult<usize> {
    conn.execute(
        "DELETE FROM vectors WHERE vec_kind = ?1 AND model_id = ?2",
        params![vec_kind_str(kind), model_id],
    )
    .map_err(db_err)
}

/// `(WHERE fragment, ?3, ?4)` for one `VecUnit`. Two positional params
/// keep a single prepared shape per fragment.
fn unit_filter(unit: &VecUnit) -> (&'static str, String, Option<i64>) {
    match unit {
        VecUnit::AnnotationChunk {
            event_id,
            chunk_index,
        } => (
            "event_id = ?3 AND chunk_index = ?4",
            event_id.clone(),
            Some(i64::from(*chunk_index)),
        ),
        VecUnit::Image { image_hash } => {
            ("image_hash = ?3 AND ?4 IS NULL", image_hash.clone(), None)
        }
    }
}

fn unit_columns(unit: &VecUnit) -> (Option<String>, Option<String>, i64) {
    match unit {
        VecUnit::AnnotationChunk {
            event_id,
            chunk_index,
        } => (Some(event_id.clone()), None, i64::from(*chunk_index)),
        VecUnit::Image { image_hash } => (None, Some(image_hash.clone()), 0),
    }
}

/// Sanitize a model id for use in a filename (§1.3
/// `{model_id_sanitized}`): anything outside `[A-Za-z0-9._-]` becomes `_`.
pub fn sanitize_model_id(model_id: &str) -> String {
    model_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn db_err(e: rusqlite::Error) -> VectorStoreError {
    VectorStoreError::Metadata(e.to_string())
}
