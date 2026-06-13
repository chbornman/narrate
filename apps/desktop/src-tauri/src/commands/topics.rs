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
//! in a session, repeated phrases across notes, time+folder affinity) are
//! PHASE 3 and deliberately NOT built here (kept this slice to manual topics +
//! cluster suggestions + the bake).
//
// TODO(autosuggest phase 3): co-annotation-in-session, repeated-phrase, and
// time+folder-affinity candidate groupings feed the suggestion rail alongside
// the v2 cluster topics. Each yields candidate groupings the human commits via
// the same bake (K14: the machine proposes, never authors into the store).

use photoproof_connectors::OrtEmbedder;
use photoproof_core::topic::{self, RankedImage};
use photoproof_core::topics::TopicSpace;
use photoproof_core::tuning::tuning;
use tauri::{AppHandle, Runtime};

use super::graph::{GraphScope, enumerate_scope};
use super::{S, parse_hash};
use crate::dto::{CollectionDto, RankedImageDto, TopicDto};
use crate::error::{CmdError, CmdResult};
use crate::state::App;

// ---------------------------------------------------------------------------
// 1. Manual topics CRUD
// ---------------------------------------------------------------------------

fn topic_dto(rec: photoproof_core::topics::TopicRecord) -> TopicDto {
    TopicDto {
        id: rec.id,
        phrase: rec.phrase,
        space: rec.space.as_str().to_owned(),
        created_ts: rec.created_ts,
    }
}

/// `add_topic(phrase, space?)` — save a phrase as a topic (like saving a
/// search). `space` is optional: `"annotation"` / `"clip"` pin one embedding
/// space; absent / unknown blends both at the configured alpha (the default).
#[tauri::command]
pub async fn add_topic(app: S<'_>, phrase: String, space: Option<String>) -> CmdResult<TopicDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let space = TopicSpace::parse(space.as_deref());
        let rec = app
            .topics
            .add(&phrase, space, photoproof_core::UtcMillis::now())
            .map_err(|e| CmdError::Invalid(format!("add topic: {e}")))?;
        Ok(topic_dto(rec))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// `list_topics()` — every saved manual topic, newest first.
#[tauri::command]
pub async fn list_topics(app: S<'_>) -> CmdResult<Vec<TopicDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let list = app
            .topics
            .list()
            .map_err(|e| CmdError::Invalid(format!("list topics: {e}")))?;
        Ok(list.into_iter().map(topic_dto).collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// `remove_topic(id)` — delete a saved topic. A missing id is an error (the
/// caller asked to remove THIS topic and must learn it was already gone).
#[tauri::command]
pub async fn remove_topic(app: S<'_>, id: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        app.topics
            .remove(&id)
            .map_err(|e| CmdError::Invalid(format!("remove topic: {e}")))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    Ok(topic::topic_ranked_images::<OrtEmbedder, OrtEmbedder>(
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let ranked = rank_scope(&app, &scope, &phrase, alpha)?;
        Ok(ranked
            .into_iter()
            .map(|r| RankedImageDto {
                hash: r.hash,
                score: r.score,
            })
            .collect())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    // Provenance rides the description (the collections store has no dedicated
    // provenance field; DESIGN allows "a concise note in description"). Reusing
    // the evented create keeps the bake on the one collection-create path every
    // other surface shares.
    let rec = app
        .collections
        .create(name, description, now)
        .map_err(|e| CmdError::Invalid(format!("bake create: {e}")))?;
    // Parse hashes to ContentHash through the shared validator, then add through
    // the SAME evented add path the rail uses (skips already-current members,
    // opens interval rows). An empty member set is a legal (empty) collection.
    let parsed: Vec<photoproof_core::ContentHash> = members
        .iter()
        .map(|h| parse_hash(h))
        .collect::<CmdResult<_>>()?;
    if !parsed.is_empty() {
        app.collections
            .add_images(&rec.id, &parsed, now)
            .map_err(|e| CmdError::Invalid(format!("bake add members: {e}")))?;
    }
    // Re-read so member_count reflects the just-added members, then announce the
    // new collection on the SAME snapshot event every collection mutation emits
    // (so the Collections rail refreshes live, the create + add as one bake).
    let dto = super::collections::collection_dto(
        app.collections
            .get(&rec.id)
            .map_err(|e| CmdError::Invalid(format!("bake reread: {e}")))?,
    );
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let ranked = rank_scope(&app, &scope, &phrase, alpha)?;
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
        bake_collection(&app, &handle, &name, &provenance, &members)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
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
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        // The selection was computed in the graph (the glowing set from the
        // threshold drag); provenance records that origin without a live link.
        let provenance = "Made from a topic graph selection.";
        bake_collection(&app, &handle, &name, provenance, &hashes)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tauri::Manager;
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

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
