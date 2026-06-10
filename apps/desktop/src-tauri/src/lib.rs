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
        // Thumbnails + Look images: photoproof://localhost/{thumb|display}/{hash}
        // straight from the preview cache. Bytes never cross IPC, never
        // base64 (UI §3.3, DECISIONS P16).
        .register_asynchronous_uri_scheme_protocol("photoproof", |ctx, request, responder| {
            let path = request.uri().path().to_owned();
            let cache_dir = ctx
                .app_handle()
                .try_state::<Arc<App>>()
                .map(|s| s.library.cache_dir().to_path_buf());
            std::thread::spawn(move || {
                let response = cache_dir
                    .and_then(|dir| protocol::resolve(&dir, &path))
                    .and_then(|file| std::fs::read(file).ok())
                    .map(protocol::respond_ok)
                    .unwrap_or_else(protocol::respond_not_found);
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

#[cfg(not(feature = "debug-panel"))]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::set_scope,
        commands::indicator_state,
        commands::add_note,
        commands::set_rating,
        commands::report_activity,
        commands::search,
        commands::list_roots,
        commands::add_root,
        commands::remove_root,
        commands::folder_tree,
        commands::list_folder,
        commands::ingest_status,
        commands::settings_get,
        commands::runtime_status,
        commands::export_journal,
        commands::rebuild_index,
        commands::open_settings_window,
        commands::quit,
    ]
}

/// Dev builds additionally register the debug-panel commands (UI §10.1:
/// they do not exist in release binaries; invoking one there fails as
/// unknown — asserted by scripts/assert-release-clean.sh).
#[cfg(feature = "debug-panel")]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::set_scope,
        commands::indicator_state,
        commands::add_note,
        commands::set_rating,
        commands::report_activity,
        commands::search,
        commands::list_roots,
        commands::add_root,
        commands::remove_root,
        commands::folder_tree,
        commands::list_folder,
        commands::ingest_status,
        commands::settings_get,
        commands::runtime_status,
        commands::export_journal,
        commands::rebuild_index,
        commands::open_settings_window,
        commands::quit,
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
