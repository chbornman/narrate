//! Process-wide admission policy for expensive desktop work.
//!
//! The managed-task registry owns lifetime and cancellation. This governor
//! owns concurrency and priority across those tasks. A persisted Pause stops
//! background work while keeping an explicit 1:1 develop request responsive.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::settings::ProcessingIntensity;

const CANCEL_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceLane {
    InteractiveRaw,
    LiveIngest,
    Preview,
    ModelDownload,
    Embedding,
    RootScan,
    StartupIo,
    Repair,
    Maintenance,
}

impl ResourceLane {
    fn rank(self) -> u8 {
        match self {
            Self::InteractiveRaw => 0,
            Self::LiveIngest => 1,
            Self::Preview => 2,
            Self::ModelDownload => 3,
            Self::Embedding => 4,
            Self::RootScan => 5,
            Self::StartupIo => 6,
            Self::Repair => 7,
            Self::Maintenance => 8,
        }
    }

    fn allowed_while_paused(self) -> bool {
        matches!(self, Self::InteractiveRaw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceBudget {
    pub total_concurrency: usize,
    pub ingest_concurrency: usize,
    pub ingest_batch: usize,
    pub embedding_batch: usize,
    pub raw_batch: usize,
}

impl ProcessingIntensity {
    pub fn resource_budget(self) -> ResourceBudget {
        match self {
            Self::Eco => ResourceBudget {
                total_concurrency: 1,
                ingest_concurrency: 1,
                ingest_batch: 8,
                embedding_batch: 1,
                raw_batch: 1,
            },
            Self::Balanced => ResourceBudget {
                total_concurrency: 2,
                ingest_concurrency: 2,
                ingest_batch: 32,
                embedding_batch: 4,
                raw_batch: 1,
            },
            Self::Max => ResourceBudget {
                total_concurrency: 4,
                ingest_concurrency: 8,
                ingest_batch: 64,
                embedding_batch: 8,
                raw_batch: 2,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLaneStatus {
    pub lane: ResourceLane,
    pub active: usize,
    pub waiting: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStatus {
    pub intensity: ProcessingIntensity,
    pub paused: bool,
    pub budget: ResourceBudget,
    pub active_total: usize,
    pub lanes: Vec<ResourceLaneStatus>,
}

struct GovernorState {
    intensity: ProcessingIntensity,
    paused: bool,
    active: BTreeMap<ResourceLane, usize>,
    waiting: BTreeMap<ResourceLane, usize>,
}

pub struct ResourceGovernor {
    state: Mutex<GovernorState>,
    changed: Condvar,
    pause: photoproof_core::library::PauseToken,
}

impl ResourceGovernor {
    pub fn new(intensity: ProcessingIntensity, paused: bool) -> Self {
        Self {
            state: Mutex::new(GovernorState {
                intensity,
                paused,
                active: BTreeMap::new(),
                waiting: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            pause: photoproof_core::library::PauseToken::new(paused),
        }
    }

    pub fn configure(&self, intensity: ProcessingIntensity, paused: bool) {
        let mut state = self.state.lock().expect("resource governor mutex");
        state.intensity = intensity;
        state.paused = paused;
        self.pause.set_paused(paused);
        self.changed.notify_all();
    }

    pub fn budget(&self) -> ResourceBudget {
        self.state
            .lock()
            .expect("resource governor mutex")
            .intensity
            .resource_budget()
    }

    pub fn paused(&self) -> bool {
        self.state.lock().expect("resource governor mutex").paused
    }

    pub fn pause_token(&self) -> photoproof_core::library::PauseToken {
        self.pause.clone()
    }

    /// Cooperative boundary for a long operation that already owns a permit.
    /// The permit remains attributed to the lane while sleeping, but Pause
    /// stops bytes/reads and cancellation still acknowledges within 100 ms.
    pub fn wait_until_resumed(&self, cancel: &AtomicBool) -> bool {
        self.pause.wait_until_resumed(Some(cancel))
    }

    /// Dynamic watcher policy: acquire the root-scan lane for each event burst
    /// or recovery walk, then snapshot the current intensity's hash ceiling.
    /// The permit is returned beside the options so core holds it for the
    /// whole unit.
    pub fn watcher_scan(
        self: &Arc<Self>,
        cancel: &Arc<AtomicBool>,
    ) -> Option<(photoproof_core::library::ScanOptions, ResourcePermit)> {
        let permit = self.acquire(ResourceLane::RootScan, cancel)?;
        Some((
            photoproof_core::library::ScanOptions {
                cancel: Some(Arc::clone(cancel)),
                max_concurrency: Some(self.budget().ingest_concurrency),
                pause: Some(self.pause_token()),
                ..photoproof_core::library::ScanOptions::default()
            },
            permit,
        ))
    }

    /// Wait for a lane admission. Cancellation is checked at a short bounded
    /// cadence, so Pause never compromises quit latency.
    pub fn acquire(
        self: &Arc<Self>,
        lane: ResourceLane,
        cancel: &Arc<AtomicBool>,
    ) -> Option<ResourcePermit> {
        let mut state = self.state.lock().expect("resource governor mutex");
        *state.waiting.entry(lane).or_default() += 1;
        loop {
            if cancel.load(Ordering::Acquire) {
                decrement(&mut state.waiting, lane);
                self.changed.notify_all();
                return None;
            }
            if can_admit(&state, lane) {
                decrement(&mut state.waiting, lane);
                *state.active.entry(lane).or_default() += 1;
                return Some(ResourcePermit {
                    governor: Arc::clone(self),
                    lane,
                });
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, CANCEL_POLL)
                .expect("resource governor condvar");
            state = next;
        }
    }

    pub fn snapshot(&self) -> ResourceStatus {
        let state = self.state.lock().expect("resource governor mutex");
        let lanes = [
            ResourceLane::InteractiveRaw,
            ResourceLane::LiveIngest,
            ResourceLane::Preview,
            ResourceLane::ModelDownload,
            ResourceLane::Embedding,
            ResourceLane::RootScan,
            ResourceLane::StartupIo,
            ResourceLane::Repair,
            ResourceLane::Maintenance,
        ]
        .into_iter()
        .map(|lane| ResourceLaneStatus {
            lane,
            active: state.active.get(&lane).copied().unwrap_or(0),
            waiting: state.waiting.get(&lane).copied().unwrap_or(0),
        })
        .collect();
        ResourceStatus {
            intensity: state.intensity,
            paused: state.paused,
            budget: state.intensity.resource_budget(),
            active_total: state.active.values().sum(),
            lanes,
        }
    }
}

fn can_admit(state: &GovernorState, lane: ResourceLane) -> bool {
    if state.paused && !lane.allowed_while_paused() {
        return false;
    }
    // One reserved foreground seat: a long resumable download or root walk
    // must never make an explicit 1:1 request wait for that whole operation.
    // There is still only one RAW-develop lane, so this can exceed the
    // background total by at most one bounded batch.
    if lane == ResourceLane::InteractiveRaw {
        return state.active.get(&lane).copied().unwrap_or(0) == 0;
    }
    // Higher-priority waiters get the next free slot. In-flight items are not
    // forcibly interrupted; bounded batches form the preemption boundary.
    if state
        .waiting
        .iter()
        .any(|(waiting_lane, count)| *count > 0 && waiting_lane.rank() < lane.rank())
    {
        return false;
    }
    let active_total: usize = state.active.values().sum();
    active_total < state.intensity.resource_budget().total_concurrency
}

fn decrement(map: &mut BTreeMap<ResourceLane, usize>, lane: ResourceLane) {
    if let Some(count) = map.get_mut(&lane) {
        *count -= 1;
        if *count == 0 {
            map.remove(&lane);
        }
    }
}

pub struct ResourcePermit {
    governor: Arc<ResourceGovernor>,
    lane: ResourceLane,
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        let mut state = self.governor.state.lock().expect("resource governor mutex");
        decrement(&mut state.active, self.lane);
        self.governor.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn pause_blocks_background_but_not_interactive_work() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, true));
        let cancelled = Arc::new(AtomicBool::new(false));
        let interactive = governor
            .acquire(ResourceLane::InteractiveRaw, &cancelled)
            .expect("interactive admission");
        let worker_governor = Arc::clone(&governor);
        let worker_cancel = Arc::clone(&cancelled);
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(
                worker_governor
                    .acquire(ResourceLane::LiveIngest, &worker_cancel)
                    .is_some(),
            )
            .unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(interactive);
        governor.configure(ProcessingIntensity::Eco, false);
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn intensity_changes_concurrency_and_batch_bounds() {
        let eco = ProcessingIntensity::Eco.resource_budget();
        let balanced = ProcessingIntensity::Balanced.resource_budget();
        let max = ProcessingIntensity::Max.resource_budget();
        assert_eq!((eco.total_concurrency, eco.ingest_concurrency), (1, 1));
        assert_eq!(
            (eco.ingest_batch, eco.embedding_batch, eco.raw_batch),
            (8, 1, 1)
        );
        assert_eq!(
            (balanced.total_concurrency, balanced.ingest_concurrency),
            (2, 2)
        );
        assert_eq!(
            (
                balanced.ingest_batch,
                balanced.embedding_batch,
                balanced.raw_batch
            ),
            (32, 4, 1)
        );
        assert_eq!((max.total_concurrency, max.ingest_concurrency), (4, 8));
        assert_eq!(
            (max.ingest_batch, max.embedding_batch, max.raw_batch),
            (64, 8, 2)
        );
        // The bounded queue holds at most one queued + one executing decoded
        // frame per worker, so this is the deterministic peak-memory proxy.
        assert_eq!(2 * eco.ingest_concurrency, 2);
        assert_eq!(2 * balanced.ingest_concurrency, 4);
        assert_eq!(2 * max.ingest_concurrency, 16);
    }

    #[test]
    fn interactive_raw_has_a_reserved_seat_over_background_capacity() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let background = governor
            .acquire(ResourceLane::ModelDownload, &cancelled)
            .unwrap();
        let interactive = governor
            .acquire(ResourceLane::InteractiveRaw, &cancelled)
            .expect("foreground seat must not wait for a long download");
        assert_eq!(governor.snapshot().active_total, 2);
        drop(interactive);
        drop(background);
    }

    #[test]
    fn higher_priority_waiter_wins_next_slot() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let held = governor
            .acquire(ResourceLane::Maintenance, &cancelled)
            .unwrap();
        let (tx, rx) = mpsc::channel();

        let low_governor = Arc::clone(&governor);
        let low_cancel = Arc::clone(&cancelled);
        let low_tx = tx.clone();
        let low = thread::spawn(move || {
            let _permit = low_governor
                .acquire(ResourceLane::Repair, &low_cancel)
                .unwrap();
            low_tx.send(ResourceLane::Repair).unwrap();
        });
        wait_until_waiting(&governor, ResourceLane::Repair);

        let high_governor = Arc::clone(&governor);
        let high_cancel = Arc::clone(&cancelled);
        let high = thread::spawn(move || {
            let _permit = high_governor
                .acquire(ResourceLane::LiveIngest, &high_cancel)
                .unwrap();
            tx.send(ResourceLane::LiveIngest).unwrap();
        });
        wait_until_waiting(&governor, ResourceLane::LiveIngest);
        drop(held);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ResourceLane::LiveIngest
        );
        high.join().unwrap();
        low.join().unwrap();
    }

    #[test]
    fn in_flight_work_cooperatively_suspends_until_resume() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, false));
        let cancel = Arc::new(AtomicBool::new(false));
        let _permit = governor.acquire(ResourceLane::RootScan, &cancel).unwrap();
        governor.configure(ProcessingIntensity::Eco, true);
        let worker_governor = Arc::clone(&governor);
        let worker_cancel = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(worker_governor.wait_until_resumed(&worker_cancel))
                .unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        governor.configure(ProcessingIntensity::Balanced, false);
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn watcher_options_snapshot_each_dynamic_budget() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, false));
        let cancel = Arc::new(AtomicBool::new(false));
        let (eco, permit) = governor.watcher_scan(&cancel).unwrap();
        assert_eq!(eco.max_concurrency, Some(1));
        drop(permit);
        governor.configure(ProcessingIntensity::Max, false);
        let (max, _permit) = governor.watcher_scan(&cancel).unwrap();
        assert_eq!(
            max.max_concurrency,
            Some(
                ProcessingIntensity::Max
                    .resource_budget()
                    .ingest_concurrency
            )
        );
        assert!(max.pause.is_some());
    }

    #[test]
    fn root_scan_waiter_runs_after_foreground_queue_settles() {
        let governor = Arc::new(ResourceGovernor::new(ProcessingIntensity::Eco, false));
        let cancel = Arc::new(AtomicBool::new(false));
        let held = governor
            .acquire(ResourceLane::Maintenance, &cancel)
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let root_governor = Arc::clone(&governor);
        let root_cancel = Arc::clone(&cancel);
        let root_tx = tx.clone();
        let root = thread::spawn(move || {
            let _permit = root_governor
                .acquire(ResourceLane::RootScan, &root_cancel)
                .unwrap();
            root_tx.send(ResourceLane::RootScan).unwrap();
        });
        wait_until_waiting(&governor, ResourceLane::RootScan);
        let live_governor = Arc::clone(&governor);
        let live_cancel = Arc::clone(&cancel);
        let live = thread::spawn(move || {
            let _permit = live_governor
                .acquire(ResourceLane::LiveIngest, &live_cancel)
                .unwrap();
            tx.send(ResourceLane::LiveIngest).unwrap();
        });
        wait_until_waiting(&governor, ResourceLane::LiveIngest);
        drop(held);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ResourceLane::LiveIngest
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ResourceLane::RootScan
        );
        live.join().unwrap();
        root.join().unwrap();
    }

    fn wait_until_waiting(governor: &ResourceGovernor, lane: ResourceLane) {
        while governor
            .snapshot()
            .lanes
            .iter()
            .find(|status| status.lane == lane)
            .unwrap()
            .waiting
            == 0
        {
            thread::yield_now();
        }
    }
}
