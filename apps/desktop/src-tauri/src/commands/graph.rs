//! Semantic topic-graph commands (DESIGN-SEMANTIC-GRAPH.md, v1).
//!
//! Three thin commands over `photoproof_core::topic` + the existing embedder /
//! vector-store machinery:
//!   - `topic_affinities` — per-image blended affinity to each topic anchor.
//!   - `suggest_topics`   — cheap candidate topics for the suggestion rail.
//!   - `graph_tuning`     — the GraphTuning knobs the frontend force sim reads.
//!
//! SCOPE is one of: a collection's current members, a folder's images, or the
//! WHOLE library (the deliberate scale spike — DESIGN: "point v1 at the full
//! library to feel the scale wall"). Enumeration reuses the same core reads the
//! grid feeders use (collection members / folder listing / `image_hashes`).
//!
//! GRACEFUL by construction (the whole-product M1 posture): a degraded rig (no
//! embedders, un-embedded index) returns a well-formed zeros report, never an
//! error — the mechanism is correct before any embed pass.

use std::time::Instant;

use photoproof_connectors::OrtEmbedder;
use photoproof_connectors::embedder::Embedder;
use photoproof_connectors::vector_store::{VecKind, VecSpace};
use photoproof_core::retrieval::KnnGraph;
use photoproof_core::topic::{
    self, AffinityReport, ClusterTopic, LlmSuggestions, TopicLlm, TopicSuggestion,
};
use photoproof_core::tuning::{GraphTuning, tuning};
use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};

use super::S;
use crate::error::{CmdError, CmdResult};
use crate::state::App;

/// The grid scope the graph is pointed at (mirrors the frontend `GridScope`
/// sources — folder / collection / the full-library scale spike).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphScope {
    /// A folder under a root (the grid's folder scope).
    Folder { root_id: String, folder: String },
    /// A collection's CURRENT members.
    Collection { id: String },
    /// The WHOLE library — the deliberate scale spike (DESIGN §scale).
    Library,
}

/// Resolve a scope to its in-scope image hashes (lowercase hex strings), the
/// universe `topic_affinities` scores. Reuses the SAME core reads the grid
/// feeders use, so the graph's set matches the grid's exactly.
///
/// `pub(crate)` so the topics commands (the Topics-tab ranked grid + the
/// topic→collection bake) resolve a scope identically — the tab and the graph
/// are two views of one topic set over one scope (DESIGN-TOPICS-COLLECTIONS.md).
pub(crate) fn enumerate_scope(app: &App, scope: &GraphScope) -> CmdResult<Vec<String>> {
    let hashes = match scope {
        GraphScope::Folder { root_id, folder } => app
            .library
            .list_folder(root_id, folder)
            .map_err(|e| CmdError::Invalid(format!("folder scope: {e}")))?
            .into_iter()
            .map(|f| f.hash.as_str().to_owned())
            .collect(),
        GraphScope::Collection { id } => app
            .collections
            .current_members(id)
            .map_err(|e| CmdError::Invalid(format!("collection scope: {e}")))?
            .into_iter()
            .map(|h| h.as_str().to_owned())
            .collect(),
        GraphScope::Library => app
            .library
            .image_hashes()
            .map_err(|e| CmdError::Invalid(format!("library scope: {e}")))?
            .into_iter()
            .map(|h| h.as_str().to_owned())
            .collect(),
    };
    Ok(hashes)
}

/// One in-scope image and its top-k semantically-similar neighbors, the sparse
/// graph the visualizer's force layout reads to pull alike photos together.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageNeighbors {
    pub hash: String,
    pub neighbors: Vec<Neighbor>,
}

/// A single semantic-attraction edge: a neighbor image and how strongly it
/// should pull (cosine similarity in roughly [0,1]; 0 == no pull).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Neighbor {
    pub hash: String,
    pub weight: f32,
}

/// `topic_affinities(scope, topics, alpha)` (DESIGN-SEMANTIC-GRAPH.md): score
/// every in-scope image against every topic, blended by `alpha` (looks vs
/// said). `alpha` omitted uses the GraphTuning default.
///
/// Runs on a blocking thread (the brute-force vector scan + topic embeds can
/// take real time on a large scope — see the full-library scale spike) so it
/// never stalls the UI loop, mirroring `find_similar`. The scale-spike
/// measurements (node count, scan time) are LOGGED here, not silently swallowed
/// — DESIGN wants the wall felt, not hidden.
#[tauri::command]
pub async fn topic_affinities(
    app: S<'_>,
    scope: GraphScope,
    topics: Vec<String>,
    alpha: Option<f64>,
) -> CmdResult<AffinityReport> {
    let app = app.inner().clone();
    let alpha = alpha.unwrap_or_else(|| tuning().graph.alpha_default);
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let scope_started = Instant::now();
        let hashes = enumerate_scope(&app, &scope)?;
        let scope_ms = scope_started.elapsed().as_millis();

        // The ready embedders (or None ⇒ a degraded half that contributes 0).
        let text = app.runtime.embedders.text();
        let clip = app.runtime.embedders.clip();

        let affinity_started = Instant::now();
        let report = topic::topic_affinities::<OrtEmbedder, OrtEmbedder>(
            &hashes,
            &topics,
            alpha,
            app.vectors.as_ref(),
            text.as_deref(),
            clip.as_deref(),
        );
        // The scale-spike telemetry: surface where it struggles (DESIGN — do
        // not silently cap; LOG where it falls over, at what N).
        tracing::info!(
            scope = ?scope,
            images = hashes.len(),
            topics = topics.len(),
            alpha,
            visual_ready = report.visual_ready,
            annotation_ready = report.annotation_ready,
            scope_enum_ms = scope_ms,
            affinity_scan_ms = affinity_started.elapsed().as_millis(),
            "topic_affinities computed"
        );
        Ok(report)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// `suggest_topics(scope)` (DESIGN-SEMANTIC-GRAPH.md, v1): cheap candidate
/// topics for the suggestion rail — frequent note n-grams over the scope's
/// annotation text + the names of collections the scope's images belong to.
/// NO LLM (v3), NO clustering (v2).
#[tauri::command]
pub async fn suggest_topics(app: S<'_>, scope: GraphScope) -> CmdResult<Vec<TopicSuggestion>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let hashes = enumerate_scope(&app, &scope)?;

        // The scope's note prose, mined for recurring n-grams. A fresh
        // read-only connection over the shared WAL db (the debug-readq pattern):
        // a short read-only projection has no business holding any write lock.
        let note_texts = {
            let db_path = app.app_data.join("photoproof.db");
            let conn = rusqlite::Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| CmdError::Invalid(format!("open notes read: {e}")))?;
            topic::scope_note_texts(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("scope notes: {e}")))?
        };

        // Collection names + how many of the scope's images each contains (the
        // overlap is the chip's strength). A collection with no scope overlap is
        // not a relevant suggestion, so it is dropped.
        let scope_set: std::collections::HashSet<&str> =
            hashes.iter().map(String::as_str).collect();
        let mut collection_names: Vec<(String, usize)> = Vec::new();
        if let Ok(list) = app.collections.list() {
            for c in list {
                let overlap = app
                    .collections
                    .current_members(&c.id)
                    .map(|members| {
                        members
                            .iter()
                            .filter(|h| scope_set.contains(h.as_str()))
                            .count()
                    })
                    .unwrap_or(0);
                if overlap > 0 {
                    collection_names.push((c.name, overlap));
                }
            }
        }

        Ok(topic::suggest_topics(&note_texts, &collection_names))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Which embedding space `cluster_topics` clusters over. ANNOTATION
/// (`image_summary`) is the default since these clusters become note-grounded
/// topics; CLIP is offered for a "what it LOOKS like" clustering.
#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClusterSpace {
    /// `image_summary` vectors — what you SAID (the note-grounded default).
    #[default]
    Annotation,
    /// `image_clip` vectors — what it LOOKS like.
    Clip,
}

impl ClusterSpace {
    fn vec_kind(self) -> VecKind {
        match self {
            ClusterSpace::Annotation => VecKind::ImageSummary,
            ClusterSpace::Clip => VecKind::ImageClip,
        }
    }
}

/// `cluster_topics(scope, k?, space?)` (DESIGN-SEMANTIC-GRAPH.md, v2): cluster
/// the in-scope image vectors with a small k-means and label each cluster by its
/// most representative note phrase. These feed the suggestion rail as smarter,
/// note-grounded auto-topics (v1's suggestions were just frequent n-grams).
///
/// Clusters over the ANNOTATION space (`image_summary`) by default — the space
/// the labels are grounded in — or the CLIP space when asked. `k` omitted picks
/// from the scope size by the GraphTuning heuristic within `[cluster_k_min,
/// cluster_k_max]`.
///
/// Reads STORED vectors (no embed pass): the clustering space's model id comes
/// from the active embedder when loaded, else from any stored row (so the lens
/// clusters an embedded-but-models-unloaded library). GRACEFUL: an empty or
/// un-embedded scope returns an empty rail, never an error.
#[tauri::command]
pub async fn cluster_topics(
    app: S<'_>,
    scope: GraphScope,
    k: Option<usize>,
    space: Option<ClusterSpace>,
) -> CmdResult<Vec<ClusterTopic>> {
    let app = app.inner().clone();
    let space = space.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let started = Instant::now();
        let hashes = enumerate_scope(&app, &scope)?;
        let g = tuning().graph;

        // Resolve the clustering space's model id: prefer the loaded embedder
        // (matches the live write path), else fall back to any stored row's
        // model so an embedded library clusters even with models unloaded.
        let vec_kind = space.vec_kind();
        let active_model = match space {
            ClusterSpace::Annotation => app
                .runtime
                .embedders
                .text()
                .map(|e| e.model_id().to_owned()),
            ClusterSpace::Clip => app
                .runtime
                .embedders
                .clip()
                .map(|e| e.model_id().to_owned()),
        };
        let model_id = match active_model {
            Some(m) => Some(m),
            None => app
                .vectors
                .any_model_id(vec_kind)
                .map_err(|e| CmdError::Invalid(format!("cluster model lookup: {e}")))?,
        };
        // No model id ⇒ the space is empty (un-embedded) ⇒ an empty rail.
        let Some(model_id) = model_id else {
            return Ok(Vec::new());
        };

        let vectors = app
            .vectors
            .read_image_vectors(VecSpace { vec_kind, model_id }, &hashes)
            .map_err(|e| CmdError::Invalid(format!("cluster vector read: {e}")))?;

        // Per-image note text for labeling (read-only projection over the WAL db,
        // the same pattern suggest_topics uses).
        let notes_by_hash = {
            let db_path = app.app_data.join("photoproof.db");
            let conn = rusqlite::Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| CmdError::Invalid(format!("open notes read: {e}")))?;
            topic::scope_note_texts_by_hash(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("scope notes: {e}")))?
        };

        let clusters = topic::cluster_topics(
            &vectors,
            &notes_by_hash,
            k,
            g.cluster_k_min as usize,
            g.cluster_k_max as usize,
        );
        tracing::info!(
            scope = ?scope,
            images = hashes.len(),
            embedded = vectors.len(),
            clusters = clusters.len(),
            cluster_ms = started.elapsed().as_millis(),
            "cluster_topics computed"
        );
        Ok(clusters)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// `graph_neighbors(scope, k?)`: the sparse semantic k-NN graph the visualizer's
/// force layout reads so alike photos cluster (similar images attract). For each
/// in-scope image with a stored vector, its top-`k` most-similar OTHER in-scope
/// images and how strongly each should pull.
///
/// The primary signal is the CLIP image space (what it LOOKS like), resolved the
/// SAME way `cluster_topics` resolves a space: prefer the loaded embedder's model
/// id (matches the live write path), else any stored row's model so an
/// embedded-but-models-unloaded library still graphs. We ALSO blend in the
/// annotation/summary space (what you SAID) when it has rows: for a pair present
/// in both, the weights are averaged; a pair in only one keeps its weight. The
/// blend is defensive — `image_summary` is often empty, so we simply skip it and
/// return the CLIP neighbors when it has no model id.
///
/// Runs on a blocking thread (the brute-force O(N^2) precompute can take real
/// time on a large scope), mirroring `topic_affinities`. GRACEFUL: an
/// un-embedded scope (no CLIP model id) returns an empty Vec — no edges, never an
/// error.
#[tauri::command]
pub async fn graph_neighbors(
    app: S<'_>,
    scope: GraphScope,
    k: Option<usize>,
) -> CmdResult<Vec<ImageNeighbors>> {
    let app = app.inner().clone();
    let k = k.unwrap_or(6);
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let started = Instant::now();
        let hashes = enumerate_scope(&app, &scope)?;

        // Resolve the CLIP image space's model id exactly like cluster_topics:
        // the loaded embedder when present, else any stored row's model.
        let clip_model = match app
            .runtime
            .embedders
            .clip()
            .map(|e| e.model_id().to_owned())
        {
            Some(m) => Some(m),
            None => app
                .vectors
                .any_model_id(VecKind::ImageClip)
                .map_err(|e| CmdError::Invalid(format!("clip model lookup: {e}")))?,
        };
        // No CLIP model id ⇒ the space is un-embedded ⇒ no semantic edges.
        let Some(clip_model) = clip_model else {
            return Ok(Vec::new());
        };

        let clip = app
            .vectors
            .knn_within(
                VecSpace {
                    vec_kind: VecKind::ImageClip,
                    model_id: clip_model,
                },
                &hashes,
                k,
            )
            .map_err(|e| CmdError::Invalid(format!("clip knn: {e}")))?;

        // Blend in note similarity when the annotation/summary space is
        // populated. It is often empty (no notes embedded yet), so resolving no
        // model id just skips the blend and keeps the pure CLIP graph.
        let summary_model = app
            .vectors
            .any_model_id(VecKind::ImageSummary)
            .map_err(|e| CmdError::Invalid(format!("summary model lookup: {e}")))?;
        let summary = match summary_model {
            Some(model_id) => app
                .vectors
                .knn_within(
                    VecSpace {
                        vec_kind: VecKind::ImageSummary,
                        model_id,
                    },
                    &hashes,
                    k,
                )
                .map_err(|e| CmdError::Invalid(format!("summary knn: {e}")))?,
            None => Vec::new(),
        };

        let merged = merge_neighbor_graphs(clip, summary, k);
        tracing::info!(
            scope = ?scope,
            images = hashes.len(),
            edged = merged.len(),
            k,
            neighbors_ms = started.elapsed().as_millis(),
            "graph_neighbors computed"
        );
        Ok(merged)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Merge the CLIP and annotation k-NN graphs into the layout's edge set: for a
/// pair present in BOTH spaces average the two weights (the photo looks AND
/// reads alike — a stronger, agreed-on pull); a pair in only one keeps its
/// weight. Re-trims to `k` and re-sorts (weight desc, neighbor hash asc) so the
/// blended graph stays deterministic, exactly like `knn_within`.
fn merge_neighbor_graphs(clip: KnnGraph, summary: KnnGraph, k: usize) -> Vec<ImageNeighbors> {
    use std::collections::BTreeMap;

    // neighbor_hash -> (clip_weight?, summary_weight?) for one source image.
    type PairWeights = BTreeMap<String, (Option<f32>, Option<f32>)>;
    // Per source image: its neighbor pair-weights. A BTreeMap keeps the outer
    // iteration order stable (deterministic output).
    let mut by_image: BTreeMap<String, PairWeights> = BTreeMap::new();
    for (hash, neighbors) in clip {
        let entry = by_image.entry(hash).or_default();
        for (n, w) in neighbors {
            entry.entry(n).or_default().0 = Some(w);
        }
    }
    for (hash, neighbors) in summary {
        let entry = by_image.entry(hash).or_default();
        for (n, w) in neighbors {
            entry.entry(n).or_default().1 = Some(w);
        }
    }

    let mut out = Vec::with_capacity(by_image.len());
    for (hash, neighbor_map) in by_image {
        let mut neighbors: Vec<Neighbor> = neighbor_map
            .into_iter()
            .map(|(n, (c, s))| {
                // Average when both spaces agree on the pair; otherwise take the
                // single weight that exists.
                let weight = match (c, s) {
                    (Some(c), Some(s)) => (c + s) / 2.0,
                    (Some(c), None) => c,
                    (None, Some(s)) => s,
                    (None, None) => 0.0,
                };
                Neighbor { hash: n, weight }
            })
            .collect();
        neighbors.sort_by(|a, b| {
            b.weight
                .partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.hash.cmp(&b.hash))
        });
        neighbors.truncate(k);
        out.push(ImageNeighbors { hash, neighbors });
    }
    out
}

/// The concrete `TopicLlm` this build wires. v3 SEAM: the Gemma connector is
/// spec'd but NOT yet wired (mocked in M1), so there is no real implementor yet;
/// the uninhabited type lets the command compile against the seam while
/// `suggest_topics_llm(None, ..)` always returns the honest `Unavailable` state.
//
// TODO(v3): replace this with the real Gemma-backed implementor when the LLM
// connector lands, and pass `Some(&llm)` below.
enum WiredTopicLlm {}

impl TopicLlm for WiredTopicLlm {
    fn suggest(&self, _note_texts: &[String], _max: usize) -> LlmSuggestions {
        match *self {}
    }
}

/// `suggest_topics_llm(scope)` (DESIGN-SEMANTIC-GRAPH.md, v3 SEAM): the
/// LLM-grounded suggestion path. SCAFFOLD ONLY: the Gemma connector is not wired
/// (mocked in M1), so this always returns the explicit `Unavailable` state and
/// the rail degrades to the cluster + n-gram suggestions. It NEVER fabricates
/// LLM output. The command exists so the seam is real end-to-end (the frontend
/// reads it and shows LLM suggestions only once `state == "ready"`).
#[tauri::command]
pub async fn suggest_topics_llm(app: S<'_>, scope: GraphScope) -> CmdResult<LlmSuggestions> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let hashes = enumerate_scope(&app, &scope)?;
        let note_texts = {
            let db_path = app.app_data.join("photoproof.db");
            let conn = rusqlite::Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| CmdError::Invalid(format!("open notes read: {e}")))?;
            topic::scope_note_texts(&conn, &hashes)
                .map_err(|e| CmdError::Invalid(format!("scope notes: {e}")))?
        };
        // None connector ⇒ the honest Unavailable state. When the Gemma connector
        // lands, pass `Some(&llm)` here and the rail will surface its themes.
        Ok(topic::suggest_topics_llm::<WiredTopicLlm>(
            None,
            &note_texts,
            8,
        ))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// `graph_tuning()` — the GraphTuning knobs the frontend force sim reads (the
/// default α slider value, attraction/repulsion/damping, ring radius). Sourced
/// from the centralized, file-overridable tuning config so the founder tunes
/// the lens by feel without a rebuild.
#[tauri::command]
pub fn graph_tuning() -> GraphTuning {
    tuning().graph
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

    /// The graceful contract through the REAL command path: a degraded rig (no
    /// embedders, empty library) returns a well-formed report — every scope
    /// image present (here zero, an empty library), readiness flags false —
    /// never an error.
    #[test]
    fn topic_affinities_command_degrades_gracefully() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let report = tauri::async_runtime::block_on(topic_affinities(
            state.clone(),
            GraphScope::Library,
            vec!["harbor".into(), "snow".into()],
            None,
        ))
        .expect("topic_affinities");
        // Empty library ⇒ no image rows, but it MUST NOT error.
        assert!(report.images.is_empty());
        assert!(!report.visual_ready && !report.annotation_ready);
    }

    /// `suggest_topics` over an empty library returns an empty rail, not an
    /// error — the command path is correct on a fresh machine.
    #[test]
    fn suggest_topics_command_empty_library() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let out =
            tauri::async_runtime::block_on(suggest_topics(state.clone(), GraphScope::Library))
                .expect("suggest_topics");
        assert!(out.is_empty());
    }

    /// `cluster_topics` over an empty/un-embedded library returns an empty rail,
    /// not an error — the graceful pre-embed-pass posture through the real
    /// command path (no model id ⇒ empty, never a crash).
    #[test]
    fn cluster_topics_command_degrades_gracefully() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let out = tauri::async_runtime::block_on(cluster_topics(
            state.clone(),
            GraphScope::Library,
            None,
            None,
        ))
        .expect("cluster_topics");
        assert!(out.is_empty());
    }

    /// `graph_tuning` returns the shipped defaults (the slider's start value +
    /// the sim physics) absent a tuning.toml override.
    #[test]
    fn graph_tuning_returns_defaults() {
        let g = graph_tuning();
        assert_eq!(g.alpha_default, 0.5);
        assert_eq!(g.ring_radius, 320.0);
    }
}
