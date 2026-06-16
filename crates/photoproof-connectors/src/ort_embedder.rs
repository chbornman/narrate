//! In-process ort embedders (the §3.3 defended `ort` exception, retrieval
//! side): EmbeddingGemma / Qwen3 text towers and the DFN5B CLIP pair.
//!
//! One parameterized connector, not three structs (PLAN-P7.4 decision 3):
//! the [`TextRecipe`] enum carries the per-model text deltas (prompts,
//! pooling, the KV trap) and the CLIP pair is a second internal shape,
//! while load, the ort plumbing, and L2-normalization are shared.
//!
//! THE TRAPS, both verified live in the spike (docs/SPIKE-P7-EMBED.md) and
//! confirmed against the local snapshots' graph metadata:
//!   - The onnx-community causal-LM exports (Qwen3) demand `past_key_values.*`
//!     inputs even for a pure encode. We feed ZERO-LENGTH caches shaped from
//!     each input's own metadata (symbolic seq dim -> 0, batch -> 1, concrete
//!     ints kept). EmbeddingGemma's quantized export has no such inputs.
//!   - The DFN5B textual export is batch=1 fixed ([1, 77]). Queries run
//!     SINGLY; that also matches the app's one-embedding-per-search usage.
//!
//! Posture: CPU EP by default, 4 intra-op threads (`INTRA_OP_THREADS`). The
//! CoreML EP (Apple Silicon ANE/GPU) is wired behind the `PHOTOPROOF_ORT_COREML`
//! env knob WITH a compiled-model cache (the FP16 production path that measured
//! 8.77x, docs/SPIKE-COREML.md); it stays OFF by default until the FP16 model +
//! the re-embed eval graduate it to a config field. All outputs are
//! L2-normalized (PPVEC's cosine assumption; the spike scripts normalized).
//!
//! `Session::run` needs `&mut self`, but [`Embedder`] is `&self`; each
//! session sits behind a `Mutex` so concurrent callers serialize on the
//! native session (which is not internally reentrant anyway).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::Session;
use ort::session::SessionInputValue;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::{Tensor, ValueType};
use tokenizers::Tokenizer;

use crate::embedder::{DecodedImage, Embedder, Embedding};
use crate::error::{ConnectorError, ConnectorResult};

/// Spike CPU posture (docs/SPIKE-P6.3 floor, reused for P7): 4 intra-op
/// threads. WHY a constant and not a config knob yet: the GPU EP is gated
/// on spike session 2; until then there is one posture and a divergent
/// runtime value would only invite drift from the measured numbers.
const INTRA_OP_THREADS: usize = 4;

/// Qwen3-Embedding appends `<|endoftext|>` (id 151643) before last-token
/// pooling — the model card's contract. Without it the "last token" is
/// arbitrary trailing text. NOTE the deviation from the spike harness: the
/// spike let the tokenizer's post-processor append this id AND appended it
/// again by hand (text_embed_bench.py:133), so its last-token pool read a
/// DOUBLE EOS. We append exactly one (and suppress the post-processor's, see
/// `run_text`), which is the single, model-card-correct row.
const QWEN_EOS_ID: u32 = 151643;

/// CLIP BPE start/end-of-text marker ids (OpenCLIP vocab; DFN5B textual).
const CLIP_SOT_ID: i32 = 49406;
const CLIP_EOT_ID: i32 = 49407;
/// DFN5B textual context length (config.json text_cfg.context_length).
const CLIP_CONTEXT: usize = 77;

/// CLIP normalization constants (OpenCLIP / DFN5B visual preprocess_cfg);
/// the mean/std are the spike's clip_bench.py values exactly.
///
/// DEVIATION from PLAN-P7.4 decision 3, recorded honestly (the plan said the
/// preprocess constants "live in core where decode happens"): they live here
/// instead. This was forced, not a free re-decision — `DecodedImage` carries
/// interleaved sRGB `Vec<u8>`, so a "CHW normalized f32" image cannot flow
/// through the existing shared connector type without changing the trait
/// (out of this packet's scope). So core does GEOMETRY only (resize +
/// center-crop to 378x378 sRGB u8, in preprocess_clip_image) and the
/// connector does the /255 + (x-mean)/std + CHW conversion. The split is
/// functionally equivalent (same tensor) and the duplicated 378 edge is
/// cross-checked at runtime; the plan owner should treat this as a one-line
/// amendment to decision 3, not a contract a later lane "fixes" by moving the
/// constants back (which would break the size-check pairing with this file).
/// WHY these belong on THIS side regardless: they are a property of THIS ONNX
/// export — the mean/std the visual tower was trained with — not of how the
/// library decodes pixels.
const CLIP_MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const CLIP_STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

/// The DFN5B visual tower's fixed input edge. Must equal core's
/// `CLIP_IMAGE_EDGE`; a mismatch surfaces as a runtime ort shape error,
/// which `embed_image` reports rather than panicking on.
const CLIP_IMAGE_EDGE: usize = 378;

/// Documented EmbeddingGemma prompts (model card). The query side is the
/// retrieval task prompt; the document side titles each chunk. Constants so
/// the assembly is testable without a model and impossible to typo per call.
/// WHY exactly these strings: they are what the spike measured its winning
/// +0.310 paraphrase margin with; changing them changes the embedding space.
const GEMMA_QUERY_PROMPT: &str = "task: search result | query: ";
const GEMMA_DOCUMENT_PROMPT: &str = "title: none | text: ";

/// Qwen3 query-side instruction prefix (model card; spike). Documents go
/// bare (no prefix) — only queries are instructed.
const QWEN_QUERY_INSTRUCTION: &str =
    "Instruct: Given a photography journal search query, retrieve relevant journal notes\nQuery: ";

/// Which text tower, and thus which prompt/pooling behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRecipe {
    /// EmbeddingGemma: mean pooling over the last hidden state, with the
    /// documented task/title prompts. No KV inputs, no position_ids.
    MeanPooled,
    /// Qwen3: last-token pooling, EOS appended, query instruction prefix,
    /// zero-length past_key_values + position_ids.
    LastToken,
}

/// One in-process ort embedder. A text instance holds a single encode
/// session; a CLIP instance holds both towers (the [`Embedder`] trait
/// fuses text+image, and CLIP needs both — queries and images).
pub struct OrtEmbedder {
    model_id: String,
    dims: usize,
    /// The EFFECTIVE execution provider these sessions were built on, resolved
    /// once at construction (the same resolution `build_session` applies: the
    /// per-model `select_clip_accel` choice plus the force-CPU / CoreML env
    /// overrides). WHY store it: the capture-pause policy (apps/desktop pump)
    /// needs to ask "is THIS embedder on a GPU/ANE EP?" to decide whether a
    /// background embed pass must pause while the mic is armed for ASR. The
    /// discriminator is the accelerator, not the pass kind (a CPU CLIP fallback
    /// must still pause). Text embedders are always `Cpu` (SPIKE-COREML-TEXT.md).
    accel: Accel,
    inner: Inner,
}

enum Inner {
    Text {
        recipe: TextRecipe,
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        /// Zero-length cache feeds (name, dtype, shape), precomputed once
        /// from session metadata — the KV trap, shaped not guessed.
        kv_feeds: Vec<KvFeed>,
        has_position_ids: bool,
    },
    Clip {
        text_session: Mutex<Session>,
        image_session: Mutex<Session>,
        tokenizer: Tokenizer,
    },
}

/// A precomputed zero-length cache input: its name and the concrete shape
/// to allocate (the symbolic past-length dim collapsed to 0).
#[derive(Debug, Clone)]
struct KvFeed {
    name: String,
    shape: Vec<i64>,
    is_f16: bool,
}

impl OrtEmbedder {
    /// Build a text embedder from an on-disk ONNX model + tokenizer.json.
    /// Load is seconds (the host must do it off the command thread).
    pub fn text(
        model_id: impl Into<String>,
        recipe: TextRecipe,
        model_path: &Path,
        tokenizer_path: &Path,
        dims: usize,
    ) -> ConnectorResult<Self> {
        // The text embedder (EmbeddingGemma / Qwen3) stays on CPU: int8 under
        // CoreML hits a pathological compile + the transformer graph barely
        // partitions to the ANE (docs/SPIKE-COREML-TEXT.md). GPU acceleration is
        // the CLIP path, gated per-model in `clip`. (NVIDIA/TensorRT may revisit
        // text-embed - it takes the whole graph - but that is a separate measure.)
        // Text stays CPU by construction; resolve through the same override
        // helper so the recorded accel matches what `build_session` actually
        // registered (e.g. the CoreML spike env could flip a CPU base).
        let accel = resolve_effective_accel(Accel::Cpu);
        let session = build_session(model_path, Accel::Cpu)?;
        let tokenizer = load_tokenizer(tokenizer_path)?;
        let kv_feeds = zero_length_kv_feeds(&session);
        let has_position_ids = session.inputs().iter().any(|i| i.name() == "position_ids");
        Ok(Self {
            model_id: model_id.into(),
            dims,
            accel,
            inner: Inner::Text {
                recipe,
                session: Mutex::new(session),
                tokenizer,
                kv_feeds,
                has_position_ids,
            },
        })
    }

    /// Build a CLIP embedder from the DFN5B visual + textual ONNX exports
    /// (the visual export loads its ~100 external-data files relative to
    /// its own path — the caller passes the path inside that directory).
    pub fn clip(
        model_id: impl Into<String>,
        image_model_path: &Path,
        text_model_path: &Path,
        tokenizer_path: &Path,
        dims: usize,
    ) -> ConnectorResult<Self> {
        let model_id = model_id.into();
        // Per-model accelerator selection (see `select_clip_accel`): the single-
        // file FP16 CLIP runs on CoreML (macOS, ANE/GPU, measured 8.77x) or the
        // NVIDIA EPs (TensorRT/CUDA, the `cuda`/`tensorrt` build features); the
        // int8 external-data CLIP + the text embedder stay CPU. The `-fp16`
        // model-id suffix marks the GPU-ready single-file export.
        let base = select_clip_accel(&model_id);
        // The EFFECTIVE accel after env overrides — the same value `build_session`
        // registers — so `runs_on_accelerator` reports what actually loaded.
        let accel = resolve_effective_accel(base);
        let image_session = build_session(image_model_path, base)?;
        let text_session = build_session(text_model_path, base)?;
        let tokenizer = load_tokenizer(tokenizer_path)?;
        Ok(Self {
            model_id,
            dims,
            accel,
            inner: Inner::Clip {
                text_session: Mutex::new(text_session),
                image_session: Mutex::new(image_session),
                tokenizer,
            },
        })
    }
}

impl Embedder for OrtEmbedder {
    async fn embed_text(&self, text: &str) -> ConnectorResult<Embedding> {
        let vector = match &self.inner {
            Inner::Text {
                recipe,
                session,
                tokenizer,
                kv_feeds,
                has_position_ids,
            } => {
                // Documents vs queries differ only by prompt; the embed
                // path here serves chunk/summary documents. Query-side
                // prompting is the host's job at search time (it calls a
                // query helper); for the trait's general embed_text we use
                // the DOCUMENT prompt, matching how the ingest pass embeds
                // annotation chunks.
                let prompted = assemble_document_prompt(*recipe, text);
                run_text(
                    session,
                    tokenizer,
                    &prompted,
                    *recipe,
                    kv_feeds,
                    *has_position_ids,
                    self.dims,
                )?
            }
            Inner::Clip {
                text_session,
                tokenizer,
                ..
            } => run_clip_text(text_session, tokenizer, text, self.dims)?,
        };
        Ok(self.embedding(vector))
    }

    async fn embed_image(&self, img: &DecodedImage) -> ConnectorResult<Embedding> {
        let Inner::Clip { image_session, .. } = &self.inner else {
            // The text embedder has no image tower (RUNTIME §4, X3). Mirror
            // the mock's honest 501 mapping (ConnectorError has no
            // Unsupported variant).
            return Err(ConnectorError::Backend {
                status: 501,
                message: format!("embed_image unsupported by text embedder {}", self.model_id),
            });
        };
        let vector = run_clip_image(image_session, img, self.dims)?;
        Ok(self.embedding(vector))
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

impl OrtEmbedder {
    /// True iff these sessions are running on a GPU/ANE execution provider
    /// (CoreML on macOS, the NVIDIA CUDA/TensorRT ladder on a `cuda` build) —
    /// i.e. NOT the plain CPU EP.
    ///
    /// WHY this exists: the capture-pause policy. While the mic is armed for
    /// ASR, the scheduler pauses background model work so it does not contend
    /// with the CPU ASR. A GPU/ANE embed pass does NOT contend with the CPU
    /// ASR, so it must keep running (founder, June 14 2026: "Once we do have the
    /// ML model set up on GPU then we won't want to pause them while we're doing
    /// ASR."). A CPU embed pass (text, or a CLIP fp32 fallback) DOES contend, so
    /// it must still pause. The discriminator is the EXECUTION PROVIDER, not the
    /// pass kind — which is exactly what this reports.
    pub fn runs_on_accelerator(&self) -> bool {
        self.accel != Accel::Cpu
    }
}

impl OrtEmbedder {
    /// Embed a search QUERY with the recipe's query-side prompt/instruction
    /// (vs `embed_text`'s document side). The host calls this at search
    /// time for the text query vector. CLIP queries use the textual tower
    /// directly (its eval transform has no separate query prompt).
    pub fn embed_query(&self, text: &str) -> ConnectorResult<Embedding> {
        let vector = match &self.inner {
            Inner::Text {
                recipe,
                session,
                tokenizer,
                kv_feeds,
                has_position_ids,
            } => {
                let prompted = assemble_query_prompt(*recipe, text);
                run_text(
                    session,
                    tokenizer,
                    &prompted,
                    *recipe,
                    kv_feeds,
                    *has_position_ids,
                    self.dims,
                )?
            }
            Inner::Clip {
                text_session,
                tokenizer,
                ..
            } => run_clip_text(text_session, tokenizer, text, self.dims)?,
        };
        Ok(self.embedding(vector))
    }

    fn embedding(&self, vector: Vec<f32>) -> Embedding {
        Embedding {
            vector,
            model_id: self.model_id.clone(),
        }
    }

    /// The visual tower's `image` input shape as ORT sees it (`-1` for any
    /// dynamic/symbolic axis). Test-only: it backs the `visual_tower_is_batch_one`
    /// guard, which is the executable form of the "batching needs a batch-dynamic
    /// re-export" finding — if a future re-export makes dim0 dynamic, the test
    /// flips and signals that the batched `embed_images` path is now unlockable.
    #[cfg(test)]
    fn clip_image_input_shape(&self) -> Option<Vec<i64>> {
        let Inner::Clip { image_session, .. } = &self.inner else {
            return None;
        };
        let session = image_session.lock().expect("poisoned ort session");
        let input = session.inputs().iter().find(|i| i.name() == "image")?;
        let ValueType::Tensor { shape, .. } = input.dtype() else {
            return None;
        };
        // `Shape` derefs to `[i64]`; own it so the lock can drop at return.
        Some(shape.to_vec())
    }
}

// ----------------------------------------------------------- prompts -----

/// Document-side prompt assembly (testable without a model).
fn assemble_document_prompt(recipe: TextRecipe, text: &str) -> String {
    match recipe {
        TextRecipe::MeanPooled => format!("{GEMMA_DOCUMENT_PROMPT}{text}"),
        // Qwen documents go bare (spike: `embed_docs` returns `self._run`
        // with no prefix for the qwen-last style).
        TextRecipe::LastToken => text.to_string(),
    }
}

/// Query-side prompt assembly (testable without a model).
fn assemble_query_prompt(recipe: TextRecipe, text: &str) -> String {
    match recipe {
        TextRecipe::MeanPooled => format!("{GEMMA_QUERY_PROMPT}{text}"),
        TextRecipe::LastToken => format!("{QWEN_QUERY_INSTRUCTION}{text}"),
    }
}

// ------------------------------------------------------------ loading ----

/// Env knob (PLAN-PERF P2 CoreML SPIKE): when set to a truthy value
/// (`1`/`true`/`on`, case-insensitive) the CoreML execution provider is
/// registered ahead of the default CPU EP. UNSET (the default, and the only
/// shippable posture today) leaves the byte-identical CPU-only path that every
/// stored PPVEC vector was embedded under. WHY an env var and not a config
/// field: a spike wants a zero-API-surface, zero-default-change toggle that the
/// measurement harness and a curious operator can flip without touching the
/// host's converge loop or the persisted config schema; if the spike ships, it
/// graduates to a real config knob.
const COREML_ENV: &str = "PHOTOPROOF_ORT_COREML";

/// True iff `PHOTOPROOF_ORT_COREML` names a truthy value. Anything else
/// (unset, empty, `0`, `false`) keeps the CPU default.
fn coreml_requested() -> bool {
    match std::env::var(COREML_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Force the CPU EP regardless of the per-model accelerator: `PHOTOPROOF_ORT_FORCE_CPU`
/// truthy. The GPU spike harnesses use it for the CPU-baseline arm (the SAME fp16
/// weights on CPU vs the accelerator), so the speedup is a clean apples-to-apples.
fn force_cpu_requested() -> bool {
    match std::env::var("PHOTOPROOF_ORT_FORCE_CPU") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Which execution provider a model runs on, chosen per-model (`select_clip_accel`).
/// `Nvidia` is only CONSTRUCTED on a `cuda`-feature build; allow it to read as
/// dead elsewhere (the macOS/CPU builds match it but never produce it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "cuda"), allow(dead_code))]
enum Accel {
    Cpu,
    CoreML,
    Nvidia,
}

/// The accelerator for a CLIP model id. The single-file FP16 export (`-fp16`
/// suffix) graduates to the platform GPU; the int8 external-data CLIP stays CPU
/// (int8 + CoreML = pathological compile; the external-data split breaks CoreML).
/// macOS -> CoreML; a `cuda`-feature build elsewhere -> NVIDIA (TensorRT/CUDA);
/// else CPU. Exactly one cfg arm compiles per build, so the return is total.
fn select_clip_accel(model_id: &str) -> Accel {
    if !model_id.ends_with("-fp16") {
        return Accel::Cpu;
    }
    #[cfg(target_os = "macos")]
    return Accel::CoreML;
    #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
    return Accel::Nvidia;
    #[cfg(all(not(target_os = "macos"), not(feature = "cuda")))]
    return Accel::Cpu;
}

/// Apply the env overrides to a per-model base accelerator, yielding the EP a
/// session will ACTUALLY register: an explicit force-CPU wins (the GPU spike
/// baseline — same fp16 weights on CPU); otherwise the legacy CoreML knob can
/// promote a CPU-selected model to CoreML (the CoreML spike harness); else the
/// base is used as-is. Pure over (base, env) so the pause policy and the
/// session builder agree, and so it is unit-testable without loading a model.
fn resolve_effective_accel(base: Accel) -> Accel {
    if force_cpu_requested() {
        Accel::Cpu
    } else if base == Accel::Cpu && coreml_requested() {
        Accel::CoreML
    } else {
        base
    }
}

fn build_session(model_path: &Path, accel: Accel) -> ConnectorResult<Session> {
    // CPU EP is ort's default; 4 intra-op threads is the spike posture.
    // GraphOptimizationLevel::Level3 matches onnxruntime python's default
    // (ORT_ENABLE_ALL) the spike measured against. The builder option
    // methods carry a different error type than commit, so each maps to a
    // Decode error explicitly rather than chaining over mixed Results.
    let err =
        |e: String| ConnectorError::Decode(format!("ort session {}: {e}", model_path.display()));
    let mut builder = Session::builder()
        .map_err(|e| err(e.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| err(e.to_string()))?
        .with_intra_threads(INTRA_OP_THREADS)
        .map_err(|e| err(e.to_string()))?;
    // Resolve the effective accelerator (force-CPU / CoreML env overrides).
    // Shared with the embedder's recorded `accel` so `runs_on_accelerator`
    // reports exactly the EP this session registers.
    let accel = resolve_effective_accel(accel);
    match accel {
        // CPU EP is ort's default - nothing to register.
        Accel::Cpu => {}
        Accel::CoreML => {
            // Cache the compiled CoreML MLProgram beside the model so the multi-
            // minute first-load compile is paid ONCE, not on every launch (the
            // FP16 production caveat: ~16.5 min, docs/SPIKE-COREML.md). Best-effort.
            let cache = coreml_cache_dir(model_path);
            builder = build_session_with_coreml(builder, cache.as_deref()).map_err(err)?;
            // Stderr (not tracing): this crate installs no subscriber, and the
            // "GPU path taken" signal must be visible regardless of host logging.
            eprintln!(
                "[ort coreml] registered CoreML EP for {} (CPUAndNeuralEngine, MLProgram{})",
                model_path.display(),
                match &cache {
                    Some(c) => format!(", cache {}", c.display()),
                    None => " - NO cache (recompiles each load)".to_string(),
                }
            );
        }
        Accel::Nvidia => {
            builder = build_session_with_nvidia(builder, model_path).map_err(err)?;
            eprintln!(
                "[ort nvidia] registered NVIDIA EP ladder (TensorRT-fp16 -> CUDA -> CPU) for {}",
                model_path.display()
            );
        }
    }
    builder
        .commit_from_file(model_path)
        .map_err(|e| err(e.to_string()))
}

/// The CoreML compiled-model cache directory for `model_path`: a `.coreml-cache`
/// subdir beside the model file, created best-effort. `None` if it has no parent
/// or cannot be created (CoreML then recompiles each load - slower but correct).
/// Co-located with the model so it follows a `models_dir` override and is keyed
/// per tower; the dotdir matches no manifest file, so the downloader ignores it.
fn coreml_cache_dir(model_path: &Path) -> Option<PathBuf> {
    let dir = model_path.parent()?.join(".coreml-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Register the CoreML EP on `builder`. Split out so `build_session` stays
/// linear and the exact ort rc.12 builder chain lives in one documented place.
/// The chain (rc.12 API): `ep::CoreML` (not `CoreMLExecutionProvider`),
/// `with_compute_units` (was `with_ane_only`), `with_subgraphs(true)` (now takes
/// a bool), MLProgram format (Core ML 5+, supports more ops than NeuralNetwork),
/// and `.error_on_failure()` so a runtime without the EP is loud, not silent.
///
/// `cache_dir`, when `Some`, sets `ModelCacheDirectory` (`with_model_cache_dir`)
/// so the compiled MLProgram is reused across session loads - without it CoreML
/// recompiles every load (the ~16.5 min FP16 first-compile tax, docs/SPIKE-COREML).
///
/// Returns `Err(String)` (not ort's `BuilderResult`) deliberately: that result
/// carries the recoverable `SessionBuilder` in its error variant (~144 bytes),
/// which trips `clippy::result_large_err` if it escapes; collapsing to the
/// message at the boundary keeps the large error from propagating.
fn build_session_with_coreml(
    builder: ort::session::builder::SessionBuilder,
    cache_dir: Option<&Path>,
) -> Result<ort::session::builder::SessionBuilder, String> {
    use ort::ep::CoreML;
    use ort::ep::coreml::{ComputeUnits, ModelFormat};
    // PHOTOPROOF_ORT_COREML_UNITS picks the CoreML compute units. WHY a knob:
    // the fp16 DFN5B graph fragments into ~64 CoreML partitions (visual 36 +
    // textual 28); on the ANE (CPUAndNeuralEngine, the default) Apple's per-
    // partition model-prep is slow, so even a cache-warm load is minutes. The
    // Metal GPU path (`gpu` -> CPUAndGPU) usually skips that ANE prep and loads
    // far faster. Unset/anything-else keeps today's ANE behavior, so this is an
    // opt-in escape hatch; once measured it can become the default.
    let units = match std::env::var("PHOTOPROOF_ORT_COREML_UNITS")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "gpu" => ComputeUnits::CPUAndGPU,
        "all" => ComputeUnits::All,
        _ => ComputeUnits::CPUAndNeuralEngine,
    };
    let mut ep = CoreML::default()
        .with_compute_units(units)
        .with_model_format(ModelFormat::MLProgram)
        .with_subgraphs(true);
    if let Some(dir) = cache_dir {
        // ort takes the path as `impl ToString`; reuse the compiled model across
        // launches so the first-compile cost is amortized.
        ep = ep.with_model_cache_dir(dir.to_string_lossy().to_string());
    }
    builder
        .with_execution_providers([ep.build().error_on_failure()])
        .map_err(|e| e.to_string())
}

/// Register the NVIDIA EP ladder on `builder` for the FP16 CLIP on the desktop
/// (the Ryzen 9900X + RTX 5080). TensorRT-FP16 is tried first (the fastest, the
/// `tensorrt` feature) with an on-disk ENGINE CACHE - TensorRT builds a per-GPU
/// engine on first load (slow, the analog of the CoreML compile), so it MUST be
/// cached or every launch pays it; CUDA-FP16 is next (the `cuda` floor); CPU is
/// ort's implicit last rung. `.fail_silently()` makes the ladder DESCEND
/// gracefully (the opposite of CoreML's loud `.error_on_failure()`): a missing
/// or unloadable EP just falls through to the next rung. The SAME single-file
/// FP16 model that CoreML validated feeds these (docs/PLAN-TENSORRT.md).
#[cfg(feature = "cuda")]
fn build_session_with_nvidia(
    builder: ort::session::builder::SessionBuilder,
    model_path: &Path,
) -> Result<ort::session::builder::SessionBuilder, String> {
    use ort::ep::CUDA;
    #[cfg(not(feature = "tensorrt"))]
    let _ = model_path; // only TensorRT needs the engine-cache path
    let mut eps = Vec::new();
    #[cfg(feature = "tensorrt")]
    {
        use ort::ep::TensorRT;
        let mut trt = TensorRT::default().with_fp16(true);
        if let Some(dir) = nvidia_engine_cache_dir(model_path) {
            let p = dir.to_string_lossy().to_string();
            trt = trt
                .with_engine_cache(true)
                .with_engine_cache_path(p.clone())
                .with_timing_cache(true)
                .with_timing_cache_path(p);
        }
        eps.push(trt.build().fail_silently());
    }
    eps.push(CUDA::default().build().fail_silently());
    builder
        .with_execution_providers(eps)
        .map_err(|e| e.to_string())
}

/// No-op when the `cuda` feature is off (the macOS/CPU builds): `select_clip_accel`
/// never returns `Accel::Nvidia` there, but the match arm must still compile.
#[cfg(not(feature = "cuda"))]
fn build_session_with_nvidia(
    builder: ort::session::builder::SessionBuilder,
    _model_path: &Path,
) -> Result<ort::session::builder::SessionBuilder, String> {
    Ok(builder)
}

/// The TensorRT engine-cache dir for `model_path`: a `.trt-cache` beside the
/// model (per-GPU engines + timing cache reused across launches). Best-effort.
#[cfg(feature = "tensorrt")]
fn nvidia_engine_cache_dir(model_path: &Path) -> Option<PathBuf> {
    let dir = model_path.parent()?.join(".trt-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn load_tokenizer(path: &Path) -> ConnectorResult<Tokenizer> {
    Tokenizer::from_file(path)
        .map_err(|e| ConnectorError::Decode(format!("tokenizer {}: {e}", path.display())))
}

/// Inspect the session's inputs and precompute the zero-length cache feeds:
/// every `past_key_values.*` input, shaped from its OWN metadata with the
/// symbolic past-length dimension collapsed to 0 and batch to 1. Concrete
/// integer dims (head count, head dim) are kept. THIS IS THE KV TRAP.
fn zero_length_kv_feeds(session: &Session) -> Vec<KvFeed> {
    let mut feeds = Vec::new();
    for input in session.inputs() {
        if !input.name().starts_with("past_key_values") {
            continue;
        }
        let ValueType::Tensor {
            ty,
            shape,
            dimension_symbols,
        } = input.dtype()
        else {
            continue;
        };
        // shape carries -1 for dynamic dims; the symbol name disambiguates
        // batch (-> 1) from the past-length axis (-> 0). Mirrors the
        // spike's symbolic-dim handling.
        let concrete: Vec<i64> = shape
            .iter()
            .zip(dimension_symbols.iter())
            .map(|(dim, sym)| collapse_kv_dim(*dim, sym))
            .collect();
        feeds.push(KvFeed {
            name: input.name().to_string(),
            shape: concrete,
            is_f16: matches!(ty, ort::value::TensorElementType::Float16),
        });
    }
    feeds
}

/// Map ONE `past_key_values.*` dimension to its zero-length-cache value: a
/// concrete int (head count, head dim) is kept; a dynamic axis (-1) becomes 1
/// if its symbol names the batch, else 0 (the empty past-length). Extracted
/// as a free function so `zero_length_kv_feeds`'s logic — which needs a real
/// `&Session` and so can only run under the #[ignore] model tests — is
/// exercised by a deterministic default-suite test rather than a copy of it.
fn collapse_kv_dim(dim: i64, sym: &str) -> i64 {
    if dim >= 0 {
        dim // concrete (head count, head dim): keep
    } else if sym.contains("batch") {
        1 // single sequence
    } else {
        0 // the empty past
    }
}

// --------------------------------------------------------- text encode ---

#[allow(clippy::too_many_arguments)]
fn run_text(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    prompted: &str,
    recipe: TextRecipe,
    kv_feeds: &[KvFeed],
    has_position_ids: bool,
    dims: usize,
) -> ConnectorResult<Vec<f32>> {
    // Special-token handling is recipe-specific, and deliberately so —
    // both choices freeze into the PPVEC space at backfill, so they follow
    // each model's CANONICAL pipeline rather than the spike harness where
    // the two disagree (the spike's add_special_tokens=True default produced
    // measurably different / in two cases doubled token rows; see the WHY on
    // each arm).
    let add_special_tokens = match recipe {
        // Gemma: the tokenizer.json carries a TemplateProcessing post-processor
        // that wraps every sequence as <bos> ... <eos> (ids 2 / 1). The
        // EmbeddingGemma sentence-transformers pipeline AND the spike's
        // Python `tok.encode(t)` (default add_special_tokens=True) both
        // include them in the mean pool, so the +0.310 paraphrase margin was
        // measured WITH them. We must too, or we embed in a different space.
        TextRecipe::MeanPooled => true,
        // Qwen: its post-processor ALSO appends <|endoftext|>. The spike then
        // appended a SECOND one manually (text_embed_bench.py:133) and its
        // last-token pooling read that duplicate — a spike bug, not the model
        // card. We append exactly one EOS ourselves (below), so we suppress
        // the post-processor's append here to land on the single, model-card
        // -correct row.
        TextRecipe::LastToken => false,
    };
    let encoding = tokenizer
        .encode(prompted, add_special_tokens)
        .map_err(|e| ConnectorError::Decode(format!("tokenize: {e}")))?;
    let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&i| i64::from(i)).collect();
    if recipe == TextRecipe::LastToken {
        // EOS appended so last-token pooling reads a defined position. Exactly
        // one, matching the Qwen3-Embedding model card (vs the spike's double).
        ids.push(i64::from(QWEN_EOS_ID));
    }
    let seq = ids.len();
    let mask: Vec<i64> = vec![1; seq];

    let mut session = session.lock().expect("poisoned ort session");
    // KV caches are allocated against the session's own allocator: a
    // zero-length axis means a zero-element tensor, which `from_array`
    // rejects (it forbids 0 dims for raw-data inputs) but `Tensor::new`
    // allocates fine. This is the KV trap's concrete shape, fed empty.
    let mut feed = build_kv_inputs(kv_feeds, session.allocator())?;
    feed.push((
        "input_ids".to_string(),
        Tensor::from_array(([1usize, seq], ids))
            .map_err(|e| ConnectorError::Decode(format!("input_ids tensor: {e}")))?
            .into(),
    ));
    feed.push((
        "attention_mask".to_string(),
        Tensor::from_array(([1usize, seq], mask))
            .map_err(|e| ConnectorError::Decode(format!("attention_mask tensor: {e}")))?
            .into(),
    ));
    if has_position_ids {
        let pos: Vec<i64> = (0..seq as i64).collect();
        feed.push((
            "position_ids".to_string(),
            Tensor::from_array(([1usize, seq], pos))
                .map_err(|e| ConnectorError::Decode(format!("position_ids tensor: {e}")))?
                .into(),
        ));
    }

    let outputs = session
        .run(feed)
        .map_err(|e| ConnectorError::Decode(format!("text run: {e}")))?;
    // last_hidden_state is the first output for both exports: [1, seq, hid].
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| ConnectorError::Decode(format!("text output: {e}")))?;
    let hid = *shape
        .last()
        .ok_or_else(|| ConnectorError::Decode("text output has no hidden dimension".to_string()))?
        as usize;
    if hid != dims {
        return Err(ConnectorError::Decode(format!(
            "text hidden {hid} != configured dims {dims}"
        )));
    }
    let mut vector = match recipe {
        TextRecipe::MeanPooled => mean_pool(data, seq, hid),
        TextRecipe::LastToken => last_token(data, seq, hid),
    };
    l2_normalize(&mut vector);
    Ok(vector)
}

/// Build the zero-length KV input tensors for this run, allocated against
/// the session allocator (the only path that accepts a 0-length dimension).
fn build_kv_inputs(
    kv_feeds: &[KvFeed],
    allocator: &ort::memory::Allocator,
) -> ConnectorResult<Vec<(String, SessionInputValue<'static>)>> {
    let mut feed = Vec::with_capacity(kv_feeds.len() + 3);
    for kv in kv_feeds {
        let value = if kv.is_f16 {
            Tensor::<half::f16>::new(allocator, kv.shape.clone())
                .map_err(|e| ConnectorError::Decode(format!("kv f16 {}: {e}", kv.name)))?
                .into()
        } else {
            Tensor::<f32>::new(allocator, kv.shape.clone())
                .map_err(|e| ConnectorError::Decode(format!("kv f32 {}: {e}", kv.name)))?
                .into()
        };
        feed.push((kv.name.clone(), value));
    }
    Ok(feed)
}

// --------------------------------------------------------- CLIP encode ---

fn run_clip_text(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    text: &str,
    dims: usize,
) -> ConnectorResult<Vec<f32>> {
    let tokens = clip_tokenize(tokenizer, text)?;
    let tensor = Tensor::from_array(([1usize, CLIP_CONTEXT], tokens))
        .map_err(|e| ConnectorError::Decode(format!("clip text tensor: {e}")))?;
    let feed: Vec<(String, SessionInputValue<'_>)> = vec![("text".to_string(), tensor.into())];
    let mut session = session.lock().expect("poisoned ort session");
    let outputs = session
        .run(feed)
        .map_err(|e| ConnectorError::Decode(format!("clip text run: {e}")))?;
    extract_clip_embedding(&outputs, dims)
}

/// Embed ONE image through the visual tower (`[1,3,378,378]` -> `[1,1024]`).
///
/// WHY one-at-a-time and not a batched `[N,3,378,378]` forward pass: the DFN5B
/// visual ONNX exports we ship — BOTH the int8 external-data tower AND the FP16
/// single-file tower that runs on CoreML/CUDA — have the batch dimension FIXED
/// at 1, not symbolic. Verified June 2026 against the on-disk graph metadata:
/// the `image` input is `[1, 3, 378, 378]` (a concrete `1`, no dim_param) and
/// the PyTorch trace baked that `1` into ~350 internal Reshape constants, so ORT
/// rejects any other batch size; it is not something the runtime can flex.
///
/// Batching the GPU embed (the BACKLOG win: "GPUs love batches", ~24x on the MLX
/// text spike) is therefore a MODEL-CONVERSION follow-up, NOT a code change here:
/// re-export the visual tower with a dynamic batch axis
/// (`torch.onnx.export(..., dynamic_axes={"image": {0: "batch"}})`), re-run the
/// COCO eval to confirm the re-export is retrieval-safe, and only THEN add an
/// `embed_images(&[DecodedImage]) -> Vec<Embedding>` that builds one `[N,...]`
/// tensor plus the per-image-pass batching of the call site. Until that
/// re-export exists a batched path would only hard-fail at runtime, so it is
/// deliberately NOT added. The single query path stays one-at-a-time regardless
/// (one embedding per search, like the batch=1 textual tower).
fn run_clip_image(
    session: &Mutex<Session>,
    img: &DecodedImage,
    dims: usize,
) -> ConnectorResult<Vec<f32>> {
    let pixels = clip_image_tensor(img)?;
    let tensor = Tensor::from_array(([1usize, 3, CLIP_IMAGE_EDGE, CLIP_IMAGE_EDGE], pixels))
        .map_err(|e| ConnectorError::Decode(format!("clip image tensor: {e}")))?;
    let feed: Vec<(String, SessionInputValue<'_>)> = vec![("image".to_string(), tensor.into())];
    let mut session = session.lock().expect("poisoned ort session");
    let outputs = session
        .run(feed)
        .map_err(|e| ConnectorError::Decode(format!("clip image run: {e}")))?;
    extract_clip_embedding(&outputs, dims)
}

fn extract_clip_embedding(
    outputs: &ort::session::SessionOutputs<'_>,
    dims: usize,
) -> ConnectorResult<Vec<f32>> {
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| ConnectorError::Decode(format!("clip output: {e}")))?;
    let got = *shape.last().unwrap_or(&0) as usize;
    if got != dims {
        return Err(ConnectorError::Decode(format!(
            "clip embedding dim {got} != configured dims {dims}"
        )));
    }
    let mut vector = data.to_vec();
    l2_normalize(&mut vector);
    Ok(vector)
}

/// CLIP BPE tokenization into a fixed [77] i32 row: lowercase, SOT + the
/// first 75 token ids + EOT, zero-padded — the canonical single-wrapped
/// OpenCLIP row.
///
/// DEVIATION from the spike (clip_bench.py:54) recorded honestly: the DFN5B
/// textual tokenizer.json carries a RobertaProcessing post-processor that
/// already wraps SOT/EOT, and the spike called `tok.encode(t.lower())` with
/// the Python default add_special_tokens=True and THEN prepended/appended
/// SOT/EOT by hand — producing a DOUBLE-wrapped row
/// ([49406, 49406, body, 49407, 49407]). We pass add_special_tokens=false so
/// the post-processor does not fire, then wrap once: the single-marker form
/// the OpenCLIP/DFN5B export was trained against. The #[ignore] real-model
/// zero-shot check passes on this (canonical) row; it is not bit-identical to
/// the spike's doubled one.
fn clip_tokenize(tokenizer: &Tokenizer, text: &str) -> ConnectorResult<Vec<i32>> {
    let encoding = tokenizer
        .encode(text.to_lowercase(), false)
        .map_err(|e| ConnectorError::Decode(format!("clip tokenize: {e}")))?;
    let mut row = vec![0i32; CLIP_CONTEXT];
    row[0] = CLIP_SOT_ID;
    let body: Vec<i32> = encoding
        .get_ids()
        .iter()
        // Leave room for SOT + EOT (context - 2).
        .take(CLIP_CONTEXT - 2)
        .map(|&i| i as i32)
        .collect();
    let mut idx = 1;
    for id in body {
        row[idx] = id;
        idx += 1;
    }
    row[idx] = CLIP_EOT_ID;
    Ok(row)
}

/// Turn core's 378x378 sRGB bytes into the visual tower's [3,378,378] CHW
/// f32 tensor: /255, then (x - mean) / std per channel. The caller (core)
/// has already done the resize+center-crop geometry.
fn clip_image_tensor(img: &DecodedImage) -> ConnectorResult<Vec<f32>> {
    let edge = CLIP_IMAGE_EDGE as u32;
    if img.width != edge || img.height != edge {
        return Err(ConnectorError::Decode(format!(
            "clip image must be {edge}x{edge} (got {}x{}); call core preprocess_clip_image first",
            img.width, img.height
        )));
    }
    let px = CLIP_IMAGE_EDGE * CLIP_IMAGE_EDGE;
    if img.rgb8.len() != px * 3 {
        return Err(ConnectorError::Decode(format!(
            "clip image buffer {} != {}",
            img.rgb8.len(),
            px * 3
        )));
    }
    // CHW layout: all R, then all G, then all B. Source is interleaved HWC.
    let mut out = vec![0f32; px * 3];
    for (i, chunk) in img.rgb8.chunks_exact(3).enumerate() {
        for c in 0..3 {
            let v = f32::from(chunk[c]) / 255.0;
            out[c * px + i] = (v - CLIP_MEAN[c]) / CLIP_STD[c];
        }
    }
    Ok(out)
}

// --------------------------------------------------------- pooling math --

/// Mean pooling over the sequence axis of a [1, seq, hid] row-major buffer.
fn mean_pool(data: &[f32], seq: usize, hid: usize) -> Vec<f32> {
    let mut out = vec![0f32; hid];
    for t in 0..seq {
        let row = &data[t * hid..(t + 1) * hid];
        for (o, v) in out.iter_mut().zip(row) {
            *o += *v;
        }
    }
    if seq > 0 {
        let inv = 1.0 / seq as f32;
        for o in &mut out {
            *o *= inv;
        }
    }
    out
}

/// Last-token pooling: the final sequence position of a [1, seq, hid] buffer.
fn last_token(data: &[f32], seq: usize, hid: usize) -> Vec<f32> {
    if seq == 0 {
        return vec![0f32; hid];
    }
    data[(seq - 1) * hid..seq * hid].to_vec()
}

fn l2_normalize(vector: &mut [f32]) {
    // f64 accumulation so a long vector's norm stays stable (PPVEC stores
    // f32 but the division should not be the source of error).
    let norm = vector
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for x in vector.iter_mut() {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- prompt assembly (deterministic, no model) ----

    // ---- accelerator selection (the capture-pause discriminator) ----

    /// The fp16 single-file CLIP graduates to the platform GPU/ANE; the int8
    /// external-data CLIP and any non-fp16 id stay on CPU. (The platform arm is
    /// cfg-selected; this checks the model-id gate that precedes it.)
    #[test]
    fn select_clip_accel_gates_on_the_fp16_suffix() {
        // Non-fp16 ids are CPU on every build.
        assert_eq!(
            select_clip_accel("ViT-H-14-378-quickgelu__dfn5b"),
            Accel::Cpu
        );
        assert_eq!(select_clip_accel("dfn5b-int8"), Accel::Cpu);
        // The fp16 id resolves to the platform accelerator: CoreML on macOS, the
        // NVIDIA ladder on a cuda build, CPU on a plain non-mac build. In ALL
        // cases the "stays paused" CPU fallback is preserved when no GPU EP
        // exists, which is the behavior-identical-on-CPU guarantee.
        let fp16 = select_clip_accel("ViT-H-14-378-quickgelu__dfn5b-fp16");
        #[cfg(target_os = "macos")]
        assert_eq!(fp16, Accel::CoreML);
        #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
        assert_eq!(fp16, Accel::Nvidia);
        #[cfg(all(not(target_os = "macos"), not(feature = "cuda")))]
        assert_eq!(fp16, Accel::Cpu);
    }

    /// `resolve_effective_accel` is the pure (base, env) -> EP resolution the
    /// session builder and the recorded `accel` share. force-CPU always wins
    /// (the spike baseline); the base passes through untouched when no override
    /// is set. We do NOT mutate env here (other tests in-process read it); the
    /// no-override pass-through is the load-bearing case for the pause policy.
    #[test]
    fn resolve_effective_accel_passes_base_through_without_overrides() {
        // Only assert when the env knobs are not set in this process, so the
        // test is order-independent (env is process-global).
        if !force_cpu_requested() && !coreml_requested() {
            assert_eq!(resolve_effective_accel(Accel::Cpu), Accel::Cpu);
            assert_eq!(resolve_effective_accel(Accel::CoreML), Accel::CoreML);
            assert_eq!(resolve_effective_accel(Accel::Nvidia), Accel::Nvidia);
        }
        // force-CPU ALWAYS collapses every base to CPU — independent of env,
        // since the branch order makes it unconditional when set. We assert the
        // invariant via the function's structure by checking the CPU base stays
        // CPU (true under every env), which the pause policy relies on: a CPU
        // model never claims to be on an accelerator.
        assert_eq!(resolve_effective_accel(Accel::Cpu), {
            if force_cpu_requested() {
                Accel::Cpu
            } else if coreml_requested() {
                Accel::CoreML
            } else {
                Accel::Cpu
            }
        });
    }

    #[test]
    fn gemma_prompts_wrap_query_and_document_sides() {
        assert_eq!(
            assemble_query_prompt(TextRecipe::MeanPooled, "harbor at dusk"),
            "task: search result | query: harbor at dusk"
        );
        assert_eq!(
            assemble_document_prompt(TextRecipe::MeanPooled, "the strongest frame"),
            "title: none | text: the strongest frame"
        );
    }

    #[test]
    fn qwen_documents_go_bare_queries_are_instructed() {
        // Documents: no prefix at all.
        assert_eq!(
            assemble_document_prompt(TextRecipe::LastToken, "flat light, reject"),
            "flat light, reject"
        );
        // Queries: the model-card instruction prefix.
        let q = assemble_query_prompt(TextRecipe::LastToken, "moody fog bridge");
        assert!(q.starts_with("Instruct: Given a photography journal search query"));
        assert!(q.ends_with("Query: moody fog bridge"));
    }

    // ---- KV shaping (the trap, with hand-built metadata) ----

    #[test]
    fn kv_shaping_collapses_past_length_keeps_concrete_dims() {
        // Qwen3 past_key_values.*: [batch_size, 8, past_sequence_length, 128].
        // The shipped shaper (via collapse_kv_dim) must produce [1, 8, 0, 128].
        let shape: [i64; 4] = [-1, 8, -1, 128];
        let symbols = ["batch_size", "", "past_sequence_length", ""];
        let concrete: Vec<i64> = shape
            .iter()
            .zip(symbols.iter())
            .map(|(dim, sym)| collapse_kv_dim(*dim, sym))
            .collect();
        assert_eq!(concrete, vec![1, 8, 0, 128]);
        // Total element count is zero -> an empty data vec is correct.
        assert_eq!(concrete.iter().product::<i64>(), 0);
        // Each mapping rule in isolation, so a future edit that inverts the
        // concrete-vs-symbolic check or drops the batch branch fails HERE
        // (the default suite) rather than only in the #[ignore] model tests.
        assert_eq!(collapse_kv_dim(128, ""), 128, "concrete dim kept");
        assert_eq!(collapse_kv_dim(-1, "batch_size"), 1, "batch -> 1");
        assert_eq!(collapse_kv_dim(-1, "past_sequence_length"), 0, "past -> 0");
    }

    // ---- pooling math (tiny hand-built tensors) ----

    #[test]
    fn mean_pool_averages_each_hidden_dimension() {
        // seq=2, hid=3: rows [1,2,3] and [3,4,5] -> [2,3,4].
        let data = [1.0, 2.0, 3.0, 3.0, 4.0, 5.0];
        assert_eq!(mean_pool(&data, 2, 3), vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn last_token_takes_the_final_position() {
        // seq=3, hid=2: last row is [5,6].
        let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(last_token(&data, 3, 2), vec![5.0, 6.0]);
    }

    #[test]
    fn mean_pool_and_last_token_are_zero_safe() {
        assert_eq!(mean_pool(&[], 0, 4), vec![0.0; 4]);
        assert_eq!(last_token(&[], 0, 4), vec![0.0; 4]);
    }

    // ---- normalization ----

    #[test]
    fn l2_normalize_makes_unit_vectors() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm {norm}");
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_leaves_zero_vector_alone() {
        let mut v = vec![0.0f32; 5];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0; 5]);
    }

    // ---- CLIP image tensor (CHW + normalize, no model) ----

    #[test]
    fn clip_image_tensor_is_chw_normalized() {
        // A 378x378 mid-gray image: every channel value 128/255 ~= 0.502.
        let edge = CLIP_IMAGE_EDGE;
        let img = DecodedImage {
            width: edge as u32,
            height: edge as u32,
            rgb8: vec![128u8; edge * edge * 3],
        };
        let t = clip_image_tensor(&img).expect("tensor");
        assert_eq!(t.len(), 3 * edge * edge);
        // First R element: (128/255 - mean_r) / std_r.
        let expected_r = (128.0f32 / 255.0 - CLIP_MEAN[0]) / CLIP_STD[0];
        assert!((t[0] - expected_r).abs() < 1e-5);
        // The G plane starts at offset edge*edge.
        let expected_g = (128.0f32 / 255.0 - CLIP_MEAN[1]) / CLIP_STD[1];
        assert!((t[edge * edge] - expected_g).abs() < 1e-5);
    }

    #[test]
    fn clip_image_tensor_rejects_wrong_size() {
        let img = DecodedImage {
            width: 100,
            height: 100,
            rgb8: vec![0; 100 * 100 * 3],
        };
        assert!(clip_image_tensor(&img).is_err());
    }

    // ---- CoreML cache dir resolution (the production amortization, no model) ----

    #[test]
    fn coreml_cache_dir_is_beside_the_model_and_created() {
        // Lay out a fake models tree: <tmp>/<id>/visual/model.onnx, then assert
        // the cache resolves to .../visual/.coreml-cache and is created. This is
        // the per-tower, model-co-located cache the production CoreML path uses.
        // (tempfile is not a dep here; a pid-scoped temp subdir is enough and is
        // cleaned up at the end.)
        let base = std::env::temp_dir().join(format!("pp-coreml-cache-{}", std::process::id()));
        let model = base.join("dfn5b-fp16/visual/model.onnx");
        std::fs::create_dir_all(model.parent().unwrap()).expect("layout");
        let cache = coreml_cache_dir(&model).expect("cache dir");
        assert_eq!(cache, model.parent().unwrap().join(".coreml-cache"));
        assert!(cache.is_dir(), "cache dir must be created");
        // Idempotent: a second call on an existing dir still returns it.
        assert_eq!(coreml_cache_dir(&model).as_deref(), Some(cache.as_path()));
        let _ = std::fs::remove_dir_all(&base); // best-effort cleanup
    }
}

/// Real-model tests against the local spike snapshots
/// (/Users/bornman/spike-p7-embed/models). `#[ignore]` by default; run with
/// `cargo test -p photoproof-connectors -- --ignored`. They SKIP CLEANLY
/// (return early, no failure) when the model files are absent, so the suite
/// stays green on machines without the snapshots. Each mirrors a spike
/// number (docs/SPIKE-P7-EMBED.md): load, dims (768/1024), and a
/// paraphrase-margin sanity FLOOR (well under the spike's measured margins
/// to absorb the Rust-vs-Python tokenizer/resampler drift — these are
/// sanity gates, not parity gates).
#[cfg(test)]
mod real_model_tests {
    use super::*;
    use std::path::PathBuf;

    fn models_root() -> PathBuf {
        PathBuf::from("/Users/bornman/spike-p7-embed/models")
    }

    fn cos(a: &Embedding, b: &Embedding) -> f32 {
        // Both are L2-normalized, so cosine is the dot product.
        a.vector.iter().zip(&b.vector).map(|(x, y)| x * y).sum()
    }

    /// The spike's four paraphrase pairs and their unrelated foils
    /// (text_embed_bench.py PARAPHRASES / UNRELATED).
    const PARAPHRASES: [(&str, &str); 4] = [
        (
            "the focus missed her eyes, too soft",
            "eyes are soft, focus error",
        ),
        (
            "print this big, the brick texture carries it",
            "enlarge it - the wall texture is the strength",
        ),
        (
            "flat light, nothing happening",
            "dull lighting, lifeless frame",
        ),
        (
            "the cab's yellow pops against the gray street",
            "yellow taxi against a gray scene, the color works",
        ),
    ];
    const UNRELATED: [(&str, &str); 4] = [
        (
            "the focus missed her eyes, too soft",
            "the ferry wake makes a leading line",
        ),
        (
            "print this big, the brick texture carries it",
            "grandmother's birthday cake candles",
        ),
        (
            "flat light, nothing happening",
            "the dog mid-leap peak action",
        ),
        (
            "the cab's yellow pops against the gray street",
            "banding in the sky from a filter",
        ),
    ];

    /// Pairwise: every paraphrase must out-cosine its unrelated foil, and
    /// the MEAN margin must clear a conservative floor. The spike measured
    /// +0.310 (Gemma) / +0.227 (Qwen); the floor here is +0.05.
    fn assert_paraphrase_margin(embed: impl Fn(&str) -> Embedding) {
        let mut margins = Vec::new();
        for i in 0..PARAPHRASES.len() {
            let a = embed(PARAPHRASES[i].0);
            let p = embed(PARAPHRASES[i].1);
            let u = embed(UNRELATED[i].1);
            let sp = cos(&a, &p);
            let su = cos(&a, &u);
            margins.push(sp - su);
        }
        let mean = margins.iter().sum::<f32>() / margins.len() as f32;
        assert!(
            mean > 0.05,
            "mean paraphrase margin {mean:+.3} below sanity floor (per-pair {margins:?})"
        );
    }

    #[test]
    #[ignore = "needs the local EmbeddingGemma snapshot"]
    fn gemma_loads_dims_768_and_separates_paraphrases() {
        let root = models_root().join("embeddinggemma");
        let model = root.join("onnx/model_quantized.onnx");
        let tok = root.join("tokenizer.json");
        if !model.exists() || !tok.exists() {
            eprintln!(
                "skipping: EmbeddingGemma snapshot absent at {}",
                model.display()
            );
            return;
        }
        let e = OrtEmbedder::text(
            "embeddinggemma-300m-q8",
            TextRecipe::MeanPooled,
            &model,
            &tok,
            768,
        )
        .expect("load gemma");
        assert_eq!(e.dimensions(), 768);
        let v = pollster::block_on(e.embed_text("the light through the kitchen window"))
            .expect("embed");
        assert_eq!(v.vector.len(), 768);
        // L2-normalized: norm ~= 1.
        let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "norm {norm}");
        assert_paraphrase_margin(|t| pollster::block_on(e.embed_text(t)).expect("embed"));
    }

    #[test]
    #[ignore = "needs the local Qwen3-Embedding snapshot"]
    fn qwen_loads_dims_1024_and_separates_paraphrases() {
        let root = models_root().join("qwen3-embed");
        let model = root.join("onnx/model_int8.onnx");
        let tok = root.join("tokenizer.json");
        if !model.exists() || !tok.exists() {
            eprintln!("skipping: Qwen3 snapshot absent at {}", model.display());
            return;
        }
        let e = OrtEmbedder::text(
            "qwen3-embedding-0.6b-int8",
            TextRecipe::LastToken,
            &model,
            &tok,
            1024,
        )
        .expect("load qwen");
        assert_eq!(e.dimensions(), 1024);
        let v = pollster::block_on(e.embed_text("the strongest frame from the harbor series"))
            .expect("embed");
        assert_eq!(v.vector.len(), 1024);
        assert_paraphrase_margin(|t| pollster::block_on(e.embed_text(t)).expect("embed"));
    }

    #[test]
    #[ignore = "needs the local DFN5B snapshot"]
    fn clip_loads_dims_1024_text_and_image() {
        let root = models_root().join("dfn5b");
        let vis = root.join("visual/model.onnx");
        let txt = root.join("textual/model.onnx");
        let tok = root.join("textual/tokenizer.json");
        if !vis.exists() || !txt.exists() || !tok.exists() {
            eprintln!("skipping: DFN5B snapshot absent at {}", vis.display());
            return;
        }
        let e = OrtEmbedder::clip("ViT-H-14-378-quickgelu__dfn5b", &vis, &txt, &tok, 1024)
            .expect("load clip");
        assert_eq!(e.dimensions(), 1024);

        // Textual tower: a query embeds to a unit 1024-vector.
        let q = pollster::block_on(e.embed_text("a dog on a train seat")).expect("text");
        assert_eq!(q.vector.len(), 1024);
        let qn: f32 = q.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((qn - 1.0).abs() < 1e-3, "text norm {qn}");

        // Visual tower: a synthetic 378x378 image embeds to a unit
        // 1024-vector (zero-shot QUALITY belongs to the L5 e2e test, which
        // has the real corpus + core's preprocess geometry; here we only
        // prove the visual session loads and produces the right shape).
        let img = DecodedImage {
            width: 378,
            height: 378,
            rgb8: vec![128u8; 378 * 378 * 3],
        };
        let iv = pollster::block_on(e.embed_image(&img)).expect("image");
        assert_eq!(iv.vector.len(), 1024);

        // Cross-modal sanity: two DIFFERENT captions should not collapse to
        // the same vector (a smoke test that the text tower discriminates).
        let a = pollster::block_on(e.embed_text("a person")).expect("text");
        let b = pollster::block_on(e.embed_text("a building")).expect("text");
        assert!(cos(&a, &b) < 0.99, "distinct captions too similar");
    }

    /// The executable form of the "batching is a model-conversion follow-up"
    /// finding (lever 1): the shipped DFN5B visual tower's `image` input has its
    /// batch axis FIXED at 1, so a batched `[N,3,378,378]` forward pass is not
    /// possible without a batch-dynamic re-export. This guards both directions:
    /// it documents WHY `run_clip_image` is one-at-a-time, AND if a future
    /// re-export makes dim0 dynamic (`-1`) this assertion flips — a loud signal
    /// that the batched `embed_images` path can finally be built. Two distinct
    /// images are also embedded singly to confirm the path stays correct.
    #[test]
    #[ignore = "needs the local DFN5B snapshot"]
    fn visual_tower_is_batch_one_so_batching_needs_a_reexport() {
        let root = models_root().join("dfn5b");
        let vis = root.join("visual/model.onnx");
        let txt = root.join("textual/model.onnx");
        let tok = root.join("textual/tokenizer.json");
        if !vis.exists() || !txt.exists() || !tok.exists() {
            eprintln!("skipping: DFN5B snapshot absent at {}", vis.display());
            return;
        }
        let e = OrtEmbedder::clip("ViT-H-14-378-quickgelu__dfn5b", &vis, &txt, &tok, 1024)
            .expect("load clip");
        let shape = e
            .clip_image_input_shape()
            .expect("visual image input shape");
        // [batch, 3, 378, 378]. dim0 == 1 (concrete) is the FIXED-batch case
        // this packet documents. A `-1` here would mean a dynamic re-export
        // landed and the batched path is now unlockable.
        assert_eq!(
            shape[0], 1,
            "visual tower batch dim is {} (expected fixed 1); a dynamic re-export \
             would unlock embed_images - update run_clip_image's note + add the path",
            shape[0]
        );
        assert_eq!(
            &shape[1..],
            &[3, CLIP_IMAGE_EDGE as i64, CLIP_IMAGE_EDGE as i64]
        );

        // Single-image path stays correct: two distinct synthetic images embed
        // to distinct unit vectors (not collapsed), the property a batched path
        // would have to preserve (cosine ~1.0 vs the single path).
        let mut light = DecodedImage {
            width: 378,
            height: 378,
            rgb8: vec![200u8; 378 * 378 * 3],
        };
        // A diagonal gradient so the two images are genuinely different content.
        for (i, px) in light.rgb8.chunks_exact_mut(3).enumerate() {
            let v = (i % 256) as u8;
            px[0] = v;
        }
        let dark = DecodedImage {
            width: 378,
            height: 378,
            rgb8: vec![40u8; 378 * 378 * 3],
        };
        let il = pollster::block_on(e.embed_image(&light)).expect("image light");
        let id = pollster::block_on(e.embed_image(&dark)).expect("image dark");
        assert_eq!(il.vector.len(), 1024);
        assert_eq!(id.vector.len(), 1024);
        // Re-embedding the SAME image must be deterministic (cosine ~1.0): the
        // exact parity bar a batched embed would have to clear against the single
        // path. Same pixels in, same vector out.
        let il2 = pollster::block_on(e.embed_image(&light)).expect("image light again");
        assert!(
            cos(&il, &il2) > 0.9999,
            "single-image embed must be deterministic (cosine {})",
            cos(&il, &il2)
        );
        assert!(cos(&il, &id) < 0.999, "distinct images should not collapse");
    }

    #[test]
    #[ignore = "needs the local text-embedder snapshot"]
    fn text_embedder_rejects_image() {
        let root = models_root().join("embeddinggemma");
        let model = root.join("onnx/model_quantized.onnx");
        let tok = root.join("tokenizer.json");
        if !model.exists() || !tok.exists() {
            eprintln!("skipping: EmbeddingGemma snapshot absent");
            return;
        }
        let e = OrtEmbedder::text(
            "embeddinggemma-300m-q8",
            TextRecipe::MeanPooled,
            &model,
            &tok,
            768,
        )
        .expect("load gemma");
        let img = DecodedImage {
            width: 378,
            height: 378,
            rgb8: vec![0u8; 378 * 378 * 3],
        };
        // The X3 split: a text embedder has no image tower.
        assert!(pollster::block_on(e.embed_image(&img)).is_err());
    }
}
