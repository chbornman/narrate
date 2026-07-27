//! Debug-panel commands (spec/UI.md §10, DECISIONS I6). This module exists
//! in DEV binaries (debug_assertions — matching the vite dev server, which
//! always renders the panel) and under the explicit `debug-panel` cargo
//! feature for debug bundles. Release binaries carry none of these
//! commands, and invoking them fails as unknown
//! (scripts/assert-release-clean.sh).
//!
//! Read-only except the explicitly-marked dev actions: force sidecar flush,
//! force rescan (per root).

#![cfg(any(feature = "debug-panel", debug_assertions))]

use std::sync::Arc;

use photoproof_core::UtcMillis;
use photoproof_core::library::ScanOptions;
use serde::Serialize;
use tauri::State;

use crate::command_work::CommandClass;
use crate::commands::{admit, run_blocking};
use crate::error::CmdResult;
use crate::search_types::QueryEcho;
use crate::state::App;

/// Marker string asserted absent from release binaries by
/// scripts/assert-release-clean.sh.
pub const DEBUG_PANEL_MARKER: &str = "PP_DEBUG_PANEL_RUST_MARKER";

type S<'a> = State<'a, Arc<App>>;

/// Silent server-side clamp on the events-tail row count, whatever the
/// panel asks for: each row can carry full note text and target lists, so
/// the clamp bounds the IPC payload. This is why the panel never shows
/// more than 500 rows.
const MAX_TAIL_ROWS: u32 = 500;

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
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.tail-events", CommandClass::Read)?;
    let conn = app.readq.lock().expect("readq mutex");
    let mut stmt = conn.prepare_cached(
        "SELECT id, session_id, ts, kind, source, text, payload,
                target_event, linked_event, redacted_by
         FROM annotation_events ORDER BY id DESC LIMIT ?1",
    )?;
    let mut rows: Vec<DebugEventRow> = stmt
        .query_map([limit.min(MAX_TAIL_ROWS)], |r| {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugCapture {
    /// CAPTURE §6.4 mic state, straight off the live engine.
    pub mic: &'static str,
    /// The transcriber pipeline is open (armed and not torn down).
    pub stream_open: bool,
    /// Utterances with a VAD onset and no Final yet.
    pub streaming_utterances: usize,
    /// Utterances abandoned (fatal error / drain timeout), lifetime.
    pub abandoned: u64,
    /// §8.3 readiness of the supervised ASR child.
    pub asr_ready: bool,
    /// The engine's live note feed (P6.4): VAD onsets, partials (dev
    /// builds carry the text), binding decisions, failure reasons.
    /// Newest last; refresh while speaking to watch transcription move.
    pub notes: Vec<String>,
    /// The write-scope snapshot ring (CAPTURE §3.1).
    pub scope_ring: Vec<DebugScopeSnapshot>,
}

/// Capture tab: the LIVE engine (mic state, stream, in-flight utterances,
/// the debug-note feed — partials included in dev builds) plus the scope
/// ring. This is the "is it hearing me?" window: arm, speak, refresh.
#[tauri::command]
pub fn debug_capture(app: S<'_>) -> CmdResult<DebugCapture> {
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.capture", CommandClass::Read)?;
    let scope_ring = {
        let scope = app.scope.lock().expect("scope mutex");
        scope
            .history()
            .map(|s| DebugScopeSnapshot {
                kind: s.kind.as_str(),
                targets: s.targets.iter().map(|h| h.as_str().to_owned()).collect(),
                captured_at: s.captured_at.to_rfc3339(),
            })
            .collect()
    };
    let capture = app.capture.lock().expect("capture mutex");
    match capture.as_ref() {
        Some(engine) => Ok(DebugCapture {
            mic: engine.mic().as_str(),
            stream_open: engine.stream_open(),
            streaming_utterances: engine.streaming_count(),
            abandoned: engine.abandoned_count(),
            asr_ready: app.runtime.supervisors.asr_ready(),
            notes: engine.debug_notes().to_vec(),
            scope_ring,
        }),
        None => Ok(DebugCapture {
            mic: "disarmed",
            stream_open: false,
            streaming_utterances: 0,
            abandoned: 0,
            asr_ready: app.runtime.supervisors.asr_ready(),
            notes: vec!["capture engine absent: in-process VAD failed to build".into()],
            scope_ring,
        }),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugIngest {
    pub counters: Vec<DebugPassCounter>,
    /// Cumulative per-stage wall-clock (BACKLOG metrics, first slice).
    /// Process-lifetime counters: refresh twice and diff for rates.
    pub stages: Vec<DebugStageStat>,
    pub recent_errors: Vec<(String, String, String)>,
    pub log: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStageStat {
    pub stage: &'static str,
    pub count: u64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
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
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.ingest", CommandClass::Read)?;
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
    let stages = app
        .library
        .metrics_snapshot()
        .into_iter()
        .map(|s| DebugStageStat {
            stage: s.stage,
            count: s.count,
            total_ms: s.total_ms,
            mean_ms: s.mean_ms,
            p50_ms: s.p50_ms,
            p95_ms: s.p95_ms,
            p99_ms: s.p99_ms,
            max_ms: s.max_ms,
        })
        .collect();
    Ok(DebugIngest {
        counters,
        stages,
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
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.sidecars", CommandClass::Read)?;
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
pub fn debug_search(app: S<'_>) -> CmdResult<Option<QueryEcho>> {
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.search", CommandClass::Read)?;
    let _ = DEBUG_PANEL_MARKER; // keep the marker in the binary
    Ok(app.last_search.lock().expect("last_search mutex").clone())
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
pub fn debug_runtime(app: S<'_>) -> CmdResult<DebugRuntime> {
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.runtime", CommandClass::Read)?;
    Ok(DebugRuntime {
        processes: app.runtime.debug_lines(),
        status: app.runtime.status(),
    })
}

/// [dev] Force sidecar flush.
#[tauri::command]
pub fn debug_force_flush(app: S<'_>) -> CmdResult<usize> {
    let app = app.inner().clone();
    let _permit = admit(&app, "debug.force-flush", CommandClass::Mutation)?;
    let flushed = app.engine.flush_all(UtcMillis::now())?;
    Ok(flushed.len())
}

/// [dev] Force rescan of one root.
#[tauri::command]
pub async fn debug_force_rescan(app: S<'_>, root_id: String) -> CmdResult<usize> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "debug.force-rescan",
        CommandClass::Mutation,
        move |app| {
            let report = app.library.scan_root(&root_id, &ScanOptions::default())?;
            Ok(report.files_seen)
        },
    )
    .await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugDoctorReport {
    pub repended: usize,
    pub stale_orphans: usize,
    pub temps_swept: usize,
}

/// [dev] Run the library doctor NOW (BACKLOG "Library doctor"; the
/// maintenance tick runs the same pass on its own schedule). A repair
/// action, but a CONSERVATIVE one — doctor v1 re-pends and counts, it
/// never deletes — so it sits beside flush/rescan in the dev row.
#[tauri::command]
pub async fn debug_doctor(app: S<'_>) -> CmdResult<DebugDoctorReport> {
    let app = app.inner().clone();
    run_blocking(app, "debug.doctor", CommandClass::Mutation, |app| {
        let r = app.library.doctor()?;
        Ok(DebugDoctorReport {
            repended: r.repended,
            stale_orphans: r.stale_orphans,
            temps_swept: r.temps_swept,
        })
    })
    .await
}
