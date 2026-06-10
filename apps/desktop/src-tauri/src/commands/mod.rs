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

use tauri::{AppHandle, Emitter, State};

use crate::dto::{DegradedFlags, IndicatorPulse, IndicatorState};
use crate::state::App;

pub(crate) type S<'a> = State<'a, Arc<App>>;

pub(crate) fn emit_pulse(handle: &AppHandle, event_kind: &'static str) {
    let _ = handle.emit("indicator-pulse", IndicatorPulse { event_kind });
}

pub(crate) fn indicator(app: &App) -> IndicatorState {
    IndicatorState {
        current_scope: app.scope.lock().expect("scope mutex").current().view(),
        // M1: no ASR, no voice pipeline. The mic glyph is absent until ASR is
        // ready (UI §7.3); these fields are the P6.x seam.
        mic: "disarmed",
        streaming_utterance: None,
        degraded: DegradedFlags {
            asr_unavailable: true,
        },
    }
}
