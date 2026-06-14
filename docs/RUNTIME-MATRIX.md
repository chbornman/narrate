# Runtime / acceleration matrix

Every ML model PhotoProof runs: its precision, its runtime, how it is accelerated
per platform, and its fallback chain. This is a CONSOLIDATED VIEW - the normative
source is `spec/RUNTIME.md` (S3 model serving, S6 tiers) + `docs/MODELS.md` (the
per-seam model picks); where they disagree, the spec wins. Reflects the P2 CoreML
spike finding (`docs/SPIKE-COREML.md`) and the deployment plan (macOS / Windows /
Linux; CoreML / CUDA / Vulkan).

## The fallback principle (two layers, floor = Tier 0)

Tier 0 = NO models: typed notes + grease pencil + FTS5 search - a complete product.
EVERY runtime failure (a model that will not download, an execution provider that
will not load, a native crash) lands at Tier 0 quietly. So "robust fallbacks" means:

1. **Accelerator -> CPU** per model: try the best backend for the platform; on any
   load/validate failure, fall back to the CPU path.
2. **CPU -> dark** per feature: if even CPU fails or the model is absent, THAT
   feature goes dark and the journal is unaffected (Tier 0). Voice off != app
   broken; semantic search off -> keyword search still works; etc.

## The models (5 live seams)

| seam | model | precision | runtime | process |
|---|---|---|---|---|
| **LLM** (summaries, query parse, captions) | Gemma 4 E2B QAT + vision projector | q4_0 (mmproj q8_0) | llama.cpp | `llama-server` child |
| **ASR** (voice -> text) | Nemotron-speech-streaming-en-0.6b (enc/dec/joiner) | int8 | sherpa-onnx | `pp-asr-server` child |
| **VAD** (speech gating) | silero-vad v5 (~2 MB) | int8 | ONNX Runtime (`ort`) | in-process |
| **Text embed** (search S1/S3) | EmbeddingGemma-300m (768-dim) | q8/int8 | `ort` | in-process |
| **Image+text CLIP** (search S4) | DFN5B ViT-H-14-378 (1024-dim) | int8 today; **FP16 in progress** | `ort` | in-process |
| Reranker | none (RRF fusion only) | - | - | - |

Three runtimes, three different acceleration stories.

## llama.cpp (the LLM) - already cross-platform accelerated

The model that already has the full fallback framework. Vendored per-platform
binaries; the supervisor picks by detected hardware at spawn (`--gpu-layers auto`).
This is the gold standard the others should match (`spec/RUNTIME.md` S3).

| platform | backend | runs on |
|---|---|---|
| macOS (Apple Silicon) | **Metal** | GPU |
| Windows / Linux + NVIDIA | **CUDA** | GPU |
| Windows / Linux, other GPU | **Vulkan** | GPU |
| any (no GPU) | CPU | CPU |

## ONNX Runtime / `ort` (CLIP, text-embed, VAD) - CPU today, EP work pending

`ort` accelerates via per-platform EXECUTION PROVIDERS. Spec default: CPU EP, GPU
"a tier-promoted opt-in once the spike validates stability" (`spec/RUNTIME.md`).
VAD is tiny -> CPU forever. CLIP + text-embed are the ones that want the GPU/ANE.

| platform | EP | runs on | status |
|---|---|---|---|
| macOS | **CoreML** | ANE / GPU | WIRED off-by-default (P2, `PHOTOPROOF_ORT_COREML`); BLOCKED on FP16 single-file models - int8 + the external-data split both fail on CoreML. **FP16 conversion in progress.** |
| Windows / Linux + NVIDIA | **CUDA** | GPU | PLANNED (the "Margo" desktop). The same FP16 ONNX serves it; wiring is the analog of the CoreML EP (an `ort` `cuda` feature + EP registration). |
| Windows | **DirectML** | GPU | OPTION (any DX12 GPU, no CUDA needed). Not yet evaluated. |
| any | CPU | CPU | **LIVE default.** CLIP ~3 s/image (~18 img/min) - the embedding bottleneck. |
| (Vulkan) | n/a | - | NOT available via `ort`. Would need a different runtime (ggml / ncnn). Later / cross-platform-GPU item. |

WHY FP16 (not int8) for the EPs: GPUs + the Neural Engine run FP16 fast and accept
it cleanly; int8 falls back to CPU on CoreML (measured, P2). FP16 is near-lossless,
and ONE FP16 ONNX serves both CoreML and CUDA. **int8 stays as the CPU fallback;
FP16 is added for the accelerators.** The export must be single-file (inlined
weights) - the 397-file external-data split is what broke CoreML.

Honest caveat (`spec/RUNTIME.md`): an `ort` native crash crashes the app (no process
boundary). Mitigations: pin the `ort` version per release, CPU default, a dedicated
pre-validated-shape thread, embedding only in background passes (never the capture
path) so a crash cannot lose an annotation.

## sherpa-onnx (the ASR) - CPU by design

`spec/RUNTIME.md` S6.2: CPU EP by default on every tier; "CPU-resident ASR is a
FEATURE, not a fallback" - a 0.6B streaming model is real-time on laptop CPUs at
int8, and keeping ASR off the GPU removes the worst VRAM contention (live mic vs the
LLM) by construction. GPU optional via config; sherpa-onnx itself supports
CUDA/CoreML providers if ever wanted. Default: stay on CPU.

## Precision strategy (why each format)

- **q4_0** (LLM): the K-quant size/quality sweet spot for llama.cpp; GPU-friendly.
- **int8** (ASR, VAD, and the current CLIP/text-embed CPU path): smallest, real-time
  on CPU.
- **FP16** (CLIP/text-embed accelerator path, in progress): the format GPUs and the
  ANE run fast, near-lossless. Built by ROUNDING DOWN from the FP32 source (you
  cannot recover precision by upscaling int8). One FP16 ONNX serves CoreML + CUDA.

## Fallback chains (per model)

- **LLM:** Metal | CUDA | Vulkan (by detected hardware) -> CPU -> LLM features dark
  (Tier 0).
- **CLIP / text-embed:** CoreML-FP16 (Mac) | CUDA-FP16 (NVIDIA) | DirectML (Win) ->
  CPU-int8 -> embed pass deferred / semantic search degrades to keyword (still
  works).
- **ASR:** CPU-int8 (by design) -> ASR disabled (voice dark, journal unaffected).
  Model fallback order (`spec/RUNTIME.md`): multilingual 3.5 -> English 0.6b ->
  disabled.
- **VAD:** CPU (always; tiny).

## Open / in-flight (the framework's remaining wiring)

- **[in progress]** FP16 single-file CLIP + text-embed -> unblock CoreML on Mac (the
  embedding bottleneck). `docs/SPIKE-COREML.md`.
- **[planned]** CUDA EP wiring for the `ort` embedders (Margo NVIDIA desktop) - same
  FP16 model, the CoreML analog.
- **[planned]** DirectML EP option (Windows GPUs without CUDA).
- **[later]** Vulkan path for the `ort` embedders - needs a non-`ort` runtime; the
  cross-platform GPU fallback.
- **[design -> implement]** the supervisor choosing EP/binary by DETECTED hardware +
  tier is already designed for llama.cpp; extend the same auto-detect + graceful
  fallback to the `ort` embedders (CoreML/CUDA/DirectML/CPU) once FP16 lands.

## Model concurrency - which models co-run, by user flow

WHERE (the accelerator/EP tables above) is only half the story. WHEN models run is
governed by a SCHEDULER (`crates/photoproof-core/src/runtime/scheduler.rs`,
`spec/RUNTIME.md` S9) that enforces a strict priority ladder:

> **live voice  >  interactive search  >  background fuel**

The mechanics: mic-armed pauses ALL background model work (resumes 5s after disarm,
`MIC_UNPAUSE_DELAY_MS`); an interactive LLM query parse PREEMPTS a background LLM
job if it would wait >250ms; the LLM is one `llama-server` with two slots
(interactive + background, never more); GPU/embedding passes run at concurrency 1.

### Per user flow (which models fire)
| user action | models | live/bg | who pauses/yields |
|---|---|---|---|
| **Dictate a note** (Space) | VAD + ASR | LIVE (CPU) | background embed + LLM PAUSE while mic armed |
| **Search** (type a query) | LLM parse + text-embed (query) + CLIP-text (if visual / few results) | LIVE | interactive parse PREEMPTS background LLM |
| **Import / ingest a shoot** | CLIP image-embed + text-embed (notes); LLM caption is SPEC-ONLY (M3, not wired) | BACKGROUND | all yield to voice + interactive; serialized concurrency 1 |
| **Stop reviewing** (30-min idle / session close) | LLM session + per-image summaries - SPEC-ONLY (storage + search consume them; generation not wired yet, M3) | BACKGROUND | yields to voice + interactive |
| **Open Visualizer** | precomputed CLIP + text vectors only (no live model) | precomputed | no "digest" feature exists; see note below |

> Build-status caveat (verified): of the BACKGROUND LLM jobs above, only query-parse
> is actually wired today. Captions (`PassName::Caption`) are a registered pass with
> no runner (M3). Summary GENERATION is unwired (the `derived_summaries` table, FTS,
> `image_summary` vectors, and S3 search all exist and would consume them - nothing
> produces them yet). "Digest" is not a feature - it is UI shorthand for ingest
> progress counters ("hashing 12 / previews 480"), unrelated to summaries. The
> embedders are CPU-only in production (no GPU EP wired); the priority ladder is real
> and wired.

### Real overlap scenarios (the contention cases)
| co-running | resolution | governed by |
|---|---|---|
| Dictate **+** background embedding | embedding PAUSES (voice protected); resumes +5s | scheduler `set_mic_armed` |
| Search **+** background embedding | CO-RUN (both small ort sessions; no serialization) | ort concurrent sessions |
| Search **+** background LLM summary | interactive PREEMPTS (cancels background lane) | scheduler 250ms rule |
| Image-embed **+** note-embed (both bg) | SERIALIZED (concurrency 1, queue order) | LIBRARY.md S10.3 |
| VAD **+** ASR | sequential by design (VAD gates, ASR transcribes) | CAPTURE.md S6.2 |
| LLM **+** LLM | serialized (one server, 2 slots, 1 active/lane) | RUNTIME.md S3.1 |

So contention is managed by TWO complementary layers: this scheduler decides WHEN
(priority ladder + mic-pause + preemption), and the EP/accelerator tables above
decide WHERE (which chip). That is why ASR-on-CPU + background-pause-during-capture
means the live mic never fights the GPU/LLM.

## Where the normative truth lives

`spec/RUNTIME.md` (S3 serving + the CPU-default / GPU-opt-in EP plan + llama.cpp
per-platform vendoring; S6 tiers + the Tier-0 floor) and `docs/MODELS.md` (per-seam
picks). THIS file is the consolidated matrix that was not pulled together before -
keep it in sync when an EP lands or a model changes.
