//! Debug-panel commands (spec/UI.md §10, DECISIONS I6). This module exists
//! ONLY under the `debug-panel` cargo feature: release binaries carry none
//! of these commands, and invoking them fails as unknown.
//!
//! Read-only except the explicitly-marked dev actions: force sidecar flush,
//! force rescan (per root).

#![cfg(feature = "debug-panel")]

use std::sync::Arc;

use photoproof_core::UtcMillis;
use photoproof_core::library::ScanOptions;
use serde::Serialize;
use tauri::State;

use crate::error::{CmdError, CmdResult};
use crate::search_types::QueryEcho;
use crate::state::App;

/// Marker string asserted absent from release binaries by
/// scripts/assert-release-clean.sh.
pub const DEBUG_PANEL_MARKER: &str = "PP_DEBUG_PANEL_RUST_MARKER";

type S<'a> = State<'a, Arc<App>>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEventRow {
    pub id: String,
    pub session_id: String,
    pub ts: String,
    pub kind: String,
    pub source: String,
    pub text: Option<String>,
    pub payload: Option<String>,
    pub target_event: Option<String>,
    pub linked_event: Option<String>,
    pub redacted_by: Option<String>,
    pub targets: Vec<String>,
}

/// Events tab: live tail of `annotation_events`, newest first.
#[tauri::command]
pub fn debug_tail_events(app: S<'_>, limit: u32) -> CmdResult<Vec<DebugEventRow>> {
    let conn = app.readq.lock().expect("readq mutex");
    let mut stmt = conn.prepare_cached(
        "SELECT id, session_id, ts, kind, source, text, payload,
                target_event, linked_event, redacted_by
         FROM annotation_events ORDER BY id DESC LIMIT ?1",
    )?;
    let mut rows: Vec<DebugEventRow> = stmt
        .query_map([limit.min(500)], |r| {
            Ok(DebugEventRow {
                id: r.get(0)?,
                session_id: r.get(1)?,
                ts: r.get(2)?,
                kind: r.get(3)?,
                source: r.get(4)?,
                text: r.get(5)?,
                payload: r.get(6)?,
                target_event: r.get(7)?,
                linked_event: r.get(8)?,
                redacted_by: r.get(9)?,
                targets: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut tstmt = conn.prepare_cached(
        "SELECT image_hash FROM event_targets WHERE event_id = ?1 ORDER BY position",
    )?;
    for row in &mut rows {
        row.targets = tstmt
            .query_map([&row.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(rows)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugScopeSnapshot {
    pub kind: &'static str,
    pub targets: Vec<String>,
    pub captured_at: String,
}

/// Capture tab (M1 slice): the write-scope snapshot ring (CAPTURE §3.1).
/// VAD/binding/link entries exist on the core capture engine's debug-note
/// feed (P6.1, `CaptureEngine::debug_notes`); they render here once P6.2
/// wires a live engine into the shell.
#[tauri::command]
pub fn debug_capture(app: S<'_>) -> Vec<DebugScopeSnapshot> {
    let scope = app.scope.lock().expect("scope mutex");
    scope
        .history()
        .map(|s| DebugScopeSnapshot {
            kind: s.kind.as_str(),
            targets: s.targets.iter().map(|h| h.as_str().to_owned()).collect(),
            captured_at: s.captured_at.to_rfc3339(),
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugIngest {
    pub counters: Vec<DebugPassCounter>,
    pub recent_errors: Vec<(String, String, String)>,
    pub log: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugPassCounter {
    pub pass: String,
    pub version: i64,
    pub pending: u64,
    pub running: u64,
    pub done: u64,
    pub error: u64,
    pub skipped: u64,
}

/// Ingest tab: queue depth, per-pass states with versions, recent errors.
#[tauri::command]
pub fn debug_ingest(app: S<'_>) -> CmdResult<DebugIngest> {
    let counters = app
        .library
        .pass_counters()?
        .into_iter()
        .map(|((pass, version), c)| DebugPassCounter {
            pass,
            version,
            pending: c.pending,
            running: c.running,
            done: c.done,
            error: c.error,
            skipped: c.skipped,
        })
        .collect();
    let conn = app.readq.lock().expect("readq mutex");
    let mut stmt = conn.prepare_cached(
        "SELECT image_hash, pass_name, error FROM ingest_passes
         WHERE state = 'error' ORDER BY completed_at DESC LIMIT 50",
    )?;
    let recent_errors = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(DebugIngest {
        counters,
        recent_errors,
        log: app.library.take_debug_log(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSidecars {
    pub dirty: Vec<(String, String, String)>,
    pub redaction_queue: Vec<(String, String)>,
}

/// Sidecars tab: durable dirty queue + pending offline-volume redactions.
#[tauri::command]
pub fn debug_sidecars(app: S<'_>) -> CmdResult<DebugSidecars> {
    let dirty = app
        .store
        .dirty_images()?
        .into_iter()
        .map(|d| {
            (
                d.image.as_str().to_owned(),
                d.reason.as_str().to_owned(),
                d.since_ts.to_rfc3339(),
            )
        })
        .collect();
    let redaction_queue = app
        .engine
        .redaction_queue()?
        .into_iter()
        .map(|(e, h)| (e.as_str().to_owned(), h.as_str().to_owned()))
        .collect();
    Ok(DebugSidecars {
        dirty,
        redaction_queue,
    })
}

/// Search tab: the last query echo (raw, filters, dropped, fallback).
#[tauri::command]
pub fn debug_search(app: S<'_>) -> Option<QueryEcho> {
    let _ = DEBUG_PANEL_MARKER; // keep the marker in the binary
    app.last_search.lock().expect("last_search mutex").clone()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugRuntime {
    /// Plan/supervision lines (RUNTIME §8.1 detail: states, tier, the
    /// orphan sweep, config warnings, download progress/failures).
    pub processes: Vec<String>,
    /// The full status snapshot the settings/consent surfaces render.
    pub status: crate::dto::RuntimeStatus,
}

/// Runtime tab (RUNTIME §8.1/§8.6): plan + supervision detail. No live
/// child exists before the P6.3 spike vendors binaries; supervisor state
/// histories and scheduler decisions join these lines when they do.
#[tauri::command]
pub fn debug_runtime(app: S<'_>) -> DebugRuntime {
    DebugRuntime {
        processes: app.runtime.debug_lines(),
        status: app.runtime.status(),
    }
}

/// [dev] Force sidecar flush.
#[tauri::command]
pub fn debug_force_flush(app: S<'_>) -> CmdResult<usize> {
    let flushed = app.engine.flush_all(UtcMillis::now())?;
    Ok(flushed.len())
}

/// [dev] Force rescan of one root.
#[tauri::command]
pub async fn debug_force_rescan(app: S<'_>, root_id: String) -> CmdResult<usize> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let report = app.library.scan_root(&root_id, &ScanOptions::default())?;
        Ok(report.files_seen)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}
