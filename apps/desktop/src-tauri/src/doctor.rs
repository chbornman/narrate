//! Unified startup doctor (STATE-INTEGRITY-AUDIT.md; founder: "fully robust").
//!
//! One ordered, logged disk-vs-DB integrity pass, run ONCE at launch on a
//! background thread, so silent drift heals (or surfaces) immediately instead
//! of waiting for the ~10-minute maintenance tick. The principle: outside the
//! sidecar files, all on-disk state is DERIVED and rebuildable, so the doctor's
//! job is to reconcile derived state back to the index + the active config.
//!
//! Every step emits a structured `tracing` line under the "startup integrity
//! report" banner so the log TELLS you what was wrong and what was done, rather
//! than failing silently (the whole point of the audit).

use std::sync::Arc;

use photoproof_connectors::vector_store::VecKind;
use photoproof_core::library::{PassName, ScanOptions};
use photoproof_core::retrieval::SpaceReconcileReason;

use crate::state::App;

/// The ingest pass whose re-run rebuilds a given vector space's vectors.
fn repend_pass_for(kind: VecKind) -> PassName {
    match kind {
        VecKind::ImageClip => PassName::ImageEmbedding,
        // Both text-embedder spaces are produced off the text-embedding stage.
        VecKind::AnnotationChunk | VecKind::ImageSummary => PassName::TextEmbedding,
    }
}

/// Run the startup integrity sweep. BEST-EFFORT: a failed step logs and the next
/// step still runs (a doctor that aborts on the first problem is worse than one
/// that heals what it can). Safe to call on a background thread; it only touches
/// derived state through the same APIs the maintenance tick uses.
pub fn run_startup_doctor(app: &Arc<App>) {
    tracing::info!("startup integrity report: begin");

    // 1. Vector spaces: drop superseded duplicates (e.g. an old CLIP model left
    //    behind after a swap), clear dangling spaces whose flat file vanished,
    //    and sweep orphaned `.ppvec` files. Re-pend the embedding pass for any
    //    space that must be rebuilt from scratch.
    match app
        .vectors
        .reconcile_spaces(&app.runtime.active_vector_models())
    {
        Ok(report) if report.is_empty() => {
            tracing::info!("startup integrity report: vector spaces ok");
        }
        Ok(report) => {
            for s in &report.reconciled {
                let reason = match s.reason {
                    SpaceReconcileReason::DanglingActiveFileMissing => "dangling (file missing)",
                    SpaceReconcileReason::SupersededByActiveModel => "superseded duplicate",
                    SpaceReconcileReason::OrphanFile => "orphan file",
                };
                tracing::warn!(
                    vec_kind = ?s.vec_kind,
                    model_id = %s.model_id,
                    rows = s.rows,
                    "startup doctor: reconciled vector space ({reason})"
                );
            }
            // A space whose file vanished had its dangling rows deleted; the pass
            // row still records the current model, so the model-aware re-pend
            // would no-op. Force an unconditional re-pend to rebuild dense.
            for (kind, model_id) in &report.repend {
                match app.library.repend_pass(repend_pass_for(*kind)) {
                    Ok(n) => tracing::warn!(
                        vec_kind = ?kind, model_id = %model_id, repended = n,
                        "startup doctor: re-pended embeddings to rebuild a missing space"
                    ),
                    Err(e) => {
                        tracing::error!(error = %e, vec_kind = ?kind, "startup doctor: re-pend failed")
                    }
                }
            }
        }
        Err(e) => tracing::error!(error = %e, "startup doctor: vector reconcile failed"),
    }

    // 2. Orphaned ingest passes: an image whose every path went stale (root
    //    removed, file deleted) can never complete its pending/error passes —
    //    they would defer forever and churn the drain. Catch images orphaned
    //    before the remove-root skip fix and retire their dead passes.
    match app.library.heal_orphaned_passes() {
        Ok(0) => tracing::info!("startup integrity report: no orphaned ingest passes"),
        Ok(n) => tracing::warn!(
            skipped = n,
            "startup doctor: skipped orphaned ingest passes (no active path)"
        ),
        Err(e) => tracing::error!(error = %e, "startup doctor: orphaned-pass heal failed"),
    }

    // 3. Preview artifacts (disk vs DB) + stranded temp sweep. The library doctor
    //    previously ran only on the maintenance tick; running it at startup heals
    //    a mangled preview cache immediately (audit: "run the existence walk at
    //    open, not only on the 6h tick").
    match app.library.doctor() {
        Ok(r) => tracing::info!(
            repended = r.repended,
            stale_orphans = r.stale_orphans,
            temps_swept = r.temps_swept,
            "startup integrity report: preview cache reconciled"
        ),
        Err(e) => tracing::error!(error = %e, "startup doctor: preview reconcile failed"),
    }

    // 4. Filesystem reconcile (AUDIT-2026-07-07 S1). The notify watcher does
    //    not replay events from while the app was closed, and the pump's first
    //    maintenance tick is a full interval (~10 min) after launch — so
    //    offline adds/deletes/edits were invisible until then. Scanning every
    //    active root HERE closes the gap: this thread already runs off the UI
    //    path (launch is never blocked), it runs AFTER the derived-state heals
    //    above (previews/passes are consistent before new work is enqueued),
    //    and the watchers are already live (lib.rs starts them before spawning
    //    this thread), so nothing that happens mid-scan is lost — the scan and
    //    the watcher converge on the same idempotent observe_* per-path
    //    algorithm. The pump's maintenance seed is untouched, so this cannot
    //    double-run with an early maintenance tick.
    //
    //    The walk registers with `app.scans` (the add-root/rescan idiom) so
    //    `ingest_status` reports scanning/discovered live instead of the grid
    //    lying "No photographs" over a busy startup walk.
    {
        let _walk = app.scans.begin(); // guard: de-registers on every exit
        let opts = ScanOptions {
            discovered: Some(app.scans.counter()),
            ..ScanOptions::default()
        };
        match app.library.reconcile_all(&opts) {
            Ok(reports) => {
                for (root_id, r) in &reports {
                    // Log only roots with drift; a clean root at startup is
                    // the common case and must not spam the report.
                    if r.new_images + r.superseded + r.relinked + r.went_stale > 0 {
                        tracing::info!(
                            root_id = %root_id,
                            new_images = r.new_images,
                            superseded = r.superseded,
                            relinked = r.relinked,
                            went_stale = r.went_stale,
                            "startup doctor: reconciled offline filesystem changes"
                        );
                    }
                }
                tracing::info!(
                    roots = reports.len(),
                    "startup integrity report: filesystem reconciled"
                );
            }
            Err(e) => tracing::error!(error = %e, "startup doctor: filesystem reconcile failed"),
        }
    }

    tracing::info!("startup integrity report: done");
}
