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
use std::sync::{Arc, Mutex};

use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};
use photoproof_core::capture::SystemClock;
use photoproof_core::runtime::launch;
use photoproof_core::runtime::manifest::{Manifest, ModelEntry};
use photoproof_core::runtime::plan::{ProcessPlan, RuntimePlan};
use photoproof_core::runtime::process::{
    HttpHealthProbe, OsPortSource, OsSpawner, SpawnSpec, WsHealthProbe,
};
use photoproof_core::runtime::supervisor::{Supervisor, SupervisorConfig, WeightsGate};
use photoproof_core::runtime::{ChildRegistry, InstanceLock, ProcessId, RuntimeBus};

const TICK: std::time::Duration = std::time::Duration::from_millis(250);
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Tick while driving the stop phases to completion at shutdown: quit
/// latency is user-visible, so the stop state machine runs 5x faster than
/// the steady-state TICK — still bounded by the config grace periods.
const SHUTDOWN_TICK: std::time::Duration = std::time::Duration::from_millis(50);

pub struct SupervisorHost {
    asr: Arc<Mutex<Option<Supervisor<SystemClock>>>>,
    llm: Arc<Mutex<Option<Supervisor<SystemClock>>>>,
    pub asr_endpoint: EndpointCell,
    pub llm_endpoint: EndpointCell,
    pub llm_gauge: InFlightGauge,
    pub llm_lost: LostReports,
    bus: RuntimeBus,
    registry: Option<Arc<ChildRegistry>>,
    /// §8.5: supervisors refuse to spawn without the instance lock.
    lock: Option<Arc<InstanceLock>>,
    stop: Arc<AtomicBool>,
}

/// `llama-server`: PATH for dev, app-sibling for bundles. None = the plan
/// can't run P1 on this machine (surfaced as NotConfigured downstream).
fn llama_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("llama-server");
        if sibling.exists() {
            return Some(sibling);
        }
    }
    // Dev path: whatever the founder machine has (brew/apt).
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("llama-server"))
        .find(|c| c.exists())
}

/// `pp-asr-server` is OUR binary: always built/bundled beside the app
/// executable (cargo puts workspace bins in one target dir; the bundler
/// ships it as a resource sibling).
fn asr_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let sibling = dir.join("pp-asr-server");
    sibling.exists().then_some(sibling)
}

fn model_dir(models_dir: &Path, id: &str) -> PathBuf {
    models_dir.join(id)
}

/// The P1 spec from a manifest entry: model = the entry's first .gguf
/// that is not an mmproj; projector rides when present.
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
    let model = entry
        .files
        .iter()
        .find(|f| f.path.ends_with(".gguf") && !f.file_name().starts_with("mmproj"))
        .map(|f| dir.join(&f.path))
        .unwrap_or_else(|| dir.join("model.gguf"));
    let mmproj = entry
        .files
        .iter()
        .find(|f| f.file_name().starts_with("mmproj"))
        .map(|f| dir.join(&f.path));
    SpawnSpec {
        program: binary.to_path_buf(),
        args: launch::llama_server_args(&model, mmproj.as_deref(), ctx_size, parallel_slots, None),
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
            asr: Arc::new(Mutex::new(None)),
            llm: Arc::new(Mutex::new(None)),
            asr_endpoint: EndpointCell::new(),
            llm_endpoint: EndpointCell::new(),
            llm_gauge: InFlightGauge::new(),
            llm_lost: LostReports::new(),
            bus,
            registry,
            lock,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn asr_ready(&self) -> bool {
        self.asr
            .lock()
            .expect("asr supervisor")
            .as_ref()
            .is_some_and(|s| s.is_ready())
    }

    pub fn llm_ready(&self) -> bool {
        self.llm
            .lock()
            .expect("llm supervisor")
            .as_ref()
            .is_some_and(|s| s.is_ready())
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
        // ---- P2 (asr) -------------------------------------------------------
        {
            let mut slot = self.asr.lock().expect("asr supervisor");
            match (&plan.asr, asr_binary()) {
                (ProcessPlan::Run { model_id }, Some(binary)) => {
                    if slot.is_none()
                        && let Some(entry) = manifest.model(model_id)
                    {
                        let spec = asr_spec(&binary, entry, models_dir, chunk_ms);
                        let mut sup = Supervisor::new(
                            SupervisorConfig::new(ProcessId::Asr),
                            SystemClock::new(),
                            self.bus.clone(),
                            Box::new(OsSpawner { spec }),
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
                        *slot = Some(sup);
                    }
                }
                _ => {
                    if let Some(sup) = slot.as_mut() {
                        sup.stop();
                    }
                }
            }
        }
        // ---- P1 (llm) -------------------------------------------------------
        {
            let mut slot = self.llm.lock().expect("llm supervisor");
            match (&plan.llm, llama_binary()) {
                (ProcessPlan::Run { model_id }, Some(binary)) => {
                    if slot.is_none()
                        && let Some(entry) = manifest.model(model_id)
                    {
                        let spec = llama_spec(&binary, entry, models_dir, ctx_size, parallel_slots);
                        let mut sup = Supervisor::new(
                            SupervisorConfig::new(ProcessId::Llm),
                            SystemClock::new(),
                            self.bus.clone(),
                            Box::new(OsSpawner { spec }),
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
                        *slot = Some(sup);
                    }
                }
                _ => {
                    if let Some(sup) = slot.as_mut() {
                        sup.stop();
                    }
                }
            }
        }
    }

    /// The drive thread: §8.1 cadence over both machines until shutdown.
    pub fn spawn_tick_thread(&self) {
        let asr = Arc::clone(&self.asr);
        let llm = Arc::clone(&self.llm);
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name("pp-supervisors".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for slot in [&asr, &llm] {
                        if let Some(sup) = slot.lock().expect("supervisor").as_mut() {
                            sup.tick();
                        }
                    }
                    std::thread::sleep(TICK);
                }
                // Shutdown: the §8.4 normal order on both, then drive the
                // stop phases to completion (bounded by the config graces).
                for slot in [&asr, &llm] {
                    if let Some(sup) = slot.lock().expect("supervisor").as_mut() {
                        sup.stop();
                        while !matches!(
                            sup.state(),
                            photoproof_core::runtime::supervisor::ProcState::NotConfigured
                                | photoproof_core::runtime::supervisor::ProcState::Stopped
                        ) {
                            sup.tick();
                            std::thread::sleep(SHUTDOWN_TICK);
                        }
                    }
                }
            })
            .expect("spawn supervisor thread");
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
