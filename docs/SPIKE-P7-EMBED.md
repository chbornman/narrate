# SPIKE P7-EMBED — embedder bake-off, MacBook half (June 12, 2026)

Apple Silicon (founder MacBook, unified memory), onnxruntime 1.26 CPU EP,
4 intra-op threads (the app's spike-P6.3 CPU posture). Harness:
`~/spike-p7-embed/{clip_bench.py,text_embed_bench.py}` (throwaway; this
report is the durable artifact). Quality checks are SANITY-grade by design
— the real golden-query eval runs post-dogfood on the founder's annotated
library (RETRIEVAL section 5.3 / BACKLOG "Lighting up M3").

## Verdicts

1. **Text embedder (the primary retrieval signal, K15): EmbeddingGemma-300m
   quantized (q8) wins.** Better paraphrase separation (mean margin +0.310
   vs Qwen3's +0.227 measured fairly, EOS appended), half the dims (768 vs
   1024 = half the PPVEC bytes per chunk), 316 MB vs 596 MB download, and
   8/8 top-1 on the retrieval sanity set (both models scored 8/8). Its
   85 ms/note throughput is 4x slower than Qwen3 but irrelevant at journal
   pace: a 50k-note backfill is ~71 minutes of background CPU.
   Qwen3-Embedding-0.6B int8 stays the configured alternative.
2. **DFN5B CLIP (ViT-H-14-378, the K15 choice) is feasible on M-series but
   heavy: image-side backfill belongs to idle hours or the desktop.**
   2.96 s/image at 4 CPU threads means ~41 h for 50k images on the laptop
   CPU. Query-side text embedding is 151 ms — fine interactively. Peak RSS
   during visual inference 4.9 GB; the session alone ~2.7 GB. Follow-ups
   recorded: try the CoreML execution provider (likely large win), and the
   desktop CUDA half (spike session 2) for tier-2 numbers.
3. **Zero-shot structure verified against real pixels.** Eight images from
   test-corpora/jpeg-sample classified against six scene probes; two
   spot-checked by eye: the spaniel-on-a-train-seat image scored
   "a dog or animal" 0.249 vs 0.011-0.093 for the rest; the trail-race
   image scored "a person". Confident, discriminative, correct.

## Numbers

| Model | Load | RSS | Throughput | Dim | Sanity |
|---|---|---|---|---|---|
| DFN5B visual (fp32, external data) | 4.5-10 s | 2.7 GB session, 4.9 GB peak | 2.96 s/image (378 px, batch 1) | 1024 | zero-shot correct (eye-verified x2) |
| DFN5B textual | 1.6 s | (shared) | 151 ms/query | 1024 | - |
| EmbeddingGemma-300m q8 | 0.1 s | ~0.5 GB (est; process-cumulative maxrss masks it) | 85 ms/note | 768 | top1 8/8; margin +0.310 |
| Qwen3-Embedding-0.6B int8 | 0.4 s | 1.19 GB | 20 ms/note | 1024 | top1 8/8; margin +0.227 |

Method notes: paraphrase margin = cos(paraphrase pair) - cos(unrelated
pair) over four photographer-journal-style pairs; retrieval = 8 queries vs
40 synthetic journal notes. Qwen3 used last-token pooling with EOS
appended and the model-card instruction prefix on queries; EmbeddingGemma
used mean pooling with its documented "task: search result | query:" /
"title: none | text:" prompts. Both exports demand causal-LM-style
`past_key_values.*` inputs - feed zero-length caches (integration trap,
same class as silero's context-prepend).

## Pins (for the manifest, wiring packet)

Revisions are HF repo SHAs at download time; file SHA-256 below.

immich-app/ViT-H-14-378-quickgelu__dfn5b @ a5925c6e44f6381544a7263296662135ff4df0ff
  visual/model.onnx   b291734c23e5f5ed70fef7bf564ff7069e65d066b0b070f04f9fbccc7daa2400  (616176 bytes - GRAPH ONLY)
  textual/model.onnx  f6fc8b3945c0c2d82e134a0bddc178e3b9c482b80ec4adc9531b56949cc6b923  (1417076707)
  NOTE: the visual export uses ONNX external data - ~100 sibling weight
  files (visual/visual.transformer.resblocks.* etc). The manifest pin for
  this model must either enumerate every external file (mechanical,
  generated) or the wiring packet converts to a single-file ONNX at
  install time and pins that conversion's output. Decide at wiring time;
  enumeration is the fail-closed-faithful option.

onnx-community/embeddinggemma-300m-ONNX @ 5090578d9565bb06545b4552f76e6bc2c93e4a66  (CHOSEN)
  onnx/model_quantized.onnx       172efde319fe1542dc41f31be6154910b05b78f7a861c265c4600eec906bd6d8  (567874)
  onnx/model_quantized.onnx_data  705626e28e4c23c82ade34566b4197d97f534c12275fa406dfb71e9937d388c0  (308890624)
  tokenizer.json                  4dda02faaf32bc91031dc8c88457ac272b00c1016cc679757d1c441b248b9c47  (20323312)

onnx-community/Qwen3-Embedding-0.6B-ONNX @ c25a394dd583836952667c12f008335071b3f43d  (ALTERNATIVE)
  onnx/model_int8.onnx  6d0ea863f78b4a84afa3c7fcba1ec341572b5e28121aef77b7092b1dfdf679c7  (613527539)
  tokenizer.json        def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a  (11423705)

## What the wiring packet needs (next build)

1. Manifest: replace the UNPINNED-P6.3 embedder entries - text embedder
   becomes embeddinggemma-300m-q8 (chosen, tiers [1,2]); qwen3 entry stays
   as role alternative; DFN5B entry pinned per the external-data decision
   above. Licenses: EmbeddingGemma is Gemma-licensed (acceptance_required,
   same flow as the LLM); DFN5B is apple-ascl via Immich repo; Qwen is
   Apache-2.0.
2. A real Embedder connector (in-process ort, like silero's carve-out, or
   a child process - RUNTIME section 3 decides): mean/last pooling,
   prompts, the zero-length-KV trap, tokenizer loading.
3. The P7.1 ingest passes start finding a live embedder; PPVEC fills;
   STATUS.md mock-only retrieval rows flip.
4. Schedule the image backfill politely: idle-hours pacing on laptops
   (2.96 s/image is a space heater), or advise running it on the desktop.

## Open for spike session 2 (desktop, RTX 5080)

CUDA EP throughput for DFN5B (expect 2-3 orders faster than laptop CPU),
tier-2 RAM/VRAM calibration, the full RUNTIME 12.4 concurrency matrix,
CoreML EP attempt for the MacBook backfill path.
