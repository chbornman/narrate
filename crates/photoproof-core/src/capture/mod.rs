//! The capture engine (spec/CAPTURE.md) — sessions, write scope, and the
//! voice pipeline, headless and seam-driven (packet P6.1, mock-verified).
//!
//! - `clock`: the Clock seam (§1) — monotonic decisions, wall-clock records
//! - `scope`: snapshots + the §3.1 ring buffer (`scope_at`)
//! - `audio`: the §7 in-memory audio ring (60 s, zeroed on disarm/error)
//! - `session`: §2 lifecycle — idle boundary, crash recovery, close hooks
//! - `engine`: §5/§6 — mic + utterance state machines, VAD-onset binding,
//!   voice minting, §9 linking, the §11 indicator contract
//! - `link`: the §9.1 resolution rule, pure
//!
//! The engine consumes photoproof-connectors' `Transcriber` /
//! `VoiceActivityDetector` traits (P1.2); real audio devices and process
//! supervision arrive with P6.2/P6.3 behind the same seams.

pub mod audio;
pub mod clock;
pub mod engine;
pub mod link;
pub mod scope;
pub mod session;

pub use audio::AudioRing;
pub use clock::{Clock, FakeClock, SystemClock};
pub use engine::{
    CaptureEngine, DRAIN_WINDOW_MS, DegradedFlags, IndicatorState, MicState, ONSET_ERROR_BUDGET_MS,
    StreamingView,
};
pub use link::{StrokeSpan, UtteranceSpan, resolve_stroke_link, resolve_utterance_link};
pub use scope::{ScopeKind, ScopeLookup, ScopeRing, ScopeSnapshot, ScopeView};
pub use session::{
    Activity, CaptureDrain, CloseProcessing, CloseProcessor, IDLE_BOUNDARY_MS, NoCapture, NoFlush,
    SessionEngine, SidecarFlush, recover_crashed_sessions,
};
