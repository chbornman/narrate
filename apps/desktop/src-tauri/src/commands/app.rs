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

/// Settings → "Open in external editor" target (BACKLOG "Configurable
/// external editor, D4 revisit"). Trim the input and treat empty/whitespace
/// as None — clearing the pref back to the OS default handler, so the single
/// menu seat always does something sensible. Persists in settings.json and
/// emits `settings-changed` (mirroring set_stack_display) so the Settings
/// window's edit reaches the main window's command path live. localStorage
/// is webview-local, so this preference needs the shared Rust store.
#[tauri::command]
pub fn set_external_editor(
    app: S<'_>,
    handle: AppHandle,
    editor: String,
) -> CmdResult<AppSettings> {
    let trimmed = editor.trim();
    let next = {
        let mut s = app.settings.lock().expect("settings mutex");
        s.external_editor = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        };
        crate::settings::save(&app.app_data, &s)?;
        s.clone()
    };
    let _ = handle.emit("settings-changed", next.clone());
    Ok(next)
}

/// Settings → Previews: set the 1:1 preview cache budget in BYTES
/// (DESIGN-PREVIEW-POLICY.md). Persists in settings.json and immediately runs
/// one eviction pass so a LOWERED budget takes effect now (trims the cache
/// under the new cap) rather than waiting for the next develop. Returns the
/// updated settings (the UI re-reads the persisted value). No `settings-changed`
/// emit: the budget is consumed only by the backend evictor, not by any live
/// main-window view.
#[tauri::command]
pub async fn set_preview_cache_budget(app: S<'_>, bytes: u64) -> CmdResult<AppSettings> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let next = {
            let mut s = app.settings.lock().expect("settings mutex");
            s.preview_cache_budget_bytes = bytes;
            crate::settings::save(&app.app_data, &s)?;
            s.clone()
        };
        // Apply the new cap immediately: lowering the budget should reclaim
        // disk now, not on the next view-time develop.
        app.library
            .evict_preview_cache(next.preview_cache_budget_bytes);
        Ok(next)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Settings → Previews cache-size readout (DESIGN-PREVIEW-POLICY.md): current
/// 1:1 cache size + file count, the total previews footprint, and the
/// configured budget (so the UI shows "X of Y" without a second call). Cheap
/// (one stat pass over `previews/`).
#[tauri::command]
pub async fn preview_cache_stats(app: S<'_>) -> CmdResult<crate::dto::PreviewCacheStatsDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let stats = app.library.full_cache_stats();
        let budget = app
            .settings
            .lock()
            .expect("settings mutex")
            .preview_cache_budget_bytes;
        Ok(crate::dto::PreviewCacheStatsDto {
            full_bytes: stats.full_bytes,
            full_files: stats.full_files,
            total_bytes: stats.total_bytes,
            budget_bytes: budget,
        })
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Settings → Previews "Clear 1:1 cache" / "Rebuild all previews"
/// (DESIGN-PREVIEW-POLICY.md). `kind` = `"full"` (just the 1:1 tier) | `"all"`
/// (1:1 + display + thumb; this one ALSO re-pends the preview pass for every
/// active root so the grid regenerates). SAFE — every removed artifact
/// re-derives on next view; strokes live in vector coords, never in an
/// artifact. Returns the number of files removed. An unknown `kind` is rejected
/// so a typo cannot silently nuke the whole cache.
///
/// After the sweep we emit a GLOBAL `previews-changed` (empty `hashes`). WHY:
/// the `photoproof://` protocol serves content-addressed artifacts with an
/// `immutable` cache header, so after the bytes are deleted on disk the webview
/// keeps serving its CACHED copy for the same stable URL until a restart. The
/// only live cache-bust is the `?p=<seq>` query param the grid/Look bump on
/// `previews-changed`. A hash-less ping means "bump EVERY visible thumb": a
/// `Full` clear makes any open Look re-request (and re-develop on-demand); an
/// `All` clear makes the grid immediately show truthful "?" then heal per hash
/// as each regenerated artifact lands (founder dogfood, June 2026).
#[tauri::command]
pub async fn clear_preview_cache(app: S<'_>, handle: AppHandle, kind: String) -> CmdResult<u64> {
    let app = app.inner().clone();
    let removed = tauri::async_runtime::spawn_blocking(move || {
        let kind = photoproof_core::library::ClearKind::parse(&kind)
            .ok_or_else(|| CmdError::Invalid(format!("unknown clear kind: {kind}")))?;
        Ok::<u64, CmdError>(app.library.clear_preview_cache_kind(kind)?)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))??;
    // Empty `hashes` = the global "bump every thumb" signal (see doc note).
    let _ = handle.emit(
        "previews-changed",
        crate::dto::PreviewsChanged { hashes: vec![] },
    );
    Ok(removed)
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

/// Settings → "Force re-embed (swapped weights)" maintenance action — the Seam 2
/// TAIL (`docs/ARCHITECTURE-CONTRACTS.md` rollout step 4). The automatic
/// model-aware re-pend keys off `model_id`, so replacing the WEIGHTS behind a
/// model while REUSING its id re-embeds nothing; this is the explicit escape
/// hatch for that case. It force re-pends BOTH embed passes (image + text) into
/// the active space(s) UNCONDITIONALLY; the pump drains them on its next tick,
/// exactly like `rebuild_previews`. Re-pending a pass whose embedder is not
/// configured just leaves those rows pending (NotConfigured-style, harmless),
/// so the command stays simple and does not probe which embedders are live.
/// Returns the total rows re-pended across both passes.
///
/// Kept SEPARATE from `rebuild_index` (sidecar union) and from the automatic
/// staleness path on purpose: this is the only thing that can trigger a full
/// re-embed under an unchanged id, so it must be an explicit, founder-invoked
/// verb — never something the staleness logic could fire by surprise.
#[tauri::command]
pub async fn force_reembed(app: S<'_>) -> CmdResult<usize> {
    use photoproof_core::library::PassName;
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        // Both embed passes: a weights swap can hit the CLIP and/or the text
        // model, and we cannot tell which from here — force both, let the
        // unconfigured one sit pending.
        let mut repended = app.library.force_repend_pass(PassName::ImageEmbedding)?;
        repended += app.library.force_repend_pass(PassName::TextEmbedding)?;
        Ok(repended)
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
