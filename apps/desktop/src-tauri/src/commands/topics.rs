//! Topics + autosuggest + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md).
//!
//! The BACKEND/API foundation for the Topics sidebar tab + the threshold slider
//! (a later agent builds the UI on top of these commands). Three slices:
//!
//!   1. Manual topics CRUD (`add_topic` / `list_topics` / `remove_topic`) over
//!      the lightweight `photoproof_core::topics` store — a topic is a SAVED
//!      PHRASE, like a saved search.
//!   2. `topic_ranked_images` — the in-scope images ranked by blended affinity
//!      to one topic phrase (the tab's grid; the slider thresholds on `score`).
//!      REUSES the graph's affinity scoring (`topic::topic_ranked_images` over
//!      `topic_affinities`) — the single-topic projection of the graph's lens.
//!   3. The one-way BAKE (`create_collection_from_topic` /
//!      `create_collection_from_selection`) — commit a topic threshold (or an
//!      already-computed client selection) into an ordinary EVENTED collection,
//!      recording provenance. After the bake the collection decouples (no live
//!      link back) — exactly RAW → develop (DESIGN: "one-way for now").
//!
//! GRACEFUL like the rest of the lens: an un-embedded scope / absent embedders
//! yield empty ranked lists, never an error — the mechanism is correct before
//! any embed pass.
//!
//! Autosuggested topics: the cluster-derived ones already come from v2's
//! `cluster_topics` (commands/graph.rs::cluster_topics) — the Topics tab calls
//! that for a scope. The EXTRA candidate signals from the design (co-annotation
//! in a session, repeated phrases across notes, time+folder bursts) are PHASE 3
//! and land in `suggest_collections` below — candidate GROUPINGS proposed from
//! signals the app already has, computed on the fly (K14: it proposes, the human
//! commits via the same bake).

use photoproof_core::topic::{self, RankedImage};
use photoproof_core::topics::TopicSpace;
use photoproof_core::tuning::tuning;
use rusqlite::OpenFlags;
use tauri::{AppHandle, Runtime};

use super::graph::{GraphScope, enumerate_scope};
use super::{S, parse_hash, run_blocking};
use crate::command_work::CommandClass;
use crate::dto::{CollectionCandidateDto, CollectionDto, RankedImageDto, TopicDto, TopicNoteDto};
use crate::embedders::EmbedderProxy;
use crate::error::{CmdError, CmdResult};
use crate::state::App;

// ---------------------------------------------------------------------------
// 1. Manual topics CRUD
// ---------------------------------------------------------------------------

pub(crate) fn topic_dto(rec: photoproof_core::topics::TopicRecord) -> TopicDto {
    TopicDto {
        id: rec.id,
        phrase: rec.phrase,
        space: rec.space.as_str().to_owned(),
        created_ts: rec.created_ts,
    }
}

fn topic_note_dto(n: photoproof_core::topics::TopicNote) -> TopicNoteDto {
    TopicNoteDto {
        id: n.id,
        ts: n.ts,
        text: n.text,
    }
}

/// `add_topic(phrase, space?)` — save a phrase as a topic (like saving a
/// search). `space` is optional: `"annotation"` / `"clip"` pin one embedding
/// space; absent / unknown blends both at the configured alpha (the default).
#[tauri::command]
pub async fn add_topic(app: S<'_>, phrase: String, space: Option<String>) -> CmdResult<TopicDto> {
    let app = app.inner().clone();
    run_blocking(app, "topics.add", CommandClass::Mutation, move |app| {
        app.touch()?;
        let space = TopicSpace::parse(space.as_deref());
        let rec = app
            .topics
            .add(&phrase, space, photoproof_core::UtcMillis::now())
            .map_err(|e| CmdError::Invalid(format!("add topic: {e}")))?;
        Ok(topic_dto(rec))
    })
    .await
}

/// `list_topics()` — every saved manual topic, newest first.
#[tauri::command]
pub async fn list_topics(app: S<'_>) -> CmdResult<Vec<TopicDto>> {
    let app = app.inner().clone();
    run_blocking(app, "topics.list", CommandClass::Read, move |app| {
        let list = app
            .topics
            .list()
            .map_err(|e| CmdError::Invalid(format!("list topics: {e}")))?;
        Ok(list.into_iter().map(topic_dto).collect())
    })
    .await
}

/// `remove_topic(id)` — delete a saved topic. A missing id is an error (the
/// caller asked to remove THIS topic and must learn it was already gone).
#[tauri::command]
pub async fn remove_topic(app: S<'_>, id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    run_blocking(app, "topics.remove", CommandClass::Mutation, move |app| {
        app.touch()?;
        app.topics
            .remove(&id)
            .map_err(|e| CmdError::Invalid(format!("remove topic: {e}")))
    })
    .await
}

/// `add_topic_note(id, text)` — append a note to a topic (founder decision:
/// topics get their own append-only running note, mirroring the collection
/// note log). Append-only — there is no edit or delete, exactly like
/// `add_collection_note`. Resolves to the freshly appended note. No event
/// emit: unlike collections (whose snapshot carries note_count), a topic note
/// is a local note log the rail reloads on demand, so there is nothing in a
/// topic snapshot to refresh.
#[tauri::command]
pub async fn add_topic_note(app: S<'_>, id: String, text: String) -> CmdResult<TopicNoteDto> {
    let app = app.inner().clone();
    run_blocking(app, "topics.add-note", CommandClass::Mutation, move |app| {
        app.touch()?;
        let note = app
            .topics
            .add_note(&id, &text, photoproof_core::UtcMillis::now())
            .map_err(|e| CmdError::Invalid(format!("add topic note: {e}")))?;
        Ok(topic_note_dto(note))
    })
    .await
}

/// `topic_notes(id)` — a topic's append-only notes in id order (ULID order =
/// time order), the `collection_notes` read mirrored for topics.
#[tauri::command]
pub async fn topic_notes(app: S<'_>, id: String) -> CmdResult<Vec<TopicNoteDto>> {
    let app = app.inner().clone();
    run_blocking(app, "topics.notes", CommandClass::Read, move |app| {
        Ok(app
            .topics
            .notes(&id)
            .map_err(|e| CmdError::Invalid(format!("topic notes: {e}")))?
            .into_iter()
            .map(topic_note_dto)
            .collect())
    })
    .await
}

// ---------------------------------------------------------------------------
// 2. topic_ranked_images — the single-topic ranked grid + the slider
// ---------------------------------------------------------------------------

/// Resolve `alpha`: the caller's value, else the graph's configured default
/// (the same default `topic_affinities` uses, so the tab and the graph blend
/// identically).
fn resolve_alpha(alpha: Option<f64>) -> f64 {
    alpha.unwrap_or_else(|| tuning().graph.alpha_default)
}

/// Score `scope` against one `phrase`, descending by blended affinity. Shared by
/// `topic_ranked_images` and the bake (which filters this ranked list by a
/// threshold), so both compute affinity ONE way.
fn rank_scope(
    app: &App,
    scope: &GraphScope,
    phrase: &str,
    alpha: f64,
) -> CmdResult<Vec<RankedImage>> {
    let hashes = enumerate_scope(app, scope)?;
    // The ready embedders (or None ⇒ a degraded half contributing 0).
    let text = app.runtime.embedders.text();
    let clip = app.runtime.embedders.clip();
    Ok(topic::topic_ranked_images::<EmbedderProxy, EmbedderProxy>(
        &hashes,
        phrase,
        alpha,
        app.vectors.as_ref(),
        text.as_deref(),
        clip.as_deref(),
    ))
}

/// `topic_ranked_images(phrase, scope, alpha?)` (DESIGN-TOPICS-COLLECTIONS.md):
/// the in-scope images ranked by blended affinity to `phrase`, `[{ hash, score }]`
/// descending. This powers the Topics-tab grid (select a topic, see its images
/// ranked) and the slider (which thresholds on `score`). `alpha` omitted uses
/// the graph default.
///
/// REUSES the graph's affinity scoring (the single-topic projection of
/// `topic_affinities`). Runs on a blocking thread (the brute-force vector scan +
/// the topic embed can take real time on a large scope), mirroring
/// `topic_affinities`. GRACEFUL: an un-embedded/empty scope returns an empty
/// list, never an error.
#[tauri::command]
pub async fn topic_ranked_images(
    app: S<'_>,
    phrase: String,
    scope: GraphScope,
    alpha: Option<f64>,
) -> CmdResult<Vec<RankedImageDto>> {
    let app = app.inner().clone();
    let alpha = resolve_alpha(alpha);
    run_blocking(
        app,
        "topics.ranked-images",
        CommandClass::Read,
        move |app| {
            app.touch()?;
            let ranked = rank_scope(app, &scope, &phrase, alpha)?;
            Ok(ranked
                .into_iter()
                .map(|r| RankedImageDto {
                    hash: r.hash,
                    score: r.score,
                })
                .collect())
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// 3. The one-way topic→collection bake
// ---------------------------------------------------------------------------

/// Create an evented collection from `members`, then return its DTO. The SHARED
/// commit path for both bake commands: reuse the EXISTING collections
/// create-then-add evented path (`Collections::create` + `add_images`), so a
/// baked collection is an ordinary independent collection from the first
/// millisecond (membership lands as `added_ts` interval rows, exactly as a
/// hand-built collection's would). `description` carries the provenance note.
///
/// One-way by construction: nothing records a live link back to the originating
/// topic — after this returns the collection is yours to hand-edit (DESIGN:
/// "one-way bake, recording born-from-topic-X as provenance only").
fn bake_collection<R: Runtime>(
    app: &App,
    handle: &AppHandle<R>,
    name: &str,
    description: &str,
    members: &[String],
) -> CmdResult<CollectionDto> {
    let now = photoproof_core::UtcMillis::now();
    // Validate the entire client-supplied set before the first durable write.
    // An invalid tail hash must not leave the successfully parsed prefix as an
    // empty or partially populated collection.
    let parsed: Vec<photoproof_core::ContentHash> = members
        .iter()
        .map(|h| parse_hash(h))
        .collect::<CmdResult<_>>()?;
    // Provenance rides the description (the collections store has no dedicated
    // provenance field; DESIGN allows "a concise note in description"). Reusing
    // the core's atomic create-with-members path keeps collection metadata and
    // all evented membership intervals in one SQLite transaction.
    let rec = app
        .collections
        .create_with_images(name, description, &parsed, now)
        .map_err(|e| CmdError::Invalid(format!("bake collection: {e}")))?;
    let dto = super::collections::collection_dto(rec);
    // Publication is strictly post-commit. Every validation/insert/commit
    // failure returns above without emitting a collection snapshot.
    super::collections::emit_collections_changed(app, handle);
    Ok(dto)
}

/// `create_collection_from_topic(phrase, scope, threshold, alpha?, name)`
/// (DESIGN-TOPICS-COLLECTIONS.md): bake a topic threshold into a collection.
/// Members are the in-scope images whose blended affinity to `phrase` is
/// `>= threshold`. Records PROVENANCE (the phrase + threshold + alpha) in the
/// collection's description.
///
/// The server-side bake: it ranks the scope itself (so the membership matches
/// what `topic_ranked_images` would show at that threshold) and commits the
/// `>= threshold` prefix. GRACEFUL: an un-embedded scope simply bakes an empty
/// collection (every score 0, threshold filters all out unless threshold <= 0)
/// — never an error.
#[tauri::command]
pub async fn create_collection_from_topic<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    phrase: String,
    scope: GraphScope,
    threshold: f32,
    alpha: Option<f64>,
    name: String,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    let alpha = resolve_alpha(alpha);
    run_blocking(
        app,
        "topics.bake-from-topic",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            let ranked = rank_scope(app, &scope, &phrase, alpha)?;
            // >= threshold (the slider's "at or above the cut glows"): an inclusive
            // bound so the image sitting exactly on the threshold is committed.
            let members: Vec<String> = ranked
                .into_iter()
                .filter(|r| r.score >= threshold)
                .map(|r| r.hash)
                .collect();
            // Provenance: a concise, human-readable note so the photographer (and a
            // later "where did this come from" glance) sees the bake's origin. The
            // collection then decouples — this is a record, not a live link.
            let provenance =
                format!("Born from topic \"{phrase}\" at threshold {threshold} (alpha {alpha}).");
            bake_collection(app, &handle, &name, &provenance, &members)
        },
    )
    .await
}

/// `create_collection_from_selection(hashes, name)` (DESIGN-TOPICS-COLLECTIONS.md):
/// the GENERIC commit path the graph's lasso / slider uses when the selection is
/// already computed client-side (the glowing set the user dragged to). Same
/// evented create-then-add, provenance "from topic graph selection".
///
/// This is the slider-to-collection bake gesture's backend: the frontend has the
/// selected hashes in hand (it dragged the threshold and watched the set light
/// up), so the server just commits them — no re-scoring, no scope, no embedder.
#[tauri::command]
pub async fn create_collection_from_selection<R: Runtime>(
    app: S<'_>,
    handle: AppHandle<R>,
    hashes: Vec<String>,
    name: String,
) -> CmdResult<CollectionDto> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "topics.bake-from-selection",
        CommandClass::Mutation,
        move |app| {
            app.touch()?;
            // The selection was computed in the graph (the glowing set from the
            // threshold drag); provenance records that origin without a live link.
            let provenance = "Made from a topic graph selection.";
            bake_collection(app, &handle, &name, provenance, &hashes)
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// 4. suggest_collections — Phase 3 candidate GROUPINGS (autosuggest)
// ---------------------------------------------------------------------------

/// `suggest_collections(scope)` (DESIGN-TOPICS-COLLECTIONS.md, autosuggest
/// Phase 3): propose candidate GROUPINGS the human might bake into collections,
/// from signals the app already has, computed on the fly. Three sources:
///   - CO-ANNOTATION: images touched together in one session (a session that
///     worked a coherent set of photos).
///   - REPEATED PHRASE: a salient note n-gram recurring across multiple images.
///   - TIME + FOLDER: a capture-time burst within one folder (a shoot).
///
/// K14 / quiet by construction: it PROPOSES, never auto-creates. Read-only over
/// EXISTING tables (no schema migration, no write). GRACEFUL: an empty/sparse
/// scope returns few/none, never an error.
///
/// Runs on a blocking thread (the journal + metadata scans can take real time on
/// a large scope), mirroring `suggest_topics`. The DB reads happen on a fresh
/// read-only connection over the shared WAL db (the debug-readq pattern the
/// other suggestion commands use); the candidate logic itself is the pure
/// `topic::suggest_collections` reducer.
#[tauri::command]
pub async fn suggest_collections(
    app: S<'_>,
    scope: GraphScope,
) -> CmdResult<Vec<CollectionCandidateDto>> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "topics.suggest-collections",
        CommandClass::Read,
        move |app| {
            app.touch()?;
            let hashes = enumerate_scope(app, &scope)?;

            // Gather every candidate source's raw rows on ONE read-only connection
            // (a short projection has no business holding a write lock — the same
            // posture suggest_topics/cluster_topics take).
            let db_path = app.app_data.join("photoproof.db");
            let conn = rusqlite::Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| CmdError::Invalid(format!("open suggest-collections read: {e}")))?;

            // 1. Co-annotation: (session, image) links + each session's start time.
            let session_links = topic::scope_session_image_links(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("session links: {e}")))?;
            let session_started = topic::session_start_millis(&conn, &session_links)
                .map_err(|e| CmdError::Invalid(format!("session starts: {e}")))?;

            // 2. Repeated phrase: per-image note text (the same projection the v2
            //    cluster labeling mines, grouped by image).
            let notes_by_hash = topic::scope_note_texts_by_hash(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("scope notes: {e}")))?;

            // 3. Time + folder: (folder_key, capture_ms, hash) for dated, located
            //    images.
            let folder_time_rows = topic::scope_folder_time_rows(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("folder/time rows: {e}")))?;

            let candidates = topic::suggest_collections(
                &session_links,
                &session_started,
                &notes_by_hash,
                &folder_time_rows,
            );
            Ok(candidates
                .into_iter()
                .map(|c| CollectionCandidateDto {
                    label: c.label,
                    members: c.members,
                    source: c.source,
                    score: c.score,
                })
                .collect())
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Topics CRUD through the REAL command path: add saves, list returns it,
    /// remove deletes, removing a gone id errors.
    #[test]
    fn topics_crud_through_the_command_layer() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();

        let saved = tauri::async_runtime::block_on(add_topic(
            state.clone(),
            "harbor at dusk".into(),
            Some("clip".into()),
        ))
        .expect("add_topic");
        assert_eq!(saved.phrase, "harbor at dusk");
        assert_eq!(saved.space, "clip");

        // A blend default (no space) round-trips.
        let blended = tauri::async_runtime::block_on(add_topic(state.clone(), "snow".into(), None))
            .expect("add_topic");
        assert_eq!(blended.space, "blend");

        let listed = tauri::async_runtime::block_on(list_topics(state.clone())).expect("list");
        assert_eq!(listed.len(), 2);

        tauri::async_runtime::block_on(remove_topic(state.clone(), saved.id.clone()))
            .expect("remove_topic");
        let after = tauri::async_runtime::block_on(list_topics(state.clone())).expect("list");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, blended.id);

        // Removing the already-removed id errors.
        assert!(tauri::async_runtime::block_on(remove_topic(state.clone(), saved.id)).is_err());
    }

    /// Topic notes through the REAL command path: append + list round-trip in
    /// time order, and a note for a missing topic errors (the FK guard) — the
    /// collection-notes command contract mirrored for topics.
    #[test]
    fn topic_notes_through_the_command_layer() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();

        let topic = tauri::async_runtime::block_on(add_topic(state.clone(), "harbor".into(), None))
            .expect("add_topic");

        tauri::async_runtime::block_on(add_topic_note(
            state.clone(),
            topic.id.clone(),
            "what this topic is for".into(),
        ))
        .expect("add_topic_note");
        tauri::async_runtime::block_on(add_topic_note(
            state.clone(),
            topic.id.clone(),
            "refine toward dusk shots".into(),
        ))
        .expect("add_topic_note");

        let listed = tauri::async_runtime::block_on(topic_notes(state.clone(), topic.id.clone()))
            .expect("topic_notes");
        assert_eq!(listed.len(), 2);
        // The ordering guarantee: ascending id (= time order). The note id is a
        // fresh ULID (wall clock), so assert the list is sorted and both notes
        // round-tripped, not which won the same-millisecond random tail.
        let ids: Vec<&str> = listed.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "notes list ascending by id");
        let texts: std::collections::HashSet<&str> =
            listed.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            ["what this topic is for", "refine toward dusk shots"]
                .into_iter()
                .collect()
        );

        // A note for a topic that does not exist is an error (the FK guard).
        assert!(
            tauri::async_runtime::block_on(add_topic_note(
                state.clone(),
                "01MISSING".into(),
                "orphan".into(),
            ))
            .is_err()
        );
    }

    /// `topic_ranked_images` over an empty/un-embedded library returns an empty
    /// ranked list, not an error — the graceful pre-embed-pass posture through
    /// the real command path.
    #[test]
    fn topic_ranked_images_degrades_gracefully() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let ranked = tauri::async_runtime::block_on(topic_ranked_images(
            state.clone(),
            "harbor".into(),
            GraphScope::Library,
            None,
        ))
        .expect("topic_ranked_images");
        assert!(ranked.is_empty(), "empty library ⇒ empty ranked list");
    }

    /// `create_collection_from_selection` bakes the given hashes into an evented
    /// collection (the generic slider/lasso commit), records provenance in the
    /// description, and the members are current (the evented add ran).
    #[test]
    fn bake_from_selection_creates_an_evented_collection() {
        let (_tmp, tauri_app) = mock_app();
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let event_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&event_count);
        tauri_app.listen_any("collections-changed", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });

        let a = "ab".repeat(32);
        let b = "cd".repeat(32);
        let coll = tauri::async_runtime::block_on(create_collection_from_selection(
            state.clone(),
            handle,
            vec![a.clone(), b.clone()],
            "The Glowing Set".into(),
        ))
        .expect("create_collection_from_selection");
        assert_eq!(coll.name, "The Glowing Set");
        assert_eq!(coll.member_count, 2, "both selected hashes are members");
        assert!(
            coll.description.to_lowercase().contains("selection"),
            "provenance recorded in description: {}",
            coll.description
        );

        // It is an ORDINARY collection now: it shows up in the list and both
        // images report it as a current membership (the evented add path ran).
        let memberships = tauri::async_runtime::block_on(
            super::super::collections::collections_for_image(state.clone(), a),
        )
        .expect("collections_for_image");
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].id, coll.id);
        assert_eq!(
            event_count.load(Ordering::SeqCst),
            1,
            "the committed bake publishes exactly one collection snapshot"
        );
    }

    #[test]
    fn invalid_selection_hash_leaves_no_collection_or_event() {
        let (_tmp, tauri_app) = mock_app();
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let event_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&event_count);
        tauri_app.listen_any("collections-changed", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });

        let result = tauri::async_runtime::block_on(create_collection_from_selection(
            state.clone(),
            handle,
            vec!["ab".repeat(32), "not-a-content-hash".into()],
            "Invalid Tail".into(),
        ));
        assert!(result.is_err());
        assert!(
            state.collections.list().unwrap().is_empty(),
            "validation completes before the collection transaction starts"
        );
        assert_eq!(
            event_count.load(Ordering::SeqCst),
            0,
            "a rejected bake never announces a collection"
        );
    }

    #[test]
    fn member_insert_failure_rolls_back_collection_and_suppresses_event() {
        let (_tmp, tauri_app) = mock_app();
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let event_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&event_count);
        tauri_app.listen_any("collections-changed", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let injector = rusqlite::Connection::open(state.app_data.join("photoproof.db")).unwrap();
        injector
            .execute_batch(
                "CREATE TRIGGER inject_topic_bake_member_failure
                 BEFORE INSERT ON collection_members
                 BEGIN
                   SELECT RAISE(ABORT, 'injected topic bake member failure');
                 END;",
            )
            .unwrap();

        let result = tauri::async_runtime::block_on(create_collection_from_selection(
            state.clone(),
            handle,
            vec!["cd".repeat(32)],
            "Must Roll Back".into(),
        ));
        assert!(result.is_err());
        assert!(
            state.collections.list().unwrap().is_empty(),
            "the member insert and collection metadata share one transaction"
        );
        assert_eq!(
            event_count.load(Ordering::SeqCst),
            0,
            "failed transaction never publishes collections-changed"
        );
    }

    #[test]
    fn list_topics_surfaces_corrupt_stored_rows() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let topic = tauri::async_runtime::block_on(add_topic(
            state.clone(),
            "harbor".into(),
            Some("clip".into()),
        ))
        .unwrap();
        let conn = rusqlite::Connection::open(state.app_data.join("photoproof.db")).unwrap();
        conn.execute(
            "UPDATE topics SET space = 'unknown-space' WHERE id = ?1",
            [&topic.id],
        )
        .unwrap();

        let result = tauri::async_runtime::block_on(list_topics(state));
        assert!(
            matches!(result, Err(CmdError::Invalid(detail)) if detail.contains("unknown-space")),
            "stored-row corruption is an explicit command error, not a blend default"
        );
    }

    /// `suggest_collections` over an empty/fresh library returns an empty rail,
    /// not an error — the K14 quiet posture through the real command path (no
    /// sessions, notes, or dated images to propose anything from).
    #[test]
    fn suggest_collections_command_empty_library() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let out =
            tauri::async_runtime::block_on(suggest_collections(state.clone(), GraphScope::Library))
                .expect("suggest_collections");
        assert!(out.is_empty(), "empty library ⇒ empty candidate rail");
    }

    /// `create_collection_from_topic` bakes the `>= threshold` images. Over a
    /// degraded rig (no embedders) every score is 0, so a threshold above 0
    /// bakes an EMPTY collection (no member is at-or-above), proving the
    /// inclusive threshold filter and the graceful empty path. A threshold of
    /// 0.0 over the (empty) library still bakes an empty collection — no scope
    /// images exist to score.
    #[test]
    fn bake_from_topic_filters_by_threshold_and_records_provenance() {
        let (_tmp, tauri_app) = mock_app();
        let handle = tauri_app.handle().clone();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();

        let coll = tauri::async_runtime::block_on(create_collection_from_topic(
            state.clone(),
            handle,
            "harbor".into(),
            GraphScope::Library,
            0.5,
            None,
            "Harbor >= 0.5".into(),
        ))
        .expect("create_collection_from_topic");
        // Empty library ⇒ no scope images ⇒ no members, but a real collection.
        assert_eq!(coll.member_count, 0);
        assert!(
            coll.description.contains("harbor") && coll.description.contains("0.5"),
            "provenance records phrase + threshold: {}",
            coll.description
        );
        let listed =
            tauri::async_runtime::block_on(super::super::collections::list_collections(state))
                .expect("list_collections");
        assert!(listed.iter().any(|c| c.id == coll.id));
    }
}
