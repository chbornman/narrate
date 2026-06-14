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
| **Image+text CLIP** (search S4) | DFN5B ViT-H-14-378 (1024-dim) | int8 (CPU fallback) + **FP16 (validated; CoreML 8.77x)** | `ort` | in-process |
| Reranker | none (RRF fusion only) | - | - | - |

Three runtimes, three different acceleration stories.

## Target hardware (best-first, by machine)

WHERE each model runs is not fixed - it is CHOSEN at startup from detected hardware
(OS + GPU vendor + VRAM/RAM), best-first, then falls back. Two layers stack: (1) the
best accelerator PER MODEL for that machine, and (2) the Tier-0 floor under everything
(typed notes + grease pencil + ratings + FTS5 - a complete product even with zero
models). The two CURRENT target machines, both cutting-edge-GPU focused:

- **M1 Pro MacBook Pro 16"** - Apple Silicon (ANE + ~16-core GPU, unified memory).
  Primary dev machine and the VALIDATED-TODAY target (CoreML CLIP shipped, below).
- **Ryzen 9900X + RTX 5080** desktop - AMD Zen5 12c/24t + NVIDIA Blackwell, 16 GB
  GDDR7. The powerful CUDA target; can run a HIGHER tier (more VRAM, see below).

### Per model x per machine (the 5 seams)

| model | M1 Pro (best) | Ryzen 9900X + RTX 5080 (best) | CPU fallback |
|---|---|---|---|
| **LLM** (Gemma 4 E2B q4_0, llama.cpp) | **Metal** (GPU) | **CUDA** (GPU) | CPU -> LLM features dark |
| **CLIP** (DFN5B, `ort`) | **CoreML FP16** (ANE/GPU) - DONE, **8.77x** over CPU, near-lossless (COCO nDCG 0.8212 vs int8 0.8225) | **CUDA FP16** - PLANNED (same single-file FP16 model, `ort` `cuda` EP) | CPU int8 -> keyword search |
| **Text-embed** (EmbeddingGemma, `ort`) | **CoreML FP16** - PLANNED (needs the FP16 re-export; only CLIP is converted so far) | **CUDA FP16** - PLANNED | CPU int8 -> keyword search |
| **ASR** (Nemotron 0.6b, sherpa) | **CPU** by design | **CPU** by design | smaller model -> voice dark |
| **VAD** (silero, `ort`) | **CPU** always (~2ms/chunk) | **CPU** always (~2ms/chunk) | n/a (tiny) |

ASR is CPU on BOTH machines on purpose: a 0.6B streaming model is real-time on CPU,
and keeping it off the GPU removes the worst contention (live mic vs the LLM for VRAM)
by construction. On the 5080 there IS GPU headroom, so ASR-on-GPU is reconsiderable -
but the contention-free design still favors CPU. VAD is tiny -> CPU forever, everywhere.

### The fallback ladder (when/how we fall back)

Applied PER MODEL, independently (CLIP can be on CoreML while text-embed is still on
CPU, while the LLM is on Metal - each seam descends its own ladder):

1. **Detect hardware** (OS + GPU vendor + VRAM/RAM) at startup.
2. **Best EP per model** for that machine (the table above).
3. On any **load/validate failure**, fall back to the **CPU** path for that model.
4. If **CPU also fails** (or the model is absent), THAT FEATURE goes dark - voice off,
   semantic search degrades to keyword - and the journal is unaffected.
5. The **Tier-0 floor** (typed notes + grease pencil + ratings + FTS5) is always a
   complete product underneath, no matter how many seams fell to dark.

### Tier headroom and validation status

The 5080 desktop can run a **higher tier**: 16 GB GDDR7 means bigger LLM weights
(Tier-2 quality upgrade), FP16 accelerator paths, and more concurrency than a Tier-1
machine - the cutting-edge target. The M1 Pro is the **validated-today** machine:
CoreML FP16 CLIP is the one accelerator path measured end-to-end and shipped-ready
(8.77x, near-lossless; `docs/SPIKE-COREML.md`), wired behind an env knob with the
compiled-model cache landed. CUDA on the 5080 is the next analog wiring, not yet measured.

### Future / backlog (lower-power + other hardware)

Explicitly DEFERRED, not detailed here: Intel Macs (no ANE), integrated / older GPUs
(Vulkan / DirectML), low-RAM machines (smaller models, Tier 0/1), Linux/Windows
WITHOUT NVIDIA (Vulkan), ARM Windows. Vulkan in particular needs a NON-`ort` runtime
for the embedders (noted again under the `ort` section below). These are tracked in
`docs/BACKLOG.md`; the focus now is the two cutting-edge GPU targets above.

> Supervisor logic: the detect -> tier -> select -> fallback path already EXISTS for
> llama.cpp (the gold standard below) and is being extended to the `ort` embedders -
> CoreML done, CUDA next.

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
| macOS | **CoreML** | ANE / GPU | **VALIDATED June 14: SHIP-WITH-FP16.** The inlined FP16 visual tower LOADS under CoreML (int8 + the external-data split both failed); measured **8.77x** over CPU (18 -> 162 img/min), near-lossless (cosine vs CPU min 0.9956). Caveat: a ~16.5 min FIRST-LOAD compile - production must set `.with_model_cache_dir(...)` to amortize it. Still wired off-by-default (`PHOTOPROOF_ORT_COREML`); production model-selection not yet wired. `docs/SPIKE-COREML.md`. |
| Windows / Linux + NVIDIA | **CUDA** | GPU | PLANNED (the "Margo" desktop). The same FP16 ONNX serves it; wiring is the analog of the CoreML EP (an `ort` `cuda` feature + EP registration). |
| Windows | **DirectML** | GPU | OPTION (any DX12 GPU, no CUDA needed). Not yet evaluated. |
| any | CPU | CPU | **LIVE default.** CLIP ~3 s/image (~18 img/min) - the embedding bottleneck (CoreML-FP16 lifts it 8.77x; pending production wiring). |
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

- **[DONE June 14]** FP16 single-file CLIP conversion -> CoreML VALIDATED at **8.77x**
  over CPU, near-lossless (SHIP-WITH-FP16). Models staged locally; recipe + cosines +
  measurements in `docs/SPIKE-COREML.md`. The int8 dir stays as the CPU fallback.
- **[DONE June 14 - code wiring]** the CoreML compiled-model CACHE
  (`.with_model_cache_dir`, beside each tower) + the `...__dfn5b-fp16` model spec
  are landed (`ort_embedder.rs` `coreml_cache_dir`, `model_specs.rs`). So the env-knob
  CoreML path is now practical (compile once, not per launch) and the fp16 id is
  buildable by the eval rig. CPU default stays byte-identical.
- **[NEXT - founder/infra]** to ship the 8.77x to all users: (a) HOST the fp16 model
  + add a `runtime/manifest.rs` entry (SHAs recorded in `docs/SPIKE-COREML.md`);
  (b) prefer fp16+CoreML on macOS in `runtime/plan.rs` + graduate the env knob to a
  config field; (c) run the COCO golden-nDCG re-embed eval before flipping the default.
  Also re-export the EmbeddingGemma text tower to FP16 the same way.
- **[planned]** CUDA EP wiring for the `ort` embedders (Margo NVIDIA desktop) - same
  FP16 model, the CoreML analog.
- **[planned]** DirectML EP option (Windows GPUs without CUDA).
- **[later]** Vulkan path for the `ort` embedders - needs a non-`ort` runtime; the
  cross-platform GPU fallback.
- **[planned, founder June 14 2026]** per-model capture-pause: once the embedders run
  on a GPU EP, RELAX the mic-armed pause for GPU embed passes (a GPU embed no longer
  contends with CPU ASR). Keep pausing the GPU LLM during capture (bandwidth/thermal).
  Replaces today's blanket "pause all background" with a per-model policy. Gated on the
  GPU EP landing. See `docs/BACKLOG.md` June 14 thread.
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
