# Performance / SOTA gap audit (June 13 2026)

A gap analysis of PhotoProof's performance-critical paths: our ACTUAL stack vs.
2025-2026 state-of-the-art, with named libraries and expected wins. Produced by
mapping the codebase (the scout) + a cited, adversarially-verified web research
pass (25 sources, 99 claims extracted, 25 verified by 3-vote, 22 confirmed / 3
refuted). Sources listed per item. Numbers are directional - most benchmarks are
non-Apple hardware (noted), so magnitudes shift, directions hold.

Ranked by impact. The two refuted-claim cautions and the open questions are kept
for honesty - act on the spikes, not on unvalidated magnitudes.

## 0. The one thing to check FIRST (gates #4 and #5)

Does Tauri's macOS **WKWebView** support **OffscreenCanvas, Web Workers +
`createImageBitmap`, and WebGL / WebGPU**? Several recommendations below
(off-main-thread thumbnail decode, WebGL graph rendering, GPU layout) are only
reachable if the webview exposes them. WebGL is broadly supported in WebKit;
OffscreenCanvas landed in Safari 16.4 (2023); WebGPU in Safari is newer/partial.
VERIFY on our actual Tauri target before building on them. Open question, not a
settled fact.

## 1. On-device ML inference - HIGHEST PRIORITY

**We do:** Rust `ort` (ONNX Runtime 2.0), **CPU execution provider only**, 4
intra-op threads, int8 ONNX models (DFN5B CLIP ViT-H-14-378 2.7GB, EmbeddingGemma
-300m), embedders sequential (concurrency 1). Measured live: ~20 CLIP images/min.

**SOTA / change:** Enable ORT's **CoreML execution provider** (the **MLProgram**
backend, NOT legacy NeuralNetwork) so the model runs on the Apple Silicon GPU/ANE
instead of CPU. **Immich** - a production self-hosted photo app on the same
ort/ONNX stack - shipped the CoreML EP in v2.2.0 (PR #17718). The legacy
NeuralNetwork backend casts FP32->FP16 and can flip predictions; MLProgram avoids
that. Also worth a bake-off: Apple **MLX** and native **Core ML** / **GGML** as
alternative runtimes for these embedders.
- Confidence: high that the EP exists + the MLProgram caution. **REFUTED (1-2):**
  the claim that it applies specifically to Immich's CLIP/face models - so
  CLIP-class applicability is UNVALIDATED.
- Expected win: GPU/ANE over 4-thread CPU is typically a large multiple, but the
  exact ViT-H speedup on Apple Silicon is UNMEASURED (open question), and int8
  surviving CoreML conversion without accuracy loss is unverified.
- ACTION: spike the CoreML EP (MLProgram) on our ViT-H + text model; measure
  images/min and embedding accuracy vs. the CPU baseline before committing.
- Sources: github.com/immich-app/immich/pull/17718 ; ym2132.github.io
  ONNX_MLProgram_NN_exploration ; cactuscompute.com/compare/coreml-vs-mlx

## 2. Image decode / resize / encode - HIGH, low-risk

**We do:** Rust `image` 0.25 + `rawler` 0.7 (bilinear Bayer demosaic), CatmullRom
resize, libwebp method-2 WebP, sequential per image.

**SOTA / change:** Replace the `image`-crate resize with **`fast_image_resize`**
(NEON SIMD on ARM64). Benchmarked Lanczos3: **62.16 ms vs 433.80 ms** for the
`image` crate (~7x), even beating libvips (88.65 ms). And **`rawler` 0.7.2 ships a
PPG demosaic** (higher quality than bilinear) - swap `demosaic_bilinear_rggb` in
`raw_develop.rs`. Lanczos3 via fir is the quality+speed pick over CatmullRom.
- Confidence: high (3-0 verified).
- Expected win: ~7x on the resize step (directional - benchmark is Neoverse-N1,
  not Apple Silicon).
- ACTION: drop-in `fast_image_resize` for the preview tiers; switch the demosaic
  to PPG. Lowest-risk, highest-certainty win here.
- Sources: github.com/Cykooz/fast_image_resize benchmarks-arm64.md ;
  sharp.pixelplumbing.com/performance ; github.com/dnglab/dnglab releases

## 3. Vector search - CORRECT NOW, scale-gated upgrade

**We do:** brute-force linear cosine over int8, MRL-truncated 512-dim, mmap,
rayon above 4096 rows, no ANN index.

**SOTA / verdict:** **Our choice is right at current scale.** arXiv 2409.06464:
negligible difference flat-vs-HNSW under ~100K vectors; the Qdrant rule-of-thumb
crossover is ~10k (2-1 split, secondary source). Toward 100k+ adopt **USearch**
HNSW (int8: 274,653 QPS vs 171,856 f32 at 98.9% recall@1) - or `hnswlib` /
`sqlite-vec`.
- Confidence: high. **REFUTED (1-2):** the "~10x slower past 1M" clean
  justification - do not cite it.
- ACTION: NO change now. Add a scale trigger: when a library crosses ~tens of
  thousands of images, benchmark the int8 brute-force scan on M-series and adopt
  USearch HNSW if the latency contract (<100ms) is threatened. Validate the real
  crossover empirically.
- Sources: arxiv.org/pdf/2409.06464 ; github.com/unum-cloud/usearch BENCHMARKS.md

## 4. Force-directed graph (Visualizer) - HIGH for scale (gated by #0)

**We do:** velocity-Verlet, **all-pairs O(N^2)** repulsion, **Canvas 2D**, on the
**main thread**, up to thousands of thumbnail nodes. No Barnes-Hut, no worker, no
WebGL.

**SOTA / change:** Main-thread Canvas 2D sustains ~30fps to **~5,000 nodes**
(WebGL ~7,000; PMC12061801). Beyond that: **WebGL rendering** (Sigma.js) + **GPU
layout** (**cosmos.gl** runs layout AND render on the GPU to 1M+ nodes) + a
**Barnes-Hut / quadtree O(N log N)** force step, with the sim in a **Web Worker**.
A WebGPU bottom-up quadtree ran 95k nodes in 5.48 ms.
- Confidence: high. **REFUTED (1-2):** the framing that all web graph libs are
  CPU-only/poor - several are GPU-accelerated.
- Expected win: O(N^2)->O(N log N) layout + GPU render lifts the ceiling from
  ~5k to 100k-1M nodes; FPS numbers are RTX 3060/Chrome, not WKWebView (verify #0).
- ACTION: gated on #0. If WebGL is available, adopt Sigma.js or cosmos.gl + a
  Barnes-Hut step in a worker. Cheaper interim: move the existing O(N^2) sim into
  a Web Worker so it stops blocking the main thread.
- Sources: pmc.ncbi.nlm.nih.gov/articles/PMC12061801 ; openjsf.org cosmos-gl ;
  sigmajs.org

## 5. Ingest pipeline + thumbnail loading - MEDIUM (gated by #0 for thumbs)

**We do:** BLAKE3 (rayon + mmap - already fast) but passes run **sequentially
between stages** (hash->exif->preview->embed), parallel only within a stage.
Frontend decodes thumbnails via DOM `<img>` **on the main thread**, 12-concurrent
viewport queue, no grid virtualization, no `createImageBitmap`/worker.

**SOTA / change:**
- Ingest: **pipeline/overlap** the stages (a stage-N item embeds while stage-N+1
  items preview) with bounded channels (crossbeam/flume) for backpressure. BLAKE3
  is already SOTA; the win is overlap, not the hash.
- Thumbnails: **decode off the main thread** - `createImageBitmap` in a Web
  Worker (ImageBitmap is transferable; post it back for `drawImage`/bitmaprenderer)
  - and **virtualize the grid** (render only visible cells).
- Confidence: high (3-0 verified).
- ACTION: pipeline the ingest passes; (gated on #0) move thumbnail decode to a
  worker + virtualize the grid.
- Sources: github.com/ydaniv/offthread-image ; MDN createImageBitmap ;
  oneuptime.com high-throughput-rust-pipeline ; perfplanet non-blocking-image-canvas

## What is already good (don't touch)

BLAKE3 hashing (mmap + rayon by size threshold); int8 + Matryoshka-512 vector
truncation; content-addressed immutable preview caching; the viewport-priority
thumb queue; module-scoped graph thumbnail + affinity caches; the throwaway
eval-library design; libwebp method-2 (chosen post-pp-bench for artifact size).

## Open questions (validate before building)

1. Measured CoreML EP (MLProgram) speedup over 4-thread CPU ort for the 2.7GB
   ViT-H int8 on Apple Silicon, and does int8 survive CoreML conversion without
   accuracy loss?
2. Does Tauri WKWebView support OffscreenCanvas, Web Workers + createImageBitmap,
   WebGL/WebGPU? (Gates #4, #5.)
3. Would Apple MLX / native Core ML / GGML beat the ort CoreML EP, justifying
   leaving the ort pipeline?
4. On M-series, where does int8 MRL-512 brute-force actually cross over to justify
   USearch HNSW - near 100k or higher?

## Priority order

1. CoreML EP spike (#1) - directly fixes the embedding bottleneck we hit live.
2. `fast_image_resize` + PPG demosaic (#2) - cheapest certain win.
3. WKWebView capability check (#0) - unblocks #4/#5.
4. Ingest pass pipelining (#5, backend, no webview dep).
5. Graph -> worker, then WebGL/Barnes-Hut if #0 allows (#4).
6. USearch HNSW - deferred until the scale trigger (#3).
