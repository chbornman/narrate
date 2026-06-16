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
use std::time::{Duration, Instant};

use photoproof_core::UtcMillis;
use photoproof_core::library::QueueOptions;
use tauri::{AppHandle, Emitter, Manager};

use crate::dto::{IngestStatus, PassRemaining};
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
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);
const SIDECAR_TICK: Duration = Duration::from_millis(500);
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
    let queue = match app.library.pass_counters() {
        Ok(c) => status_from_counters(&c),
        Err(_) => IngestStatus::default(),
    };
    let (scanning, discovered) = app.scans.snapshot();
    let mut status = overlay_walk(queue, scanning, discovered);
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

/// Drives `process_queue` continuously, plus the periodic volume probe and
/// maintenance tick, emitting coalesced `ingest-progress` events.
pub fn spawn_ingest_pump(handle: AppHandle) {
    std::thread::Builder::new()
        .name("pp-ingest-pump".into())
        .spawn(move || {
            let mut last_emit: Option<(Instant, IngestStatus)> = None;
            // Per-pass throughput trackers, persisted across loop iterations
            // (the loop owns the clock + prev state the rate needs). Pruned
            // inside `apply_pass_rates` when a pass finishes.
            let mut rate_trackers: HashMap<String, RateEma> = HashMap::new();
            let mut last_probe = Instant::now();
            let mut last_maintenance = Instant::now();
            loop {
                let Some(app) = handle.try_state::<Arc<App>>() else {
                    std::thread::sleep(IDLE_SLEEP);
                    continue;
                };
                let app = app.inner().clone();
                if app.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if last_probe.elapsed() >= PROBE_INTERVAL {
                    last_probe = Instant::now();
                    let _ = app.library.probe_volumes();
                }
                if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
                    last_maintenance = Instant::now();
                    let _ = app.library.maintenance_tick();
                }
                let report = app
                    .library
                    .process_queue(&QueueOptions {
                        cancel: None,
                        max_items: Some(QUEUE_BATCH),
                    })
                    .unwrap_or_default();
                let processed = report.processed;
                // The on-demand full-raw-decode drain (OD-1). It runs when the
                // regular ingest queue went idle this turn (same L4 politeness
                // as embeddings), but BEFORE embeddings: a view-time develop is
                // interactive (a user waiting on a "developing..." spinner),
                // the highest-priority work in the queue, so it outranks the
                // lowest-priority embedding backfill. It is a no-op (zero items)
                // unless a develop was requested AND the mic is not armed; its
                // completed develops emit `previews-changed` so Look swaps in
                // the developed artifact the moment it lands.
                let decoded = if processed == 0 {
                    drain_raw_decode(&app, &handle)
                } else {
                    0
                };
                // P7.4 decision 5: the embedding backfill drain. Politeness
                // rule (L4 ordering) — embeddings are the LOWEST priority, so
                // they run ONLY when the regular ingest queue went idle this
                // turn (`processed == 0`); a freshly-arriving photo or note
                // preempts them on the next loop. The drain itself is a no-op
                // (zero items) unless an embedder is ready AND the mic is not
                // armed. Its passes land in `pass_counters`, so the coalesced
                // jobs indicator below picks them up for free (the status is
                // computed after this).
                let embedded = if processed == 0 {
                    drain_embeddings(&app)
                } else {
                    0
                };
                // Hash-aware preview completions (the journal-changed
                // pattern): thumbs whose retry budget ran out heal off
                // this. One event per drain batch — same low-rate wire
                // discipline as ingest-progress (≤ QUEUE_BATCH hashes).
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
                }

                let mut status = ingest_status(&app);
                // PAUSE signal for the rate EMAs: the mic owns the machine
                // (`capture_live`) OR an offline volume is holding back work
                // (no pass can drain bytes off a disconnected drive). Either
                // way `done` cannot advance for the right reasons, so we freeze
                // the rate rather than let it decay toward 0 — the ETA the user
                // reads must survive the pause.
                let paused = app.runtime.capture_live.load(Ordering::Relaxed)
                    || !status.offline_volumes.is_empty();
                apply_pass_rates(&mut rate_trackers, &mut status, Instant::now(), paused);
                let due = match &last_emit {
                    None => true,
                    // `prev != status` already ignores `rate_per_sec` (the DTO's
                    // hand-written eq excludes the drifting float). To still let
                    // a MEANINGFUL rate move surface, also emit when the
                    // quantized rate of any pass changed — quantizing kills the
                    // 2.5/s vs 2.50001/s wobble that would otherwise pin the
                    // channel at one emit per PROGRESS_INTERVAL forever.
                    Some((at, prev)) => {
                        (*prev != status || rate_quantum_changed(prev, &status))
                            && at.elapsed() >= PROGRESS_INTERVAL
                    }
                };
                if due {
                    // `passes` made the status non-Copy: clone for the
                    // wire, keep the original as the change-detection prev.
                    let _ = handle.emit("ingest-progress", status.clone());
                    last_emit = Some((Instant::now(), status));
                }
                // Sleep only when ALL drains are idle — otherwise a develop or
                // embedding batch would pace at one batch per IDLE_SLEEP and
                // never catch up. Any work this turn loops straight back.
                if processed == 0 && decoded == 0 && embedded == 0 {
                    std::thread::sleep(IDLE_SLEEP);
                }
            }
        })
        .expect("spawn ingest pump");
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
fn drain_embeddings(app: &App) -> usize {
    let armed = app.runtime.capture_live.load(Ordering::Relaxed);
    let text = app.runtime.embedders.text();
    let clip = app.runtime.embedders.clip();
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
        let rig = photoproof_core::library::EmbeddingRig::<photoproof_connectors::OrtEmbedder> {
            text: None,
            clip: Some(clip.as_ref()),
            vectors: &app.vectors,
        };
        return run_embedding_drain(
            app, &rig,
            // No `capture_live` cancel here: it is true for the whole armed
            // window, so it would cancel the GPU work we WANT to keep draining.
            // The bounded batch keeps the armed turn short on its own.
            None,
        );
    }

    // Not armed: full rig, both passes, with the capture cancel wired so that
    // arming the mic MID-drain preempts the CPU work at the next item.
    let rig = photoproof_core::library::EmbeddingRig {
        text: text.as_deref(),
        clip: clip.as_deref(),
        vectors: &app.vectors,
    };
    run_embedding_drain(app, &rig, Some(app.runtime.capture_live.clone()))
}

/// Run one bounded embedding-queue drain with the given cancel flag, logging and
/// swallowing drain-level errors (never crashes the pump). Shared by the armed
/// (GPU-only, no cancel) and not-armed (full rig, capture cancel) paths.
fn run_embedding_drain<TE: photoproof_connectors::Embedder, CE: photoproof_connectors::Embedder>(
    app: &App,
    rig: &photoproof_core::library::EmbeddingRig<'_, TE, CE>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> usize {
    match app.library.process_embedding_queue(
        rig,
        &QueueOptions {
            cancel,
            max_items: Some(EMBED_BATCH),
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
fn drain_raw_decode(app: &App, handle: &AppHandle) -> usize {
    // The mic owns the machine: no develop batch starts while armed.
    if app.runtime.capture_live.load(Ordering::Relaxed) {
        return 0;
    }
    match app.library.process_raw_decode_queue(&QueueOptions {
        cancel: Some(app.runtime.capture_live.clone()),
        max_items: Some(DECODE_BATCH),
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
pub fn spawn_sidecar_pump(handle: AppHandle) {
    std::thread::Builder::new()
        .name("pp-sidecar-pump".into())
        .spawn(move || {
            loop {
                std::thread::sleep(SIDECAR_TICK);
                let Some(app) = handle.try_state::<Arc<App>>() else {
                    continue;
                };
                let app = app.inner().clone();
                if app.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if let Err(e) = app.engine.pump(UtcMillis::now()) {
                    tracing::error!(error = %e, "sidecar pump error");
                }
                // Collections ride the same tick (RETRIEVAL §10.2: "the
                // same debounced writer that maintains sidecars"); a write
                // failure backs off inside the core writer and retries here.
                if let Err(e) = app.collections.pump(UtcMillis::now()) {
                    tracing::error!(error = %e, "collections pump error");
                }
                if let Err(e) = app.run_close_processing() {
                    tracing::error!(error = %e, "close processing error");
                }
            }
        })
        .expect("spawn sidecar pump");
}

/// Readiness fingerprint for the runtime pump's idle-tick re-emit gate: the
/// fields that can transition WITHOUT a bus event. The embedder host lands a
/// slot Ready/Failed on a background build thread and publishes nothing, so the
/// timeout path must notice the change itself. Download byte-progress is NOT
/// here — it rides real bus events and never needs the timeout refresh.
type ReadyFp = (
    crate::dto::EmbedderState, // clip slot state (covers clip_ready == Ready)
    crate::dto::EmbedderState, // text embedder slot state
    bool,                      // asr_ready
    bool,                      // llm_ready
    bool,                      // asr_blocked present
    bool,                      // llm_blocked present
);

fn readiness_fp(s: &crate::dto::RuntimeStatus) -> ReadyFp {
    (
        s.clip.state,
        s.text_embedder.state,
        s.asr_ready,
        s.llm_ready,
        s.asr_blocked.is_some(),
        s.llm_blocked.is_some(),
    )
}

/// The runtime pump (RUNTIME §8.3): forwards core-bus events to the
/// webview as coalesced `runtime-status` snapshots — readiness changes,
/// state transitions, download progress. Payloads stay snapshot-shaped
/// and low-rate (UI §7.4 wire discipline / tauri #852). An idle-tick refresh
/// (gated on `readiness_fp`) catches the embedder's no-bus-event slot landing.
pub fn spawn_runtime_pump(handle: AppHandle) {
    std::thread::Builder::new()
        .name("pp-runtime-pump".into())
        .spawn(move || {
            let rx = loop {
                if let Some(app) = handle.try_state::<Arc<App>>() {
                    break app.runtime.bus.subscribe();
                }
                std::thread::sleep(IDLE_SLEEP);
            };
            // Last-emitted READINESS fingerprint. The embedder host lands a slot
            // Ready/Failed on a background build thread and publishes NO bus
            // event, so without a timeout-side refresh the webview freezes on the
            // last 'building' snapshot forever (the embedder-loading-that-never-
            // finishes bug). We re-emit on the idle tick ONLY when this changed,
            // so an idle runtime still stays quiet (no 2/s snapshot spam).
            let mut last_ready: Option<ReadyFp> = None;
            loop {
                let Some(app) = handle.try_state::<Arc<App>>() else {
                    return;
                };
                let app = app.inner().clone();
                if app.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                // Block for one event, then drain the burst (coalesce).
                match rx.recv_timeout(RUNTIME_PUMP_TICK) {
                    Ok(first) => {
                        let mut events = vec![first];
                        while let Ok(e) = rx.try_recv() {
                            events.push(e);
                        }
                        for e in &events {
                            if let photoproof_core::runtime::RuntimeEvent::DownloadProgress {
                                model_id,
                                downloaded_bytes,
                                total_bytes,
                            } = e
                            {
                                app.runtime
                                    .note_progress(model_id, *downloaded_bytes, *total_bytes);
                            }
                        }
                        let status = app.runtime.status();
                        last_ready = Some(readiness_fp(&status));
                        let _ = handle.emit("runtime-status", status);
                    }
                    // Timeout (or a disconnect, handled by the top-of-loop
                    // shutdown/try_state checks): no bus event fired, but a
                    // silent slot transition may have. Re-emit only on a
                    // readiness change so the UI self-corrects within one tick.
                    Err(_) => {
                        let status = app.runtime.status();
                        let fp = readiness_fp(&status);
                        if last_ready.as_ref() != Some(&fp) {
                            last_ready = Some(fp);
                            let _ = handle.emit("runtime-status", status);
                        }
                    }
                }
            }
        })
        .expect("spawn runtime pump");
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
    use super::{RATE_ALPHA, fold_rate_ema};

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
