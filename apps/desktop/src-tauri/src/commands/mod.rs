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

/// Voice events land without UI actions (CAPTURE §11): announce each like
/// any typed commit — a pulse per event, one `journal-changed` carrying
/// every touched hash. Session-scoped remarks have zero targets and stay
/// silent on the journal channel by `emit_journal_changed`'s own rule.
pub(crate) fn announce_events(handle: &AppHandle, events: &[photoproof_core::Event]) {
    if events.is_empty() {
        return;
    }
    for _ in events {
        emit_pulse(handle, "remark");
    }
    let mut touched: Vec<String> = events
        .iter()
        .flat_map(|e| e.targets.iter().map(|h| h.as_str().to_owned()))
        .collect();
    touched.sort();
    touched.dedup();
    emit_journal_changed(handle, touched);
}

/// Core capture `ScopeView` → the wire shape (CAPTURE §11).
fn scope_view_dto(view: photoproof_core::capture::ScopeView) -> crate::dto::ScopeView {
    crate::dto::ScopeView {
        kind: view.kind.as_str(),
        count: view.count,
        preview_hashes: view
            .preview_hashes
            .iter()
            .map(|h| h.as_str().to_owned())
            .collect(),
    }
}

pub(crate) fn indicator(app: &App) -> IndicatorState {
    // The scope tracker stays the indicator's scope truth (it is what a
    // typed note binds to); the engine's ring receives the same updates
    // for VOICE binding at onset time (set_scope feeds both).
    let current_scope = app.scope.lock().expect("scope mutex").current_view();
    let capture = app.capture.lock().expect("capture mutex");
    match capture.as_ref() {
        // P6.4: the LIVE engine's §11 state.
        Some(engine) => {
            let s = engine.indicator();
            IndicatorState {
                current_scope,
                mic: s.mic.as_str(),
                streaming_utterance: s.streaming_utterance.map(|u| crate::dto::StreamingView {
                    bound_scope: scope_view_dto(u.bound_scope),
                    started_at: u.started_at.to_rfc3339(),
                }),
                degraded: DegradedFlags {
                    // Muted-mic renders when the engine saw a failure OR
                    // the supervised child simply isn't ready (§8.3).
                    asr_unavailable: s.degraded.asr_unavailable
                        || !app.runtime.supervisors.asr_ready(),
                },
            }
        }
        // The in-process VAD never built: voice is off for this run.
        None => IndicatorState {
            current_scope,
            mic: "disarmed",
            streaming_utterance: None,
            degraded: DegradedFlags {
                asr_unavailable: true,
            },
        },
    }
}
