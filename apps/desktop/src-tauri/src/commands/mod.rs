//! The Tauri command layer: thin orchestration over photoproof-core. No
//! business logic lives here — scope derivation is `scope.rs` (CAPTURE §3
//! mechanics), session policy is `session.rs`, text rules are `note.rs`,
//! reads are core APIs.
//!
//! Split per domain for parallel stage ownership (UI-ARCHITECTURE §5):
//! capture/search/library/app moved verbatim from the old commands.rs by
//! FOUNDATIONS; `journal.rs` (Stage C) and `os.rs` (Stage A) start empty.
//! Shared helpers live here; INTEGRATION registers new handlers in lib.rs.
//!
//! Image bytes never appear in any command (DECISIONS P16): previews are
//! served by the `photoproof://` protocol (protocol.rs).

pub mod app;
pub mod capture;
pub mod journal;
pub mod library;
pub mod os;
pub mod search;

use std::sync::Arc;

use photoproof_core::ContentHash;
use tauri::{AppHandle, Emitter, State};

use crate::dto::{DegradedFlags, IndicatorPulse, IndicatorState, JournalChanged};
use crate::error::{CmdError, CmdResult};
use crate::state::App;

pub(crate) type S<'a> = State<'a, Arc<App>>;

pub(crate) fn emit_pulse(handle: &AppHandle, event_kind: &'static str) {
    let _ = handle.emit("indicator-pulse", IndicatorPulse { event_kind });
}

/// `journal-changed` (BACKLOG): every committed journal mutation announces
/// the affected image hashes so open surfaces (journal panel, grid badges,
/// the Look overlay) refresh without frontend-triggered reloads — the seam
/// M2b's voice events (which land without UI actions) will ride. Events
/// with no image targets (session-level remarks) stay silent: no per-image
/// surface changed. Same emission pattern as `settings-changed` (app.rs).
pub(crate) fn emit_journal_changed(handle: &AppHandle, hashes: Vec<String>) {
    if hashes.is_empty() {
        return;
    }
    let _ = handle.emit("journal-changed", JournalChanged { hashes });
}

/// Shared hash plumbing (journal.rs + capture.rs commands).
pub(crate) fn parse_hash(hash: &str) -> CmdResult<ContentHash> {
    ContentHash::from_hex(hash).map_err(|e| CmdError::Invalid(format!("bad image hash: {e}")))
}

pub(crate) fn hashes(targets: &[ContentHash]) -> Vec<String> {
    targets.iter().map(|h| h.as_str().to_owned()).collect()
}

pub(crate) fn indicator(app: &App) -> IndicatorState {
    IndicatorState {
        current_scope: app.scope.lock().expect("scope mutex").current_view(),
        // No live voice pipeline in the shell yet: the core capture engine
        // (P6.1, mock-verified) reports these through its §11
        // `IndicatorState`; the shell maps them once P6.2's supervised ASR
        // client is wired. Until then: mic disarmed, ASR unavailable —
        // the mic glyph stays absent (UI §7.3).
        mic: "disarmed",
        streaming_utterance: None,
        degraded: DegradedFlags {
            asr_unavailable: true,
        },
    }
}
