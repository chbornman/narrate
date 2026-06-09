//! Vector-store boundary (spec/RETRIEVAL.md §1.2–1.3, trait sketch §1.3).
//!
//! All vector access goes through this trait; it is the scale escape hatch
//! (brute-force flat files now, usearch/sqlite-vec later behind the same
//! seam). Rows are derived, disposable, and rebuildable from the event log
//! + previews; embeddings never appear in event rows or sidecars (kernel).
//!
//! RETRIEVAL's sketch leaves `VecKey` and the error type undefined; this
//! module defines both from the §1.2 DDL (flagged in the P1.2 report).
//! Ids cross this boundary as strings: `event_id` is a ULID string,
//! `image_hash` a blake3 hex string (the crate has no dependency on
//! photoproof-core by design).

use serde::{Deserialize, Serialize};

use crate::embedder::Embedding;

/// The three vector spaces (RETRIEVAL §1.2 `vec_kind`, DECISIONS X4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VecKind {
    /// One chunk of one event's folded text, keyed by `event_id`.
    AnnotationChunk,
    /// Embedding of the per-image rolling summary; one live row per
    /// (image, model).
    ImageSummary,
    /// Image embedding of the preview pixels (CLIP image tower); one live
    /// row per (image, model).
    ImageClip,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VecSpace {
    pub vec_kind: VecKind,
    pub model_id: String,
}

/// The unit a vector row describes within a space — the natural key of the
/// RETRIEVAL §1.2 unique indexes (`vectors_chunk` / `vectors_image`):
/// exactly one of `event_id` or `image_hash` is set per row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VecUnit {
    /// `annotation_chunk` rows: (event_id, chunk_index).
    AnnotationChunk { event_id: String, chunk_index: u32 },
    /// `image_summary` / `image_clip` rows: image_hash (blake3 hex).
    Image { image_hash: String },
}

/// Full key of one vector row: space + unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VecKey {
    pub space: VecSpace,
    pub unit: VecUnit,
}

/// Joins back to the `vectors` table by rowid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VecHit {
    pub vector_id: i64,
    pub score: f32,
}

/// Storage-side error. Distinct from `ConnectorError`: the vector store is
/// local derived storage, not a supervised backend — its failures are IO
/// and integrity, never readiness/transport (choice flagged in the P1.2
/// report; RETRIEVAL's sketch says only `Result`).
#[derive(Debug, thiserror::Error)]
pub enum VectorStoreError {
    #[error("vector store io error")]
    Io(#[from] std::io::Error),
    /// Vector length disagrees with the space's recorded `dims`
    /// (RETRIEVAL §1.2: `dims` is stored with every vector).
    #[error("dimension mismatch in {space:?}: expected {expected}, got {got}")]
    DimensionMismatch {
        space: VecSpace,
        expected: usize,
        got: usize,
    },
    /// Embedding's `model_id` disagrees with the space's — mixing models
    /// in one space is corruption, never a soft fallback.
    #[error("model mismatch: space expects {expected}, embedding carries {got}")]
    ModelMismatch { expected: String, got: String },
    /// `mark_deleted`/`scrub` addressed a row that does not exist.
    #[error("no vector row for key {0:?}")]
    NotFound(VecKey),
    /// On-disk state failed validation (bad header, truncated file …).
    #[error("vector store corrupt: {0}")]
    Corrupt(String),
}

pub type VectorStoreResult<T> = Result<T, VectorStoreError>;

pub trait VectorStore: Send + Sync {
    /// Insert or replace the row at `key`. Replacing un-deletes.
    fn upsert(&self, key: VecKey, v: &Embedding) -> VectorStoreResult<()>;
    /// Brute-force top-k over all non-deleted rows of `space`; vectors are
    /// L2-normalized so cosine similarity = dot product. A space with no
    /// rows yields an empty hit list.
    fn search(
        &self,
        query: &Embedding,
        space: VecSpace,
        k: usize,
    ) -> VectorStoreResult<Vec<VecHit>>;
    /// Logical delete (`deleted=1`); search skips, compaction reclaims.
    fn mark_deleted(&self, key: VecKey) -> VectorStoreResult<()>;
    /// Physical zero of the stored row bytes + logical delete — redaction
    /// (RETRIEVAL §1.3: the scrub must be physical, per EVENTS.md).
    fn scrub(&self, key: VecKey) -> VectorStoreResult<()>;
    /// Rewrite the space dropping dead rows (RETRIEVAL §1.3 thresholds:
    /// deleted > 20% of a file or 10,000 — the *caller* schedules this;
    /// the store only executes it).
    fn compact(&self, space: VecSpace) -> VectorStoreResult<()>;
}
