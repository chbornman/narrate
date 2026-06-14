# docs/ — what each file is

One line per file so nothing here reads as randomly named. The normative
implementation contract lives in `spec/`, not here; where any doc disagrees
with a spec, the spec wins. Retired docs are listed at the bottom of
BUILD-LOOP.md with where their content went; full text stays in git history.

## Live (maintained continuously)

| File | What it is |
|---|---|
| `STATUS.md` | **The capability ledger** — every spec obligation, its build state (five states), and the evidence. Updated at every packet close. |
| `BUILD-LOOP.md` | The packet-grain build ledger: how packets run, gates, the status table, retired-docs list. |
| `BACKLOG.md` | Decided-but-not-scheduled work; items graduate into packets. Open items only — shipped items move to LANDED.md. |
| `ROADMAP.md` | The milestone-organized map of everything left to build: the navigational view over BACKLOG + STATUS + the spec milestone tags. |
| `LANDED.md` | The shipped-from-backlog archive: every `[x]` item, moved verbatim with its hashes and context — the de facto changelog of backlog-sourced work. |
| `FOUNDER-CHECKLIST.md` | Decisions awaiting Caleb + founder-machine verification pending. |

## Reference (stable; updated when their subject changes)

| File | What it is |
|---|---|
| `SCOPE.md` | The vision and architecture overview — the pitch, the problem, the shape of the product. |
| `FEATURES.md` | The milestone-tagged feature inventory the specs elaborate. |
| `UI-FEATURESET.md` | Normative UI addendum (desktop-conventions agreement); where UI.md is silent, this wins. |
| `UI-ARCHITECTURE.md` | Frontend architecture contracts (action registry, slices, guardrails) — frozen by FOUNDATIONS. |
| `DOGFOOD-M1.md` / `DOGFOOD-M2.md` | Founder-machine verification scripts: what to run, what to look at. |
| `architecture.html` | The one-page visual architecture (self-contained HTML+SVG): processes, models, seams, truth stores. Regenerate at packet close, beside STATUS.md. |
| `diagrams.html` | Seven Mermaid diagrams for understanding the system (self-contained HTML + one CDN script): process/truth-store architecture, the 5 models, capture flow, ingest pipeline, search fusion, the concurrency priority ladder, fallback chains. Complements architecture.html; honest about M3/planned parts. |
| `features.html` | The one-page cascading feature tree (self-contained HTML): everything BUILT, by product area, with honest partial markers. Regenerate beside STATUS.md. |
| `index.html` | The docs landing page (self-contained HTML): this catalog as a linked map — one row per doc, Live / Reference / spec/. |
| `MODELS.md` | The connector-options matrix: per seam, the current pick, alternates evaluated, candidates, watch triggers. |
| `SPIKE-ASR35.md` | Nemotron 3.5 dev evaluation + the chunk-size root-cause finding (B74). |
| `SPIKE-P6.3.md` | Model-spike findings and recipes (ASR/LLM/VAD pins, flags, measurements) — load-bearing for RUNTIME. |
| `SPEC-GAPS.md` | CLOSED historical record: the gap-id registry the spec status banners cite ("Closes E5"). Not a TODO. |
| `research/` | The cited pre-build research reports (archive). |

## Acceleration & runtime (the ML backend story)

The consolidated matrix plus the per-seam plans and measured spikes behind it.
Reproducible harnesses live in `scripts/` (`asr-ab.sh`, the `cuda_spike` /
`coreml_spike` connector tests).

| File | What it is |
|---|---|
| `RUNTIME-MATRIX.md` | **The authoritative WHERE/WHEN matrix**: best EP per model per machine, the scheduler priority ladder, and the fallback chains. Folds in every spike/plan finding below; keep in sync when an EP lands. |
| `PLAN-ORT-BLACKWELL.md` | Blackwell sm_120 unblock: load a hardware-matched onnxruntime via `load-dynamic`; CUDA FP16 on the 5080 (62.69x). |
| `PLAN-TENSORRT.md` | TensorRT EP on the 5080 (112.35x), the `tensorrt-cu12<11` recipe. |
| `PLAN-NVIDIA-LAUNCH.md` | Wiring the NVIDIA path into the running app (`ort_runtime.rs`, `ORT_DYLIB_PATH` / `LD_LIBRARY_PATH` staging). |
| `PLAN-VULKAN.md` | Cross-platform GPU: DirectML (Windows DX12) + WebGPU - NOT raw Vulkan (`ort` has no Vulkan EP). |
| `PLAN-GEMMA-MTP.md` | Gemma 4 multi-token prediction: CUDA-only speedup (~1.3-3x, prompt-dependent), regresses on Metal; staged for the 5080. |
| `PLAN-NEMOTRON-35.md` | The crate-bump path to Nemotron 3.5 ASR (NO-GO until the sherpa-onnx Rust crate ships it). |
| `PLAN-NEMOTRON-35-SIDECAR.md` | The LANDED path: 3.5 via the `parakeet-rs` engine behind `engine-parakeet`; §10 the GO, §11 the cross-machine latency/RSS A/B (gate PASSED). |
| `PLAN-PERF.md` | The performance plan: once CLIP is GPU-fast, decode/resize is the new ceiling - pool width + batching. |
| `PERF-AUDIT.md` | Performance / SOTA gap analysis: our actual stack vs 2025-26 SOTA, named libraries + expected wins. |
| `PLAN-P7.4-EMBEDDER-WIRING.md` | Plan to extend the supervisor's detect -> tier -> select -> fallback to the `ort` embedders. |
| `PLAN-RAW-DECODE.md` | On-demand RAW decode/develop + disk-cache plan. |
| `SPIKE-COREML.md` | FP16 CLIP on CoreML: 8.77x, COCO nDCG parity, the cache + single-file-export findings. |
| `SPIKE-COREML-TEXT.md` | Text-embed on CoreML: DON'T-SHIP (int8/CPU wins; the graph barely partitions to the ANE). |
| `SPIKE-MLX-COREML-TEXT.md` | Text-embed via native MLX: rejected for a non-bottleneck seam. |
| `SPIKE-P7-EMBED.md` | Embedder spike findings. |

## Design notes (feature-level explorations)

| File | What it is |
|---|---|
| `DESIGN-*.md` | Per-feature design explorations: `ATTENTION-HEATMAP`, `PREVIEW-POLICY`, `SEARCH-AS-SCOPE`, `SEMANTIC-GRAPH`, `STATION`, `TOPICS-COLLECTIONS`, `TUNING-CONFIG`, `TUNING-LOOP`, `VIEW-MODES`, `VOICE-SUBJECTS`. Pre-implementation thinking; the spec wins where they disagree. |
