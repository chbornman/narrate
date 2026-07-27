//! Roots & grid commands (LIBRARY §5, UI §3) — moved verbatim from the old
//! commands.rs (FOUNDATIONS split), plus the NEW `rescan_root` for the
//! rail-folder menu (featureset §6).

use std::sync::Arc;

use photoproof_core::library::ScanOptions;
use tauri::{AppHandle, Emitter, Runtime};

use super::{S, run_blocking};
use crate::command_work::CommandClass;
use crate::convergence::StateDomain;
use crate::dto::{AddRootOutcome, FolderNode, GridItem, IngestStatus, RootDto};
use crate::error::{CmdError, CmdResult};
use crate::managed_tasks::{SpawnTaskError, TaskPriority};
use crate::pump;
use crate::resource_governor::ResourceLane;
use crate::settings::{NewRootPolicy, save as save_settings};
use crate::state::App;
use photoproof_core::library::LibraryError;

pub(crate) fn root_dto(
    app: &App,
    root: &photoproof_core::library::RootRecord,
) -> CmdResult<RootDto> {
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
        archived: root.state == "archived",
    })
}

/// Active-roots snapshot: `list_roots` and the `roots-changed` payload
/// share it.
pub(crate) fn active_roots(app: &App) -> CmdResult<Vec<RootDto>> {
    let mut out = Vec::new();
    for r in app.library.roots()? {
        if r.state == "active" {
            out.push(root_dto(app, &r)?);
        }
    }
    Ok(out)
}

pub(crate) fn archived_roots(app: &App) -> CmdResult<Vec<RootDto>> {
    let mut out = Vec::new();
    for r in app.library.archived_roots()? {
        out.push(root_dto(app, &r)?);
    }
    Ok(out)
}

/// Root edits land in EVERY window instantly (a folder added/removed in
/// Settings appears in the main-window rail live — the P4.2b
/// `settings-changed` pattern): emit the fresh snapshot to all windows.
fn emit_roots_changed<R: Runtime>(app: &App, handle: &AppHandle<R>) {
    if let Ok(roots) = active_roots(app) {
        let _ = handle.emit("roots-changed", roots);
        app.convergence.publish(handle, [StateDomain::Roots]);
    }
}

/// Ensure an active root owns a live watcher. Finished watcher threads are
/// removed before replacement so health recovery cannot mistake a retained
/// dead handle for active coverage.
fn ensure_root_watcher(app: &App, root_id: &str) -> CmdResult<()> {
    let root = app
        .library
        .root(root_id)?
        .ok_or_else(|| CmdError::Invalid(format!("unknown root {root_id}")))?;
    if root.state != "active" {
        return Err(CmdError::Invalid(format!(
            "root {root_id} is {}; only active roots can be watched",
            root.state
        )));
    }

    {
        let mut watchers = app.watchers.lock().expect("watchers mutex");
        if watchers
            .get(root_id)
            .is_some_and(photoproof_core::library::RootWatcherHandle::is_active)
        {
            return Ok(());
        }
        watchers.remove(root_id);
    }

    let watcher_resources = Arc::clone(&app.resources);
    let watcher = app
        .library
        .start_watcher_with_options(root_id, move |cancel| {
            watcher_resources.watcher_scan(cancel)
        })
        .map_err(|error| {
            CmdError::Unavailable(format!("could not watch root {root_id}: {error}"))
        })?;
    app.watchers
        .lock()
        .expect("watchers mutex")
        .insert(root_id.to_owned(), watcher);
    Ok(())
}

/// Start a filesystem walk under the process registry. A root has one scan
/// lane for UI-triggered background walks, so repeated add/unarchive actions
/// cannot stack concurrent walks over the same root.
fn spawn_root_scan(
    app: &Arc<App>,
    root_id: String,
    trigger: &'static str,
) -> Result<(), SpawnTaskError> {
    let scan_app = Arc::clone(app);
    let key = format!("root-scan:{root_id}");
    let task_root_id = root_id.clone();
    let spawn = app
        .tasks
        .spawn("library", key, TaskPriority::Maintenance, move |task| {
            let cancel = task.cancel_flag();
            let Some(_resource) = scan_app.resources.acquire(ResourceLane::RootScan, &cancel)
            else {
                return Ok(());
            };
            let _walk = scan_app.scans.begin();
            let opts = ScanOptions {
                cancel: Some(cancel),
                discovered: Some(scan_app.scans.counter()),
                max_concurrency: Some(scan_app.resources.budget().ingest_concurrency),
                pause: Some(scan_app.resources.pause_token()),
                ..ScanOptions::default()
            };
            match scan_app.library.scan_root(&task_root_id, &opts) {
                Ok(_) => Ok(()),
                Err(_) if task.is_cancelled() => Ok(()),
                Err(error) => {
                    tracing::error!(
                        root_id = %task_root_id,
                        %trigger,
                        error = %error,
                        "managed root scan failed"
                    );
                    Err(error.to_string())
                }
            }
        });
    match spawn {
        Ok(()) | Err(SpawnTaskError::AlreadyRunning { .. }) => Ok(()),
        Err(error) => {
            tracing::error!(root_id, %trigger, %error, "managed root scan unavailable");
            Err(error)
        }
    }
}

fn rollback_new_root(app: &App, root_id: &str) -> CmdResult<()> {
    app.watchers.lock().expect("watchers mutex").remove(root_id);
    app.library.remove_root(root_id)?;
    let mut settings = app.settings.lock().expect("settings mutex");
    super::app::persist_then_publish_settings(
        &mut settings,
        |candidate| {
            candidate.root_processing_policies.remove(root_id);
        },
        |candidate| save_settings(&app.app_data, candidate),
    )?;
    Ok(())
}

#[tauri::command]
pub async fn list_roots(app: S<'_>) -> CmdResult<Vec<RootDto>> {
    let app = app.inner().clone();
    run_blocking(app, "library.list-roots", CommandClass::Read, active_roots).await
}

/// Register a watched root, kick the initial scan (background), start the
/// live watcher. Ingest runs through the pump; the grid populates
/// incrementally (UI §9.1). Emits `roots-changed` so every window's rail
/// updates live.
#[tauri::command]
pub async fn add_root<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    path: String,
    policy: Option<NewRootPolicy>,
) -> CmdResult<AddRootOutcome> {
    let app = app.inner().clone();
    let scan_app = Arc::clone(&app);
    run_blocking(
        app,
        "library.add-root",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            // Refuse + alias (folder-tree improvements): a folder overlapping an
            // existing active root is NOT an error to surface raw — it is a
            // navigation. Catch the structured refusal and hand the rail the
            // existing root's id so it can jump there instead of double-ingesting.
            let root_id = match app.library.register_root(std::path::Path::new(&path), None) {
                Ok(id) => id,
                Err(LibraryError::OverlappingRoot {
                    existing_root_id, ..
                }) => {
                    return Ok(AddRootOutcome::Overlap { existing_root_id });
                }
                Err(e) => return Err(e.into()),
            };
            let effective_policy = {
                let mut settings = app.settings.lock().expect("settings mutex");
                let effective = policy.unwrap_or(settings.new_root_policy);
                if let Err(error) = super::app::persist_then_publish_settings(
                    &mut settings,
                    |candidate| {
                        candidate
                            .root_processing_policies
                            .insert(root_id.clone(), effective);
                    },
                    |candidate| save_settings(&app.app_data, candidate),
                ) {
                    // Registration and its processing contract are one product
                    // action. A failed durable policy write must not leave an
                    // active, unwatched root that silently falls back to full
                    // model processing.
                    if let Err(rollback) = app.library.remove_root(&root_id) {
                        tracing::error!(
                            %root_id,
                            %rollback,
                            "failed to roll back root after policy persistence failure"
                        );
                    }
                    return Err(error.into());
                }
                effective
            };
            // Initial scan on its own thread: it only enqueues ingest passes;
            // the pump processes them. The walk registers with `app.scans` so
            // `ingest_status` reports it live (scanning + discovered) — pass
            // rows only exist from hash time, and without this the empty grid
            // read "No photographs" for the whole walk of a slow volume
            // (founder, June 2026).
            if matches!(
                effective_policy,
                NewRootPolicy::ProcessNow | NewRootPolicy::PreviewOnly
            ) {
                if let Err(error) = spawn_root_scan(&scan_app, root_id.clone(), "initial") {
                    rollback_new_root(app, &root_id)?;
                    return Err(CmdError::Unavailable(format!(
                        "could not start initial folder scan: {error}"
                    )));
                }
            } else {
                tracing::info!(
                    root_id = %root_id,
                    "new source registered without an initial scan by processing policy"
                );
            }
            let watcher_resources = Arc::clone(&app.resources);
            match app
                .library
                .start_watcher_with_options(&root_id, move |cancel| {
                    watcher_resources.watcher_scan(cancel)
                }) {
                Ok(handle) => {
                    app.watchers
                        .lock()
                        .expect("watchers mutex")
                        .insert(root_id.clone(), handle);
                }
                Err(e) => tracing::warn!(
                    root_id = %root_id,
                    error = %e,
                    "watcher failed; polled rescans still run"
                ),
            }
            let record = app
                .library
                .root(&root_id)?
                .ok_or_else(|| CmdError::Invalid("root vanished after register".into()))?;
            let dto = root_dto(app, &record)?;
            emit_roots_changed(app, &handle);
            Ok(AddRootOutcome::Added { root: dto })
        },
    )
    .await
}

/// Remove a watched root (UI §2.4: journals and sidecars untouched; the
/// images leave the index). Emits `roots-changed` — same as `add_root`.
#[tauri::command]
pub async fn remove_root<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    root_id: String,
) -> CmdResult<()> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.remove-root",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            app.tasks.cancel("library", &format!("root-scan:{root_id}"));
            app.watchers
                .lock()
                .expect("watchers mutex")
                .remove(&root_id);
            app.library.remove_root(&root_id)?;
            let mut settings = app.settings.lock().expect("settings mutex");
            if let Err(error) = super::app::persist_then_publish_settings(
                &mut settings,
                |candidate| {
                    candidate.root_processing_policies.remove(&root_id);
                },
                |candidate| save_settings(&app.app_data, candidate),
            ) {
                tracing::warn!(%error, %root_id, "removed root policy cleanup was not persisted");
            }
            emit_roots_changed(app, &handle);
            Ok(())
        },
    )
    .await
}

/// Archive a root (folder-tree improvements): hide it from the active rail
/// without destroying anything. Non-destructive — the library flips the
/// state only (journals + collection memberships are keyed by image hash, so
/// they are untouched); here the command also stops the live watcher (an
/// archived root should not consume an inotify/FSEvents handle). Emits
/// `roots-changed` so the now-archived root drops out of every window's
/// active list live.
#[tauri::command]
pub async fn archive_root<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    root_id: String,
) -> CmdResult<()> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.archive-root",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            // A root lifecycle transition is also a work-ownership transition.
            // Signal an add/rescan/unarchive walk before flipping the durable
            // state; ScanOptions observes the same token and suppresses stale
            // inference on its incomplete exit.
            app.tasks.cancel("library", &format!("root-scan:{root_id}"));
            app.library.archive_root(&root_id)?;
            // Drop the watcher AFTER the state flip succeeds: a failed archive
            // (e.g. already removed) must not silently stop watching an active
            // root.
            app.watchers
                .lock()
                .expect("watchers mutex")
                .remove(&root_id);
            emit_roots_changed(app, &handle);
            Ok(())
        },
    )
    .await
}

/// Restore an archived root to active (folder-tree improvements): the reverse
/// of `archive_root`. Restarts the watcher and kicks a rescan so any on-disk
/// drift while the root rested reconciles, then emits `roots-changed`.
#[tauri::command]
pub async fn unarchive_root<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    root_id: String,
) -> CmdResult<RootDto> {
    let app = app.inner().clone();
    let scan_app = Arc::clone(&app);
    run_blocking(
        app,
        "library.unarchive-root",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            app.library.unarchive_root(&root_id)?;
            // Restart the live watcher (mirrors add_root): an active root watches.
            let watcher_resources = Arc::clone(&app.resources);
            match app
                .library
                .start_watcher_with_options(&root_id, move |cancel| {
                    watcher_resources.watcher_scan(cancel)
                }) {
                Ok(h) => {
                    app.watchers
                        .lock()
                        .expect("watchers mutex")
                        .insert(root_id.clone(), h);
                }
                Err(e) => tracing::warn!(
                    root_id = %root_id,
                    error = %e,
                    "watcher failed on unarchive; polled rescans still run"
                ),
            }
            // Reconcile drift accumulated while archived (its own thread, like the
            // add_root initial scan): only enqueues passes, the pump drains them.
            if let Err(error) = spawn_root_scan(&scan_app, root_id.clone(), "unarchive") {
                app.watchers
                    .lock()
                    .expect("watchers mutex")
                    .remove(&root_id);
                app.library.archive_root(&root_id)?;
                return Err(CmdError::Unavailable(format!(
                    "could not start restored-folder scan: {error}"
                )));
            }
            let record = app
                .library
                .root(&root_id)?
                .ok_or_else(|| CmdError::Invalid("root vanished after unarchive".into()))?;
            let dto = root_dto(app, &record)?;
            emit_roots_changed(app, &handle);
            Ok(dto)
        },
    )
    .await
}

/// Archived-roots snapshot for the rail's collapsed "Archived" affordance.
#[tauri::command]
pub async fn list_archived_roots(app: S<'_>) -> CmdResult<Vec<RootDto>> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.list-archived-roots",
        CommandClass::Read,
        archived_roots,
    )
    .await
}

/// NEW (P4.2): on-demand rescan of one root — the rail-folder menu's
/// "Rescan" (featureset §6). The scan only enqueues ingest passes; the
/// pump processes them and the grid refreshes incrementally.
#[tauri::command]
pub async fn rescan_root(app: S<'_>, root_id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.rescan-root",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            ensure_root_watcher(app, &root_id)?;
            {
                let mut settings = app.settings.lock().expect("settings mutex");
                super::app::persist_then_publish_settings(
                    &mut settings,
                    |candidate| {
                        candidate
                            .root_processing_policies
                            .insert(root_id.clone(), NewRootPolicy::ProcessNow);
                    },
                    |candidate| save_settings(&app.app_data, candidate),
                )?;
            }
            // Same live-walk registration as add_root's initial scan: this
            // command blocks until the rescan completes, and the pump reports
            // scanning + discovered to the empty grid meanwhile.
            let _walk = app.scans.begin();
            app.library.scan_root(
                &root_id,
                &ScanOptions {
                    discovered: Some(app.scans.counter()),
                    max_concurrency: Some(app.resources.budget().ingest_concurrency),
                    pause: Some(app.resources.pause_token()),
                    ..ScanOptions::default()
                },
            )?;
            Ok(())
        },
    )
    .await
}

/// Recover every active root from backend-authoritative inventory. This does
/// not depend on a possibly stale/failed frontend roots read: each watcher is
/// ensured synchronously, then one managed reconciliation scan is admitted per
/// root. All roots are attempted and aggregate failures are returned.
#[tauri::command]
pub async fn recover_roots<R: Runtime>(app: S<'_>, handle: AppHandle<R>) -> CmdResult<()> {
    let app = app.inner().clone();
    let scan_app = Arc::clone(&app);
    run_blocking(
        app,
        "library.recover-roots",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            let roots = app.library.roots()?;
            let mut failures = Vec::new();
            for root in roots.into_iter().filter(|root| root.state == "active") {
                if let Err(error) = ensure_root_watcher(app, &root.root_id) {
                    failures.push(format!("{} watcher: {error}", root.root_id));
                    continue;
                }
                if let Err(error) =
                    spawn_root_scan(&scan_app, root.root_id.clone(), "health-recovery")
                {
                    failures.push(format!("{} scan: {error}", root.root_id));
                }
            }
            emit_roots_changed(app, &handle);
            if failures.is_empty() {
                Ok(())
            } else {
                Err(CmdError::Unavailable(format!(
                    "folder recovery was incomplete: {}",
                    failures.join("; ")
                )))
            }
        },
    )
    .await
}

/// "Rebuild previews…" on the rail-folder menu (BACKLOG, founder dogfood
/// round 3) — the recovery verb SEPARATE from Rescan: it re-pends the
/// preview pass for every image under the root at backfill priority
/// (LIBRARY §9.8/§10.3 — the generator_version machinery's manual
/// trigger); the pump regenerates artifacts idempotently and thumbs heal
/// off `previews-changed`. Returns the number of passes re-pended.
#[tauri::command]
pub async fn rebuild_previews(app: S<'_>, root_id: String) -> CmdResult<usize> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.rebuild-previews",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            Ok(app.library.rebuild_previews(&root_id)?)
        },
    )
    .await
}

/// On-demand full RAW develop trigger (OD-1): Look calls this when its
/// resolution ladder reaches a RAW with no cached full-decode artifact. It
/// enqueues ONE develop pass at the top interactive priority (above the
/// watcher) and returns whether a develop is now pending (true) or the cache
/// already held it (false — Look serves `/full-decode` immediately). Idempotent
/// and cheap: a cache hit or an existing row is a no-op. Non-RAW hashes return
/// false. The develop runs on the pump's `drain_raw_decode` tick; the
/// `/full-decode/<hash>` route 404s ("developing...") until it lands.
#[tauri::command]
pub async fn request_full_decode(app: S<'_>, hash: String) -> CmdResult<bool> {
    let app = app.inner().clone();
    let hash = super::parse_hash(&hash)?;
    run_blocking(
        app,
        "library.request-full-decode",
        CommandClass::Mutation,
        move |app| Ok(app.library.request_full_decode(&hash)?),
    )
    .await
}

/// Viewport-first preview generation (OD-2): the grid sends the hashes the
/// user is currently scrolled to (visible + a small look-ahead) whose thumbnail
/// is not yet generated, so those preview passes jump ahead of the offscreen
/// backfill the pump would otherwise grind through first. Bumps only PENDING
/// preview rows to the top interactive priority; running/done rows and hashes
/// without a pending preview are untouched. Only hashes cross IPC; returns how
/// many rows were promoted. The frontend debounces this on scroll-settle and
/// only sends previews-missing hashes, so it complements (does not fight) the
/// client-side `thumbqueue` LOAD ordering.
#[tauri::command]
pub async fn prioritize_previews(app: S<'_>, hashes: Vec<String>) -> CmdResult<usize> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "library.prioritize-previews",
        CommandClass::Mutation,
        move |app| {
            let parsed = hashes
                .iter()
                .map(|h| super::parse_hash(h))
                .collect::<CmdResult<Vec<_>>>()?;
            Ok(app.library.prioritize_previews(&parsed)?)
        },
    )
    .await
}

/// Core badge row → wire shape — shared with the collection-members grid
/// read (commands/collections.rs), so both grid listings stay one mapping.
pub(crate) fn grid_item(i: photoproof_core::library::FolderImage) -> GridItem {
    GridItem {
        hash: i.hash.as_str().to_owned(),
        file_name: i.file_name,
        rel_path: i.rel_path,
        root_id: i.root_id,
        capture_ts: i.capture_ts,
        added_ts: i.first_ingested_at,
        has_journal: i.has_journal,
        rating: i.rating,
        offline: i.offline,
        preview_ready: i.preview_ready,
    }
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
    run_blocking(app, "library.folder-tree", CommandClass::Read, move |app| {
        let tree = app.library.folder_tree(&root_id)?;
        Ok(tree.into_iter().map(folder_node).collect())
    })
    .await
}

/// Grid listing: direct-children images of one folder with badge data
/// (has-journal, rating fold, offline) in one batched read
/// (`Library::list_folder`). Thumbnails load via `photoproof://` URLs the
/// frontend derives from `hash`.
#[tauri::command]
pub async fn list_folder(app: S<'_>, root_id: String, folder: String) -> CmdResult<Vec<GridItem>> {
    let app = app.inner().clone();
    run_blocking(app, "library.list-folder", CommandClass::Read, move |app| {
        let items = app.library.list_folder(&root_id, &folder)?;
        Ok(items.into_iter().map(grid_item).collect())
    })
    .await
}

/// Grid listing for an explicit hash list, in the SAME order given
/// (M3 search-as-scope, Phase 1): the `search` command returns result
/// hashes in fused/relevance order, and the query grid renders them as
/// ordinary cells — so it needs the same badge-bearing `GridItem` rows the
/// folder and collection grids get. Reuses `Library::list_images` (the
/// collection-members read uses it too), which preserves input order and
/// silently skips hashes the library never indexed (a result whose file
/// isn't in THIS library has nothing to render); the frontend keeps the
/// fused order by feeding these straight to the grid under `relevance`
/// sort (a pass-through).
#[tauri::command]
pub async fn list_images(app: S<'_>, hashes: Vec<String>) -> CmdResult<Vec<GridItem>> {
    let app = app.inner().clone();
    run_blocking(app, "library.list-images", CommandClass::Read, move |app| {
        let parsed = hashes
            .iter()
            .map(|h| super::parse_hash(h))
            .collect::<CmdResult<Vec<_>>>()?;
        let items = app.library.list_images(&parsed)?;
        Ok(items.into_iter().map(grid_item).collect())
    })
    .await
}

#[tauri::command]
pub fn ingest_status(app: S<'_>) -> CmdResult<IngestStatus> {
    let app = app.inner().clone();
    let _permit = super::admit(&app, "library.ingest-status", CommandClass::Read)?;
    Ok(pump::ingest_status(&app))
}

// ---------------------------------------------------------------------------
// Tests — the `roots-changed` emission over a mock Tauri app (the commands
// are generic over Runtime so the MockRuntime drives the REAL command path).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
    use tauri::{Listener, Manager};

    use super::*;

    fn mock_app() -> (tempfile::TempDir, tauri::App<MockRuntime>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tauri_app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let state = Arc::new(App::init(tmp.path().join("appdata")).expect("app init"));
        tauri_app.manage(state);
        (tmp, tauri_app)
    }

    /// Adding/removing a folder in the Settings window must appear in the
    /// main-window rail INSTANTLY (founder dogfood, round 2): both commands
    /// emit `roots-changed` carrying the fresh active-roots snapshot — the
    /// P4.2b `settings-changed` pattern.
    #[test]
    fn add_and_remove_root_emit_roots_changed_snapshots() {
        let (tmp, tauri_app) = mock_app();
        let payloads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = payloads.clone();
        tauri_app.listen_any("roots-changed", move |e| {
            sink.lock()
                .expect("payload mutex")
                .push(e.payload().to_owned());
        });
        let photos = tmp.path().join("photos");
        std::fs::create_dir_all(&photos).expect("photos dir");

        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            handle.clone(),
            photos.display().to_string(),
            None,
        ))
        .expect("add_root");
        // A fresh folder adds cleanly (no overlap).
        let root_id = match outcome {
            AddRootOutcome::Added { root } => root.root_id,
            AddRootOutcome::Overlap { .. } => panic!("fresh folder must not overlap"),
        };
        {
            let got = payloads.lock().expect("payload mutex");
            assert_eq!(got.len(), 1, "add_root emits exactly once");
            assert!(
                got[0].contains(&root_id),
                "payload is the fresh snapshot (carries the new root)"
            );
        }

        tauri::async_runtime::block_on(remove_root(state, handle, root_id.clone()))
            .expect("remove_root");
        let got = payloads.lock().expect("payload mutex");
        assert_eq!(got.len(), 2, "remove_root emits exactly once");
        assert!(
            !got[1].contains(&root_id),
            "the removed root has left the snapshot"
        );
    }

    #[test]
    fn process_later_registers_and_watches_without_starting_initial_walk() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("later");
        std::fs::create_dir_all(&photos).unwrap();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        state
            .settings
            .lock()
            .expect("settings mutex")
            .new_root_policy = NewRootPolicy::ProcessLater;

        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            tauri_app.handle().clone(),
            photos.display().to_string(),
            None,
        ))
        .unwrap();
        let AddRootOutcome::Added { root } = outcome else {
            panic!("fresh folder must add");
        };
        assert!(
            !state
                .tasks
                .is_running("library", &format!("root-scan:{}", root.root_id)),
            "Process later must not start the expensive initial tree walk"
        );
        assert!(
            state
                .watchers
                .lock()
                .expect("watchers mutex")
                .contains_key(&root.root_id),
            "Process later still installs live change detection"
        );
    }

    #[test]
    fn recover_roots_reinstalls_a_missing_watcher_from_backend_inventory() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("recover-watcher");
        std::fs::create_dir_all(&photos).unwrap();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            tauri_app.handle().clone(),
            photos.display().to_string(),
            Some(NewRootPolicy::ProcessLater),
        ))
        .unwrap();
        let AddRootOutcome::Added { root } = outcome else {
            panic!("fresh folder must add");
        };

        state
            .watchers
            .lock()
            .expect("watchers mutex")
            .remove(&root.root_id);
        assert!(
            !state
                .watchers
                .lock()
                .expect("watchers mutex")
                .contains_key(&root.root_id)
        );

        tauri::async_runtime::block_on(recover_roots(state.clone(), tauri_app.handle().clone()))
            .expect("backend-authoritative root recovery");

        assert!(
            state
                .watchers
                .lock()
                .expect("watchers mutex")
                .get(&root.root_id)
                .is_some_and(photoproof_core::library::RootWatcherHandle::is_active),
            "recovery must replace the missing watcher without a frontend root list"
        );
    }

    #[test]
    fn one_shot_preview_policy_is_persisted_without_changing_the_default() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("preview-only");
        std::fs::create_dir_all(&photos).unwrap();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        assert_eq!(
            state.settings.lock().unwrap().new_root_policy,
            NewRootPolicy::ProcessNow
        );
        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            tauri_app.handle().clone(),
            photos.display().to_string(),
            Some(NewRootPolicy::PreviewOnly),
        ))
        .unwrap();
        let AddRootOutcome::Added { root } = outcome else {
            panic!("fresh folder must add");
        };
        let settings = state.settings.lock().unwrap();
        assert_eq!(settings.new_root_policy, NewRootPolicy::ProcessNow);
        assert_eq!(
            settings.root_processing_policies.get(&root.root_id),
            Some(&NewRootPolicy::PreviewOnly)
        );
        let disk = crate::settings::load_checked(&tmp.path().join("appdata"))
            .unwrap()
            .settings;
        assert_eq!(
            disk.root_processing_policies.get(&root.root_id),
            Some(&NewRootPolicy::PreviewOnly)
        );
    }

    #[test]
    fn archive_remove_and_readd_transfer_watcher_and_scan_ownership() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("lifecycle");
        std::fs::create_dir_all(&photos).unwrap();
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();

        // ProcessLater makes watcher ownership deterministic without an initial
        // walk racing the first archive assertion.
        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            handle.clone(),
            photos.display().to_string(),
            Some(NewRootPolicy::ProcessLater),
        ))
        .unwrap();
        let AddRootOutcome::Added { root } = outcome else {
            panic!("fresh folder must add");
        };
        let root_id = root.root_id;
        assert!(state.watchers.lock().unwrap().contains_key(&root_id));

        tauri::async_runtime::block_on(archive_root(
            state.clone(),
            handle.clone(),
            root_id.clone(),
        ))
        .unwrap();
        assert!(
            !state.watchers.lock().unwrap().contains_key(&root_id),
            "archived roots own neither watcher nor root-scan lane"
        );
        assert!(
            !state
                .tasks
                .is_running("library", &format!("root-scan:{root_id}"))
        );
        assert_eq!(
            state.library.root(&root_id).unwrap().unwrap().state,
            "archived"
        );

        let restored = tauri::async_runtime::block_on(unarchive_root(
            state.clone(),
            handle.clone(),
            root_id.clone(),
        ))
        .unwrap();
        assert_eq!(restored.root_id, root_id);
        assert!(
            state.watchers.lock().unwrap().contains_key(&root_id),
            "unarchive restores live change detection"
        );

        tauri::async_runtime::block_on(remove_root(state.clone(), handle.clone(), root_id.clone()))
            .unwrap();
        assert!(
            !state.watchers.lock().unwrap().contains_key(&root_id),
            "removed roots relinquish watcher ownership"
        );
        assert_eq!(
            state.library.root(&root_id).unwrap().unwrap().state,
            "removed"
        );

        let outcome = tauri::async_runtime::block_on(add_root(
            state,
            handle,
            photos.display().to_string(),
            Some(NewRootPolicy::ProcessLater),
        ))
        .unwrap();
        let AddRootOutcome::Added { root: readded } = outcome else {
            panic!("removed location must re-add");
        };
        assert_eq!(
            readded.root_id, root_id,
            "re-add revives the same durable root identity"
        );
        let managed: tauri::State<'_, Arc<App>> = tauri_app.state();
        assert!(managed.watchers.lock().unwrap().contains_key(&root_id));
    }

    #[test]
    fn initial_scan_dispatch_failure_rolls_back_the_new_active_root() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("dispatch-failure");
        std::fs::create_dir_all(&photos).unwrap();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        assert!(
            state
                .tasks
                .shutdown(std::time::Duration::from_millis(10))
                .acknowledged
        );

        let error = tauri::async_runtime::block_on(add_root(
            state.clone(),
            tauri_app.handle().clone(),
            photos.display().to_string(),
            Some(NewRootPolicy::ProcessNow),
        ))
        .expect_err("stopped task registry must reject initial scan");

        assert!(
            error
                .to_string()
                .contains("could not start initial folder scan")
        );
        assert!(active_roots(&state).unwrap().is_empty());
        assert!(
            state
                .settings
                .lock()
                .unwrap()
                .root_processing_policies
                .is_empty(),
            "the rolled-back registration cannot retain a phantom policy"
        );
    }

    #[test]
    fn unarchive_scan_dispatch_failure_restores_the_archived_state() {
        let (tmp, tauri_app) = mock_app();
        let photos = tmp.path().join("restore-dispatch-failure");
        std::fs::create_dir_all(&photos).unwrap();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let handle = tauri_app.handle().clone();
        let outcome = tauri::async_runtime::block_on(add_root(
            state.clone(),
            handle.clone(),
            photos.display().to_string(),
            Some(NewRootPolicy::ProcessLater),
        ))
        .unwrap();
        let AddRootOutcome::Added { root } = outcome else {
            panic!("fresh folder must add");
        };
        tauri::async_runtime::block_on(archive_root(
            state.clone(),
            handle.clone(),
            root.root_id.clone(),
        ))
        .unwrap();
        assert!(
            state
                .tasks
                .shutdown(std::time::Duration::from_millis(10))
                .acknowledged
        );

        let error = tauri::async_runtime::block_on(unarchive_root(
            state.clone(),
            handle,
            root.root_id.clone(),
        ))
        .expect_err("stopped task registry must reject restore scan");

        assert!(
            error
                .to_string()
                .contains("could not start restored-folder scan")
        );
        assert_eq!(
            state.library.root(&root.root_id).unwrap().unwrap().state,
            "archived"
        );
        assert!(!state.watchers.lock().unwrap().contains_key(&root.root_id));
    }
}
