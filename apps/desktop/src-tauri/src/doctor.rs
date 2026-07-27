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
use std::time::{SystemTime, UNIX_EPOCH};

use photoproof_connectors::vector_store::VecKind;
use photoproof_core::library::{
    DoctorReport, PassName, RootReconcileOutcome, RootReconcileResult, ScanOptions,
};
use photoproof_core::retrieval::SpaceReconcileReason;
use serde::Serialize;

use crate::lifecycle::{Subsystem, SubsystemHealth};
use crate::managed_tasks::TaskContext;
use crate::runtime::ActiveVectorTarget;
use crate::state::App;

/// Retained outcome of the startup repair pass. Unlike task history, this
/// remains available after the managed task reaches terminal state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairIntegritySnapshot {
    pub state: &'static str,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub vector_reconciled: Option<bool>,
    pub orphaned_passes_skipped: Option<usize>,
    pub retention: Option<RetentionRepairSnapshot>,
    pub roots: Option<RootRepairSnapshot>,
    pub errors: Vec<String>,
}

impl Default for RepairIntegritySnapshot {
    fn default() -> Self {
        Self {
            state: "pending",
            started_at_ms: None,
            completed_at_ms: None,
            vector_reconciled: None,
            orphaned_passes_skipped: None,
            retention: None,
            roots: None,
            errors: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionRepairSnapshot {
    pub repended: usize,
    pub stale_orphans: usize,
    pub orphan_images: usize,
    pub retention_eligible: usize,
    pub retention_deferred_recent: usize,
    pub retention_deferred_unknown_timestamp: usize,
    pub retention_deferred_busy: usize,
    pub retention_dry_run: bool,
    pub reclaimed_images: usize,
    pub preview_rows_reclaimed: usize,
    pub preview_files_reclaimed: usize,
    pub preview_bytes_reclaimed: u64,
    pub vector_rows_reclaimed: usize,
    pub vector_spaces_compacted: usize,
    pub journal_vector_rows_retained: usize,
    pub temps_swept: usize,
}

impl From<DoctorReport> for RetentionRepairSnapshot {
    fn from(report: DoctorReport) -> Self {
        Self {
            repended: report.repended,
            stale_orphans: report.stale_orphans,
            orphan_images: report.orphan_images,
            retention_eligible: report.retention_eligible,
            retention_deferred_recent: report.retention_deferred_recent,
            retention_deferred_unknown_timestamp: report.retention_deferred_unknown_timestamp,
            retention_deferred_busy: report.retention_deferred_busy,
            retention_dry_run: report.retention_dry_run,
            reclaimed_images: report.reclaimed_images,
            preview_rows_reclaimed: report.preview_rows_reclaimed,
            preview_files_reclaimed: report.preview_files_reclaimed,
            preview_bytes_reclaimed: report.preview_bytes_reclaimed,
            vector_rows_reclaimed: report.vector_rows_reclaimed,
            vector_spaces_compacted: report.vector_spaces_compacted,
            journal_vector_rows_retained: report.journal_vector_rows_retained,
            temps_swept: report.temps_swept,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootRepairSnapshot {
    pub total_roots: usize,
    pub scanned_roots: usize,
    pub degraded_roots: usize,
    pub new_images: usize,
    pub superseded: usize,
    pub relinked: usize,
    pub retention_repairs_revived: usize,
    pub went_stale: usize,
    pub io_errors: usize,
}

fn repair_update(app: &App, update: impl FnOnce(&mut RepairIntegritySnapshot)) {
    update(&mut app.repair_integrity.lock().expect("repair integrity mutex"));
}

fn repair_cancelled(app: &App) {
    repair_update(app, |repair| {
        repair.state = "cancelled";
        repair.completed_at_ms = Some(now_ms());
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn summarize_roots(reports: &[RootReconcileResult]) -> RootRepairSnapshot {
    let mut summary = RootRepairSnapshot {
        total_roots: reports.len(),
        ..RootRepairSnapshot::default()
    };
    for root in reports {
        match &root.outcome {
            RootReconcileOutcome::Scanned(report) => {
                summary.scanned_roots += 1;
                summary.new_images += report.new_images;
                summary.superseded += report.superseded;
                summary.relinked += report.relinked;
                summary.retention_repairs_revived += report.retention_repairs_revived;
                summary.went_stale += report.went_stale;
                summary.io_errors += report.io_errors;
                summary.degraded_roots += usize::from(report.stale_inference_suppressed);
            }
            RootReconcileOutcome::Offline { .. } | RootReconcileOutcome::Failed { .. } => {
                summary.degraded_roots += 1;
            }
        }
    }
    summary
}

/// The ingest pass whose re-run rebuilds a given vector space's vectors.
fn repend_pass_for(kind: VecKind) -> PassName {
    match kind {
        VecKind::ImageClip => PassName::ImageEmbedding,
        // Both text-embedder spaces are produced off the text-embedding stage.
        VecKind::AnnotationChunk | VecKind::ImageSummary => PassName::TextEmbedding,
    }
}

/// Reconcile vector disk/metadata truth against models that are actually
/// Ready. Startup calls this with the currently-ready set for safe orphan
/// cleanup; the readiness coordinator calls it again when an embedder lands.
pub fn reconcile_vector_spaces(
    app: &Arc<App>,
    task: &TaskContext,
    active_models: &std::collections::HashMap<VecKind, String>,
) -> Result<(), String> {
    match app.vectors.reconcile_spaces(active_models) {
        Ok(report) if report.is_empty() => {
            app.lifecycle
                .set_health(Subsystem::Vectors, SubsystemHealth::Healthy);
            tracing::info!("vector integrity: spaces ok");
            Ok(())
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
                    "vector integrity: reconciled space ({reason})"
                );
            }
            for (kind, model_id) in &report.repend {
                match app.library.repend_pass(repend_pass_for(*kind)) {
                    Ok(n) => tracing::warn!(
                        vec_kind = ?kind, model_id = %model_id, repended = n,
                        "vector integrity: re-pended embeddings to rebuild a missing space"
                    ),
                    Err(error) => {
                        task.report_error(format!("vector re-pend failed: {error}"));
                        app.lifecycle.set_health(
                            Subsystem::Vectors,
                            SubsystemHealth::Degraded {
                                summary: error.to_string(),
                            },
                        );
                        return Err(error.to_string());
                    }
                }
            }
            app.lifecycle
                .set_health(Subsystem::Vectors, SubsystemHealth::Healthy);
            Ok(())
        }
        Err(error) => {
            task.report_error(format!("vector reconcile failed: {error}"));
            app.lifecycle.set_health(
                Subsystem::Vectors,
                SubsystemHealth::Degraded {
                    summary: error.to_string(),
                },
            );
            tracing::error!(error = %error, "vector integrity: reconcile failed");
            Err(error.to_string())
        }
    }
}

/// Run the startup integrity sweep. BEST-EFFORT: a failed step logs and the next
/// step still runs (a doctor that aborts on the first problem is worse than one
/// that heals what it can). Safe to call on a background thread; it only touches
/// derived state through the same APIs the maintenance tick uses.
pub fn run_startup_doctor(app: &Arc<App>, task: &TaskContext) -> Option<ActiveVectorTarget> {
    repair_update(app, |repair| {
        *repair = RepairIntegritySnapshot {
            state: "running",
            started_at_ms: Some(now_ms()),
            ..RepairIntegritySnapshot::default()
        };
    });
    tracing::info!("startup integrity report: begin");

    // 1. Vector spaces: drop superseded duplicates (e.g. an old CLIP model left
    //    behind after a swap), clear dangling spaces whose flat file vanished,
    //    and sweep orphaned `.ppvec` files. Re-pend the embedding pass for any
    //    space that must be rebuilt from scratch.
    task.report_progress(0.05, "checking vector spaces");
    if task.is_cancelled() {
        tracing::info!("startup integrity report: cancelled before vector reconcile");
        repair_cancelled(app);
        return None;
    }
    let active_target = app.runtime.active_vector_target();
    let vector_result = reconcile_vector_spaces(app, task, &active_target.models);
    let vector_reconciled = vector_result.is_ok();
    repair_update(app, |repair| {
        repair.vector_reconciled = Some(vector_reconciled);
        if let Err(error) = vector_result {
            repair.errors.push(format!("vector reconcile: {error}"));
        }
    });

    // 2. Orphaned ingest passes: an image whose every path went stale (root
    //    removed, file deleted) can never complete its pending/error passes —
    //    they would defer forever and churn the drain. Catch images orphaned
    //    before the remove-root skip fix and retire their dead passes.
    task.report_progress(0.25, "checking ingest passes");
    if task.is_cancelled() {
        tracing::info!("startup integrity report: cancelled before ingest-pass heal");
        repair_cancelled(app);
        return vector_reconciled.then_some(active_target);
    }
    match app.library.heal_orphaned_passes() {
        Ok(n) => {
            repair_update(app, |repair| repair.orphaned_passes_skipped = Some(n));
            if n == 0 {
                tracing::info!("startup integrity report: no orphaned ingest passes");
            } else {
                tracing::warn!(
                    skipped = n,
                    "startup doctor: skipped orphaned ingest passes (no active path)"
                );
            }
        }
        Err(e) => {
            task.report_error(format!("orphaned-pass heal failed: {e}"));
            repair_update(app, |repair| {
                repair.errors.push(format!("orphaned-pass heal: {e}"));
            });
            tracing::error!(error = %e, "startup doctor: orphaned-pass heal failed");
        }
    }

    // 3. Preview artifacts (disk vs DB) + stranded temp sweep. The library doctor
    //    previously ran only on the maintenance tick; running it at startup heals
    //    a mangled preview cache immediately (audit: "run the existence walk at
    //    open, not only on the 6h tick").
    task.report_progress(0.45, "checking preview cache");
    if task.is_cancelled() {
        tracing::info!("startup integrity report: cancelled before preview reconcile");
        repair_cancelled(app);
        return vector_reconciled.then_some(active_target);
    }
    match app.library.doctor_with_cancel(&task.cancel_flag()) {
        Ok(r) => {
            if r.cancelled {
                tracing::info!("startup integrity report: preview reconcile cancelled");
                repair_cancelled(app);
                return vector_reconciled.then_some(active_target);
            }
            repair_update(app, |repair| {
                repair.retention = Some(r.into());
            });
            app.lifecycle
                .set_health(Subsystem::Previews, SubsystemHealth::Healthy);
            tracing::info!(
                repended = r.repended,
                stale_orphans = r.stale_orphans,
                temps_swept = r.temps_swept,
                "startup integrity report: preview cache reconciled"
            );
        }
        Err(e) => {
            task.report_error(format!("preview reconcile failed: {e}"));
            repair_update(app, |repair| {
                repair.errors.push(format!("preview reconcile: {e}"));
            });
            app.lifecycle.set_health(
                Subsystem::Previews,
                SubsystemHealth::Degraded {
                    summary: e.to_string(),
                },
            );
            tracing::error!(error = %e, "startup doctor: preview reconcile failed");
        }
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
        task.report_progress(0.65, "reconciling library roots");
        if task.is_cancelled() {
            tracing::info!("startup integrity report: cancelled before filesystem reconcile");
            repair_cancelled(app);
            return vector_reconciled.then_some(active_target);
        }
        let _walk = app.scans.begin(); // guard: de-registers on every exit
        let opts = ScanOptions {
            cancel: Some(task.cancel_flag()),
            discovered: Some(app.scans.counter()),
            max_concurrency: Some(app.resources.budget().ingest_concurrency),
            pause: Some(app.resources.pause_token()),
            ..ScanOptions::default()
        };
        match app.library.reconcile_all(&opts) {
            Ok(reports) => {
                if task.is_cancelled() {
                    tracing::info!("startup integrity report: filesystem reconcile cancelled");
                    repair_cancelled(app);
                    return vector_reconciled.then_some(active_target);
                }
                let mut degraded = Vec::new();
                let summary = summarize_roots(&reports);
                for root in &reports {
                    match &root.outcome {
                        RootReconcileOutcome::Scanned(r) => {
                            if r.stale_inference_suppressed {
                                degraded.push(format!(
                                    "{}: incomplete filesystem walk ({} I/O errors)",
                                    root.root_id, r.io_errors
                                ));
                                task.report_error(format!(
                                    "root {} walk incomplete; stale inference suppressed",
                                    root.root_id
                                ));
                                tracing::warn!(
                                    root_id = %root.root_id,
                                    io_errors = r.io_errors,
                                    "startup doctor: incomplete root walk; preserved unseen paths"
                                );
                            }
                            // Log only roots with drift; a clean root at startup
                            // is the common case and must not spam the report.
                            if r.new_images + r.superseded + r.relinked + r.went_stale > 0 {
                                tracing::info!(
                                    root_id = %root.root_id,
                                    new_images = r.new_images,
                                    superseded = r.superseded,
                                    relinked = r.relinked,
                                    went_stale = r.went_stale,
                                    "startup doctor: reconciled offline filesystem changes"
                                );
                            }
                        }
                        RootReconcileOutcome::Offline { volume_id } => {
                            degraded.push(format!("{}: volume {volume_id} offline", root.root_id));
                            tracing::info!(
                                root_id = %root.root_id,
                                volume_id,
                                "startup doctor: root waits for its offline volume"
                            );
                        }
                        RootReconcileOutcome::Failed { error } => {
                            degraded.push(format!("{}: {error}", root.root_id));
                            task.report_error(format!(
                                "root {} reconcile failed: {error}",
                                root.root_id
                            ));
                            tracing::error!(
                                root_id = %root.root_id,
                                error,
                                "startup doctor: root reconcile failed; continuing"
                            );
                        }
                    }
                }
                let scanned = summary.scanned_roots;
                repair_update(app, |repair| {
                    repair.roots = Some(summary);
                });
                app.lifecycle.set_health(
                    Subsystem::Roots,
                    if degraded.is_empty() {
                        SubsystemHealth::Healthy
                    } else {
                        SubsystemHealth::Degraded {
                            summary: degraded.join("; "),
                        }
                    },
                );
                tracing::info!(
                    roots = reports.len(),
                    scanned,
                    degraded = degraded.len(),
                    "startup integrity report: filesystem reconciled"
                );
            }
            Err(e) => {
                task.report_error(format!("filesystem reconcile failed: {e}"));
                repair_update(app, |repair| {
                    repair.errors.push(format!("filesystem reconcile: {e}"));
                });
                app.lifecycle.set_health(
                    Subsystem::Roots,
                    SubsystemHealth::Degraded {
                        summary: e.to_string(),
                    },
                );
                tracing::error!(error = %e, "startup doctor: filesystem reconcile failed");
            }
        }
    }

    task.report_progress(1.0, "startup integrity checks complete");
    repair_update(app, |repair| {
        let roots_degraded = repair
            .roots
            .as_ref()
            .is_some_and(|roots| roots.degraded_roots > 0);
        repair.state = if repair.errors.is_empty() && !roots_degraded {
            "completed"
        } else {
            "degraded"
        };
        repair.completed_at_ms = Some(now_ms());
    });
    tracing::info!("startup integrity report: done");
    vector_reconciled.then_some(active_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_core::library::ScanReport;

    #[test]
    fn repair_snapshot_preserves_s5_and_relink_outcomes() {
        let retention = RetentionRepairSnapshot::from(DoctorReport {
            repended: 2,
            reclaimed_images: 3,
            preview_files_reclaimed: 4,
            vector_rows_reclaimed: 5,
            ..DoctorReport::default()
        });
        assert_eq!(retention.repended, 2);
        assert_eq!(retention.reclaimed_images, 3);
        assert_eq!(retention.preview_files_reclaimed, 4);
        assert_eq!(retention.vector_rows_reclaimed, 5);

        let roots = summarize_roots(&[
            RootReconcileResult {
                root_id: "healthy".into(),
                outcome: RootReconcileOutcome::Scanned(ScanReport {
                    relinked: 2,
                    retention_repairs_revived: 6,
                    ..ScanReport::default()
                }),
            },
            RootReconcileResult {
                root_id: "incomplete".into(),
                outcome: RootReconcileOutcome::Scanned(ScanReport {
                    io_errors: 1,
                    stale_inference_suppressed: true,
                    ..ScanReport::default()
                }),
            },
            RootReconcileResult {
                root_id: "offline".into(),
                outcome: RootReconcileOutcome::Offline {
                    volume_id: "volume".into(),
                },
            },
        ]);
        assert_eq!(roots.total_roots, 3);
        assert_eq!(roots.scanned_roots, 2);
        assert_eq!(roots.degraded_roots, 2);
        assert_eq!(roots.relinked, 2);
        assert_eq!(roots.retention_repairs_revived, 6);
        assert_eq!(roots.io_errors, 1);
    }
}
