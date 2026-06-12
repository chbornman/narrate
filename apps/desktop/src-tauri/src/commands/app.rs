//! App-level commands: settings, runtime status, export/rebuild, window
//! plumbing — moved verbatim from the old commands.rs (FOUNDATIONS split).

use photoproof_core::UtcMillis;
use photoproof_core::sidecar::VolumeInfo;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::S;
use super::library::root_dto;
use crate::dto::{ExportReportDto, RebuildReportDto, RuntimeStatus};
use crate::error::{CmdError, CmdResult};
use crate::settings::{AppSettings, StackDisplay};

/// Initial webview background: must mirror the frontend theme's dark
/// background (`--bg: #0e0e0e` in lib/theme/tokens.css, and the main
/// window's `backgroundColor` in tauri.conf.json) — it suppresses the
/// white flash before the webview paints. If the Svelte theme color
/// changes, this changes with it.
const WINDOW_BACKGROUND: tauri::webview::Color = tauri::webview::Color(14, 14, 14, 255);

#[tauri::command]
pub fn settings_get(app: S<'_>) -> AppSettings {
    app.settings.lock().expect("settings mutex").clone()
}

/// Settings → Watched folders: "Stacked pairs show: JPEG (default) | RAW"
/// (featureset §5 dogfood amendment). Persists in settings.json and emits
/// `settings-changed` to every window so the main grid re-pairs LIVE —
/// localStorage is webview-local, so the Settings window cannot carry this
/// preference itself.
#[tauri::command]
pub fn set_stack_display(
    app: S<'_>,
    handle: AppHandle,
    display: StackDisplay,
) -> CmdResult<AppSettings> {
    let next = {
        let mut s = app.settings.lock().expect("settings mutex");
        s.stack_display = display;
        crate::settings::save(&app.app_data, &s)?;
        s.clone()
    };
    let _ = handle.emit("settings-changed", next.clone());
    Ok(next)
}

/// The RUNTIME contract (P6.2): tier, consent, per-model rows with
/// license + progress, readiness gates. Settings renders the Models
/// section from this; the one-time consent card reads the same snapshot.
#[tauri::command]
pub fn runtime_status(app: S<'_>) -> RuntimeStatus {
    app.runtime.status()
}

/// §10.2–10.3: the one consent decision — "download" | "later" | "never".
/// No download starts without this; Never is remembered; Later re-offers
/// from settings only. Skipping changes nothing about journaling.
#[tauri::command]
pub fn runtime_consent(app: S<'_>, handle: AppHandle, decision: String) -> RuntimeStatus {
    app.runtime.set_consent(&decision);
    let status = app.runtime.status();
    let _ = handle.emit("runtime-status", status.clone());
    status
}

/// §5.3: record one model's license acceptance (model id, license url,
/// timestamp — persisted in app data; texts stay viewable in settings).
#[tauri::command]
pub fn runtime_accept_license(app: S<'_>, model_id: String) -> CmdResult<RuntimeStatus> {
    app.runtime
        .accept_license(&model_id)
        .map_err(CmdError::Invalid)?;
    Ok(app.runtime.status())
}

/// Settings → download one model now (consent + license gates apply in
/// the manager — zero bytes move for an unaccepted gated model, §13.7).
#[tauri::command]
pub fn runtime_download_model(app: S<'_>, model_id: String) -> CmdResult<RuntimeStatus> {
    app.runtime
        .download_model(&model_id)
        .map_err(CmdError::Invalid)?;
    Ok(app.runtime.status())
}

/// Settings → remove a model's weights.
#[tauri::command]
pub fn runtime_remove_model(app: S<'_>, model_id: String) -> CmdResult<RuntimeStatus> {
    app.runtime
        .remove_model(&model_id)
        .map_err(CmdError::Invalid)?;
    Ok(app.runtime.status())
}

/// Settings → "restart runtime" (§8.1: Failed re-enters Spawning with a
/// fresh budget; surfaced download failures clear for retry).
#[tauri::command]
pub fn runtime_restart(app: S<'_>) -> RuntimeStatus {
    app.runtime.restart_runtime();
    app.runtime.status()
}

/// Settings → re-detect hardware (§6.1.4: cached + re-detect on demand).
#[tauri::command]
pub fn runtime_redetect(app: S<'_>) -> RuntimeStatus {
    app.runtime.redetect_tier()
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
        // RETRIEVAL §10.2: collections.photoproof.json travels beside the
        // sidecar set + manifest — "you can walk away with everything"
        // includes your collections. When a newer-version file owns the
        // app-data path this ERRORS (propagated to the user) instead of
        // silently exporting a database-derived subset that omits the
        // newer file's collections.
        app.collections
            .export_to(std::path::Path::new(&dest), now)?;
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

/// The settings window (UI §2.4): one modest separate window.
#[tauri::command]
pub fn open_settings_window(handle: AppHandle) -> CmdResult<()> {
    if let Some(existing) = handle.get_webview_window("settings") {
        let _ = existing.set_focus();
        return Ok(());
    }
    let builder =
        WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("settings.html".into()))
            .title("Settings")
            .inner_size(620.0, 700.0)
            .min_inner_size(480.0, 480.0)
            .background_color(WINDOW_BACKGROUND);
    // Platform chrome (UI §2.3), mirroring tauri.macos.conf.json for the
    // main window: macOS keeps native decorations — rounded corners,
    // shadow, traffic lights overlaying the drag strip (SettingsApp.svelte
    // insets past them and drops its custom close button); Windows/Linux
    // stay undecorated with the custom strip.
    #[cfg(target_os = "macos")]
    let builder = builder
        .decorations(true)
        .hidden_title(true)
        .title_bar_style(tauri::TitleBarStyle::Overlay);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(false);
    builder
        .build()
        .map_err(|e| CmdError::Invalid(format!("settings window: {e}")))?;
    Ok(())
}

/// Tab lights-out, the native half (featureset §0: "hides ALL chrome").
/// With macOS's Overlay titlebar the traffic lights are native NSButtons,
/// not DOM — App.svelte's region gates cannot touch them, and left visible
/// they float over (and click-block) the chrome-less grid's top-left
/// corner. WHY standardWindowButton:setHidden: and not set_decorations:
/// tao rebuilds the style mask from scratch on re-decorate, dropping
/// FullSizeContentView and wrecking the Overlay layout — hiding the
/// buttons is lossless and exactly reversible. Nothing persists: lib.rs
/// strips DECORATIONS from the window-state flags, and this hidden bit
/// lives only on the live NSWindow. No-op off macOS (the custom DOM
/// controls there are gated in Svelte).
#[tauri::command]
pub fn set_traffic_lights_hidden(window: tauri::WebviewWindow, hidden: bool) -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    {
        // AppKit is main-thread-only; the command may arrive off it.
        let win = window.clone();
        window
            .run_on_main_thread(move || {
                use objc2::msg_send;
                use objc2::runtime::AnyObject;
                let Ok(ns_window) = win.ns_window() else {
                    return;
                };
                let ns_window = ns_window.cast::<AnyObject>();
                // NSWindowButton: close = 0, miniaturize = 1, zoom = 2.
                for kind in 0_usize..=2 {
                    unsafe {
                        let button: *mut AnyObject =
                            msg_send![ns_window, standardWindowButton: kind];
                        if !button.is_null() {
                            let _: () = msg_send![button, setHidden: hidden];
                        }
                    }
                }
            })
            .map_err(|e| CmdError::Invalid(format!("traffic lights: {e}")))?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, hidden);
    }
    Ok(())
}

/// Cmd/Ctrl+Q. Cleanup (session close + sidecar flush) runs in the
/// `ExitRequested` handler in lib.rs, so OS-initiated quits get it too.
#[tauri::command]
pub fn quit(handle: AppHandle) {
    handle.exit(0);
}
