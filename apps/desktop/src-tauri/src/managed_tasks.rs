//! Owned background work for the desktop process.
//!
//! Tasks are single-flight by `(owner, key)`, observable, cooperatively
//! cancellable, and acknowledged during a bounded shutdown. Terminal history
//! remains inspectable after its OS thread has been joined.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPriority {
    Background,
    Maintenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskProgress {
    pub fraction: f32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub owner: String,
    pub key: String,
    pub priority: TaskPriority,
    pub started_at: SystemTime,
    pub progress: Option<TaskProgress>,
    pub last_error: Option<String>,
    pub state: TaskState,
    pub ended_at: Option<SystemTime>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpawnTaskError {
    #[error("task {owner}/{key} is already running")]
    AlreadyRunning { owner: String, key: String },
    #[error("managed task registry is stopping")]
    Stopping,
    #[error("failed to spawn managed task: {0}")]
    Spawn(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub acknowledged: bool,
    pub remaining: Vec<(String, String)>,
}

struct CancellationState {
    cancelled: Arc<AtomicBool>,
    changed: Condvar,
    mutex: Mutex<()>,
}

#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: Arc::new(AtomicBool::new(false)),
            changed: Condvar::new(),
            mutex: Mutex::new(()),
        }))
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.changed.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0.cancelled)
    }

    /// Wait until cancellation or the timeout. This gives periodic loops an
    /// immediate quit wake-up without a polling sleep.
    pub fn wait_for_cancel(&self, timeout: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let guard = self.0.mutex.lock().expect("cancellation mutex");
        let _ = self
            .0
            .changed
            .wait_timeout_while(guard, timeout, |_| !self.is_cancelled())
            .expect("cancellation condvar");
        self.is_cancelled()
    }
}

pub struct TaskContext {
    owner: String,
    key: String,
    cancel: CancellationToken,
    registry: Weak<ManagedTaskRegistry>,
}

impl TaskContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.flag()
    }

    pub fn wait_for_cancel(&self, timeout: Duration) -> bool {
        self.cancel.wait_for_cancel(timeout)
    }

    pub fn report_progress(&self, fraction: f32, message: impl Into<String>) {
        if let Some(registry) = self.registry.upgrade() {
            registry.update(&self.owner, &self.key, |task| {
                task.snapshot.progress = Some(TaskProgress {
                    fraction: fraction.clamp(0.0, 1.0),
                    message: message.into(),
                });
            });
        }
    }

    pub fn report_error(&self, error: impl Into<String>) {
        if let Some(registry) = self.registry.upgrade() {
            registry.update(&self.owner, &self.key, |task| {
                task.snapshot.last_error = Some(error.into());
            });
        }
    }
}

struct TaskEntry {
    snapshot: TaskSnapshot,
    cancel: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct RegistryState {
    accepting: bool,
    tasks: BTreeMap<(String, String), TaskEntry>,
    history: Vec<TaskSnapshot>,
}

pub struct ManagedTaskRegistry {
    state: Mutex<RegistryState>,
    changed: Condvar,
    resources: Mutex<Option<Arc<crate::resource_governor::ResourceGovernor>>>,
}

impl Default for ManagedTaskRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                accepting: true,
                ..RegistryState::default()
            }),
            changed: Condvar::new(),
            resources: Mutex::new(None),
        }
    }
}

impl ManagedTaskRegistry {
    /// Attach the process resource authority after settings have loaded. The
    /// registry only auto-classifies runtime download workers here; every
    /// other lane acquires at its short, bounded work-unit boundary.
    pub fn attach_resource_governor(
        &self,
        governor: Arc<crate::resource_governor::ResourceGovernor>,
    ) {
        *self.resources.lock().expect("resource governor link") = Some(governor);
    }

    pub fn resource_governor(&self) -> Option<Arc<crate::resource_governor::ResourceGovernor>> {
        self.resources
            .lock()
            .expect("resource governor link")
            .clone()
    }

    pub fn spawn<F>(
        self: &Arc<Self>,
        owner: impl Into<String>,
        key: impl Into<String>,
        priority: TaskPriority,
        task: F,
    ) -> Result<(), SpawnTaskError>
    where
        F: FnOnce(TaskContext) -> Result<(), String> + Send + 'static,
    {
        let owner = owner.into();
        let key = key.into();
        let map_key = (owner.clone(), key.clone());
        let mut old_handle = None;
        let cancel = CancellationToken::new();
        {
            let mut state = self.state.lock().expect("managed task mutex");
            if !state.accepting {
                return Err(SpawnTaskError::Stopping);
            }
            if let Some(existing) = state.tasks.get(&map_key)
                && existing.snapshot.state == TaskState::Running
            {
                return Err(SpawnTaskError::AlreadyRunning { owner, key });
            }
            if let Some(mut completed) = state.tasks.remove(&map_key) {
                state.history.push(completed.snapshot);
                old_handle = completed.handle.take();
            }
            state.tasks.insert(
                map_key.clone(),
                TaskEntry {
                    snapshot: TaskSnapshot {
                        owner: owner.clone(),
                        key: key.clone(),
                        priority,
                        started_at: SystemTime::now(),
                        progress: None,
                        last_error: None,
                        state: TaskState::Running,
                        ended_at: None,
                    },
                    cancel: cancel.clone(),
                    handle: None,
                },
            );
        }
        if let Some(handle) = old_handle {
            let _ = handle.join();
        }

        let registry = Arc::clone(self);
        let resources = self
            .resources
            .lock()
            .expect("resource governor link")
            .clone();
        let thread_owner = owner.clone();
        let thread_key = key.clone();
        let context = TaskContext {
            owner,
            key,
            cancel: cancel.clone(),
            registry: Arc::downgrade(self),
        };
        let spawn = std::thread::Builder::new()
            .name(format!("pp-{}", thread_key))
            .spawn(move || {
                let download_resource =
                    if thread_owner == "runtime" && thread_key.starts_with("model-download-") {
                        resources.as_ref().and_then(|resources| {
                            resources.acquire(
                                crate::resource_governor::ResourceLane::ModelDownload,
                                &cancel.flag(),
                            )
                        })
                    } else {
                        None
                    };
                let result = if resources.is_some()
                    && thread_owner == "runtime"
                    && thread_key.starts_with("model-download-")
                    && download_resource.is_none()
                {
                    Ok(Ok(()))
                } else {
                    catch_unwind(AssertUnwindSafe(|| task(context)))
                };
                drop(download_resource);
                registry.update(&thread_owner, &thread_key, |entry| {
                    entry.snapshot.ended_at = Some(SystemTime::now());
                    entry.snapshot.state = if cancel.is_cancelled() {
                        TaskState::Cancelled
                    } else {
                        match result {
                            Ok(Ok(())) => TaskState::Completed,
                            Ok(Err(error)) => {
                                entry.snapshot.last_error = Some(error);
                                TaskState::Failed
                            }
                            Err(_) => {
                                entry.snapshot.last_error = Some("task panicked".into());
                                TaskState::Failed
                            }
                        }
                    };
                });
                registry.changed.notify_all();
            });

        match spawn {
            Ok(handle) => {
                let mut state = self.state.lock().expect("managed task mutex");
                if let Some(entry) = state.tasks.get_mut(&map_key) {
                    entry.handle = Some(handle);
                }
                self.changed.notify_all();
                Ok(())
            }
            Err(error) => {
                self.state
                    .lock()
                    .expect("managed task mutex")
                    .tasks
                    .remove(&map_key);
                self.changed.notify_all();
                Err(SpawnTaskError::Spawn(error.to_string()))
            }
        }
    }

    pub fn cancel(&self, owner: &str, key: &str) -> bool {
        let state = self.state.lock().expect("managed task mutex");
        let Some(entry) = state.tasks.get(&(owner.to_owned(), key.to_owned())) else {
            return false;
        };
        if entry.snapshot.state != TaskState::Running {
            return false;
        }
        entry.cancel.cancel();
        true
    }

    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let state = self.state.lock().expect("managed task mutex");
        state
            .history
            .iter()
            .cloned()
            .chain(state.tasks.values().map(|entry| entry.snapshot.clone()))
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .expect("managed task mutex")
            .tasks
            .values()
            .filter(|entry| entry.snapshot.state == TaskState::Running)
            .count()
    }

    pub fn is_running(&self, owner: &str, key: &str) -> bool {
        self.state
            .lock()
            .expect("managed task mutex")
            .tasks
            .get(&(owner.to_owned(), key.to_owned()))
            .is_some_and(|entry| entry.snapshot.state == TaskState::Running)
    }

    pub fn managed_count(&self) -> usize {
        self.state.lock().expect("managed task mutex").tasks.len()
    }

    pub fn wait_for_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("managed task mutex");
        while state
            .tasks
            .values()
            .any(|entry| entry.snapshot.state == TaskState::Running)
        {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("managed task condvar");
            state = next;
        }
        true
    }

    /// Close admission and signal every running task without waiting. The App
    /// uses this first shutdown-barrier phase before dropping watcher event
    /// sources, then calls [`Self::shutdown`] to await acknowledgements.
    pub fn begin_shutdown(&self) {
        let mut state = self.state.lock().expect("managed task mutex");
        state.accepting = false;
        for entry in state.tasks.values() {
            if entry.snapshot.state == TaskState::Running {
                entry.cancel.cancel();
            }
        }
        self.changed.notify_all();
    }

    pub fn shutdown(&self, timeout: Duration) -> ShutdownReport {
        let deadline = Instant::now() + timeout;
        self.begin_shutdown();
        let mut state = self.state.lock().expect("managed task mutex");

        while state
            .tasks
            .values()
            .any(|entry| entry.snapshot.state == TaskState::Running || entry.handle.is_none())
        {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("managed task condvar");
            state = next;
        }

        let remaining = state
            .tasks
            .values()
            .filter(|entry| entry.snapshot.state == TaskState::Running || entry.handle.is_none())
            .map(|entry| (entry.snapshot.owner.clone(), entry.snapshot.key.clone()))
            .collect::<Vec<_>>();
        let acknowledged = remaining.is_empty();

        let completed_keys = state
            .tasks
            .iter()
            .filter(|(_, entry)| entry.snapshot.state != TaskState::Running)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut handles = Vec::with_capacity(completed_keys.len());
        for key in completed_keys {
            if let Some(mut entry) = state.tasks.remove(&key) {
                state.history.push(entry.snapshot);
                if let Some(handle) = entry.handle.take() {
                    handles.push(handle);
                }
            }
        }
        drop(state);
        for handle in handles {
            let _ = handle.join();
        }

        ShutdownReport {
            acknowledged,
            remaining,
        }
    }

    fn update(&self, owner: &str, key: &str, update: impl FnOnce(&mut TaskEntry)) {
        let mut state = self.state.lock().expect("managed task mutex");
        if let Some(entry) = state.tasks.get_mut(&(owner.to_owned(), key.to_owned())) {
            update(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ProcessingIntensity;
    use std::sync::mpsc;

    #[test]
    fn tasks_are_single_flight_by_owner_and_key() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        registry
            .spawn("runtime", "converge", TaskPriority::Background, move |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
        started_rx.recv().unwrap();

        assert_eq!(
            registry.spawn("runtime", "converge", TaskPriority::Background, |_| Ok(())),
            Err(SpawnTaskError::AlreadyRunning {
                owner: "runtime".into(),
                key: "converge".into(),
            })
        );
        finish_tx.send(()).unwrap();
        assert!(registry.shutdown(Duration::from_secs(1)).acknowledged);
    }

    #[test]
    fn model_download_workers_obey_the_shared_pause_gate() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        let governor = Arc::new(crate::resource_governor::ResourceGovernor::new(
            ProcessingIntensity::Balanced,
            true,
        ));
        registry.attach_resource_governor(Arc::clone(&governor));
        let (ran_tx, ran_rx) = mpsc::channel();
        registry
            .spawn(
                "runtime",
                "model-download-7",
                TaskPriority::Background,
                move |_| {
                    ran_tx.send(()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        assert!(
            ran_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "paused downloads must wait before transport work starts"
        );
        governor.configure(ProcessingIntensity::Balanced, false);
        ran_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(registry.shutdown(Duration::from_secs(1)).acknowledged);
    }

    #[test]
    fn shutdown_cancels_joins_and_leaves_zero_managed_tasks() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        let (started_tx, started_rx) = mpsc::channel();
        registry
            .spawn(
                "integrity",
                "doctor",
                TaskPriority::Maintenance,
                move |task| {
                    started_tx.send(()).unwrap();
                    assert!(task.wait_for_cancel(Duration::from_secs(5)));
                    Ok(())
                },
            )
            .unwrap();
        started_rx.recv().unwrap();

        let report = registry.shutdown(Duration::from_secs(1));
        assert_eq!(
            report,
            ShutdownReport {
                acknowledged: true,
                remaining: Vec::new(),
            }
        );
        assert_eq!(registry.active_count(), 0);
        assert_eq!(registry.managed_count(), 0);
        assert!(matches!(
            registry.snapshots().last().unwrap().state,
            TaskState::Cancelled
        ));
        assert_eq!(
            registry.spawn("late", "work", TaskPriority::Background, |_| Ok(())),
            Err(SpawnTaskError::Stopping),
            "shutdown permanently prevents task respawn"
        );
    }

    #[test]
    fn progress_error_and_terminal_state_remain_observable() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        registry
            .spawn("integrity", "doctor", TaskPriority::Maintenance, |task| {
                task.report_progress(0.5, "previews");
                task.report_error("one root offline");
                Ok(())
            })
            .unwrap();
        assert!(registry.wait_for_idle(Duration::from_secs(1)));
        assert!(registry.shutdown(Duration::from_secs(1)).acknowledged);
        let snapshot = registry.snapshots().pop().unwrap();
        assert_eq!(snapshot.state, TaskState::Completed);
        assert_eq!(snapshot.last_error.as_deref(), Some("one root offline"));
        assert_eq!(
            snapshot.progress,
            Some(TaskProgress {
                fraction: 0.5,
                message: "previews".into(),
            })
        );
    }

    #[test]
    fn all_long_lived_lanes_acknowledge_the_two_phase_shutdown_barrier() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        let (started_tx, started_rx) = mpsc::channel();
        for (owner, key) in [
            ("scheduler", "ingest-pump"),
            ("derived", "preview-pump"),
            ("derived", "interactive-raw-pump"),
            ("derived", "embedding-pump"),
            ("monitor", "volume-probe"),
            ("maintenance", "library-and-store"),
            ("scheduler", "sidecar-pump"),
            ("scheduler", "runtime-pump"),
        ] {
            let tx = started_tx.clone();
            registry
                .spawn(owner, key, TaskPriority::Background, move |task| {
                    tx.send(()).unwrap();
                    task.wait_for_cancel(Duration::from_secs(5));
                    Ok(())
                })
                .unwrap();
        }
        for _ in 0..8 {
            started_rx.recv().unwrap();
        }
        assert!(registry.is_running("scheduler", "ingest-pump"));
        assert!(registry.is_running("derived", "preview-pump"));
        assert!(registry.is_running("derived", "interactive-raw-pump"));
        assert!(registry.is_running("derived", "embedding-pump"));
        assert!(registry.is_running("monitor", "volume-probe"));
        assert!(registry.is_running("maintenance", "library-and-store"));
        assert!(registry.is_running("scheduler", "sidecar-pump"));
        assert!(registry.is_running("scheduler", "runtime-pump"));

        registry.begin_shutdown();
        assert_eq!(
            registry.spawn(
                "library",
                "late-scan",
                TaskPriority::Maintenance,
                |_| Ok(())
            ),
            Err(SpawnTaskError::Stopping)
        );
        let report = registry.shutdown(Duration::from_secs(1));
        assert!(report.acknowledged);
        assert!(report.remaining.is_empty());
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn managed_quit_phase_matrix_acknowledges_every_filesystem_writer_lane() {
        let registry = Arc::new(ManagedTaskRegistry::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (ack_tx, ack_rx) = mpsc::channel();
        let phases = [
            ("initial scan", "library", "root-scan:acceptance"),
            ("resume reconcile", "library", "resume-reconcile"),
            ("preview generation", "derived", "preview-pump"),
            ("embedding backfill", "derived", "embedding-pump"),
            ("model download", "runtime", "model-download-acceptance"),
            ("sidecar flush pump", "scheduler", "sidecar-pump"),
        ];
        for (phase, owner, key) in phases {
            let started = started_tx.clone();
            let acknowledged = ack_tx.clone();
            registry
                .spawn(owner, key, TaskPriority::Background, move |task| {
                    started.send(phase).unwrap();
                    assert!(
                        task.wait_for_cancel(Duration::from_secs(5)),
                        "{phase} did not receive shutdown cancellation"
                    );
                    acknowledged.send(phase).unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let mut entered = Vec::new();
        for _ in 0..phases.len() {
            entered.push(started_rx.recv().unwrap());
        }
        entered.sort_unstable();

        registry.begin_shutdown();
        let mut acknowledged = Vec::new();
        for _ in 0..phases.len() {
            acknowledged.push(
                ack_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("phase acknowledged cancellation"),
            );
        }
        acknowledged.sort_unstable();
        assert_eq!(acknowledged, entered);

        let report = registry.shutdown(Duration::from_secs(1));
        assert!(report.acknowledged);
        assert!(report.remaining.is_empty());
        assert_eq!(registry.active_count(), 0);
    }
}
