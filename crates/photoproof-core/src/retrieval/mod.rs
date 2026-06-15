//! Retrieval storage: PPVEC v2 vector files, §2 chunking, and the
//! `inputs_hash` staleness recipe.
//!
//! Contract: spec/RETRIEVAL.md §1.2–1.3 (vectors table + flat files), §2
//! (chunking), §3 (what is embedded, instruction prefixes). The embedding
//! ingest passes that feed this storage live in `crate::library` (LIBRARY
//! §10 queue mechanics, DECISIONS L4); the M3 query pipeline that consumes
//! it is packet P7.2.
//!
//! B69 (retrieval stays additive): everything here adds machine signals
//! beside the photographer's own words; nothing retires CLIP or caption
//! signals based on journal coverage.

mod chunk;
mod ppvec;

pub use chunk::{Chunk, ChunkContext, PREFIX_SCHEME_VERSION, chunk_folded_text};
pub use ppvec::{
    COMPACT_DEAD_FRACTION, COMPACT_DEAD_ROWS, DTYPE_F32, DTYPE_INT8, KnnGraph, MRL_DIMS,
    PpvecHeader, PpvecStore, ReconciledSpace, SpaceReconcileReason, SpaceReconcileReport, VecMeta,
    default_vectors_dir, mrl_truncate_normalize, sanitize_model_id,
};
pub(crate) use ppvec::{zero_deleted_rows_for_event, zero_deleted_rows_for_image};

/// Version of the §3 query instruct template. An `inputs_hash` input
/// (§1.2): a template change invalidates and re-embeds rather than
/// silently mixing recipes.
pub const INSTRUCT_TEMPLATE_VERSION: u32 = 1;

/// The §3 query-side instruct template for the (instruction-aware) text
/// embedder, verbatim from the spec. Documents are embedded BARE — the
/// asymmetry is the model's convention. The CLIP text tower takes bare
/// query text, never this template.
pub fn instruct_query(q: &str) -> String {
    format!(
        "Instruct: Given a photographer's search, retrieve their journal notes about matching images\nQuery: {q}"
    )
}

/// The §1.2 `inputs_hash` recipe: blake3 over the embedded payload (chunk
/// embed-text bytes, or preview bytes) plus the context-prefix scheme
/// version (§2) and the instruct template version (§3). Any recipe change
/// re-embeds instead of silently mixing.
pub fn inputs_hash(payload: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(payload);
    hasher.update(&PREFIX_SCHEME_VERSION.to_le_bytes());
    hasher.update(&INSTRUCT_TEMPLATE_VERSION.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}
