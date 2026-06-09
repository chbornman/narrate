//! Optional cross-encoder reranker (spec/RETRIEVAL.md §5.3 stage 3b,
//! DECISIONS P7). M3+, default OFF behind config; the fused order is the
//! fallback whenever the reranker is off, unavailable, or over budget —
//! search never blocks on it.

use crate::error::ConnectorResult;

/// One candidate's best-evidence text: a RETRIEVAL §5.4 best-quote span
/// (1–3 sentences), never a full document. RETRIEVAL's sketch leaves this
/// type undefined; the minimal shape is the text itself (flagged in the
/// P1.2 report) — callers keep their own index → image mapping, since
/// `rerank` returns indices into `candidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateText {
    pub text: String,
}

pub trait Reranker: Send + Sync {
    /// Reorders `candidates` by query relevance; returns the new order as
    /// indices into `candidates` (a permutation of `0..candidates.len()`).
    fn rerank(&self, query: &str, candidates: &[CandidateText]) -> ConnectorResult<Vec<usize>>;

    fn model_id(&self) -> &str;
}
