# Runtime / acceleration matrix

How PhotoProof picks the best ML backend for whatever machine it is on. This is the
AUTHORITATIVE consolidated view of WHERE each model runs and WHEN models co-run. The
normative source is `spec/RUNTIME.md` (S3 model serving, S6 tiers, S9 scheduler) +
`docs/MODELS.md` (the per-seam model picks); where they disagree, the spec wins. This
file folds in the validated spike/plan findings (CoreML, Blackwell CUDA, MTP, MLX,
Vulkan) - pointers throughout.

## The intelligent-detection principle

At startup PhotoProof DETECTS the machine (OS + GPU vendor + VRAM/RAM), maps it to a
TIER, and picks the best execution provider (EP) PER MODEL - then descends a graceful
fallback ladder if anything fails to load. The vision: give every user the best
experience their hardware allows, degrading cleanly. Most people will not have a 5080;
they will have a Mac (CoreML), some NVIDIA card (CUDA), a Windows DX12 GPU (DirectML),
or no GPU (CPU) - and ALL of those are good experiences.

Two layers stack, and the Tier-0 floor is always underneath:

- **Tier-0 floor (zero models):** typed notes + grease pencil + ratings + FTS5 search
  is already a COMPLETE product. Every runtime failure (model will not download, EP
  will not load, a native crash) lands here quietly. The journal is never at risk.
- **WHERE (this doc, top half):** the best EP per model for the detected machine, with
  a per-model fallback ladder down to CPU and then to dark.
- **WHEN (this doc, bottom half):** the SCHEDULER - a strict priority ladder
  (live voice > interactive search > background fuel) deciding which models co-run.

WHERE is chosen PER MODEL and independently: CLIP can be on CoreML while text-embed
stays on CPU while the LLM is on Metal. Each seam descends its own ladder. The
supervisor already does detect -> tier -> select -> fallback for llama.cpp (the gold
standard) and is being extended to the `ort` embedders.

### The fallback ladder (applied per model)

1. **Detect hardware** (OS + GPU vendor + VRAM/RAM) at startup.
2. **Best EP per model** for that machine (the matrix below).
3. On any **load/validate failure**, fall back to the **CPU** path for that model.
4. If **CPU also fails** (or the model is absent), THAT FEATURE goes dark - voice off,
   semantic search degrades to keyword - and the journal is unaffected.
5. The **Tier-0 floor** is always a complete product underneath, no matter how many
   seams fell to dark.

## State of the matrix (June 14 2026)

What is validated/shipped vs planned vs staged, per seam, with the numbers. Speedups
are over the CPU/int8 fallback for that seam.

| seam | best result so far | status | source |
|---|---|---|---|
| **CLIP image embed** (the bottleneck) | M1 Pro CoreML FP16 = **8.77x** (18 -> 162 img/min), near-lossless | **SHIPPED**, flipped on the dev machine | `docs/SPIKE-COREML.md` |
| **CLIP image embed** | RTX 5080 **TensorRT FP16 = 85.79x** (41 -> 3635 img/min), cosine 0.99994 (CUDA alone = 54.47x) | **VALIDATED** (both rungs) | `docs/PLAN-ORT-BLACKWELL.md`, `docs/PLAN-TENSORRT.md` |
| **Text embed** (EmbeddingGemma) | int8/CPU is BEST everywhere measured; CoreML LOSES (0.48-0.64x) | CPU shipped; CUDA whole-graph re-measure TBD | `docs/SPIKE-COREML-TEXT.md`, `docs/SPIKE-MLX-COREML-TEXT.md` |
| **LLM** (Gemma 4) | Metal (Mac) / CUDA (NVIDIA); Gemma 4 MTP = 1.4-2.98x but CUDA-ONLY | MTP staged for the 5080; laptop stays plain E2B | `docs/PLAN-GEMMA-MTP.md` |
| **ASR** (Nemotron 0.6b) | CPU by design (real-time, frees the GPU) | shipped; 3.5 upgrade staged, NO-GO pending crate | `docs/PLAN-NEMOTRON-35.md` |
| **VAD** (silero) | CPU always (~2ms/chunk, tiny) | shipped | - |
| **Cross-platform GPU** (non-Apple/non-NVIDIA) | DirectML EP (Windows DX12) + WebGPU EP (strategic) | planned; NOT raw Vulkan (`ort` has no Vulkan EP) | `docs/PLAN-VULKAN.md` |

Headline: with the GPU embed at 54x, the BOTTLENECK MOVED off CLIP onto decode/resize
(see "The new perf frontier" below).

## The models (5 live seams)

| seam | model | precision | runtime | process |
|---|---|---|---|---|
| **LLM** (summaries, query parse, captions) | Gemma 4 E2B QAT + vision projector (5080: + MTP variant) | q4_0 (mmproj q8_0) | llama.cpp | `llama-server` child |
| **ASR** (voice -> text) | Nemotron-speech-streaming-en-0.6b (enc/dec/joiner) | int8 | sherpa-onnx | `pp-asr-server` child |
| **VAD** (speech gating) | silero-vad v5 (~2 MB) | int8 | ONNX Runtime (`ort`) | in-process |
| **Text embed** (search S1/S3) | EmbeddingGemma-300m (768-dim) | q8/int8 | `ort` | in-process |
| **Image+text CLIP** (search S4) | DFN5B ViT-H-14-378 (1024-dim) | int8 (CPU) + **FP16 (accelerators)** | `ort` | in-process |
| Reranker | none (RRF fusion only) | - | - | - |

Three runtimes (llama.cpp, sherpa-onnx, `ort`), three different acceleration stories.

## The per-model x per-hardware matrix

The full range, top to floor. Most users land in the MIDDLE columns (a Mac, or some
NVIDIA card, or a Windows GPU, or no GPU) - and every one of those is a good
experience. Speedups are over that seam's CPU fallback.

| model | RTX 5080 (top) | M1 Pro (validated) | no-GPU laptop | Tier-0 floor |
|---|---|---|---|---|
| **LLM** (Gemma 4, llama.cpp) | **CUDA** + MTP variant (1.4-2.98x; CUDA-only) | **Metal** - plain E2B (MTP REGRESSES 11-28% on Metal) | CPU | LLM features dark; notes + search intact |
| **CLIP image** (DFN5B, `ort`) | **TensorRT FP16 = 85.79x** (3635 img/min, cosine 0.99994); CUDA alone = 54.47x | **CoreML FP16** - **8.77x** (162 img/min), near-lossless; SHIPPED | **CPU int8** (~18-41 img/min) | semantic image search -> keyword |
| **Text embed** (EmbeddingGemma, `ort`) | **CUDA FP16** - TBD (whole-graph; worth re-measuring) | **CPU int8** - BEST (CoreML/MLX lose; ANE takes ~3% of graph) | **CPU int8** - BEST | semantic search -> keyword |
| **ASR** (Nemotron 0.6b, sherpa) | **CPU** by design (frees the GPU) | **CPU** by design | **CPU** by design | voice dark; journal intact |
| **VAD** (silero, `ort`) | **CPU** always | **CPU** always | **CPU** always | n/a (tiny) |
| Windows DX12 GPU (any tier) | **DirectML FP16** (CLIP), CUDA if NVIDIA | n/a | DirectML if GPU present | same floor |

Notes that span the matrix:

- **ASR is CPU on every machine on purpose.** A 0.6B streaming model is real-time on
  laptop CPUs at int8, and keeping it off the GPU removes the worst contention (live
  mic vs the LLM for VRAM) by construction. Even on the 5080 (which has GPU headroom)
  the contention-free design favors CPU.
- **Text embed is CPU everywhere measured.** Its transformer graph barely partitions
  to the ANE (~3%), so CoreML measured SLOWER (0.48-0.64x); native MLX is ~4x on the
  Metal GPU but not worth the macOS-only integration for a non-bottleneck seam
  (`docs/SPIKE-MLX-COREML-TEXT.md`). On NVIDIA the whole graph runs on CUDA, so this
  is worth a re-measure - TBD.
- **CPU fallback numbers:** CLIP int8 ~18 img/min (M1) / ~41 img/min (Ryzen 9900X).

### Tier headroom

The 5080 desktop runs a HIGHER tier: 16 GB GDDR7 affords bigger LLM weights (the MTP
variant), FP16/TensorRT accelerator paths, and more concurrency than a Tier-1 machine.
The M1 Pro is the validated-today reference (CoreML FP16 CLIP measured end-to-end and
shipped). A no-GPU laptop is a fully usable Tier-1 product on CPU int8.

## Detection -> selection logic

What is auto-detected at startup and how it maps to the EP choice:

| detected | mapped to | drives |
|---|---|---|
| **OS** (macOS / Windows / Linux) | the EP family available | CoreML on Mac; DirectML/CUDA on Windows; CUDA/Vulkan(LLM) on Linux |
| **GPU vendor** (Apple / NVIDIA / AMD / Intel / none) | the per-model EP | CoreML / CUDA / DirectML / CPU |
| **GPU compute capability** (e.g. sm_120 Blackwell) | which onnxruntime binary to load | hardware-matched runtime via load-dynamic (see Blackwell lesson) |
| **VRAM / RAM** | the TIER | LLM weight size, FP16 vs int8, concurrency, MTP eligibility |

The supervisor ALREADY does this detect -> tier -> select -> fallback for llama.cpp
(picks Metal / CUDA / Vulkan / CPU binary by detected hardware at spawn, `--gpu-layers
auto`). The same machinery is being EXTENDED to the `ort` embedders: CoreML done, CUDA
next, DirectML planned. So the embedders inherit the gold-standard auto-detect rather
than reinventing it.

### The Blackwell lesson (the model for bleeding-edge GPUs)

On brand-new silicon, PREBUILT binaries lag the hardware - plan for it. The RTX 5080 is
Blackwell, compute capability **sm_120**. Both `ort`'s bundled onnxruntime AND the
official `onnxruntime-gpu` 1.26 pip wheel top out at **sm_90 (Hopper)** - they cannot
emit kernels for the 5080. The fix: the official **cuda13 onnxruntime tarball** carries
real sm_120 SASS, loaded via `ort`'s `load-dynamic` + `ORT_DYLIB_PATH` (the
`cuda-dynamic` cargo feature). Lesson to generalize: when detection finds a GPU newer
than the bundled runtime supports, load a HARDWARE-MATCHED onnxruntime via load-dynamic
rather than the prebuilt binary. (`docs/PLAN-ORT-BLACKWELL.md`.)

## Per-runtime detail

### llama.cpp (the LLM) - already cross-platform accelerated

The model that already has the full fallback framework. Vendored per-platform binaries;
the supervisor picks by detected hardware at spawn. The gold standard the others should
match (`spec/RUNTIME.md` S3).

| platform | backend | runs on |
|---|---|---|
| macOS (Apple Silicon) | **Metal** | GPU |
| Windows / Linux + NVIDIA | **CUDA** | GPU |
| Windows / Linux, other GPU | **Vulkan** | GPU |
| any (no GPU) | CPU | CPU |

Gemma 4 MTP (multi-token prediction) gives 1.4-2.98x BUT is CUDA-only - it REGRESSES
11-28% on Metal. So the 5080 gets the gemma-4 MTP variant; the laptop stays plain E2B
(`docs/PLAN-GEMMA-MTP.md`).

### ONNX Runtime / `ort` (CLIP, text-embed, VAD)

`ort` accelerates via per-platform EXECUTION PROVIDERS. Spec default: CPU EP, GPU as a
tier-promoted opt-in once a spike validates stability (`spec/RUNTIME.md`). VAD is tiny
-> CPU forever. CLIP is the one that wants the GPU/ANE (the bottleneck; a conv/matmul
stack CoreML/CUDA take whole). Text-embed measured BEST on int8/CPU - its transformer
graph barely partitions, so CoreML LOSES. So on `ort`, only CLIP graduates to the GPU.

| platform | EP | runs on | status |
|---|---|---|---|
| macOS | **CoreML** | ANE / GPU | **SHIPPED.** Inlined FP16 visual tower; **8.77x** over CPU (18 -> 162 img/min), near-lossless (cosine vs CPU min 0.9956, COCO-1k nDCG 0.8212 vs int8 0.8225). Caveat: a ~16.5 min FIRST-LOAD compile, amortized by `.with_model_cache_dir(...)` (landed). Flipped on the dev machine. `docs/SPIKE-COREML.md` |
| Windows / Linux + NVIDIA | **TensorRT / CUDA** | GPU | **VALIDATED on the 5080: TensorRT FP16 = 85.79x** (3635 img/min, cosine 0.99994; CUDA alone 54.47x). Same FP16 ONNX; via `cuda-dynamic` + the cuda13 onnxruntime (sm_120) + TensorRT 10.16. `docs/PLAN-ORT-BLACKWELL.md`, `docs/PLAN-TENSORRT.md` |
| Windows | **DirectML** | GPU | PLANNED. Any DX12 GPU (AMD/Intel/NVIDIA), no CUDA needed; accepts our FP16 ONNX. The cheapest cross-platform win - an `ort` feature + EP registration, analogous to CoreML/CUDA. `docs/PLAN-VULKAN.md` |
| Win/Linux, other GPU | **WebGPU** | GPU | WATCH. `ort` 2.0 `WebGPUExecutionProvider` (Dawn-backed: DX12/Vulkan/Metal) is the strategic one-EP cross-platform bet; younger than DirectML for our models. `docs/PLAN-VULKAN.md` |
| any | CPU | CPU | **LIVE default.** CLIP int8 ~18-41 img/min - the floor under every accelerator. |
| (raw Vulkan) | n/a | - | NOT available via `ort` (no Vulkan EP, confirmed). DirectML + WebGPU are the answers instead. |

WHY FP16 (not int8) for the EPs: GPUs and the Neural Engine run FP16 fast and accept it
cleanly; int8 falls back to CPU on CoreML (measured). FP16 is near-lossless, and ONE
FP16 ONNX serves CoreML + CUDA + DirectML. **int8 stays as the CPU fallback; FP16 is
added for the accelerators.** The export must be single-file (inlined weights) - the
397-file external-data split is what broke CoreML.

Honest caveat (`spec/RUNTIME.md`): an `ort` native crash crashes the app (no process
boundary). Mitigations: pin the `ort` version per release, CPU default, a dedicated
pre-validated-shape thread, embedding only in background passes (never the capture
path) so a crash cannot lose an annotation.

### sherpa-onnx (the ASR) - CPU by design

`spec/RUNTIME.md` S6.2: CPU EP by default on every tier; "CPU-resident ASR is a
FEATURE, not a fallback" - a 0.6B streaming model is real-time on laptop CPUs at int8,
and keeping ASR off the GPU removes the worst VRAM contention (live mic vs the LLM) by
construction. The Nemotron 3.5 upgrade is STAGED but a NO-GO until the sherpa-onnx Rust
crate ships it (`docs/PLAN-NEMOTRON-35.md`). GPU optional via config; default: CPU.

## Precision strategy (why each format)

- **q4_0** (LLM): the K-quant size/quality sweet spot for llama.cpp; GPU-friendly.
- **int8** (ASR, VAD, and the CLIP/text-embed CPU path): smallest, real-time on CPU.
- **FP16** (the CLIP accelerator path): the format GPUs and the ANE run fast,
  near-lossless. Built by ROUNDING DOWN from the FP32 source (you cannot recover
  precision by upscaling int8). One FP16 ONNX serves CoreML + CUDA + DirectML.
  (Text-embed FP16 was measured and REJECTED on CoreML/MLX - it stays int8/CPU.)

## Fallback chains (per model)

- **LLM:** Metal | CUDA (+MTP on capable NVIDIA) | Vulkan (by detected hardware) ->
  CPU -> LLM features dark (Tier 0).
- **CLIP:** CoreML-FP16 (Mac, SHIPPED) | CUDA-FP16 / TensorRT (NVIDIA, validated) |
  DirectML (Windows DX12, planned) -> CPU-int8 -> embed pass deferred / semantic search
  degrades to keyword (still works).
- **Text-embed:** **CPU-int8 IS the best path** (CoreML/MLX rejected; CUDA whole-graph
  TBD) -> if even CPU fails / model absent, semantic search degrades to keyword.
- **ASR:** CPU-int8 (by design) -> ASR disabled (voice dark, journal unaffected). Model
  order (`spec/RUNTIME.md`): multilingual 3.5 (staged) -> English 0.6b -> disabled.
- **VAD:** CPU (always; tiny).

## The new perf frontier (the bottleneck moved)

With the GPU embed at 54x, decode/resize is the new ceiling - CLIP is no longer the
slow seam on a fast GPU. Decode IS already parallelized (rayon pool `min(cores, 8)`,
`library/mod.rs:2986`; BLAKE3 hashing `min(cores, 8)`). Two levers now matter:

1. **Re-bench the `min(cores, 8)` cap on the desktop.** That cap was tuned on the M1
   Pro; the Ryzen 9900X (12c/24t) feeding a 54x GPU may want a higher cap to keep the
   GPU fed. (`docs/PLAN-PERF.md`.)
2. **Batch the GPU embed.** Today the embed is single-image / unbatched. Batching
   16-32 images per forward could beat even TensorRT by amortizing launch overhead.

These are the next perf items, not yet built.

## Open / in-flight (remaining wiring)

- **[SHIPPED]** FP16 single-file CLIP -> CoreML at **8.77x**, near-lossless; cache +
  `...__dfn5b-fp16` model spec landed; flipped on the dev machine. `docs/SPIKE-COREML.md`.
- **[VALIDATED]** CUDA FP16 on the 5080 at **54.47x** via `cuda-dynamic` + cuda13
  onnxruntime (sm_120). `docs/PLAN-ORT-BLACKWELL.md`.
- **[VALIDATED]** TensorRT EP = **85.79x** (3635 img/min, +1.58x over CUDA, cosine
  0.99994; TensorRT 10.16 sm120 via `pip 'tensorrt-cu12<11'`). `docs/PLAN-TENSORRT.md`.
- **[NEXT - distribution]** ship the CoreML 8.77x to ALL Mac users: host the fp16 model
  + `runtime/manifest.rs` entry; prefer fp16+CoreML in `runtime/plan.rs`; run the COCO
  golden-nDCG re-embed eval before flipping the default for others.
- **[REJECTED]** EmbeddingGemma text-embed on CoreML/FP16 and MLX - both lose to
  int8/CPU; text-embed stays int8/CPU. `docs/SPIKE-COREML-TEXT.md`, `docs/SPIKE-MLX-COREML-TEXT.md`.
- **[TBD]** CUDA whole-graph text-embed re-measure on the 5080.
- **[planned]** DirectML EP (Windows DX12 GPUs); **[watch]** WebGPU EP (cross-platform).
  `docs/PLAN-VULKAN.md`.
- **[staged, NO-GO]** Nemotron 3.5 ASR upgrade - blocked on the sherpa-onnx Rust crate.
  `docs/PLAN-NEMOTRON-35.md`.
- **[perf]** re-bench `min(cores, 8)` on the desktop + batch the GPU embed (above).
- **[planned, founder]** per-model capture-pause: once embedders run on a GPU EP, RELAX
  the mic-armed pause for GPU embed passes (a GPU embed no longer contends with CPU
  ASR). Keep pausing the GPU LLM during capture. See `docs/BACKLOG.md`.
- **[design -> implement]** extend the supervisor's detect -> tier -> select -> fallback
  (already wired for llama.cpp) to the `ort` embedders across CoreML/CUDA/DirectML/CPU.

## Model concurrency - which models co-run, by user flow (the WHEN layer)

WHERE (the EP tables above) is only half the story. WHEN models run is governed by a
SCHEDULER (`crates/photoproof-core/src/runtime/scheduler.rs`, `spec/RUNTIME.md` S9) that
enforces a strict priority ladder:

> **live voice  >  interactive search  >  background fuel**

The mechanics: mic-armed pauses ALL background model work (resumes 5s after disarm,
`MIC_UNPAUSE_DELAY_MS`); an interactive LLM query parse PREEMPTS a background LLM job if
it would wait >250ms; the LLM is one `llama-server` with two slots (interactive +
background, never more); GPU/embedding passes run at concurrency 1.

### Per user flow (which models fire)
| user action | models | live/bg | who pauses/yields |
|---|---|---|---|
| **Dictate a note** (Space) | VAD + ASR | LIVE (CPU) | background embed + LLM PAUSE while mic armed |
| **Search** (type a query) | LLM parse + text-embed (query) + CLIP-text (if visual / few results) | LIVE | interactive parse PREEMPTS background LLM |
| **Import / ingest a shoot** | CLIP image-embed + text-embed (notes); LLM caption is SPEC-ONLY (M3, not wired) | BACKGROUND | all yield to voice + interactive; serialized concurrency 1 |
| **Stop reviewing** (30-min idle / session close) | LLM session + per-image summaries - SPEC-ONLY (storage + search consume them; generation not wired yet, M3) | BACKGROUND | yields to voice + interactive |
| **Open Visualizer** | precomputed CLIP + text vectors only (no live model) | precomputed | no "digest" feature exists; see note below |

> Build-status caveat (verified): of the BACKGROUND LLM jobs above, only query-parse is
> actually wired today. Captions (`PassName::Caption`) are a registered pass with no
> runner (M3). Summary GENERATION is unwired (the `derived_summaries` table, FTS,
> `image_summary` vectors, and S3 search all exist and would consume them - nothing
> produces them yet). "Digest" is not a feature - it is UI shorthand for ingest
> progress counters ("hashing 12 / previews 480"), unrelated to summaries. The
> priority ladder is real and wired; GPU embed EPs are landing per the matrix above.

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
(priority ladder + mic-pause + preemption), and the EP/accelerator tables above decide
WHERE (which chip). That is why ASR-on-CPU + background-pause-during-capture means the
live mic never fights the GPU/LLM.

## Where the normative truth lives

`spec/RUNTIME.md` (S3 serving + the CPU-default / GPU-opt-in EP plan + llama.cpp
per-platform vendoring; S6 tiers + the Tier-0 floor; S9 scheduler) and `docs/MODELS.md`
(per-seam picks). THIS file is the consolidated matrix - keep it in sync when an EP
lands or a model changes. Detail per seam lives in the SPIKE / PLAN docs linked above.
