//! Connector trait boundaries (spec/RUNTIME.md §4, spec/RETRIEVAL.md
//! §1.2–1.3 & §5.3, spec/CAPTURE.md §6.2).
//!
//! Every external capability — transcription, voice-activity detection,
//! embedding, language model, vector store, reranking — is a trait with
//! swappable implementations, selected at startup from config (§4.4).
//! Local↔cloud is symmetric by construction: swapping a backend is a
//! config change, never a code change.
//!
//! This crate is deliberately self-contained on std/primitive types: ids
//! cross the boundary as strings (ULID/blake3-hex), audio as f32 frames,
//! time as stream-clock milliseconds. It depends on no other photoproof
//! crate.
//!
//! Real implementations arrive with the runtime work packets; the
//! [`mock`] module ships deterministic, scriptable implementations for
//! every trait so capture, retrieval, and supervision can be tested
//! without models (docs/BUILD-LOOP.md).

// RUNTIME §4 is explicit: "Rust 2024, native `async fn` in traits;
// streaming uses `futures_core::stream::BoxStream` for object safety."
// The lint exists because native async-fn-in-trait futures carry no Send
// bound and the traits are not dyn-compatible; the spec chose this shape
// knowingly (generic/static dispatch at the call sites).
#![allow(async_fn_in_trait)]

pub mod config;
pub mod embedder;
pub mod error;
pub mod llm;
pub mod mock;
pub mod reranker;
pub mod transcriber;
pub mod vad;
pub mod vector_store;

pub use embedder::{DecodedImage, Embedder, Embedding};
pub use error::{ConnectorError, ConnectorResult};
pub use llm::{ChatMessage, ChatRequest, ChatResponse, Lane, LanguageModel, Role};
pub use reranker::{CandidateText, Reranker};
pub use transcriber::{AudioFrame, SegmentKind, StreamMs, Transcriber, TranscriptSegment};
pub use vad::{VadEvent, VadFrameResult, VoiceActivityDetector};
pub use vector_store::{
    VecHit, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError, VectorStoreResult,
};
