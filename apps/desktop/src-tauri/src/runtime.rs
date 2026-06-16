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

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use photoproof_connectors::config::{Config, from_toml_str};
use photoproof_core::UtcMillis;
use photoproof_core::runtime::{
    Acceptances, ChildRegistry, DownloadError, DownloadManager, InstanceLock, Manifest,
    ProcessPlan, RuntimeBus, RuntimePlan, SleepPacer, TierDecision, compiled_manifest, plan,
    resolve_tier,
};

use crate::dto::{ModelRow, RuntimeStatus};
use crate::hardware::LiveProbe;

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

/// §10.3: "Later" re-offers from settings only; "Never" is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Undecided,
    Later,
    Never,
    Download,
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

    fn parse(s: &str) -> Self {
        match s {
            "later" => Consent::Later,
            "never" => Consent::Never,
            "download" => Consent::Download,
            _ => Consent::Undecided,
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

/// §5.3: recorded license acceptances.
fn acceptances_path(app_data: &std::path::Path) -> PathBuf {
    runtime_dir(app_data).join("acceptances.json")
}

/// §10.2–10.3: the remembered consent decision.
fn consent_path(app_data: &std::path::Path) -> PathBuf {
    runtime_dir(app_data).join("consent")
}

struct HostState {
    config: Config,
    /// Read by the debug panel (feature-gated); kept in every build.
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
    config_warnings: Vec<String>,
    tier: TierDecision,
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
    download_worker_live: bool,
    /// Startup orphan sweep result (killed, skipped) for the debug panel.
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
    orphan_sweep: (Vec<String>, Vec<String>),
}

pub struct RuntimeHost {
    pub bus: RuntimeBus,
    /// P6.4: the real supervisors (None inside until the plan says Run).
    pub supervisors: crate::supervisors::SupervisorHost,
    /// P7.4: the in-process ort embedders (§3.3). Converges on the same
    /// plan as the supervisors, on the same 2 s loop; readiness flows into
    /// `RuntimeStatus` and the search rig.
    pub embedders: crate::embedders::EmbedderHost,
    app_data: PathBuf,
    manifest: Manifest,
    lock: Option<Arc<InstanceLock>>,
    /// §5.2 throttle-while-capture-live seam: P6.3's arm/disarm wiring
    /// flips this; the pacer consults it per chunk.
    pub capture_live: Arc<AtomicBool>,
    state: Mutex<HostState>,
    /// Pins the §5.2 cross-model serialization: which thread ran each
    /// model's download, in order.
    #[cfg(test)]
    download_thread_log: Mutex<Vec<(String, std::thread::ThreadId)>>,
}

impl RuntimeHost {
    pub fn init(app_data: PathBuf) -> Self {
        let runtime_dir = runtime_dir(&app_data);
        let bus = RuntimeBus::new();
        // §8.5: the exclusive lock; supervisors refuse to spawn without
        // it. (The Tauri single-instance plugin already kept a second
        // launch from reaching this point; belt and braces.)
        let lock = InstanceLock::acquire(&runtime_dir)
            .ok()
            .flatten()
            .map(Arc::new);
        // §8.4 crash net, before any spawn could ever happen — but ONLY
        // while holding the §8.5 lock. Without it another instance is
        // live, and its healthy supervised children are exactly what the
        // sweep looks for (alive PID + matching start-time): a lockless
        // sweep would SIGKILL the live instance's children and erase its
        // crash-net records. No lock ⇒ children.json is not touched.
        // The registry PERSISTS (P6.4): supervised children record
        // themselves through it for the next launch's sweep.
        let registry = lock
            .as_ref()
            .and_then(|_| ChildRegistry::new(&runtime_dir).ok().map(Arc::new));
        let orphan_sweep = registry
            .as_ref()
            .and_then(|reg| reg.kill_orphans().ok())
            .unwrap_or_default();

        // §4.4 config: unknown keys warn, missing sections default; a
        // parse error falls back to defaults LOUDLY in the log (the
        // journal must never block on config).
        let config_path = app_data.join("config.toml");
        let (config, config_warnings) = match std::fs::read_to_string(&config_path) {
            Ok(text) => match from_toml_str(&text) {
                Ok(loaded) => (
                    loaded.config,
                    loaded
                        .unknown_keys
                        .iter()
                        .map(|k| format!("config.toml: unknown key `{k}`"))
                        .collect(),
                ),
                Err(e) => (
                    Config::default(),
                    vec![format!("config.toml rejected ({e}); using defaults")],
                ),
            },
            Err(_) => (Config::default(), Vec::new()),
        };

        // §6: detect (cached) + the override that always wins.
        let tier = resolve_tier(
            &mut LiveProbe,
            config.runtime.tier,
            &tier_path(&app_data),
            false,
        );

        // §5.1: the compiled manifest, also written for the debug panel.
        let manifest = compiled_manifest();
        let _ = manifest.write_to(&Self::models_dir_for(&app_data, &config));

        let acceptances = Acceptances::load(&acceptances_path(&app_data));
        let consent = std::fs::read_to_string(consent_path(&app_data))
            .map(|s| Consent::parse(s.trim()))
            .unwrap_or(Consent::Undecided);

        let supervisors =
            crate::supervisors::SupervisorHost::new(bus.clone(), registry, lock.clone());
        Self {
            bus,
            app_data,
            manifest,
            lock,
            supervisors,
            embedders: crate::embedders::EmbedderHost::new(),
            capture_live: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(HostState {
                config,
                config_warnings,
                tier,
                consent,
                acceptances,
                downloads: BTreeMap::new(),
                download_errors: BTreeMap::new(),
                download_retries: BTreeMap::new(),
                download_queue: VecDeque::new(),
                download_worker_live: false,
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
            PathBuf::from(&config.runtime.models_dir)
        }
    }

    fn manager(&self) -> DownloadManager {
        let state = self.state.lock().expect("runtime state");
        DownloadManager::new(
            Self::models_dir_for(&self.app_data, &state.config),
            self.bus.clone(),
        )
    }

    pub fn plan(&self) -> RuntimePlan {
        let state = self.state.lock().expect("runtime state");
        let installed = self.manager_installed(&state);
        plan(
            &state.config,
            state.tier.effective_tier,
            &self.manifest,
            &installed,
        )
    }

    /// P6.4/P7.4: converge BOTH the supervisors (external children) and the
    /// in-process embedders onto the current plan. Called at startup and on
    /// a slow cadence (state.rs) — every consent/config/download mutation is
    /// picked up within a couple of seconds without each command needing to
    /// remember to call this. The one plan computation drives both hosts so
    /// they never see divergent installed/tier state.
    pub fn apply_supervisor_plan(&self) {
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
        // roles build in-process (local-ort) vs. stay a remote seam.
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
        DownloadManager::new(
            Self::models_dir_for(&self.app_data, &state.config),
            self.bus.clone(),
        )
        .installed()
    }

    /// The model id each vector `vec_kind` is actively embedded under today,
    /// from the resolved config (the embedder host writes vectors under exactly
    /// these ids). Feeds the startup doctor's space reconciliation
    /// (STATE-INTEGRITY-AUDIT): a stored space under any OTHER model id for the
    /// same kind is superseded. ImageClip follows the CLIP embedder; the two
    /// text-embedder spaces (annotation_chunk + image_summary) follow the text
    /// embedder.
    pub fn active_vector_models(
        &self,
    ) -> std::collections::HashMap<photoproof_connectors::vector_store::VecKind, String> {
        use photoproof_connectors::vector_store::VecKind;
        // Read the RESOLVED plan, not raw config: the embedder bypass may run a
        // DIFFERENT model than config names (e.g. the unhostable fp16 default
        // falls back to an installed dfn5b). The doctor must reconcile vector
        // spaces against the model that actually WRITES them, or it would drop
        // the live fallback's space as "superseded". A NotConfigured slot
        // contributes nothing, so the doctor leaves that kind's spaces alone.
        // VERIFY-BEFORE-RETIRE (self-heal 3A): only count a model as "active"
        // when its embedder is actually LOADED, not merely NAMED active by the
        // resolved plan. WHY: the plan can name fp16 active while fp16 fails to
        // load (unhostable on this host); reporting it as active made the doctor
        // treat the live dfn5b space as "superseded by fp16" and retire the only
        // copy of the library's vectors. A named-but-unloaded model contributes
        // nothing, so the doctor leaves that kind's spaces untouched until the
        // real write path comes up.
        let plan = self.plan();
        let mut models = std::collections::HashMap::new();
        if let ProcessPlan::Run { model_id } = &plan.clip_embedder
            && self.embedders.clip_ready()
        {
            models.insert(VecKind::ImageClip, model_id.clone());
        }
        if let ProcessPlan::Run { model_id } = &plan.text_embedder
            && self.embedders.text_ready()
        {
            models.insert(VecKind::AnnotationChunk, model_id.clone());
            models.insert(VecKind::ImageSummary, model_id.clone());
        }
        models
    }

    // ------------------------------------------------------------ status ----

    pub fn status(&self) -> RuntimeStatus {
        let state = self.state.lock().expect("runtime state");
        let installed = self.manager_installed(&state);
        let tier = state.tier.effective_tier;
        let offered = self.manifest.offered_at(tier);
        let models = self
            .manifest
            .models
            .iter()
            .map(|m| {
                let offered_here = offered.iter().any(|o| o.id == m.id);
                let is_installed = installed.contains_key(&m.id);
                let downloading = state.downloads.get(&m.id);
                let error = state.download_errors.get(&m.id).cloned();
                let state_name = if is_installed {
                    "installed"
                } else if error.is_some() {
                    "failed"
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
                ModelRow {
                    id: m.id.clone(),
                    role: m.role.clone(),
                    state: state_name.into(),
                    total_bytes: m.total_bytes,
                    downloaded_bytes: downloading.map(|(d, _)| *d).unwrap_or(if is_installed {
                        m.total_bytes
                    } else {
                        0
                    }),
                    license_name: m.license.name.clone(),
                    license_url: m.license.url.clone(),
                    acceptance_required: m.license.acceptance_required,
                    accepted: state.acceptances.accepted.contains_key(&m.id),
                    error,
                    retry_hint: state.download_retries.get(&m.id).cloned(),
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
            asr_blocked: self.supervisors.asr_blocked(),
            llm_blocked: self.supervisors.llm_blocked(),
            // P7.4 §3.3: in-process embedder readiness (sessions built).
            clip_ready: self.embedders.clip_ready(),
            text_embedder_ready: self.embedders.text_ready(),
            tier_detected: state.tier.detected_tier,
            tier_effective: tier,
            tier_overridden_above: state.tier.overridden_above,
            consent: state.consent.as_str().into(),
            consent_offer_bytes: self.manifest.total_bytes_at(tier),
            models,
            instance_lock_held: self.lock.is_some(),
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
        // P7.4: the LIVE embedder-host slot state (building/ready/failed) —
        // the plan lines above describe the PLAN; these the actual ort
        // sessions, including a degraded-with-error load (§3.3).
        lines.extend(self.embedders.debug_lines());
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
    pub fn set_consent(self: &Arc<Self>, decision: &str) {
        let consent = Consent::parse(decision);
        {
            let mut state = self.state.lock().expect("runtime state");
            state.consent = consent;
        }
        let _ = std::fs::create_dir_all(runtime_dir(&self.app_data));
        let _ = std::fs::write(consent_path(&self.app_data), consent.as_str());
        if consent == Consent::Download {
            self.download_offered();
        }
    }

    /// §5.3: record an acceptance (model id, license url, timestamp).
    pub fn accept_license(&self, model_id: &str) -> Result<(), String> {
        let Some(model) = self.manifest.model(model_id) else {
            return Err(format!("unknown model {model_id:?}"));
        };
        let mut state = self.state.lock().expect("runtime state");
        state
            .acceptances
            .accept(model_id, &model.license.url, &UtcMillis::now().to_rfc3339());
        state
            .acceptances
            .save(&acceptances_path(&self.app_data))
            .map_err(|e| e.to_string())
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
        self.enqueue_downloads(vec![model]);
        Ok(())
    }

    fn download_offered(self: &Arc<Self>) {
        let tier = self
            .state
            .lock()
            .expect("runtime state")
            .tier
            .effective_tier;
        // Unpinned entries (B55 fail-closed) never enqueue: the worker would
        // only mint a "failed" row for a download that cannot exist yet, and
        // settings shows them as pending instead. Every shipped model is
        // pinned post-B73; the filter stays for any future unpinned entry.
        let offered: Vec<_> = self
            .manifest
            .offered_at(tier)
            .into_iter()
            .filter(|m| m.is_pinned())
            .cloned()
            .collect();
        self.enqueue_downloads(offered);
    }

    /// §5.2 "Concurrency: one file at a time" is a property of the
    /// download MANAGER, not of one model. Every requested model joins
    /// one queue and a single `pp-download` worker drains it in order —
    /// consent at Tier 1 enqueues four models and they transfer strictly
    /// sequentially (the within-model file ordering is core's half of
    /// the rule). The pacer throttle therefore applies to the one live
    /// transfer, undiluted.
    fn enqueue_downloads(self: &Arc<Self>, models: Vec<photoproof_core::runtime::ModelEntry>) {
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
        let spawn_worker = {
            let mut state = self.state.lock().expect("runtime state");
            for (model, on_disk) in models {
                if state.downloads.contains_key(&model.id) {
                    continue; // already queued or mid-transfer
                }
                state
                    .downloads
                    .insert(model.id.clone(), (on_disk, model.total_bytes));
                state.download_errors.remove(&model.id);
                state.download_queue.push_back(model.id);
            }
            if state.download_worker_live || state.download_queue.is_empty() {
                false
            } else {
                state.download_worker_live = true;
                true
            }
        };
        if spawn_worker {
            let host = self.clone();
            let spawned = std::thread::Builder::new()
                .name("pp-download".into())
                .spawn(move || host.drain_download_queue());
            if spawned.is_err() {
                // No worker came up: let a later enqueue try again.
                self.state
                    .lock()
                    .expect("runtime state")
                    .download_worker_live = false;
            }
        }
    }

    /// The one download worker: pops model ids until the queue is empty,
    /// running each download to completion before the next starts.
    fn drain_download_queue(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().expect("runtime state");
                match state.download_queue.pop_front() {
                    Some(id) => id,
                    None => {
                        state.download_worker_live = false;
                        return;
                    }
                }
            };
            if let Some(model) = self.manifest.model(&next).cloned() {
                self.run_download(&model);
            }
        }
    }

    /// One model, files strictly in sequence (core's loop); called only
    /// from the single queue worker. Interrupted transfers auto-retry on
    /// the [`INTERRUPTED_BACKOFF`] schedule before anything terminal is
    /// surfaced.
    fn run_download(&self, model: &photoproof_core::runtime::ModelEntry) {
        #[cfg(test)]
        self.download_thread_log
            .lock()
            .expect("download thread log")
            .push((model.id.clone(), std::thread::current().id()));
        let acceptances = self
            .state
            .lock()
            .expect("runtime state")
            .acceptances
            .clone();
        let manager = self.manager();
        let mut pacer = SleepPacer::new(self.capture_live.clone());
        let attempt = |pacer: &mut SleepPacer| {
            manager.download_model(
                model,
                self.manifest.manifest_version,
                &acceptances,
                pacer,
                &UtcMillis::now().to_rfc3339(),
            )
        };
        let mut result = attempt(&mut pacer);
        let attempts_total = 1 + INTERRUPTED_BACKOFF.len();
        for (retry, backoff) in INTERRUPTED_BACKOFF.iter().enumerate() {
            if !matches!(result, Err(DownloadError::Interrupted { .. })) {
                break; // success, or a class where retrying re-proves a falsehood
            }
            {
                let mut state = self.state.lock().expect("runtime state");
                // The row stays "downloading" — download_errors is written
                // only when the schedule is exhausted, so a single cut
                // never flashes a terminal "failed". The hint names what
                // the worker is actually doing for settings to surface.
                state.download_retries.insert(
                    model.id.clone(),
                    format!(
                        "connection interrupted — retrying (attempt {} of {})",
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
            // observed within a beat instead of after up to 30 s.
            let deadline = std::time::Instant::now() + *backoff;
            loop {
                if self.supervisors.stopping() {
                    // Quitting: no further attempt, and no error row for a
                    // download that was never given its retries — the part
                    // files keep the progress for the next launch.
                    let mut state = self.state.lock().expect("runtime state");
                    state.downloads.remove(&model.id);
                    state.download_retries.remove(&model.id);
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
        let mut state = self.state.lock().expect("runtime state");
        state.downloads.remove(&model.id);
        state.download_retries.remove(&model.id);
        match result {
            Ok(_) => {}
            Err(DownloadError::LicenseNotAccepted { .. }) => {
                // Not an error row: the gate simply hasn't opened —
                // settings shows the acceptance affordance instead.
            }
            Err(e) => {
                state
                    .download_errors
                    .insert(model.id.clone(), e.to_string());
            }
        }
    }

    /// Settings → remove a model's weights (§2.4). Installed records and
    /// files go together; tier offers are unaffected (§6.2: selection
    /// never deletes — but the user's explicit remove does).
    pub fn remove_model(&self, model_id: &str) -> Result<(), String> {
        let manager = self.manager();
        manager
            .remove_installed_record(model_id)
            .map_err(|e| e.to_string())?;
        let dir = manager.models_dir().join(model_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Settings → "restart runtime" (§8.1: re-enters Spawning with a
    /// fresh budget). No supervisor exists before P6.3; the action clears
    /// surfaced download failures so a retry can run.
    pub fn restart_runtime(&self) {
        let mut state = self.state.lock().expect("runtime state");
        state.download_errors.clear();
    }

    /// Settings → re-detect hardware (§6.1.4).
    pub fn redetect_tier(&self) -> RuntimeStatus {
        {
            let mut state = self.state.lock().expect("runtime state");
            let tier = resolve_tier(
                &mut LiveProbe,
                state.config.runtime.tier,
                &tier_path(&self.app_data),
                true,
            );
            state.tier = tier;
        }
        self.status()
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn consent_is_remembered_across_hosts_and_never_stays_never() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 0\n").unwrap();
        {
            let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
            host.set_consent("never");
        }
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        assert_eq!(host.status().consent, "never", "§10.3: Never is remembered");
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

    /// §5.2: "one file at a time" is manager-wide, not per-model —
    /// consent at Tier 1 drains every PINNED offered model through ONE
    /// worker thread, strictly in order; nothing transfers concurrently;
    /// any unpinned entry (B55 fail-closed) would never enqueue and would
    /// surface as "unpinned", not "failed" (none ships unpinned post-B73).
    /// (Every transfer fails fast here — no network in tests — which is
    /// exactly why thread identity is the observable: the old fan-out ran
    /// four threads regardless.)
    #[test]
    fn consent_download_drains_all_offered_models_on_one_worker_thread() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        let offered: Vec<String> = host
            .manifest
            .offered_at(1)
            .iter()
            .filter(|m| m.is_pinned())
            .map(|m| m.id.clone())
            .collect();
        assert!(offered.len() >= 2, "Tier 1 offers several pinned models");

        host.set_consent("download");
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
            offered,
            "every PINNED offered model ran, in enqueue order"
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
    }

    /// B73 (June 2026): the embedders ship PINNED. Every manifest entry now
    /// resolves to a real revision + SHA, so none surfaces as "unpinned";
    /// the tier-1 embedders (DFN5B + EmbeddingGemma default) are offered,
    /// render as downloadable, and the explicit download command accepts
    /// them at the seam instead of refusing. (The Qwen3 alternative is
    /// tier-2-only, so it reads "not-offered" at this tier — also a valid,
    /// non-failure state.) The B55 fail-closed path is exercised separately
    /// by the core download tests; nothing ships unpinned to drive it here.
    #[test]
    fn b73_embedders_are_pinned_offered_and_downloadable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\ntier = 1\n").unwrap();
        let host = Arc::new(RuntimeHost::init(dir.path().to_path_buf()));
        assert!(
            host.manifest.models.iter().all(|m| m.is_pinned()),
            "post-B73 every shipped model is pinned — none reads 'unpinned'"
        );
        let status = host.status();
        assert!(
            status.models.iter().all(|m| m.state != "unpinned"),
            "no row surfaces the pending-unpinned state"
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
}
