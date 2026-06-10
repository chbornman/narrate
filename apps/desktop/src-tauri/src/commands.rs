//! The Tauri command layer: thin orchestration over photoproof-core. No
//! business logic lives here — scope derivation is `scope.rs` (CAPTURE §3
//! mechanics), session policy is `session.rs`, text rules are `note.rs`,
//! reads are `gridq.rs`/core APIs.
//!
//! Image bytes never appear in any command (DECISIONS P16): previews are
//! served by the `photoproof://` protocol (protocol.rs).

use std::sync::Arc;

use photoproof_core::library::ScanOptions;
use photoproof_core::sidecar::VolumeInfo;
use photoproof_core::{ContentHash, EventDraft, RemarkSource, UtcMillis};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::dto::{
    DegradedFlags, ExportReportDto, FolderNode, GridItem, IndicatorPulse, IndicatorState,
    IngestStatus, RebuildReportDto, RootDto, RuntimeStatus, ScopeView,
};
use crate::error::{CmdError, CmdResult};
use crate::note::normalize_note;
use crate::pump;
use crate::search_types::{Filter, SearchResults};
use crate::settings::AppSettings;
use crate::state::App;

type S<'a> = State<'a, Arc<App>>;

fn emit_pulse(handle: &AppHandle, event_kind: &'static str) {
    let _ = handle.emit("indicator-pulse", IndicatorPulse { event_kind });
}

fn indicator(app: &App) -> IndicatorState {
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

// ---------------------------------------------------------------------------
// Scope & capture (CAPTURE §3–4, §10–11)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Search (RETRIEVAL §4 / §5.4 — M1 contract)
// ---------------------------------------------------------------------------

/// M1 search over the journal (RETRIEVAL §4 engine, packet P3.1).
///
/// A new keystroke's query interrupts any in-flight statement first
/// (§4 search-as-you-type); the interrupted call surfaces as an error
/// its (stale) caller discards.
#[tauri::command]
pub fn search(app: S<'_>, query: String, filters: Vec<Filter>) -> CmdResult<SearchResults> {
    app.touch()?;
    app.searcher.interrupt();
    let results = crate::search_wire::run_search(&app.searcher, query, filters)?;
    *app.last_search.lock().expect("last_search mutex") = Some(results.query.clone());
    Ok(results)
}

// ---------------------------------------------------------------------------
// Roots & grid (LIBRARY §5, UI §3)
// ---------------------------------------------------------------------------

fn root_dto(app: &App, root: &photoproof_core::library::RootRecord) -> CmdResult<RootDto> {
    let volume = app.library.volume(&root.volume_id)?;
    let (online, mount) = volume
        .map(|v| (v.online, v.mount_point))
        .unwrap_or((false, None));
    let display_name = root.display_name.clone().unwrap_or_else(|| {
        root.rel_path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("Volume root")
            .to_owned()
    });
    let abs_path = mount.as_deref().map(|m| {
        if root.rel_path.is_empty() {
            m.to_owned()
        } else {
            format!("{}/{}", m.trim_end_matches('/'), root.rel_path)
        }
    });
    Ok(RootDto {
        root_id: root.root_id.clone(),
        display_name,
        rel_path: root.rel_path.clone(),
        volume_id: root.volume_id.clone(),
        online,
        abs_path,
    })
}

#[tauri::command]
pub async fn list_roots(app: S<'_>) -> CmdResult<Vec<RootDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        for r in app.library.roots()? {
            if r.state == "active" {
                out.push(root_dto(&app, &r)?);
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Register a watched root, kick the initial scan (background), start the
/// live watcher. Ingest runs through the pump; the grid populates
/// incrementally (UI §9.1).
#[tauri::command]
pub async fn add_root(app: S<'_>, path: String) -> CmdResult<RootDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let root_id = app
            .library
            .register_root(std::path::Path::new(&path), None)?;
        // Initial scan on its own thread: it only enqueues ingest passes;
        // the pump processes them.
        {
            let lib = app.library.clone();
            let rid = root_id.clone();
            std::thread::Builder::new()
                .name("pp-initial-scan".into())
                .spawn(move || {
                    if let Err(e) = lib.scan_root(&rid, &ScanOptions::default()) {
                        eprintln!("photoproof: initial scan failed for {rid}: {e}");
                    }
                })
                .expect("spawn scan thread");
        }
        match app.library.start_watcher(&root_id) {
            Ok(handle) => {
                app.watchers
                    .lock()
                    .expect("watchers mutex")
                    .insert(root_id.clone(), handle);
            }
            Err(e) => eprintln!(
                "photoproof: watcher failed for {root_id}: {e} (polled rescans still run)"
            ),
        }
        let record = app
            .library
            .root(&root_id)?
            .ok_or_else(|| CmdError::Invalid("root vanished after register".into()))?;
        root_dto(&app, &record)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Remove a watched root (UI §2.4: journals and sidecars untouched; the
/// images leave the index).
#[tauri::command]
pub async fn remove_root(app: S<'_>, root_id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.watchers
            .lock()
            .expect("watchers mutex")
            .remove(&root_id);
        app.library.remove_root(&root_id)?;
        Ok(())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

fn folder_node(n: photoproof_core::library::FolderTreeNode) -> FolderNode {
    FolderNode {
        name: n.name,
        rel_path: n.rel_path,
        children: n.children.into_iter().map(folder_node).collect(),
    }
}

#[tauri::command]
pub async fn folder_tree(app: S<'_>, root_id: String) -> CmdResult<Vec<FolderNode>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let tree = app.library.folder_tree(&root_id)?;
        Ok(tree.into_iter().map(folder_node).collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Grid listing: direct-children images of one folder with badge data
/// (has-journal, rating fold, offline) in one batched read
/// (`Library::list_folder`). Thumbnails load via `photoproof://` URLs the
/// frontend derives from `hash`.
#[tauri::command]
pub async fn list_folder(app: S<'_>, root_id: String, folder: String) -> CmdResult<Vec<GridItem>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let items = app.library.list_folder(&root_id, &folder)?;
        Ok(items
            .into_iter()
            .map(|i| GridItem {
                hash: i.hash.as_str().to_owned(),
                file_name: i.file_name,
                rel_path: i.rel_path,
                capture_ts: i.capture_ts,
                added_ts: i.first_ingested_at,
                has_journal: i.has_journal,
                rating: i.rating,
                offline: i.offline,
            })
            .collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

// ---------------------------------------------------------------------------
// Ingest, settings, export (UI §2.4, §7.5)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn ingest_status(app: S<'_>) -> IngestStatus {
    pump::ingest_status(&app)
}

#[tauri::command]
pub fn settings_get(app: S<'_>) -> AppSettings {
    app.settings.lock().expect("settings mutex").clone()
}

/// RUNTIME contract seam (P6.2). M1: degraded mode — no models, no ASR; the
/// Microphone section stays hidden and Models renders the explainer.
#[tauri::command]
pub fn runtime_status() -> RuntimeStatus {
    RuntimeStatus {
        asr_ready: false,
        hardware_tier: None,
        models: Vec::new(),
    }
}

/// Settings → Export: sidecar set + manifest (SIDECARS §12).
#[tauri::command]
pub async fn export_journal(app: S<'_>, dest: String) -> CmdResult<ExportReportDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let volumes: Vec<VolumeInfo> = app
            .library
            .volumes()?
            .into_iter()
            .map(|v| VolumeInfo {
                label: v.label.clone().unwrap_or_else(|| v.volume_id.clone()),
                id: v.volume_id,
                last_seen: UtcMillis::now().to_rfc3339(),
            })
            .collect();
        let now = UtcMillis::now();
        let report = app.engine.export(
            std::path::Path::new(&dest),
            env!("CARGO_PKG_VERSION"),
            now,
            &volumes,
        )?;
        let ts = now.to_rfc3339();
        {
            let mut s = app.settings.lock().expect("settings mutex");
            s.last_export_ts = Some(ts);
            crate::settings::save(&app.app_data, &s)?;
        }
        Ok(ExportReportDto {
            dir: report.dir.display().to_string(),
            manifest_path: report.manifest_path.display().to_string(),
            images: report.images,
            events: report.events,
            sessions: report.sessions,
        })
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Settings → "Rebuild index from sidecars…" (UI §2.4 maintenance action).
/// In-process reading (flagged): union sidecar truth back into the live
/// store (merge = set-union by event id, K6 — idempotent), then rebuild the
/// derived tables. The full fresh-database restore remains the offline
/// rebuild path (SIDECARS §12.3).
#[tauri::command]
pub async fn rebuild_index(app: S<'_>) -> CmdResult<RebuildReportDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let mut roots = Vec::new();
        for r in app.library.roots()? {
            if r.state != "active" {
                continue;
            }
            if let Some(dto) = root_dto(&app, &r)?.abs_path {
                roots.push(std::path::PathBuf::from(dto));
            }
        }
        let opts = photoproof_core::sidecar::RebuildOptions::live(roots);
        let report =
            photoproof_core::sidecar::rebuild_from_sidecars(&app.engine, &opts, UtcMillis::now())?;
        app.store.rebuild_derived()?;
        Ok(RebuildReportDto {
            files_scanned: report.files_scanned,
            files_parsed: report.files_parsed,
            failures: report.failures.len(),
        })
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

// ---------------------------------------------------------------------------
// Window plumbing
// ---------------------------------------------------------------------------

/// The settings window (UI §2.4): one modest separate window.
#[tauri::command]
pub fn open_settings_window(handle: AppHandle) -> CmdResult<()> {
    if let Some(existing) = handle.get_webview_window("settings") {
        let _ = existing.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("settings.html".into()))
        .title("Settings")
        .inner_size(620.0, 700.0)
        .min_inner_size(480.0, 480.0)
        .decorations(false)
        .background_color(tauri::webview::Color(14, 14, 14, 255))
        .build()
        .map_err(|e| CmdError::Invalid(format!("settings window: {e}")))?;
    Ok(())
}

/// Cmd/Ctrl+Q. Cleanup (session close + sidecar flush) runs in the
/// `ExitRequested` handler in lib.rs, so OS-initiated quits get it too.
#[tauri::command]
pub fn quit(handle: AppHandle) {
    handle.exit(0);
}
