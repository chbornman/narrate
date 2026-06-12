//! P7.4 decision 4: the EmbedderHost — the in-process counterpart to
//! `SupervisorHost`. It owns one `Option<Arc<OrtEmbedder>>` per role (text +
//! CLIP) and converges them on the runtime plan exactly like
//! `apply_supervisor_plan` does, on the same 2 s converge loop
//! (`state.rs::PLAN_CONVERGE_INTERVAL`).
//!
//! WHY a host and not a supervisor: the embedders run IN-PROCESS via `ort`
//! (RUNTIME §3.3 — the defended exception), so there is no child process to
//! spawn, no port, no health probe. "Converge" here means: when the plan
//! says `Run` (model installed, offered at tier, backend local-ort), build
//! the ort sessions; when the plan goes dark or the configured model
//! changes, drop and rebuild.
//!
//! THREADING: building a session is seconds (the DFN5B visual tower loads
//! ~100 external-data files). That MUST NOT happen on the command thread or
//! the converge thread's critical section — a wedged load would freeze
//! search and status. So `apply` spawns ONE background build thread per
//! role transition and the slot stays `Building` until it lands. Readiness
//! (`text_ready`/`clip_ready`) is "the session is constructed".
//!
//! FAILURE: a native load failure (corrupt weights, missing file, an ort
//! shape error) marks the role `Failed(msg)` — visible in the debug panel
//! (`debug_lines`), surfaced as idle/degraded in the settings row text —
//! and NEVER crashes the app. RUNTIME §3.3's whole defense ("a crash inside
//! ort crashes Photoproof") rests on this isolation being honest: a load
//! that fails leaves the journal and every other feature untouched.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use photoproof_connectors::config::{EmbedderBackend, TextEmbedderBackend};
use photoproof_connectors::{OrtEmbedder, TextRecipe};
use photoproof_core::runtime::plan::{ProcessPlan, RuntimePlan};

/// One role's lifecycle. `Building` carries the model id whose load is in
/// flight so a config change mid-build can tell "still the right model"
/// from "rebuild needed"; `Ready` holds the constructed connector.
enum Slot {
    /// No model planned (tier 0, uninstalled, disabled backend, or an
    /// unsupported one). The feature is dark; the journal is whole.
    Idle,
    /// A background build thread is loading `model_id`'s sessions.
    Building { model_id: String },
    /// Sessions constructed; search and the embedding drain can use it.
    Ready {
        model_id: String,
        embedder: Arc<OrtEmbedder>,
    },
    /// The native load failed; `Failed.msg` is the debug-panel detail. The
    /// model id is kept so a later identical plan does not retry-loop the
    /// same doomed load every 2 s (only a model/path change re-attempts).
    Failed { model_id: String, msg: String },
}

impl Slot {
    fn planned_model(&self) -> Option<&str> {
        match self {
            Slot::Idle => None,
            Slot::Building { model_id }
            | Slot::Ready { model_id, .. }
            | Slot::Failed { model_id, .. } => Some(model_id),
        }
    }
}

/// Which text recipe + which on-disk onnx file a model id resolves to. The
/// three embedders are PINNED (manifest.rs), so a match on id is the simple,
/// obvious resolution — no discovery, no heuristics. dims mirror the spike
/// (docs/SPIKE-P7-EMBED.md): EmbeddingGemma 768, Qwen3 1024, DFN5B 1024.
struct TextSpec {
    recipe: TextRecipe,
    /// Relative to `models_dir/<id>/` — the manifest's pinned file paths.
    onnx: &'static str,
    tokenizer: &'static str,
    dims: usize,
}

fn text_spec(model_id: &str) -> Option<TextSpec> {
    match model_id {
        "embeddinggemma-300m-q8" => Some(TextSpec {
            recipe: TextRecipe::MeanPooled,
            onnx: "onnx/model_quantized.onnx",
            tokenizer: "tokenizer.json",
            dims: 768,
        }),
        "qwen3-embedding-0.6b-int8" => Some(TextSpec {
            recipe: TextRecipe::LastToken,
            onnx: "onnx/model_int8.onnx",
            tokenizer: "tokenizer.json",
            dims: 1024,
        }),
        _ => None,
    }
}

/// The DFN5B CLIP pair's on-disk layout (manifest.rs / dfn5b_files.rs). The
/// visual tower's external-data files load RELATIVE to `visual/model.onnx`,
/// so the subdirectory layout the path-preserving downloads keep (L1) is
/// load-bearing here.
struct ClipSpec {
    visual: &'static str,
    textual: &'static str,
    tokenizer: &'static str,
    dims: usize,
}

fn clip_spec(model_id: &str) -> Option<ClipSpec> {
    match model_id {
        "ViT-H-14-378-quickgelu__dfn5b" => Some(ClipSpec {
            visual: "visual/model.onnx",
            textual: "textual/model.onnx",
            tokenizer: "textual/tokenizer.json",
            dims: 1024,
        }),
        _ => None,
    }
}

/// The host. Both slots sit behind one mutex each (transitions are cheap;
/// only the background build does real work, off-lock). Each role has its
/// OWN generation counter — a PER-ROLE bump so the other role's dispatch
/// can never supersede this one's in-flight build (a single shared counter
/// silently discarded the first role's finished session when the second
/// dispatched). EVERY transition away from the current slot bumps this role's
/// generation — the build dispatch, shutdown, AND the converge drop-to-Idle /
/// park-Failed paths — so a stale build thread's result is discarded whenever
/// THIS role's plan moved on under it (config swap, drop, uninstall). A drop
/// or fail leaves no build to redispatch, so the stale one simply never wins
/// the `land_build` gate.
pub struct EmbedderHost {
    text: Arc<Mutex<Slot>>,
    text_gen: Arc<AtomicU64>,
    clip: Arc<Mutex<Slot>>,
    clip_gen: Arc<AtomicU64>,
    /// Serializes the actual ort session construction across roles. WHY:
    /// the CPU EP runs 4 intra-op threads (spike posture), so two heavy
    /// loads at once oversubscribe the cores — measured, the DFN5B visual
    /// load (~10 s alone) balloons past minutes when a second build runs
    /// alongside. One load at a time keeps each at its measured cost; the
    /// slots still flip to Ready independently as each finishes.
    build_lock: Arc<Mutex<()>>,
    /// Latched true by `shutdown()`. WHY: the `pp-plan-converge` loop
    /// (state.rs) keeps calling `apply()` on its 2 s cadence during the
    /// quit teardown — whose capture drain + sidecar/collections flushes can
    /// exceed that interval — so without this latch a tick after `shutdown()`
    /// would see `needs_rebuild(Idle, Some(model))` and dispatch a brand-new
    /// multi-gigabyte ort load (legitimately passing the generation gate),
    /// flipping a role back to Ready while the app is quitting. Once
    /// stopped, `apply()` is a no-op; the host stays dark for good.
    stopped: Arc<AtomicBool>,
}

impl EmbedderHost {
    pub fn new() -> Self {
        Self {
            text: Arc::new(Mutex::new(Slot::Idle)),
            text_gen: Arc::new(AtomicU64::new(0)),
            clip: Arc::new(Mutex::new(Slot::Idle)),
            clip_gen: Arc::new(AtomicU64::new(0)),
            build_lock: Arc::new(Mutex::new(())),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// True once the text-embedder sessions are constructed (§8.3 readiness
    /// = sessions built). Read by `RuntimeStatus` and the search rig.
    pub fn text_ready(&self) -> bool {
        matches!(&*self.text.lock().expect("text slot"), Slot::Ready { .. })
    }

    /// True once the CLIP sessions are constructed.
    pub fn clip_ready(&self) -> bool {
        matches!(&*self.clip.lock().expect("clip slot"), Slot::Ready { .. })
    }

    /// The constructed text embedder, when ready — the search rig and the
    /// embedding drain clone this `Arc` for their pass. `None` = degraded
    /// (keyword-only / pending), never an error.
    pub fn text(&self) -> Option<Arc<OrtEmbedder>> {
        match &*self.text.lock().expect("text slot") {
            Slot::Ready { embedder, .. } => Some(embedder.clone()),
            _ => None,
        }
    }

    /// The constructed CLIP embedder, when ready.
    pub fn clip(&self) -> Option<Arc<OrtEmbedder>> {
        match &*self.clip.lock().expect("clip slot") {
            Slot::Ready { embedder, .. } => Some(embedder.clone()),
            _ => None,
        }
    }

    /// Debug-panel lines (§8.6): one per role, naming the state honestly so
    /// a degraded-with-error embedder is visible without a crash.
    pub fn debug_lines(&self) -> Vec<String> {
        vec![
            describe("text-embedder", &self.text.lock().expect("text slot")),
            describe("clip-embedder", &self.clip.lock().expect("clip slot")),
        ]
    }

    /// Converge both roles onto the plan, building/dropping as needed. The
    /// backends gate in-process builds: only `local-ort` loads here (a
    /// remote/openai-compatible embedder is the runtime's HTTP seam, not
    /// this host's). Idempotent: a no-change plan touches nothing.
    pub fn apply(
        &self,
        plan: &RuntimePlan,
        clip_backend: EmbedderBackend,
        text_backend: TextEmbedderBackend,
        models_dir: &Path,
    ) {
        // Once shutdown() has latched the host, the converge loop must not
        // redispatch a fresh ort build during quit teardown (see `stopped`).
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        // CLIP role. Only local-ort builds in-process; an openai-compatible
        // embedder backend reaches a remote endpoint (not this host's job),
        // so it stays Idle here.
        let clip_target = match (&plan.clip_embedder, clip_backend) {
            (ProcessPlan::Run { model_id }, EmbedderBackend::LocalOrt) => Some(model_id.clone()),
            _ => None,
        };
        self.converge_clip(clip_target.as_deref(), models_dir);

        // Text role. local-ort builds here; local-llamacpp / openai-compatible
        // are remote seams (RUNTIME §3.3 alt backend) and stay Idle.
        let text_target = match (&plan.text_embedder, text_backend) {
            (ProcessPlan::Run { model_id }, TextEmbedderBackend::LocalOrt) => {
                Some(model_id.clone())
            }
            _ => None,
        };
        self.converge_text(text_target.as_deref(), models_dir);
    }

    /// Drop both slots to Idle (shutdown / restart). The build threads are
    /// detached and each per-role generation bump makes any in-flight build
    /// a no-op when it lands.
    pub fn shutdown(&self) {
        // Latch FIRST so any converge tick that races this teardown sees the
        // host stopped and returns before dispatching a new build.
        self.stopped.store(true, Ordering::SeqCst);
        self.text_gen.fetch_add(1, Ordering::SeqCst);
        self.clip_gen.fetch_add(1, Ordering::SeqCst);
        *self.text.lock().expect("text slot") = Slot::Idle;
        *self.clip.lock().expect("clip slot") = Slot::Idle;
    }

    fn converge_text(&self, target: Option<&str>, models_dir: &Path) {
        let mut slot = self.text.lock().expect("text slot");
        if !needs_rebuild(&slot, target) {
            return;
        }
        // Bump the generation on EVERY transition away from the current slot,
        // not just the build dispatch below. WHY: `land_build` gates landing
        // purely on generation equality, so a drop-to-Idle (target None) or a
        // park-Failed that left the generation untouched would let a stale
        // in-flight build land Ready over the dropped/failed slot — handing
        // search and the embedding drain a model the plan says must be dark
        // (decision 4: config/plan change -> drop and rebuild). Bumping here,
        // exactly as `shutdown()` does, makes any in-flight build a no-op.
        self.text_gen.fetch_add(1, Ordering::SeqCst);
        let Some(model_id) = target else {
            *slot = Slot::Idle;
            return;
        };
        // An unknown id (no recipe) is a config/manifest mismatch — record
        // it as Failed, dark, never a crash.
        let Some(spec) = text_spec(model_id) else {
            *slot = Slot::Failed {
                model_id: model_id.to_owned(),
                msg: format!("no ort text recipe for {model_id}"),
            };
            return;
        };
        let dir = models_dir.join(model_id);
        let model_id = model_id.to_owned();
        *slot = Slot::Building {
            model_id: model_id.clone(),
        };
        let dispatch = self.text_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self.text_gen.clone();
        let target_slot = self.text.clone();
        let build_lock = self.build_lock.clone();
        // Build OFF the converge thread (load is seconds). The result lands
        // back in the slot only if no newer build superseded it. The
        // build_lock serializes the heavy ort load against the other role.
        let _ = std::thread::Builder::new()
            .name("pp-embed-build-text".into())
            .spawn(move || {
                let built = {
                    let _hold = build_lock.lock().expect("build lock");
                    OrtEmbedder::text(
                        model_id.clone(),
                        spec.recipe,
                        &dir.join(spec.onnx),
                        &dir.join(spec.tokenizer),
                        spec.dims,
                    )
                };
                land_build(&target_slot, &generation, dispatch, model_id, built);
            });
    }

    fn converge_clip(&self, target: Option<&str>, models_dir: &Path) {
        let mut slot = self.clip.lock().expect("clip slot");
        if !needs_rebuild(&slot, target) {
            return;
        }
        // Bump on EVERY transition (see converge_text): the Idle and Failed
        // paths below must also discard a stale in-flight build, or a dropped
        // plan's load lands Ready over a dark slot.
        self.clip_gen.fetch_add(1, Ordering::SeqCst);
        let Some(model_id) = target else {
            *slot = Slot::Idle;
            return;
        };
        let Some(spec) = clip_spec(model_id) else {
            *slot = Slot::Failed {
                model_id: model_id.to_owned(),
                msg: format!("no ort clip recipe for {model_id}"),
            };
            return;
        };
        let dir = models_dir.join(model_id);
        let model_id = model_id.to_owned();
        *slot = Slot::Building {
            model_id: model_id.clone(),
        };
        let dispatch = self.clip_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self.clip_gen.clone();
        let target_slot = self.clip.clone();
        let build_lock = self.build_lock.clone();
        let _ = std::thread::Builder::new()
            .name("pp-embed-build-clip".into())
            .spawn(move || {
                let built = {
                    let _hold = build_lock.lock().expect("build lock");
                    OrtEmbedder::clip(
                        model_id.clone(),
                        &dir.join(spec.visual),
                        &dir.join(spec.textual),
                        &dir.join(spec.tokenizer),
                        spec.dims,
                    )
                };
                land_build(&target_slot, &generation, dispatch, model_id, built);
            });
    }
}

impl Default for EmbedderHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a slot must rebuild for `target`. Rebuild iff the planned model
/// differs from the slot's current model (None target = drop to Idle). A
/// `Failed` slot for the SAME model does NOT rebuild — that would retry the
/// doomed load every converge tick; only a model/path change re-attempts.
fn needs_rebuild(slot: &Slot, target: Option<&str>) -> bool {
    slot.planned_model() != target
}

/// Land a finished build into its slot — but only if no newer build
/// (config swap, shutdown) superseded it; otherwise the stale connector is
/// dropped on the floor. WHY the generation guard: two converge ticks could
/// dispatch builds for different models; without it the slower one could
/// clobber the newer.
fn land_build(
    slot: &Mutex<Slot>,
    generation: &AtomicU64,
    my_gen: u64,
    model_id: String,
    built: photoproof_connectors::ConnectorResult<OrtEmbedder>,
) {
    if generation.load(Ordering::SeqCst) != my_gen {
        return; // superseded — discard quietly
    }
    let mut slot = slot.lock().expect("embedder slot");
    // Re-check under the lock: a shutdown between the load above and here
    // must win (the slot would be Idle / a different model).
    if generation.load(Ordering::SeqCst) != my_gen {
        return;
    }
    *slot = match built {
        Ok(embedder) => Slot::Ready {
            model_id,
            embedder: Arc::new(embedder),
        },
        Err(e) => Slot::Failed {
            // The honest degraded-with-error landing (RUNTIME §3.3): a load
            // failure is data for the debug panel, never a crash.
            msg: format!("ort load failed: {e}"),
            model_id,
        },
    };
}

fn describe(role: &str, slot: &Slot) -> String {
    match slot {
        Slot::Idle => format!("{role}: idle (no model)"),
        Slot::Building { model_id } => format!("{role}: building {model_id}"),
        Slot::Ready { model_id, .. } => format!("{role}: ready {model_id}"),
        Slot::Failed { model_id, msg } => format!("{role}: FAILED {model_id} — {msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_plan(clip: Option<&str>, text: Option<&str>) -> RuntimePlan {
        let run = |id: Option<&str>| match id {
            Some(id) => ProcessPlan::Run {
                model_id: id.to_owned(),
            },
            None => ProcessPlan::NotConfigured {
                reason: "test".into(),
                fixable_by_download: false,
            },
        };
        RuntimePlan {
            effective_tier: 1,
            llm: run(None),
            asr: run(None),
            clip_embedder: run(clip),
            text_embedder: run(text),
        }
    }

    /// A fresh host is fully degraded: neither role ready, both Idle in the
    /// debug lines. This is the shipping posture until weights install.
    #[test]
    fn fresh_host_is_degraded_and_idle() {
        let host = EmbedderHost::new();
        assert!(!host.text_ready());
        assert!(!host.clip_ready());
        assert!(host.text().is_none());
        assert!(host.clip().is_none());
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("text-embedder: idle"));
        assert!(lines.contains("clip-embedder: idle"));
    }

    /// An unknown model id (no recipe) lands the slot in FAILED, not a
    /// panic — the synchronous resolve path is exercised without any model
    /// files. Both roles take the same firewall.
    #[test]
    fn unknown_model_id_fails_dark_never_panics() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");
        let plan = run_plan(Some("not-a-clip"), Some("not-a-text"));
        host.apply(
            &plan,
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(!host.text_ready());
        assert!(!host.clip_ready());
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: FAILED"), "{lines}");
        assert!(lines.contains("text-embedder: FAILED"), "{lines}");
    }

    /// A dark plan (NotConfigured everywhere) keeps both slots Idle, and a
    /// non-local-ort backend never builds in-process even when the plan
    /// would Run (the remote-endpoint seam stays the runtime's job).
    #[test]
    fn dark_plan_and_remote_backend_stay_idle() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");

        host.apply(
            &run_plan(None, None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(
            host.debug_lines()
                .join("\n")
                .contains("text-embedder: idle")
        );

        // Plan says Run, but the backend is openai-compatible: no in-process
        // build — stays Idle.
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::OpenaiCompatible,
            TextEmbedderBackend::OpenaiCompatible,
            dir,
        );
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: idle"), "{lines}");
        assert!(lines.contains("text-embedder: idle"), "{lines}");
    }

    /// Resolution truth (PLAN-P7.4 decision 2/3): the three pinned ids map
    /// to the right recipe + dims; an id with no spec resolves to None.
    #[test]
    fn pinned_ids_resolve_to_recipes() {
        assert_eq!(text_spec("embeddinggemma-300m-q8").unwrap().dims, 768);
        assert_eq!(
            text_spec("embeddinggemma-300m-q8").unwrap().recipe,
            TextRecipe::MeanPooled
        );
        assert_eq!(text_spec("qwen3-embedding-0.6b-int8").unwrap().dims, 1024);
        assert_eq!(
            text_spec("qwen3-embedding-0.6b-int8").unwrap().recipe,
            TextRecipe::LastToken
        );
        assert_eq!(
            clip_spec("ViT-H-14-378-quickgelu__dfn5b").unwrap().dims,
            1024
        );
        assert!(text_spec("nope").is_none());
        assert!(clip_spec("nope").is_none());
    }

    /// REGRESSION (review L4-host): a drop-to-Idle must discard a stale
    /// in-flight build. The Idle and Failed converge transitions bump the
    /// per-role generation, so a build dispatched before the drop fails the
    /// `land_build` gate and never overwrites the dropped slot with Ready —
    /// otherwise search and the embedding drain would run a model the plan
    /// has dropped (decision 4). We model the race without a real ort load:
    /// capture the dispatch generation, converge to a dropped plan (which
    /// must bump), then drive `land_build` with the captured generation and
    /// assert it is a no-op.
    #[test]
    fn drop_to_idle_discards_a_stale_inflight_build() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");

        // Tick 1: plan wants the CLIP model. `apply` dispatches a background
        // build and bumps clip_gen to its dispatch value. The build thread
        // will fail (no files) and try to land — but we race a drop first.
        let dispatch_gen = host.clip_gen.load(Ordering::SeqCst) + 1;
        host.apply(
            &run_plan(Some("ViT-H-14-378-quickgelu__dfn5b"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );

        // Tick 2: the user drops the plan (uninstall / backend swap). The
        // converge-to-Idle path must bump clip_gen so the dispatch_gen above
        // is now stale.
        host.apply(
            &run_plan(None, None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(
            host.clip_gen.load(Ordering::SeqCst) > dispatch_gen,
            "drop-to-Idle must bump the generation past the in-flight dispatch"
        );

        // Now simulate the stale build finishing and trying to land against
        // the dropped slot. With the generation bumped, `land_build` returns
        // before touching the slot — so the slot stays Idle (NOT Failed,
        // which is what this Err would otherwise write), proving the stale
        // result never reached the slot at all. The Ready case is identical:
        // the gate is checked before the match on `built`.
        land_build(
            &host.clip,
            &host.clip_gen,
            dispatch_gen,
            "ViT-H-14-378-quickgelu__dfn5b".to_owned(),
            Err(photoproof_connectors::ConnectorError::NotReady("stale")),
        );
        assert!(!host.clip_ready(), "stale build must not land over a drop");
        assert!(
            host.debug_lines()
                .join("\n")
                .contains("clip-embedder: idle"),
            "stale Err must not even land Failed; slot must stay Idle: {}",
            host.debug_lines().join("\n")
        );
    }

    /// REGRESSION (review L4-host): after `shutdown()` the host is latched —
    /// a converge tick that fires during quit teardown (its flushes can
    /// exceed the 2 s interval) must NOT redispatch a fresh ort build. Here
    /// `apply` with a Run plan after shutdown is a no-op; the slot stays Idle.
    #[test]
    fn apply_after_shutdown_is_a_no_op() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");
        host.shutdown();
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: idle"), "{lines}");
        assert!(lines.contains("text-embedder: idle"), "{lines}");
    }

    /// The local DFN5B/EmbeddingGemma snapshots, if present, prove the FULL
    /// host path end to end: `apply` dispatches a background build, the slot
    /// transitions Building -> Ready, and `text()`/`clip()` then hand out a
    /// usable connector. Skips cleanly when the snapshots are absent (the
    /// gate machine has no weights). Lays out a temp `models_dir/<id>/`
    /// pointing at each snapshot so the host's `<id>/<file.path>` joins
    /// resolve exactly as they would post-download (L1 path layout).
    #[test]
    #[ignore = "needs the local EmbeddingGemma + DFN5B snapshots"]
    fn host_builds_real_sessions_and_reaches_ready() {
        let snaps = std::path::Path::new("/Users/bornman/spike-p7-embed/models");
        let gemma = snaps.join("embeddinggemma");
        let dfn = snaps.join("dfn5b");
        if !gemma.join("onnx/model_quantized.onnx").exists()
            || !dfn.join("visual/model.onnx").exists()
        {
            eprintln!(
                "skipping: embedder snapshots absent under {}",
                snaps.display()
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        // Build REAL id directories and symlink the snapshot's immediate
        // children in (not the id dir itself): the DFN5B visual tower's
        // external-data files load relative to `visual/model.onnx`, and
        // mirroring `visual/`+`textual/`+`onnx/` as their own symlinks keeps
        // that resolution identical to a direct snapshot path (the L3 test's
        // working layout) — a single directory-level symlink at the id made
        // ort's external-data resolution pathologically slow.
        let gemma_dir = models_dir.join("embeddinggemma-300m-q8");
        let dfn_dir = models_dir.join("ViT-H-14-378-quickgelu__dfn5b");
        std::fs::create_dir_all(&gemma_dir).unwrap();
        std::fs::create_dir_all(&dfn_dir).unwrap();
        std::os::unix::fs::symlink(gemma.join("onnx"), gemma_dir.join("onnx")).unwrap();
        std::os::unix::fs::symlink(
            gemma.join("tokenizer.json"),
            gemma_dir.join("tokenizer.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(dfn.join("visual"), dfn_dir.join("visual")).unwrap();
        std::os::unix::fs::symlink(dfn.join("textual"), dfn_dir.join("textual")).unwrap();

        let host = EmbedderHost::new();
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            &models_dir,
        );

        // The build is on a background thread (load is seconds — the DFN5B
        // visual tower especially: a 2.7 GB session over ~100 external-data
        // files; ~13 s for the pair under the debug profile in the spike).
        // Poll for readiness with a generous cap.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while (!host.text_ready() || !host.clip_ready()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let lines = host.debug_lines().join("\n");
        assert!(host.text_ready(), "text never reached Ready: {lines}");
        assert!(host.clip_ready(), "clip never reached Ready: {lines}");

        // A ready slot hands out a working connector. The trait methods
        // (`dimensions`/`embed_text`) need `Embedder` in scope.
        use photoproof_connectors::Embedder;
        let te = host.text().expect("text connector");
        assert_eq!(te.dimensions(), 768);
        let v = pollster::block_on(te.embed_text("a quiet harbor at dusk")).expect("embed");
        assert_eq!(v.vector.len(), 768);
        let ce = host.clip().expect("clip connector");
        assert_eq!(ce.dimensions(), 1024);

        host.shutdown();
        assert!(!host.text_ready(), "shutdown drops the sessions");
        assert!(!host.clip_ready());
    }
}
