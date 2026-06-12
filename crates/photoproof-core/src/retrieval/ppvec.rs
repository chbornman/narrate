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
//! Brute-force scan: v1 reads the file sequentially. The multithreaded
//! SIMD/mmap scan path and the usearch escape hatch (§1.3) sit behind this
//! same trait; nothing here is load-bearing for that swap.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::vector_store::{
    VecHit, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError, VectorStoreResult,
};
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

const MAGIC: &[u8; 6] = b"PPVEC\x02";
/// magic(6) + dims u32 LE (4) + dtype u8 (1) + reserved zero padding (5).
const HEADER_LEN: u64 = 16;

pub const DTYPE_F32: u8 = 0;
pub const DTYPE_INT8: u8 = 1;

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

/// The PPVEC v2 store: SQLite metadata + one flat file per space.
pub struct PpvecStore {
    db: Mutex<Connection>,
    dir: PathBuf,
}

impl PpvecStore {
    /// Open over the shared photoproof database; `dir` hosts the flat
    /// files (`appdata/vectors/`), created if missing. Opening the
    /// EventStore first runs schema migrations, mirroring `Library::open`.
    pub fn open(db_path: impl AsRef<Path>, dir: impl Into<PathBuf>) -> VectorStoreResult<Self> {
        let db_path = db_path.as_ref();
        drop(
            crate::store::EventStore::open(db_path)
                .map_err(|e| VectorStoreError::Metadata(format!("schema: {e}")))?,
        );
        let conn = crate::library::open_library_connection(db_path).map_err(db_err)?;
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            db: Mutex::new(conn),
            dir,
        })
    }

    /// `appdata/vectors/{vec_kind}.{model_id_sanitized}.ppvec` (§1.3).
    pub fn file_path(&self, space: &VecSpace) -> PathBuf {
        self.dir.join(format!(
            "{}.{}.ppvec",
            vec_kind_str(space.vec_kind),
            sanitize_model_id(&space.model_id)
        ))
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

    /// `(live_rows, total_file_rows)` for a space — the compaction
    /// threshold inputs. Total comes from the file length, so orphaned
    /// rows (crash between file append and metadata commit) count as dead.
    pub fn space_stats(&self, space: &VecSpace) -> VectorStoreResult<(u64, u64)> {
        let path = self.file_path(space);
        if !path.exists() {
            return Ok((0, 0));
        }
        let mut f = File::open(&path)?;
        let header = read_header(&mut f)?;
        let total = (f.metadata()?.len().saturating_sub(header.data_offset())) / header.row_len();
        let conn = self.db.lock().expect("poisoned");
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
    /// and redaction (RETRIEVAL §1.1) — it cannot reach the flat files; the
    /// embedding drain calls this so the redaction contract's byte-zeroing
    /// happens on the next drain. Zeroing every dead row (not just
    /// redactions) is deliberate: core cannot cheaply distinguish causes,
    /// and zeroing is always safe, strictly stronger than the contract.
    pub fn sweep_dead(&self) -> VectorStoreResult<usize> {
        let conn = self.db.lock().expect("poisoned");
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
            let path = self.dir.join(format!(
                "{}.{}.ppvec",
                vec_kind,
                sanitize_model_id(model_id)
            ));
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

    fn locate(&self, key: &VecKey) -> VectorStoreResult<Option<i64>> {
        let conn = self.db.lock().expect("poisoned");
        let (sql, p1, p2) = unit_filter(&key.unit);
        conn.query_row(
            &format!(
                "SELECT file_row FROM vectors
                 WHERE vec_kind = ?1 AND model_id = ?2 AND {sql}"
            ),
            params![vec_kind_str(key.space.vec_kind), key.space.model_id, p1, p2],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)
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
        if !path.exists() {
            return Ok(Vec::new());
        }
        let (header, data) = {
            let mut f = File::open(&path)?;
            let header = read_header(&mut f)?;
            f.seek(SeekFrom::Start(header.data_offset()))?;
            let mut data = Vec::new();
            f.read_to_end(&mut data)?;
            (header, data)
        };
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

        let live: Vec<(i64, i64)> = {
            let conn = self.db.lock().expect("poisoned");
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
        let mut hits: Vec<VecHit> = Vec::with_capacity(live.len());
        for (vector_id, file_row) in live {
            let start = file_row as usize * dims;
            let Some(row) = data.get(start..start + dims) else {
                return Err(VectorStoreError::Corrupt(format!(
                    "{}: file_row {file_row} beyond file end",
                    path.display()
                )));
            };
            let score: f32 = row
                .iter()
                .enumerate()
                .map(|(i, &b)| dequantize(b as i8, i, &header) * q[i])
                .sum();
            hits.push(VecHit { vector_id, score });
        }
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
        let Some(file_row) = self.locate(&key)? else {
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
        self.mark_deleted(key)
    }

    fn compact(&self, space: VecSpace) -> VectorStoreResult<()> {
        let path = self.file_path(&space);
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

        let mut conn = self.db.lock().expect("poisoned");
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

        // Rewrite dropping dead rows: temp file + rename in the same
        // directory, then remap `file_row` in one transaction (§1.3). A
        // crash between rename and commit leaves stale pointers — vectors
        // are derived and rebuildable, and the pass re-runs heal it.
        let tmp = path.with_extension("ppvec.compact-tmp");
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
        fs::rename(&tmp, &path)?;
        sync_dir(&self.dir)?;

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
        tx.commit().map_err(db_err)?;
        Ok(())
    }
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
fn ensure_file(path: &Path, dims: usize) -> VectorStoreResult<PpvecHeader> {
    if path.exists() {
        let mut f = File::open(path)?;
        return read_header(&mut f);
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
    if data_len % header.row_len() != 0 {
        return Err(VectorStoreError::Corrupt(format!(
            "{}: misaligned data region ({data_len} bytes)",
            path.display()
        )));
    }
    let file_row = data_len / header.row_len();
    f.seek(SeekFrom::End(0))?;
    f.write_all(&as_bytes(row))?;
    f.sync_all()?;
    Ok(file_row)
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
