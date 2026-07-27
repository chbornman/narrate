//! Application state: thin composition of photoproof-core engines. The shell
//! owns wiring and lifetimes; all business logic stays in the core.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use photoproof_connectors::SherpaOnlineTranscriber;
use photoproof_connectors::silero::SileroVad;
use photoproof_core::capture::{CaptureDrain, CaptureEngine, SubjectNoteSink, SystemClock};
use photoproof_core::collections::Collections;
use photoproof_core::library::{Library, RootWatcherHandle};
use photoproof_core::retrieval::PpvecStore;
use photoproof_core::search::Searcher;
use photoproof_core::sidecar::SidecarEngine;
use photoproof_core::topics::Topics;
use photoproof_core::{EventStore, SessionContext, SessionId, UtcMillis};

use crate::command_work::{CommandShutdownReport, CommandWorkRegistry};
use crate::convergence::{StateConvergence, StateDomain};
use crate::error::CmdError;
use crate::lifecycle::{AppLifecycle, LifecyclePhase, Subsystem, SubsystemHealth};
use crate::managed_tasks::{
    ManagedTaskRegistry, ShutdownReport, SpawnTaskError, TaskContext, TaskPriority,
};
use crate::resource_governor::{ResourceGovernor, ResourceLane};
use crate::runtime::{ActiveVectorTarget, RuntimeHost};
use crate::scope::ScopeTracker;
use crate::search_types;
use crate::session::SessionManager;
use crate::settings::{
    self, AppSettings, ControlFileIssue, ControlFileRecovery, ControlFileSource, LiveControlFile,
    LiveControlState, LiveControlWatcher,
};
use tauri::{AppHandle, Emitter, Runtime};

/// Cadence of the `pp-plan-converge` loop: every consent/config/download
/// mutation is re-applied to the supervisors within this interval (the
/// "within a couple of seconds" self-heal latency that
/// `RuntimeHost::apply_supervisor_plan` documents — this const is the one
/// authoritative home for that number).
const PLAN_CONVERGE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Pump-side wait between `drain_capture_at_quit` iterations while
/// trailing voice finals land at shutdown. The loop's BOUND is the
/// engine's own `DRAIN_WINDOW_MS` (5 s) deadline, so this only sets the
/// iteration granularity (~50 iterations worst case): short enough that
/// quit feels immediate when the final lands early.
const QUIT_DRAIN_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// Background owners get this long to acknowledge cooperative cancellation
/// before shutdown continues. The wait is bounded so a wedged filesystem call
/// can never trap the process in quit forever.
const MANAGED_TASK_SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
const STARTUP_DEPENDENCY_TICK: std::time::Duration = std::time::Duration::from_millis(50);

/// Capability token proving every shell-owned task and every admitted finite
/// IPC read/write acknowledged shutdown. Final data flush and WAL checkpoint
/// live strictly after this token is constructed.
struct FinalizationGate;

#[derive(Debug)]
struct FinalizationBarrierFailure {
    managed: ShutdownReport,
    commands: CommandShutdownReport,
}

fn await_finalization_gate(
    tasks: &ManagedTaskRegistry,
    command_work: &CommandWorkRegistry,
    timeout: std::time::Duration,
) -> Result<FinalizationGate, FinalizationBarrierFailure> {
    let managed = tasks.shutdown(timeout);
    let commands = command_work.shutdown(timeout);
    if managed.acknowledged && commands.acknowledged {
        Ok(FinalizationGate)
    } else {
        Err(FinalizationBarrierFailure { managed, commands })
    }
}

impl FinalizationGate {
    fn checkpoint(&self, store: &EventStore) -> Result<(), photoproof_core::StoreError> {
        self.checkpoint_inner(store, || {})
    }

    fn checkpoint_inner(
        &self,
        store: &EventStore,
        before: impl FnOnce(),
    ) -> Result<(), photoproof_core::StoreError> {
        before();
        store.checkpoint_at_shutdown()
    }

    #[cfg(test)]
    fn checkpoint_observed(
        &self,
        store: &EventStore,
        before: impl FnOnce(),
    ) -> Result<(), photoproof_core::StoreError> {
        self.checkpoint_inner(store, before)
    }
}

/// SQLite busy timeout for the debug panel's read-only sibling connection.
/// The connection shares a WAL database with the ingest pump and sidecar
/// engine, so the timeout must outlast their longest write transaction or
/// debug reads error spuriously. Named from core's spec-pinned EVENTS §5.1
/// constant so this fifth connection to the events file cannot silently
/// diverge from the four core opens.
#[cfg(any(feature = "debug-panel", debug_assertions))]
const READQ_BUSY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(photoproof_core::store::BUSY_TIMEOUT_MS);

pub struct App {
    /// Shared with the sidecar engine (`SidecarEngine::new_shared`); lives
    /// for the process. Shutdown flushing is explicit, not Drop-driven.
    pub store: Arc<EventStore>,
    pub library: Arc<Library>,
    /// The library implements `ImageLocator` directly (DECISIONS B29).
    pub engine: SidecarEngine<'static, Arc<Library>>,
    /// Collections (RETRIEVAL §10, P7.3): user truth mirrored to
    /// `collections.photoproof.json` by the sidecar pump's tick.
    pub collections: Arc<Collections>,
    /// Manual topics (DESIGN-TOPICS-COLLECTIONS.md): saved phrases and authored
    /// notes over the shared DB. Explicit journal export/import preserves that
    /// intent; ranked image membership is recomputed affinity and never copied.
    pub topics: Arc<Topics>,
    pub app_data: PathBuf,
    pub scope: Mutex<ScopeTracker>,
    pub session: Mutex<SessionManager>,
    /// Read-only sibling connection for the debug panel's raw-row reads
    /// (dev builds only; every product-facing read goes through core APIs).
    #[cfg(any(feature = "debug-panel", debug_assertions))]
    pub readq: Mutex<rusqlite::Connection>,
    pub watchers: Mutex<HashMap<String, RootWatcherHandle>>,
    pub settings: Mutex<AppSettings>,
    /// Post-Usable reload attempts and their retained recovery/error truth.
    /// Startup recovery remains in the fields below; this state advances for
    /// every externally observed settings/config/tuning candidate.
    pub live_controls: Mutex<LiveControlState>,
    /// Startup recovery truth for the two shell-owned control files. Kept
    /// alongside the live settings so application health can distinguish a
    /// normal first run from LKG recovery, quarantine, or an unavailable file.
    pub settings_recovery: Result<ControlFileRecovery, ControlFileIssue>,
    pub device_identity_recovery: ControlFileRecovery,
    /// Recovery truth for the core-owned tuning control file. Tuning failures
    /// never prevent the journal from opening, but they must not disappear
    /// behind an implicit defaults fallback.
    pub tuning_recovery: Result<
        photoproof_core::tuning::TuningControlLoad,
        photoproof_core::runtime::ControlFileError,
    >,
    /// Last query echo for the debug panel's Search tab.
    pub last_search: Mutex<Option<search_types::QueryEcho>>,
    /// The M1 search engine (RETRIEVAL §4, packet P3.1) on its own
    /// connection; `interrupt()` cancels in-flight queries on new keystrokes.
    pub searcher: Searcher,
    /// P7.4: the PPVEC flat-file vector store (RETRIEVAL §1.3). Backs both
    /// the embedding backfill drain (writes) and the hybrid search rig
    /// (reads). One store for the process; its own SQLite metadata
    /// connection lives inside.
    pub vectors: Arc<PpvecStore>,
    /// The model runtime (RUNTIME, P6.2): instance lock, orphan sweep,
    /// tier, manifest, consent, downloads. No supervised child exists
    /// until P6.3 vendors real binaries; readiness stays false and the
    /// app IS the degraded mode that is the whole M1 product (§7).
    pub runtime: Arc<RuntimeHost>,
    /// P6.4: the LIVE capture engine over the supervised sherpa client —
    /// one for the process, shared between commands (toggle/scope/
    /// indicator) and the `pp-mic` audio thread. `None` only when the
    /// in-process VAD failed to build: the app runs, the mic stays away.
    /// Lock order is session → capture everywhere; the mic thread takes
    /// capture only.
    pub capture: Arc<Mutex<Option<CaptureEngine<'static, SystemClock>>>>,
    /// The running mic thread, present exactly while armed; dropping the
    /// handle stops and joins it (and the cpal stream with it).
    pub mic: Mutex<Option<crate::mic::MicHandle>>,
    /// Generation of the newest post-disarm drain. A rapid re-arm/disarm can
    /// overlap the prior managed task's final observation; only the newest
    /// generation may pump trailing voice events.
    pub mic_drain_generation: AtomicU64,
    /// Live walk visibility (ingest empty-state honesty): user-initiated
    /// scans (add-root initial scan, rescan) register here so
    /// `pump::ingest_status` can report `scanning`/`discovered` WHILE the
    /// walk runs — the queue's pass counters only materialize at hash
    /// time, which left the whole walk dark and the empty grid lying "No
    /// photographs" (founder, June 2026).
    pub scans: ScanTracker,
    pub shutdown: Arc<AtomicBool>,
    /// Launch/quit phase and independently degraded subsystem health.
    pub lifecycle: Arc<AppLifecycle>,
    /// Process-owned background work. New migrations land here instead of
    /// detached `thread::spawn` calls so quit can cancel and acknowledge them.
    pub tasks: Arc<ManagedTaskRegistry>,
    /// Finite IPC work running on Tauri's shared blocking pool. Those threads
    /// need their own admission/join barrier because this process did not
    /// spawn them through `ManagedTaskRegistry`.
    pub command_work: Arc<CommandWorkRegistry>,
    /// Monotone cross-window snapshot/event clock. Every committed settings,
    /// roots, collections, runtime, or preview-cache change advances it.
    pub convergence: StateConvergence,
    /// Retained-log and previous-unclean-launch state for installed shells.
    pub diagnostics: Option<crate::diagnostics::CrashDiagnostics>,
    /// Capacity truth and conservative admission for large reproducible
    /// writers. Its recursive inventory runs off the setup/ingest lanes.
    pub disk: Arc<crate::disk::DiskGovernor>,
    /// One admission/priority authority for all expensive desktop work.
    pub resources: Arc<ResourceGovernor>,
    /// File-sink/launch-marker setup failure, retained because it occurred
    /// before tracing was installed and otherwise could not report itself.
    pub diagnostics_error: Option<String>,
    /// Last startup integrity pass, retained after its managed task exits so
    /// health consumers can inspect S5 cleanup and relink self-healing.
    pub repair_integrity: Mutex<crate::doctor::RepairIntegritySnapshot>,
    /// Coalesces readiness-triggered vector repairs. A model swap that lands
    /// while repair is running replaces the pending target and runs next;
    /// repeated 500 ms status observations of the same Ready generation do
    /// not duplicate I/O.
    vector_repair: Mutex<VectorRepairState>,
}

/// Registry of in-flight filesystem walks. `begin()` hands out a guard so
/// EVERY exit path (success, error, unwind) de-registers — a failed scan
/// must never pin `scanning: true` forever.
#[derive(Default)]
pub struct ScanTracker {
    /// In-flight walk count (an add-root scan and a rescan can overlap).
    active: AtomicUsize,
    /// Files discovered across in-flight walks. Shared as an `Arc` because
    /// `ScanOptions::discovered` (core) takes the counter by handle — the
    /// walk thread bumps it directly, no channel, no polling seam.
    discovered: Arc<AtomicU64>,
}

#[derive(Default)]
struct VectorRepairState {
    pending: Option<ActiveVectorTarget>,
    in_flight: Option<ActiveVectorTarget>,
    last_completed: Option<ActiveVectorTarget>,
    running: bool,
}

impl VectorRepairState {
    fn defer(&mut self, target: ActiveVectorTarget) {
        if self.last_completed.as_ref() != Some(&target) && self.in_flight.as_ref() != Some(&target)
        {
            self.pending = Some(target);
        }
    }

    fn enqueue(&mut self, target: ActiveVectorTarget) -> bool {
        if self.last_completed.as_ref() == Some(&target) || self.in_flight.as_ref() == Some(&target)
        {
            return false;
        }
        if self.pending.as_ref() == Some(&target) {
            if self.running {
                return false;
            }
            self.running = true;
            return true;
        }
        self.pending = Some(target);
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }

    fn take_pending(&mut self) -> Option<ActiveVectorTarget> {
        let target = self.pending.take()?;
        self.in_flight = Some(target.clone());
        Some(target)
    }

    fn completed(&mut self, target: ActiveVectorTarget) {
        self.in_flight = None;
        self.last_completed = Some(target);
    }

    fn external_completed(&mut self, target: ActiveVectorTarget) {
        if self.pending.as_ref() == Some(&target) {
            self.pending = None;
        }
        self.last_completed = Some(target);
    }

    fn stopped(&mut self) {
        self.in_flight = None;
        self.running = false;
    }
}

impl ScanTracker {
    /// Register a walk. The counter resets only on the FIRST concurrent
    /// walk: it reads as "found so far" for the current burst of scan
    /// activity, not a forever-total across the app's lifetime.
    pub fn begin(&self) -> ScanGuard<'_> {
        if self.active.fetch_add(1, Ordering::SeqCst) == 0 {
            self.discovered.store(0, Ordering::SeqCst);
        }
        ScanGuard(self)
    }

    /// The shared counter for `ScanOptions::discovered`.
    pub fn counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.discovered)
    }

    /// (scanning, discovered) — folded into `IngestStatus` by the pump.
    pub fn snapshot(&self) -> (bool, u64) {
        (
            self.active.load(Ordering::SeqCst) > 0,
            self.discovered.load(Ordering::Relaxed),
        )
    }
}

/// De-registers its walk on drop (the RAII shape `begin()` documents).
pub struct ScanGuard<'a>(&'a ScanTracker);

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn control_file_recovery_health(
    settings_label: &str,
    settings: &ControlFileRecovery,
    device_identity: Option<&ControlFileRecovery>,
    tuning: &Result<
        photoproof_core::tuning::TuningControlLoad,
        photoproof_core::runtime::ControlFileError,
    >,
) -> SubsystemHealth {
    let mut notes = Vec::new();
    let mut inspect = |label: &str, recovery: &ControlFileRecovery| {
        if recovery.source == ControlFileSource::LastKnownGood {
            notes.push(format!("{label} recovered from last-known-good"));
        }
        if !recovery.quarantined.is_empty() {
            notes.push(format!(
                "{label} quarantined {} corrupt file(s)",
                recovery.quarantined.len()
            ));
        }
        if !recovery.warnings.is_empty() {
            notes.push(format!(
                "{label} has {} durability warning(s)",
                recovery.warnings.len()
            ));
        }
    };
    inspect(settings_label, settings);
    if let Some(device_identity) = device_identity {
        inspect("device identity", device_identity);
    }
    match tuning {
        Ok(tuning) => {
            if tuning.recovery.source == photoproof_core::runtime::ControlFileSource::LastKnownGood
            {
                notes.push("tuning recovered from last-known-good".into());
            }
            if !tuning.recovery.quarantined.is_empty() {
                notes.push(format!(
                    "tuning quarantined {} corrupt file(s)",
                    tuning.recovery.quarantined.len()
                ));
            }
            if !tuning.recovery.warnings.is_empty() {
                notes.push(format!(
                    "tuning has {} durability warning(s)",
                    tuning.recovery.warnings.len()
                ));
            }
            if !tuning.validation_warnings.is_empty() {
                notes.push(format!(
                    "tuning has {} unsupported key(s)",
                    tuning.validation_warnings.len()
                ));
            }
        }
        Err(error) => notes.push(format!("tuning unavailable: {error}")),
    }
    if notes.is_empty() {
        SubsystemHealth::Healthy
    } else {
        SubsystemHealth::Degraded {
            summary: notes.join("; "),
        }
    }
}

impl App {
    #[cfg(test)]
    pub fn init(app_data: PathBuf) -> Result<Self, CmdError> {
        Self::init_with_diagnostics(app_data, None, None)
    }

    pub fn init_with_diagnostics(
        app_data: PathBuf,
        diagnostics: Option<crate::diagnostics::CrashDiagnostics>,
        diagnostics_error: Option<String>,
    ) -> Result<Self, CmdError> {
        let lifecycle = Arc::new(AppLifecycle::default());
        lifecycle
            .transition(LifecyclePhase::OpeningData)
            .expect("Cold -> OpeningData");
        let tasks = Arc::new(ManagedTaskRegistry::default());
        let command_work = Arc::new(CommandWorkRegistry::default());
        std::fs::create_dir_all(&app_data)?;
        // Install the centralized tuning config BEFORE any search or preview
        // work: reads `<app-data>/tuning.toml` if present (else ship-defaults),
        // range-validating every field so a hand edit can never inject a silent
        // bad number. One process-global init; hybrid.rs and preview.rs read it.
        let tuning_recovery = photoproof_core::tuning::init_from_checked(&app_data);
        // An unrecoverable tuning control file degrades settings health but
        // cannot strand startup. Force the same safe shipped defaults that the
        // compatibility initializer uses before any consumer can read tuning.
        if tuning_recovery.is_err() {
            let _ = photoproof_core::tuning::tuning();
        }
        let db_path = app_data.join("photoproof.db");
        let cache_dir = app_data.join("previews");

        let store = Arc::new(EventStore::open(&db_path)?);
        let library = Arc::new(Library::open(&db_path, &cache_dir)?);
        let engine =
            SidecarEngine::new_shared(store.clone(), &db_path, &app_data, library.clone())?;
        // Open AFTER the sidecar engine so the schema exists; the open-time
        // reconcile union-merges an existing app-data
        // collections.photoproof.json into the database. The export-restore
        // case (fresh machine, only the one-click export) is covered inside
        // rebuild_from_sidecars, which imports the collections file it
        // finds beside the manifest (RETRIEVAL 10.2).
        let collections = Arc::new(Collections::open(&db_path, &app_data)?);
        // Manual topics: opened after the schema exists (the same throwaway-open
        // migration the collections engine relies on); no portability file.
        let topics =
            Arc::new(Topics::open(&db_path).map_err(|e| CmdError::Invalid(e.to_string()))?);
        let searcher = Searcher::open(&db_path).map_err(|e| CmdError::Invalid(e.to_string()))?;
        // PPVEC store beside the journal db (RETRIEVAL §1.3:
        // appdata/vectors/). Opening it is cheap (it lazily mmaps spaces on
        // first touch); the embedding drain and search rig share it.
        let vectors = Arc::new(
            PpvecStore::open(&db_path, app_data.join("vectors"))
                .map_err(|e| CmdError::Invalid(e.to_string()))?,
        );
        lifecycle.set_health(Subsystem::Storage, SubsystemHealth::Healthy);

        // CAPTURE §2.4 crash recovery, before opening the launch session:
        // any session left open by a dead process closes at its last event's
        // ts (else its start) with `closed_clean = false`, and close
        // processing is enqueued ONCE. Recovery mints no events. The empty
        // P6.1 processor registry then drains the pending queue (idempotent;
        // M3 registers the real processors).
        photoproof_core::capture::recover_crashed_sessions(&store)?;
        photoproof_core::capture::CloseProcessing::new().run_pending(&store)?;

        let device_identity = settings::device_id_checked(&app_data)
            .map_err(|issue| CmdError::Invalid(format!("device identity unavailable: {issue}")))?;
        let ctx = SessionContext {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: device_identity.device_id,
            root_context: None,
        };
        let mut session = SessionManager::open(&store, ctx)?;
        let settings_load = settings::load_checked(&app_data);
        let (app_settings, settings_recovery) = match settings_load {
            Ok(loaded) => {
                let health = control_file_recovery_health(
                    "settings",
                    &loaded.recovery,
                    Some(&device_identity.recovery),
                    &tuning_recovery,
                );
                lifecycle.set_health(Subsystem::Settings, health);
                (loaded.settings, Ok(loaded.recovery))
            }
            Err(issue) => {
                let tuning_suffix = tuning_recovery
                    .as_ref()
                    .err()
                    .map(|error| format!("; tuning unavailable: {error}"))
                    .unwrap_or_default();
                lifecycle.set_health(
                    Subsystem::Settings,
                    SubsystemHealth::Unavailable {
                        summary: format!(
                            "settings recovery failed at {}: {}{}",
                            issue.path.display(),
                            issue.detail,
                            tuning_suffix
                        ),
                    },
                );
                (AppSettings::default(), Err(issue))
            }
        };
        // RUNTIME init AFTER the journal spine: nothing about journaling
        // ever blocks on the runtime (§7/§10.1). Acquires the §8.5
        // instance lock, sweeps the §8.4 crash net, resolves config +
        // tier, writes the manifest.
        let runtime = Arc::new(RuntimeHost::init_managed(
            app_data.clone(),
            Arc::clone(&tasks),
        ));
        let disk = Arc::new(crate::disk::DiskGovernor::new(
            app_data.clone(),
            runtime.models_dir(),
        ));
        let resources = Arc::new(ResourceGovernor::new(
            app_settings.processing_intensity,
            app_settings.processing_paused,
        ));
        tasks.attach_resource_governor(Arc::clone(&resources));
        lifecycle.set_health(
            Subsystem::Runtime,
            SubsystemHealth::Degraded {
                summary: "hardware capabilities are provisional".into(),
            },
        );
        // Native VAD/session construction can load ONNX and take seconds on a
        // cold machine. Start dark here so the journal/window can become
        // Usable; `start_capture_runtime` builds it under managed ownership
        // after setup returns.
        let capture = Arc::new(Mutex::new(None));
        // The §2.5/§2.2 seam: rotations and closes drain + re-point the
        // engine through the session manager, no caller burden.
        session.attach_capture(Box::new(SharedDrain(Arc::clone(&capture))));

        lifecycle
            .transition(LifecyclePhase::Usable)
            .expect("OpeningData -> Usable");
        Ok(Self {
            store,
            library,
            engine,
            collections,
            topics,
            app_data,
            scope: Mutex::new(ScopeTracker::new()),
            session: Mutex::new(session),
            #[cfg(any(feature = "debug-panel", debug_assertions))]
            readq: Mutex::new(open_read_only(&db_path)?),
            watchers: Mutex::new(HashMap::new()),
            settings: Mutex::new(app_settings),
            live_controls: Mutex::new(LiveControlState::default()),
            settings_recovery,
            device_identity_recovery: device_identity.recovery,
            tuning_recovery,
            last_search: Mutex::new(None),
            searcher,
            vectors,
            runtime,
            capture,
            mic: Mutex::new(None),
            mic_drain_generation: AtomicU64::new(0),
            scans: ScanTracker::default(),
            shutdown: Arc::new(AtomicBool::new(false)),
            lifecycle,
            tasks,
            command_work,
            convergence: StateConvergence::default(),
            diagnostics,
            disk,
            resources,
            diagnostics_error,
            repair_integrity: Mutex::new(crate::doctor::RepairIntegritySnapshot::default()),
            vector_repair: Mutex::new(VectorRepairState::default()),
        })
    }

    /// Start child-process supervision only after the journal/window is usable.
    /// The ticker owns its OS thread and is joined after the trailing ASR drain
    /// during shutdown; it deliberately does not share the earlier managed-task
    /// cancellation barrier.
    pub fn start_supervisor_runtime(
        &self,
    ) -> Result<(), crate::supervisors::SupervisorThreadStartError> {
        if !matches!(
            self.lifecycle.snapshot().phase,
            LifecyclePhase::Usable | LifecyclePhase::Reconciling | LifecyclePhase::Ready
        ) {
            return Err(crate::supervisors::SupervisorThreadStartError::BeforeUsable);
        }
        // Start ownership before applying a Run plan so a successfully spawned
        // child can never exist without a ticker handle responsible for it.
        self.runtime.supervisors.start_tick_thread()?;
        self.runtime.apply_supervisor_plan();
        Ok(())
    }

    /// Start the self-healing runtime-plan loop under process ownership.
    /// Cancellation wakes its interval immediately, so shutdown never waits
    /// for the full convergence cadence.
    pub fn start_plan_convergence(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let runtime = Arc::clone(&self.runtime);
        self.tasks.spawn(
            "runtime",
            "plan-converge",
            TaskPriority::Background,
            move |task| {
                while !task.wait_for_cancel(PLAN_CONVERGE_INTERVAL) {
                    runtime.apply_supervisor_plan();
                }
                Ok(())
            },
        )
    }

    /// Observe all three user-editable installed controls after the app is
    /// usable. One managed single-flight owner covers polling, debounce,
    /// application, and shutdown acknowledgement; no detached filesystem
    /// callback can outlive finalization.
    pub fn start_live_control_watcher<R: Runtime>(
        self: &Arc<Self>,
        handle: AppHandle<R>,
    ) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let app = Arc::clone(self);
        self.tasks.spawn(
            "controls",
            "live-reload",
            TaskPriority::Background,
            move |task| {
                let mut watcher =
                    LiveControlWatcher::new(app.app_data.clone(), settings::CONTROL_FILE_DEBOUNCE);
                while !task.wait_for_cancel(settings::CONTROL_FILE_POLL_INTERVAL) {
                    for file in watcher.poll(std::time::Instant::now()) {
                        if task.is_cancelled() || app.shutdown.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        if let Err(error) = app.apply_live_control(file, &handle) {
                            task.report_error(format!("{}: {error}", file.name()));
                            app.live_controls
                                .lock()
                                .expect("live controls mutex")
                                .failed(file, &error);
                            app.lifecycle.set_health(
                                Subsystem::Settings,
                                SubsystemHealth::Degraded {
                                    summary: format!(
                                        "{} reload failed; last-known-good remains active: {error}",
                                        file.name()
                                    ),
                                },
                            );
                        }
                        watcher.acknowledge(file);
                    }
                }
                Ok(())
            },
        )
    }

    pub(crate) fn apply_live_control<R: Runtime>(
        &self,
        file: LiveControlFile,
        handle: &AppHandle<R>,
    ) -> Result<(), String> {
        self.live_controls
            .lock()
            .expect("live controls mutex")
            .begin_attempt(file);
        match file {
            LiveControlFile::Settings => {
                let loaded =
                    settings::load_checked(&self.app_data).map_err(|error| error.to_string())?;
                let source = match loaded.recovery.source {
                    ControlFileSource::Primary => "primary",
                    ControlFileSource::LastKnownGood => "last-known-good",
                    ControlFileSource::MissingDefault => "missing-default",
                    ControlFileSource::Created => "created",
                };
                let mut warnings = loaded
                    .recovery
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if let Err(error) = settings::prune_control_file_quarantines(
                    &self.app_data,
                    settings::CONTROL_FILE_QUARANTINE_RETENTION,
                ) {
                    warnings.push(format!("quarantine retention: {error}"));
                }
                let next = loaded.settings;
                let (changed, preview_budget_changed) = {
                    let mut current = self.settings.lock().expect("settings mutex");
                    let changed = *current != next;
                    let preview_budget_changed =
                        current.preview_cache_budget_bytes != next.preview_cache_budget_bytes;
                    if changed {
                        self.resources
                            .configure(next.processing_intensity, next.processing_paused);
                        *current = next.clone();
                    }
                    (changed, preview_budget_changed)
                };
                if preview_budget_changed {
                    self.library
                        .evict_preview_cache(next.preview_cache_budget_bytes);
                }
                self.live_controls
                    .lock()
                    .expect("live controls mutex")
                    .applied(file, source, loaded.recovery.quarantined, warnings);
                if changed {
                    let _ = handle.emit("settings-changed", next);
                    if preview_budget_changed {
                        let _ = handle.emit(
                            "preview-cache-changed",
                            crate::commands::app::preview_cache_snapshot(self),
                        );
                        self.convergence
                            .publish(handle, [StateDomain::Settings, StateDomain::PreviewCache]);
                    } else {
                        self.convergence.publish(handle, [StateDomain::Settings]);
                    }
                }
            }
            LiveControlFile::Tuning => {
                let loaded = photoproof_core::tuning::Tuning::load_checked(&self.app_data)
                    .map_err(|error| error.to_string())?;
                let source = match loaded.recovery.source {
                    photoproof_core::runtime::ControlFileSource::Primary => "primary",
                    photoproof_core::runtime::ControlFileSource::LastKnownGood => "last-known-good",
                    photoproof_core::runtime::ControlFileSource::Missing => "missing-default",
                };
                let current = photoproof_core::tuning::tuning();
                let changed = current != loaded.value;
                if changed && current.preview != loaded.value.preview {
                    self.library
                        .force_repend_pass(photoproof_core::library::PassName::Preview)
                        .map_err(|error| format!("re-pend previews for tuning: {error}"))?;
                }
                if changed
                    && current.voice != loaded.value.voice
                    && let Some(capture) = self.capture.lock().expect("capture mutex").as_mut()
                    && !capture.reconfigure_vad(
                        loaded.value.voice.vad_enter as f32,
                        loaded.value.voice.vad_exit as f32,
                        loaded.value.voice.vad_hang,
                    )
                {
                    return Err("the active capture engine cannot apply live VAD tuning; \
                         last-known-good tuning remains active"
                        .into());
                }
                if changed {
                    photoproof_core::tuning::replace(loaded.value.clone());
                    self.runtime.apply_supervisor_plan();
                }
                let mut warnings = loaded.validation_warnings.clone();
                warnings.extend(loaded.recovery.warnings.iter().map(ToString::to_string));
                if let Err(error) = settings::prune_control_file_quarantines(
                    &self.app_data,
                    settings::CONTROL_FILE_QUARANTINE_RETENTION,
                ) {
                    warnings.push(format!("quarantine retention: {error}"));
                }
                self.live_controls
                    .lock()
                    .expect("live controls mutex")
                    .applied(file, source, loaded.recovery.quarantined, warnings);
                if changed {
                    let _ = handle.emit("tuning-changed", loaded.value);
                    let _ = handle.emit("runtime-status", self.runtime.status());
                    self.convergence.publish(
                        handle,
                        [
                            StateDomain::Settings,
                            StateDomain::Runtime,
                            StateDomain::PreviewCache,
                        ],
                    );
                }
            }
            LiveControlFile::Config => {
                let loaded = self.runtime.reload_config_checked()?;
                let recovery = loaded.status.recovery.as_ref();
                let source = recovery
                    .map(|recovery| match recovery.source {
                        photoproof_core::runtime::ControlFileSource::Primary => "primary",
                        photoproof_core::runtime::ControlFileSource::LastKnownGood => {
                            "last-known-good"
                        }
                        photoproof_core::runtime::ControlFileSource::Missing => "missing-default",
                    })
                    .unwrap_or("unavailable");
                let mut warnings = loaded.status.validation_warnings.clone();
                if let Some(recovery) = recovery {
                    warnings.extend(recovery.warnings.iter().map(ToString::to_string));
                }
                if let Err(error) = settings::prune_control_file_quarantines(
                    &self.app_data,
                    settings::CONTROL_FILE_QUARANTINE_RETENTION,
                ) {
                    warnings.push(format!("quarantine retention: {error}"));
                }
                self.live_controls
                    .lock()
                    .expect("live controls mutex")
                    .applied(
                        file,
                        source,
                        recovery
                            .map(|recovery| recovery.quarantined.clone())
                            .unwrap_or_default(),
                        warnings,
                    );
                if loaded.changed {
                    let _ = handle.emit("runtime-status", self.runtime.status());
                    self.convergence.publish(handle, [StateDomain::Runtime]);
                }
            }
        }
        Ok(())
    }

    /// Validate cached/safe provisional hardware capabilities after the app is
    /// already usable. First-launch detection and Settings re-detection share
    /// this managed lane, so neither setup nor an IPC handler enters a graphics
    /// driver or waits for tier-cache fsync.
    pub fn start_runtime_capability_detection(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        if self.tasks.is_running("runtime", "capability-detect") {
            return Err(SpawnTaskError::AlreadyRunning {
                owner: "runtime".into(),
                key: "capability-detect".into(),
            });
        }
        self.runtime.begin_capability_detection();
        self.lifecycle.set_health(
            Subsystem::Runtime,
            SubsystemHealth::Degraded {
                summary: "detecting hardware capabilities".into(),
            },
        );
        let app = Arc::clone(self);
        let result = self.tasks.spawn(
            "runtime",
            "capability-detect",
            TaskPriority::Background,
            move |task| {
                if task.is_cancelled() || app.shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
                match app.runtime.detect_capabilities(task.cancel_flag()) {
                    Ok(status) => {
                        let tier_cache_degraded = status
                            .control_files
                            .iter()
                            .find(|file| file.name == "tier")
                            .is_some_and(|file| !file.errors.is_empty());
                        let registry_pending = app.runtime.model_registry_recovery_pending();
                        app.lifecycle.set_health(
                            Subsystem::Runtime,
                            if registry_pending {
                                SubsystemHealth::Degraded {
                                    summary:
                                        "hardware detected; verifying recovered model registry"
                                            .into(),
                                }
                            } else if tier_cache_degraded {
                                SubsystemHealth::Degraded {
                                    summary: status.capability_summary.unwrap_or_else(|| {
                                        "hardware detected but tier cache is degraded".into()
                                    }),
                                }
                            } else {
                                SubsystemHealth::Healthy
                            },
                        );
                        // A fresh decision can replace a conservative/cached
                        // provisional tier.
                        app.runtime.apply_supervisor_plan();
                        Ok(())
                    }
                    Err(error) => {
                        app.lifecycle.set_health(
                            Subsystem::Runtime,
                            SubsystemHealth::Degraded {
                                summary: error.clone(),
                            },
                        );
                        Err(error)
                    }
                }
            },
        );
        if let Err(error) = &result {
            self.runtime
                .fail_capability_detection(format!("capability task could not start: {error}"));
            self.lifecycle.set_health(
                Subsystem::Runtime,
                SubsystemHealth::Degraded {
                    summary: format!("capability task could not start: {error}"),
                },
            );
        }
        result
    }

    /// Recover a missing/corrupt/stale installed-model index only after the
    /// shell is usable. Multi-GB hashing must never sit on Tauri setup; models
    /// remain absent from the authoritative installed snapshot until this
    /// managed task durably commits their proof.
    pub fn start_model_registry_recovery(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        if !self.runtime.model_registry_recovery_pending() {
            return Ok(());
        }
        self.lifecycle.set_health(
            Subsystem::Runtime,
            SubsystemHealth::Degraded {
                summary: "verifying recovered model registry".into(),
            },
        );
        let app = Arc::clone(self);
        let result = self.tasks.spawn(
            "runtime",
            "model-registry-recovery",
            TaskPriority::Maintenance,
            move |task| {
                let report = app.runtime.recover_model_registry(&task.cancel_flag())?;
                if report.cancelled {
                    return Ok(());
                }
                if report.remaining > 0 || report.rejected > 0 {
                    app.lifecycle.set_health(
                        Subsystem::Runtime,
                        SubsystemHealth::Degraded {
                            summary: format!(
                                "model registry recovery verified {}, rejected {}, remaining {}",
                                report.verified, report.rejected, report.remaining
                            ),
                        },
                    );
                } else {
                    let status = app.runtime.status();
                    app.lifecycle.set_health(
                        Subsystem::Runtime,
                        if status.capability_state == "ready" {
                            SubsystemHealth::Healthy
                        } else {
                            SubsystemHealth::Degraded {
                                summary: status.capability_summary.unwrap_or_else(|| {
                                    "model registry recovered; hardware detection is pending".into()
                                }),
                            }
                        },
                    );
                }
                app.runtime.apply_supervisor_plan();
                Ok(())
            },
        );
        if let Err(error) = &result {
            self.lifecycle.set_health(
                Subsystem::Runtime,
                SubsystemHealth::Degraded {
                    summary: format!("model registry recovery could not start: {error}"),
                },
            );
        }
        result
    }

    /// Build the in-process VAD/capture engine after the application is usable.
    /// A failed or slow ONNX load degrades voice only and participates in the
    /// same shutdown acknowledgement barrier as other managed startup work.
    pub fn start_capture_runtime(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let app = Arc::clone(self);
        self.tasks.spawn(
            "capture",
            "initialize",
            TaskPriority::Background,
            move |task| {
                let voice = photoproof_core::tuning::tuning().voice;
                let vad = match SileroVad::with_params(
                    voice.vad_enter as f32,
                    voice.vad_exit as f32,
                    voice.vad_hang,
                ) {
                    Ok(vad) => vad,
                    Err(error) => {
                        app.lifecycle.set_health(
                            Subsystem::Capture,
                            SubsystemHealth::Unavailable {
                                summary: error.to_string(),
                            },
                        );
                        tracing::warn!(
                            error = %error,
                            "in-process VAD failed to build; voice capture disabled"
                        );
                        return Err(error.to_string());
                    }
                };
                if task.is_cancelled() || app.shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }

                // Process-lifetime because CaptureEngine borrows the
                // transcriber and the WS client follows supervisor endpoint
                // changes in memory. Allocate only after the expensive VAD
                // build succeeds so a failed voice lane leaks nothing.
                let transcriber: &'static SherpaOnlineTranscriber =
                    Box::leak(Box::new(SherpaOnlineTranscriber::new(
                        app.runtime.asr_model_id(),
                        app.runtime.supervisors.asr_endpoint.clone(),
                    )));
                let engine = CaptureEngine::new(
                    SystemClock::new(),
                    transcriber,
                    Box::new(vad),
                    app.session_id(),
                )
                .with_note_sink(Box::new(SubjectNotes {
                    collections: Arc::clone(&app.collections),
                    topics: Arc::clone(&app.topics),
                }));
                if task.is_cancelled() || app.shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
                *app.capture.lock().expect("capture mutex") = Some(engine);
                app.lifecycle
                    .set_health(Subsystem::Capture, SubsystemHealth::Healthy);
                Ok(())
            },
        )
    }

    /// Readiness-driven active-space repair. The runtime status pump calls this
    /// on every observed snapshot; the coordinator turns that polling seam into
    /// exactly-once work per distinct actually-ready model set.
    pub fn request_active_vector_reconcile(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let target = self.runtime.active_vector_target();
        if target.models.is_empty() {
            return Ok(());
        }
        if self.tasks.is_running("integrity", "startup-doctor") {
            self.vector_repair
                .lock()
                .expect("vector repair mutex")
                .defer(target);
            return Ok(());
        }
        let should_spawn = self
            .vector_repair
            .lock()
            .expect("vector repair mutex")
            .enqueue(target);
        if !should_spawn {
            return Ok(());
        }

        let app = Arc::clone(self);
        let result = self.tasks.spawn(
            "integrity",
            "active-vector-reconcile",
            TaskPriority::Maintenance,
            move |task| loop {
                if task.is_cancelled() {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .stopped();
                    return Ok(());
                }
                let target = {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .take_pending()
                };
                let Some(target) = target else {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .stopped();
                    return Ok(());
                };
                let cancel = task.cancel_flag();
                let Some(_resource) = app.resources.acquire(ResourceLane::Repair, &cancel) else {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .stopped();
                    return Ok(());
                };
                if let Err(error) =
                    crate::doctor::reconcile_vector_spaces(&app, &task, &target.models)
                {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .stopped();
                    return Err(error);
                }
                app.vector_repair
                    .lock()
                    .expect("vector repair mutex")
                    .completed(target);
            },
        );
        if result.is_err() {
            self.vector_repair
                .lock()
                .expect("vector repair mutex")
                .stopped();
        }
        result
    }

    /// Probe mounted volumes and restore live watchers without holding up the
    /// Tauri setup callback. This is process-owned so quit observes the task,
    /// and the cancellation check prevents a late watcher from appearing
    /// after shutdown has started.
    pub fn start_startup_watchers(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        self.spawn_startup_watchers_task(move |app, task| {
            let cancel = task.cancel_flag();
            let Some(_resource) = app.resources.acquire(ResourceLane::StartupIo, &cancel) else {
                return Ok(());
            };
            if let Err(error) = app.library.probe_volumes() {
                app.lifecycle.set_health(
                    Subsystem::Roots,
                    SubsystemHealth::Degraded {
                        summary: format!("startup volume probe failed: {error}"),
                    },
                );
                return Err(error.to_string());
            }
            if task.is_cancelled() {
                return Ok(());
            }

            let roots = app.library.roots().map_err(|error| {
                app.lifecycle.set_health(
                    Subsystem::Roots,
                    SubsystemHealth::Degraded {
                        summary: format!("could not enumerate roots: {error}"),
                    },
                );
                error.to_string()
            })?;
            app.lifecycle
                .set_health(Subsystem::Roots, SubsystemHealth::Healthy);

            let mut failures = Vec::new();
            for root in roots.iter().filter(|root| root.state == "active") {
                if task.is_cancelled() {
                    return Ok(());
                }
                let watcher_resources = Arc::clone(&app.resources);
                match app
                    .library
                    .start_watcher_with_options(&root.root_id, move |cancel| {
                        watcher_resources.watcher_scan(cancel)
                    }) {
                    Ok(handle) => {
                        if task.is_cancelled() {
                            drop(handle);
                            return Ok(());
                        }
                        app.watchers
                            .lock()
                            .expect("watchers mutex")
                            .insert(root.root_id.clone(), handle);
                    }
                    Err(error) => {
                        tracing::warn!(
                            root_id = %root.root_id,
                            error = %error,
                            "watcher unavailable at launch"
                        );
                        failures.push(root.root_id.clone());
                    }
                }
            }
            if failures.is_empty() {
                app.lifecycle
                    .set_health(Subsystem::Watchers, SubsystemHealth::Healthy);
            } else {
                app.lifecycle.set_health(
                    Subsystem::Watchers,
                    SubsystemHealth::Degraded {
                        summary: format!("{} active root watcher(s) unavailable", failures.len()),
                    },
                );
            }
            Ok(())
        })
    }

    /// Scheduling seam kept separate from the native probe/watcher body so an
    /// adversarial test can hold that body indefinitely and prove the usable
    /// barrier and setup caller are already released.
    fn spawn_startup_watchers_task<F>(self: &Arc<Self>, work: F) -> Result<(), SpawnTaskError>
    where
        F: FnOnce(Arc<Self>, TaskContext) -> Result<(), String> + Send + 'static,
    {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let app = Arc::clone(self);
        self.tasks.spawn(
            "library",
            "startup-watchers",
            TaskPriority::Maintenance,
            move |task| work(app, task),
        )
    }

    /// Start the launch integrity sweep as a cancellable, single-flight task.
    pub fn start_startup_doctor(self: &Arc<Self>) -> Result<(), SpawnTaskError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(SpawnTaskError::Stopping);
        }
        let phase = self.lifecycle.snapshot().phase;
        if phase != LifecyclePhase::Ready
            && let Err(error) = self.lifecycle.transition(LifecyclePhase::Reconciling)
        {
            tracing::warn!(%error, "startup doctor rejected by lifecycle");
            return Err(SpawnTaskError::Stopping);
        }
        let app = Arc::clone(self);
        let result = self.tasks.spawn(
            "integrity",
            "startup-doctor",
            TaskPriority::Maintenance,
            move |task| {
                // Watchers must be installed before the full root reconcile:
                // otherwise an edit arriving mid-walk can fall between the
                // scan's directory read and watcher subscription. Both tasks
                // remain off the setup path; this is an explicit dependency
                // between their managed state machines.
                while app.tasks.is_running("library", "startup-watchers") {
                    if task.wait_for_cancel(STARTUP_DEPENDENCY_TICK) {
                        return Ok(());
                    }
                }
                let cancel = task.cancel_flag();
                let Some(_resource) = app.resources.acquire(ResourceLane::StartupIo, &cancel)
                else {
                    return Ok(());
                };
                if let Some(target) = crate::doctor::run_startup_doctor(&app, &task) {
                    app.vector_repair
                        .lock()
                        .expect("vector repair mutex")
                        .external_completed(target);
                }
                if !task.is_cancelled() && phase != LifecyclePhase::Ready {
                    let _ = app.lifecycle.transition(LifecyclePhase::Ready);
                }
                Ok(())
            },
        );
        if let Err(error @ SpawnTaskError::Spawn(_)) = &result {
            self.lifecycle.set_health(
                Subsystem::Previews,
                SubsystemHealth::Degraded {
                    summary: format!("startup doctor unavailable: {error}"),
                },
            );
            let _ = self.lifecycle.transition(LifecyclePhase::Ready);
        }
        result
    }

    /// §2.5 step 3, pump-owned: drain enqueued close processing. Called
    /// from the sidecar pump tick — never from the close/quit path.
    pub fn run_close_processing(&self) -> Result<(), CmdError> {
        let mut session = self.session.lock().expect("session mutex");
        session.run_pending_close_processing(&self.store)?;
        Ok(())
    }

    /// Activity touch (CAPTURE §2.1/§2.2): refreshes the idle timer, rotating
    /// the session across a 30-minute boundary (idle measured on the
    /// monotonic capture clock; `ended_at` = the last activity's wall time).
    pub fn touch(&self) -> Result<(), CmdError> {
        let mut session = self.session.lock().expect("session mutex");
        session.touch(&self.store, &mut EngineFlush { app: self })?;
        Ok(())
    }

    pub fn session_id(&self) -> SessionId {
        self.session.lock().expect("session mutex").id().clone()
    }

    /// Shutdown (CAPTURE §2.5): close the session through the core engine
    /// (capture drain — `NoCapture` until P6.3 attaches the live engine;
    /// the pump-owned bounded drain wait is `pump::drain_capture_at_quit`
    /// — then sidecar flush, then bookkeeping; step 3 is enqueued for the
    /// next launch's pump), and re-flush the session journal afterwards so
    /// the sidecar carries `ended_ts` (SIDECARS S3).
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.lifecycle.transition(LifecyclePhase::Stopping);
        // Phase 1 closes task admission and signals cancellation. Watchers are
        // then dropped before we wait, so no filesystem producer can enqueue
        // fresh work behind an exiting ingest pump. Startup watcher restore
        // also observes cancellation before installing another handle.
        self.runtime.begin_download_shutdown();
        self.command_work.begin_shutdown();
        self.tasks.begin_shutdown();
        self.watchers.lock().expect("watchers mutex").clear();
        let finalization_gate = match await_finalization_gate(
            &self.tasks,
            &self.command_work,
            MANAGED_TASK_SHUTDOWN_WAIT,
        ) {
            Ok(gate) => gate,
            Err(failure) => {
                tracing::error!(
                    remaining = ?failure.managed.remaining,
                    command_work = ?failure.commands.remaining,
                    "managed background tasks did not acknowledge bounded shutdown; \
                     skipping final data flush/checkpoint to avoid racing a live writer"
                );
                // There is no safe finalization barrier while a DB/filesystem task
                // can resume. Stop live producers/consumers best-effort, but leave
                // this launch session open and do NOT flush sidecars/collections or
                // checkpoint the WAL. The next launch's existing crash recovery
                // closes the session and rebuilds derived mirrors from durable DB
                // truth instead of falsely claiming this quit was clean.
                drop(self.mic.lock().expect("mic mutex").take());
                self.runtime
                    .capture_live
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                let supervisor = self
                    .runtime
                    .supervisors
                    .shutdown(crate::supervisors::SHUTDOWN_WAIT);
                if !supervisor.acknowledged || supervisor.panicked {
                    tracing::error!(
                        ?supervisor,
                        "supervisor ticker did not cleanly acknowledge bounded shutdown"
                    );
                }
                self.runtime.embedders.shutdown();
                return;
            }
        };
        let mut clean_shutdown = true;
        // B52: stop the audio thread, then the bounded trailing-final
        // drain — BEFORE the supervisors stop (the ASR child must outlive
        // the drain to deliver finals whose onsets predate the quit).
        drop(self.mic.lock().expect("mic mutex").take());
        {
            let mut capture = self.capture.lock().expect("capture mutex");
            if let Some(engine) = capture.as_mut() {
                crate::pump::drain_capture_at_quit(engine, &self.store, &mut || {
                    std::thread::sleep(QUIT_DRAIN_WAIT);
                });
            }
        }
        self.runtime
            .capture_live
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // P6.4: children walk the §8.4 normal order before we flush.
        let supervisor = self
            .runtime
            .supervisors
            .shutdown(crate::supervisors::SHUTDOWN_WAIT);
        if !supervisor.acknowledged || supervisor.panicked {
            tracing::error!(
                ?supervisor,
                "supervisor ticker did not cleanly acknowledge bounded shutdown; \
                 skipping final data flush/checkpoint"
            );
            self.runtime.embedders.shutdown();
            return;
        }
        // P7.4: drop the in-process ort sessions (no child to reap; this
        // just frees the native sessions and stops any pending build from
        // landing).
        self.runtime.embedders.shutdown();
        let session_id = self.session_id();
        if let Err(e) = self
            .session
            .lock()
            .expect("session mutex")
            .close(&self.store, &mut EngineFlush { app: self })
        {
            clean_shutdown = false;
            tracing::error!(error = %e, "session close failed at shutdown");
        }
        if let Err(e) = self.engine.flush_session(&session_id) {
            clean_shutdown = false;
            tracing::error!(error = %e, "session journal flush failed at shutdown");
        }
        // App shutdown is an immediate-flush trigger (SIDECARS §9.1); the
        // collections file drains with the sidecars.
        if let Err(e) = self.collections.flush(UtcMillis::now()) {
            clean_shutdown = false;
            tracing::error!(error = %e, "collections flush failed at shutdown");
        }
        // Redaction supremacy (EVENTS §7-step-8): truncate the WAL LAST, after
        // every other subsystem has flushed and released the db, so scrubbed
        // plaintext cannot linger in `-wal` past exit. A longer retry budget than
        // idle maintenance (it is worth waiting out a slow reader at quit). If it
        // still blocks, log loudly: the next launch's open() recovers it.
        if let Err(e) = finalization_gate.checkpoint(&self.store) {
            clean_shutdown = false;
            tracing::error!(error = %e, "shutdown WAL checkpoint blocked; -wal retained until next open recovers it");
        }
        if clean_shutdown
            && let Some(diagnostics) = &self.diagnostics
            && let Err(error) = diagnostics.mark_clean_shutdown()
        {
            tracing::error!(%error, "could not clear clean-shutdown marker");
        }
    }
}

/// The session engine's capture seam over the SHARED engine slot: every
/// rotation/close drains and re-points whatever is armed. The session
/// mutex is always taken before this lock (commands go session → capture;
/// the mic thread takes capture only), so the nesting cannot deadlock.
struct SharedDrain(Arc<Mutex<Option<CaptureEngine<'static, SystemClock>>>>);

impl CaptureDrain for SharedDrain {
    fn drain_for_close(&mut self, store: &EventStore, closing: &SessionId) {
        if let Some(engine) = self.0.lock().expect("capture mutex").as_mut() {
            engine.drain_for_close(store, closing);
        }
    }

    fn last_capture_activity(&self) -> Option<(u64, UtcMillis)> {
        self.0
            .lock()
            .expect("capture mutex")
            .as_ref()
            .and_then(CaptureDrain::last_capture_activity)
    }

    fn session_rotated(&mut self, opened: &SessionId) {
        if let Some(engine) = self.0.lock().expect("capture mutex").as_mut() {
            engine.session_rotated(opened);
        }
    }
}

/// DESIGN-VOICE-SUBJECTS.md: the subject-note seam over the shell's
/// `Collections`/`Topics` handles. The capture engine writes EVENTS through
/// its `&EventStore`, but the subject note tables hang off these separate
/// connections (same db); this sink is the thin accessor `on_final` calls to
/// land a collection/topic voice final in its note log. `UtcMillis::now()`
/// matches the typed composer commands' timestamp source exactly.
struct SubjectNotes {
    collections: Arc<Collections>,
    topics: Arc<Topics>,
}

impl SubjectNoteSink for SubjectNotes {
    fn append_collection_note(
        &self,
        collection_id: &str,
        text: &str,
        ts: UtcMillis,
    ) -> Result<(), String> {
        self.collections
            .add_note(collection_id, text, ts)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn append_topic_note(&self, topic_id: &str, text: &str, ts: UtcMillis) -> Result<(), String> {
        self.topics
            .add_note(topic_id, text, ts)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The §2.5 step-2 hook: flush pending sidecars (and the closing session's
/// journal) when a session closes — rotation and shutdown alike.
struct EngineFlush<'a> {
    app: &'a App,
}

impl photoproof_core::capture::SidecarFlush for EngineFlush<'_> {
    fn flush_for_close(&mut self, closing: &SessionId) {
        let now = UtcMillis::now();
        if let Err(e) = self.app.engine.flush_all(now) {
            tracing::error!(error = %e, "sidecar flush at session close failed");
        }
        if let Err(e) = self.app.engine.flush_session(closing) {
            tracing::error!(error = %e, "session journal flush at session close failed");
        }
    }
}

/// Read-only sibling connection over the shared WAL database (the debug
/// panel's raw tail reads bypass core on purpose: they render raw rows).
#[cfg(any(feature = "debug-panel", debug_assertions))]
fn open_read_only(db_path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(READQ_BUSY_TIMEOUT)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_connectors::vector_store::VecKind;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[test]
    fn shutdown_checkpoint_begins_only_after_tasks_and_commands_acknowledge() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EventStore::open(dir.path().join("barrier.db")).unwrap());
        let tasks = Arc::new(ManagedTaskRegistry::default());
        let commands = Arc::new(CommandWorkRegistry::default());
        let order = Arc::new(AtomicUsize::new(0));
        let (task_started_tx, task_started_rx) = mpsc::channel();
        let (release_task_tx, release_task_rx) = mpsc::channel();
        let (task_ack_tx, task_ack_rx) = mpsc::channel();
        let task_order = Arc::clone(&order);
        tasks
            .spawn("test", "writer", TaskPriority::Background, move |_| {
                task_started_tx.send(()).unwrap();
                release_task_rx.recv().unwrap();
                assert_eq!(
                    task_order.compare_exchange(
                        0,
                        1,
                        AtomicOrdering::SeqCst,
                        AtomicOrdering::SeqCst,
                    ),
                    Ok(0)
                );
                task_ack_tx.send(()).unwrap();
                Ok(())
            })
            .unwrap();
        task_started_rx.recv().unwrap();
        let command_permit = commands
            .admit("test.reader", crate::command_work::CommandClass::Read)
            .unwrap();

        let barrier_tasks = Arc::clone(&tasks);
        let barrier_commands = Arc::clone(&commands);
        let checkpoint_store = Arc::clone(&store);
        let checkpoint_order = Arc::clone(&order);
        let checkpoint = std::thread::spawn(move || {
            let gate = await_finalization_gate(
                &barrier_tasks,
                &barrier_commands,
                std::time::Duration::from_secs(1),
            )
            .expect("both owners acknowledge");
            gate.checkpoint_observed(&checkpoint_store, || {
                assert_eq!(
                    checkpoint_order.compare_exchange(
                        2,
                        3,
                        AtomicOrdering::SeqCst,
                        AtomicOrdering::SeqCst,
                    ),
                    Ok(2),
                    "checkpoint cannot begin before task and command acknowledgement"
                );
            })
            .unwrap();
        });

        assert_eq!(order.load(AtomicOrdering::SeqCst), 0);
        release_task_tx.send(()).unwrap();
        task_ack_rx.recv().unwrap();
        assert_eq!(
            order.load(AtomicOrdering::SeqCst),
            1,
            "the admitted command still holds the finalization gate closed"
        );
        order.store(2, AtomicOrdering::SeqCst);
        drop(command_permit);
        checkpoint.join().unwrap();
        assert_eq!(order.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(tasks.managed_count(), 0);
        assert!(commands.snapshots().is_empty());
    }

    #[test]
    fn shutdown_barrier_timeout_skips_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("timeout.db")).unwrap();
        let tasks = Arc::new(ManagedTaskRegistry::default());
        let commands = Arc::new(CommandWorkRegistry::default());
        let (task_started_tx, task_started_rx) = mpsc::channel();
        let (release_task_tx, release_task_rx) = mpsc::channel();
        tasks
            .spawn(
                "test",
                "wedged-writer",
                TaskPriority::Background,
                move |_| {
                    task_started_tx.send(()).unwrap();
                    release_task_rx.recv().unwrap();
                    Ok(())
                },
            )
            .unwrap();
        task_started_rx.recv().unwrap();
        let command_permit = commands
            .admit(
                "test.wedged-reader",
                crate::command_work::CommandClass::Read,
            )
            .unwrap();

        let barrier =
            await_finalization_gate(&tasks, &commands, std::time::Duration::from_millis(1));
        let checkpoint_started = AtomicBool::new(false);
        if let Ok(gate) = barrier {
            gate.checkpoint_observed(&store, || {
                checkpoint_started.store(true, Ordering::SeqCst);
            })
            .unwrap();
        }
        assert!(
            !checkpoint_started.load(Ordering::SeqCst),
            "a timeout must skip final WAL checkpoint entirely"
        );
        let failure = match await_finalization_gate(&tasks, &commands, std::time::Duration::ZERO) {
            Ok(_) => panic!("wedged work cannot form a finalization gate"),
            Err(failure) => failure,
        };
        assert!(!failure.managed.acknowledged);
        assert!(!failure.commands.acknowledged);
        assert_eq!(
            failure.managed.remaining,
            vec![("test".into(), "wedged-writer".into())]
        );
        assert_eq!(failure.commands.remaining, vec!["test.wedged-reader"]);

        release_task_tx.send(()).unwrap();
        drop(command_permit);
        assert!(
            tasks
                .shutdown(std::time::Duration::from_secs(1))
                .acknowledged
        );
        assert!(
            commands
                .shutdown(std::time::Duration::from_secs(1))
                .acknowledged
        );
    }

    fn vector_target(model: &str) -> ActiveVectorTarget {
        ActiveVectorTarget {
            models: [(VecKind::ImageClip, model.to_owned())]
                .into_iter()
                .collect(),
            ready_generations: [(VecKind::ImageClip, 1)].into_iter().collect(),
        }
    }

    #[test]
    fn vector_repair_coalesces_same_ready_model_and_serializes_a_swap() {
        let mut repair = VectorRepairState::default();
        let a = vector_target("clip-a");
        let b = vector_target("clip-b");
        assert!(repair.enqueue(a.clone()), "first target starts the worker");
        assert_eq!(repair.take_pending(), Some(a.clone()));
        assert!(!repair.enqueue(a.clone()), "same in-flight target dedupes");
        assert!(
            !repair.enqueue(b.clone()),
            "a swap queues behind the running repair"
        );
        repair.completed(a);
        assert_eq!(repair.take_pending(), Some(b.clone()));
        repair.completed(b.clone());
        assert_eq!(repair.take_pending(), None);
        repair.stopped();
        assert!(
            !repair.enqueue(b),
            "the completed ready generation stays exactly-once"
        );
    }

    #[test]
    fn vector_repair_exit_race_can_restart_a_pending_target() {
        let mut repair = VectorRepairState {
            running: true,
            ..VectorRepairState::default()
        };
        let target = vector_target("clip-ready");
        assert!(!repair.enqueue(target.clone()));
        repair.stopped();
        assert!(
            repair.enqueue(target),
            "a request queued during worker exit starts a replacement"
        );
    }

    #[test]
    fn startup_reconcile_completion_dedupes_the_first_ready_observation() {
        let mut repair = VectorRepairState::default();
        let target = vector_target("clip-ready-at-launch");
        repair.defer(target.clone());
        repair.external_completed(target.clone());
        assert_eq!(repair.pending, None);
        assert!(
            !repair.enqueue(target),
            "the runtime pump must not repeat startup's completed generation"
        );
    }

    #[test]
    fn same_model_new_ready_generation_runs_a_fresh_repair() {
        let mut repair = VectorRepairState::default();
        let first = vector_target("clip-same");
        assert!(repair.enqueue(first.clone()));
        assert_eq!(repair.take_pending(), Some(first.clone()));
        repair.completed(first);
        repair.stopped();

        let mut reloaded = vector_target("clip-same");
        reloaded.ready_generations.insert(VecKind::ImageClip, 2);
        assert!(
            repair.enqueue(reloaded),
            "same model id with a new Ready generation is new work"
        );
    }

    #[test]
    fn supervisor_ticker_starts_only_after_usable_and_is_joined_on_quit() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::init(dir.path().to_path_buf()).unwrap();
        assert_eq!(app.lifecycle.snapshot().phase, LifecyclePhase::Usable);
        assert!(
            !app.runtime.supervisors.tick_thread_started(),
            "state construction reaches Usable without starting the ticker"
        );

        app.start_supervisor_runtime().unwrap();
        assert!(app.runtime.supervisors.tick_thread_started());
        app.shutdown();
        assert!(
            !app.runtime.supervisors.tick_thread_started(),
            "bounded shutdown acknowledges and joins the ticker"
        );
    }

    #[test]
    fn blocked_startup_volume_probe_does_not_hold_the_usable_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let app = Arc::new(App::init(dir.path().to_path_buf()).unwrap());
        assert_eq!(app.lifecycle.snapshot().phase, LifecyclePhase::Usable);

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        app.spawn_startup_watchers_task(move |_app, _task| {
            entered_tx.send(()).unwrap();
            // Deterministic stand-in for a native mount call that has entered
            // the OS and is not returning. The scheduling contract under test
            // is that this happens only after App is managed/Usable.
            release_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("injected mount probe entered");

        assert_eq!(
            app.lifecycle.snapshot().phase,
            LifecyclePhase::Usable,
            "a blocked post-usable volume probe cannot move the usable barrier backwards"
        );
        assert!(
            app.tasks.is_running("library", "startup-watchers"),
            "the blocked probe remains observable as owned work"
        );

        release_tx.send(()).unwrap();
        assert!(app.tasks.wait_for_idle(std::time::Duration::from_secs(1)));
        app.shutdown();
    }

    #[test]
    fn init_recovers_open_sessions_then_opens_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a previous run that died with a session open.
        let db = dir.path().join("photoproof.db");
        let dead_sid = {
            let store = EventStore::open(&db).unwrap();
            store
                .open_session(SessionContext {
                    app_version: "0.0.1".into(),
                    device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
                    root_context: None,
                })
                .unwrap()
        };
        let app = Arc::new(App::init(dir.path().to_path_buf()).unwrap());
        app.start_plan_convergence().unwrap();
        app.start_startup_doctor().unwrap();
        // The dead session is closed…
        let rec = app.store.session(&dead_sid).unwrap().unwrap();
        assert!(rec.ended_ts.is_some(), "recovered session must be closed");
        // …and the launch session is open and distinct.
        let live = app.session_id();
        assert_ne!(live, dead_sid);
        assert!(
            app.store
                .session(&live)
                .unwrap()
                .unwrap()
                .ended_ts
                .is_none()
        );
        app.shutdown();
        assert_eq!(app.lifecycle.snapshot().phase, LifecyclePhase::Stopping);
        assert_eq!(app.tasks.active_count(), 0);
        assert_eq!(
            app.tasks.managed_count(),
            0,
            "all managed OS threads are joined before the data flush"
        );
        let closed = app.store.session(&live).unwrap().unwrap();
        assert!(closed.ended_ts.is_some(), "shutdown closes the session");
    }

    #[test]
    fn completed_integrity_repair_can_be_retried_from_ready() {
        let dir = tempfile::tempdir().unwrap();
        let app = Arc::new(App::init(dir.path().to_path_buf()).unwrap());
        app.lifecycle.transition(LifecyclePhase::Ready).unwrap();

        app.start_startup_doctor().expect("repair retry admitted");
        assert!(app.tasks.wait_for_idle(std::time::Duration::from_secs(2)));
        assert_eq!(app.lifecycle.snapshot().phase, LifecyclePhase::Ready);
        let repair = app
            .repair_integrity
            .lock()
            .expect("repair integrity mutex")
            .clone();
        assert_eq!(repair.state, "completed");
        assert!(repair.started_at_ms.is_some());
        assert!(repair.completed_at_ms.is_some());
        app.shutdown();
    }

    #[test]
    fn init_retains_control_file_recovery_truth() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::init(dir.path().to_path_buf()).unwrap();

        assert!(matches!(
            &app.settings_recovery,
            Ok(recovery) if recovery.source == ControlFileSource::MissingDefault
        ));
        assert_eq!(
            app.device_identity_recovery.source,
            ControlFileSource::Created
        );
        assert!(matches!(
            &app.tuning_recovery,
            Ok(loaded)
                if loaded.recovery.source
                    == photoproof_core::runtime::ControlFileSource::Missing
        ));
        assert_eq!(
            app.lifecycle.snapshot().phase,
            LifecyclePhase::Usable,
            "native capture/model construction is not part of the usable barrier"
        );
        assert_eq!(
            app.runtime.status().capability_state,
            "provisional",
            "the usable barrier never waits for the live hardware probe"
        );
        assert!(
            app.capture.lock().expect("capture mutex").is_none(),
            "capture starts dark until its managed initializer runs"
        );
        assert_eq!(
            app.lifecycle.snapshot().health[&Subsystem::Settings],
            SubsystemHealth::Healthy
        );
        app.shutdown();
    }

    #[test]
    fn model_registry_payload_recovery_starts_only_after_usable_as_managed_work() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = photoproof_core::runtime::compiled_manifest();
        let candidate = manifest.models.first().expect("compiled model");
        std::fs::create_dir_all(dir.path().join("models").join(&candidate.id)).unwrap();

        let app = Arc::new(App::init(dir.path().to_path_buf()).unwrap());
        assert_eq!(app.lifecycle.snapshot().phase, LifecyclePhase::Usable);
        assert!(
            app.runtime.model_registry_recovery_pending(),
            "an unindexed model directory stays dark rather than hashing during App::init"
        );
        assert!(!dir.path().join("models/installed.json").exists());

        app.start_model_registry_recovery().unwrap();
        assert!(
            app.tasks.wait_for_idle(std::time::Duration::from_secs(2)),
            "managed recovery reaches a terminal state"
        );
        assert!(!app.runtime.model_registry_recovery_pending());
        assert!(app.tasks.snapshots().iter().any(|task| {
            task.owner == "runtime"
                && task.key == "model-registry-recovery"
                && task.state == crate::managed_tasks::TaskState::Completed
        }));
        app.shutdown();
    }

    #[test]
    fn corrupt_settings_without_lkg_are_visible_not_silent_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), b"{\"stackDisplay\":").unwrap();
        let app = App::init(dir.path().to_path_buf()).unwrap();

        assert!(app.settings_recovery.is_err());
        assert!(matches!(
            app.lifecycle.snapshot().health[&Subsystem::Settings],
            SubsystemHealth::Unavailable { .. }
        ));
        assert_eq!(
            app.settings.lock().expect("settings mutex").stack_display,
            crate::settings::StackDisplay::Jpeg,
            "the fallback remains usable but health labels it unavailable"
        );
        app.shutdown();
    }

    #[test]
    fn valid_live_settings_apply_updates_governor_preview_policy_and_revision() {
        let dir = tempfile::tempdir().unwrap();
        let shell = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let app = Arc::new(App::init(dir.path().join("appdata")).unwrap());
        let candidate = AppSettings {
            stack_display: crate::settings::StackDisplay::Raw,
            preview_cache_budget_bytes: 123_456,
            processing_intensity: crate::settings::ProcessingIntensity::Eco,
            processing_paused: true,
            ..AppSettings::default()
        };
        settings::save(&app.app_data, &candidate).unwrap();

        app.apply_live_control(LiveControlFile::Settings, shell.handle())
            .unwrap();

        assert_eq!(*app.settings.lock().expect("settings mutex"), candidate);
        let resources = app.resources.snapshot();
        assert_eq!(
            resources.intensity,
            crate::settings::ProcessingIntensity::Eco
        );
        assert!(resources.paused);
        let (_, revisions) = app.convergence.snapshot();
        assert_eq!(revisions.settings, 1);
        assert_eq!(revisions.preview_cache, 1);
        let status = app.live_controls.lock().unwrap().snapshot();
        let settings = status
            .iter()
            .find(|status| status.name == "settings")
            .unwrap();
        assert!(settings.last_applied_at_ms.is_some());
        assert_eq!(settings.retained_error, None);
        app.shutdown();
    }

    #[test]
    fn invalid_live_settings_restore_lkg_without_mutating_or_republishing() {
        let dir = tempfile::tempdir().unwrap();
        let shell = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let app = Arc::new(App::init(dir.path().join("appdata")).unwrap());
        let committed = AppSettings {
            stack_display: crate::settings::StackDisplay::Raw,
            ..AppSettings::default()
        };
        settings::save(&app.app_data, &committed).unwrap();
        app.apply_live_control(LiveControlFile::Settings, shell.handle())
            .unwrap();
        let revision = app.convergence.snapshot().0;
        std::fs::write(settings::settings_path(&app.app_data), b"{").unwrap();

        app.apply_live_control(LiveControlFile::Settings, shell.handle())
            .unwrap();

        assert_eq!(*app.settings.lock().expect("settings mutex"), committed);
        assert_eq!(
            app.convergence.snapshot().0,
            revision,
            "effective last-known-good was already live, so recovery emits no duplicate revision"
        );
        let status = app.live_controls.lock().unwrap().snapshot();
        let settings = status
            .iter()
            .find(|status| status.name == "settings")
            .unwrap();
        assert_eq!(settings.recovery_source.as_deref(), Some("last-known-good"));
        assert!(settings.last_recovered_at_ms.is_some());
        assert_eq!(settings.quarantined.len(), 1);
        app.shutdown();
    }

    #[test]
    fn live_control_watcher_is_single_flight_and_acknowledges_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let shell = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let app = Arc::new(App::init(dir.path().join("appdata")).unwrap());

        app.start_live_control_watcher(shell.handle().clone())
            .unwrap();
        assert!(matches!(
            app.start_live_control_watcher(shell.handle().clone()),
            Err(SpawnTaskError::AlreadyRunning { owner, key })
                if owner == "controls" && key == "live-reload"
        ));
        assert!(app.tasks.is_running("controls", "live-reload"));
        let report = app.tasks.shutdown(std::time::Duration::from_secs(1));
        assert!(report.acknowledged);
        assert_eq!(app.tasks.managed_count(), 0);
        assert!(!app.tasks.is_running("controls", "live-reload"));
        app.shutdown();
    }
}
