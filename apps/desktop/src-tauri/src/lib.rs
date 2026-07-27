//! Photoproof desktop shell — Tauri 2 + Svelte 5.
//!
//! Contract: spec/UI.md (surfaces, navigation, keyboard map), CAPTURE §3–4
//! (scope, typed notes, ratings) and §2/§9 (session lifecycle), RETRIEVAL
//! §4/§5.4 (the search contract the Search surface renders), DECISIONS K16,
//! I1–I6, C6, P16, P17. Owned by work packet P3.2.
//!
//! This crate is a THIN command layer: business logic lives in
//! photoproof-core; here is wiring, lifetimes, and the custom URI protocol
//! that serves preview bytes without ever touching IPC (P16).

/// Offline backup/verify/restore primitives for the installed-shell handoff
/// and recovery tooling. Never invoke restore while the live App is managed.
pub mod backup;
mod bootstrap;
mod command_work;
mod commands;
mod convergence;
#[cfg(any(feature = "debug-panel", debug_assertions))]
mod debug;
mod diagnostics;
mod disk;
mod doctor;
mod dto;
mod embedders;
mod error;
mod hardware;
pub mod installed_smoke;
pub mod lifecycle;
pub mod managed_tasks;
mod model_registry;
mod performance;
// Native menu bar (desktop-conventions pass): macOS only — Windows/Linux
// run undecorated with custom DOM chrome and need no menu roles (WHY in
// menu.rs); compiling it out is the platform guard.
#[cfg(target_os = "macos")]
mod menu;
mod mic;
mod note;
// onnxruntime runtime-resolution (NVIDIA `cuda-dynamic` build): `pub` so
// `main()` can call `ort_runtime::resolve()` as its first statement, BEFORE any
// thread spawns and before the first ort session is built. A no-op on the
// macOS/CPU builds where the feature is off.
pub mod ort_runtime;
// `pub` so the crate's integration tests (tests/preview_serve_latency.rs)
// can drive `protocol::serve` and the SAME bounded serve pool the
// registration below uses (AUDIT-2026-07-07 F1/T1); nothing else imports it.
pub mod protocol;
mod pump;
mod resource_governor;
mod runtime;
mod scope;
mod search_types;
mod search_wire;
mod session;
mod settings;
mod state;
mod supervisors;
mod updates;

use std::sync::Arc;

use tauri::{Manager, RunEvent};
use tauri_plugin_window_state::StateFlags;

use state::App;

/// Private child-process entry point for bounded hardware/ORT discovery.
/// `main` dispatches here before Tauri/GTK initialization when the parent
/// supplies the internal helper flag.
pub fn run_capability_probe_helper() -> i32 {
    match std::panic::catch_unwind(hardware::LiveProbe::probe_for_helper) {
        Ok(report) => match serde_json::to_writer(std::io::stdout(), &report) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("serialize capability report: {error}");
                1
            }
        },
        Err(_) => {
            eprintln!("native capability probe panicked");
            1
        }
    }
}

/// Structured logging: core/connectors emit `tracing` spans/events and the
/// shell is the ONE place a subscriber installs. Two sinks share one filter
/// (RUST_LOG overrides; default keeps release consoles quiet at `info` while
/// photoproof's own drains/passes show at `debug` — set RUST_LOG=trace for
/// the firehose):
///   - the console (stdout), as before, for `cargo tauri dev`;
///   - a fresh active file at `<app_data>/logs/photoproof.log`; the prior
///     launch is rotated and retained before this one opens, so relaunch never
///     destroys crash evidence. Best-effort: a logs dir that will not open
///     leaves the console sink alone.
///
/// Installed from `.setup()` (not the top of `run()`) because the path comes
/// from Tauri's resolver — `App::init`'s heavy startup still logs into it,
/// since init runs after this.
fn install_logging(
    app_data: &std::path::Path,
) -> (Option<diagnostics::CrashDiagnostics>, Option<String>) {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,photoproof_core=debug,photoproof_desktop=debug".into());
    let console = tracing_subscriber::fmt::layer().compact();

    let (prepared, diagnostics_error) = match diagnostics::prepare(app_data) {
        Ok(prepared) => (Some(prepared), None),
        Err(error) => (None, Some(error.to_string())),
    };
    if let Some(prepared) = &prepared {
        diagnostics::install_panic_recording(&prepared.diagnostics.logs_dir);
    }
    let file_layer = prepared.as_ref().and_then(|prepared| {
        prepared.log_file.try_clone().ok().map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false) // no terminal escapes in a file
                .with_writer(std::sync::Mutex::new(file))
        })
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file_layer) // Option<Layer>: None = console-only, no-op
        .init();
    (
        prepared.map(|prepared| prepared.diagnostics),
        diagnostics_error,
    )
}

pub fn run() {
    let builder = tauri::Builder::default()
        // §8.5 single-instance discipline: a second launch never reaches
        // the supervisor — it forwards focus to the first instance and
        // exits. Registered FIRST (plugin guidance); the core's
        // instance.lock is the belt-and-braces half supervisors check.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.unminimize();
                let _ = main.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        // Signature verification is mandatory inside Tauri's updater. The
        // plugin is present in every build so the IPC contract is stable, but
        // updates.rs refuses all network/install work unless the production
        // release build explicitly enables it and merges a real HTTPS endpoint
        // plus matching public key.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Window geometry persisted (featureset §6). The settings window is
        // denylisted: it stays the one modest window (UI §2.4). Known risk
        // (UI-ARCHITECTURE Appendix B): restore drift with custom titlebars
        // on some Wayland compositors — named in DOGFOOD §visual; the
        // fallback, if it misbehaves, is manual save/restore in
        // commands/app.rs keyed off the same RunEvent hooks.
        //
        // NORMAL GEOMETRY ONLY: remember the last non-maximized size and
        // position, but never restore a transient presentation state.
        //
        // Four default flags are deliberately excluded:
        // - DECORATIONS: decoration state is owned per-platform by
        //   config/code (macOS keeps native Overlay chrome, Windows/Linux
        //   run undecorated) and is toggled live by Tab lights-out. Any
        //   macOS machine that ran the pre-Overlay builds has
        //   decorated:false persisted for "main"; restoring it would strip
        //   the traffic lights — leaving ZERO window controls — and the
        //   exit-time save would make that sticky forever.
        // - FULLSCREEN: shell.svelte.ts owns fullscreen as frontend state
        //   (starts false); a disk-restored fullscreen window would desync
        //   the F toggle on first press.
        // - MAXIMIZED: restoring it made every later launch look fullscreen
        //   after one maximized close. The plugin still preserves the last
        //   normal SIZE/POSITION while maximized, so opening is predictable
        //   without forgetting the user's chosen geometry.
        // - VISIBLE: startup visibility is application lifecycle state, not
        //   user window geometry.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION)
                .with_denylist(&["settings"])
                .build(),
        )
        // Thumbnails + Look images: photoproof://localhost/{thumb|display}/{hash}
        // straight from the preview cache, plus the progressive full-
        // resolution ladder Look climbs past 1:1 — /original/{hash}
        // (webview-decodable stored formats) and /embedded/{hash} (the
        // RAW's native-size embedded JPEG, extracted on demand); protocol.rs
        // owns the allowlists. Bytes never cross IPC, never base64 (UI
        // §3.3, DECISIONS P16).
        .register_asynchronous_uri_scheme_protocol("photoproof", |ctx, request, responder| {
            let path = request.uri().path().to_owned();
            let library = ctx
                .app_handle()
                .try_state::<Arc<App>>()
                .map(|s| s.library.clone());
            let performance = ctx
                .app_handle()
                .try_state::<Arc<performance::PerformanceMonitor>>()
                .map(|monitor| Arc::clone(monitor.inner()));
            // Fixed workers plus a bounded, route-priority queue: visible
            // Look/display requests pass queued grid thumbnails, and a fling
            // supersedes its oldest still-queued thumbnails instead of
            // growing work without limit. The no-library window (before
            // setup manages App) still answers 404 through the same path.
            let priority = protocol::priority_for_path(&path);
            protocol::serve_pool().run(priority, move |disposition| {
                if disposition == protocol::ServeDisposition::Overloaded {
                    responder.respond(protocol::respond_overloaded());
                    return;
                }
                let started = std::time::Instant::now();
                let response = match library {
                    Some(lib) => protocol::serve(&lib, &path),
                    None => protocol::respond_not_found(),
                };
                if let Some(performance) = performance {
                    let ok = response.status().is_success();
                    let _ = performance.record_backend_with_context(
                        performance::Journey::Preview,
                        performance::Phase::Serve,
                        started.elapsed().as_secs_f64() * 1_000.0,
                        ok,
                        Some(1),
                        Some(response.body().len() as u64),
                        protocol::backend_cache_status(&path, response.status()),
                    );
                }
                responder.respond(response);
            });
        })
        .setup(|app| {
            // Menu bar before anything shows: App/File/Edit/View/Window
            // with standard roles + registry-id custom items (menu.rs).
            #[cfg(target_os = "macos")]
            menu::install(app)?;

            let app_data = app.path().app_data_dir()?;
            let bootstrap = Arc::new(bootstrap::BootstrapState::default());
            let performance =
                Arc::new(performance::PerformanceMonitor::app_data_default(&app_data));
            app.manage(Arc::clone(&bootstrap));
            app.manage(Arc::clone(&performance));
            app.manage(Arc::new(updates::UpdateCoordinator::default()));
            // Logging first, so App::init's startup (DB open, migrations,
            // supervisor plan) lands in this launch's fresh log file.
            let (diagnostics, diagnostics_error) = install_logging(&app_data);
            let init_started = std::time::Instant::now();
            let state = match App::init_with_diagnostics(app_data, diagnostics, diagnostics_error) {
                Ok(state) => {
                    let _ = performance.record_backend(
                        performance::Journey::LibraryOpen,
                        performance::Phase::Total,
                        init_started.elapsed().as_secs_f64() * 1_000.0,
                        true,
                    );
                    Arc::new(state)
                }
                Err(error) => {
                    let _ = performance.record_backend(
                        performance::Journey::LibraryOpen,
                        performance::Phase::Total,
                        init_started.elapsed().as_secs_f64() * 1_000.0,
                        false,
                    );
                    tracing::error!(%error, "application data could not be opened");
                    let message = error.to_string();
                    let recovery = message
                        .starts_with("device identity unavailable:")
                        .then_some("reset-device-identity");
                    bootstrap.fatal(message, recovery);
                    // Keep Tauri and the configured window alive. The minimal
                    // bootstrap command remains available even though Arc<App>
                    // is intentionally not managed after a fatal open.
                    return Ok(());
                }
            };
            // Publish state before starting any owned background task so
            // commands/protocol requests see one coherent App immediately.
            app.manage(Arc::clone(&state));
            bootstrap.ready();
            if let Err(error) = state.start_supervisor_runtime() {
                tracing::error!(%error, "owned supervisor ticker unavailable");
            }
            if let Err(error) = state.start_runtime_capability_detection() {
                tracing::error!(%error, "managed hardware capability detection unavailable");
            }
            if let Err(error) = state.start_model_registry_recovery() {
                tracing::error!(%error, "managed model registry recovery unavailable");
            }
            if let Err(error) = state.start_plan_convergence() {
                tracing::error!(%error, "managed plan convergence unavailable");
            }
            if let Err(error) = state.start_live_control_watcher(app.handle().clone()) {
                tracing::error!(%error, "managed live control watcher unavailable");
            }
            if let Err(error) = state.start_capture_runtime() {
                tracing::error!(%error, "managed capture initialization unavailable");
            }

            // Volume discovery and watcher restoration can block on slow or
            // newly-unresponsive mounts, so they begin after state is usable
            // under the managed task registry instead of delaying setup.
            if let Err(error) = state.start_startup_watchers() {
                tracing::error!(%error, "managed startup watcher restore unavailable");
            }

            // Unified startup doctor (STATE-INTEGRITY-AUDIT): one ordered,
            // logged disk-vs-DB integrity sweep. On a background thread so a
            // large library's preview existence walk never blocks the window
            // from showing; it only touches DERIVED state (vector spaces +
            // preview cache) and re-pends what must rebuild.
            if let Err(error) = state.start_startup_doctor() {
                tracing::error!(%error, "managed startup doctor unavailable");
            }

            if let Err(error) = pump::spawn_ingest_pump(&state, app.handle().clone()) {
                tracing::error!(%error, "managed ingest pump unavailable");
            }
            if let Err(error) = pump::spawn_preview_pump(&state, app.handle().clone()) {
                tracing::error!(%error, "managed preview pump unavailable");
            }
            if let Err(error) = pump::spawn_raw_decode_pump(&state, app.handle().clone()) {
                tracing::error!(%error, "managed interactive RAW pump unavailable");
            }
            if let Err(error) = pump::spawn_embedding_pump(&state) {
                tracing::error!(%error, "managed embedding pump unavailable");
            }
            if let Err(error) = pump::spawn_volume_monitor(&state) {
                tracing::error!(%error, "managed volume monitor unavailable");
            }
            if let Err(error) = pump::spawn_maintenance_pump(&state) {
                tracing::error!(%error, "managed maintenance pump unavailable");
            }
            if let Err(error) = pump::spawn_sidecar_pump(&state) {
                tracing::error!(%error, "managed sidecar pump unavailable");
            }
            if let Err(error) = pump::spawn_runtime_pump(&state, app.handle().clone()) {
                tracing::error!(%error, "managed runtime pump unavailable");
            }
            Ok(())
        });

    let builder = builder.invoke_handler(handlers());

    builder
        .build(tauri::generate_context!())
        .expect("error while building Photoproof")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                // CAPTURE §2.5 (M1 slice): close the session, flush sidecars.
                if let Some(state) = app_handle.try_state::<Arc<App>>() {
                    let started = std::time::Instant::now();
                    state.shutdown();
                    if let Some(performance) =
                        app_handle.try_state::<Arc<performance::PerformanceMonitor>>()
                    {
                        let _ = performance.record_backend(
                            performance::Journey::Shutdown,
                            performance::Phase::Total,
                            started.elapsed().as_secs_f64() * 1_000.0,
                            true,
                        );
                    }
                }
            }
        });
}

// OS-launcher verbs stay on the os.rs xdg-open-class spawns (Stage A) —
// tauri-plugin-opener was deliberately NOT adopted: the command surface is
// implemented and tested without it (deviation recorded in DECISIONS).
#[cfg(not(any(feature = "debug-panel", debug_assertions)))]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        bootstrap::bootstrap_status,
        bootstrap::bootstrap_relaunch,
        bootstrap::bootstrap_reset_device_identity,
        commands::backup::backup_operation_status,
        commands::backup::backup_and_quit,
        commands::backup::restore_and_restart,
        updates::update_status,
        updates::update_check,
        updates::update_install,
        commands::capture::set_scope,
        commands::capture::indicator_state,
        commands::capture::add_note,
        commands::capture::set_rating,
        commands::capture::report_activity,
        commands::capture::add_stroke,
        commands::capture::toggle_mic,
        commands::capture::set_mic,
        commands::search::search,
        commands::search::find_similar,
        // Tier-1 near-duplicate detection (DESIGN-DEDUP-AND-SIMILARITY.md).
        commands::dedup::find_near_duplicates,
        // Semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md).
        commands::graph::topic_affinities,
        commands::graph::suggest_topics,
        commands::graph::cluster_topics,
        commands::graph::graph_neighbors,
        commands::graph::suggest_topics_llm,
        commands::graph::graph_tuning,
        // Diversify / duplication-tolerance view filter (DESIGN-DEDUP-AND-SIMILARITY.md).
        commands::diversify::diversify_scope,
        // Topics + autosuggest + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md).
        commands::topics::add_topic,
        commands::topics::list_topics,
        commands::topics::remove_topic,
        commands::topics::add_topic_note,
        commands::topics::topic_notes,
        commands::topics::topic_ranked_images,
        commands::topics::create_collection_from_topic,
        commands::topics::create_collection_from_selection,
        commands::topics::suggest_collections,
        commands::collections::list_collections,
        commands::collections::create_collection,
        commands::collections::rename_collection,
        commands::collections::set_collection_status,
        commands::collections::add_to_collection,
        commands::collections::remove_from_collection,
        commands::collections::collections_for_image,
        commands::collections::set_collection_description,
        commands::collections::add_collection_note,
        commands::collections::collection_notes,
        commands::collections::list_collection_members,
        commands::library::list_roots,
        commands::library::add_root,
        commands::library::remove_root,
        commands::library::archive_root,
        commands::library::unarchive_root,
        commands::library::list_archived_roots,
        commands::library::rescan_root,
        commands::library::recover_roots,
        commands::library::rebuild_previews,
        commands::library::request_full_decode,
        commands::library::prioritize_previews,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::list_folder_delta,
        commands::library::list_images,
        commands::library::ingest_status,
        commands::health::application_health,
        commands::health::retry_integrity_repair,
        commands::convergence::application_state_snapshot,
        commands::performance::performance_ingest,
        commands::performance::performance_snapshot,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::set_external_editor,
        commands::app::set_processing_policy,
        commands::app::restore_control_defaults,
        commands::app::set_preview_cache_budget,
        commands::app::preview_cache_stats,
        commands::app::clear_preview_cache,
        commands::app::runtime_status,
        commands::app::runtime_consent,
        commands::app::runtime_accept_license,
        commands::app::runtime_download_model,
        commands::app::runtime_select_model,
        commands::app::runtime_remove_model,
        commands::app::runtime_verify_model,
        commands::app::runtime_discard_partial,
        commands::app::runtime_restart,
        commands::app::runtime_redetect,
        runtime::runtime_cancel_download,
        commands::app::export_journal,
        commands::app::import_topics,
        commands::app::rebuild_index,
        commands::app::force_reembed,
        commands::app::open_settings_window,
        commands::app::set_traffic_lights_hidden,
        commands::app::quit,
        commands::journal::image_journal,
        commands::journal::image_metadata,
        commands::journal::revise_event,
        commands::journal::retract_event,
        commands::journal::unretract_event,
        commands::journal::redact_event,
        commands::os::image_abs_path,
        commands::os::reveal_in_file_manager,
        commands::os::reveal_folder,
        commands::os::reveal_logs,
        commands::os::open_with_default,
        commands::os::open_in_external_editor,
        // Attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md).
        commands::heatmap::record_dwell,
        commands::heatmap::image_intensity,
        commands::heatmap::clear_dwell,
    ]
}

/// Dev builds additionally register the debug-panel commands (UI §10.1:
/// they do not exist in release binaries; invoking one there fails as
/// unknown — asserted by scripts/assert-release-clean.sh).
#[cfg(any(feature = "debug-panel", debug_assertions))]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        bootstrap::bootstrap_status,
        bootstrap::bootstrap_relaunch,
        bootstrap::bootstrap_reset_device_identity,
        commands::backup::backup_operation_status,
        commands::backup::backup_and_quit,
        commands::backup::restore_and_restart,
        updates::update_status,
        updates::update_check,
        updates::update_install,
        commands::capture::set_scope,
        commands::capture::indicator_state,
        commands::capture::add_note,
        commands::capture::set_rating,
        commands::capture::report_activity,
        commands::capture::add_stroke,
        commands::capture::toggle_mic,
        commands::capture::set_mic,
        commands::search::search,
        commands::search::find_similar,
        // Tier-1 near-duplicate detection (DESIGN-DEDUP-AND-SIMILARITY.md).
        commands::dedup::find_near_duplicates,
        // Semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md).
        commands::graph::topic_affinities,
        commands::graph::suggest_topics,
        commands::graph::cluster_topics,
        commands::graph::graph_neighbors,
        commands::graph::suggest_topics_llm,
        commands::graph::graph_tuning,
        // Diversify / duplication-tolerance view filter (DESIGN-DEDUP-AND-SIMILARITY.md).
        commands::diversify::diversify_scope,
        // Topics + autosuggest + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md).
        commands::topics::add_topic,
        commands::topics::list_topics,
        commands::topics::remove_topic,
        commands::topics::add_topic_note,
        commands::topics::topic_notes,
        commands::topics::topic_ranked_images,
        commands::topics::create_collection_from_topic,
        commands::topics::create_collection_from_selection,
        commands::topics::suggest_collections,
        commands::collections::list_collections,
        commands::collections::create_collection,
        commands::collections::rename_collection,
        commands::collections::set_collection_status,
        commands::collections::add_to_collection,
        commands::collections::remove_from_collection,
        commands::collections::collections_for_image,
        commands::collections::set_collection_description,
        commands::collections::add_collection_note,
        commands::collections::collection_notes,
        commands::collections::list_collection_members,
        commands::library::list_roots,
        commands::library::add_root,
        commands::library::remove_root,
        commands::library::archive_root,
        commands::library::unarchive_root,
        commands::library::list_archived_roots,
        commands::library::rescan_root,
        commands::library::recover_roots,
        commands::library::rebuild_previews,
        commands::library::request_full_decode,
        commands::library::prioritize_previews,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::list_folder_delta,
        commands::library::list_images,
        commands::library::ingest_status,
        commands::health::application_health,
        commands::health::retry_integrity_repair,
        commands::convergence::application_state_snapshot,
        commands::performance::performance_ingest,
        commands::performance::performance_snapshot,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::set_external_editor,
        commands::app::set_processing_policy,
        commands::app::restore_control_defaults,
        commands::app::set_preview_cache_budget,
        commands::app::preview_cache_stats,
        commands::app::clear_preview_cache,
        commands::app::runtime_status,
        commands::app::runtime_consent,
        commands::app::runtime_accept_license,
        commands::app::runtime_download_model,
        commands::app::runtime_select_model,
        commands::app::runtime_remove_model,
        commands::app::runtime_verify_model,
        commands::app::runtime_discard_partial,
        commands::app::runtime_restart,
        commands::app::runtime_redetect,
        runtime::runtime_cancel_download,
        commands::app::export_journal,
        commands::app::import_topics,
        commands::app::rebuild_index,
        commands::app::force_reembed,
        commands::app::open_settings_window,
        commands::app::set_traffic_lights_hidden,
        commands::app::quit,
        commands::journal::image_journal,
        commands::journal::image_metadata,
        commands::journal::revise_event,
        commands::journal::retract_event,
        commands::journal::unretract_event,
        commands::journal::redact_event,
        commands::os::image_abs_path,
        commands::os::reveal_in_file_manager,
        commands::os::reveal_folder,
        commands::os::reveal_logs,
        commands::os::open_with_default,
        commands::os::open_in_external_editor,
        // Attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md).
        commands::heatmap::record_dwell,
        commands::heatmap::image_intensity,
        commands::heatmap::clear_dwell,
        debug::debug_tail_events,
        debug::debug_capture,
        debug::debug_ingest,
        debug::debug_sidecars,
        debug::debug_search,
        debug::debug_runtime,
        debug::debug_force_flush,
        debug::debug_force_rescan,
        debug::debug_doctor,
    ]
}
