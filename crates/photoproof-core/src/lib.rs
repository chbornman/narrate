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

pub mod canonical;
pub mod capture;
pub mod event;
pub mod id;
pub mod library;
pub mod metrics;
pub mod retrieval;
pub mod runtime;
pub mod search;
pub mod sidecar;
pub mod store;

pub use canonical::{CanonicalParseError, canonical_json, parse_canonical};
pub use event::{Event, Kind, Payload, Source, StrokePayload, StrokePoint, Tool, ValidationError};
pub use id::{ContentHash, EventId, IdError, Minted, Minter, SessionId, UtcMillis};
pub use store::{
    AppendError, DirtyImage, DirtyReason, EventDraft, EventStore, JournalEntry, JournalStats,
    MergeReport, RedactError, RemarkSource, RetractionSource, SessionContext, SessionRecord,
    StoreError,
};
