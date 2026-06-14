# Performance implementation plan (ordered)

The build plan for the gaps in `docs/PERF-AUDIT.md`. Derived from: the SOTA audit
(what to change), a codebase recon (where it lands + blast radius), and cited
implementation-gating research (the exact APIs/versions, adversarially verified).
Dependency-ordered. Each item states where, the specific API, effort/risk, the
expected win, how to validate, and any gating. Magnitudes are DIRECTIONAL - the
spikes measure the real numbers on our hardware.

## Sequencing rationale

- **P1 is certain + isolated** -> do now.
- **P2 (CoreML EP) is the highest upside but MEASURE-FIRST** (it can even be
  slower if the graph fragments) -> spike in parallel with P1.
- **P3 (graph sim -> Worker) is NOT webview-version-gated** (Workers are
  universal) and serves the "buttery smooth" Visualizer goal -> do it.
- WebGL *render* and off-thread decode ARE version-gated (WebGL2 = Safari 17 /
  Sonoma; WebGPU = Safari 26 / Tahoe in WKWebView) and partly redundant (the grid
  is ALREADY virtualized) -> later / optional, behind feature-detection.
- Ingest pipelining (bounded win, hard preview->embed dependency, rewrites
  crash-recovery) and HNSW (scale-gated) -> deferred.

## P1. Preview-tier `fast_image_resize`  [DO FIRST - certain, isolated]

- Where: `crates/photoproof-core/src/library/preview.rs::resize_to_edge` (~750),
  called twice in `write_artifacts` (display 2560 -> thumb 512).
- How: add `fast_image_resize` (with its `image` Cargo feature for
  DynamicImage/RgbImage interop, image crate 0.25.6+). Replace the
  `img.resize(.., FilterType::CatmullRom)` body with a `Resizer` +
  `ResizeAlg::Convolution(FilterType::CatmullRom)` (same filter -> near-parity) or
  `Lanczos3` (sharper). Keep the `resize_to_edge(&DynamicImage, u32) -> DynamicImage`
  signature so no caller changes.
- Effort: LOW. Risk: LOW (pure isolated fn).
- Win: ~7x on the resize step (Neoverse-N1 bench; directional on M-series).
- Validate: the preview-artifact reproducibility tests; A/B CatmullRom (parity)
  vs Lanczos3 (sharper) through `pp-bench` for size + speed.
- Gating: none.

## P2. CoreML execution-provider spike  [HIGHEST UPSIDE - measure-first]

Fixes the embedding bottleneck we hit live (~20 img/min CLIP on CPU).
- Where: `crates/photoproof-connectors/src/ort_embedder.rs::build_session` (~321),
  between `with_intra_threads` and `commit_from_file`. Cargo: add the `coreml`
  feature to `ort` (currently `2.0.0-rc.12`, features `["half"]`).
- How: register the EP on the SessionBuilder:
  `...with_execution_providers([CoreMLExecutionProvider::default()
  .with_compute_units(ComputeUnits::CPUAndNeuralEngine)
  .with_model_format(ModelFormat::MLProgram)  // Core ML 5+/macOS 12+, NOT legacy NeuralNetwork
  .with_subgraphs(true).build()])?`
  ort registers EPs in order and SILENTLY falls back to CPU per-op, so a missing
  CoreML never crashes - but make the EP a config knob (CPU vs CoreML) so it ships
  off-by-default until validated. PIN the rc version and verify the builder
  against `src/ep/coreml.rs` (the API is drifting: `.with_ane_only()` became
  `.with_compute_units(...)`; `.with_subgraphs` now takes a bool).
- CRITICAL - FP16 not int8: the CoreML EP's int8/quantized handling is
  undocumented and quantized ops likely fall to CPU (defeating the point).
  RE-EXPORT the DFN5B visual + EmbeddingGemma towers as FP16 ONNX for the CoreML
  path; keep the int8 models for the CPU fallback. Feeding FP16 maximizes ANE/GPU
  op coverage.
- Effort: MEDIUM (EP wiring + FP16 re-export + the config seam). Risk: MEDIUM -
  the EP partitions the graph and could be SLOWER if our ViT-H fragments badly
  (the documented Pad-fallback case made 14 partitions). This is why it is a SPIKE.
- Win: potentially a large multiple (ANE/GPU vs 4-thread CPU) - UNMEASURED.
- Validate: (a) images/min vs the CPU baseline; (b) embedding cosine-similarity vs
  the CPU FP32 reference on the SPIKE-P7-EMBED corpus + a retrieval sanity on the
  COCO/golden sets; (c) `ProfileComputePlan` to see the ANE-vs-CPU op split.
- Decision gate: ship CoreML ONLY if the measured speedup is real AND retrieval
  accuracy holds; otherwise keep CPU. MLX/Core ML rewrite is NOT justified (the
  only Rust MLX bindings, mlx-rs, are unofficial/pre-1.0 with no CLIP examples).

## P3. Graph sim -> Web Worker  [Visualizer smoothness - not version-gated]

- Where: `apps/desktop/src/lib/logic/forcegraph.ts::step()` is a PURE function over
  plain arrays (no DOM); `TopicGraph.svelte` rAF loop (~635) calls `step()` then
  `draw()`.
- How: run `step()` in a Web Worker (universally supported - no macOS floor). Post
  node/anchor state in, get positions + energy back; keep `draw()` (Canvas 2D) on
  the main thread. Use structure-of-arrays + transferable typed arrays to avoid
  per-frame copy cost.
- Effort: MEDIUM (async marshaling + SoA refactor). Risk: MEDIUM.
- Win: unblocks the main thread during layout (the "buttery smooth" goal) +
  headroom toward the ~5k-node Canvas-2D ceiling.
- Validate: the existing `forcegraph` unit tests still cover the pure `step()`; a
  frame-time check while a topic is added.

## P4. Demosaic -> PPG

- Where: `raw_develop.rs::demosaic_bilinear_rggb` (~561), called in `develop_cfa`
  (~231).
- How: swap to rawler 0.7.2's PPG demosaic behind the same signature. Bump
  `RAW_DEVELOP_VERSION` (the mechanism already exists) to invalidate cached
  full-res artifacts.
- Effort: MEDIUM. Risk: LOW-MEDIUM (cache invalidation is handled).
- Win: better demosaic quality (fewer zipper/maze artifacts), maybe speed.
- Validate: develop tests; visual spot-check on a few RAWs.

## P5. CLIP-preprocess `fast_image_resize`  [gated by re-validation]

- Where: `clip_preprocess.rs` 378x378 resize (~70), CatmullRom - correctness-LOCKED
  (byte-validated against OpenCLIP; the 0.310 paraphrase margin depends on it).
- How: use `fast_image_resize` with `FilterType::CatmullRom` (same filter), then
  VALIDATE the embedding eval (cosine parity + retrieval margin) before shipping.
  Convolution implementations differ subtly, so this is not a blind drop-in.
- Effort: MEDIUM. Risk: MEDIUM (embedding correctness).
- Win: speeds the embed preprocess (helps P2's throughput too).
- Validate: re-run the OpenCLIP parity tests + the retrieval sanity; ship only on a
  pass.

## P6. WebGL graph render  [gated: WebGL2 / Safari 17 / Sonoma]

- Where: `TopicGraph.svelte` Canvas-2D draw loop.
- How: move rendering to WebGL (Sigma.js, or custom regl/atlas). FEATURE-DETECT
  WebGL2 and fall back to Canvas 2D. Only worth it if P3 (worker sim) isn't enough
  at the target node count (~5k+ on Canvas 2D).
- Effort: HIGH (render rewrite). Risk: HIGH. Gating: WebGL2 (Sonoma+); detect.

## P7. Off-thread thumbnail decode  [small/optional - grid already virtualized]

- The grid is ALREADY virtualized (`gridlayout.ts` visible-window + DOM pool) and
  `Thumb.svelte` already uses `<img decoding="async">` (browser decodes off the
  main thread). So this is a control upgrade, not a fix: `createImageBitmap` in a
  Worker -> transfer the ImageBitmap -> draw to a canvas. Low priority; do only if
  scroll-decode jank is actually measured. Floor: OffscreenCanvas/WebGL2 Sonoma+
  (plain `createImageBitmap` is broader).

## P8. Ingest pass pipelining  [DEFERRED - bounded win, high risk]

The real dependency DAG: Hash || Exif (independent), Exif -> Preview (weak, just
orientation), but Preview -> ImageEmbed is a HARD dependency (embed reads the
preview artifact off disk). So the safe overlap is bounded, and it rewrites the
`process_queue` claim/drain orchestration plus the idempotency + crash-recovery
contracts. Defer unless ingest throughput becomes a measured bottleneck. (BLAKE3
hashing is already SOTA - mmap + rayon.)

## P9. USearch HNSW  [DEFERRED - scale-triggered]

int8 + Matryoshka-512 brute-force is CORRECT now (negligible vs HNSW under ~100k).
TRIGGER: when a library crosses ~tens of thousands of images, benchmark the
M-series brute-force scan against the <100ms contract and adopt USearch HNSW (int8
274k QPS vs 171k f32 @ 98.9% recall@1) if threatened. No work before the trigger.

## Frontend feature-detection (cross-macOS, since the webview is the system one)

Runtime-detect, NEVER assume by OS version: WebGPU (`navigator.gpu`) -> WebGL2
(`canvas.getContext('webgl2')`) -> Canvas 2D. Floors: OffscreenCanvas+WebGL2 =
Safari 17 / macOS Sonoma; WebGPU = Safari 26 / macOS Tahoe in WKWebView (Safari
feature flags do NOT enable it in a shipped app on older macOS). Workers +
`createImageBitmap` are broadly available.

## Recommended order

`P1 (now) || P2-spike (measure)` -> `P3 (graph worker)` -> `P4 (PPG)` -> then
`P5 / P6 / P7` only as warranted by measurements -> `P8 / P9` deferred.

## Already good - do not touch

BLAKE3 hashing (mmap + rayon by size); int8 + Matryoshka-512 brute-force vectors
(correct at current scale); content-addressed immutable preview caching; the grid
is ALREADY virtualized; the viewport-priority load queue; `<img decoding="async">`;
libwebp method-2 (chosen post-`pp-bench` for artifact size).

## Open questions the spikes resolve

1. Measured ViT-H CoreML speedup, FP16 vs int8, and the embedding accuracy delta
   vs the CPU FP32 reference (P2).
2. How badly our ViT-H partitions under the CoreML EP (profile) (P2).
3. Real M-series `fast_image_resize` numbers (measure in `pp-bench`) (P1).
4. Whether the worker-sim alone gets the Visualizer smooth enough, or P6 (WebGL)
   is needed (P3 first).
