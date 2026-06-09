//! Embedder connector boundary (spec/RUNTIME.md §4, normative; consumed by
//! spec/RETRIEVAL.md §3).

use crate::error::ConnectorResult;

#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// L2-normalized.
    pub vector: Vec<f32>,
    /// e.g. "ViT-H-14-378-quickgelu__dfn5b".
    pub model_id: String,
}

/// Decoded, display-oriented sRGB pixels (LIBRARY.md owns decode).
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    pub rgb8: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Two configured instances exist behind this one trait (RUNTIME §3.3,
/// DECISIONS X3): the TEXT embedder (annotation chunks & summaries;
/// embed_image unsupported) and the CLIP embedder (image vectors;
/// embed_text = short queries only — its CLIP text tower truncates at 77
/// tokens, ample for queries, never for chunks).
pub trait Embedder: Send + Sync {
    async fn embed_text(&self, text: &str) -> ConnectorResult<Embedding>;
    async fn embed_image(&self, img: &DecodedImage) -> ConnectorResult<Embedding>;
    /// 1024 for the DFN5B ViT-H-14 presets; per the configured text model
    /// for the text instance. Stored with every vector.
    fn dimensions(&self) -> usize;
    fn model_id(&self) -> &str;
}
