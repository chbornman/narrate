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
#[cfg(feature = "debug-panel")]
mod debug;
mod dto;
mod error;
mod note;
mod protocol;
mod pump;
mod scope;
mod search_types;
mod search_wire;
mod session;
mod settings;
mod state;

use std::sync::Arc;

use tauri::{Manager, RunEvent};

use state::App;

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Window geometry persisted (featureset §6). The settings window is
        // denylisted: it stays the one modest window (UI §2.4). Known risk
        // (UI-ARCHITECTURE Appendix B): restore drift with custom titlebars
        // on some Wayland compositors — named in DOGFOOD §visual; the
        // fallback, if it misbehaves, is manual save/restore in
        // commands/app.rs keyed off the same RunEvent hooks.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_denylist(&["settings"])
                .build(),
        )
        // Thumbnails + Look images: photoproof://localhost/{thumb|display}/{hash}
        // straight from the preview cache, plus /original/{hash} — the
        // progressive full-resolution route Look swaps to past 1:1 (served
        // only for webview-decodable stored formats; protocol.rs owns the
        // allowlist). Bytes never cross IPC, never base64 (UI §3.3,
        // DECISIONS P16).
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
            let app_data = app.path().app_data_dir()?;
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
                        Err(e) => eprintln!(
                            "photoproof: watcher for {} unavailable at launch: {e}",
                            r.root_id
                        ),
                    }
                }
            }

            app.manage(state);
            pump::spawn_ingest_pump(app.handle().clone());
            pump::spawn_sidecar_pump(app.handle().clone());
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
#[cfg(not(feature = "debug-panel"))]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::capture::set_scope,
        commands::capture::indicator_state,
        commands::capture::add_note,
        commands::capture::set_rating,
        commands::capture::report_activity,
        commands::capture::add_stroke,
        commands::search::search,
        commands::library::list_roots,
        commands::library::add_root,
        commands::library::remove_root,
        commands::library::rescan_root,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::ingest_status,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::runtime_status,
        commands::app::export_journal,
        commands::app::rebuild_index,
        commands::app::open_settings_window,
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
    ]
}

/// Dev builds additionally register the debug-panel commands (UI §10.1:
/// they do not exist in release binaries; invoking one there fails as
/// unknown — asserted by scripts/assert-release-clean.sh).
#[cfg(feature = "debug-panel")]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::capture::set_scope,
        commands::capture::indicator_state,
        commands::capture::add_note,
        commands::capture::set_rating,
        commands::capture::report_activity,
        commands::capture::add_stroke,
        commands::search::search,
        commands::library::list_roots,
        commands::library::add_root,
        commands::library::remove_root,
        commands::library::rescan_root,
        commands::library::folder_tree,
        commands::library::list_folder,
        commands::library::ingest_status,
        commands::app::settings_get,
        commands::app::set_stack_display,
        commands::app::runtime_status,
        commands::app::export_journal,
        commands::app::rebuild_index,
        commands::app::open_settings_window,
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
        debug::debug_tail_events,
        debug::debug_capture,
        debug::debug_ingest,
        debug::debug_sidecars,
        debug::debug_search,
        debug::debug_runtime,
        debug::debug_force_flush,
        debug::debug_force_rescan,
    ]
}
