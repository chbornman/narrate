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
        self.stop.load(Ordering::Relaxed)
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
                    *self.asr_blocked.lock().expect("asr blocked") = None;
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
                (ProcessPlan::Run { .. }, None) => {
                    // The incident arm: the plan wants P2 but there is no
                    // binary to spawn (in dev a target prune removes it —
                    // `tauri dev` only rebuilds the app, not sibling
                    // workspace bins). Used to fall into `_` and vanish;
                    // now the reason is recorded for status()/debug_lines
                    // and any live supervisor (binary deleted mid-run)
                    // still winds down through the §8.4 order.
                    *self.asr_blocked.lock().expect("asr blocked") = Some(
                        "pp-asr-server binary is missing beside the app executable \
                         (dev: `cargo build -p pp-asr-server` restores it)"
                            .into(),
                    );
                    if let Some(sup) = slot.as_mut() {
                        sup.stop();
                    }
                }
                _ => {
                    // Plan went dark (NotConfigured/External): not blocked —
                    // the plan itself names that state in debug_lines.
                    *self.asr_blocked.lock().expect("asr blocked") = None;
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
                    *self.llm_blocked.lock().expect("llm blocked") = None;
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
                (ProcessPlan::Run { .. }, None) => {
                    // Same silent-dark failure mode as P2: weights are
                    // installed (the plan says Run) but no binary resolved
                    // — without a recorded reason the LLM features just
                    // never light up and nothing says why.
                    *self.llm_blocked.lock().expect("llm blocked") = Some(
                        "llama-server binary not found beside the app executable \
                         or on PATH (dev: `brew install llama.cpp`)"
                            .into(),
                    );
                    if let Some(sup) = slot.as_mut() {
                        sup.stop();
                    }
                }
                _ => {
                    *self.llm_blocked.lock().expect("llm blocked") = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_core::runtime::compiled_manifest;

    fn not_configured() -> ProcessPlan {
        ProcessPlan::NotConfigured {
            reason: "test".into(),
            fixable_by_download: false,
        }
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
