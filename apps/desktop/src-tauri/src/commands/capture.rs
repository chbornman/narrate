//! Scope & capture commands (CAPTURE §3–4, §10–11) — moved verbatim from
//! the old commands.rs (FOUNDATIONS split; owned by no parallel stage).

use photoproof_core::{ContentHash, EventDraft, RemarkSource, UtcMillis};
use tauri::{AppHandle, Emitter};

use super::{S, emit_pulse, indicator};
use crate::dto::{IndicatorState, ScopeView};
use crate::error::{CmdError, CmdResult};
use crate::note::normalize_note;

/// The UI reports its selection/view-derived target list (ordered); the core
/// echoes the scope back (UI §3.4: the UI performs no scope logic).
#[tauri::command]
pub fn set_scope(app: S<'_>, handle: AppHandle, targets: Vec<String>) -> CmdResult<ScopeView> {
    app.touch()?;
    let hashes: Result<Vec<ContentHash>, _> =
        targets.iter().map(|t| ContentHash::from_hex(t)).collect();
    let hashes = hashes.map_err(|e| CmdError::Invalid(format!("bad target hash: {e}")))?;
    let view = {
        let mut scope = app.scope.lock().expect("scope mutex");
        scope.set(hashes, UtcMillis::now()).view()
    };
    let _ = handle.emit("indicator-state", indicator(&app));
    Ok(view)
}

#[tauri::command]
pub fn indicator_state(app: S<'_>) -> IndicatorState {
    indicator(&app)
}

/// Typed note (CAPTURE §4): binds to the current scope snapshot at submit
/// time; verbatim text (one trailing newline trimmed); empty mints nothing.
/// Returns whether an event was committed.
#[tauri::command]
pub fn add_note(app: S<'_>, handle: AppHandle, text: String) -> CmdResult<bool> {
    app.touch()?;
    let Some(text) = normalize_note(&text) else {
        return Ok(false);
    };
    let targets = app
        .scope
        .lock()
        .expect("scope mutex")
        .current()
        .targets
        .clone();
    let session = app.session_id();
    app.store.append(
        &session,
        EventDraft::Remark {
            source: RemarkSource::Typed,
            text,
            targets,
        },
        None,
    )?;
    emit_pulse(&handle, "remark");
    Ok(true)
}

/// Rating keys 0–5 (CAPTURE §10, DECISIONS C6): bound to the current scope
/// at keystroke time; multi-select mints ONE event targeting all N (selection
/// order); 0 = explicit clear; session scope → rating keys do nothing.
#[tauri::command]
pub fn set_rating(app: S<'_>, handle: AppHandle, value: u8) -> CmdResult<bool> {
    app.touch()?;
    if value > 5 {
        return Err(CmdError::Invalid(format!("rating out of range: {value}")));
    }
    let targets = {
        let scope = app.scope.lock().expect("scope mutex");
        let snap = scope.current();
        if snap.targets.is_empty() {
            return Ok(false); // session scope: rating keys do nothing
        }
        snap.targets.clone()
    };
    let session = app.session_id();
    app.store
        .append(&session, EventDraft::Rating { value, targets }, None)?;
    emit_pulse(&handle, "rating");
    Ok(true)
}

/// Generic activity report (CAPTURE §2.1: keyboard/pointer input, view
/// changes). The frontend throttles; this only refreshes the idle timer.
#[tauri::command]
pub fn report_activity(app: S<'_>) -> CmdResult<()> {
    app.touch()
}
