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

use crate::dto::IngestStatus;
use crate::state::App;

const QUEUE_BATCH: usize = 64;
const IDLE_SLEEP: Duration = Duration::from_millis(500);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);
const SIDECAR_TICK: Duration = Duration::from_millis(500);

pub fn ingest_status(app: &App) -> IngestStatus {
    let counters = match app.library.pass_counters() {
        Ok(c) => c,
        Err(_) => return IngestStatus::default(),
    };
    let mut s = IngestStatus::default();
    for c in counters.values() {
        s.done += c.done + c.skipped;
        s.total += c.pending + c.running + c.done + c.error + c.skipped;
        s.errors += c.error;
    }
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
                let processed = app
                    .library
                    .process_queue(&QueueOptions {
                        cancel: None,
                        max_items: Some(QUEUE_BATCH),
                    })
                    .map(|r| r.processed)
                    .unwrap_or(0);

                let status = ingest_status(&app);
                let due = match &last_emit {
                    None => true,
                    Some((at, prev)) => *prev != status && at.elapsed() >= PROGRESS_INTERVAL,
                };
                if due {
                    let _ = handle.emit("ingest-progress", status);
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
            }
        })
        .expect("spawn sidecar pump");
}
