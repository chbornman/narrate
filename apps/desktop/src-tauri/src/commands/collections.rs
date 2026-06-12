//! Collections commands (RETRIEVAL §10, B71 — work packet P7.3): thin
//! wrappers over `photoproof_core::collections`. No UI surface ships in
//! this packet; the rail tab builds on these commands and on the
//! `collections-changed` snapshot event (the `roots-changed` pattern —
//! mutations from any window land in every window live).
//!
//! Membership HISTORY (closed intervals) stays a core-level read until a
//! surface needs it; everything else in §10.1 — including notes and the
//! description — is reachable here.

use std::sync::Mutex;

use photoproof_core::ContentHash;
use photoproof_core::collections::{CollectionRecord, CollectionStatus, NoteEntry};
use tauri::{AppHandle, Emitter, Runtime};

use super::{S, parse_hash};
use crate::dto::{CollectionDto, CollectionNoteDto, GridItem};
use crate::error::{CmdError, CmdResult};
use crate::state::App;

fn dto(rec: CollectionRecord) -> CollectionDto {
    CollectionDto {
        id: rec.id,
        name: rec.name,
        description: rec.description,
        status: rec.status.as_str().to_owned(),
        created_ts: rec.created_ts,
        updated_ts: rec.updated_ts,
        member_count: rec.member_count,
        note_count: rec.note_count,
    }
}

fn note_dto(n: NoteEntry) -> CollectionNoteDto {
    CollectionNoteDto {
        id: n.id,
        ts: n.ts,
        text: n.text,
    }
}

fn snapshot(app: &App) -> CmdResult<Vec<CollectionDto>> {
    Ok(app.collections.list()?.into_iter().map(dto).collect())
}

/// Every mutation emits the fresh full snapshot — collections number in
/// the tens, so snapshot payloads stay trivially small and the frontend
/// never reconciles deltas.
///
/// WHY the lock spans snapshot-read AND emit: commands run on independent
/// blocking threads, so two concurrent mutations could otherwise read
/// snapshots in one order and emit them in the other — the last-delivered
/// snapshot would then be the STALE one, and (since the frontend never
/// reconciles deltas) every window would render stale state until the next
/// mutation. Reading the snapshot inside the lock, after the mutation
/// committed, guarantees each emitted snapshot is current as of its emit
/// and emits leave in snapshot order.
fn emit_collections_changed<R: Runtime>(app: &App, handle: &AppHandle<R>) {
    static EMIT_ORDER: Mutex<()> = Mutex::new(());
    let _ordered = EMIT_ORDER.lock().expect("collections emit mutex");
    if let Ok(list) = snapshot(app) {
        let _ = handle.emit("collections-changed", list);
    }
}

fn parse_hashes(hashes: &[String]) -> CmdResult<Vec<ContentHash>> {
    hashes.iter().map(|h| parse_hash(h)).collect()
}

#[tauri::command]
pub async fn list_collections(app: S<'_>) -> CmdResult<Vec<CollectionDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || snapshot(&app))
        .await
        .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[tauri::command]
pub async fn create_collection<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    name: String,
    description: Option<String>,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let rec = app.collections.create(
            &name,
            description.as_deref().unwrap_or(""),
            photoproof_core::UtcMillis::now(),
        )?;
        emit_collections_changed(&app, &handle);
        Ok(dto(rec))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[tauri::command]
pub async fn rename_collection<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    name: String,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.collections
            .rename(&id, &name, photoproof_core::UtcMillis::now())?;
        emit_collections_changed(&app, &handle);
        Ok(dto(app.collections.get(&id)?))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[tauri::command]
pub async fn set_collection_status<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    status: String,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let status = CollectionStatus::parse(&status)?;
        app.collections
            .set_status(&id, status, photoproof_core::UtcMillis::now())?;
        emit_collections_changed(&app, &handle);
        Ok(dto(app.collections.get(&id)?))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[tauri::command]
pub async fn set_collection_description<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    description: String,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.collections
            .set_description(&id, &description, photoproof_core::UtcMillis::now())?;
        emit_collections_changed(&app, &handle);
        Ok(dto(app.collections.get(&id)?))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Append a note (§10.1: append-only — no edit or delete exists).
#[tauri::command]
pub async fn add_collection_note<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    text: String,
) -> CmdResult<CollectionNoteDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let note = app
            .collections
            .add_note(&id, &text, photoproof_core::UtcMillis::now())?;
        // note_count is part of every snapshot row, so notes announce too.
        emit_collections_changed(&app, &handle);
        Ok(note_dto(note))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Notes in id order (ULID order = time order).
#[tauri::command]
pub async fn collection_notes(app: S<'_>, id: String) -> CmdResult<Vec<CollectionNoteDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(app
            .collections
            .notes(&id)?
            .into_iter()
            .map(note_dto)
            .collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Add images by explicit hash list. Returns the number of NEWLY opened
/// membership intervals (already-current members are skipped, §10.1).
#[tauri::command]
pub async fn add_to_collection<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    hashes: Vec<String>,
) -> CmdResult<usize> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let parsed = parse_hashes(&hashes)?;
        let added = app
            .collections
            .add_images(&id, &parsed, photoproof_core::UtcMillis::now())?;
        if added > 0 {
            emit_collections_changed(&app, &handle);
        }
        Ok(added)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Remove images by explicit hash list: closes the open intervals —
/// evented, never destructive; history stays queryable (§10.1). Returns
/// the number of images whose membership was closed.
#[tauri::command]
pub async fn remove_from_collection<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    id: String,
    hashes: Vec<String>,
) -> CmdResult<usize> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let parsed = parse_hashes(&hashes)?;
        let removed =
            app.collections
                .remove_images(&id, &parsed, photoproof_core::UtcMillis::now())?;
        if removed > 0 {
            emit_collections_changed(&app, &handle);
        }
        Ok(removed)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Current members as grid rows (the rail's Collections tab: clicking a
/// collection shows its members in the grid, exactly as folder selection
/// drives it). Members the library index does not know — membership is
/// evented and outlives files (§10.1) — are skipped: nothing to render.
#[tauri::command]
pub async fn list_collection_members(app: S<'_>, id: String) -> CmdResult<Vec<GridItem>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let members = app.collections.current_members(&id)?;
        let items = app.library.list_images(&members)?;
        Ok(items.into_iter().map(super::library::grid_item).collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Collections this image is CURRENTLY a member of (the Look's membership
/// row and the grid's context menu checkmarks).
#[tauri::command]
pub async fn collections_for_image(app: S<'_>, hash: String) -> CmdResult<Vec<CollectionDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let h = parse_hash(&hash)?;
        Ok(app
            .collections
            .collections_for_image(&h)?
            .into_iter()
            .map(dto)
            .collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

// ---------------------------------------------------------------------------
// Tests — the real command path over a mock Tauri app (the library.rs
// pattern): mutations emit `collections-changed` snapshots.
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

    #[test]
    fn collection_lifecycle_through_the_command_layer() {
        let (_tmp, tauri_app) = mock_app();
        let payloads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = payloads.clone();
        tauri_app.listen_any("collections-changed", move |e| {
            sink.lock()
                .expect("payload mutex")
                .push(e.payload().to_owned());
        });
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();

        let created = tauri::async_runtime::block_on(create_collection(
            state.clone(),
            handle.clone(),
            "Quiet Hours".into(),
            None,
        ))
        .expect("create_collection");
        assert_eq!(created.status, "active");

        let img = "ab".repeat(32);
        let added = tauri::async_runtime::block_on(add_to_collection(
            state.clone(),
            handle.clone(),
            created.id.clone(),
            vec![img.clone()],
        ))
        .expect("add_to_collection");
        assert_eq!(added, 1);

        let memberships =
            tauri::async_runtime::block_on(collections_for_image(state.clone(), img.clone()))
                .expect("collections_for_image");
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].member_count, 1);

        // The members grid read skips hashes the library index does not
        // know (membership is evented and outlives files, §10.1): this
        // member was never ingested, so there is nothing to render — and
        // the command must not error over it.
        let grid = tauri::async_runtime::block_on(list_collection_members(
            state.clone(),
            created.id.clone(),
        ))
        .expect("list_collection_members");
        assert!(grid.is_empty(), "un-ingested members render no grid rows");

        let removed = tauri::async_runtime::block_on(remove_from_collection(
            state.clone(),
            handle.clone(),
            created.id.clone(),
            vec![img.clone()],
        ))
        .expect("remove_from_collection");
        assert_eq!(removed, 1);
        assert!(
            tauri::async_runtime::block_on(collections_for_image(state.clone(), img))
                .expect("collections_for_image")
                .is_empty()
        );

        let renamed = tauri::async_runtime::block_on(rename_collection(
            state.clone(),
            handle.clone(),
            created.id.clone(),
            "Quiet Hours II".into(),
        ))
        .expect("rename_collection");
        assert_eq!(renamed.name, "Quiet Hours II");

        let done = tauri::async_runtime::block_on(set_collection_status(
            state.clone(),
            handle.clone(),
            created.id.clone(),
            "done".into(),
        ))
        .expect("set_collection_status");
        assert_eq!(done.status, "done");

        // Unknown status surfaces as an error, never a silent default.
        assert!(
            tauri::async_runtime::block_on(set_collection_status(
                state.clone(),
                handle.clone(),
                created.id.clone(),
                "archived".into(),
            ))
            .is_err()
        );

        // The §10.1 notes/description surface (P7.3 follow-up to the
        // original seven commands).
        let described = tauri::async_runtime::block_on(set_collection_description(
            state.clone(),
            handle.clone(),
            created.id.clone(),
            "the fog series".into(),
        ))
        .expect("set_collection_description");
        assert_eq!(described.description, "the fog series");

        let noted = tauri::async_runtime::block_on(add_collection_note(
            state.clone(),
            handle,
            created.id.clone(),
            "started during the cold snap".into(),
        ))
        .expect("add_collection_note");
        let notes =
            tauri::async_runtime::block_on(collection_notes(state.clone(), created.id.clone()))
                .expect("collection_notes");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, noted.id);
        assert_eq!(notes[0].text, "started during the cold snap");

        let listed =
            tauri::async_runtime::block_on(list_collections(state)).expect("list_collections");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].member_count, 0, "membership was closed");
        assert_eq!(listed[0].note_count, 1);

        // Every mutation emitted a snapshot: create, add, remove, rename,
        // set-status, set-description, add-note (the failed set-status
        // emits nothing).
        let got = payloads.lock().expect("payload mutex");
        assert_eq!(got.len(), 7, "one snapshot per committed mutation");
        assert!(got[4].contains("Quiet Hours II"));
        assert!(got[6].contains("\"noteCount\":1"));
    }
}
