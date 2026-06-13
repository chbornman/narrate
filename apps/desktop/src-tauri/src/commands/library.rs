//! Roots & grid commands (LIBRARY §5, UI §3) — moved verbatim from the old
//! commands.rs (FOUNDATIONS split), plus the NEW `rescan_root` for the
//! rail-folder menu (featureset §6).

use photoproof_core::library::ScanOptions;
use tauri::{AppHandle, Emitter, Runtime};

use super::S;
use crate::dto::{AddRootOutcome, FolderNode, GridItem, IngestStatus, RootDto};
use crate::error::{CmdError, CmdResult};
use crate::pump;
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
fn active_roots(app: &App) -> CmdResult<Vec<RootDto>> {
    let mut out = Vec::new();
    for r in app.library.roots()? {
        if r.state == "active" {
            out.push(root_dto(app, &r)?);
        }
    }
    Ok(out)
}

/// Root edits land in EVERY window instantly (a folder added/removed in
/// Settings appears in the main-window rail live — the P4.2b
/// `settings-changed` pattern): emit the fresh snapshot to all windows.
fn emit_roots_changed<R: Runtime>(app: &App, handle: &AppHandle<R>) {
    if let Ok(roots) = active_roots(app) {
        let _ = handle.emit("roots-changed", roots);
    }
}

#[tauri::command]
pub async fn list_roots(app: S<'_>) -> CmdResult<Vec<RootDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || active_roots(&app))
        .await
        .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
) -> CmdResult<AddRootOutcome> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
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
        // Initial scan on its own thread: it only enqueues ingest passes;
        // the pump processes them. The walk registers with `app.scans` so
        // `ingest_status` reports it live (scanning + discovered) — pass
        // rows only exist from hash time, and without this the empty grid
        // read "No photographs" for the whole walk of a slow volume
        // (founder, June 2026).
        {
            let scan_app = app.clone();
            let rid = root_id.clone();
            std::thread::Builder::new()
                .name("pp-initial-scan".into())
                .spawn(move || {
                    let _walk = scan_app.scans.begin(); // guard: de-registers on every exit
                    let opts = ScanOptions {
                        discovered: Some(scan_app.scans.counter()),
                        ..ScanOptions::default()
                    };
                    if let Err(e) = scan_app.library.scan_root(&rid, &opts) {
                        tracing::error!(root_id = %rid, error = %e, "initial scan failed");
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
        let dto = root_dto(&app, &record)?;
        emit_roots_changed(&app, &handle);
        Ok(AddRootOutcome::Added { root: dto })
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.watchers
            .lock()
            .expect("watchers mutex")
            .remove(&root_id);
        app.library.remove_root(&root_id)?;
        emit_roots_changed(&app, &handle);
        Ok(())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.library.archive_root(&root_id)?;
        // Drop the watcher AFTER the state flip succeeds: a failed archive
        // (e.g. already removed) must not silently stop watching an active
        // root.
        app.watchers
            .lock()
            .expect("watchers mutex")
            .remove(&root_id);
        emit_roots_changed(&app, &handle);
        Ok(())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.library.unarchive_root(&root_id)?;
        // Restart the live watcher (mirrors add_root): an active root watches.
        match app.library.start_watcher(&root_id) {
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
        {
            let scan_app = app.clone();
            let rid = root_id.clone();
            std::thread::Builder::new()
                .name("pp-unarchive-scan".into())
                .spawn(move || {
                    let _walk = scan_app.scans.begin();
                    let opts = ScanOptions {
                        discovered: Some(scan_app.scans.counter()),
                        ..ScanOptions::default()
                    };
                    if let Err(e) = scan_app.library.scan_root(&rid, &opts) {
                        tracing::error!(root_id = %rid, error = %e, "unarchive rescan failed");
                    }
                })
                .expect("spawn unarchive scan thread");
        }
        let record = app
            .library
            .root(&root_id)?
            .ok_or_else(|| CmdError::Invalid("root vanished after unarchive".into()))?;
        let dto = root_dto(&app, &record)?;
        emit_roots_changed(&app, &handle);
        Ok(dto)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Archived-roots snapshot for the rail's collapsed "Archived" affordance.
#[tauri::command]
pub async fn list_archived_roots(app: S<'_>) -> CmdResult<Vec<RootDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        for r in app.library.archived_roots()? {
            out.push(root_dto(&app, &r)?);
        }
        Ok(out)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// NEW (P4.2): on-demand rescan of one root — the rail-folder menu's
/// "Rescan" (featureset §6). The scan only enqueues ingest passes; the
/// pump processes them and the grid refreshes incrementally.
#[tauri::command]
pub async fn rescan_root(app: S<'_>, root_id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        // Same live-walk registration as add_root's initial scan: this
        // command blocks until the rescan completes, and the pump reports
        // scanning + discovered to the empty grid meanwhile.
        let _walk = app.scans.begin();
        app.library.scan_root(
            &root_id,
            &ScanOptions {
                discovered: Some(app.scans.counter()),
                ..ScanOptions::default()
            },
        )?;
        Ok(())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        Ok(app.library.rebuild_previews(&root_id)?)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || Ok(app.library.request_full_decode(&hash)?))
        .await
        .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = hashes
            .iter()
            .map(|h| super::parse_hash(h))
            .collect::<CmdResult<Vec<_>>>()?;
        Ok(app.library.prioritize_previews(&parsed)?)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
        Ok(items.into_iter().map(grid_item).collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = hashes
            .iter()
            .map(|h| super::parse_hash(h))
            .collect::<CmdResult<Vec<_>>>()?;
        let items = app.library.list_images(&parsed)?;
        Ok(items.into_iter().map(grid_item).collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[tauri::command]
pub fn ingest_status(app: S<'_>) -> IngestStatus {
    pump::ingest_status(&app)
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
}
