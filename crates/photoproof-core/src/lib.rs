//! Photoproof core: the append-only journal engine.
//!
//! Normative contracts live in `spec/` at the repository root; this crate
//! implements them. No Tauri, no UI dependencies — drivable from tests and
//! a future CLI.
//!
//! - `id`: ContentHash, EventId/SessionId, UtcMillis, monotonic Minter (EVENTS §1)
//! - `event`: the event record + shape validation (EVENTS §2–3)
//! - `canonical`: canonical JSON serialization, byte-exact round-trip (EVENTS §4)
//! - `store`: SQLite schema + EventStore (append/fold/redact/merge) (EVENTS §5–10)
//! - `capture`: sessions, write scope, the voice pipeline (CAPTURE, P6.1)
//! - `runtime`: supervision, weights, tiers, scheduling (RUNTIME, P6.2)
//! - `retrieval`: PPVEC vector storage + chunking (RETRIEVAL, P7.1)
//! - `collections`: named collections + portability file (RETRIEVAL §10, P7.3)
//! - `voice_wer`: word-error-rate scoring for the voice stress harness (BACKLOG)
//! - `voice_bench`: the shared headless voice-pipeline harness (VAD+ASR+engine
//!   wiring + gated-vs-raw scoring) driven by `pp-voice-bench` and `pp-sweep voice`
//! - `retrieval_eval`: pure IR metrics + golden-query-set format for the M3
//!   retrieval-quality gate (RETRIEVAL §12; the `pp-retrieval-eval` runner)
//! - `tuning`: centralized, file-overridable tuning config — ONE home for the
//!   scattered ranking/preview weights (DESIGN-TUNING-CONFIG.md, docs/tuning.html)
//! - `topic`: semantic topic-graph backend — per-image affinity to named topic
//!   anchors (visual/said blend) + cheap topic suggestions (DESIGN-SEMANTIC-GRAPH.md)
//! - `topics`: manual saved-topic CRUD store — a topic is a saved phrase, like a
//!   saved search; its images are always computed affinity (DESIGN-TOPICS-COLLECTIONS.md)

pub mod canonical;
pub mod capture;
pub mod collections;
pub mod event;
pub mod id;
pub mod library;
pub mod metrics;
pub mod retrieval;
pub mod retrieval_eval;
pub mod runtime;
pub mod search;
pub mod sidecar;
pub mod store;
pub mod topic;
pub mod topics;
pub mod tuning;
pub mod voice_bench;
pub mod voice_wer;

pub use canonical::{CanonicalParseError, canonical_json, parse_canonical};
pub use event::{Event, Kind, Payload, Source, StrokePayload, StrokePoint, Tool, ValidationError};
pub use id::{ContentHash, EventId, IdError, Minted, Minter, SessionId, UtcMillis};
pub use store::{
    AppendError, DirtyImage, DirtyReason, DwellSource, EventDraft, EventStore, ImageIntensity,
    JournalEntry, JournalStats, MergeReport, RedactError, RemarkSource, RetractionSource,
    SessionContext, SessionRecord, StoreError,
};
