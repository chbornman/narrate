//! The shell's runtime host (spec/RUNTIME.md, packet P6.2): owns the
//! instance lock, the startup orphan sweep, config + tier resolution, the
//! compiled manifest, consent + license records, and the download worker.
//!
//! HONEST SCOPE: no vendored llama-server/sherpa binaries exist until the
//! P6.3 spike, so no supervisor is ever instantiated here yet — the
//! supervision machinery is core code verified against the stub child
//! (tests/runtime_process.rs); this host computes plans, serves status,
//! and wires consent/downloads. Features light up individually as
//! readiness events arrive (§8.3/§10.5); in this packet they simply never
//! do, which renders exactly the degraded mode that is the whole M1
//! product (§7).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use photoproof_connectors::config::{
    AsrBackend, Config, EmbedderBackend, LlmBackend, ModelSelection, TextEmbedderBackend,
    from_toml_str, with_selected_model,
};
use photoproof_core::UtcMillis;
use photoproof_core::runtime::{
    Acceptances, ChildRegistry, ControlFileError, ControlFileErrorKind, ControlFileRecovery,
    ControlFileSource, DownloadError, DownloadManager, DownloadPhase, HardwareProbe,
    HardwareReport, InstanceLock, Manifest, Pacer, ProcessPlan, RuntimeBus, RuntimePlan,
    SleepPacer, TierCache, TierDecision, compiled_manifest, decide_tier, load_control, plan,
    resolve_tier_checked, save_control,
};

use crate::dto::{
    EmbedderSlot, EmbedderState, ModelConsumerStatus, ModelOperationStatus, ModelRow,
    RuntimeAdapterStatus, RuntimeCapabilities, RuntimeControlFileStatus, RuntimeModelCompatibility,
    RuntimeStatus,
};
use crate::hardware::LiveProbe;
use crate::managed_tasks::{ManagedTaskRegistry, TaskContext, TaskPriority};
use crate::model_registry::ModelOperationRegistry;

/// Adds manual processing Pause to core's existing capture-aware transport
/// pacer. Core invokes this after every 64 KiB write, so pausing leaves a valid
/// resumable `.part` and resumes the same HTTP body instead of cancelling or
/// fabricating a failed row.
struct GovernorDownloadPacer<'a> {
    capture: SleepPacer,
    resources: Option<Arc<crate::resource_governor::ResourceGovernor>>,
    cancel: Arc<AtomicBool>,
    registry: &'a ModelOperationRegistry,
    model_id: &'a str,
    attempt_id: &'a str,
}

impl Pacer for GovernorDownloadPacer<'_> {
    fn pace(&mut self, just_transferred: usize) {
        if let Some(resources) = &self.resources {
            let _ = resources.wait_until_resumed(&self.cancel);
        }
        self.capture.pace(just_transferred);
    }

    fn phase(&mut self, phase: DownloadPhase) {
        let phase = match phase {
            DownloadPhase::Downloading => "downloading",
            DownloadPhase::Verifying => "verifying",
            DownloadPhase::Installing => "installing",
        };
        self.registry
            .publish_operation(self.model_id, self.attempt_id, phase, false, None);
    }
}

/// Backoff before each automatic resume of an Interrupted transfer — and
/// ONLY that class: a connection cut or read stall on a multi-GB CDN
/// transfer is weather, not a verdict, and the part files mean every
/// retry resumes instead of restarting. Checksum/license/HTTP-status/
/// unpinned failures keep failing fast — retrying those re-runs a proof
/// that already came back false. Four retries (five attempts total) with
/// growing gaps outlasts the blips the founder actually hit (3× in one
/// dogfood night) without hammering a CDN that is genuinely down.
#[cfg(not(test))]
const INTERRUPTED_BACKOFF: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(30),
];
/// Tests run offline, where every connect fails into the same
/// Interrupted class as a mid-read cut — so they exercise the full
/// five-attempt POLICY. Only the schedule shrinks; minutes of wall sleep
/// would teach the drain test nothing.
#[cfg(test)]
const INTERRUPTED_BACKOFF: [std::time::Duration; 4] = [std::time::Duration::from_millis(1); 4];

/// The backoff sleeps in slices this long so the quit signal is observed
/// promptly — a quit mid-backoff must not hang on a 30 s sleep.
const BACKOFF_SLICE: std::time::Duration = std::time::Duration::from_millis(250);

/// D2: ceiling on an honored `Retry-After`. A server asking for a couple
/// of minutes is throttling honestly; anything longer would pin the ONE
/// download worker (and the whole queue behind it) on a single model, so
/// past this the schedule's own backoff takes over.
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(120);

/// D1: free space that must REMAIN after a download batch completes.
/// A model pull that lands the last byte on a zero-free disk still breaks
/// the app around it (SQLite journal, previews, logs all share the
/// volume), so the preflight demands the batch fit with this to spare.
const DOWNLOAD_DISK_MARGIN_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// D1: the disk-space preflight verdict, pure for testing. `available`
/// is `None` when the platform cannot answer (see
/// `available_disk_bytes`) — that must PASS, not block: an unknown is
/// not a zero. A zero-byte requirement (everything already on disk,
/// only verification left) also passes — finishing needs no new space.
/// Returns `Some((needed, available))` when the batch must be refused,
/// with `needed` including the margin (it is the "needs X free" the
/// settings row shows).
fn disk_shortfall(required_bytes: u64, available: Option<u64>) -> Option<(u64, u64)> {
    if required_bytes == 0 {
        return None;
    }
    let available = available?;
    let needed = required_bytes.saturating_add(DOWNLOAD_DISK_MARGIN_BYTES);
    (available < needed).then_some((needed, available))
}

/// D2: should this failed attempt re-run on the backoff schedule?
/// `None` = terminal (retrying re-proves a falsehood: checksum, license,
/// 4xx verdicts). `Some(server_wait)` = retryable, where the inner
/// Option carries a server-requested Retry-After (capped at
/// [`RETRY_AFTER_CAP`]) to honor INSTEAD of the schedule's own gap.
fn retry_wait(err: &DownloadError) -> Option<Option<std::time::Duration>> {
    match err {
        // A cut or stall is weather; the part files make retries resume.
        DownloadError::Interrupted { .. } => Some(None),
        DownloadError::Http {
            status,
            retry_after_secs,
            ..
        } if photoproof_core::runtime::is_retryable_status(*status) => {
            Some(retry_after_secs.map(|s| std::time::Duration::from_secs(s).min(RETRY_AFTER_CAP)))
        }
        _ => None,
    }
}

/// §10.3: "Later" re-offers from settings only; "Never" is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Undecided,
    Later,
    Never,
    Download,
}

/// Result of a successfully persisted consent decision. An automatic
/// "Download now" dispatch happens only after that durable commit, so its
/// failure cannot be represented by returning `Err`: doing so would tell the
/// caller the consent mutation rolled back when the next launch will in fact
/// read `download`. The command layer returns this secondary failure alongside
/// the committed global runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentCommit {
    pub operation_error: Option<String>,
}

impl Consent {
    pub fn as_str(self) -> &'static str {
        match self {
            Consent::Undecided => "undecided",
            Consent::Later => "later",
            Consent::Never => "never",
            Consent::Download => "download",
        }
    }

    fn parse_checked(s: &str) -> Result<Self, String> {
        match s {
            "undecided" => Ok(Consent::Undecided),
            "later" => Ok(Consent::Later),
            "never" => Ok(Consent::Never),
            "download" => Ok(Consent::Download),
            other => Err(format!("unsupported consent value {other:?}")),
        }
    }
}

// Runtime persistence paths. Load (init) and save (set_consent /
// accept_license / redetect_tier) used to assemble these from different
// roots with separate string literals; one drifting literal would make
// saves keep succeeding while the next launch silently loads nothing.
// Every site goes through these helpers so load and save are provably
// the same file.

fn runtime_dir(app_data: &std::path::Path) -> PathBuf {
    app_data.join("runtime")
}

/// §6: the cached tier detection (`resolve_tier` reads and writes it).
fn tier_path(app_data: &std::path::Path) -> PathBuf {
    runtime_dir(app_data).join("tier.json")
}

fn unknown_hardware_report() -> HardwareReport {
    HardwareReport {
        schema_version: 1,
        adapters: Vec::new(),
        apple_unified_bytes: None,
        detected_at: String::new(),
    }
}

/// §5.3: recorded license acceptances.
fn acceptances_path(app_data: &std::path::Path) -> PathBuf {
    runtime_dir(app_data).join("acceptances.json")
}

/// §10.2–10.3: the remembered consent decision.
fn consent_path(app_data: &std::path::Path) -> PathBuf {
    runtime_dir(app_data).join("consent")
}

fn control_status(
    name: &str,
    recovery: Option<ControlFileRecovery>,
    errors: Vec<ControlFileError>,
    validation_warnings: Vec<String>,
) -> RuntimeControlFileStatus {
    RuntimeControlFileStatus {
        name: name.into(),
        recovery,
        errors,
        validation_warnings,
    }
}

fn control_io_error(path: &std::path::Path, error: std::io::Error) -> ControlFileError {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => ControlFileErrorKind::Missing,
        std::io::ErrorKind::PermissionDenied => ControlFileErrorKind::PermissionDenied,
        _ => ControlFileErrorKind::Io,
    };
    ControlFileError {
        kind,
        path: path.to_owned(),
        detail: error.to_string(),
        quarantined_path: None,
    }
}

fn committed_recovery() -> ControlFileRecovery {
    ControlFileRecovery {
        source: ControlFileSource::Primary,
        quarantined: Vec::new(),
        warnings: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityPhase {
    Provisional,
    Detecting,
    Ready,
    Failed,
}

impl CapabilityPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::Detecting => "detecting",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveVectorTarget {
    pub models: std::collections::HashMap<photoproof_connectors::vector_store::VecKind, String>,
    pub ready_generations:
        std::collections::HashMap<photoproof_connectors::vector_store::VecKind, u64>,
}

/// Feeds an already-completed live report through core's durable tier
/// resolution path. Keeping the driver call separate lets the host avoid
/// holding its status mutex while still retaining quarantine/LKG behavior.
struct FixedHardwareProbe(Option<HardwareReport>);

impl HardwareProbe for FixedHardwareProbe {
    fn probe(&mut self) -> HardwareReport {
        self.0
            .take()
            .expect("fixed hardware probe is consumed exactly once")
    }
}

struct HostState {
    config: Config,
    /// Read by the debug panel (feature-gated); kept in every build.
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
    config_warnings: Vec<String>,
    control_files: BTreeMap<String, RuntimeControlFileStatus>,
    tier: TierDecision,
    capability_phase: CapabilityPhase,
    capability_summary: Option<String>,
    hardware_report: Option<HardwareReport>,
    capabilities: Option<RuntimeCapabilities>,
    consent: Consent,
    acceptances: Acceptances,
    /// model id → live download progress (downloaded, total), both in
    /// MODEL-cumulative bytes (bytes of the model on disk over its
    /// manifest total — the bus speaks the same language). An entry
    /// exists from enqueue to completion; it is seeded from the on-disk
    /// baseline so a resumed multi-GB part never reads "0 bytes".
    downloads: BTreeMap<String, (u64, u64)>,
    /// model id → surfaced failure (§5.2: settings + debug panel).
    download_errors: BTreeMap<String, String>,
    /// model id → live auto-retry hint while the worker waits out an
    /// interrupted transfer. Present ONLY between an interruption and the
    /// final verdict — the row stays "downloading" (no error row) so a
    /// connection cut on a multi-GB CDN transfer doesn't flash a terminal
    /// "failed" the founder has to click through (happened 3× in one
    /// dogfood night).
    download_retries: BTreeMap<String, String>,
    /// §5.2 "one file at a time" is a rule of the download MANAGER, not
    /// of one model: pending model ids drain through ONE worker thread.
    download_queue: VecDeque<String>,
    /// D3: model id → the cancel flag its transfer observes (per chunk
    /// and between files). One flag per ENQUEUE, replaced on re-download,
    /// so a cancel can never leak into a later attempt of the same model.
    download_cancels: BTreeMap<String, Arc<AtomicBool>>,
    /// Model id → the stable attempt identity shared by all seven observable
    /// operation phases. Re-enqueue always mints a new id.
    download_attempts: BTreeMap<String, String>,
    download_worker_live: bool,
    /// Permanent process-exit admission latch. Set before cancellation so a
    /// settings command racing quit cannot enqueue fresh mutation after the
    /// queue has been terminally settled.
    downloads_stopping: bool,
    /// Monotonic task key generation. A worker that has atomically observed an
    /// empty queue can overlap a newly-enqueued successor only while returning;
    /// distinct keys let the successor start without racing the old managed
    /// task's terminal bookkeeping.
    download_worker_generation: u64,
    /// Models in an unload/remove transition are excluded from plan snapshots
    /// before their durable installed record changes. This lets consumers
    /// drain while the old index still truthfully describes on-disk files.
    unavailable_models: BTreeSet<String>,
    /// Startup orphan sweep result (killed, skipped) for the debug panel.
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
    orphan_sweep: (Vec<String>, Vec<String>),
}

/// The backend-owned first-run offer: one configured model for each local
/// functional seam. Tier-compatible alternatives remain visible in Settings,
/// but consent never downloads them implicitly.
fn configured_default_offer_ids(config: &Config) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if config.llm.backend == LlmBackend::LocalLlamacpp {
        ids.insert(config.llm.model.clone());
    }
    if config.asr.backend == AsrBackend::LocalSherpa {
        ids.insert(config.asr.model.clone());
    }
    if config.embedder.backend == EmbedderBackend::LocalOrt {
        ids.insert(config.embedder.model.clone());
    }
    if config.embedder.text.backend == TextEmbedderBackend::LocalOrt {
        ids.insert(config.embedder.text.model.clone());
    }
    ids
}

fn model_seam(role: &str) -> Option<&'static str> {
    if role == "llm" || role == "llm-alt" {
        Some("llm")
    } else if role == "asr" {
        Some("asr")
    } else if role == "embedder" {
        Some("clip")
    } else if role == "text-embedder" || role == "text-embedder-alt" {
        Some("text-embedder")
    } else {
        None
    }
}

fn selected_default_offer_ids(
    config: &Config,
    manifest: &Manifest,
    tier: u8,
    capabilities: Option<&RuntimeCapabilities>,
) -> BTreeSet<String> {
    let requested = configured_default_offer_ids(config);
    let safe_defaults = configured_default_offer_ids(&Config::default());
    let compatible = |model_id: &str| {
        capabilities.is_some_and(|capabilities| {
            capabilities
                .model_compatibility
                .iter()
                .find(|row| row.model_id == model_id)
                .is_some_and(|row| row.compatible)
        })
    };
    let offered = manifest
        .offered_at(tier)
        .into_iter()
        .filter(|model| model.is_pinned() && compatible(&model.id))
        .collect::<Vec<_>>();
    let mut selected = BTreeSet::new();
    for seam in ["llm", "asr", "clip", "text-embedder"] {
        let seam_enabled = match seam {
            "llm" => config.llm.backend == LlmBackend::LocalLlamacpp,
            "asr" => config.asr.backend == AsrBackend::LocalSherpa,
            "clip" => config.embedder.backend == EmbedderBackend::LocalOrt,
            "text-embedder" => config.embedder.text.backend == TextEmbedderBackend::LocalOrt,
            _ => false,
        };
        // External/disabled connector seams intentionally offer no local model.
        if !seam_enabled {
            continue;
        }
        let candidates = offered
            .iter()
            .copied()
            .filter(|model| model_seam(&model.role) == Some(seam))
            .collect::<Vec<_>>();
        let configured = candidates
            .iter()
            .find(|model| requested.contains(&model.id))
            .copied();
        let safe = candidates
            .iter()
            .find(|model| safe_defaults.contains(&model.id))
            .copied();
        if let Some(model) = configured.or(safe).or_else(|| candidates.first().copied()) {
            selected.insert(model.id.clone());
        }
    }
    selected
}

fn embedder_state_name(state: EmbedderState) -> &'static str {
    match state {
        EmbedderState::Idle => "idle",
        EmbedderState::Queued => "queued",
        EmbedderState::Building => "building",
        EmbedderState::Ready => "ready",
        EmbedderState::Failed => "failed",
        EmbedderState::Stopping => "stopping",
    }
}

fn embedder_consumer(
    role: &str,
    desired_model_id: Option<&str>,
    row_model_id: &str,
    slot: &EmbedderSlot,
) -> Option<ModelConsumerStatus> {
    let desired = desired_model_id == Some(row_model_id);
    let active = slot.model_id.as_deref() == Some(row_model_id);
    if !desired && !active {
        return None;
    }
    let (requested_provider, actual_provider, fallback_reason) =
        if let Some(execution) = &slot.execution {
            let requested = execution
                .sessions
                .iter()
                .flat_map(|session| session.requested.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let actual = execution
                .sessions
                .iter()
                .flat_map(|session| session.actual.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let fallback = execution
                .sessions
                .iter()
                .filter_map(|session| session.fallback_reason.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            (
                (!requested.is_empty()).then(|| requested.join(" + ")),
                Some(if actual.is_empty() {
                    "unknown".into()
                } else {
                    actual.join(" + ")
                }),
                (!fallback.is_empty()).then(|| fallback.join("; ")),
            )
        } else {
            (None, None, None)
        };
    Some(ModelConsumerStatus {
        role: role.into(),
        desired,
        active,
        state: if active {
            embedder_state_name(slot.state).into()
        } else {
            "idle".into()
        },
        retryable: active && slot.state == EmbedderState::Failed,
        error: active.then(|| slot.error.clone()).flatten(),
        requested_provider,
        actual_provider,
        fallback_reason,
    })
}

fn child_consumer(
    role: &str,
    configured_model_id: Option<&str>,
    row_model_id: &str,
    slot: &crate::supervisors::SupervisorRoleSnapshot,
    blocked: Option<&str>,
    requested_provider: Option<String>,
) -> Option<ModelConsumerStatus> {
    let desired = configured_model_id == Some(row_model_id)
        || slot.desired_model_id.as_deref() == Some(row_model_id);
    let active = slot.active_model_id.as_deref() == Some(row_model_id);
    if !desired && !active {
        return None;
    }
    Some(ModelConsumerStatus {
        role: role.into(),
        desired,
        active,
        state: if active {
            slot.state.clone()
        } else {
            "notConfigured".into()
        },
        retryable: slot.retryable || blocked.is_some(),
        error: desired.then(|| blocked.map(str::to_owned)).flatten(),
        requested_provider,
        // The child protocols do not currently expose per-graph placement.
        // Unknown is honest backend truth; it must never be inferred from
        // adapter discovery or launch arguments.
        actual_provider: active.then(|| "unknown".into()),
        fallback_reason: active
            .then(|| "child runtime does not report per-model execution placement".into()),
    })
}

pub struct RuntimeHost {
    pub bus: RuntimeBus,
    /// P6.4: the real supervisors (None inside until the plan says Run).
    pub supervisors: crate::supervisors::SupervisorHost,
    /// P7.4: the isolated ORT embedder helpers (§3.3). Converges on the same
    /// plan as the supervisors, on the same 2 s loop; readiness flows into
    /// `RuntimeStatus` and the search rig.
    pub embedders: crate::embedders::EmbedderHost,
    app_data: PathBuf,
    manifest: Manifest,
    lock: Option<Arc<InstanceLock>>,
    /// §5.2 throttle-while-capture-live seam: P6.3's arm/disarm wiring
    /// flips this; the pacer consults it per chunk.
    pub capture_live: Arc<AtomicBool>,
    /// Serializes read-modify-persist-commit control-file actions without
    /// holding the main status mutex across fsync.
    control_file_gate: Mutex<()>,
    state: Mutex<HostState>,
    /// One authority for model operations and the verified, cheap in-memory
    /// installed snapshot. The operation gate also serializes different model
    /// ids because they share one installed.json read-modify-write commit.
    model_registry: ModelOperationRegistry,
    /// Download transfers participate in the process-wide shutdown barrier and
    /// remain visible through application health instead of escaping as a
    /// detached thread.
    tasks: Arc<ManagedTaskRegistry>,
    /// Pins the §5.2 cross-model serialization: which thread ran each
    /// model's download, in order.
    #[cfg(test)]
    download_thread_log: Mutex<Vec<(String, std::thread::ThreadId)>>,
}

#[derive(Debug, Clone)]
pub struct ConfigReload {
    pub status: RuntimeControlFileStatus,
    pub changed: bool,
}

impl RuntimeHost {
    #[cfg(test)]
    pub fn init(app_data: PathBuf) -> Self {
        Self::init_managed(app_data, Arc::new(ManagedTaskRegistry::default()))
    }

    pub fn init_managed(app_data: PathBuf, tasks: Arc<ManagedTaskRegistry>) -> Self {
        let runtime_dir = runtime_dir(&app_data);
        let bus = RuntimeBus::new();
        let mut control_files = BTreeMap::new();
        // §8.5: the exclusive lock; supervisors refuse to spawn without
        // it. (The Tauri single-instance plugin already kept a second
        // launch from reaching this point; belt and braces.)
        let lock = match InstanceLock::acquire(&runtime_dir) {
            Ok(lock) => lock.map(Arc::new),
            Err(error) => {
                control_files.insert(
                    "instance-lock".into(),
                    control_status(
                        "instance-lock",
                        None,
                        vec![control_io_error(&runtime_dir.join("instance.lock"), error)],
                        Vec::new(),
                    ),
                );
                None
            }
        };
        // §8.4 crash net, before any spawn could ever happen — but ONLY
        // while holding the §8.5 lock. Without it another instance is
        // live, and its healthy supervised children are exactly what the
        // sweep looks for (alive PID + matching start-time): a lockless
        // sweep would SIGKILL the live instance's children and erase its
        // crash-net records. No lock ⇒ children.json is not touched.
        // The registry PERSISTS (P6.4): supervised children record
        // themselves through it for the next launch's sweep.
        let registry = if lock.is_some() {
            match ChildRegistry::new(&runtime_dir) {
                Ok(registry) => Some(Arc::new(registry)),
                Err(error) => {
                    control_files.insert(
                        "children".into(),
                        control_status(
                            "children",
                            None,
                            vec![control_io_error(&runtime_dir.join("children.json"), error)],
                            Vec::new(),
                        ),
                    );
                    None
                }
            }
        } else {
            None
        };
        let mut orphan_sweep = (Vec::new(), Vec::new());
        if let Some(registry) = &registry {
            let children_path = runtime_dir.join("children.json");
            let (mut status, can_sweep) = match photoproof_core::runtime::load_json::<
                Vec<photoproof_core::runtime::ChildRecord>,
            >(&children_path)
            {
                Ok(loaded) => (
                    control_status("children", Some(loaded.recovery), Vec::new(), Vec::new()),
                    true,
                ),
                Err(issue) => (
                    control_status("children", None, vec![issue], Vec::new()),
                    false,
                ),
            };
            if can_sweep {
                match registry.kill_orphans() {
                    Ok(sweep) => orphan_sweep = sweep,
                    Err(error) => status.errors.push(control_io_error(&children_path, error)),
                }
            } else if let Err(error) = photoproof_core::runtime::save_json(
                &children_path,
                &Vec::<photoproof_core::runtime::ChildRecord>::new(),
            ) {
                // Preserve the corrupt bytes in quarantine and report that
                // their old children could not be proven/killed, but restore a
                // fresh registry so newly spawned children remain trackable.
                status.errors.push(control_io_error(&children_path, error));
            }
            control_files.insert("children".into(), status);
        }

        // §4.4 config: invalid bytes are quarantined and recover from LKG.
        // Missing is the only case that quietly selects defaults.
        let config_path = app_data.join("config.toml");
        let config_loaded = load_control(&config_path, |bytes| {
            let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            from_toml_str(text)
                .map(|loaded| {
                    let warnings: Vec<String> = loaded
                        .unknown_keys
                        .iter()
                        .map(|key| format!("config.toml: unknown key `{key}`"))
                        .collect();
                    (loaded.config, warnings)
                })
                .map_err(|error| error.to_string())
        });
        let (config, config_warnings) = match config_loaded {
            Ok(loaded) => {
                let (config, warnings) = loaded.value.unwrap_or_default();
                control_files.insert(
                    "config".into(),
                    control_status(
                        "config",
                        Some(loaded.recovery),
                        Vec::new(),
                        warnings.clone(),
                    ),
                );
                (config, warnings)
            }
            Err(issue) => {
                let warning = format!("config.toml recovery failed ({issue}); using defaults");
                control_files.insert(
                    "config".into(),
                    control_status("config", None, vec![issue], vec![warning.clone()]),
                );
                (Config::default(), vec![warning])
            }
        };

        // §6: launch never enters a graphics driver here. Adopt a cached
        // report provisionally when one is readable; otherwise use the
        // conservative no-adapter decision (with the explicit config override
        // still winning). A managed post-Usable task validates this in the
        // background and atomically swaps in the fresh report + decision.
        let (tier, hardware_report) = match TierCache::load_checked(&tier_path(&app_data)) {
            Ok(loaded) => {
                let cached = loaded.value;
                control_files.insert(
                    "tier".into(),
                    control_status("tier", Some(loaded.recovery), Vec::new(), Vec::new()),
                );
                match cached {
                    Some(cached) => (
                        decide_tier(&cached.report, config.runtime.tier),
                        Some(cached.report),
                    ),
                    None => (
                        decide_tier(&unknown_hardware_report(), config.runtime.tier),
                        None,
                    ),
                }
            }
            Err(issue) => {
                control_files.insert(
                    "tier".into(),
                    control_status("tier", None, vec![issue], Vec::new()),
                );
                (
                    decide_tier(&unknown_hardware_report(), config.runtime.tier),
                    None,
                )
            }
        };

        // §5.1: the compiled manifest, also written for the debug panel.
        let manifest = compiled_manifest();
        let manifest_path = Self::models_dir_for(&app_data, &config).join("manifest.json");
        let manifest_errors = manifest
            .write_to(&Self::models_dir_for(&app_data, &config))
            .err()
            .map(|error| vec![control_io_error(&manifest_path, error)])
            .unwrap_or_default();
        control_files.insert(
            "manifest".into(),
            control_status("manifest", None, manifest_errors, Vec::new()),
        );

        let acceptances_path = acceptances_path(&app_data);
        let acceptances = match Acceptances::load_checked(&acceptances_path) {
            Ok(loaded) => {
                control_files.insert(
                    "acceptances".into(),
                    control_status("acceptances", Some(loaded.recovery), Vec::new(), Vec::new()),
                );
                loaded.value.unwrap_or_default()
            }
            Err(issue) => {
                control_files.insert(
                    "acceptances".into(),
                    control_status("acceptances", None, vec![issue], Vec::new()),
                );
                Acceptances::default()
            }
        };
        let consent_path = consent_path(&app_data);
        let consent = match load_control(&consent_path, |bytes| {
            let value = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            Consent::parse_checked(value.trim())
        }) {
            Ok(loaded) => {
                control_files.insert(
                    "consent".into(),
                    control_status("consent", Some(loaded.recovery), Vec::new(), Vec::new()),
                );
                loaded.value.unwrap_or(Consent::Undecided)
            }
            Err(issue) => {
                control_files.insert(
                    "consent".into(),
                    control_status("consent", None, vec![issue], Vec::new()),
                );
                Consent::Undecided
            }
        };

        let supervisors =
            crate::supervisors::SupervisorHost::new(bus.clone(), registry, lock.clone());
        let model_registry = ModelOperationRegistry::open(
            Self::models_dir_for(&app_data, &config),
            &manifest,
            bus.clone(),
            lock.is_some(),
        );
        Self {
            bus,
            app_data,
            manifest,
            lock,
            supervisors,
            embedders: crate::embedders::EmbedderHost::new(),
            capture_live: Arc::new(AtomicBool::new(false)),
            control_file_gate: Mutex::new(()),
            tasks,
            model_registry,
            state: Mutex::new(HostState {
                config,
                config_warnings,
                control_files,
                tier,
                capability_phase: CapabilityPhase::Provisional,
                capability_summary: Some(if hardware_report.is_some() {
                    "using cached hardware capabilities while background validation runs".into()
                } else {
                    "hardware capabilities have not been detected yet".into()
                }),
                hardware_report,
                capabilities: None,
                consent,
                acceptances,
                downloads: BTreeMap::new(),
                download_errors: BTreeMap::new(),
                download_retries: BTreeMap::new(),
                download_queue: VecDeque::new(),
                download_cancels: BTreeMap::new(),
                download_attempts: BTreeMap::new(),
                download_worker_live: false,
                downloads_stopping: false,
                download_worker_generation: 0,
                unavailable_models: BTreeSet::new(),
                orphan_sweep,
            }),
            #[cfg(test)]
            download_thread_log: Mutex::new(Vec::new()),
        }
    }

    fn models_dir_for(app_data: &std::path::Path, config: &Config) -> PathBuf {
        if config.runtime.models_dir.is_empty() {
            app_data.join("models")
        } else {
            let configured = PathBuf::from(&config.runtime.models_dir);
            if configured.is_absolute() {
                configured
            } else {
                app_data.join(configured)
            }
        }
    }

    /// Effective model/download root after resolving a relative config path
    /// against app data. Storage health uses this rather than assuming models
    /// share the database volume.
    pub fn models_dir(&self) -> PathBuf {
        let state = self.state.lock().expect("runtime state");
        Self::models_dir_for(&self.app_data, &state.config)
    }

    /// Parse, recover, and atomically install an externally edited
    /// `config.toml`. A failed candidate never changes the live plan. The model
    /// store root is intentionally launch-bound because the registry owns
    /// in-flight operations and verified installed state for that directory;
    /// changing it underneath those operations is rejected with retained
    /// status and takes effect on the next launch instead.
    pub fn reload_config_checked(&self) -> Result<ConfigReload, String> {
        let _control_guard = self
            .control_file_gate
            .lock()
            .expect("runtime control-file gate");
        let path = self.app_data.join("config.toml");

        // Preflight the one launch-bound field before `load_control` refreshes
        // the LKG. A valid next-launch configuration must not replace the live
        // process's rollback copy when it cannot be applied to this registry.
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(text) = std::str::from_utf8(&bytes)
            && let Ok(parsed) = from_toml_str(text)
        {
            let requested = Self::models_dir_for(&self.app_data, &parsed.config);
            if requested != self.model_registry.models_dir() {
                let detail = format!(
                    "config.toml models_dir change to {} is launch-bound; \
                     current runtime remains on {} until restart",
                    requested.display(),
                    self.model_registry.models_dir().display()
                );
                let status = control_status("config", None, Vec::new(), vec![detail.clone()]);
                self.state
                    .lock()
                    .expect("runtime state")
                    .control_files
                    .insert("config".into(), status);
                return Err(detail);
            }
        }

        let loaded = match load_control(&path, |bytes| {
            let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            from_toml_str(text)
                .map(|loaded| {
                    let warnings = loaded
                        .unknown_keys
                        .iter()
                        .map(|key| format!("config.toml: unknown key `{key}`"))
                        .collect::<Vec<_>>();
                    (loaded.config, warnings)
                })
                .map_err(|error| error.to_string())
        }) {
            Ok(loaded) => loaded,
            Err(issue) => {
                let status = control_status("config", None, vec![issue.clone()], Vec::new());
                self.state
                    .lock()
                    .expect("runtime state")
                    .control_files
                    .insert("config".into(), status);
                return Err(issue.to_string());
            }
        };
        let recovery = loaded.recovery;
        let (config, warnings) = loaded.value.unwrap_or_default();
        let status = control_status("config", Some(recovery), Vec::new(), warnings.clone());
        let changed = {
            let mut state = self.state.lock().expect("runtime state");
            let report = state
                .hardware_report
                .clone()
                .unwrap_or_else(unknown_hardware_report);
            state.tier = decide_tier(&report, config.runtime.tier);
            let changed = state.config != config;
            state.config = config;
            state.config_warnings = warnings;
            state.control_files.insert("config".into(), status.clone());
            changed
        };
        drop(_control_guard);
        if changed {
            self.apply_supervisor_plan();
        }
        Ok(ConfigReload { status, changed })
    }

    /// Settings → Models: persist one installed alternative as the configured
    /// model for its functional seam, then converge the live runtime onto it.
    /// Selection is deliberately narrower than editing config.toml: an
    /// unavailable, incompatible, or incomplete model can never be activated
    /// by a stale UI row.
    pub fn select_model(&self, model_id: &str) -> Result<ConfigReload, String> {
        self.require_model_mutation_authority()?;
        let snapshot = self.status();
        let row = snapshot
            .models
            .iter()
            .find(|row| row.id == model_id)
            .ok_or_else(|| format!("unknown model `{model_id}`"))?;
        if row.state != "installed" {
            return Err(format!(
                "{} must finish downloading and verification before it can be selected",
                model_id
            ));
        }
        if !row.compatible {
            return Err(format!(
                "{} is not compatible with this machine: {}",
                model_id, row.compatibility_reason
            ));
        }
        if !row.default_offer && !row.advanced_available {
            return Err(format!(
                "{} is not available for the current hardware tier",
                model_id
            ));
        }
        if row.default_offer {
            return Ok(ConfigReload {
                status: snapshot
                    .control_files
                    .iter()
                    .find(|status| status.name == "config")
                    .cloned()
                    .unwrap_or_else(|| control_status("config", None, Vec::new(), Vec::new())),
                changed: false,
            });
        }
        let manifest_model = self
            .manifest
            .model(model_id)
            .ok_or_else(|| format!("unknown model `{model_id}`"))?;
        let selection = match manifest_model.role.as_str() {
            "llm" | "llm-alt" => ModelSelection::Llm,
            "asr" => ModelSelection::Asr,
            "embedder" => ModelSelection::VisualEmbedder,
            "text-embedder" | "text-embedder-alt" => ModelSelection::TextEmbedder,
            role => return Err(format!("model role `{role}` is not selectable")),
        };
        let path = self.app_data.join("config.toml");
        let current = match std::fs::read_to_string(&path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(format!("could not read {}: {error}", path.display()));
            }
        };
        let next = with_selected_model(&current, selection, model_id)
            .map_err(|error| error.to_string())?;
        {
            let _control_guard = self
                .control_file_gate
                .lock()
                .expect("runtime control-file gate");
            save_control(&path, next.as_bytes())
                .map_err(|error| format!("could not save {}: {error}", path.display()))?;
        }
        self.reload_config_checked()
    }

    fn manager(&self) -> DownloadManager {
        DownloadManager::new(
            self.model_registry.models_dir().to_owned(),
            self.bus.clone(),
        )
    }

    pub fn model_registry_recovery_pending(&self) -> bool {
        self.model_registry.recovery_pending()
    }

    pub fn recover_model_registry(
        &self,
        cancel: &AtomicBool,
    ) -> Result<crate::model_registry::RegistryRecoveryReport, String> {
        self.model_registry
            .recover_pending(&self.manifest, self.bus.clone(), cancel)
    }

    fn require_model_mutation_authority(&self) -> Result<(), String> {
        if self.lock.is_some() {
            Ok(())
        } else {
            Err("another Photoproof instance owns the runtime; model files were not changed".into())
        }
    }

    pub fn plan(&self) -> RuntimePlan {
        let state = self.state.lock().expect("runtime state");
        let installed = self.manager_installed(&state);
        // Cached tier data is display/recovery context only. Until this launch
        // has an authoritative adapter + ORT-library report, local runtime
        // roles stay dark; a stale GPU cache must never launch an incompatible
        // supervisor or enter native ORT construction.
        let effective_tier =
            if state.capability_phase == CapabilityPhase::Ready && state.capabilities.is_some() {
                state.tier.effective_tier
            } else {
                0
            };
        let mut resolved = plan(&state.config, effective_tier, &self.manifest, &installed);
        if let Some(capabilities) = &state.capabilities {
            for slot in [
                &mut resolved.llm,
                &mut resolved.asr,
                &mut resolved.clip_embedder,
                &mut resolved.text_embedder,
            ] {
                let ProcessPlan::Run { model_id } = slot else {
                    continue;
                };
                if let Some(compatibility) = capabilities
                    .model_compatibility
                    .iter()
                    .find(|compatibility| compatibility.model_id == *model_id)
                    && !compatibility.compatible
                {
                    *slot = ProcessPlan::NotConfigured {
                        reason: compatibility.reason.clone(),
                        fixable_by_download: false,
                    };
                }
            }
        }
        resolved
    }

    /// P6.4/P7.4: converge BOTH the supervisors (external children) and the
    /// embedder helpers onto the current plan. Called at startup and on
    /// a slow cadence (state.rs) — every consent/config/download mutation is
    /// picked up within a couple of seconds without each command needing to
    /// remember to call this. The one plan computation drives both hosts so
    /// they never see divergent installed/tier state.
    pub fn apply_supervisor_plan(&self) {
        // Download/verify/remove may hold the gate across cancellable file
        // work. A lifecycle tick must never block the shell behind that I/O;
        // skipping one converge is safe because the owned cadence retries.
        // Entering the same authority still guarantees that load/unload
        // dispatch cannot race an installed-index mutation or file deletion.
        let Some(_operation) = self.model_registry.try_lock_operation() else {
            return;
        };
        self.apply_supervisor_plan_locked();
    }

    /// Converge while the caller already owns the model-operation gate.
    /// Removal uses this after hiding a model from the plan so cancellation,
    /// helper kill/reap, consumer drain, deletion, and index commit form one
    /// serialized transition.
    fn apply_supervisor_plan_locked(&self) {
        let (ctx, slots, chunk, clip_backend, text_backend, models_dir) = {
            let state = self.state.lock().expect("runtime state");
            (
                state.config.llm.local_llamacpp.ctx_size,
                state.config.llm.local_llamacpp.parallel_slots,
                state.config.asr.chunk_ms,
                state.config.embedder.backend,
                state.config.embedder.text.backend,
                Self::models_dir_for(&self.app_data, &state.config),
            )
        };
        let plan = self.plan();
        self.supervisors
            .apply(&plan, &self.manifest, &models_dir, ctx, slots, chunk);
        // The embedders share the plan + models_dir; the backends gate which
        // roles build in isolated helpers (local-ort) vs. stay a remote seam.
        self.embedders
            .apply(&plan, clip_backend, text_backend, &models_dir);
    }

    /// The configured ASR model id — the supervised P2 child and the WS
    /// client must agree on it; both read it from here (§4.2).
    pub fn asr_model_id(&self) -> String {
        self.state
            .lock()
            .expect("runtime state")
            .config
            .asr
            .model
            .clone()
    }

    fn manager_installed(
        &self,
        state: &HostState,
    ) -> BTreeMap<String, photoproof_core::runtime::InstalledRecord> {
        let mut installed = self.model_registry.installed();
        installed.retain(|model_id, _| !state.unavailable_models.contains(model_id));
        installed
    }

    /// The model id and Ready generation each vector `vec_kind` is actively
    /// embedded under today, directly from the loaded embedder slots. Feeds
    /// the startup doctor's space reconciliation
    /// (STATE-INTEGRITY-AUDIT): a stored space under any OTHER model id for the
    /// same kind is superseded. ImageClip follows the CLIP embedder; the two
    /// text-embedder spaces (annotation_chunk + image_summary) follow the text
    /// embedder.
    pub fn active_vector_target(&self) -> ActiveVectorTarget {
        use photoproof_connectors::vector_store::VecKind;
        // Read the actual ready slots, not raw config: the embedder bypass may
        // run a DIFFERENT model than config names. The doctor must reconcile vector
        // spaces against the model that actually WRITES them, or it would drop
        // the live fallback's space as "superseded". An unready slot contributes
        // nothing, so the doctor leaves that kind's spaces alone.
        // VERIFY-BEFORE-RETIRE (self-heal 3A): only count a model as "active"
        // when its embedder is actually LOADED, not merely NAMED active by the
        // resolved plan. WHY: the plan can name fp16 active while fp16 fails to
        // load (unhostable on this host); reporting it as active made the doctor
        // treat the live dfn5b space as "superseded by fp16" and retire the only
        // copy of the library's vectors. A named-but-unloaded model contributes
        // nothing, so the doctor leaves that kind's spaces untouched until the
        // real write path comes up.
        let mut models = std::collections::HashMap::new();
        let mut generations = std::collections::HashMap::new();
        let identities = self.embedders.ready_vector_identities();
        let text = identities.text;
        let clip = identities.clip;
        if let Some((model_id, generation)) = clip {
            models.insert(VecKind::ImageClip, model_id);
            generations.insert(VecKind::ImageClip, generation);
        }
        if let Some((model_id, generation)) = text {
            models.insert(VecKind::AnnotationChunk, model_id.clone());
            models.insert(VecKind::ImageSummary, model_id.clone());
            generations.insert(VecKind::AnnotationChunk, generation);
            generations.insert(VecKind::ImageSummary, generation);
        }
        ActiveVectorTarget {
            models,
            ready_generations: generations,
        }
    }

    // ------------------------------------------------------------ status ----

    pub fn status(&self) -> RuntimeStatus {
        let state = self.state.lock().expect("runtime state");
        let installed = self.manager_installed(&state);
        let tier = state.tier.effective_tier;
        let authoritative_tier =
            if state.capability_phase == CapabilityPhase::Ready && state.capabilities.is_some() {
                tier
            } else {
                0
            };
        let offered = self.manifest.offered_at(authoritative_tier);
        let default_offer_ids = selected_default_offer_ids(
            &state.config,
            &self.manifest,
            authoritative_tier,
            state.capabilities.as_ref(),
        );
        let asr_slot = self.supervisors.asr_status();
        let llm_slot = self.supervisors.llm_status();
        let asr_blocked = self.supervisors.asr_blocked();
        let llm_blocked = self.supervisors.llm_blocked();
        let clip_slot = self.embedders.clip_slot();
        let text_slot = self.embedders.text_slot();
        let selected_for = |seam: &str| {
            default_offer_ids.iter().find_map(|id| {
                self.manifest
                    .model(id)
                    .filter(|model| model_seam(&model.role) == Some(seam))
                    .map(|_| id.as_str())
            })
        };
        let configured_llm = selected_for("llm");
        let configured_asr = selected_for("asr");
        let configured_clip = selected_for("clip");
        let configured_text = selected_for("text-embedder");
        let compatible = |model_id: &str| {
            state.capabilities.as_ref().is_none_or(|capabilities| {
                capabilities
                    .model_compatibility
                    .iter()
                    .find(|compatibility| compatibility.model_id == model_id)
                    .is_none_or(|compatibility| compatibility.compatible)
            })
        };
        let models = self
            .manifest
            .models
            .iter()
            .map(|m| {
                let compatibility = state.capabilities.as_ref().and_then(|capabilities| {
                    capabilities
                        .model_compatibility
                        .iter()
                        .find(|compatibility| compatibility.model_id == m.id)
                });
                let offered_here = offered.iter().any(|o| o.id == m.id) && compatible(&m.id);
                let default_offer = offered_here && default_offer_ids.contains(&m.id);
                let is_installed = installed.contains_key(&m.id);
                let downloading = state.downloads.get(&m.id);
                let partial_bytes = self.model_registry.partial_bytes(&m.id);
                let error = state.download_errors.get(&m.id).cloned();
                let operation = self.model_registry.operation(&m.id);
                let operation_event = self.model_registry.last_operation(&m.id);
                let active_download_phase = operation.as_deref().filter(|phase| {
                    matches!(
                        *phase,
                        "queued" | "downloading" | "verifying" | "installing"
                    )
                });
                let state_name = if let Some(phase) = active_download_phase {
                    phase
                } else if is_installed {
                    "installed"
                } else if error.is_some() {
                    "failed"
                } else if operation_event
                    .as_ref()
                    .is_some_and(|event| event.terminal && event.phase == "cancelled")
                {
                    "cancelled"
                } else if downloading.is_some() {
                    "downloading"
                } else if !offered_here {
                    "not-offered"
                } else if !m.is_pinned() {
                    // B55: offered here but no pin yet (spike session 2) —
                    // pending, not a failure; settings offers no Download
                    // button.
                    "unpinned"
                } else {
                    "not-downloaded"
                };
                let mut consumers = Vec::new();
                if let Some(consumer) = child_consumer(
                    "llm",
                    configured_llm,
                    &m.id,
                    &llm_slot,
                    llm_blocked.as_deref(),
                    Some("llama.cpp:auto".into()),
                ) {
                    consumers.push(consumer);
                }
                if let Some(consumer) = child_consumer(
                    "asr",
                    configured_asr,
                    &m.id,
                    &asr_slot,
                    asr_blocked.as_deref(),
                    Some(format!("{:?}", state.config.asr.device).to_lowercase()),
                ) {
                    consumers.push(consumer);
                }
                if let Some(consumer) =
                    embedder_consumer("clip", configured_clip, &m.id, &clip_slot)
                {
                    consumers.push(consumer);
                }
                if let Some(consumer) =
                    embedder_consumer("text-embedder", configured_text, &m.id, &text_slot)
                {
                    consumers.push(consumer);
                }
                ModelRow {
                    id: m.id.clone(),
                    role: m.role.clone(),
                    default_offer,
                    advanced_available: offered_here && !default_offer,
                    compatible: compatibility.is_some_and(|row| row.compatible),
                    compatibility_reason: compatibility
                        .map(|row| row.reason.clone())
                        .unwrap_or_else(|| {
                            "hardware capabilities have not settled for this launch".into()
                        }),
                    compatible_providers: compatibility
                        .map(|row| row.compatible_providers.clone())
                        .unwrap_or_default(),
                    consumers,
                    state: state_name.into(),
                    total_bytes: m.total_bytes,
                    downloaded_bytes: downloading.map(|(d, _)| *d).unwrap_or(if is_installed {
                        m.total_bytes
                    } else {
                        partial_bytes
                    }),
                    license_name: m.license.name.clone(),
                    license_url: m.license.url.clone(),
                    acceptance_required: m.license.acceptance_required,
                    accepted: state.acceptances.accepted.contains_key(&m.id),
                    error,
                    retry_hint: state.download_retries.get(&m.id).cloned(),
                    operation,
                    operation_event: operation_event.map(|event| ModelOperationStatus {
                        attempt_id: event.attempt_id,
                        sequence: event.sequence,
                        phase: event.phase,
                        terminal: event.terminal,
                        error: event.error,
                    }),
                    registry_error: self.model_registry.disagreement(&m.id),
                }
            })
            .collect();
        RuntimeStatus {
            // §8.3: Ready only when a supervised child reports it —
            // live since P6.4 (the supervisors hold the truth; absent
            // supervisors read false and the mic glyph stays away).
            asr_ready: self.supervisors.asr_ready(),
            llm_ready: self.supervisors.llm_ready(),
            // Plan-says-Run-but-no-binary (the June 2026 silent-dark
            // incident): the supervisors record the reason on the same
            // apply() converge that would have spawned the child, so this
            // surfacing is exactly as fresh as the readiness flags above.
            asr_blocked,
            llm_blocked,
            // P7.4 §3.3: isolated embedder readiness (helper sessions built). The
            // bools stay for the readiness-gate consumers; the `clip`/
            // `text_embedder` slots carry the full queued/building/ready/
            // failed/stopping lifecycle plus attempt identity and errors.
            clip_ready: self.embedders.clip_ready(),
            text_embedder_ready: self.embedders.text_ready(),
            clip: clip_slot,
            text_embedder: text_slot,
            capability_state: state.capability_phase.as_str().into(),
            capability_summary: state.capability_summary.clone(),
            capability_adapters: state
                .hardware_report
                .as_ref()
                .map(|report| {
                    report
                        .adapters
                        .iter()
                        .map(|adapter| RuntimeAdapterStatus {
                            name: adapter.name.clone(),
                            backend: adapter.backend.clone(),
                            vendor_id: adapter.vendor_id,
                            device_id: adapter.device_id,
                            driver: adapter.driver.clone(),
                            driver_info: adapter.driver_info.clone(),
                            vram_bytes: adapter.vram_bytes,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            capability_detected_at: state
                .hardware_report
                .as_ref()
                .map(|report| report.detected_at.clone())
                .filter(|detected_at| !detected_at.is_empty()),
            capabilities: state.capabilities.clone(),
            tier_detected: state.tier.detected_tier,
            tier_effective: tier,
            tier_overridden_above: state.tier.overridden_above,
            consent: state.consent.as_str().into(),
            consent_offer_bytes: self
                .manifest
                .models
                .iter()
                .filter(|model| default_offer_ids.contains(&model.id))
                .filter(|model| offered.iter().any(|offered| offered.id == model.id))
                .filter(|model| compatible(&model.id))
                .map(|model| model.total_bytes)
                .sum(),
            models,
            instance_lock_held: self.lock.is_some(),
            control_files: state.control_files.values().cloned().collect(),
        }
    }

    /// Debug-panel detail (§8.1/§8.6): plan states + the orphan sweep +
    /// config warnings.
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
    pub fn debug_lines(&self) -> Vec<String> {
        let state = self.state.lock().expect("runtime state");
        let plan = {
            let installed = self.manager_installed(&state);
            plan(
                &state.config,
                state.tier.effective_tier,
                &self.manifest,
                &installed,
            )
        };
        let describe = |name: &str, p: &ProcessPlan| match p {
            ProcessPlan::NotConfigured { reason, .. } => {
                format!("{name}: notConfigured — {reason}")
            }
            ProcessPlan::Run { model_id } => {
                format!("{name}: run {model_id} (awaiting P6.3 vendored binaries)")
            }
            ProcessPlan::External { base_url } => format!("{name}: external {base_url}"),
        };
        let mut lines = vec![
            format!(
                "tier: detected {} effective {}{}",
                state.tier.detected_tier,
                state.tier.effective_tier,
                if state.tier.overridden_above {
                    " (override above detected)"
                } else {
                    ""
                }
            ),
            describe("llm", &plan.llm),
            describe("asr", &plan.asr),
            describe("embedder", &plan.clip_embedder),
            describe("text-embedder", &plan.text_embedder),
            format!(
                "instance lock: {}",
                if self.lock.is_some() {
                    "held"
                } else {
                    "NOT HELD"
                }
            ),
            if self.lock.is_some() {
                format!(
                    "orphan sweep: killed {:?}, skipped {:?}",
                    state.orphan_sweep.0, state.orphan_sweep.1
                )
            } else {
                "orphan sweep: NOT RUN (instance lock not held, §8.5)".into()
            },
        ];
        // The June 2026 silent-dark incident: the plan lines above say
        // "run", but a Run plan whose binary resolution came up empty
        // never spawns anything — name that per process or the panel
        // shows a healthy-looking plan over a runtime that went dark.
        for (name, blocked) in [
            ("llm", self.supervisors.llm_blocked()),
            ("asr", self.supervisors.asr_blocked()),
        ] {
            if let Some(reason) = blocked {
                lines.push(format!("{name}: plan says run but {reason}"));
            }
        }
        // P7.4: the LIVE embedder-host slot state —
        // the plan lines above describe the PLAN; these the actual ort
        // sessions, including a degraded-with-error load (§3.3).
        lines.extend(self.embedders.debug_lines());
        lines.extend(self.model_registry.debug_lines());
        lines.extend(state.config_warnings.iter().cloned());
        // manifest_version drift (STATE-INTEGRITY-AUDIT.md): installed weights
        // whose recorded manifest version is behind the running manifest may be
        // stale. Surfaced, not auto-re-downloaded (a global bump must not nuke
        // every model); the user can re-download to refresh.
        let stale = photoproof_core::runtime::plan::stale_installed_models(
            &self.manifest,
            &self.manager_installed(&state),
        );
        if !stale.is_empty() {
            lines.push(format!(
                "model manifest drift: {} installed behind current manifest (re-download to refresh): {}",
                stale.len(),
                stale.join(", ")
            ));
        }
        for (id, (done, total)) in &state.downloads {
            lines.push(format!("download {id}: {done}/{total} bytes"));
        }
        for (id, err) in &state.download_errors {
            lines.push(format!("download {id} FAILED: {err}"));
        }
        lines
    }

    // ----------------------------------------------------------- actions ----

    /// §10.2–10.3: the one consent decision. "download" enqueues the
    /// offered models whose license state permits (gated models wait for
    /// their recorded acceptance).
    pub fn set_consent(self: &Arc<Self>, decision: &str) -> Result<ConsentCommit, String> {
        let consent = Consent::parse_checked(decision)?;
        let _control_guard = self
            .control_file_gate
            .lock()
            .expect("runtime control-file gate");
        let path = consent_path(&self.app_data);
        if let Err(error) = save_control(&path, consent.as_str().as_bytes()) {
            let issue = control_io_error(&path, error);
            let mut state = self.state.lock().expect("runtime state");
            state.control_files.insert(
                "consent".into(),
                control_status("consent", None, vec![issue.clone()], Vec::new()),
            );
            return Err(issue.to_string());
        }
        {
            let mut state = self.state.lock().expect("runtime state");
            state.consent = consent;
            state.control_files.insert(
                "consent".into(),
                control_status(
                    "consent",
                    Some(committed_recovery()),
                    Vec::new(),
                    Vec::new(),
                ),
            );
        }
        let operation_error = if consent == Consent::Download {
            self.download_offered().err()
        } else {
            None
        };
        Ok(ConsentCommit { operation_error })
    }

    /// §5.3: record an acceptance (model id, license url, timestamp).
    pub fn accept_license(&self, model_id: &str) -> Result<(), String> {
        let Some(model) = self.manifest.model(model_id) else {
            return Err(format!("unknown model {model_id:?}"));
        };
        let _control_guard = self
            .control_file_gate
            .lock()
            .expect("runtime control-file gate");
        let path = acceptances_path(&self.app_data);
        let mut next = self
            .state
            .lock()
            .expect("runtime state")
            .acceptances
            .clone();
        next.accept(model_id, &model.license.url, &UtcMillis::now().to_rfc3339());
        match next.save(&path) {
            Ok(()) => {
                let mut state = self.state.lock().expect("runtime state");
                state.acceptances = next;
                state.control_files.insert(
                    "acceptances".into(),
                    control_status(
                        "acceptances",
                        Some(committed_recovery()),
                        Vec::new(),
                        Vec::new(),
                    ),
                );
                Ok(())
            }
            Err(error) => {
                let issue = control_io_error(&path, error);
                let mut state = self.state.lock().expect("runtime state");
                state.control_files.insert(
                    "acceptances".into(),
                    control_status("acceptances", None, vec![issue.clone()], Vec::new()),
                );
                Err(issue.to_string())
            }
        }
    }

    /// Settings → download one model now (post-consent path; also the
    /// "Later → settings" re-offer, §10.3). Joins the single queue —
    /// never a transfer thread of its own (§5.2).
    pub fn download_model(self: &Arc<Self>, model_id: &str) -> Result<(), String> {
        let Some(model) = self.manifest.model(model_id).cloned() else {
            return Err(format!("unknown model {model_id:?}"));
        };
        if !model.is_pinned() {
            // B55: the worker would only fail closed; refuse at the seam
            // (settings shows no Download button for unpinned rows — this
            // guards the raw command). Every shipped model is pinned post-B73;
            // this guard stays for any future entry awaiting a spike pin.
            return Err(format!("{model_id} is not pinned yet"));
        }
        {
            let state = self.state.lock().expect("runtime state");
            if state.capability_phase != CapabilityPhase::Ready {
                return Err("hardware capabilities are not ready; no model was queued".into());
            }
            let compatible = state.capabilities.as_ref().is_some_and(|capabilities| {
                capabilities
                    .model_compatibility
                    .iter()
                    .find(|compatibility| compatibility.model_id == model_id)
                    .is_some_and(|compatibility| compatibility.compatible)
            });
            if !compatible || !model.tiers.contains(&state.tier.effective_tier) {
                return Err(format!(
                    "{model_id} is not compatible and offered for this machine"
                ));
            }
        }
        if self.model_registry.installed().contains_key(model_id) {
            return Err(format!(
                "{model_id} is already installed; use Verify or remove it before downloading again"
            ));
        }
        self.enqueue_downloads(vec![model])
    }

    fn download_offered(self: &Arc<Self>) -> Result<(), String> {
        let state = self.state.lock().expect("runtime state");
        if state.capability_phase != CapabilityPhase::Ready {
            return Err("hardware capabilities are not ready; no models were queued".into());
        }
        let tier = state.tier.effective_tier;
        let default_offer_ids = selected_default_offer_ids(
            &state.config,
            &self.manifest,
            tier,
            state.capabilities.as_ref(),
        );
        let compatible = state
            .capabilities
            .as_ref()
            .map(|capabilities| &capabilities.model_compatibility);
        let installed = self.model_registry.installed();
        // Unpinned entries (B55 fail-closed) never enqueue: the worker would
        // only mint a "failed" row for a download that cannot exist yet, and
        // settings shows them as pending instead. Every shipped model is
        // pinned post-B73; the filter stays for any future unpinned entry.
        let offered: Vec<_> = self
            .manifest
            .offered_at(tier)
            .into_iter()
            .filter(|model| default_offer_ids.contains(&model.id))
            .filter(|model| {
                compatible.is_some_and(|rows| {
                    rows.iter()
                        .find(|row| row.model_id == model.id)
                        .is_some_and(|row| row.compatible)
                })
            })
            .filter(|m| m.is_pinned())
            .filter(|m| !installed.contains_key(&m.id))
            .cloned()
            .collect();
        drop(state);
        self.enqueue_downloads(offered)
    }

    /// §5.2 "Concurrency: one file at a time" is a property of the
    /// download MANAGER, not of one model. Every requested model joins
    /// one queue and a single `pp-download` worker drains it in order —
    /// consent at Tier 1 enqueues four models and they transfer strictly
    /// sequentially (the within-model file ordering is core's half of
    /// the rule). The pacer throttle therefore applies to the one live
    /// transfer, undiluted.
    fn enqueue_downloads(
        self: &Arc<Self>,
        models: Vec<photoproof_core::runtime::ModelEntry>,
    ) -> Result<(), String> {
        self.require_model_mutation_authority()?;
        // Seed the progress row from the ON-DISK baseline, not zero: a
        // resumed download has its prior gigabytes in final + part files,
        // and a "0 bytes" row on resume reads like the progress was lost.
        // Stat the files BEFORE taking the state lock — downloaded_bytes
        // walks up to ~400 entries for DFN5B, and fs IO under the host
        // lock would stall every concurrent status() call.
        let manager = self.manager();
        let models: Vec<(photoproof_core::runtime::ModelEntry, u64)> = models
            .into_iter()
            .map(|m| {
                let on_disk = manager.downloaded_bytes(&m);
                (m, on_disk)
            })
            .collect();
        // D1 disk-space preflight, BEFORE anything is queued: remaining
        // bytes for the whole batch (manifest totals minus what the part/
        // final files already hold) must fit on the models volume with a
        // margin. Failing here writes one distinct error per row instead
        // of a raw Io error minutes into a 13 GB pull; freeing space and
        // clicking Download re-runs this check fresh.
        let required: u64 = models
            .iter()
            .map(|(m, on_disk)| m.total_bytes.saturating_sub(*on_disk))
            .sum();
        let available = photoproof_core::runtime::available_disk_bytes(manager.models_dir());
        if let Some((needed, available)) = disk_shortfall(required, available) {
            let mut state = self.state.lock().expect("runtime state");
            let mut rejected = Vec::new();
            for (model, _) in &models {
                // Skip rows already transferring: their bytes were counted
                // as on-disk and stopping them mid-flight helps nothing.
                if state.downloads.contains_key(&model.id) {
                    continue;
                }
                let error = DownloadError::InsufficientSpace {
                    required: needed,
                    available,
                }
                .to_string();
                state
                    .download_errors
                    .insert(model.id.clone(), error.clone());
                rejected.push((model.id.clone(), ulid::Ulid::new().to_string(), error));
            }
            drop(state);
            for (model_id, attempt_id, error) in rejected {
                self.model_registry.publish_operation(
                    &model_id,
                    &attempt_id,
                    "queued",
                    false,
                    None,
                );
                self.model_registry.publish_operation(
                    &model_id,
                    &attempt_id,
                    "failed",
                    true,
                    Some(error),
                );
            }
            return Ok(());
        }
        let (worker_generation, queued_attempts) = {
            let mut state = self.state.lock().expect("runtime state");
            if state.downloads_stopping {
                return Err("application is stopping; download was not queued".into());
            }
            if let Some((model, _)) = models
                .iter()
                .find(|(model, _)| state.unavailable_models.contains(&model.id))
            {
                return Err(format!(
                    "{} is unloading or being removed; Download was not queued",
                    model.id
                ));
            }
            let mut queued_attempts = Vec::new();
            for (model, on_disk) in models {
                if state.downloads.contains_key(&model.id) {
                    continue; // already queued or mid-transfer
                }
                let attempt_id = ulid::Ulid::new().to_string();
                state
                    .downloads
                    .insert(model.id.clone(), (on_disk, model.total_bytes));
                state.download_errors.remove(&model.id);
                // A fresh cancel flag per enqueue: this transfer's Cancel
                // button flips exactly this one (D3).
                state
                    .download_cancels
                    .insert(model.id.clone(), Arc::new(AtomicBool::new(false)));
                state
                    .download_attempts
                    .insert(model.id.clone(), attempt_id.clone());
                queued_attempts.push((model.id.clone(), attempt_id));
                state.download_queue.push_back(model.id);
            }
            let worker_generation = if state.download_worker_live || state.download_queue.is_empty()
            {
                None
            } else {
                state.download_worker_live = true;
                state.download_worker_generation =
                    state.download_worker_generation.saturating_add(1);
                Some(state.download_worker_generation)
            };
            (worker_generation, queued_attempts)
        };
        for (model_id, attempt_id) in &queued_attempts {
            self.model_registry
                .publish_operation(model_id, attempt_id, "queued", false, None);
        }
        if let Some(generation) = worker_generation {
            let host = self.clone();
            let key = format!("model-download-{generation}");
            if let Err(error) =
                self.tasks
                    .spawn("runtime", key, TaskPriority::Background, move |task| {
                        host.drain_download_queue(&task);
                        Ok(())
                    })
            {
                self.settle_downloads_with_verdict(
                    "failed",
                    Some(format!("download worker unavailable: {error}")),
                );
                return Err(format!("download worker unavailable: {error}"));
            }
        }
        Ok(())
    }

    /// The one download worker: pops model ids until the queue is empty,
    /// running each download to completion before the next starts.
    fn drain_download_queue(&self, task: &TaskContext) {
        loop {
            if task.is_cancelled() {
                self.settle_downloads(true);
                return;
            }
            let next = {
                let mut state = self.state.lock().expect("runtime state");
                match state.download_queue.pop_front() {
                    Some(id) => id,
                    None => {
                        state.download_worker_live = false;
                        drop(state);
                        task.report_progress(1.0, "download queue settled");
                        return;
                    }
                }
            };
            if let Some(model) = self.manifest.model(&next).cloned() {
                task.report_progress(0.0, format!("downloading model {}", model.id));
                self.run_download(&model, task);
            }
        }
    }

    /// Enter the one model-operation authority before touching transfer
    /// files or installed.json. Cancel/remove may race between queue pop and
    /// this lock; re-checking the attempt flag after admission makes a
    /// cancelled queued item inert instead of resurrecting it behind removal.
    fn run_download(&self, model: &photoproof_core::runtime::ModelEntry, task: &TaskContext) {
        let _operation = self.model_registry.lock_operation();
        let Some(attempt_id) = self
            .state
            .lock()
            .expect("runtime state")
            .download_attempts
            .get(&model.id)
            .cloned()
        else {
            return;
        };
        if self.model_registry.installed().contains_key(&model.id) {
            // A different attempt may have committed this model after queue
            // admission. Never rewrite files beneath a newly loaded consumer;
            // settle this now-redundant queue row instead.
            let mut state = self.state.lock().expect("runtime state");
            state.downloads.remove(&model.id);
            state.download_retries.remove(&model.id);
            state.download_cancels.remove(&model.id);
            state.download_attempts.remove(&model.id);
            drop(state);
            self.model_registry
                .publish_operation(&model.id, &attempt_id, "installed", true, None);
            return;
        }
        if !self
            .state
            .lock()
            .expect("runtime state")
            .download_cancels
            .contains_key(&model.id)
        {
            return;
        }
        self.run_download_locked(model, &attempt_id, task);
    }

    /// One model, files strictly in sequence (core's loop); called only
    /// from the single queue worker. Interrupted transfers auto-retry on
    /// the [`INTERRUPTED_BACKOFF`] schedule before anything terminal is
    /// surfaced.
    fn run_download_locked(
        &self,
        model: &photoproof_core::runtime::ModelEntry,
        attempt_id: &str,
        task: &TaskContext,
    ) {
        #[cfg(test)]
        self.download_thread_log
            .lock()
            .expect("download thread log")
            .push((model.id.clone(), std::thread::current().id()));
        let (acceptances, cancel) = {
            let state = self.state.lock().expect("runtime state");
            (
                state.acceptances.clone(),
                // D3: THIS transfer's cancel flag (seeded at enqueue).
                // Cloned out so a re-enqueue's fresh flag can never be
                // flipped by a stale cancel of this attempt.
                state
                    .download_cancels
                    .get(&model.id)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        let manager = self.manager();
        let mut pacer = GovernorDownloadPacer {
            capture: SleepPacer::new(self.capture_live.clone()),
            resources: self.tasks.resource_governor(),
            cancel: Arc::clone(&cancel),
            registry: &self.model_registry,
            model_id: &model.id,
            attempt_id,
        };
        let attempt = |pacer: &mut GovernorDownloadPacer| {
            manager.download_model(
                model,
                self.manifest.manifest_version,
                &acceptances,
                pacer,
                &cancel,
                &UtcMillis::now().to_rfc3339(),
            )
        };
        let mut result = attempt(&mut pacer);
        let attempts_total = 1 + INTERRUPTED_BACKOFF.len();
        for (retry, backoff) in INTERRUPTED_BACKOFF.iter().enumerate() {
            // D2: retry the transient classes — a cut/stall (Interrupted)
            // and the "try again later" HTTP statuses (408/425/429/5xx) —
            // through the same schedule. Everything else is a verdict;
            // retrying re-proves a falsehood.
            let server_wait = match &result {
                Err(e) => match retry_wait(e) {
                    Some(w) => w,
                    None => break,
                },
                Ok(_) => break,
            };
            {
                let mut state = self.state.lock().expect("runtime state");
                // The row stays "downloading" — download_errors is written
                // only when the schedule is exhausted, so a single cut
                // never flashes a terminal "failed". The hint names what
                // the worker is actually doing for settings to surface.
                let reason = match &result {
                    Err(DownloadError::Http { status, .. }) => {
                        format!("server answered {status}")
                    }
                    _ => "connection interrupted".into(),
                };
                state.download_retries.insert(
                    model.id.clone(),
                    format!(
                        "{reason}, retrying (attempt {} of {})",
                        retry + 2,
                        attempts_total
                    ),
                );
                // Refresh the displayed bytes from disk: bus events
                // coalesce every 4 MB, so the cut usually lands past the
                // last published number and the part files hold the truth.
                if let Some(slot) = state.downloads.get_mut(&model.id) {
                    slot.0 = manager.downloaded_bytes(model);
                }
            }
            // Backoff in the worker thread — like the pacer, this is the
            // one honest kind of sleep: waiting out network weather is IO
            // pacing, not decision logic, and this dedicated thread has
            // nothing else to do. Sliced so the quit signal (the
            // supervisors' stop latch, flipped once by App::shutdown) is
            // observed within a beat instead of after up to 30 s. A
            // Retry-After from a 429/503 (capped) replaces the schedule's
            // own gap — the server named the honest wait.
            let deadline = std::time::Instant::now() + server_wait.unwrap_or(*backoff);
            loop {
                if task.is_cancelled() || self.supervisors.stopping() {
                    // Quitting: no further attempt, and no error row for a
                    // download that was never given its retries — the part
                    // files keep the progress for the next launch.
                    let mut state = self.state.lock().expect("runtime state");
                    state.downloads.remove(&model.id);
                    state.download_retries.remove(&model.id);
                    state.download_attempts.remove(&model.id);
                    drop(state);
                    self.model_registry.publish_operation(
                        &model.id,
                        attempt_id,
                        "cancelled",
                        true,
                        Some("application is stopping".into()),
                    );
                    return;
                }
                if cancel.load(Ordering::Relaxed) {
                    // D3: cancelled while waiting out the backoff —
                    // cancel_download already cleared this model's rows.
                    return;
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                std::thread::sleep(BACKOFF_SLICE.min(deadline - now));
            }
            // Each retry resumes from the part files (core Ranges from
            // their lengths); nothing already verified moves again.
            result = attempt(&mut pacer);
        }
        if matches!(result, Err(DownloadError::Cancelled)) {
            if task.is_cancelled() {
                let mut state = self.state.lock().expect("runtime state");
                if state
                    .download_cancels
                    .get(&model.id)
                    .is_some_and(|flag| Arc::ptr_eq(flag, &cancel))
                {
                    state.downloads.remove(&model.id);
                    state.download_retries.remove(&model.id);
                    state.download_cancels.remove(&model.id);
                    state.download_attempts.remove(&model.id);
                }
            }
            // D3: user intent, not a failure — no error row, and the maps
            // were already cleared by cancel_download (touching them here
            // could clobber a re-enqueued fresh attempt of the same model).
            return;
        }
        if result.is_ok() {
            if let Some(record) = manager.installed().get(&model.id).cloned() {
                self.model_registry.publish_installed(&model.id, record);
            } else {
                self.model_registry.publish_error(
                    &model.id,
                    "download completed but installed.json has no committed record".into(),
                );
                result = Err(DownloadError::Io(std::io::Error::other(
                    "download completed without an installed-index commit",
                )));
            }
        } else {
            self.model_registry
                .publish_partial_bytes(&model.id, manager.downloaded_bytes(model));
        }
        let mut state = self.state.lock().expect("runtime state");
        match state.download_cancels.get(&model.id) {
            Some(flag) if Arc::ptr_eq(flag, &cancel) => {}
            // A cancel raced in after the verdict (its cleanup already
            // ran), possibly followed by a fresh enqueue whose rows and
            // flag must survive this attempt's cleanup — leave everything.
            _ => return,
        }
        state.downloads.remove(&model.id);
        state.download_retries.remove(&model.id);
        state.download_cancels.remove(&model.id);
        state.download_attempts.remove(&model.id);
        let terminal = match result {
            Ok(_) => ("installed", None),
            Err(DownloadError::LicenseNotAccepted { .. }) => {
                // The raw command normally prevents this, but if acceptance
                // is revoked between enqueue and execution it is still an
                // explicit failed attempt rather than a vanished row.
                ("failed", Some("model license is not accepted".into()))
            }
            Err(e) => {
                let detail = e.to_string();
                state
                    .download_errors
                    .insert(model.id.clone(), detail.clone());
                ("failed", Some(detail))
            }
        };
        drop(state);
        self.model_registry
            .publish_operation(&model.id, attempt_id, terminal.0, true, terminal.1);
    }

    /// Phase-one download shutdown. Flip every transfer flag, forget queued
    /// work, and clear ephemeral rows before the managed-task barrier waits for
    /// the worker to acknowledge. Part files remain resumable and no terminal
    /// error is fabricated for user-requested process exit.
    pub fn begin_download_shutdown(&self) {
        self.settle_downloads(true);
    }

    fn settle_downloads(&self, stopping: bool) {
        self.settle_downloads_with_verdict(
            "cancelled",
            stopping.then(|| "application is stopping".into()),
        );
    }

    fn settle_downloads_with_verdict(&self, phase: &str, error: Option<String>) {
        let mut state = self.state.lock().expect("runtime state");
        state.downloads_stopping |= error.as_deref() == Some("application is stopping");
        for cancel in state.download_cancels.values() {
            cancel.store(true, Ordering::Release);
        }
        let attempts = std::mem::take(&mut state.download_attempts);
        if phase == "failed" {
            for model_id in attempts.keys() {
                state.download_errors.insert(
                    model_id.clone(),
                    error
                        .clone()
                        .unwrap_or_else(|| "download worker failed".into()),
                );
            }
        }
        state.download_queue.clear();
        state.downloads.clear();
        state.download_retries.clear();
        state.download_cancels.clear();
        state.download_worker_live = false;
        drop(state);
        for (model_id, attempt_id) in attempts {
            self.model_registry.publish_operation(
                &model_id,
                &attempt_id,
                phase,
                true,
                error.clone(),
            );
        }
    }

    /// D3: Settings → Cancel a queued or in-flight download. The flag is
    /// observed per chunk and between files, so the worker stops within a
    /// beat; part files are KEPT (a later Download resumes from them), no
    /// error row is written, and the row retains one explicit `cancelled`
    /// terminal operation until a later attempt replaces it.
    pub fn cancel_download(&self, model_id: &str) {
        let attempt_id = {
            let mut state = self.state.lock().expect("runtime state");
            if let Some(flag) = state.download_cancels.remove(model_id) {
                flag.store(true, Ordering::Relaxed);
            }
            // Queued but not started: drop it before the worker gets there.
            state.download_queue.retain(|id| id != model_id);
            // Clear the live rows now rather than when the worker notices —
            // the status snapshot the cancel command returns must already
            // read "not-downloaded".
            state.downloads.remove(model_id);
            state.download_retries.remove(model_id);
            state.download_attempts.remove(model_id)
        };
        if let Some(model) = self.manifest.model(model_id) {
            self.model_registry
                .publish_partial_bytes(model_id, self.manager().downloaded_bytes(model));
        }
        if let Some(attempt_id) = attempt_id {
            self.model_registry
                .publish_operation(model_id, &attempt_id, "cancelled", true, None);
        }
    }

    /// Re-hash a complete model against the immutable manifest. A valid,
    /// previously unindexed directory is adopted into installed.json under
    /// the same serialized operation authority used by download/remove.
    pub fn verify_model(&self, model_id: &str) -> Result<(), String> {
        self.require_model_mutation_authority()?;
        let Some(model) = self.manifest.model(model_id) else {
            return Err(format!("unknown model {model_id:?}"));
        };
        let _operation = self.model_registry.lock_operation();
        if self
            .state
            .lock()
            .expect("runtime state")
            .downloads
            .contains_key(model_id)
        {
            return Err(format!(
                "{model_id} is queued or downloading; cancel it before Verify"
            ));
        }
        self.model_registry
            .set_operation(model_id, Some("verifying"));
        let manager = self.manager();
        let result = manager
            .verify_model(model, &AtomicBool::new(false))
            .map_err(|error| error.to_string())
            .and_then(|()| {
                let record = self
                    .model_registry
                    .installed()
                    .get(model_id)
                    .cloned()
                    .unwrap_or_else(|| photoproof_core::runtime::InstalledRecord {
                        manifest_version: self.manifest.manifest_version,
                        when: UtcMillis::now().to_rfc3339(),
                    });
                self.model_registry
                    .commit_installed_locked(self.bus.clone(), model_id, record)
            });
        if let Err(error) = &result {
            self.model_registry
                .publish_error(model_id, format!("Verify failed: {error}"));
        }
        self.model_registry.set_operation(model_id, None);
        result
    }

    /// Reclaim only resumable partial bytes. Cancellation is a pause and keeps
    /// parts; this explicit operation is the destructive counterpart the UI
    /// can offer after a terminal failure.
    pub fn discard_partial(&self, model_id: &str) -> Result<u64, String> {
        self.require_model_mutation_authority()?;
        let Some(model) = self.manifest.model(model_id) else {
            return Err(format!("unknown model {model_id:?}"));
        };
        self.cancel_download(model_id);
        let _operation = self.model_registry.lock_operation();
        // Catch an enqueue that won the gate after the optimistic cancellation
        // but before this destructive operation was admitted.
        self.cancel_download(model_id);
        self.model_registry
            .set_operation(model_id, Some("discarding-partial"));
        let result = self
            .manager()
            .discard_partial(model)
            .map_err(|error| error.to_string());
        if result.is_ok() {
            self.model_registry.publish_partial_bytes(model_id, 0);
            let mut state = self.state.lock().expect("runtime state");
            state.download_errors.remove(model_id);
            state.download_retries.remove(model_id);
        }
        self.model_registry.set_operation(model_id, None);
        result
    }

    /// Settings → remove a model's weights (§2.4). Installed records and
    /// files go together; tier offers are unaffected (§6.2: selection
    /// never deletes — but the user's explicit remove does).
    pub fn remove_model(&self, model_id: &str) -> Result<(), String> {
        if self.manifest.model(model_id).is_none() {
            return Err(format!("unknown model {model_id:?}"));
        }
        self.retire_model(model_id, "removing", "removal")
            .map(|_| ())
    }

    /// Destructive half of automatic model GC, called only after the
    /// scheduler/reindex coordinator has granted its verified-ready,
    /// reindex-complete, idle permit. Keeping this internal avoids inventing a
    /// user-visible GC policy while guaranteeing that a future production
    /// caller cannot bypass consumer drain or the installed-index authority.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn gc_model_after_scheduler_approval(&self, model_id: &str) -> Result<u64, String> {
        if !self.model_registry.installed().contains_key(model_id) {
            return Err(format!(
                "{model_id:?} is not a committed installed model; GC did not delete anything"
            ));
        }
        self.retire_model(model_id, "garbage-collecting", "garbage collection")
    }

    fn retire_model(
        &self,
        model_id: &str,
        operation: &'static str,
        action: &'static str,
    ) -> Result<u64, String> {
        self.require_model_mutation_authority()?;
        // Cancel before waiting on the operation gate so an in-flight
        // transfer observes its flag and releases the gate promptly. A queued
        // transfer is removed before it can enter.
        self.cancel_download(model_id);
        let _operation = self.model_registry.lock_operation();

        // Hide it from the derived plan while installed.json still truthfully
        // records the files. Convergence drops ready embedders immediately and
        // invalidates queued attempts, kills and reaps building helpers, and
        // asks supervised children to drain/reap.
        {
            self.state
                .lock()
                .expect("runtime state")
                .unavailable_models
                .insert(model_id.to_owned());
        }
        // Catch a queue admission that raced the first cancellation while
        // removal was waiting for an in-flight transfer to release the gate.
        // Later enqueue attempts observe `unavailable_models` and fail closed.
        self.cancel_download(model_id);
        self.model_registry.set_operation(model_id, Some(operation));
        self.embedders.cancel_model(model_id);
        self.apply_supervisor_plan_locked();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.embedders.model_in_use(model_id) || self.supervisors.model_in_use(model_id) {
            if std::time::Instant::now() >= deadline {
                self.state
                    .lock()
                    .expect("runtime state")
                    .unavailable_models
                    .remove(model_id);
                self.model_registry.set_operation(model_id, None);
                self.apply_supervisor_plan_locked();
                return Err(format!(
                    "{model_id} is still in use; {action} stopped without deleting its files"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let manager = self.manager();
        let dir = manager.models_dir().join(model_id);
        let bytes_reclaimed = self.model_registry.model_bytes(model_id);
        if dir.exists()
            && let Err(error) = std::fs::remove_dir_all(&dir)
        {
            self.state
                .lock()
                .expect("runtime state")
                .unavailable_models
                .remove(model_id);
            self.model_registry.set_operation(model_id, None);
            self.apply_supervisor_plan_locked();
            return Err(error.to_string());
        }
        if let Err(error) = self
            .model_registry
            .commit_removed_locked(self.bus.clone(), model_id)
        {
            // Files are already gone, so re-enabling the stale in-memory
            // record would invite a load from nothing. Keep it unavailable
            // and surface the durable disagreement for recovery/Verify.
            let detail = format!(
                "model files were removed but installed.json could not commit removal: {error}"
            );
            self.model_registry.publish_error(model_id, detail.clone());
            self.model_registry.set_operation(model_id, None);
            return Err(detail);
        }
        {
            let mut state = self.state.lock().expect("runtime state");
            state.unavailable_models.remove(model_id);
            state.download_errors.remove(model_id);
            state.download_retries.remove(model_id);
        }
        self.model_registry.set_operation(model_id, None);
        Ok(bytes_reclaimed)
    }

    /// Settings → "restart runtime": supervised ASR/LLM roles re-enter
    /// Spawning with fresh attempt budgets, failed embedder helpers return
    /// to Idle so the current plan rebuilds them once, and surfaced download
    /// failures clear. Ready/building embedders and plan-dark roles are left
    /// alone.
    pub fn restart_runtime(&self) {
        {
            let mut state = self.state.lock().expect("runtime state");
            state.download_errors.clear();
        }
        self.supervisors.restart_runtime();
        self.embedders.retry_failed();
        self.apply_supervisor_plan();
    }

    /// Mark the capability report as actively validating. This is a tiny
    /// in-memory transition used before dispatching the managed probe; the
    /// driver call itself never runs on setup or an IPC command thread.
    pub fn begin_capability_detection(&self) {
        let mut state = self.state.lock().expect("runtime state");
        state.capability_phase = CapabilityPhase::Detecting;
        state.capability_summary = Some("detecting hardware capabilities in the background".into());
    }

    /// Probe hardware and atomically adopt the fresh report and tier decision.
    /// The expensive/driver-facing work and tier-cache fsync happen without
    /// the main runtime state mutex, so status and model commands remain
    /// responsive throughout detection.
    pub fn detect_capabilities(&self, cancel: Arc<AtomicBool>) -> Result<RuntimeStatus, String> {
        let live = LiveProbe::probe_bounded(cancel, std::time::Duration::from_secs(20)).map_err(
            |error| {
                let detail = error.to_string();
                self.fail_capability_detection(detail.clone());
                detail
            },
        )?;
        Ok(self.adopt_capabilities(live.hardware, live.providers, live.total_memory_bytes))
    }

    #[cfg(test)]
    fn adopt_hardware_report(&self, report: HardwareReport) -> RuntimeStatus {
        self.adopt_capabilities(
            report,
            photoproof_connectors::ort_provider_capabilities(),
            None,
        )
    }

    fn adopt_capabilities(
        &self,
        report: HardwareReport,
        providers: Vec<photoproof_connectors::OrtProviderCapability>,
        total_memory_bytes: Option<u64>,
    ) -> RuntimeStatus {
        let config_tier = self
            .state
            .lock()
            .expect("runtime state")
            .config
            .runtime
            .tier;
        let mut fixed_probe = FixedHardwareProbe(Some(report.clone()));
        let resolved = resolve_tier_checked(
            &mut fixed_probe,
            config_tier,
            &tier_path(&self.app_data),
            true,
        );
        let issue_count = resolved.issues.len();
        {
            let mut state = self.state.lock().expect("runtime state");
            state.control_files.insert(
                "tier".into(),
                control_status("tier", resolved.recovery, resolved.issues, Vec::new()),
            );
            state.tier = resolved.decision;
            state.hardware_report = Some(report.clone());
            state.capabilities =
                Some(self.build_capabilities(&report, providers, total_memory_bytes));
            state.capability_phase = CapabilityPhase::Ready;
            state.capability_summary = Some(if issue_count == 0 {
                format!(
                    "detected {} hardware adapter{}",
                    report.adapters.len(),
                    if report.adapters.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "hardware detected, but {issue_count} tier-cache durability issue{} occurred",
                    if issue_count == 1 { "" } else { "s" }
                )
            });
        }
        self.status()
    }

    fn build_capabilities(
        &self,
        report: &HardwareReport,
        providers: Vec<photoproof_connectors::OrtProviderCapability>,
        total_memory_bytes: Option<u64>,
    ) -> RuntimeCapabilities {
        let available = |name: &str| {
            providers.iter().any(|provider| {
                provider.provider == name
                    && provider.compiled
                    && provider.runtime_available == Some(true)
            })
        };
        let has_metal_adapter = report.adapters.iter().any(|adapter| {
            adapter.backend.eq_ignore_ascii_case("metal")
                || adapter.vendor_id == Some(0x106b)
                || adapter.name.to_ascii_lowercase().contains("apple")
        });
        let has_nvidia_adapter = report.adapters.iter().any(|adapter| {
            adapter.vendor_id == Some(0x10de)
                || adapter.name.to_ascii_lowercase().contains("nvidia")
        });
        let model_compatibility = self
            .manifest
            .models
            .iter()
            .map(|model| {
                let accelerated_export = model.id.ends_with("-fp16");
                let mut compatible_providers = if accelerated_export {
                    Vec::new()
                } else {
                    vec!["CPU".to_owned()]
                };
                if accelerated_export && has_metal_adapter && available("CoreML") {
                    compatible_providers.push("CoreML".into());
                }
                if accelerated_export && has_nvidia_adapter && available("CUDA") {
                    compatible_providers.push("CUDA".into());
                }
                if accelerated_export && has_nvidia_adapter && available("TensorRT") {
                    compatible_providers.push("TensorRT".into());
                }
                // Every public/pinned model retains a CPU path. Advanced
                // accelerator-only exports remain outside default tiers until
                // a measured compatibility rule explicitly graduates them.
                RuntimeModelCompatibility {
                    model_id: model.id.clone(),
                    compatible: !compatible_providers.is_empty(),
                    reason: if accelerated_export && compatible_providers.is_empty() {
                        "accelerator export requires a matching Metal/CoreML or NVIDIA/CUDA/TensorRT adapter and runtime"
                            .into()
                    } else if accelerated_export {
                        format!(
                            "matched adapter and runtime provider: {}",
                            compatible_providers.join(", ")
                        )
                    } else {
                        "CPU-compatible model".into()
                    },
                    compatible_providers,
                }
            })
            .collect();
        let fingerprint_source = report
            .adapters
            .iter()
            .map(|adapter| {
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    adapter.vendor_id.unwrap_or_default(),
                    adapter.device_id.unwrap_or_default(),
                    adapter.name,
                    adapter.backend,
                    adapter.driver.as_deref().unwrap_or(""),
                    adapter.driver_info.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        RuntimeCapabilities {
            report_schema_version: report.schema_version,
            detected_at: report.detected_at.clone(),
            hardware_fingerprint: blake3::hash(fingerprint_source.as_bytes())
                .to_hex()
                .to_string(),
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            total_memory_bytes,
            apple_unified_bytes: report.apple_unified_bytes,
            adapters: report
                .adapters
                .iter()
                .map(|adapter| RuntimeAdapterStatus {
                    name: adapter.name.clone(),
                    backend: adapter.backend.clone(),
                    vendor_id: adapter.vendor_id,
                    device_id: adapter.device_id,
                    driver: adapter.driver.clone(),
                    driver_info: adapter.driver_info.clone(),
                    vram_bytes: adapter.vram_bytes,
                })
                .collect(),
            runtime_library_available: providers
                .iter()
                .filter(|provider| provider.compiled)
                .any(|provider| provider.runtime_available.is_some()),
            providers,
            model_compatibility,
        }
    }

    pub fn fail_capability_detection(&self, detail: String) {
        let mut state = self.state.lock().expect("runtime state");
        state.capability_phase = CapabilityPhase::Failed;
        state.capability_summary = Some(detail);
    }

    /// Live download progress for the pump's snapshot events. The bus
    /// speaks model-cumulative bytes over the manifest total (bus.rs),
    /// which is exactly what the settings row divides — stored verbatim.
    pub fn note_progress(&self, model_id: &str, downloaded: u64, total: u64) {
        let mut state = self.state.lock().expect("runtime state");
        if let Some(slot) = state.downloads.get_mut(model_id) {
            *slot = (downloaded, total);
        }
    }
}

/// D3: Settings → Cancel a queued or in-flight model download. Lives here
/// beside the host it drives (the other runtime_* commands predate this
/// file's split and sit in commands/app.rs); registered in lib.rs's
/// generate_handler! lists like the rest.
#[tauri::command]
pub fn runtime_cancel_download(
    app: tauri::State<'_, Arc<crate::state::App>>,
    handle: tauri::AppHandle,
    model_id: String,
) -> crate::error::CmdResult<crate::dto::RuntimeStatus> {
    let app = app.inner().clone();
    let _permit = crate::commands::admit(
        &app,
        "runtime.cancel-download",
        crate::command_work::CommandClass::Mutation,
    )?;
    app.runtime.cancel_download(&model_id);
    // Push the fresh snapshot on the same channel the pump uses so every
    // webview sees the row flip back to not-downloaded at once (the bus
    // publishes nothing for a cancel, so nobody else would emit).
    let status = app.runtime.status();
    let _ = crate::pump::emit_runtime_status(&handle, status.clone());
    app.convergence
        .publish(&handle, [crate::convergence::StateDomain::Runtime]);
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_pacer_suspends_an_in_flight_transport_chunk_until_resume() {
        let resources = Arc::new(crate::resource_governor::ResourceGovernor::new(
            crate::settings::ProcessingIntensity::Eco,
            true,
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_resources = Arc::clone(&resources);
        let worker_cancel = Arc::clone(&cancel);
        let temp = tempfile::tempdir().unwrap();
        let host = Arc::new(RuntimeHost::init(temp.path().join("app")));
        let worker_host = Arc::clone(&host);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut pacer = GovernorDownloadPacer {
                capture: SleepPacer::new(Arc::new(AtomicBool::new(false))),
                resources: Some(worker_resources),
                cancel: worker_cancel,
                registry: &worker_host.model_registry,
                model_id: "fixture-model",
                attempt_id: "fixture-attempt",
            };
            pacer.pace(64 * 1024);
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "the transport loop must stop advancing while manually paused"
        );
        resources.configure(crate::settings::ProcessingIntensity::Eco, false);
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }

    /// Hosts under test pin `[runtime] tier = 0` via config — the
    /// override always wins (§6.2), making these assertions independent
    /// of whatever GPU the gate machine actually has (the founder box
    /// carries an RTX 5080; CI may carry nothing).
    fn host() -> (tempfile::TempDir, Arc<RuntimeHost>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        (dir, host)
    }

    fn settle_capabilities(host: &RuntimeHost) {
        let _ = host.adopt_hardware_report(HardwareReport {
            schema_version: 1,
            adapters: Vec::new(),
            apple_unified_bytes: None,
            detected_at: "2026-07-27T00:00:00Z".into(),
        });
    }

    /// Tier 0 (forced by the always-winning override): nothing offered,
    /// nothing ready, the consent sum is zero, and the lock is held.
    #[test]
    fn tier_0_offers_nothing_and_spawns_nothing() {
        let (_dir, host) = host();
        let status = host.status();
        assert_eq!(status.tier_effective, 0);
        assert!(!status.asr_ready);
        assert!(!status.llm_ready);
        // Dark by PLAN (tier 0 runs nothing) is not "blocked": the
        // missing-binary surfacing must stay quiet here.
        assert!(status.asr_blocked.is_none());
        assert!(status.llm_blocked.is_none());
        assert_eq!(status.consent, "undecided");
        assert_eq!(status.consent_offer_bytes, 0, "§5.4 live sum at tier 0");
        assert!(status.instance_lock_held);
        assert!(
            status.models.iter().all(|m| m.state == "not-offered"),
            "tier 0 offers nothing: {:?}",
            status.models.iter().map(|m| &m.state).collect::<Vec<_>>()
        );
        assert!(host.plan().spawns_nothing(), "Tier-0-whole: zero children");
    }

    #[test]
    fn init_is_provisional_and_never_requires_a_live_hardware_probe() {
        let dir = tempfile::tempdir().unwrap();
        let host = RuntimeHost::init(dir.path().to_path_buf());
        let status = host.status();

        assert_eq!(status.capability_state, "provisional");
        assert_eq!(
            status.tier_effective, 0,
            "a cache miss launches on the conservative journal-only floor"
        );
        assert!(
            status.capability_adapters.is_empty(),
            "empty means not detected yet, not a completed no-GPU verdict"
        );
        assert!(
            !tier_path(dir.path()).exists(),
            "init must not probe and materialize a tier cache"
        );
    }

    #[test]
    fn capability_detection_atomically_moves_provisional_to_detecting_to_ready() {
        let dir = tempfile::tempdir().unwrap();
        let host = RuntimeHost::init(dir.path().to_path_buf());
        host.begin_capability_detection();
        let detecting = host.status();
        assert_eq!(detecting.capability_state, "detecting");
        assert_eq!(detecting.tier_effective, 0);

        let report = HardwareReport {
            schema_version: 1,
            adapters: vec![photoproof_core::runtime::GpuAdapter {
                name: "Test accelerator".into(),
                backend: "Vulkan".into(),
                vram_bytes: Some(16 * 1024 * 1024 * 1024),
                vendor_id: None,
                device_id: None,
                driver: None,
                driver_info: None,
            }],
            apple_unified_bytes: None,
            detected_at: "2026-07-27T00:00:00Z".into(),
        };
        let ready = host.adopt_hardware_report(report.clone());
        assert_eq!(ready.capability_state, "ready");
        assert_eq!(ready.tier_detected, 2);
        assert_eq!(ready.tier_effective, 2);
        assert_eq!(ready.capability_adapters.len(), 1);
        assert_eq!(
            ready.capability_detected_at.as_deref(),
            Some("2026-07-27T00:00:00Z")
        );
        assert_eq!(
            TierCache::load(&tier_path(dir.path())).unwrap().report,
            report,
            "the final report is durable before it becomes published state"
        );
    }

    #[test]
    fn model_compatibility_requires_matching_adapter_and_compiled_runtime_provider() {
        use photoproof_connectors::OrtProviderCapability;
        use photoproof_core::runtime::GpuAdapter;

        let (_dir, host) = host();
        let report = |adapters: Vec<GpuAdapter>| HardwareReport {
            schema_version: 1,
            adapters,
            apple_unified_bytes: None,
            detected_at: "2026-07-27T00:00:00Z".into(),
        };
        let provider = |name: &str, compiled: bool, ready: bool| OrtProviderCapability {
            provider: name.into(),
            compiled,
            runtime_available: Some(ready),
            error: None,
        };
        let adapter = |name: &str, backend: &str, vendor_id: Option<u32>| GpuAdapter {
            name: name.into(),
            backend: backend.into(),
            vendor_id,
            device_id: Some(1),
            driver: Some("fixture".into()),
            driver_info: Some("1.0".into()),
            vram_bytes: Some(16 * 1024 * 1024 * 1024),
        };
        fn compatibility<'a>(
            capabilities: &'a RuntimeCapabilities,
            id: &str,
        ) -> &'a RuntimeModelCompatibility {
            capabilities
                .model_compatibility
                .iter()
                .find(|row| row.model_id == id)
                .unwrap()
        }
        let fp16 = "ViT-H-14-378-quickgelu__dfn5b-fp16";
        let int8 = "ViT-H-14-378-quickgelu__dfn5b";

        let cpu =
            host.build_capabilities(&report(Vec::new()), vec![provider("CPU", true, true)], None);
        assert!(!compatibility(&cpu, fp16).compatible);
        assert!(compatibility(&cpu, int8).compatible);

        let metal = host.build_capabilities(
            &report(vec![adapter("Apple M4", "Metal", Some(0x106b))]),
            vec![provider("CPU", true, true), provider("CoreML", true, true)],
            None,
        );
        assert_eq!(compatibility(&metal, fp16).compatible_providers, ["CoreML"]);

        let nvidia = host.build_capabilities(
            &report(vec![adapter("NVIDIA RTX", "Vulkan", Some(0x10de))]),
            vec![
                provider("CPU", true, true),
                provider("CUDA", true, true),
                provider("TensorRT", true, true),
            ],
            None,
        );
        assert_eq!(
            compatibility(&nvidia, fp16).compatible_providers,
            ["CUDA", "TensorRT"]
        );

        let amd_with_cuda_library = host.build_capabilities(
            &report(vec![adapter("AMD Radeon", "Vulkan", Some(0x1002))]),
            vec![provider("CPU", true, true), provider("CUDA", true, true)],
            None,
        );
        assert!(
            !compatibility(&amd_with_cuda_library, fp16).compatible,
            "a library export without a matching adapter is not compatibility"
        );

        let uncompiled_cuda = host.build_capabilities(
            &report(vec![adapter("NVIDIA RTX", "Vulkan", Some(0x10de))]),
            vec![provider("CPU", true, true), provider("CUDA", false, true)],
            None,
        );
        assert!(!compatibility(&uncompiled_cuda, fp16).compatible);
    }

    #[test]
    fn forced_or_cached_tier_cannot_authorize_a_provisional_plan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 2\n").unwrap();
        let host = RuntimeHost::init(dir.path().to_path_buf());
        assert_eq!(
            host.plan().effective_tier,
            0,
            "this launch stays dark until authoritative discovery"
        );

        let report = HardwareReport {
            schema_version: 1,
            adapters: vec![photoproof_core::runtime::GpuAdapter {
                name: "Test accelerator".into(),
                backend: "Vulkan".into(),
                vendor_id: Some(0x10de),
                device_id: Some(0x2c02),
                driver: Some("test".into()),
                driver_info: Some("1.0".into()),
                vram_bytes: Some(16 * 1024 * 1024 * 1024),
            }],
            apple_unified_bytes: None,
            detected_at: "2026-07-27T00:00:00Z".into(),
        };
        let ready = host.adopt_hardware_report(report);
        assert!(ready.capabilities.is_some());
        assert_eq!(host.plan().effective_tier, 2);
    }

    #[test]
    fn failed_detection_keeps_the_provisional_safe_tier_visible() {
        let dir = tempfile::tempdir().unwrap();
        let host = RuntimeHost::init(dir.path().to_path_buf());
        host.begin_capability_detection();
        host.fail_capability_detection("driver probe failed".into());

        let status = host.status();
        assert_eq!(status.capability_state, "failed");
        assert_eq!(status.tier_effective, 0);
        assert_eq!(
            status.capability_summary.as_deref(),
            Some("driver probe failed")
        );
    }

    #[test]
    fn consent_is_remembered_across_hosts_and_never_stays_never() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        {
            let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
            host.set_consent("never").unwrap();
        }
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        assert_eq!(host.status().consent, "never", "§10.3: Never is remembered");
    }

    #[test]
    fn corrupt_consent_is_quarantined_and_recovers_last_known_decision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        {
            let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
            host.set_consent("never").unwrap();
        }
        std::fs::write(dir.path().join("runtime/consent"), b"truncated-value").unwrap();

        let host = RuntimeHost::init(dir.path().to_path_buf());
        let status = host.status();
        assert_eq!(status.consent, "never");
        let consent = status
            .control_files
            .iter()
            .find(|entry| entry.name == "consent")
            .unwrap();
        assert_eq!(
            consent.recovery.as_ref().unwrap().source,
            ControlFileSource::LastKnownGood
        );
        assert_eq!(consent.recovery.as_ref().unwrap().quarantined.len(), 1);
    }

    #[test]
    fn corrupt_config_recovers_lkg_and_relative_models_dir_uses_app_data_base() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[runtime]\ntier = 0\nmodels_dir = \"nested/models\"\n",
        )
        .unwrap();
        {
            let host = RuntimeHost::init(dir.path().to_path_buf());
            assert_eq!(host.status().tier_effective, 0);
            assert!(dir.path().join("nested/models/manifest.json").exists());
        }
        std::fs::write(&config_path, b"[runtime").unwrap();

        let host = RuntimeHost::init(dir.path().to_path_buf());
        let status = host.status();
        assert_eq!(status.tier_effective, 0);
        let config = status
            .control_files
            .iter()
            .find(|entry| entry.name == "config")
            .unwrap();
        assert_eq!(
            config.recovery.as_ref().unwrap().source,
            ControlFileSource::LastKnownGood
        );
        assert!(dir.path().join("nested/models/manifest.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_consent_write_does_not_commit_memory() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, host) = host();
        let runtime_dir = dir.path().join("runtime");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = host.set_consent("never");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        let status = host.status();
        assert_eq!(status.consent, "undecided");
        let consent = status
            .control_files
            .iter()
            .find(|entry| entry.name == "consent")
            .unwrap();
        assert_eq!(
            consent.errors[0].kind,
            ControlFileErrorKind::PermissionDenied
        );
    }

    #[test]
    fn committed_download_consent_reports_retryable_dispatch_failure() {
        let (dir, host) = host();
        settle_capabilities(&host);
        host.begin_download_shutdown();

        let commit = host.set_consent("download").unwrap();

        assert_eq!(
            commit.operation_error.as_deref(),
            Some("application is stopping; download was not queued")
        );
        assert_eq!(host.status().consent, "download");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("runtime/consent")).unwrap(),
            "download",
            "the dispatch failure must not disguise the already durable choice"
        );
    }

    #[test]
    fn license_acceptance_is_recorded_with_url_and_timestamp_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        {
            let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
            host.accept_license("gemma-4-e4b-it-q4_k_m").unwrap();
            assert!(host.accept_license("no-such-model").is_err());
        }
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        let status = host.status();
        let gemma = status
            .models
            .iter()
            .find(|m| m.id == "gemma-4-e4b-it-q4_k_m")
            .unwrap();
        assert!(gemma.accepted, "§5.3: acceptance recorded + persisted");
        assert!(gemma.acceptance_required);
        let on_disk = Acceptances::load(&dir.path().join("runtime/acceptances.json"));
        let rec = &on_disk.accepted["gemma-4-e4b-it-q4_k_m"];
        assert_eq!(rec.license_url, "https://ai.google.dev/gemma/terms");
        assert!(rec.at.starts_with("20"), "RFC 3339 timestamp");
    }

    #[cfg(unix)]
    #[test]
    fn failed_acceptance_write_does_not_commit_memory() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, host) = host();
        let runtime_dir = dir.path().join("runtime");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = host.accept_license("gemma-4-e4b-it-q4_k_m");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        let status = host.status();
        let gemma = status
            .models
            .iter()
            .find(|model| model.id == "gemma-4-e4b-it-q4_k_m")
            .unwrap();
        assert!(!gemma.accepted, "memory changes only after durable commit");
        let acceptances = status
            .control_files
            .iter()
            .find(|entry| entry.name == "acceptances")
            .unwrap();
        assert_eq!(
            acceptances.errors[0].kind,
            ControlFileErrorKind::PermissionDenied
        );
    }

    #[test]
    fn manifest_json_lands_in_models_dir_for_the_debug_panel() {
        let (dir, _host) = host();
        assert!(dir.path().join("models/manifest.json").exists(), "§5.1");
    }

    #[test]
    fn second_host_on_the_same_app_data_does_not_hold_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        let first = RuntimeHost::init(dir.path().to_path_buf());
        let second = RuntimeHost::init(dir.path().to_path_buf());
        assert!(first.status().instance_lock_held);
        assert!(
            !second.status().instance_lock_held,
            "§8.5: one instance holds the lock; supervisors refuse without it"
        );
    }

    /// §8.4/§8.5: the orphan sweep is the LOCK HOLDER's crash net. A host
    /// that lost the lock race must not run it — the first instance's
    /// healthy supervised children (alive PID + matching start-time) are
    /// exactly what the sweep kills. The recorded child here is this very
    /// test process with its CORRECT start-time: if the lockless host
    /// swept, it would SIGKILL us.
    #[test]
    fn orphan_sweep_does_not_run_without_the_instance_lock() {
        use photoproof_core::runtime::{ChildRecord, process_start_time};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        let first = RuntimeHost::init(dir.path().to_path_buf());
        assert!(first.status().instance_lock_held);
        // Simulate the first instance's live supervised child…
        let reg = ChildRegistry::new(&dir.path().join("runtime")).unwrap();
        let my_pid = std::process::id();
        reg.record(ChildRecord {
            process: "llm".into(),
            pid: my_pid,
            start_time: process_start_time(my_pid),
            port: 4000,
        })
        .unwrap();
        // …then a second host that loses the lock race.
        let second = RuntimeHost::init(dir.path().to_path_buf());
        assert!(!second.status().instance_lock_held);
        assert_eq!(
            reg.list().len(),
            1,
            "no lock ⇒ no sweep: children.json untouched (we are also \
             alive to assert this — a sweep would have SIGKILLed us)"
        );
        assert!(
            second
                .debug_lines()
                .join("\n")
                .contains("orphan sweep: NOT RUN"),
            "the skip is named for the debug panel"
        );
    }

    /// §5.2/A15: consent enqueues exactly one backend-selected default per
    /// local functional seam through one manager-wide worker. Compatible
    /// alternatives remain explicit Settings choices and never ride the
    /// first-run decision.
    /// (Every transfer fails fast here — no network in tests — which is
    /// exactly why thread identity is the observable: the old fan-out ran
    /// four threads regardless.)
    #[test]
    fn consent_download_drains_only_backend_defaults_on_one_worker_thread() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        settle_capabilities(&host);
        let defaults: Vec<String> = host
            .manifest
            .models
            .iter()
            .filter_map(|model| {
                host.status()
                    .models
                    .iter()
                    .find(|row| row.id == model.id && row.default_offer)
                    .map(|row| row.id.clone())
            })
            .collect();
        assert_eq!(
            defaults.len(),
            4,
            "one LLM, ASR, CLIP, and text-embedding default"
        );
        assert!(
            host.status()
                .models
                .iter()
                .any(|row| row.advanced_available && !row.default_offer),
            "compatible alternatives remain explicit"
        );

        host.set_consent("download").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            {
                let state = host.state.lock().unwrap();
                if state.downloads.is_empty()
                    && state.download_queue.is_empty()
                    && !state.download_worker_live
                {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "download queue should drain (every transfer fails fast)"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let log = host.download_thread_log.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            defaults,
            "only backend-selected defaults ran, in manifest order"
        );
        let worker = log[0].1;
        assert!(
            log.iter().all(|(_, t)| *t == worker),
            "ONE worker thread ran them all — same thread ⇒ strictly \
             sequential, never four concurrent transfers"
        );
        assert_ne!(
            worker,
            std::thread::current().id(),
            "…and it is the background worker, not the caller"
        );
        drop(log);
        assert!(host.tasks.wait_for_idle(std::time::Duration::from_secs(1)));
        let download_task = host
            .tasks
            .snapshots()
            .into_iter()
            .find(|task| task.owner == "runtime" && task.key.starts_with("model-download-"))
            .expect("the transfer is process-owned and observable");
        assert_eq!(
            download_task.state,
            crate::managed_tasks::TaskState::Completed
        );
    }

    #[test]
    fn staged_or_unknown_configured_clip_falls_back_to_hosted_default_offer() {
        let (dir, host) = host();
        settle_capabilities(&host);
        let capabilities = host.status().capabilities.unwrap();
        let mut config = Config::default();
        for configured in ["ViT-H-14-378-quickgelu__dfn5b-fp16", "unknown-clip-export"] {
            config.embedder.model = configured.into();
            let selected =
                selected_default_offer_ids(&config, &host.manifest, 1, Some(&capabilities));
            assert!(
                selected.contains("ViT-H-14-378-quickgelu__dfn5b"),
                "{configured}: a stale/unsupported config must still offer the hosted compatible default"
            );
            assert!(!selected.contains(configured));
        }
        drop(dir);
    }

    /// B73/D8: public offers are immutably pinned. The tier-1 embedders
    /// (DFN5B + EmbeddingGemma default) render as downloadable and the
    /// explicit command accepts them. Staged-only entries may remain in the
    /// manifest, but are unpinned and offered at no tier.
    #[test]
    fn b73_embedders_are_pinned_offered_and_downloadable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        settle_capabilities(&host);
        assert!(
            host.manifest.offered_at(1).iter().all(|m| m.is_pinned()),
            "every public tier-1 offer is immutably pinned"
        );
        let status = host.status();
        assert!(
            status
                .models
                .iter()
                .filter(|row| {
                    host.manifest
                        .offered_at(1)
                        .iter()
                        .any(|entry| entry.id == row.id)
                })
                .all(|m| m.state != "unpinned"),
            "no offered row surfaces the pending-unpinned state"
        );

        // The text-embedder default + the CLIP embedder are offered at the
        // tier-1 floor: downloadable now, not pending.
        for id in ["ViT-H-14-378-quickgelu__dfn5b", "embeddinggemma-300m-q8"] {
            let row = status.models.iter().find(|m| m.id == id).unwrap();
            assert_eq!(
                row.state, "not-downloaded",
                "{id} is offered + downloadable"
            );
            assert!(row.error.is_none());
            assert!(
                host.download_model(id).is_ok(),
                "{id}: explicit download is accepted at the seam"
            );
        }
    }

    #[test]
    fn installed_model_cannot_be_redownloaded_beneath_a_live_consumer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        settle_capabilities(&host);
        let model_id = "embeddinggemma-300m-q8".to_owned();
        {
            let _operation = host.model_registry.lock_operation();
            host.model_registry
                .commit_installed_locked(
                    host.bus.clone(),
                    &model_id,
                    photoproof_core::runtime::InstalledRecord {
                        manifest_version: host.manifest.manifest_version,
                        when: "2026-07-27T00:00:00Z".into(),
                    },
                )
                .unwrap();
        }

        let error = host.download_model(&model_id).unwrap_err();
        assert!(error.contains("already installed"), "{error}");
        assert!(
            host.state.lock().unwrap().downloads.is_empty(),
            "the rejected rewrite must not leave queued work"
        );
    }

    #[test]
    fn selecting_an_installed_alternative_persists_and_replans_its_seam() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\ntier = 2\nvram_headroom_mb = 4096\n\n[llm]\nmodel = \"gemma-4-e2b-it-qat-q4_0\"\n",
        )
        .unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        settle_capabilities(&host);
        let model_id = "gemma-4-e4b-it-q4_k_m";
        {
            let _operation = host.model_registry.lock_operation();
            host.model_registry
                .commit_installed_locked(
                    host.bus.clone(),
                    model_id,
                    photoproof_core::runtime::InstalledRecord {
                        manifest_version: host.manifest.manifest_version,
                        when: "2026-07-27T00:00:00Z".into(),
                    },
                )
                .unwrap();
        }

        let changed = host.select_model(model_id).unwrap();
        assert!(changed.changed);
        let persisted =
            from_toml_str(&std::fs::read_to_string(dir.path().join("config.toml")).unwrap())
                .unwrap()
                .config;
        assert_eq!(persisted.llm.model, model_id);
        assert_eq!(persisted.runtime.vram_headroom_mb, 4096);
        assert!(
            host.status()
                .models
                .iter()
                .find(|model| model.id == model_id)
                .unwrap()
                .default_offer
        );
    }

    #[test]
    fn selecting_an_incomplete_alternative_is_rejected_without_config_change() {
        let dir = tempfile::tempdir().unwrap();
        let original = "[runtime]\ntier = 2\n";
        std::fs::write(dir.path().join("config.toml"), original).unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        settle_capabilities(&host);

        let error = host.select_model("gemma-4-e4b-it-q4_k_m").unwrap_err();
        assert!(error.contains("finish downloading"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            original
        );
    }

    /// D1: the preflight verdict — unknown availability passes, a
    /// zero-byte requirement passes, and a real shortfall reports the
    /// margin-inclusive need.
    #[test]
    fn disk_shortfall_blocks_only_a_real_known_shortfall() {
        let gib = 1024u64 * 1024 * 1024;
        // Unknown availability (non-unix / probe failure): never block.
        assert_eq!(disk_shortfall(10 * gib, None), None);
        // Nothing left to download: finishing needs no new space, so a
        // verification-only resume passes even on a nearly full disk.
        assert_eq!(disk_shortfall(0, Some(0)), None);
        // Fits with the margin to spare.
        assert_eq!(disk_shortfall(gib, Some(4 * gib)), None);
        // Fits only WITHOUT the margin: blocked, and the reported need
        // includes it (that is the "needs X free" the row shows).
        assert_eq!(disk_shortfall(gib, Some(2 * gib)), Some((3 * gib, 2 * gib)));
    }

    /// D2: the retry classification — transient HTTP statuses join
    /// Interrupted on the backoff schedule, Retry-After is honored for the
    /// throttling statuses (capped), and verdict classes stay terminal.
    #[test]
    fn retry_classification_matches_d2() {
        use std::time::Duration;
        let http = |status: u16, retry_after_secs: Option<u64>| DownloadError::Http {
            status,
            url: "https://cdn.test/w".into(),
            retry_after_secs,
        };
        assert_eq!(
            retry_wait(&DownloadError::Interrupted { got_bytes: 1 }),
            Some(None)
        );
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert_eq!(
                retry_wait(&http(status, None)),
                Some(None),
                "{status} retries on the schedule"
            );
        }
        for status in [0, 400, 401, 403, 404, 416, 501] {
            assert_eq!(
                retry_wait(&http(status, Some(5))),
                None,
                "{status} is terminal even with a Retry-After"
            );
        }
        // Retry-After (seconds form) replaces the schedule's gap, capped.
        assert_eq!(
            retry_wait(&http(429, Some(30))),
            Some(Some(Duration::from_secs(30)))
        );
        assert_eq!(
            retry_wait(&http(503, Some(86_400))),
            Some(Some(RETRY_AFTER_CAP))
        );
        // Verdict classes never retry.
        assert_eq!(retry_wait(&DownloadError::Cancelled), None);
        assert_eq!(
            retry_wait(&DownloadError::ChecksumFailed { file: "f".into() }),
            None
        );
    }

    #[test]
    fn discard_partial_backend_reclaims_part_but_preserves_final_files() {
        let (dir, host) = host();
        let model = host.manifest.models.first().unwrap();
        let first = model.files.first().unwrap();
        let final_path = dir.path().join("models").join(&model.id).join(&first.path);
        std::fs::create_dir_all(final_path.parent().unwrap()).unwrap();
        let mut part_name = final_path.as_os_str().to_owned();
        part_name.push(".part");
        let part_path = std::path::PathBuf::from(part_name);
        std::fs::write(&part_path, b"partial").unwrap();
        std::fs::write(&final_path, b"final-must-survive").unwrap();

        assert_eq!(host.discard_partial(&model.id).unwrap(), 7);
        assert!(!part_path.exists());
        assert_eq!(std::fs::read(final_path).unwrap(), b"final-must-survive");
    }

    #[test]
    fn remove_waits_for_the_serialized_operation_gate_and_cancels_queued_work() {
        let (dir, host) = host();
        let model = host.manifest.models.first().unwrap();
        let model_id = model.id.clone();
        let model_dir = dir.path().join("models").join(&model_id);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("partial.bin.part"), b"partial").unwrap();
        {
            let _operation = host.model_registry.lock_operation();
            for id in [&model_id, "concurrent-sibling"] {
                host.model_registry
                    .commit_installed_locked(
                        host.bus.clone(),
                        id,
                        photoproof_core::runtime::InstalledRecord {
                            manifest_version: host.manifest.manifest_version,
                            when: "2026-07-27T00:00:00Z".into(),
                        },
                    )
                    .unwrap();
            }
        }
        {
            let mut state = host.state.lock().unwrap();
            state
                .downloads
                .insert(model_id.clone(), (7, model.total_bytes));
            state.download_queue.push_back(model_id.clone());
            state
                .download_cancels
                .insert(model_id.clone(), Arc::new(AtomicBool::new(false)));
        }

        let gate = host.model_registry.lock_operation();
        let removing = Arc::clone(&host);
        let removing_id = model_id.clone();
        let thread = std::thread::spawn(move || removing.remove_model(&removing_id));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            model_dir.exists(),
            "removal cannot touch files while another model operation owns the gate"
        );
        drop(gate);
        thread.join().unwrap().unwrap();

        assert!(!model_dir.exists());
        let state = host.state.lock().unwrap();
        assert!(!state.downloads.contains_key(&model_id));
        assert!(!state.download_queue.contains(&model_id));
        assert!(!state.download_cancels.contains_key(&model_id));
        drop(state);
        let durable: BTreeMap<String, photoproof_core::runtime::InstalledRecord> =
            serde_json::from_slice(
                &std::fs::read(dir.path().join("models/installed.json")).unwrap(),
            )
            .unwrap();
        assert!(!durable.contains_key(&model_id));
        assert!(
            durable.contains_key("concurrent-sibling"),
            "removing one model must preserve another model's committed record"
        );
    }

    #[test]
    fn gated_gc_cancels_queued_work_and_preserves_other_index_commits() {
        let (dir, host) = host();
        let model = host.manifest.models.first().unwrap();
        let model_id = model.id.clone();
        let model_dir = dir.path().join("models").join(&model_id);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("old-a.bin"), b"old-model").unwrap();
        std::fs::write(model_dir.join("old-b.bin"), b"bytes").unwrap();
        {
            let _operation = host.model_registry.lock_operation();
            for id in [&model_id, "gc-sibling"] {
                host.model_registry
                    .commit_installed_locked(
                        host.bus.clone(),
                        id,
                        photoproof_core::runtime::InstalledRecord {
                            manifest_version: host.manifest.manifest_version,
                            when: "2026-07-27T00:00:00Z".into(),
                        },
                    )
                    .unwrap();
            }
        }
        {
            let mut state = host.state.lock().unwrap();
            state
                .downloads
                .insert(model_id.clone(), (4, model.total_bytes));
            state.download_queue.push_back(model_id.clone());
            state
                .download_cancels
                .insert(model_id.clone(), Arc::new(AtomicBool::new(false)));
        }

        let gate = host.model_registry.lock_operation();
        let collecting = Arc::clone(&host);
        let collecting_id = model_id.clone();
        let worker = std::thread::spawn(move || {
            collecting.gc_model_after_scheduler_approval(&collecting_id)
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            model_dir.exists(),
            "GC cannot delete while another model operation owns the authority"
        );
        drop(gate);

        assert_eq!(worker.join().unwrap().unwrap(), 14);
        assert!(!model_dir.exists());
        let state = host.state.lock().unwrap();
        assert!(!state.downloads.contains_key(&model_id));
        assert!(!state.download_queue.contains(&model_id));
        assert!(!state.download_cancels.contains_key(&model_id));
        drop(state);
        let durable: BTreeMap<String, photoproof_core::runtime::InstalledRecord> =
            serde_json::from_slice(
                &std::fs::read(dir.path().join("models/installed.json")).unwrap(),
            )
            .unwrap();
        assert!(!durable.contains_key(&model_id));
        assert!(
            durable.contains_key("gc-sibling"),
            "GC of one model must preserve another model's committed record"
        );
    }

    #[test]
    fn second_instance_cannot_mutate_shared_model_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        let first = RuntimeHost::init(dir.path().to_path_buf());
        let second = RuntimeHost::init(dir.path().to_path_buf());
        assert!(first.status().instance_lock_held);
        assert!(!second.status().instance_lock_held);
        let model_id = second.manifest.models.first().unwrap().id.clone();
        let error = second.discard_partial(&model_id).unwrap_err();
        assert!(error.contains("another Photoproof instance"));
    }

    /// D3: cancel drops a QUEUED model, flips the in-flight flag, and
    /// clears the live rows and publishes an explicit cancelled terminal —
    /// with no error row (cancel is intent, not failure).
    #[test]
    fn cancel_download_clears_rows_and_flips_the_flag() {
        let (_dir, host) = host();
        let rx = host.bus.subscribe();
        let flag = Arc::new(AtomicBool::new(false));
        {
            let mut state = host.state.lock().unwrap();
            state.downloads.insert("m".into(), (0, 100));
            state.download_retries.insert("m".into(), "retrying".into());
            state.download_cancels.insert("m".into(), flag.clone());
            state
                .download_attempts
                .insert("m".into(), "attempt-m".into());
            state.download_queue.push_back("m".into());
        }
        host.cancel_download("m");
        let state = host.state.lock().unwrap();
        assert!(
            flag.load(Ordering::Relaxed),
            "the in-flight transfer observes the flip per chunk"
        );
        assert!(state.downloads.is_empty(), "row reads not-downloaded now");
        assert!(state.download_retries.is_empty());
        assert!(state.download_queue.is_empty(), "queued models never start");
        assert!(state.download_cancels.is_empty());
        assert!(
            state.download_errors.is_empty(),
            "no error row: cancel is not a failure"
        );
        drop(state);
        let event = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert!(matches!(
            event,
            photoproof_core::runtime::RuntimeEvent::ModelOperation {
                ref model_id,
                ref attempt_id,
                ref phase,
                terminal: true,
                error: None,
                ..
            } if model_id == "m" && attempt_id == "attempt-m" && phase == "cancelled"
        ));
        assert_eq!(
            host.model_registry.last_operation("m").unwrap().phase,
            "cancelled"
        );
    }

    #[test]
    fn download_shutdown_terminally_settles_current_and_queued_rows() {
        let (_dir, host) = host();
        let current = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicBool::new(false));
        {
            let mut state = host.state.lock().unwrap();
            state.download_worker_live = true;
            for (id, cancel) in [("current", &current), ("queued", &queued)] {
                state.downloads.insert(id.into(), (64, 100));
                state.download_retries.insert(id.into(), "retrying".into());
                state.download_cancels.insert(id.into(), Arc::clone(cancel));
                state
                    .download_attempts
                    .insert(id.into(), format!("attempt-{id}"));
                state.download_queue.push_back(id.into());
            }
        }

        host.begin_download_shutdown();

        assert!(current.load(Ordering::Acquire));
        assert!(queued.load(Ordering::Acquire));
        let state = host.state.lock().unwrap();
        assert!(state.downloads.is_empty());
        assert!(state.download_retries.is_empty());
        assert!(state.download_cancels.is_empty());
        assert!(state.download_queue.is_empty());
        assert!(state.download_attempts.is_empty());
        assert!(!state.download_worker_live);
        assert!(
            state.download_errors.is_empty(),
            "process exit is cancellation, not a fabricated transfer failure"
        );
        drop(state);
        for id in ["current", "queued"] {
            let event = host.model_registry.last_operation(id).unwrap();
            assert!(event.terminal);
            assert_eq!(event.phase, "cancelled");
            assert_eq!(event.error.as_deref(), Some("application is stopping"));
        }

        let late = host.enqueue_downloads(vec![host.manifest.models[0].clone()]);
        assert!(
            late.is_err(),
            "shutdown permanently closes download admission"
        );
        let state = host.state.lock().unwrap();
        assert!(state.downloads.is_empty());
        assert!(state.download_queue.is_empty());
    }

    #[test]
    fn debug_lines_carry_plan_states_and_the_orphan_sweep() {
        let (_dir, host) = host();
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("tier: detected"));
        assert!(lines.contains("effective 0"), "the override pinned tier 0");
        assert!(lines.contains("llm: notConfigured"));
        assert!(lines.contains("asr: notConfigured"));
        assert!(lines.contains("instance lock: held"));
        assert!(lines.contains("orphan sweep"));
    }

    #[test]
    fn valid_config_reload_commits_one_new_runtime_plan_candidate() {
        let (dir, host) = host();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();

        let loaded = host.reload_config_checked().unwrap();

        assert!(loaded.changed);
        assert_eq!(
            loaded.status.recovery.unwrap().source,
            ControlFileSource::Primary
        );
        assert_eq!(
            host.state.lock().unwrap().config.runtime.tier,
            photoproof_connectors::config::Tier::Fixed(1)
        );
    }

    #[test]
    fn invalid_config_reload_recovers_lkg_without_changing_the_live_candidate() {
        let (dir, host) = host();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        assert!(host.reload_config_checked().unwrap().changed);
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 9\n").unwrap();

        let recovered = host.reload_config_checked().unwrap();

        assert!(!recovered.changed);
        assert_eq!(
            recovered.status.recovery.unwrap().source,
            ControlFileSource::LastKnownGood
        );
        assert_eq!(
            host.state.lock().unwrap().config.runtime.tier,
            photoproof_connectors::config::Tier::Fixed(1)
        );
        assert!(
            std::fs::read_to_string(dir.path().join("config.toml"))
                .unwrap()
                .contains("tier = 1")
        );
    }

    #[test]
    fn launch_bound_models_dir_edit_is_retained_without_partial_live_apply() {
        let (dir, host) = host();
        let before = host.models_dir();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\ntier = 1\nmodels_dir = \"alternate-models\"\n",
        )
        .unwrap();

        let error = host.reload_config_checked().unwrap_err();

        assert!(error.contains("launch-bound"));
        assert_eq!(host.models_dir(), before);
        assert_eq!(
            host.state.lock().unwrap().config.runtime.tier,
            photoproof_connectors::config::Tier::Fixed(0),
            "the otherwise-valid tier edit is not partially applied"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("config.toml"))
                .unwrap()
                .contains("alternate-models"),
            "valid next-launch intent remains available instead of being quarantined"
        );
    }
}
