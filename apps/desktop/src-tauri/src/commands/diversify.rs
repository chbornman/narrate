//! The Diversify / duplication-tolerance command
//! (DESIGN-DEDUP-AND-SIMILARITY.md, the duplication-tolerance slider).
//!
//! `diversify_scope(scope, tolerance)` answers, for a scope and a single
//! `tolerance ∈ [0, 1]`: WHICH images are representatives (SHOWN in the grid)
//! and which are redundant (HIDDEN). It is a NON-DESTRUCTIVE view filter — no
//! image is deleted, the frontend simply renders `shown` and folds `hidden`.
//!
//! It is thin orchestration over `photoproof_core::retrieval::diversity` (the
//! pure selection math) and the existing CLIP k-NN graph (`knn_within`):
//!   1. resolve the scope to its hashes (the SAME `enumerate_scope` the graph
//!      lens uses, so the Diversify set matches the grid exactly),
//!   2. build the CLIP cosine k-NN graph over the scope,
//!   3. read per-image ratings as the QUALITY score (so a cluster's
//!      representative is its highest-rated frame),
//!   4. map `tolerance` to a similarity cutoff and run greedy
//!      FACILITY-LOCATION (the design's recommended core / the default).
//!
//! GRACEFUL by construction (the whole-product M1 posture, like every other
//! lens command): a degraded rig (no CLIP model, un-embedded scope) yields an
//! "all shown, nothing hidden" report — never an error. With no similarity
//! signal there is nothing to collapse, which is exactly the correct answer.

use std::collections::BTreeMap;
use std::time::Instant;

use photoproof_connectors::embedder::Embedder;
use photoproof_connectors::vector_store::{VecKind, VecSpace};
use photoproof_core::retrieval::{QualityScores, facility_location_select, tolerance_to_cutoff};
use photoproof_core::tuning::tuning;
use rusqlite::OpenFlags;
use serde::Serialize;

use super::S;
use super::graph::{GraphScope, enumerate_scope};
use crate::error::{CmdError, CmdResult};
use crate::state::App;

/// The Diversify result: which in-scope images the view SHOWS (representatives)
/// vs HIDES (redundant), plus the resolved cutoff for transparency/telemetry.
/// `shown ∪ hidden` is exactly the scope; both are sorted by hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiversifyReport {
    /// Representative image hashes — what the grid renders.
    pub shown: Vec<String>,
    /// Redundant image hashes folded into a representative — what the grid hides.
    pub hidden: Vec<String>,
    /// The cosine similarity cutoff the tolerance resolved to (for the UI to
    /// surface "collapsing at ≥ N% similar" and for telemetry). Two images at or
    /// above this similarity are treated as redundant.
    pub cutoff: f32,
    /// True when no CLIP similarity signal existed (un-embedded / no model), so
    /// the report is a trivial "all shown" — the frontend can show "embed to
    /// diversify" rather than implying the slider had no effect.
    pub degraded: bool,
}

/// `diversify_scope(scope, tolerance)` — the duplication-tolerance view filter.
///
/// `tolerance ∈ [0, 1]`: 0 shows everything; raising it collapses each CLIP
/// similarity cluster to a single highest-rated representative and hides the
/// rest. The mapping `tolerance → cutoff` (and WHY it is linear / pinned at 1.0
/// at zero) is documented on `retrieval::diversity::tolerance_to_cutoff`; the
/// endpoints (`cutoff_high` / `cutoff_low`) and the k-NN fan-out are
/// founder-tunable in `[diversify]` of `tuning.toml`.
///
/// Runs on a blocking thread (the brute-force k-NN precompute can take real time
/// on a large scope), mirroring `graph_neighbors`. The default OBJECTIVE is
/// greedy facility-location (the design's recommended core) — MMR / k-center are
/// also implemented in the core module for founder experimentation but are not
/// wired to the slider yet (see FOR FOUNDER REVIEW in the build report).
#[tauri::command]
pub async fn diversify_scope(
    app: S<'_>,
    scope: GraphScope,
    tolerance: f32,
) -> CmdResult<DiversifyReport> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let started = Instant::now();
        let hashes = enumerate_scope(&app, &scope)?;
        let t = tuning().diversify;
        // Map the single slider to the objective's cutoff (the documented curve).
        let cutoff = tolerance_to_cutoff(tolerance, t.cutoff_high as f32, t.cutoff_low as f32);

        // Resolve the CLIP image space's model id exactly like graph_neighbors:
        // the loaded embedder when present, else any stored row's model so an
        // embedded-but-models-unloaded library still diversifies.
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

        // No CLIP model id ⇒ no similarity signal ⇒ nothing to collapse: the
        // honest "all shown" report (degraded), never an error.
        let Some(clip_model) = clip_model else {
            let mut shown = hashes;
            shown.sort();
            shown.dedup();
            tracing::info!(
                scope = ?scope,
                images = shown.len(),
                tolerance,
                cutoff,
                "diversify_scope: no CLIP model (degraded all-shown)"
            );
            return Ok(DiversifyReport {
                shown,
                hidden: Vec::new(),
                cutoff,
                degraded: true,
            });
        };

        // The sparse CLIP cosine k-NN graph over exactly the scope — the same
        // brute-force kernel the visualizer reads, with a wider fan-out (the
        // Diversify pass wants to see more of each cluster than the layout does).
        let graph = app
            .vectors
            .knn_within(
                VecSpace {
                    vec_kind: VecKind::ImageClip,
                    model_id: clip_model,
                },
                &hashes,
                t.knn_k as usize,
            )
            .map_err(|e| CmdError::Invalid(format!("clip knn: {e}")))?;

        // Per-image QUALITY = current rating (0..=5), so a cluster collapses to
        // its highest-rated frame. Unrated images score 0 ⇒ the deterministic
        // hash tie-break decides among equally-(un)rated frames. A read-only
        // projection over the shared WAL db (the established debug-readq pattern).
        let quality = read_ratings(&app, &hashes)?;

        let sel = facility_location_select(&graph, &hashes, &quality, cutoff);
        tracing::info!(
            scope = ?scope,
            images = hashes.len(),
            shown = sel.shown.len(),
            hidden = sel.hidden.len(),
            tolerance,
            cutoff,
            diversify_ms = started.elapsed().as_millis(),
            "diversify_scope computed"
        );
        Ok(DiversifyReport {
            shown: sel.shown,
            hidden: sel.hidden,
            cutoff,
            degraded: false,
        })
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Read the current rating (0..=5) for each in-scope image from the
/// `image_ratings` fold as the representative-quality score. Unrated images are
/// simply absent (the selection treats absent as 0). Read-only WAL connection —
/// a short projection has no business holding a write lock, the same pattern
/// `suggest_topics` / `cluster_topics` use for their note reads.
fn read_ratings(app: &App, hashes: &[String]) -> CmdResult<QualityScores> {
    let mut out: QualityScores = BTreeMap::new();
    if hashes.is_empty() {
        return Ok(out);
    }
    let db_path = app.app_data.join("photoproof.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CmdError::Invalid(format!("open ratings read: {e}")))?;

    // One IN-list projection over the fold. A scope's hashes are bounded by the
    // grid, so the placeholder list stays sane; chunk defensively so a very large
    // scope can't exceed SQLite's bind-parameter limit.
    let mut stmt = conn
        .prepare("SELECT image_hash, rating FROM image_ratings WHERE image_hash = ?1")
        .map_err(|e| CmdError::Invalid(format!("prepare ratings: {e}")))?;
    for hash in hashes {
        let rating: Option<i64> = stmt.query_row([hash], |r| r.get(0)).ok();
        if let Some(r) = rating {
            // Rating is 0..=5; a higher rating ⇒ a better representative.
            out.insert(hash.clone(), r as f32);
        }
    }
    Ok(out)
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
    /// CLIP model, empty/un-embedded library) returns a well-formed all-shown
    /// report flagged `degraded`, never an error — the mechanism is correct
    /// before any embed pass.
    #[test]
    fn diversify_scope_degrades_gracefully() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let report = tauri::async_runtime::block_on(diversify_scope(
            state.clone(),
            GraphScope::Library,
            0.7,
        ))
        .expect("diversify_scope");
        // Empty library ⇒ nothing shown or hidden, flagged degraded (no signal).
        assert!(report.shown.is_empty());
        assert!(report.hidden.is_empty());
        assert!(report.degraded);
    }

    /// Tolerance 0 resolves to the documented cutoff (1.0) through the real
    /// command path — the "slider at zero shows everything" mapping is wired to
    /// the tuning endpoints, not a hardcoded number.
    #[test]
    fn diversify_scope_tolerance_zero_resolves_full_cutoff() {
        let (_tmp, tauri_app) = mock_app();
        let state: tauri::State<'_, Arc<App>> = tauri_app.state();
        let report = tauri::async_runtime::block_on(diversify_scope(
            state.clone(),
            GraphScope::Library,
            0.0,
        ))
        .expect("diversify_scope");
        assert_eq!(report.cutoff, 1.0);
    }
}
