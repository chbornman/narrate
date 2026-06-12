//! Background pumps: the ingest scheduler shell (LIBRARY B20 — the core
//! ships synchronous `process_queue` + `maintenance_tick`/`probe_volumes`
//! hooks; the shell drives them) and the sidecar debounce pump (SIDECARS S3).
//!
//! Event emission discipline (UI §7.4 / tauri #852): the ingest channel is
//! low-rate by construction — progress is emitted at most every
//! `PROGRESS_INTERVAL` and only when counters changed; payloads are four
//! integers.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use photoproof_core::UtcMillis;
use photoproof_core::library::QueueOptions;
use tauri::{AppHandle, Emitter, Manager};

use crate::dto::{IngestStatus, PassRemaining};
use crate::state::App;

const QUEUE_BATCH: usize = 64;
const IDLE_SLEEP: Duration = Duration::from_millis(500);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);
const SIDECAR_TICK: Duration = Duration::from_millis(500);
/// Block-for-one-event timeout in the runtime pump's recv loop. The
/// timeout exists to re-check `app.shutdown`, not to pace events —
/// events wake the loop immediately; this only bounds quit latency.
const RUNTIME_PUMP_TICK: Duration = Duration::from_millis(500);

pub fn ingest_status(app: &App) -> IngestStatus {
    match app.library.pass_counters() {
        Ok(c) => status_from_counters(&c),
        Err(_) => IngestStatus::default(),
    }
}

/// Pure fold from the queue's pass counters to the wire status — split
/// from `ingest_status` so the aggregation is unit-testable without an
/// `App` (the same pure-controller shape as logic/jobs.ts on the TS side).
fn status_from_counters(
    counters: &std::collections::BTreeMap<(String, i64), photoproof_core::library::PassCounters>,
) -> IngestStatus {
    let mut s = IngestStatus::default();
    // Per-KIND remaining work, versions summed (the header pill names
    // kinds; a version bump re-running a pass is the same kind of work).
    // BTreeMap keeps the breakdown order deterministic for `!=` below.
    let mut remaining = std::collections::BTreeMap::<&str, u64>::new();
    for ((name, _version), c) in counters {
        s.done += c.done + c.skipped;
        s.total += c.pending + c.running + c.done + c.error + c.skipped;
        s.errors += c.error;
        // Errored rows are NOT remaining work: counting them would keep
        // the "digesting" pill lit forever on a library with failed passes.
        let queued = c.pending + c.running;
        if queued > 0 {
            *remaining.entry(name.as_str()).or_default() += queued;
        }
    }
    s.passes = remaining
        .into_iter()
        .map(|(name, remaining)| PassRemaining {
            name: name.to_owned(),
            remaining,
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

                let status = ingest_status(&app);
                let due = match &last_emit {
                    None => true,
                    Some((at, prev)) => *prev != status && at.elapsed() >= PROGRESS_INTERVAL,
                };
                if due {
                    // `passes` made the status non-Copy: clone for the
                    // wire, keep the original as the change-detection prev.
                    let _ = handle.emit("ingest-progress", status.clone());
                    last_emit = Some((Instant::now(), status));
                }
                if processed == 0 {
                    std::thread::sleep(IDLE_SLEEP);
                }
            }
        })
        .expect("spawn ingest pump");
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
                    eprintln!("photoproof: sidecar pump error: {e}");
                }
                // Collections ride the same tick (RETRIEVAL §10.2: "the
                // same debounced writer that maintains sidecars"); a write
                // failure backs off inside the core writer and retries here.
                if let Err(e) = app.collections.pump(UtcMillis::now()) {
                    eprintln!("photoproof: collections pump error: {e}");
                }
                if let Err(e) = app.run_close_processing() {
                    eprintln!("photoproof: close processing error: {e}");
                }
            }
        })
        .expect("spawn sidecar pump");
}

/// The runtime pump (RUNTIME §8.3): forwards core-bus events to the
/// webview as coalesced `runtime-status` snapshots — readiness changes,
/// state transitions, download progress. Payloads stay snapshot-shaped
/// and low-rate (UI §7.4 wire discipline / tauri #852).
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
            loop {
                let Some(app) = handle.try_state::<Arc<App>>() else {
                    return;
                };
                let app = app.inner().clone();
                if app.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                // Block for one event, then drain the burst (coalesce).
                let Ok(first) = rx.recv_timeout(RUNTIME_PUMP_TICK) else {
                    continue;
                };
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
                let _ = handle.emit("runtime-status", app.runtime.status());
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
                remaining: 9,
            }]
        );
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
