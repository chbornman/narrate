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

mod commands;
#[cfg(any(feature = "debug-panel", debug_assertions))]
mod debug;
mod dto;
mod embedders;
mod error;
mod hardware;
// Native menu bar (desktop-conventions pass): macOS only — Windows/Linux
// run undecorated with custom DOM chrome and need no menu roles (WHY in
// menu.rs); compiling it out is the platform guard.
#[cfg(target_os = "macos")]
mod menu;
mod mic;
mod note;
mod protocol;
mod pump;
mod runtime;
mod scope;
mod search_types;
mod search_wire;
mod session;
mod settings;
mod state;
mod supervisors;

use std::sync::Arc;

use tauri::{Manager, RunEvent};
use tauri_plugin_window_state::StateFlags;

use state::App;

/// Structured logging: core/connectors emit `tracing` spans/events and the
/// shell is the ONE place a subscriber installs. Two sinks share one filter
/// (RUST_LOG overrides; default keeps release consoles quiet at `info` while
/// photoproof's own drains/passes show at `debug` — set RUST_LOG=trace for
/// the firehose):
///   - the console (stdout), as before, for `cargo tauri dev`;
///   - a FRESH file at `<app_data>/logs/photoproof.log`, truncated on every
///     launch (founder, June 2026: "a new log file whenever we run tauri
///     dev"). Each Rust process start = one clean session log, so a jank is
///     reviewable after the fact without console scrollback. Best-effort: a
///     logs dir that won't open leaves the console sink alone.
///
/// Installed from `.setup()` (not the top of `run()`) because the path comes
/// from Tauri's resolver — `App::init`'s heavy startup still logs into it,
/// since init runs after this.
fn install_logging(app_data: &std::path::Path) {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,photoproof_core=debug,photoproof_desktop=debug".into());
    let console = tracing_subscriber::fmt::layer().compact();

    // File::create truncates-or-creates: the "new file each launch" the
    // founder asked for falls out of opening it fresh per process.
    let file_layer = std::fs::create_dir_all(app_data.join("logs"))
        .and_then(|()| std::fs::File::create(app_data.join("logs").join("photoproof.log")))
        .ok()
        .map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false) // no terminal escapes in a file
                .with_writer(std::sync::Mutex::new(file))
        });

    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file_layer) // Option<Layer>: None = console-only, no-op
        .init();
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
        // Window geometry persisted (featureset §6). The settings window is
        // denylisted: it stays the one modest window (UI §2.4). Known risk
        // (UI-ARCHITECTURE Appendix B): restore drift with custom titlebars
        // on some Wayland compositors — named in DOGFOOD §visual; the
        // fallback, if it misbehaves, is manual save/restore in
        // commands/app.rs keyed off the same RunEvent hooks.
        //
        // GEOMETRY ONLY — two flags are deliberately stripped from the
        // default StateFlags::all():
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
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    StateFlags::all() & !(StateFlags::DECORATIONS | StateFlags::FULLSCREEN),
                )
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
            std::thread::spawn(move || {
                let response = match library {
                    Some(lib) => protocol::serve(&lib, &path),
                    None => protocol::respond_not_found(),
                };
                responder.respond(response);
            });
        })
        .setup(|app| {
            // Menu bar before anything shows: App/File/Edit/View/Window
            // with standard roles + registry-id custom items (menu.rs).
            #[cfg(target_os = "macos")]
            menu::install(app)?;

            let app_data = app.path().app_data_dir()?;
            // Logging first, so App::init's startup (DB open, migrations,
            // supervisor plan) lands in this launch's fresh log file.
            install_logging(&app_data);
            let state = Arc::new(App::init(app_data)?);

            // Restart watchers for every active root on online volumes.
            {
                let _ = state.library.probe_volumes();
                let roots = state.library.roots().unwrap_or_default();
                let mut watchers = state.watchers.lock().expect("watchers mutex");
                for r in roots.iter().filter(|r| r.state == "active") {
                    match state.library.start_watcher(&r.root_id) {
                        Ok(h) => {
                            watchers.insert(r.root_id.clone(), h);
                        }
                        Err(e) => tracing::warn!(
                            root_id = %r.root_id,
                            error = %e,
                            "watcher unavailable at launch"
                        ),
                    }
                }
            }

            app.manage(state);
            pump::spawn_ingest_pump(app.handle().clone());
            pump::spawn_sidecar_pump(app.handle().clone());
            pump::spawn_runtime_pump(app.handle().clone());
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
                    state.shutdown();
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
        // Semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md).
        commands::graph::topic_affinities,
        commands::graph::suggest_topics,
        commands::graph::cluster_topics,
        commands::graph::suggest_topics_llm,
        commands::graph::graph_tuning,
        // Topics + autosuggest + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md).
        commands::topics::add_topic,
        commands::topics::list_topics,
        commands::topics::remove_topic,
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
        commands::library::rebuild_previews,
        commands::library::request_full_decode,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::list_images,
        commands::library::ingest_status,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::set_external_editor,
        commands::app::set_preview_cache_budget,
        commands::app::preview_cache_stats,
        commands::app::clear_preview_cache,
        commands::app::runtime_status,
        commands::app::runtime_consent,
        commands::app::runtime_accept_license,
        commands::app::runtime_download_model,
        commands::app::runtime_remove_model,
        commands::app::runtime_restart,
        commands::app::runtime_redetect,
        commands::app::export_journal,
        commands::app::rebuild_index,
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
        // Semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md).
        commands::graph::topic_affinities,
        commands::graph::suggest_topics,
        commands::graph::cluster_topics,
        commands::graph::suggest_topics_llm,
        commands::graph::graph_tuning,
        // Topics + autosuggest + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md).
        commands::topics::add_topic,
        commands::topics::list_topics,
        commands::topics::remove_topic,
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
        commands::library::rebuild_previews,
        commands::library::request_full_decode,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::list_images,
        commands::library::ingest_status,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::set_external_editor,
        commands::app::set_preview_cache_budget,
        commands::app::preview_cache_stats,
        commands::app::clear_preview_cache,
        commands::app::runtime_status,
        commands::app::runtime_consent,
        commands::app::runtime_accept_license,
        commands::app::runtime_download_model,
        commands::app::runtime_remove_model,
        commands::app::runtime_restart,
        commands::app::runtime_redetect,
        commands::app::export_journal,
        commands::app::rebuild_index,
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
