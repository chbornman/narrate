//! Roots & grid commands (LIBRARY §5, UI §3) — moved verbatim from the old
//! commands.rs (FOUNDATIONS split), plus the NEW `rescan_root` for the
//! rail-folder menu (featureset §6).

use photoproof_core::library::ScanOptions;

use super::S;
use crate::dto::{FolderNode, GridItem, IngestStatus, RootDto};
use crate::error::{CmdError, CmdResult};
use crate::pump;
use crate::state::App;

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

/// NEW (P4.2): on-demand rescan of one root — the rail-folder menu's
/// "Rescan" (featureset §6). The scan only enqueues ingest passes; the
/// pump processes them and the grid refreshes incrementally.
#[tauri::command]
pub async fn rescan_root(app: S<'_>, root_id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.library.scan_root(&root_id, &ScanOptions::default())?;
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

#[tauri::command]
pub fn ingest_status(app: S<'_>) -> IngestStatus {
    pump::ingest_status(&app)
}
