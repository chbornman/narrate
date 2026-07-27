//! P6.4: the RuntimePlan finally maps onto REAL supervisors — the seam
//! P6.2 deliberately left (`runtime.rs` "P6.3 maps plans onto
//! supervisors"). Two children:
//!
//! - **P1 `llama-server`** (HTTP `/health` probe): binary discovered on
//!   PATH for dev (`brew install llama.cpp`), beside the app binary for
//!   bundles; argv from `runtime::launch::llama_server_args` — the
//!   spike-pinned flags ride along by construction.
//! - **P2 `pp-asr-server`** (WS probe): our owned wrapper child (B67),
//!   built as a sibling binary of the app executable in BOTH dev
//!   (target/{profile}/) and bundle layouts.
//!
//! The supervisors own their EndpointCells: Ready sets the live port,
//! stop/fail clears it — the sherpa WS transcriber and the OpenAI client
//! read those cells and never learn about restarts. One tick thread
//! drives both machines at the §8.1 cadence and re-applies the plan when
//! the runtime generation bumps (config/tier/consent changes).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};
use photoproof_core::capture::SystemClock;
use photoproof_core::runtime::launch;
use photoproof_core::runtime::manifest::{Manifest, ModelEntry};
use photoproof_core::runtime::plan::{ProcessPlan, RuntimePlan};
use photoproof_core::runtime::process::{
    HttpHealthProbe, OsPortSource, OsSpawner, SpawnSpec, WsHealthProbe,
};
use photoproof_core::runtime::supervisor::{ProcState, Supervisor, SupervisorConfig, WeightsGate};
use photoproof_core::runtime::{ChildRegistry, InstanceLock, ProcessId, RuntimeBus};

const TICK: std::time::Duration = std::time::Duration::from_millis(250);
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Tick while driving the stop phases to completion at shutdown: quit
/// latency is user-visible, so the stop state machine runs 5x faster than
/// the steady-state TICK — still bounded by the config grace periods.
const SHUTDOWN_TICK: std::time::Duration = std::time::Duration::from_millis(50);
/// The normal-order stop machine has two five-second grace periods. Leave
/// another two seconds for scheduler/probe/reap handoff before quit reports a
/// missed acknowledgement instead of waiting without a bound.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(12);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SupervisorThreadStartError {
    #[error("supervisor tick thread is already started")]
    AlreadyStarted,
    #[error("supervisor tick thread cannot start before the application is usable")]
    BeforeUsable,
    #[error("supervisor host is stopping")]
    Stopping,
    #[error("failed to spawn supervisor tick thread: {0}")]
    Spawn(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorShutdownReport {
    pub acknowledged: bool,
    pub thread_started: bool,
    pub panicked: bool,
}

struct TickCompletion {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Default for TickCompletion {
    fn default() -> Self {
        Self {
            done: Mutex::new(false),
            changed: Condvar::new(),
        }
    }
}

struct CompletionGuard(Arc<TickCompletion>);

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        *self.0.done.lock().expect("supervisor completion mutex") = true;
        self.0.changed.notify_all();
    }
}

/// The exact process incarnation a role should run. The model id alone is
/// insufficient: changing context size, parallelism, ASR chunking, or the
/// resolved binary must also replace the child. `SpawnSpec` is the complete
/// launch contract, so equality deliberately covers its program and argv.
#[derive(Clone)]
struct SupervisorTarget {
    model_id: String,
    spec: SpawnSpec,
}

impl PartialEq for SupervisorTarget {
    fn eq(&self, other: &Self) -> bool {
        self.model_id == other.model_id
            && self.spec.program == other.spec.program
            && self.spec.args == other.spec.args
    }
}

impl Eq for SupervisorTarget {}

struct ManagedSupervisor {
    target: SupervisorTarget,
    machine: Supervisor<SystemClock>,
}

/// One role's desired/current state. Keeping `desired` even while an old
/// current child drains is what makes Run(A) -> Run(B) a real convergence
/// operation rather than an ignored request.
#[derive(Default)]
struct SupervisorRole {
    desired: Option<SupervisorTarget>,
    current: Option<ManagedSupervisor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorRoleSnapshot {
    pub desired_model_id: Option<String>,
    pub active_model_id: Option<String>,
    pub state: String,
    pub retryable: bool,
}

pub struct SupervisorHost {
    asr: Arc<Mutex<SupervisorRole>>,
    llm: Arc<Mutex<SupervisorRole>>,
    pub asr_endpoint: EndpointCell,
    pub llm_endpoint: EndpointCell,
    pub llm_gauge: InFlightGauge,
    pub llm_lost: LostReports,
    bus: RuntimeBus,
    registry: Option<Arc<ChildRegistry>>,
    /// §8.5: supervisors refuse to spawn without the instance lock.
    lock: Option<Arc<InstanceLock>>,
    stop: Arc<AtomicBool>,
    /// The ticker is process-owned. Keeping the handle here prevents quit from
    /// detaching the thread while it still owns/reaps child processes.
    tick_thread: Mutex<Option<JoinHandle<()>>>,
    tick_wake: Arc<(Mutex<()>, Condvar)>,
    tick_completion: Arc<TickCompletion>,
    /// The silent-dark incident (founder machine, June 2026): a target
    /// prune ate `pp-asr-server`, the plan still said Run, and `apply`'s
    /// `_` arm did nothing visible — `asr_ready()` read false forever
    /// with NOTHING anywhere saying why. These carry the human reason
    /// whenever a plan says Run but binary resolution returns None, so
    /// status() and the debug panel can name the failure instead of
    /// going dark. None = not blocked (which includes the normal
    /// "spawning, not ready yet" warm-up). Rewritten on EVERY apply()
    /// — the same converge cadence that drives the supervisors — so the
    /// reason clears itself the moment the binary reappears.
    asr_blocked: Mutex<Option<String>>,
    llm_blocked: Mutex<Option<String>>,
}

fn llama_sibling(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let sibling = dir.join(name);
    sibling.exists().then_some(sibling)
}

fn llama_from_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    std::env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.exists())
}

fn resolve_llama_binary(
    exe: &Path,
    path: Option<&std::ffi::OsStr>,
    allow_dev_path: bool,
) -> Option<PathBuf> {
    llama_sibling(exe).or_else(|| {
        allow_dev_path
            .then(|| path.and_then(llama_from_path))
            .flatten()
    })
}

/// `llama-server` is explicitly excluded from installed packages today.
/// Release builds therefore resolve app-sibling only and can never consume an
/// arbitrary system binary. Debug/dev builds may use PATH for founder spikes.
fn llama_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = std::env::var_os("PATH");
    resolve_llama_binary(&exe, path.as_deref(), cfg!(debug_assertions))
}

/// `pp-asr-server` is OUR binary: always built/bundled beside the app
/// executable (cargo puts workspace bins in one target dir; the bundler
/// ships it as a resource sibling).
pub(crate) fn asr_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Tauri strips the target triple from externalBin but preserves the
    // platform executable suffix. Without `.exe` here every correctly bundled
    // Windows child was present yet permanently reported missing.
    let sibling = dir.join(format!("pp-asr-server{}", std::env::consts::EXE_SUFFIX));
    sibling.exists().then_some(sibling)
}

fn model_dir(models_dir: &Path, id: &str) -> PathBuf {
    models_dir.join(id)
}

/// `--spec-draft-n-max`: the Unsloth MTP card default and the ceiling of
/// the 1-4 range mainline tests (docs/PLAN-GEMMA-MTP.md §3). Good for the
/// high-acceptance CUDA runs this path is gated to.
const MTP_DRAFT_N_MAX: u32 = 4;

/// The Metal gate (docs/PLAN-GEMMA-MTP.md §4). MTP is LOSSLESS but it is a
/// CUDA/Vulkan win and an Apple-Silicon (Metal) LOSS — 11-28% SLOWER, the
/// draft-eval overhead exceeds the speculative gain (ggml-org/llama.cpp
/// #23752, closed, no fix). So:
///
/// - **macOS / Apple Silicon (Metal)** -> `None`. Even if the config names
///   an MTP model id, the supervisor strips the drafter and runs the plain
///   target — no failure, no slowdown. This is the WHOLE reason the gate
///   lives here in the shell and not in the pure argv builder.
/// - **non-Apple (CUDA / Vulkan)** AND the chosen model entry ships an
///   `mtp-` drafter file beside the target -> `Some(MtpDraft{..})`, so the
///   `--spec-type draft-mtp` flags ride along.
///
/// A model with no `mtp-` file (the plain E2B default, E4B, ...) yields
/// `None` on every platform, so the legacy argv is byte-identical there too.
fn mtp_draft_for(entry: &ModelEntry, dir: &Path) -> Option<launch::MtpDraft> {
    // The Apple-Silicon gate: strip MTP regardless of the model id named.
    // `cfg!` (not `#[cfg]`) keeps this a plain runtime const the compiler
    // folds away per target — non-macOS builds never even check the file.
    if cfg!(target_os = "macos") {
        return None;
    }
    // Non-Apple: activate MTP only when the entry actually ships the tiny
    // `mtp-*.gguf` drafter (the manifest pins it beside the Q4_K_XL target
    // for the *-mtp entries). Resolve against the FULL path, same layout
    // rule as the target/mmproj below.
    entry
        .files
        .iter()
        .find(|f| f.file_name().starts_with("mtp-") && f.path.ends_with(".gguf"))
        .map(|f| launch::MtpDraft {
            draft_model: dir.join(&f.path),
            n_max: MTP_DRAFT_N_MAX,
        })
}

/// The P1 spec from a manifest entry: model = the entry's first .gguf
/// that is not an mmproj or an `mtp-` drafter; projector rides when present;
/// the MTP drafter rides when the entry ships one AND the platform is
/// non-Apple (the Metal gate, [`mtp_draft_for`]).
fn llama_spec(
    binary: &Path,
    entry: &ModelEntry,
    models_dir: &Path,
    ctx_size: u32,
    parallel_slots: u32,
) -> SpawnSpec {
    let dir = model_dir(models_dir, &entry.id);
    // Resolve against the FULL relative path (download.rs preserves layout
    // under models_dir/<id>/<file.path>). The gguf entries are flat today
    // (path == basename), so this is identical on disk — but joining `path`
    // keeps the launcher correct if a future entry ever ships nested. The
    // mmproj predicate still keys off the basename, which is what names the
    // projector regardless of any directory prefix.
    //
    // The target is the gguf that is NEITHER the mmproj NOR the `mtp-`
    // drafter — the *-mtp entries ship both beside the target, so without
    // the `mtp-` exclusion the drafter could be mistaken for the target.
    let model = entry
        .files
        .iter()
        .find(|f| {
            f.path.ends_with(".gguf")
                && !f.file_name().starts_with("mmproj")
                && !f.file_name().starts_with("mtp-")
        })
        .map(|f| dir.join(&f.path))
        .unwrap_or_else(|| dir.join("model.gguf"));
    let mmproj = entry
        .files
        .iter()
        .find(|f| f.file_name().starts_with("mmproj"))
        .map(|f| dir.join(&f.path));
    // The Metal gate resolves the Option<MtpDraft> (None on Apple Silicon,
    // and None for any model that ships no `mtp-` drafter — the legacy path).
    let mtp = mtp_draft_for(entry, &dir);
    SpawnSpec {
        program: binary.to_path_buf(),
        args: launch::llama_server_args_mtp(
            &model,
            mmproj.as_deref(),
            ctx_size,
            parallel_slots,
            None,
            mtp.as_ref(),
        ),
        log: None,
    }
}

fn asr_spec(binary: &Path, entry: &ModelEntry, models_dir: &Path, chunk_ms: u32) -> SpawnSpec {
    SpawnSpec {
        program: binary.to_path_buf(),
        args: launch::asr_wrapper_args(
            &model_dir(models_dir, &entry.id),
            chunk_ms,
            launch::ASR_MIN_THREADS,
        ),
        log: None,
    }
}

impl SupervisorHost {
    pub fn new(
        bus: RuntimeBus,
        registry: Option<Arc<ChildRegistry>>,
        lock: Option<Arc<InstanceLock>>,
    ) -> Self {
        Self {
            asr: Arc::new(Mutex::new(SupervisorRole::default())),
            llm: Arc::new(Mutex::new(SupervisorRole::default())),
            asr_endpoint: EndpointCell::new(),
            llm_endpoint: EndpointCell::new(),
            llm_gauge: InFlightGauge::new(),
            llm_lost: LostReports::new(),
            bus,
            registry,
            lock,
            stop: Arc::new(AtomicBool::new(false)),
            tick_thread: Mutex::new(None),
            tick_wake: Arc::new((Mutex::new(()), Condvar::new())),
            tick_completion: Arc::new(TickCompletion::default()),
            asr_blocked: Mutex::new(None),
            llm_blocked: Mutex::new(None),
        }
    }

    /// True once `shutdown()` latched the stop flag — the quit signal as
    /// seen from the runtime's worker threads. `App::shutdown` flips it
    /// exactly once, at quit; the download worker reads it between
    /// auto-retry backoff slices so a quit never waits out a 30 s backoff
    /// (and no new transfer starts during teardown).
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn asr_ready(&self) -> bool {
        self.asr
            .lock()
            .expect("asr supervisor")
            .current
            .as_ref()
            .is_some_and(|s| s.machine.is_ready())
    }

    pub fn llm_ready(&self) -> bool {
        self.llm
            .lock()
            .expect("llm supervisor")
            .current
            .as_ref()
            .is_some_and(|s| s.machine.is_ready())
    }

    /// Plan says Run but `pp-asr-server` could not be resolved: the human
    /// reason, or None when not blocked. Distinguishes "will never become
    /// ready until the binary returns" from the silent warm-up that
    /// `asr_ready() == false` also covers.
    pub fn asr_blocked(&self) -> Option<String> {
        self.asr_blocked.lock().expect("asr blocked").clone()
    }

    /// Same surfacing for `llama-server` — it shares the silent-dark
    /// failure mode (PATH-or-sibling resolution can also come up empty).
    pub fn llm_blocked(&self) -> Option<String> {
        self.llm_blocked.lock().expect("llm blocked").clone()
    }

    pub fn asr_status(&self) -> SupervisorRoleSnapshot {
        Self::role_snapshot(&self.asr)
    }

    pub fn llm_status(&self) -> SupervisorRoleSnapshot {
        Self::role_snapshot(&self.llm)
    }

    fn role_snapshot(role: &Mutex<SupervisorRole>) -> SupervisorRoleSnapshot {
        let role = role.lock().expect("supervisor");
        let state = role
            .current
            .as_ref()
            .map(|current| current.machine.state().name())
            .unwrap_or("notConfigured");
        SupervisorRoleSnapshot {
            desired_model_id: role.desired.as_ref().map(|target| target.model_id.clone()),
            active_model_id: role
                .current
                .as_ref()
                .map(|current| current.target.model_id.clone()),
            state: state.into(),
            retryable: matches!(
                role.current.as_ref().map(|current| current.machine.state()),
                Some(ProcState::Failed | ProcState::DownloadFailed)
            ),
        }
    }

    /// True while a desired or still-draining child references `model_id`.
    /// The model-operation registry uses this after converging the plan dark:
    /// deletion waits until the supervised process has released its file
    /// handles instead of racing a live consumer.
    pub fn model_in_use(&self, model_id: &str) -> bool {
        [&self.asr, &self.llm].into_iter().any(|role| {
            let role = role.lock().expect("supervisor");
            role.desired
                .as_ref()
                .is_some_and(|target| target.model_id == model_id)
                || role
                    .current
                    .as_ref()
                    .is_some_and(|current| current.target.model_id == model_id)
        })
    }

    /// Map the (re)computed plan onto the machines: build a supervisor
    /// the first time a process plans Run, retire it when the plan goes
    /// dark. Spec changes (model swap) rebuild — supervisors are cheap;
    /// children are not, and stop() walks the §8.4 normal order.
    pub fn apply(
        &self,
        plan: &RuntimePlan,
        manifest: &Manifest,
        models_dir: &Path,
        ctx_size: u32,
        parallel_slots: u32,
        chunk_ms: u32,
    ) {
        // The plan-converge loop can race quit teardown. Once shutdown is
        // latched, no later plan is allowed to construct or rearm a child.
        if self.stopping() {
            return;
        }
        self.apply_with_binaries(
            plan,
            manifest,
            models_dir,
            ctx_size,
            parallel_slots,
            chunk_ms,
            asr_binary(),
            llama_binary(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_with_binaries(
        &self,
        plan: &RuntimePlan,
        manifest: &Manifest,
        models_dir: &Path,
        ctx_size: u32,
        parallel_slots: u32,
        chunk_ms: u32,
        asr_binary: Option<PathBuf>,
        llama_binary: Option<PathBuf>,
    ) {
        // ---- P2 (asr) -------------------------------------------------------
        {
            let mut role = self.asr.lock().expect("asr supervisor");
            let desired = match (&plan.asr, asr_binary) {
                (ProcessPlan::Run { model_id }, Some(binary)) => {
                    *self.asr_blocked.lock().expect("asr blocked") = None;
                    manifest.model(model_id).map(|entry| SupervisorTarget {
                        model_id: model_id.clone(),
                        spec: asr_spec(&binary, entry, models_dir, chunk_ms),
                    })
                }
                (ProcessPlan::Run { model_id }, None) => {
                    *self.asr_blocked.lock().expect("asr blocked") = Some(
                        "pp-asr-server binary is missing beside the app executable \
                         (dev: `cargo build -p pp-asr-server` restores it)"
                            .into(),
                    );
                    tracing::warn!(model = %model_id, "ASR plan blocked: pp-asr-server missing");
                    None
                }
                _ => {
                    *self.asr_blocked.lock().expect("asr blocked") = None;
                    None
                }
            };
            converge_role(&mut role, desired, |target| {
                let mut sup = Supervisor::new(
                    SupervisorConfig::new(ProcessId::Asr),
                    SystemClock::new(),
                    self.bus.clone(),
                    Box::new(OsSpawner {
                        spec: target.spec.clone(),
                    }),
                    Box::new(WsHealthProbe::new(PROBE_TIMEOUT)),
                    Box::new(OsPortSource),
                    self.lock.clone(),
                    self.registry.clone(),
                    InFlightGauge::new(),
                    LostReports::new(),
                    self.asr_endpoint.clone(),
                );
                sup.set_gate(WeightsGate::Installed);
                sup.set_desired_run(true);
                sup
            });
        }
        // ---- P1 (llm) -------------------------------------------------------
        {
            let mut role = self.llm.lock().expect("llm supervisor");
            let desired = match (&plan.llm, llama_binary) {
                (ProcessPlan::Run { model_id }, Some(binary)) => {
                    *self.llm_blocked.lock().expect("llm blocked") = None;
                    manifest.model(model_id).map(|entry| SupervisorTarget {
                        model_id: model_id.clone(),
                        spec: llama_spec(&binary, entry, models_dir, ctx_size, parallel_slots),
                    })
                }
                (ProcessPlan::Run { model_id }, None) => {
                    *self.llm_blocked.lock().expect("llm blocked") = Some(
                        "llama-server binary not found beside the app executable \
                         or on PATH (dev: `brew install llama.cpp`)"
                            .into(),
                    );
                    tracing::warn!(model = %model_id, "LLM plan blocked: llama-server missing");
                    None
                }
                _ => {
                    *self.llm_blocked.lock().expect("llm blocked") = None;
                    None
                }
            };
            converge_role(&mut role, desired, |target| {
                let mut sup = Supervisor::new(
                    SupervisorConfig::new(ProcessId::Llm),
                    SystemClock::new(),
                    self.bus.clone(),
                    Box::new(OsSpawner {
                        spec: target.spec.clone(),
                    }),
                    Box::new(HttpHealthProbe::new(PROBE_TIMEOUT)),
                    Box::new(OsPortSource),
                    self.lock.clone(),
                    self.registry.clone(),
                    self.llm_gauge.clone(),
                    self.llm_lost.clone(),
                    self.llm_endpoint.clone(),
                );
                sup.set_gate(WeightsGate::Installed);
                sup.set_desired_run(true);
                sup
            });
        }
    }

    /// Start the owned §8.1 drive thread. The shell calls this only after the
    /// lifecycle has reached Usable; construction no longer starts a detached
    /// pre-window thread.
    pub fn start_tick_thread(&self) -> Result<(), SupervisorThreadStartError> {
        if self.stopping() {
            return Err(SupervisorThreadStartError::Stopping);
        }
        let mut owned = self.tick_thread.lock().expect("supervisor thread mutex");
        if owned.is_some() {
            return Err(SupervisorThreadStartError::AlreadyStarted);
        }
        *self
            .tick_completion
            .done
            .lock()
            .expect("supervisor completion mutex") = false;
        let asr = Arc::clone(&self.asr);
        let llm = Arc::clone(&self.llm);
        let stop = Arc::clone(&self.stop);
        let wake = Arc::clone(&self.tick_wake);
        let completion = Arc::clone(&self.tick_completion);
        let handle = std::thread::Builder::new()
            .name("pp-supervisors".into())
            .spawn(move || {
                let _completion = CompletionGuard(completion);
                while !stop.load(Ordering::Acquire) {
                    for role in [&asr, &llm] {
                        let mut role = role.lock().expect("supervisor");
                        if let Some(current) = role.current.as_mut() {
                            current.machine.tick();
                        }
                        retire_superseded(&mut role);
                    }
                    let guard = wake.0.lock().expect("supervisor wake mutex");
                    let _ = wake
                        .1
                        .wait_timeout_while(guard, TICK, |_| !stop.load(Ordering::Acquire))
                        .expect("supervisor wake condvar");
                }
                // Shutdown: the §8.4 normal order on both, then drive the
                // stop phases together. Driving both per iteration makes the
                // bound the longest role's grace, not ASR + LLM serially.
                for role in [&asr, &llm] {
                    let mut role = role.lock().expect("supervisor");
                    if let Some(current) = role.current.as_mut() {
                        current.machine.stop();
                    }
                }
                loop {
                    let mut all_stopped = true;
                    for role in [&asr, &llm] {
                        let mut role = role.lock().expect("supervisor");
                        if let Some(current) = role.current.as_mut()
                            && !matches!(
                                current.machine.state(),
                                ProcState::NotConfigured | ProcState::Stopped
                            )
                        {
                            current.machine.tick();
                            all_stopped = false;
                        }
                    }
                    if all_stopped {
                        break;
                    }
                    std::thread::sleep(SHUTDOWN_TICK);
                }
            })
            .map_err(|error| SupervisorThreadStartError::Spawn(error.to_string()))?;
        *owned = Some(handle);
        Ok(())
    }

    /// Latch stop, wake the cadence wait immediately, and join the owned
    /// ticker when it acknowledges within `timeout`. A missed bound leaves the
    /// handle owned for a later retry/Drop instead of silently detaching it.
    pub fn shutdown(&self, timeout: Duration) -> SupervisorShutdownReport {
        self.stop.store(true, Ordering::Release);
        self.tick_wake.1.notify_all();

        let thread_started = self
            .tick_thread
            .lock()
            .expect("supervisor thread mutex")
            .is_some();
        if !thread_started {
            return SupervisorShutdownReport {
                acknowledged: true,
                thread_started: false,
                panicked: false,
            };
        }

        let deadline = Instant::now() + timeout;
        let mut done = self
            .tick_completion
            .done
            .lock()
            .expect("supervisor completion mutex");
        while !*done {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = self
                .tick_completion
                .changed
                .wait_timeout(done, deadline.saturating_duration_since(now))
                .expect("supervisor completion condvar");
            done = next;
        }
        let acknowledged = *done;
        drop(done);

        let panicked = if acknowledged {
            self.tick_thread
                .lock()
                .expect("supervisor thread mutex")
                .take()
                .is_some_and(|handle| handle.join().is_err())
        } else {
            false
        };
        SupervisorShutdownReport {
            acknowledged,
            thread_started,
            panicked,
        }
    }

    #[cfg(test)]
    pub(crate) fn tick_thread_started(&self) -> bool {
        self.tick_thread
            .lock()
            .expect("supervisor thread mutex")
            .is_some()
    }

    /// Settings' explicit retry: only roles still desired by the latest plan
    /// receive a fresh restart budget. A child already draining because the
    /// role went dark or changed model must never be resurrected.
    pub fn restart_runtime(&self) {
        if self.stopping() {
            return;
        }
        for role in [&self.asr, &self.llm] {
            let mut role = role.lock().expect("supervisor");
            let desired = role.desired.clone();
            if let (Some(desired), Some(current)) = (desired, role.current.as_mut())
                && current.target == desired
            {
                current.machine.restart_runtime();
            }
        }
    }
}

/// Apply one desired target to one role. Replacement waits for the old child
/// to finish its normal drain/terminate/reap sequence; a stopped incarnation
/// is discarded and the desired spec is then constructed immediately.
fn converge_role(
    role: &mut SupervisorRole,
    desired: Option<SupervisorTarget>,
    build: impl FnOnce(&SupervisorTarget) -> Supervisor<SystemClock>,
) {
    role.desired = desired;

    if let Some(current) = role.current.as_mut() {
        let still_desired = role.desired.as_ref() == Some(&current.target);
        if still_desired {
            current.machine.set_gate(WeightsGate::Installed);
            current.machine.set_desired_run(true);
        } else {
            current.machine.stop();
        }
    }
    retire_superseded(role);

    if role.current.is_none()
        && let Some(target) = role.desired.clone()
    {
        role.current = Some(ManagedSupervisor {
            machine: build(&target),
            target,
        });
    }
}

/// Once a no-longer-desired machine has completed its stop, remove the slot.
/// The desired target remains in the role so the next converge can construct
/// its replacement.
fn retire_superseded(role: &mut SupervisorRole) {
    let retire = role.current.as_ref().is_some_and(|current| {
        role.desired.as_ref() != Some(&current.target)
            && matches!(
                current.machine.state(),
                ProcState::NotConfigured | ProcState::Stopped
            )
    });
    if retire {
        role.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_core::runtime::compiled_manifest;

    #[test]
    fn installed_llama_resolution_never_falls_through_to_developer_path() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("installed");
        let path_dir = temp.path().join("developer-path");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&path_dir).unwrap();
        let binary_name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let exe = app_dir.join(if cfg!(windows) {
            "photoproof.exe"
        } else {
            "photoproof"
        });
        let path_binary = path_dir.join(binary_name);
        std::fs::write(&path_binary, b"fixture").unwrap();
        let path = std::env::join_paths([&path_dir]).unwrap();

        assert_eq!(
            resolve_llama_binary(&exe, Some(&path), false),
            None,
            "installed/release policy must not consume an arbitrary PATH binary"
        );
        assert_eq!(
            resolve_llama_binary(&exe, Some(&path), true),
            Some(path_binary),
            "debug/dev policy may opt into PATH explicitly"
        );

        let sibling = app_dir.join(binary_name);
        std::fs::write(&sibling, b"bundled").unwrap();
        assert_eq!(
            resolve_llama_binary(&exe, Some(&path), false),
            Some(sibling),
            "a deliberately bundled sibling remains authoritative"
        );
    }

    fn not_configured() -> ProcessPlan {
        ProcessPlan::NotConfigured {
            reason: "test".into(),
            fixable_by_download: false,
        }
    }

    fn run_plan(asr: Option<&str>, llm: Option<&str>) -> RuntimePlan {
        let process = |model: Option<&str>| match model {
            Some(model_id) => ProcessPlan::Run {
                model_id: model_id.into(),
            },
            None => not_configured(),
        };
        RuntimePlan {
            effective_tier: 2,
            llm: process(llm),
            asr: process(asr),
            clip_embedder: not_configured(),
            text_embedder: not_configured(),
        }
    }

    fn apply_resolved(
        host: &SupervisorHost,
        plan: &RuntimePlan,
        asr: Option<&str>,
        llm: Option<&str>,
        chunk_ms: u32,
    ) {
        host.apply_with_binaries(
            plan,
            &compiled_manifest(),
            Path::new("/models"),
            16_384,
            2,
            chunk_ms,
            asr.map(PathBuf::from),
            llm.map(PathBuf::from),
        );
    }

    /// The June 2026 incident, pinned: the plan says Run for P2 but no
    /// `pp-asr-server` sits beside the executable (the test binary lives
    /// in target/{profile}/deps — deterministically empty of it), so apply
    /// must record a VISIBLE reason instead of silently doing nothing —
    /// and clear it again when the plan goes dark. P1 shares the arm
    /// shape but resolves via PATH too, so only P2 is deterministic here.
    #[test]
    fn plan_run_without_binary_records_a_blocked_reason() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let manifest = compiled_manifest();
        let run = RuntimePlan {
            effective_tier: 1,
            llm: not_configured(),
            asr: ProcessPlan::Run {
                model_id: "any".into(),
            },
            clip_embedder: not_configured(),
            text_embedder: not_configured(),
        };
        host.apply(&run, &manifest, Path::new("/nonexistent"), 4096, 1, 560);
        let reason = host.asr_blocked().expect("the missing binary is named");
        assert!(
            reason.contains("pp-asr-server"),
            "the reason names the binary: {reason}"
        );
        assert!(!host.asr_ready(), "blocked is never ready");
        assert!(
            host.llm_blocked().is_none(),
            "llm plan is dark, not blocked"
        );

        // Plan goes dark → the blocked reason clears on the same converge.
        let dark = RuntimePlan {
            effective_tier: 1,
            llm: not_configured(),
            asr: not_configured(),
            clip_embedder: not_configured(),
            text_embedder: not_configured(),
        };
        host.apply(&dark, &manifest, Path::new("/nonexistent"), 4096, 1, 560);
        assert!(host.asr_blocked().is_none());
    }

    #[test]
    fn run_a_to_run_b_replaces_the_supervisor_incarnation() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let a = "nemotron-speech-streaming-en-0.6b-560ms-int8";
        let b = "nemotron-3.5-asr-streaming-0.6b-560ms-int8";

        apply_resolved(&host, &run_plan(Some(a), None), Some("/bin/asr"), None, 560);
        {
            let role = host.asr.lock().unwrap();
            assert_eq!(role.desired.as_ref().unwrap().model_id, a);
            assert_eq!(role.current.as_ref().unwrap().target.model_id, a);
        }

        apply_resolved(&host, &run_plan(Some(b), None), Some("/bin/asr"), None, 560);
        let role = host.asr.lock().unwrap();
        assert_eq!(role.desired.as_ref().unwrap().model_id, b);
        assert_eq!(
            role.current.as_ref().unwrap().target.model_id,
            b,
            "the stopped A slot must be retired and replaced by B"
        );
    }

    #[test]
    fn launch_spec_change_replaces_the_same_model() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let model = "nemotron-speech-streaming-en-0.6b-560ms-int8";
        let plan = run_plan(Some(model), None);
        apply_resolved(&host, &plan, Some("/bin/asr"), None, 560);
        let first_args = host
            .asr
            .lock()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .target
            .spec
            .args
            .clone();

        apply_resolved(&host, &plan, Some("/bin/asr"), None, 800);
        let role = host.asr.lock().unwrap();
        let current = role.current.as_ref().unwrap();
        assert_eq!(current.target.model_id, model);
        assert_ne!(
            current.target.spec.args, first_args,
            "chunk/config changes are part of desired identity"
        );
    }

    #[test]
    fn run_to_dark_retires_the_slot_and_run_recreates_it() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let model = "nemotron-speech-streaming-en-0.6b-560ms-int8";

        apply_resolved(
            &host,
            &run_plan(Some(model), None),
            Some("/bin/asr"),
            None,
            560,
        );
        assert!(host.asr.lock().unwrap().current.is_some());

        apply_resolved(&host, &run_plan(None, None), Some("/bin/asr"), None, 560);
        {
            let role = host.asr.lock().unwrap();
            assert!(role.desired.is_none());
            assert!(
                role.current.is_none(),
                "a fully stopped dark slot must not linger"
            );
        }

        apply_resolved(
            &host,
            &run_plan(Some(model), None),
            Some("/bin/asr"),
            None,
            560,
        );
        assert_eq!(
            host.asr
                .lock()
                .unwrap()
                .current
                .as_ref()
                .unwrap()
                .target
                .model_id,
            model
        );
    }

    #[test]
    fn binary_disappearance_blocks_and_reappearance_recreates() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let model = "nemotron-speech-streaming-en-0.6b-560ms-int8";
        let plan = run_plan(Some(model), None);

        apply_resolved(&host, &plan, Some("/bin/asr"), None, 560);
        assert!(host.asr_blocked().is_none());

        apply_resolved(&host, &plan, None, None, 560);
        assert!(host.asr_blocked().is_some());
        {
            let role = host.asr.lock().unwrap();
            assert!(role.desired.is_none());
            assert!(role.current.is_none());
        }

        apply_resolved(&host, &plan, Some("/bin/asr"), None, 560);
        assert!(host.asr_blocked().is_none());
        assert_eq!(
            host.asr
                .lock()
                .unwrap()
                .current
                .as_ref()
                .unwrap()
                .target
                .model_id,
            model
        );
    }

    #[test]
    fn explicit_restart_restarts_both_desired_supervisors() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let asr = "nemotron-speech-streaming-en-0.6b-560ms-int8";
        let llm = "gemma-4-e2b-it-qat-q4_0";
        let plan = run_plan(Some(asr), Some(llm));
        apply_resolved(
            &host,
            &plan,
            Some("/bin/asr"),
            Some("/bin/llama-server"),
            560,
        );

        host.restart_runtime();

        for role in [&host.asr, &host.llm] {
            let role = role.lock().unwrap();
            let current = role.current.as_ref().unwrap();
            assert!(
                current
                    .machine
                    .decision_log()
                    .any(|line| line.contains("restart-runtime: fresh attempt budget")),
                "settings restart must reach every desired supervisor"
            );
        }
    }

    #[test]
    fn apply_after_shutdown_cannot_construct_a_new_supervisor() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        assert_eq!(
            host.shutdown(Duration::ZERO),
            SupervisorShutdownReport {
                acknowledged: true,
                thread_started: false,
                panicked: false,
            }
        );
        host.apply(
            &run_plan(Some("nemotron-speech-streaming-en-0.6b-560ms-int8"), None),
            &compiled_manifest(),
            Path::new("/models"),
            16_384,
            2,
            560,
        );
        let role = host.asr.lock().unwrap();
        assert!(role.desired.is_none());
        assert!(role.current.is_none());
    }

    #[test]
    fn tick_thread_is_single_flight_and_bounded_shutdown_joins_it() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        assert!(!host.tick_thread_started());
        host.start_tick_thread().expect("start ticker");
        assert!(host.tick_thread_started());
        assert_eq!(
            host.start_tick_thread(),
            Err(SupervisorThreadStartError::AlreadyStarted)
        );

        let report = host.shutdown(Duration::from_secs(1));
        assert_eq!(
            report,
            SupervisorShutdownReport {
                acknowledged: true,
                thread_started: true,
                panicked: false,
            }
        );
        assert!(
            !host.tick_thread_started(),
            "acknowledged ticker is joined and its handle retired"
        );
        assert_eq!(
            host.start_tick_thread(),
            Err(SupervisorThreadStartError::Stopping),
            "the permanent quit latch rejects a late restart"
        );
    }

    #[test]
    fn owned_ticker_drives_configured_roles_to_stopped_before_ack() {
        let host = SupervisorHost::new(RuntimeBus::new(), None, None);
        let model = "nemotron-speech-streaming-en-0.6b-560ms-int8";
        apply_resolved(
            &host,
            &run_plan(Some(model), None),
            Some("/bin/asr"),
            None,
            560,
        );
        assert!(host.asr.lock().unwrap().current.is_some());
        host.start_tick_thread().expect("start ticker");

        let report = host.shutdown(Duration::from_secs(1));
        assert!(report.acknowledged, "ticker acknowledges its child stop");
        let role = host.asr.lock().unwrap();
        assert!(matches!(
            role.current.as_ref().unwrap().machine.state(),
            ProcState::NotConfigured | ProcState::Stopped
        ));
    }

    /// The shipped E2B default ships NO `mtp-` drafter, so its argv must be
    /// EXACTLY the legacy `llama_server_args` (the back-compat wrapper) on
    /// EVERY platform — the Metal gate never even reaches the file check.
    /// This pins the laptop/default path byte-for-byte across the new wiring.
    #[test]
    fn the_plain_e2b_default_argv_is_byte_identical_to_the_legacy_path() {
        let m = compiled_manifest();
        let entry = m.model("gemma-4-e2b-it-qat-q4_0").unwrap();
        let models = Path::new("/models");
        let spec = llama_spec(Path::new("/bin/llama-server"), entry, models, 16384, 2);

        // The legacy builder over the same resolved target + projector.
        let dir = models.join(&entry.id);
        let target = dir.join("gemma-4-E2B_q4_0-it.gguf");
        let mmproj = dir.join("mmproj-gemma-4-E2B-it-Q8_0.gguf");
        let legacy = launch::llama_server_args(&target, Some(mmproj.as_path()), 16384, 2, None);
        assert_eq!(spec.args, legacy, "default LLM argv must not drift");
        assert!(
            !spec.args.join(" ").contains("draft-mtp"),
            "the plain default never carries MTP flags"
        );
    }

    /// The drafter-resolution half of the Metal gate, independent of the
    /// running platform: `mtp_draft_for` finds the `mtp-*.gguf` the manifest
    /// pins beside the MTP target and joins it under the model dir.
    /// (On macOS the gate short-circuits to None BEFORE this — covered by the
    /// platform-conditional test below.)
    #[test]
    fn mtp_draft_for_resolves_the_pinned_drafter_when_offered() {
        let m = compiled_manifest();
        let entry = m.model("gemma-4-e2b-it-qat-q4_k_xl-mtp").unwrap();
        let dir = Path::new("/models").join(&entry.id);
        let draft = mtp_draft_for(entry, &dir);
        if cfg!(target_os = "macos") {
            // Metal LOSS (#23752): the gate strips MTP regardless of the id.
            assert!(draft.is_none(), "Apple Silicon strips MTP");
        } else {
            // CUDA/Vulkan: the drafter rides, pointing at the pinned mtp- file.
            let draft = draft.expect("non-Apple activates MTP for an mtp- entry");
            assert_eq!(
                draft.draft_model,
                dir.join("mtp-gemma-4-E2B-it.gguf"),
                "drafter joined under the model dir"
            );
            assert_eq!(draft.n_max, MTP_DRAFT_N_MAX);
        }
    }

    /// The whole-path Metal gate over `llama_spec` for an MTP entry: on
    /// Apple Silicon the argv is the plain target (no MTP flags); on
    /// CUDA/Vulkan the `--spec-type draft-mtp` flags land, the target is the
    /// Q4_K_XL gguf (NOT the `mtp-` drafter), and the projector still rides.
    #[test]
    fn llama_spec_gates_mtp_flags_by_platform() {
        let m = compiled_manifest();
        let entry = m.model("gemma-4-e2b-it-qat-q4_k_xl-mtp").unwrap();
        let spec = llama_spec(
            Path::new("/bin/llama-server"),
            entry,
            Path::new("/models"),
            16384,
            2,
        );
        let joined = spec.args.join(" ");
        // The target is the real Q4_K_XL, never the drafter — on every
        // platform (the `mtp-` exclusion in the target predicate).
        assert!(
            joined.contains(
                "--model /models/gemma-4-e2b-it-qat-q4_k_xl-mtp/gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf"
            ),
            "{joined}"
        );
        if cfg!(target_os = "macos") {
            assert!(!joined.contains("draft-mtp"), "Metal strips MTP: {joined}");
            assert!(
                !joined.contains("--model-draft"),
                "no drafter on Apple Silicon: {joined}"
            );
        } else {
            assert!(joined.contains("--spec-type draft-mtp"), "{joined}");
            assert!(
                joined.contains(
                    "--model-draft /models/gemma-4-e2b-it-qat-q4_k_xl-mtp/mtp-gemma-4-E2B-it.gguf"
                ),
                "{joined}"
            );
            assert!(joined.contains("--spec-draft-n-max 4"), "{joined}");
        }
        // The projector rides on every platform.
        assert!(joined.contains("--mmproj"), "{joined}");
    }
}
