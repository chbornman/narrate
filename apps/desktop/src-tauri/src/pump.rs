//! Background pumps: the ingest scheduler shell (LIBRARY B20 — the core
//! ships synchronous `process_queue` + `maintenance_tick`/`probe_volumes`
//! hooks; the shell drives them) and the sidecar debounce pump (SIDECARS S3).
//!
//! Event emission discipline (UI §7.4 / tauri #852): the ingest channel is
//! low-rate by construction — progress is emitted at most every
//! `PROGRESS_INTERVAL` and only when counters changed; payloads are four
//! integers.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use photoproof_core::UtcMillis;
use photoproof_core::library::{
    QueueOptions, RootReconcileOutcome, RootReconcileResult, ScanOptions,
};
use photoproof_core::runtime::RuntimeEvent;
use tauri::{AppHandle, Emitter, Runtime};

use crate::convergence::StateDomain;
use crate::dto::{IngestStatus, PassRemaining};
use crate::lifecycle::{Subsystem, SubsystemHealth};
use crate::managed_tasks::{SpawnTaskError, TaskPriority};
use crate::resource_governor::ResourceLane;
use crate::settings::NewRootPolicy;
use crate::state::App;

const QUEUE_BATCH: usize = 64;
/// P7.4 decision 5: the embedding drain's bounded batch. Embeddings are the
/// LOWEST backfill priority (L4 ordering) — a small batch keeps each idle
/// turn short so a freshly-arriving ingest item (a new photo, a new note)
/// preempts the embedding backfill on the next loop, and so the coalesced
/// jobs indicator updates promptly. Embedding a batch is seconds of CPU
/// (DFN5B image ~3 s each, spike), so this stays deliberately small.
const EMBED_BATCH: usize = 8;
/// The on-demand full-raw-decode drain's bounded batch (OD-1). A develop is
/// seconds of CPU + a full-sensor buffer in flight, and it is interactive
/// (a user waiting on a "developing..." spinner), so the batch is small: each
/// idle turn develops a couple, then the loop re-checks for a freshly-arriving
/// ingest item or an armed mic before the next. Cancel is checked PER ITEM
/// inside the drain, so a mic armed mid-batch preempts at once.
const DECODE_BATCH: usize = 2;
const IDLE_SLEEP: Duration = Duration::from_millis(500);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const DISK_INVENTORY_INTERVAL: Duration = Duration::from_secs(15 * 60);
const MAINTENANCE_HOURS: u64 = 6;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(MAINTENANCE_HOURS * 60 * 60);
const MAINTENANCE_IDLE_RETRY: Duration = Duration::from_secs(30);
const SIDECAR_TICK: Duration = Duration::from_millis(500);
/// Wall-clock gap between pump iterations that we treat as a sleep/resume
/// (AUDIT-2026-07-07 S2). WHY wall clock: `Instant` is suspend-paused on
/// macOS/Linux, so a laptop lid-close leaves `last_maintenance.elapsed()`
/// unmoved and the watcher's missed-while-asleep events invisible until the
/// next 10-minute tick; `SystemTime` keeps advancing through sleep, so a big
/// jump between iterations is the cross-platform wake signal (no OS wake
/// observer needed). WHY 2 minutes: iterations are normally paced by
/// IDLE_SLEEP (500 ms) or a bounded work batch — far below this — while any
/// real sleep worth reconciling is minutes-plus. A rare false positive (an
/// iteration genuinely stalled > 2 min) just triggers one redundant
/// reconcile, which is idempotent and arguably owed after such a stall.
const RESUME_WALL_GAP: Duration = Duration::from_secs(120);
/// Block-for-one-event timeout in the runtime pump's recv loop. The
/// timeout exists to re-check `app.shutdown`, not to pace events —
/// events wake the loop immediately; this only bounds quit latency.
const RUNTIME_PUMP_TICK: Duration = Duration::from_millis(500);

/// EMA smoothing factor for the per-pass throughput. 0.3 weights the newest
/// instantaneous sample at 30% and the running average at 70%: responsive
/// enough that the rate climbs/falls within a few 400 ms emits when ingest
/// speed genuinely shifts, smooth enough that one fat or thin batch does not
/// jerk the ETA around. (Chosen to match the human-perceived "settling in a
/// second or two" feel; not load-bearing, easy to retune.)
const RATE_ALPHA: f32 = 0.3;

fn progress_emit_due(has_previous: bool, changed: bool, elapsed: Duration) -> bool {
    !has_previous || (changed && elapsed >= PROGRESS_INTERVAL)
}

/// Quantum (items/sec) for emit-gating the otherwise-continuous rate. A change
/// smaller than this does not, on its own, force a progress emit — without it a
/// 2.5/s vs 2.50001/s EMA wobble would re-trigger a 400 ms emit forever even
/// when no structural progress (done/total/remaining) changed. 0.5/s is below
/// what a human reads off a moving throughput number.
const RATE_QUANTUM: f32 = 0.5;

/// Per-pass throughput tracker: the smoothed rate plus the last sample we
/// folded (done count + wall-clock instant), so the next tick can form a
/// done-delta / secs instantaneous rate. Lives in the pump loop across
/// iterations, keyed by pass name.
struct RateEma {
    /// Smoothed items/sec; 0.0 until the first real (unpaused, positive-delta)
    /// sample folds in.
    ema: f32,
    /// `done` count at the last sample — the baseline for the next delta.
    last_done: u64,
    /// Wall-clock instant of the last sample — the baseline for the next secs.
    last_at: Instant,
}

/// Pure EMA fold, factored out so it is testable without a clock or an `App`.
/// Returns the next smoothed rate given the previous EMA, the done-delta and
/// elapsed seconds since the last sample, and whether the pass is paused.
///
/// FREEZE-ON-PAUSE: a paused sample returns the previous EMA unchanged (the
/// caller must NOT advance `last_done`/`last_at` either), so a long mic-armed
/// or offline-volume window holds the rate steady and the ETA stays meaningful.
/// A non-positive delta or a non-positive interval is treated as "no
/// information" and also returns the previous EMA — this is what clamps the
/// resume sample: even if the caller did advance the baseline across a pause,
/// the very next fold sees done_delta==0 (no work happened while frozen) and
/// cannot register a giant spurious burst.
fn fold_rate_ema(prev_ema: f32, done_delta: u64, secs: f32, paused: bool) -> f32 {
    if paused || done_delta == 0 || secs <= 0.0 {
        return prev_ema;
    }
    let instant = done_delta as f32 / secs;
    RATE_ALPHA * instant + (1.0 - RATE_ALPHA) * prev_ema
}

/// Fold this tick's per-pass `done` counts into the rate trackers and write the
/// resulting smoothed rate back onto each `PassRemaining`. `now` and `paused`
/// are injected (production passes `Instant::now()` + the live pause signal;
/// tests pass a fake clock) so the timing is the caller's, not buried here.
///
/// PAUSE handling: while `paused`, EMAs are frozen (not decayed) AND the
/// baselines (`last_done`/`last_at`) are NOT advanced — so on resume the first
/// unpaused tick measures the delta from BEFORE the pause over the real elapsed
/// time, which `fold_rate_ema` further guards (a zero work-delta across the
/// freeze cannot spike). PRUNE: trackers for passes no longer in the status
/// (finished kinds dropped out of the breakdown) are removed so the map cannot
/// grow unbounded across a long session of many one-shot passes.
fn apply_pass_rates(
    trackers: &mut HashMap<String, RateEma>,
    status: &mut IngestStatus,
    now: Instant,
    paused: bool,
) {
    for pass in &mut status.passes {
        match trackers.get_mut(&pass.name) {
            Some(t) => {
                if !paused {
                    // Wall-clock delta + work delta since the last fold. The
                    // done count is monotonic per kind, but guard the subtract
                    // anyway (a version reset could lower it) so we never form
                    // a bogus huge delta.
                    let secs = now.duration_since(t.last_at).as_secs_f32();
                    let done_delta = pass.done.saturating_sub(t.last_done);
                    t.ema = fold_rate_ema(t.ema, done_delta, secs, false);
                    // Advance the baseline ONLY when not paused, so a paused
                    // span never becomes a single giant delta on resume.
                    t.last_done = pass.done;
                    t.last_at = now;
                }
                pass.rate_per_sec = t.ema;
            }
            None => {
                // First sighting of this pass: seed the baseline, no rate yet
                // (need two samples to measure throughput). 0.0 = unknown.
                trackers.insert(
                    pass.name.clone(),
                    RateEma {
                        ema: 0.0,
                        last_done: pass.done,
                        last_at: now,
                    },
                );
                pass.rate_per_sec = 0.0;
            }
        }
    }
    // Prune trackers whose pass disappeared (finished) this tick.
    let live: std::collections::HashSet<&str> =
        status.passes.iter().map(|p| p.name.as_str()).collect();
    trackers.retain(|name, _| live.contains(name.as_str()));
}

/// True when any pass's throughput moved by at least `RATE_QUANTUM` between two
/// statuses (matched by name). Used only for emit-gating — it lets a real rate
/// shift surface on the wire while the sub-quantum EMA jitter (excluded from the
/// DTO's `PartialEq`) does NOT keep re-arming the 400 ms emit.
fn rate_quantum_changed(prev: &IngestStatus, next: &IngestStatus) -> bool {
    next.passes.iter().any(|p| {
        let before = prev
            .passes
            .iter()
            .find(|q| q.name == p.name)
            .map_or(0.0, |q| q.rate_per_sec);
        (p.rate_per_sec - before).abs() >= RATE_QUANTUM
    })
}

pub fn ingest_status(app: &App) -> IngestStatus {
    let queue = match app.library.active_pass_counters() {
        Ok(c) => status_from_counters(&c),
        Err(_) => IngestStatus::default(),
    };
    let (scanning, discovered) = app.scans.snapshot();
    let mut status = overlay_walk(queue, scanning, discovered);
    let resources = app.resources.snapshot();
    status.processing_paused = resources.paused;
    status.processing_intensity = resources.intensity;
    // Warn surface (founder: warn + pause): list offline volumes the library
    // lives on so the shell can say "drive disconnected, N photos unavailable"
    // instead of silently stalling. A read failure degrades to "no warning"
    // (never blocks the status the rest of the header needs).
    status.offline_volumes = app
        .library
        .offline_volume_burden()
        .unwrap_or_default()
        .into_iter()
        .map(|(label, images)| crate::dto::OfflineVolume { label, images })
        .collect();
    // Seam 1: carry the vector-store version so the pump's `prev != status`
    // emit-gate fires when a committed vector write advances it, refreshing
    // views over the existing channel instead of polling.
    status.vectors_version = app.vectors.vectors_version();
    // Seam 1 (siblings): the image-set + journal versions ride the same gate so
    // the grid re-lists on a NEW image and the inspector re-reads on a journal
    // mutation — over this one channel, replacing the bespoke 2s grid throttle
    // and the journal membership-test relist.
    status.images_version = app.library.images_version();
    status.journal_version = app.store.journal_version();
    status
}

/// Overlay the LIVE walk state onto the queue-derived status. Pass rows
/// only materialize at hash time — and hashing starts only after the full
/// walk (size-ordered queue, §1.2) — so the queue counters read idle for
/// the WHOLE walk of a slow volume. Folding the walk into `running` keeps
/// every "work is pending" consumer honest at once: the header pill, the
/// empty-state copy, the mid-scan grid re-list (founder, June 2026: "No
/// photographs" shown over a folder busily being scanned). The changing
/// `discovered` count also makes the pump's `!=` change detection emit
/// during the walk, on the same coalesced cadence as everything else.
fn overlay_walk(mut s: IngestStatus, scanning: bool, discovered: u64) -> IngestStatus {
    s.scanning = scanning;
    s.discovered = discovered;
    s.running = s.running || scanning;
    s
}

/// Pure fold from the queue's pass counters to the wire status — split
/// from `ingest_status` so the aggregation is unit-testable without an
/// `App` (the same pure-controller shape as logic/jobs.ts on the TS side).
fn status_from_counters(
    counters: &std::collections::BTreeMap<(String, i64), photoproof_core::library::PassCounters>,
) -> IngestStatus {
    let mut s = IngestStatus::default();
    // Per-KIND breakdown, versions summed (the header pill names kinds; a
    // version bump re-running a pass is the same kind of work). We carry
    // remaining AND done/total per kind so the digest UI can draw a per-pass
    // bar — same summing rule as the aggregate fields below. BTreeMap keeps
    // the breakdown order deterministic for `!=` change detection.
    //
    // (done, total, remaining) per kind. `done` = done + skipped (matches the
    // aggregate `done`); `total` = every known unit for the kind; `remaining`
    // = pending + running (errored rows are NOT remaining work, or the
    // "digesting" pill would stay lit forever on a library with failed passes).
    let mut per_pass = std::collections::BTreeMap::<&str, (u64, u64, u64)>::new();
    for ((name, _version), c) in counters {
        let done = c.done + c.skipped;
        let total = c.pending + c.running + c.done + c.error + c.skipped;
        let queued = c.pending + c.running;
        s.done += done;
        s.total += total;
        s.errors += c.error;
        // Only kinds with queued work appear in the breakdown (the pill lists
        // still-digesting kinds). A finished kind drops out — which is also
        // what prunes its rate tracker in the pump.
        if queued > 0 {
            let e = per_pass.entry(name.as_str()).or_default();
            e.0 += done;
            e.1 += total;
            e.2 += queued;
        }
    }
    s.passes = per_pass
        .into_iter()
        .map(|(name, (done, total, remaining))| PassRemaining {
            name: name.to_owned(),
            remaining,
            done,
            total,
            // The pump fills the real EMA in before emit; the pure fold has no
            // clock/prev-state, so it cannot compute a rate. 0.0 = unknown.
            rate_per_sec: 0.0,
        })
        .collect();
    s.running = s.total > s.done + s.errors;
    s
}

/// Pure sleep/resume decision (AUDIT-2026-07-07 S2), factored out of the pump
/// loop so it is testable without real clocks: did the machine plausibly
/// suspend between two consecutive iterations? A clock that went BACKWARDS
/// (NTP step, manual adjustment) is a clock correction, not a suspend, and
/// must return false — otherwise every backwards step near a scan would risk
/// a spurious full-library walk.
fn wall_gap_signals_resume(prev: SystemTime, now: SystemTime) -> bool {
    now.duration_since(prev)
        .is_ok_and(|gap| gap >= RESUME_WALL_GAP)
}

fn should_run_idle_maintenance(
    elapsed: Duration,
    ingest_running: bool,
    scanning: bool,
    capture_live: bool,
) -> bool {
    elapsed >= MAINTENANCE_INTERVAL && !ingest_running && !scanning && !capture_live
}

fn observe_store_maintenance(
    disk: &crate::disk::DiskGovernor,
    result: Result<(), photoproof_core::StoreError>,
) -> Result<(), photoproof_core::StoreError> {
    match &result {
        Ok(()) => {
            disk.record_wal_maintenance_success();
        }
        Err(error) => {
            disk.record_wal_maintenance_failure(
                error.to_string(),
                matches!(error, photoproof_core::StoreError::CheckpointBlocked),
            );
        }
    }
    result
}

fn log_reconcile_outcomes(trigger: &str, reports: &[RootReconcileResult]) {
    for report in reports {
        match &report.outcome {
            RootReconcileOutcome::Scanned(scan) if scan.stale_inference_suppressed => {
                tracing::warn!(
                    root_id = %report.root_id,
                    io_errors = scan.io_errors,
                    %trigger,
                    "root reconcile was incomplete; unseen paths were preserved"
                );
            }
            RootReconcileOutcome::Scanned(_) => {}
            RootReconcileOutcome::Offline { volume_id } => {
                tracing::warn!(
                    root_id = %report.root_id,
                    %volume_id,
                    %trigger,
                    "root reconcile deferred because its volume is offline"
                );
            }
            RootReconcileOutcome::Failed { error } => {
                tracing::error!(
                    root_id = %report.root_id,
                    %error,
                    %trigger,
                    "root reconcile failed"
                );
            }
        }
    }
}

/// Drives only the latency-sensitive essential ingest queue and publishes the
/// aggregate status snapshot. Preview, interactive RAW, and embedding work
/// each have their own independently paced lane below; a slow derived batch
/// therefore cannot hold this loop (or another derived kind) hostage.
pub fn spawn_ingest_pump(app: &Arc<App>, handle: AppHandle) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "scheduler",
        "ingest-pump",
        TaskPriority::Background,
        move |task| {
            let mut last_emit: Option<(Instant, IngestStatus)> = None;
            // Per-pass throughput trackers, persisted across loop iterations
            // (the loop owns the clock + prev state the rate needs). Pruned
            // inside `apply_pass_rates` when a pass finishes.
            let mut rate_trackers: HashMap<String, RateEma> = HashMap::new();
            // S2 sleep/resume detection state: the previous iteration's WALL
            // time (see RESUME_WALL_GAP for why not `Instant`), plus a
            // registry single-flight key so rapid wake/sleep cycles cannot
            // stack concurrent full-library reconciles.
            let mut last_wall = SystemTime::now();
            loop {
                if task.is_cancelled() || pump_app.shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                // S2: a large wall-clock gap since the previous iteration means
                // the machine slept — the notify watcher silently missed every
                // filesystem event in between, so force the §7.3 wake reconcile
                // now instead of waiting out the 10-minute maintenance tick.
                let now_wall = SystemTime::now();
                if wall_gap_signals_resume(last_wall, now_wall) {
                    tracing::info!(
                        gap_secs = now_wall
                            .duration_since(last_wall)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        "wall-clock gap between pump iterations: treating as \
                         sleep/resume, reconciling roots"
                    );
                    // Off-thread: reconcile_all walks every root and can take
                    // minutes on a big library, and the pump must keep draining
                    // the queue and emitting progress meanwhile. Registered
                    // with `app.scans` (the add-root/rescan idiom) so the walk
                    // is visible as scanning/discovered in `ingest_status`.
                    let resume_app = Arc::clone(&pump_app);
                    let spawn = pump_app.tasks.spawn(
                        "library",
                        "resume-reconcile",
                        TaskPriority::Maintenance,
                        move |scan_task| {
                            let cancel = scan_task.cancel_flag();
                            let Some(_resource) = resume_app
                                .resources
                                .acquire(ResourceLane::RootScan, &cancel)
                            else {
                                return Ok(());
                            };
                            let _walk = resume_app.scans.begin();
                            let opts = ScanOptions {
                                cancel: Some(cancel),
                                discovered: Some(resume_app.scans.counter()),
                                max_concurrency: Some(
                                    resume_app.resources.budget().ingest_concurrency,
                                ),
                                pause: Some(resume_app.resources.pause_token()),
                                ..ScanOptions::default()
                            };
                            match resume_app.library.on_system_resume(&opts) {
                                Ok(reports) => log_reconcile_outcomes("resume", &reports),
                                Err(e) => {
                                    tracing::warn!(error = %e, "resume reconcile failed");
                                }
                            }
                            Ok(())
                        },
                    );
                    if let Err(error) = spawn
                        && !matches!(error, SpawnTaskError::AlreadyRunning { .. })
                    {
                        tracing::warn!(%error, "resume reconcile task unavailable");
                    }
                }
                last_wall = now_wall;
                let report = if pump_app.resources.paused() {
                    Default::default()
                } else {
                    let budget = pump_app.resources.budget();
                    let Some(metadata_resource) = pump_app
                        .resources
                        .acquire(ResourceLane::LiveIngest, &task.cancel_flag())
                    else {
                        return Ok(());
                    };
                    let queue_options = QueueOptions {
                        cancel: Some(task.cancel_flag()),
                        additional_cancel: None,
                        max_items: Some(budget.ingest_batch.min(QUEUE_BATCH)),
                        max_concurrency: Some(budget.ingest_concurrency),
                        excluded_embedding_root_ids: Vec::new(),
                    };
                    let report = match pump_app.library.process_essential_queue(&queue_options) {
                        Ok(report) => {
                            pump_app
                                .lifecycle
                                .set_health(Subsystem::Ingest, SubsystemHealth::Healthy);
                            report
                        }
                        Err(_) if task.is_cancelled() => return Ok(()),
                        Err(error) => {
                            task.report_error(format!("ingest queue batch failed: {error}"));
                            pump_app.lifecycle.set_health(
                                Subsystem::Ingest,
                                SubsystemHealth::Degraded {
                                    summary: error.to_string(),
                                },
                            );
                            tracing::error!(%error, "ingest queue batch failed");
                            if task.wait_for_cancel(IDLE_SLEEP) {
                                return Ok(());
                            }
                            continue;
                        }
                    };
                    drop(metadata_resource);
                    report
                };
                let processed = report.processed;

                let mut status = ingest_status(&pump_app);
                // PAUSE signal for the rate EMAs: the mic owns the machine
                // (`capture_live`) OR an offline volume is holding back work
                // (no pass can drain bytes off a disconnected drive). Either
                // way `done` cannot advance for the right reasons, so we freeze
                // the rate rather than let it decay toward 0 — the ETA the user
                // reads must survive the pause.
                let paused = pump_app.runtime.capture_live.load(Ordering::Relaxed)
                    || !status.offline_volumes.is_empty();
                apply_pass_rates(&mut rate_trackers, &mut status, Instant::now(), paused);
                let due = match &last_emit {
                    None => progress_emit_due(false, true, Duration::ZERO),
                    // `prev != status` already ignores `rate_per_sec` (the DTO's
                    // hand-written eq excludes the drifting float). To still let
                    // a MEANINGFUL rate move surface, also emit when the
                    // quantized rate of any pass changed — quantizing kills the
                    // 2.5/s vs 2.50001/s wobble that would otherwise pin the
                    // channel at one emit per PROGRESS_INTERVAL forever.
                    Some((at, prev)) => progress_emit_due(
                        true,
                        *prev != status || rate_quantum_changed(prev, &status),
                        at.elapsed(),
                    ),
                };
                if due {
                    // `passes` made the status non-Copy: clone for the
                    // wire, keep the original as the change-detection prev.
                    let _ = handle.emit("ingest-progress", status.clone());
                    last_emit = Some((Instant::now(), status));
                }
                if processed == 0 && task.wait_for_cancel(IDLE_SLEEP) {
                    return Ok(());
                }
            }
        },
    )
}

/// Independently paced preview backfill. Resource admission retains the
/// process-wide priority policy, while the separate managed owner/key gives
/// this lane its own cancellation, health, errors, and shutdown acknowledgement.
pub fn spawn_preview_pump(app: &Arc<App>, handle: AppHandle) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "derived",
        "preview-pump",
        TaskPriority::Background,
        move |task| loop {
            if task.is_cancelled() || pump_app.shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            if pump_app.resources.paused() || pump_app.disk.derived_work_paused() {
                if task.wait_for_cancel(IDLE_SLEEP) {
                    return Ok(());
                }
                continue;
            }
            let cancel = task.cancel_flag();
            let Some(_resource) = pump_app.resources.acquire(ResourceLane::Preview, &cancel) else {
                return Ok(());
            };
            let budget = pump_app.resources.budget();
            let options = QueueOptions {
                cancel: Some(cancel),
                additional_cancel: None,
                max_items: Some(budget.ingest_batch.min(QUEUE_BATCH)),
                max_concurrency: Some(budget.ingest_concurrency),
                excluded_embedding_root_ids: Vec::new(),
            };
            let report = match pump_app.library.process_preview_queue(&options) {
                Ok(report) => {
                    pump_app
                        .lifecycle
                        .set_health(Subsystem::Previews, SubsystemHealth::Healthy);
                    report
                }
                Err(_) if task.is_cancelled() => return Ok(()),
                Err(error) => {
                    task.report_error(format!("preview queue batch failed: {error}"));
                    pump_app.lifecycle.set_health(
                        Subsystem::Previews,
                        SubsystemHealth::Degraded {
                            summary: error.to_string(),
                        },
                    );
                    tracing::error!(%error, "preview queue batch failed");
                    if task.wait_for_cancel(IDLE_SLEEP) {
                        return Ok(());
                    }
                    continue;
                }
            };
            if !report.completed_previews.is_empty() {
                let _ = handle.emit(
                    "previews-changed",
                    crate::dto::PreviewsChanged {
                        hashes: report
                            .completed_previews
                            .iter()
                            .map(|hash| hash.as_str().to_owned())
                            .collect(),
                    },
                );
            }
            if report.processed == 0 && task.wait_for_cancel(IDLE_SLEEP) {
                return Ok(());
            }
        },
    )
}

/// Interactive develops are not paced by live ingest or preview backlog. The
/// resource governor still gives this lane interactive priority and the drain
/// retains per-item capture preemption.
pub fn spawn_raw_decode_pump(app: &Arc<App>, handle: AppHandle) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "derived",
        "interactive-raw-pump",
        TaskPriority::Background,
        move |task| loop {
            if task.is_cancelled() || pump_app.shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            if pump_app.disk.derived_work_paused() {
                if task.wait_for_cancel(IDLE_SLEEP) {
                    return Ok(());
                }
                continue;
            }
            let processed = drain_raw_decode(&pump_app, &handle, &task.cancel_flag());
            if processed == 0 && task.wait_for_cancel(IDLE_SLEEP) {
                return Ok(());
            }
        },
    )
}

/// Lowest-priority embedding backfill, with independent pacing. A busy ingest
/// queue no longer starves it; the resource governor and capture policy remain
/// the admission authority for contention and microphone preemption.
pub fn spawn_embedding_pump(app: &Arc<App>) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "derived",
        "embedding-pump",
        TaskPriority::Background,
        move |task| loop {
            if task.is_cancelled() || pump_app.shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            if pump_app.resources.paused() || pump_app.disk.derived_work_paused() {
                if task.wait_for_cancel(IDLE_SLEEP) {
                    return Ok(());
                }
                continue;
            }
            let processed = drain_embeddings(&pump_app, &task.cancel_flag());
            if processed == 0 && task.wait_for_cancel(IDLE_SLEEP) {
                return Ok(());
            }
        },
    )
}

/// Lightweight volume monitoring is isolated from the ingest queue. A slow
/// mount probe can delay only this lane, never live hashing, preview work, or
/// progress publication.
pub fn spawn_volume_monitor(app: &Arc<App>) -> Result<(), SpawnTaskError> {
    let monitor_app = Arc::clone(app);
    app.tasks.spawn(
        "monitor",
        "volume-probe",
        TaskPriority::Background,
        move |task| {
            let mut last_inventory = Instant::now()
                .checked_sub(DISK_INVENTORY_INTERVAL)
                .unwrap_or_else(Instant::now);
            let mut last_disk_state = None;
            let mut last_wal_state = None;
            loop {
                let disk = if last_inventory.elapsed() >= DISK_INVENTORY_INTERVAL {
                    last_inventory = Instant::now();
                    monitor_app.disk.refresh_inventory()
                } else {
                    monitor_app.disk.refresh_capacity()
                };
                let disk_state = (disk.app_data_state, disk.models_state);
                if last_disk_state != Some(disk_state) {
                    match disk.app_data_state {
                        crate::disk::CapacityState::Healthy => {
                            tracing::info!("app-data disk capacity is healthy")
                        }
                        crate::disk::CapacityState::Warning => tracing::warn!(
                            available_bytes =
                                disk.stores.first().and_then(|store| store.available_bytes),
                            "app-data disk space is low"
                        ),
                        crate::disk::CapacityState::Critical => tracing::error!(
                            available_bytes =
                                disk.stores.first().and_then(|store| store.available_bytes),
                            "app-data disk space is critical; derived work is paused"
                        ),
                        crate::disk::CapacityState::Unknown => {
                            tracing::warn!("app-data disk capacity could not be determined")
                        }
                    }
                    match disk.models_state {
                        crate::disk::CapacityState::Healthy => {}
                        crate::disk::CapacityState::Warning => {
                            tracing::warn!("configured model disk space is low")
                        }
                        crate::disk::CapacityState::Critical => {
                            tracing::error!("configured model disk space is critical")
                        }
                        crate::disk::CapacityState::Unknown => {
                            tracing::warn!("configured model disk capacity could not be determined")
                        }
                    }
                    last_disk_state = Some(disk_state);
                }
                if last_wal_state != Some(disk.wal.state) {
                    match disk.wal.state {
                        crate::disk::WalState::Healthy => {
                            tracing::info!(
                                wal_bytes = ?disk.wal.size_bytes,
                                "SQLite WAL health is healthy"
                            );
                        }
                        crate::disk::WalState::Warning => {
                            tracing::warn!(
                                wal_bytes = ?disk.wal.size_bytes,
                                wal_age_ms = ?disk.wal.age_ms,
                                last_error = ?disk.wal.last_maintenance_error,
                                "SQLite WAL exceeds a warning threshold or maintenance failed"
                            );
                        }
                        crate::disk::WalState::Critical => {
                            tracing::error!(
                                wal_bytes = ?disk.wal.size_bytes,
                                wal_age_ms = ?disk.wal.age_ms,
                                "SQLite WAL exceeds a critical size or age threshold"
                            );
                        }
                        crate::disk::WalState::Blocked => {
                            tracing::error!(
                                wal_bytes = ?disk.wal.size_bytes,
                                last_error = ?disk.wal.last_maintenance_error,
                                "SQLite WAL checkpoint is blocked by a reader"
                            );
                        }
                        crate::disk::WalState::Unknown => {
                            tracing::warn!(
                                error = ?disk.wal.inventory_error,
                                "SQLite WAL health could not be measured"
                            );
                        }
                    }
                    last_wal_state = Some(disk.wal.state);
                }
                if let Err(error) = monitor_app.library.probe_volumes() {
                    tracing::warn!(%error, "periodic volume probe failed");
                }
                if task.wait_for_cancel(PROBE_INTERVAL) {
                    return Ok(());
                }
            }
        },
    )
}

/// Six-hour repair/reconciliation and EventStore maintenance have their own
/// lane. Once due, maintenance waits for a genuine idle snapshot and retries
/// that admission check without resetting the six-hour deadline.
pub fn spawn_maintenance_pump(app: &Arc<App>) -> Result<(), SpawnTaskError> {
    let maintenance_app = Arc::clone(app);
    app.tasks.spawn(
        "maintenance",
        "library-and-store",
        TaskPriority::Maintenance,
        move |task| {
            let mut last_maintenance = Instant::now();
            loop {
                let elapsed = last_maintenance.elapsed();
                if elapsed < MAINTENANCE_INTERVAL
                    && task.wait_for_cancel(MAINTENANCE_INTERVAL - elapsed)
                {
                    return Ok(());
                }
                if task.is_cancelled() {
                    return Ok(());
                }

                let status = ingest_status(&maintenance_app);
                let capture_live = maintenance_app.runtime.capture_live.load(Ordering::Relaxed);
                if !should_run_idle_maintenance(
                    last_maintenance.elapsed(),
                    status.running,
                    status.scanning,
                    capture_live,
                ) {
                    if task.wait_for_cancel(MAINTENANCE_IDLE_RETRY) {
                        return Ok(());
                    }
                    continue;
                }
                if maintenance_app.disk.derived_work_paused() || maintenance_app.resources.paused()
                {
                    maintenance_app.lifecycle.set_health(
                        Subsystem::Maintenance,
                        SubsystemHealth::Degraded {
                            summary: if maintenance_app.resources.paused() {
                                "paused by processing policy".into()
                            } else {
                                "paused while app-data disk space is critical".into()
                            },
                        },
                    );
                    if task.wait_for_cancel(MAINTENANCE_IDLE_RETRY) {
                        return Ok(());
                    }
                    continue;
                }

                let cancel = task.cancel_flag();
                let Some(_resource) = maintenance_app
                    .resources
                    .acquire(ResourceLane::Maintenance, &cancel)
                else {
                    return Ok(());
                };
                let _walk = maintenance_app.scans.begin();
                let opts = ScanOptions {
                    cancel: Some(cancel),
                    discovered: Some(maintenance_app.scans.counter()),
                    max_concurrency: Some(maintenance_app.resources.budget().ingest_concurrency),
                    pause: Some(maintenance_app.resources.pause_token()),
                    ..ScanOptions::default()
                };
                let library_maintained = match maintenance_app
                    .library
                    .maintenance_tick_without_volume_probe(&opts)
                {
                    Ok(reports) => {
                        log_reconcile_outcomes("six-hour-maintenance", &reports);
                        true
                    }
                    Err(_) if task.is_cancelled() => return Ok(()),
                    Err(error) => {
                        task.report_error(format!("library maintenance failed: {error}"));
                        tracing::warn!(%error, "library maintenance tick failed");
                        false
                    }
                };
                if task.is_cancelled() {
                    return Ok(());
                }
                let store_maintained = match observe_store_maintenance(
                    &maintenance_app.disk,
                    maintenance_app.store.maintain(),
                ) {
                    Ok(()) => true,
                    Err(error) => {
                        task.report_error(format!("event-store maintenance failed: {error}"));
                        tracing::warn!(
                            %error,
                            "event-store idle maintenance deferred; next idle retry will rerun it"
                        );
                        false
                    }
                };
                if library_maintained && store_maintained {
                    maintenance_app
                        .lifecycle
                        .set_health(Subsystem::Maintenance, SubsystemHealth::Healthy);
                    last_maintenance = Instant::now();
                } else {
                    maintenance_app.lifecycle.set_health(
                        Subsystem::Maintenance,
                        SubsystemHealth::Degraded {
                            summary: "idle repair or WAL maintenance will retry".into(),
                        },
                    );
                    if task.wait_for_cancel(MAINTENANCE_IDLE_RETRY) {
                        return Ok(());
                    }
                }
            }
        },
    )
}

/// The per-model capture-pause policy (the GPU-embedder relax).
///
/// Today the scheduler reserves the machine for the CPU ASR while the mic is
/// armed by pausing background model work. The RIGHT discriminator is the
/// pass's EXECUTION PROVIDER, not the pass kind:
///
/// - A CPU embed pass (the int8/CPU EmbeddingGemma text embed per
///   docs/SPIKE-COREML-TEXT.md, or a CLIP fp32 CPU fallback) CONTENDS for the
///   same silicon as the CPU ASR -> it MUST pause (returns true).
/// - A GPU/ANE embed pass (the CoreML / CUDA / TensorRT CLIP image embed) does
///   NOT contend with the CPU ASR -> it must KEEP RUNNING (returns false).
///   (Founder, June 14 2026: "Once we do have the ML model set up on GPU then
///   we won't want to pause them while we're doing ASR.")
///
/// The GPU LLM stays paused during capture regardless — it contends for GPU
/// memory bandwidth + thermal headroom — but there is no background LLM drain in
/// the pump today (the LLM is request-driven), so this policy governs the
/// embedders, the only background model work the pump schedules.
///
/// Behavior is IDENTICAL on a pure-CPU machine: with no GPU/ANE EP available
/// `runs_on_accelerator` is false for every embedder, so everything still
/// pauses, exactly as before this relax.
fn should_pause_during_capture(runs_on_accelerator: bool) -> bool {
    !runs_on_accelerator
}

/// P7.4 decision 5: drain a bounded batch of the embedding backfill when the
/// embedders are ready. Returns the number of pass rows processed (0 = nothing
/// to do / paused / degraded).
///
/// CAPTURE PAUSE (per-model, RUNTIME §9): while `capture_live` the mic owns the
/// machine, so background model work that CONTENDS WITH THE CPU ASR pauses. Which
/// passes contend is decided by `should_pause_during_capture` on each embedder's
/// EXECUTION PROVIDER (not its kind):
///
/// - The TEXT embed (CPU per SPIKE-COREML-TEXT.md) pauses while armed.
/// - The CLIP IMAGE embed continues while armed IFF it is on a GPU/ANE EP
///   (CoreML/CUDA/TensorRT) — it no longer shares silicon with the CPU ASR.
///   On a CPU build the CLIP fallback is CPU too, so it pauses like today.
///
/// Two enforcement layers mirror the downloads posture (per-item, not per-batch):
///
/// 1. While armed we build a CLIP-ONLY rig (text = None) and only when the CLIP
///    EP is an accelerator; otherwise nothing starts (the all-CPU path is the
///    pre-relax behavior).
/// 2. `cancel` is wired to `capture_live` ONLY for the paused (not-armed) drain.
///    The armed GPU drain deliberately does NOT cancel on `capture_live` — that
///    flag is true for its whole duration, so wiring it would cancel the very
///    work we want to keep running. The batch bound (`EMBED_BATCH`) keeps each
///    armed turn short instead.
///
/// DEGRADED: with no embedder ready this returns 0 and the rows sit pending
/// — the journal is whole, the backfill is simply dark (RETRIEVAL §3 /
/// embedding.rs degraded contract).
fn drain_embeddings(app: &Arc<App>, cancel: &Arc<std::sync::atomic::AtomicBool>) -> usize {
    let Some(_resource) = app.resources.acquire(ResourceLane::Embedding, cancel) else {
        return 0;
    };
    let batch = app.resources.budget().embedding_batch;
    let armed = app.runtime.capture_live.load(Ordering::Relaxed);
    let (defer_text, defer_image, excluded_roots) = {
        let settings = app.settings.lock().expect("settings mutex");
        (
            settings.defer_text_embeddings,
            settings.defer_image_embeddings,
            settings
                .root_processing_policies
                .iter()
                .filter_map(|(root_id, policy)| {
                    matches!(
                        policy,
                        NewRootPolicy::PreviewOnly | NewRootPolicy::ProcessLater
                    )
                    .then_some(root_id.clone())
                })
                .collect::<Vec<_>>(),
        )
    };
    let text = (!defer_text)
        .then(|| app.runtime.embedders.text())
        .flatten();
    let clip = (!defer_image)
        .then(|| app.runtime.embedders.clip())
        .flatten();
    // Nothing ready ⇒ degraded; leave the rows pending, no claim, no error.
    if text.is_none() && clip.is_none() {
        return 0;
    }

    if armed {
        // While the mic is armed, run ONLY the passes that do not contend with
        // the CPU ASR: the GPU/ANE CLIP image embed. The text embed (CPU) and
        // the session-level text sweep stay paused (they are skipped because the
        // rig's `text` is None). If the CLIP embedder is on CPU (no GPU EP, or a
        // non-fp16 model), `should_pause_during_capture` is true and we start
        // nothing — the exact pre-relax behavior on a CPU machine.
        let Some(clip) = clip else { return 0 };
        if should_pause_during_capture(clip.runs_on_accelerator()) {
            return 0;
        }
        // CLIP-only rig: `process_embedding_queue` then claims only
        // ImageEmbedding passes and skips the text session-level sweep.
        let rig = photoproof_core::library::EmbeddingRig::<crate::embedders::EmbedderProxy> {
            text: None,
            clip: Some(clip.as_ref()),
            vectors: &app.vectors,
        };
        return run_embedding_drain(
            app,
            &rig,
            // No `capture_live` cancel here: it is true for the whole armed
            // window, so it would cancel the GPU work we WANT to keep draining.
            // The bounded batch keeps the armed turn short on its own.
            None,
            Arc::clone(cancel),
            batch,
            excluded_roots,
        );
    }

    // Not armed: full rig, both passes, with the capture cancel wired so that
    // arming the mic MID-drain preempts the CPU work at the next item.
    let rig = photoproof_core::library::EmbeddingRig {
        text: text.as_deref(),
        clip: clip.as_deref(),
        vectors: &app.vectors,
    };
    run_embedding_drain(
        app,
        &rig,
        Some(app.runtime.capture_live.clone()),
        Arc::clone(cancel),
        batch,
        excluded_roots,
    )
}

/// Run one bounded embedding-queue drain with the given cancel flag, logging and
/// swallowing drain-level errors (never crashes the pump). Shared by the armed
/// (GPU-only, no cancel) and not-armed (full rig, capture cancel) paths.
fn run_embedding_drain<TE: photoproof_connectors::Embedder, CE: photoproof_connectors::Embedder>(
    app: &App,
    rig: &photoproof_core::library::EmbeddingRig<'_, TE, CE>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    shutdown_cancel: Arc<std::sync::atomic::AtomicBool>,
    batch: usize,
    excluded_embedding_root_ids: Vec<String>,
) -> usize {
    match app.library.process_embedding_queue(
        rig,
        &QueueOptions {
            cancel,
            additional_cancel: Some(shutdown_cancel),
            max_items: Some(batch.min(EMBED_BATCH)),
            max_concurrency: Some(1),
            excluded_embedding_root_ids,
        },
    ) {
        Ok(report) => report.processed,
        Err(e) => {
            // A drain-level error (db/IO) is logged and the turn ends; the
            // per-item transient path inside the drain already handles model
            // failures without aborting. Never crashes the pump.
            tracing::warn!(error = %e, "embedding drain failed this turn");
            0
        }
    }
}

/// OD-1: drain a bounded batch of the on-demand full-raw-decode queue. Returns
/// the number of develop passes processed (0 = nothing requested / paused /
/// failed). Mirrors `drain_embeddings`' politeness, with the same two-layer
/// `capture_live` pause (no new batch while armed; `cancel` wired so a mic
/// armed mid-batch preempts at the next item — a develop is seconds, the §10.3
/// preempt bound). Completed develops emit `previews-changed` so Look swaps in
/// the developed full-res artifact the instant it lands.
fn drain_raw_decode(
    app: &Arc<App>,
    handle: &AppHandle,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> usize {
    // The mic owns the machine: no develop batch starts while armed.
    if app.runtime.capture_live.load(Ordering::Relaxed) {
        return 0;
    }
    let Some(_resource) = app.resources.acquire(ResourceLane::InteractiveRaw, cancel) else {
        return 0;
    };
    match app.library.process_raw_decode_queue(&QueueOptions {
        cancel: Some(app.runtime.capture_live.clone()),
        additional_cancel: Some(Arc::clone(cancel)),
        max_items: Some(app.resources.budget().raw_batch.min(DECODE_BATCH)),
        max_concurrency: Some(1),
        excluded_embedding_root_ids: Vec::new(),
    }) {
        Ok(report) => {
            // A developed RAW's display/thumb artifacts changed (source flips
            // to 'full-decode', needs_full_decode clears) AND its native
            // full-decode artifact is now servable — `previews-changed` tells
            // the grid/Look to refresh, exactly like the preview pass.
            if !report.completed_previews.is_empty() {
                let _ = handle.emit(
                    "previews-changed",
                    crate::dto::PreviewsChanged {
                        hashes: report
                            .completed_previews
                            .iter()
                            .map(|h| h.as_str().to_owned())
                            .collect(),
                    },
                );
                // DESIGN-PREVIEW-POLICY.md: a develop just wrote (at least) one
                // new full-res 1:1 artifact, so the 1:1 cache may now exceed
                // its budget — trim it back to the user's cap, evicting
                // least-recently-VIEWED first. We run it HERE (right after the
                // write, only when something landed) rather than on a schedule:
                // the cache can only grow on a develop, so this is exactly when
                // a check is warranted. SAFE — every evicted 1:1 re-derives on
                // next view.
                let budget = app
                    .settings
                    .lock()
                    .expect("settings mutex")
                    .preview_cache_budget_bytes;
                app.library.evict_preview_cache(budget);
            }
            report.processed
        }
        Err(e) => {
            tracing::warn!(error = %e, "full-raw-decode drain failed this turn");
            0
        }
    }
}

/// The sidecar debounce pump: one tick syncs the durable dirty queue into
/// the debouncer and flushes whatever is due (engine.pump). Shutdown flushes
/// happen in `App::shutdown`, not here.
///
/// The same tick drains §2.5 step-3 close processing (P6.2 obligation:
/// processors run on the PUMP, never inline on the close/quit path — a
/// quit-before-done re-enqueues on next launch by bookkeeping).
pub fn spawn_sidecar_pump(app: &Arc<App>) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "scheduler",
        "sidecar-pump",
        TaskPriority::Background,
        move |task| {
            loop {
                if task.wait_for_cancel(SIDECAR_TICK) || pump_app.shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Err(e) = pump_app.engine.pump(UtcMillis::now()) {
                    tracing::error!(error = %e, "sidecar pump error");
                }
                // Collections ride the same tick (RETRIEVAL §10.2: "the
                // same debounced writer that maintains sidecars"); a write
                // failure backs off inside the core writer and retries here.
                if let Err(e) = pump_app.collections.pump(UtcMillis::now()) {
                    tracing::error!(error = %e, "collections pump error");
                }
                if let Err(e) = pump_app.run_close_processing() {
                    tracing::error!(error = %e, "close processing error");
                }
            }
        },
    )
}

/// Committed runtime fingerprint for transitions that can land without a bus
/// event. In particular, the final download progress event may be observed
/// while the row is still `downloading` at 100%; registry commit + progress
/// cleanup happens immediately afterwards and must produce a terminal
/// `installed` snapshot on the next bounded tick. Full blocked reasons,
/// embedder attempts/generations, capability settlement, tier, and model
/// terminal/error states are included so a same-shape change cannot stay stale
/// in one window forever.
#[derive(Debug, PartialEq, Eq)]
struct ModelFp {
    id: String,
    state: String,
    downloaded_bytes: u64,
    error: Option<String>,
    operation: Option<String>,
    operation_sequence: Option<u64>,
    operation_phase: Option<String>,
    operation_terminal: Option<bool>,
    registry_error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct RuntimeFp {
    clip: crate::dto::EmbedderSlot,
    text: crate::dto::EmbedderSlot,
    asr_ready: bool,
    llm_ready: bool,
    asr_blocked: Option<String>,
    llm_blocked: Option<String>,
    capability_state: String,
    capability_summary: Option<String>,
    tier_detected: u8,
    tier_effective: u8,
    consent: String,
    models: Vec<ModelFp>,
}

fn runtime_fp(s: &crate::dto::RuntimeStatus) -> RuntimeFp {
    RuntimeFp {
        clip: s.clip.clone(),
        text: s.text_embedder.clone(),
        asr_ready: s.asr_ready,
        llm_ready: s.llm_ready,
        asr_blocked: s.asr_blocked.clone(),
        llm_blocked: s.llm_blocked.clone(),
        capability_state: s.capability_state.clone(),
        capability_summary: s.capability_summary.clone(),
        tier_detected: s.tier_detected,
        tier_effective: s.tier_effective,
        consent: s.consent.clone(),
        models: s
            .models
            .iter()
            .map(|model| ModelFp {
                id: model.id.clone(),
                state: model.state.clone(),
                downloaded_bytes: model.downloaded_bytes,
                error: model.error.clone(),
                operation: model.operation.clone(),
                operation_sequence: model.operation_event.as_ref().map(|event| event.sequence),
                operation_phase: model
                    .operation_event
                    .as_ref()
                    .map(|event| event.phase.clone()),
                operation_terminal: model.operation_event.as_ref().map(|event| event.terminal),
                registry_error: model.registry_error.clone(),
            })
            .collect(),
    }
}

/// One production broadcast seam for runtime snapshots. `AppHandle::emit`
/// targets every webview; keeping command mutations and the background pump
/// on this helper prevents a future caller from accidentally narrowing an
/// authoritative snapshot to the invoking Settings window.
pub(crate) fn emit_runtime_status<R: Runtime>(
    handle: &AppHandle<R>,
    status: crate::dto::RuntimeStatus,
) -> tauri::Result<()> {
    handle.emit("runtime-status", status)
}

/// Forward the sequenced operation event without exposing the Rust enum's
/// serialization shape as a frontend contract. Returns `false` for unrelated
/// bus events so the pump can call this on every drained event.
pub(crate) fn emit_model_operation<R: Runtime>(
    handle: &AppHandle<R>,
    event: &RuntimeEvent,
) -> tauri::Result<bool> {
    let RuntimeEvent::ModelOperation {
        model_id,
        attempt_id,
        sequence,
        phase,
        terminal,
        error,
    } = event
    else {
        return Ok(false);
    };
    handle.emit(
        "model-operation",
        serde_json::json!({
            "modelId": model_id,
            "attemptId": attempt_id,
            "sequence": sequence,
            "phase": phase,
            "terminal": terminal,
            "error": error,
        }),
    )?;
    Ok(true)
}

#[cfg(test)]
mod runtime_fingerprint_tests {
    use super::{ModelFp, RuntimeFp};
    use crate::dto::{EmbedderSlot, EmbedderState};

    fn idle_slot() -> EmbedderSlot {
        EmbedderSlot {
            state: EmbedderState::Idle,
            attempt_id: None,
            model_id: None,
            generation: 0,
            started_at: None,
            error: None,
            execution: None,
        }
    }

    fn fingerprint() -> RuntimeFp {
        RuntimeFp {
            clip: idle_slot(),
            text: idle_slot(),
            asr_ready: false,
            llm_ready: false,
            asr_blocked: None,
            llm_blocked: None,
            capability_state: "detecting".into(),
            capability_summary: None,
            tier_detected: 0,
            tier_effective: 0,
            consent: "download".into(),
            models: vec![ModelFp {
                id: "clip".into(),
                state: "downloading".into(),
                downloaded_bytes: 100,
                error: None,
                operation: None,
                operation_sequence: None,
                operation_phase: None,
                operation_terminal: None,
                registry_error: None,
            }],
        }
    }

    #[test]
    fn terminal_download_and_changed_block_reason_force_a_new_snapshot() {
        let downloading = fingerprint();
        let mut installed = fingerprint();
        installed.models[0].state = "installed".into();
        assert_ne!(downloading, installed);

        let mut first_reason = fingerprint();
        first_reason.asr_blocked = Some("binary missing".into());
        let mut changed_reason = fingerprint();
        changed_reason.asr_blocked = Some("binary is not executable".into());
        assert_ne!(
            first_reason, changed_reason,
            "reason text changes are product state, not just Option presence"
        );

        let mut verifying = fingerprint();
        verifying.models[0].operation = Some("verifying".into());
        assert_ne!(
            fingerprint(),
            verifying,
            "operation phases trigger a bounded cross-window snapshot"
        );
    }
}

#[cfg(test)]
mod runtime_broadcast_tests {
    use std::sync::{Arc, Mutex};

    use photoproof_core::runtime::RuntimeEvent;
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use tauri::{Listener, WebviewWindowBuilder};

    use super::{emit_model_operation, emit_runtime_status};
    use crate::runtime::RuntimeHost;

    fn payload_sink(
        window: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        event: &'static str,
    ) -> Arc<Mutex<Vec<String>>> {
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&payloads);
        window.listen(event, move |event| {
            sink.lock()
                .expect("payload sink")
                .push(event.payload().into());
        });
        payloads
    }

    #[test]
    fn runtime_and_model_operation_broadcasts_reach_two_webviews_with_identical_truth() {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let main = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("main webview");
        let settings = WebviewWindowBuilder::new(&app, "settings", Default::default())
            .build()
            .expect("settings webview");
        let main_runtime = payload_sink(&main, "runtime-status");
        let settings_runtime = payload_sink(&settings, "runtime-status");
        let main_operation = payload_sink(&main, "model-operation");
        let settings_operation = payload_sink(&settings, "model-operation");

        let temp = tempfile::tempdir().expect("tempdir");
        let status = RuntimeHost::init(temp.path().join("app-data")).status();
        emit_runtime_status(app.handle(), status).expect("runtime broadcast");
        let operation = RuntimeEvent::ModelOperation {
            model_id: "fixture-model".into(),
            attempt_id: "fixture-attempt".into(),
            sequence: 7,
            phase: "installing".into(),
            terminal: false,
            error: None,
        };
        assert!(emit_model_operation(app.handle(), &operation).expect("operation broadcast"));

        let main_runtime = main_runtime.lock().expect("main runtime").clone();
        let settings_runtime = settings_runtime.lock().expect("settings runtime").clone();
        assert_eq!(main_runtime.len(), 1);
        assert_eq!(main_runtime, settings_runtime);
        let runtime: serde_json::Value =
            serde_json::from_str(&main_runtime[0]).expect("runtime payload");
        assert_eq!(runtime["capabilityState"], "provisional");

        let main_operation = main_operation.lock().expect("main operation").clone();
        let settings_operation = settings_operation
            .lock()
            .expect("settings operation")
            .clone();
        assert_eq!(main_operation.len(), 1);
        assert_eq!(main_operation, settings_operation);
        let operation: serde_json::Value =
            serde_json::from_str(&main_operation[0]).expect("operation payload");
        assert_eq!(operation["modelId"], "fixture-model");
        assert_eq!(operation["attemptId"], "fixture-attempt");
        assert_eq!(operation["sequence"], 7);
        assert_eq!(operation["phase"], "installing");
        assert_eq!(operation["terminal"], false);
    }
}

/// The runtime pump (RUNTIME §8.3): forwards core-bus events to the
/// webview as coalesced `runtime-status` snapshots — readiness changes,
/// state transitions, download progress. Payloads stay snapshot-shaped
/// and low-rate (UI §7.4 wire discipline / tauri #852). An idle-tick refresh
/// (gated on `runtime_fp`) catches committed no-bus-event transitions.
pub fn spawn_runtime_pump(app: &Arc<App>, handle: AppHandle) -> Result<(), SpawnTaskError> {
    let pump_app = Arc::clone(app);
    app.tasks.spawn(
        "scheduler",
        "runtime-pump",
        TaskPriority::Background,
        move |task| {
            let rx = pump_app.runtime.bus.subscribe();
            // Last-emitted READINESS fingerprint. The embedder host lands a slot
            // Ready/Failed on a background build thread and publishes NO bus
            // event, so without a timeout-side refresh the webview freezes on the
            // last 'building' snapshot forever (the embedder-loading-that-never-
            // finishes bug). We re-emit on the idle tick ONLY when this changed,
            // so an idle runtime still stays quiet (no 2/s snapshot spam).
            let mut last_runtime: Option<RuntimeFp> = None;
            loop {
                if task.is_cancelled() || pump_app.shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                // Block for one event, then drain the burst (coalesce).
                match rx.recv_timeout(RUNTIME_PUMP_TICK) {
                    Ok(first) => {
                        let mut events = vec![first];
                        while let Ok(e) = rx.try_recv() {
                            events.push(e);
                        }
                        for e in &events {
                            let _ = emit_model_operation(&handle, e);
                            if let photoproof_core::runtime::RuntimeEvent::DownloadProgress {
                                model_id,
                                downloaded_bytes,
                                total_bytes,
                            } = e
                            {
                                pump_app.runtime.note_progress(
                                    model_id,
                                    *downloaded_bytes,
                                    *total_bytes,
                                );
                            }
                        }
                        let status = pump_app.runtime.status();
                        if let Err(error) = pump_app.request_active_vector_reconcile()
                            && !matches!(error, SpawnTaskError::AlreadyRunning { .. })
                        {
                            tracing::warn!(%error, "active vector reconcile unavailable");
                        }
                        last_runtime = Some(runtime_fp(&status));
                        let _ = emit_runtime_status(&handle, status);
                        pump_app
                            .convergence
                            .publish(&handle, [StateDomain::Runtime]);
                    }
                    // Timeout (or a disconnect, handled by the top-of-loop
                    // shutdown/try_state checks): no bus event fired, but a
                    // silent slot transition may have. Re-emit only on a
                    // readiness change so the UI self-corrects within one tick.
                    Err(_) => {
                        let status = pump_app.runtime.status();
                        if let Err(error) = pump_app.request_active_vector_reconcile()
                            && !matches!(error, SpawnTaskError::AlreadyRunning { .. })
                        {
                            tracing::warn!(%error, "active vector reconcile unavailable");
                        }
                        let fp = runtime_fp(&status);
                        if last_runtime.as_ref() != Some(&fp) {
                            last_runtime = Some(fp);
                            let _ = emit_runtime_status(&handle, status);
                            pump_app
                                .convergence
                                .publish(&handle, [StateDomain::Runtime]);
                        }
                    }
                }
            }
        },
    )
}

/// B52 / CAPTURE §2.5: the REAL bounded wait for trailing finals at quit.
/// The engine never sleeps — it enforces the 5 s drain deadline on its
/// own clock; this pump-side loop owns the blocking wait between pumps.
/// `wait` is the seam: production passes a short real sleep, tests
/// advance a fake clock — so the loop's bound is the ENGINE's deadline,
/// not wall-clock luck. Returns the number of trailing finals minted.
pub fn drain_capture_at_quit<C: photoproof_core::capture::Clock>(
    engine: &mut photoproof_core::capture::CaptureEngine<'_, C>,
    store: &photoproof_core::EventStore,
    wait: &mut dyn FnMut(),
) -> usize {
    let mut minted = engine.disarm(store).len();
    while engine.stream_open() {
        wait();
        minted += engine.pump(store).len();
    }
    minted
}

#[cfg(test)]
mod resume_gap_tests {
    use std::time::{Duration, SystemTime};

    use super::{RESUME_WALL_GAP, wall_gap_signals_resume};

    /// AUDIT-2026-07-07 S2: the sleep/resume decision. Normal pacing (the
    /// 500 ms idle tick, even a slow multi-second batch) stays far below the
    /// threshold; at/above it a resume reconcile fires. Boundary pinned at
    /// exactly RESUME_WALL_GAP so a future threshold tweak is deliberate.
    #[test]
    fn gap_at_or_above_threshold_signals_resume() {
        let prev = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        assert!(
            !wall_gap_signals_resume(prev, prev + Duration::from_millis(500)),
            "the idle tick is not a resume"
        );
        assert!(
            !wall_gap_signals_resume(prev, prev + RESUME_WALL_GAP - Duration::from_secs(1)),
            "just under the threshold: still normal pacing"
        );
        assert!(
            wall_gap_signals_resume(prev, prev + RESUME_WALL_GAP),
            "the threshold itself fires"
        );
        assert!(
            wall_gap_signals_resume(prev, prev + Duration::from_secs(8 * 3600)),
            "an overnight sleep fires"
        );
    }

    /// A wall clock that stepped BACKWARDS (NTP correction, manual change) is
    /// not a suspend and must never trigger a full-library walk.
    #[test]
    fn backwards_clock_is_not_a_resume() {
        let prev = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        assert!(!wall_gap_signals_resume(
            prev,
            prev - Duration::from_secs(3600)
        ));
    }
}

#[cfg(test)]
mod idle_maintenance_tests {
    use std::time::Duration;

    use super::{
        MAINTENANCE_HOURS, MAINTENANCE_INTERVAL, observe_store_maintenance,
        should_run_idle_maintenance,
    };

    #[test]
    fn maintenance_requires_both_due_cadence_and_a_fully_idle_turn() {
        assert_eq!(MAINTENANCE_HOURS, 6);
        assert_eq!(MAINTENANCE_INTERVAL, Duration::from_secs(21_600));
        assert!(!should_run_idle_maintenance(
            MAINTENANCE_INTERVAL - Duration::from_millis(1),
            false,
            false,
            false
        ));
        assert!(should_run_idle_maintenance(
            MAINTENANCE_INTERVAL,
            false,
            false,
            false
        ));
        assert!(!should_run_idle_maintenance(
            MAINTENANCE_INTERVAL,
            true,
            false,
            false
        ));
        assert!(!should_run_idle_maintenance(
            MAINTENANCE_INTERVAL,
            false,
            true,
            false
        ));
        assert!(!should_run_idle_maintenance(
            MAINTENANCE_INTERVAL,
            false,
            false,
            true
        ));
    }

    #[test]
    fn blocked_reader_is_wal_health_until_the_next_idle_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("photoproof.db");
        let store = photoproof_core::EventStore::open(&db).unwrap();
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE wal_health_probe (value INTEGER NOT NULL);
                 INSERT INTO wal_health_probe VALUES (1);",
            )
            .unwrap();
        let blocker = rusqlite::Connection::open(&db).unwrap();
        blocker.execute_batch("BEGIN").unwrap();
        let _: i64 = blocker
            .query_row("SELECT COUNT(*) FROM wal_health_probe", [], |row| {
                row.get(0)
            })
            .unwrap();
        writer
            .execute("INSERT INTO wal_health_probe VALUES (2)", [])
            .unwrap();
        drop(writer);

        let disk =
            crate::disk::DiskGovernor::new(dir.path().to_path_buf(), dir.path().join("models"));
        let blocked = observe_store_maintenance(&disk, store.maintain());
        assert!(matches!(
            blocked,
            Err(photoproof_core::StoreError::CheckpointBlocked)
        ));
        let blocked_health = disk.snapshot().wal;
        assert_eq!(blocked_health.state, crate::disk::WalState::Blocked);
        assert!(blocked_health.blocked_by_reader);
        assert!(blocked_health.last_maintenance_failure_at_ms.is_some());
        assert!(
            blocked_health
                .last_maintenance_error
                .as_deref()
                .is_some_and(|error| error.contains("wal_checkpoint"))
        );

        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        observe_store_maintenance(&disk, store.maintain()).unwrap();
        let recovered = disk.snapshot().wal;
        assert_eq!(recovered.state, crate::disk::WalState::Healthy);
        assert!(!recovered.blocked_by_reader);
        assert!(recovered.last_maintenance_success_at_ms.is_some());
        assert!(
            recovered.last_maintenance_failure_at_ms.is_some(),
            "recovery keeps the last failure timestamp for diagnostics"
        );
    }
}

#[cfg(test)]
mod capture_pause_policy_tests {
    use super::should_pause_during_capture;

    /// The per-model capture-pause policy keyed on a pass's execution provider.
    /// These cases pin the founder's rule (June 14 2026) so a future refactor
    /// cannot silently re-pause GPU embedders or un-pause CPU work.
    #[test]
    fn gpu_ane_embed_keeps_running_cpu_work_pauses() {
        // GPU/ANE CLIP image embed (CoreML / CUDA / TensorRT): does NOT contend
        // with the CPU ASR -> must KEEP RUNNING during capture.
        assert!(
            !should_pause_during_capture(true),
            "a GPU/ANE embed pass must keep draining while the mic is armed"
        );

        // CPU text embed (EmbeddingGemma int8/CPU) AND the all-CPU CLIP fallback:
        // contend with the CPU ASR -> must PAUSE during capture. This is also the
        // exact pre-relax behavior on a pure-CPU machine (no GPU EP), which keeps
        // the relax behavior-identical when no accelerator is present.
        assert!(
            should_pause_during_capture(false),
            "a CPU embed pass must still pause while the mic is armed"
        );
    }
}

#[cfg(test)]
mod status_tests {
    use std::collections::BTreeMap;

    use photoproof_core::library::PassCounters;

    use super::status_from_counters;
    use crate::dto::PassRemaining;

    fn counters(rows: &[(&str, i64, PassCounters)]) -> BTreeMap<(String, i64), PassCounters> {
        rows.iter()
            .map(|(name, version, c)| (((*name).to_owned(), *version), *c))
            .collect()
    }

    fn c(pending: u64, running: u64, done: u64, error: u64, skipped: u64) -> PassCounters {
        PassCounters {
            pending,
            running,
            done,
            error,
            skipped,
        }
    }

    #[test]
    fn empty_counters_mean_idle() {
        let s = status_from_counters(&BTreeMap::new());
        assert!(!s.running);
        assert!(s.passes.is_empty());
        assert_eq!((s.done, s.total, s.errors), (0, 0, 0));
    }

    /// remaining = pending + running; errors and done/skipped never count
    /// as queued work — otherwise a library with failed passes would show
    /// a permanent "digesting" pill.
    #[test]
    fn errors_and_finished_rows_are_not_remaining_work() {
        let s = status_from_counters(&counters(&[
            ("hash", 1, c(0, 0, 90, 7, 3)),
            ("preview", 1, c(11, 1, 5, 2, 0)),
        ]));
        assert_eq!(
            s.passes,
            vec![PassRemaining {
                name: "preview".into(),
                remaining: 12,
                done: 5,   // preview: done 5 + skipped 0
                total: 19, // 11 + 1 + 5 + 2 + 0
                rate_per_sec: 0.0,
            }],
            "hash finished (errors included): only preview is still queued"
        );
        assert_eq!(s.done, 98); // done + skipped, both kinds
        assert_eq!(s.errors, 9);
        assert_eq!(s.total, 119);
        assert!(s.running, "queued preview work keeps running true");
    }

    /// A fully-errored library is NOT running: running compares total
    /// against done + errors, so failed rows cannot wedge the pill on.
    #[test]
    fn all_errored_is_not_running() {
        let s = status_from_counters(&counters(&[("exif", 2, c(0, 0, 0, 4, 0))]));
        assert!(!s.running);
        assert!(s.passes.is_empty());
    }

    /// Versions of the same pass sum under ONE kind: a version bump
    /// re-running a pass must not double-list it in the hover breakdown.
    #[test]
    fn versions_of_a_kind_sum_into_one_entry() {
        let s = status_from_counters(&counters(&[
            ("preview", 1, c(3, 0, 10, 0, 0)),
            ("preview", 2, c(5, 1, 0, 0, 0)),
        ]));
        assert_eq!(
            s.passes,
            vec![PassRemaining {
                name: "preview".into(),
                remaining: 9, // (3+0) + (5+1)
                done: 10,     // 10 + 0 across versions (done + skipped)
                total: 19,    // 13 + 6 across versions
                rate_per_sec: 0.0,
            }]
        );
    }

    /// done/total are summed across versions just like remaining, and `done`
    /// folds skipped in (matching the aggregate `done`). A pure fold leaves
    /// `rate_per_sec` at 0.0 (no clock here) — the pump fills it.
    #[test]
    fn done_and_total_carry_per_pass_with_skipped_folded() {
        let s = status_from_counters(&counters(&[("preview", 1, c(2, 1, 4, 0, 3))]));
        let p = &s.passes[0];
        assert_eq!(p.remaining, 3); // pending 2 + running 1
        assert_eq!(p.done, 7); // done 4 + skipped 3
        assert_eq!(p.total, 10); // 2 + 1 + 4 + 0 + 3
        assert_eq!(p.rate_per_sec, 0.0);
    }

    /// A walk with ZERO pass rows is still pending work: `running` must
    /// flip true the moment the scan registers, or the empty grid lies
    /// "No photographs" for the whole walk (the founder incident this
    /// overlay exists for).
    #[test]
    fn a_live_walk_keeps_running_true_with_empty_counters() {
        let s = super::overlay_walk(status_from_counters(&BTreeMap::new()), true, 137);
        assert!(s.running, "scanning alone is pending work");
        assert!(s.scanning);
        assert_eq!(s.discovered, 137);

        let idle = super::overlay_walk(status_from_counters(&BTreeMap::new()), false, 0);
        assert!(!idle.running, "no walk, no queue: truly idle");
        assert!(!idle.scanning);
    }

    /// The breakdown order is deterministic (name-sorted): the pump's `!=`
    /// change detection must never see a phantom reorder between emits.
    #[test]
    fn breakdown_order_is_name_sorted() {
        let s = status_from_counters(&counters(&[
            ("preview", 1, c(1, 0, 0, 0, 0)),
            ("exif", 1, c(2, 0, 0, 0, 0)),
            ("hash", 1, c(3, 0, 0, 0, 0)),
        ]));
        let names: Vec<&str> = s.passes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["exif", "hash", "preview"]);
    }
}

#[cfg(test)]
mod rate_ema_tests {
    use super::{PROGRESS_INTERVAL, RATE_ALPHA, fold_rate_ema, progress_emit_due};
    use std::time::Duration;

    /// Two samples with a known time + done delta fold to the expected
    /// smoothed rate: alpha * instant + (1 - alpha) * prev_ema. From a 0.0
    /// seed, the first fold of 10 items over 2 s (instant = 5/s) is
    /// alpha * 5 = 1.5/s.
    #[test]
    fn two_samples_fold_to_expected_smoothed_rate() {
        let after_first = fold_rate_ema(0.0, 10, 2.0, false);
        assert!(
            (after_first - RATE_ALPHA * 5.0).abs() < 1e-6,
            "first fold = alpha * instant from a 0 seed; got {after_first}"
        );
        // A second sample at the same 5/s instant pulls the EMA further toward
        // 5: 0.3*5 + 0.7*1.5 = 2.55/s.
        let after_second = fold_rate_ema(after_first, 5, 1.0, false);
        let expected = RATE_ALPHA * 5.0 + (1.0 - RATE_ALPHA) * after_first;
        assert!(
            (after_second - expected).abs() < 1e-6,
            "second fold compounds toward the instant rate; got {after_second}"
        );
    }

    /// A PAUSED sample does not move the EMA, no matter the apparent delta —
    /// the rate freezes so the ETA the user reads survives the pause.
    #[test]
    fn a_paused_sample_freezes_the_ema() {
        let warm = fold_rate_ema(0.0, 10, 2.0, false); // 1.5/s
        let frozen = fold_rate_ema(warm, 999, 0.001, true);
        assert_eq!(frozen, warm, "paused fold must not change the EMA");
    }

    /// A post-pause sample with a ZERO work-delta (nothing drained while
    /// frozen) cannot spike: no work happened, so the instantaneous rate is
    /// undefined and the EMA holds. This is the resume guard — a long pause
    /// over which `done` did not move never registers as a giant burst.
    #[test]
    fn a_post_pause_zero_delta_sample_does_not_spike() {
        let warm = fold_rate_ema(0.0, 10, 2.0, false); // 1.5/s
        // Huge elapsed (the pause), but done_delta == 0 because work was
        // frozen: the fold returns prev unchanged, no spike.
        let resumed = fold_rate_ema(warm, 0, 600.0, false);
        assert_eq!(
            resumed, warm,
            "zero work over the pause cannot spike the rate"
        );
        // And a non-positive interval is likewise ignored (defensive).
        assert_eq!(fold_rate_ema(warm, 5, 0.0, false), warm);
    }

    #[test]
    fn progress_cadence_is_immediate_then_coalesced_at_400ms() {
        assert!(progress_emit_due(false, false, Duration::ZERO));
        assert!(!progress_emit_due(
            true,
            true,
            PROGRESS_INTERVAL - Duration::from_millis(1)
        ));
        assert!(progress_emit_due(true, true, PROGRESS_INTERVAL));
        assert!(!progress_emit_due(
            true,
            false,
            PROGRESS_INTERVAL + Duration::from_secs(10)
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::task::{Context, Poll};

    use futures_core::stream::{BoxStream, Stream};
    use photoproof_connectors::ConnectorResult;
    use photoproof_connectors::mock::{MockVad, SpeechSpan};
    use photoproof_connectors::transcriber::{
        AudioFrame, SegmentKind, Transcriber, TranscriptSegment,
    };
    use photoproof_core::capture::{CaptureEngine, FakeClock};
    use photoproof_core::{EventStore, SessionContext};

    use super::drain_capture_at_quit;

    const SR: u32 = 16_000;

    /// Pending until the pump-side wait has run `release_after` times,
    /// then one trailing Final, then end — the late-real-wire shape.
    struct SlowFinal {
        waits: Arc<AtomicU32>,
        release_after: u32,
    }

    struct SlowFinalStream {
        waits: Arc<AtomicU32>,
        release_after: u32,
        emitted: bool,
    }

    impl Stream for SlowFinalStream {
        type Item = ConnectorResult<TranscriptSegment>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if this.waits.load(AtomicOrdering::SeqCst) < this.release_after {
                return Poll::Pending;
            }
            if this.emitted {
                return Poll::Ready(None);
            }
            this.emitted = true;
            Poll::Ready(Some(Ok(TranscriptSegment {
                utterance_id: 1,
                kind: SegmentKind::Final,
                text: "spoken right before quit".into(),
                onset: 100,
                end: 800,
                confidence: None,
                language: None,
            })))
        }
    }

    impl Transcriber for SlowFinal {
        fn stream<'a>(
            &'a self,
            _audio: BoxStream<'a, AudioFrame>,
        ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
            Ok(Box::pin(SlowFinalStream {
                waits: self.waits.clone(),
                release_after: self.release_after,
                emitted: false,
            }))
        }
        fn sample_rate(&self) -> u32 {
            SR
        }
        fn model_id(&self) -> &str {
            "slow-final"
        }
    }

    fn rig(
        release_after: u32,
    ) -> (
        tempfile::TempDir,
        EventStore,
        FakeClock,
        Arc<AtomicU32>,
        SlowFinal,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("photoproof.db")).unwrap();
        let clock = FakeClock::new(1_780_000_000_000);
        let waits = Arc::new(AtomicU32::new(0));
        let transcriber = SlowFinal {
            waits: waits.clone(),
            release_after,
        };
        (dir, store, clock, waits, transcriber)
    }

    fn armed_engine<'t>(
        store: &EventStore,
        clock: &FakeClock,
        transcriber: &'t SlowFinal,
    ) -> CaptureEngine<'t, FakeClock> {
        let session = store
            .open_session(SessionContext {
                app_version: "0.0.1-test".into(),
                device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
                root_context: None,
            })
            .unwrap();
        let vad = MockVad::new(
            SR,
            vec![SpeechSpan {
                onset: 100,
                end: 900,
            }],
        );
        let mut engine = CaptureEngine::new(clock.clone(), transcriber, Box::new(vad), session);
        engine.arm();
        for i in 0..20u64 {
            engine.push_audio(
                store,
                AudioFrame {
                    samples: vec![0.01; (u64::from(SR) * 50 / 1000) as usize],
                    captured_at: i * 50,
                },
            );
            clock.advance(50);
        }
        assert_eq!(engine.streaming_count(), 1, "one utterance in flight");
        engine
    }

    /// B52: the pump-side wait blocks quit until the trailing final lands
    /// — bounded by the ENGINE's 5 s deadline on its own clock, which the
    /// wait seam advances (no wall-clock dependence in the test).
    #[test]
    fn quit_drain_waits_boundedly_and_mints_the_trailing_final() {
        let (_dir, store, clock, waits, transcriber) = rig(3);
        let mut engine = armed_engine(&store, &clock, &transcriber);
        let minted = drain_capture_at_quit(&mut engine, &store, &mut || {
            waits.fetch_add(1, AtomicOrdering::SeqCst);
            clock.advance(500); // a real sleep in production
        });
        assert_eq!(minted, 1, "the trailing final minted during the wait");
        assert!(!engine.stream_open(), "stream fully closed at quit");
        assert!(engine.audio_is_zeroed());
        assert_eq!(
            waits.load(AtomicOrdering::SeqCst),
            3,
            "three waits, then done"
        );
    }

    /// A stream that never yields cannot hold quit hostage: the engine's
    /// 5 s deadline abandons, and the loop exits.
    #[test]
    fn quit_drain_is_capped_by_the_engine_deadline() {
        let (_dir, store, clock, waits, transcriber) = rig(u32::MAX);
        let mut engine = armed_engine(&store, &clock, &transcriber);
        let minted = drain_capture_at_quit(&mut engine, &store, &mut || {
            waits.fetch_add(1, AtomicOrdering::SeqCst);
            clock.advance(500);
        });
        assert_eq!(minted, 0);
        assert!(!engine.stream_open(), "the 5 s cap closed the stream");
        assert_eq!(engine.abandoned_count(), 1, "in-flight utterance abandoned");
        assert!(
            waits.load(AtomicOrdering::SeqCst) <= 11,
            "bounded: ~5 s of 500 ms waits"
        );
    }
}
