# SPIKE-MLX-COREML-TEXT -- Apple-native runtimes (MLX, native Core ML) for the EmbeddingGemma TEXT embedder (June 14, 2026)

Follow-up to `docs/SPIKE-COREML-TEXT.md`. That spike found the **ort -> CoreML
EP bridge** put only ~3% of the EmbeddingGemma graph on the accelerator and ran
0.48-0.64x SLOWER than int8/CPU. The hypothesis here: an **Apple-NATIVE** path
(Apple's own MLX, or a native `.mlpackage` Core ML conversion) might map the
WHOLE transformer to the ANE/GPU and win. We measured both, honestly, on this
M1 Pro Mac.

Machine: Apple Silicon M1 Pro (8 perf + 2 eff cores), unified memory. The
reference behavior matched is the connector's `OrtEmbedder::run_text` MeanPooled
path: Gemma document/query prompts, `add_special_tokens=True` (`<bos>..<eos>`),
mean-pool the RAW `last_hidden_state` (dim 768, NO SentenceTransformers Dense
projection), L2-normalize. Corpus: 199 short photography-journal note chunks +
queries (11-25 tokens, mean ~18), tokenized with the SAME shipped
`tokenizer.json` so cosine-vs-reference is meaningful.

## VERDICT: native Apple paths DO beat int8/CPU on speed (MLX ~4x single, ~24x batched) -- but the WIN IS ON THE GPU, NOT THE ANE, and the seam is not a bottleneck. RECOMMENDATION: DON'T-SHIP now (the Rust-integration cost is not worth it for a non-bottleneck seam), but MLX is the path IF text-embed ever becomes hot.

Two true findings, held together:

1. **The speed hypothesis is CONFIRMED.** MLX (Metal GPU) does ~73-85 embeds/s
   single-item vs int8/CPU's ~18.6, i.e. **~4x**, at cosine 0.996 vs the int8
   reference (retrieval-safe). Native Core ML (fp32) matches it (~65/s, 0.997).
   Unlike the ort bridge, these run the WHOLE transformer on the accelerator.
2. **The ANE hypothesis is FALSE.** The native path does NOT map the transformer
   to the ANE. Core ML's own compute plan places 96-100% of ops on the **GPU**;
   the ANE takes only 3.8% (fp16) -- essentially the same ~3% the ort bridge
   got. EmbeddingGemma's RMSNorm / attention / rotary ops are not ANE-mappable,
   so "native" buys a GPU placement, not an ANE one. And the only way to coax
   more onto the ANE (fp16) CORRUPTS the embedding (cosine drops to 0.81).

So the win is real but it is a **GPU win**, and MLX delivers that GPU win with
far less integration risk than Core ML. The reason to still say DON'T-SHIP is
cost vs. benefit: text-embed at ~18-21/s is NOT the ingest bottleneck (the CLIP
visual tower is, already fixed 8.8x in `docs/SPIKE-COREML.md`), so spending a new
in-process Rust runtime + a re-embed + a second vector space to speed up a
non-bottleneck is not justified TODAY.

## The numbers (per-path, single + batched, accuracy, ANE utilization)

All on the 199-text corpus, mean-pooled dim 768, L2-normalized. Single-item is
our REAL usage (one note chunk / one query at a time); batched is the
GPU-favorable case. Accuracy = mean cosine vs the int8/CPU reference vectors.
The int8/CPU baseline was re-measured in THIS session (~18.6/s) and is also
quoted at the prior spike's 20.7/s; ratios below use the prior spike's 20.7 as
the canonical baseline.

| Path | runtime / device | single-item embeds/s | batched embeds/s | vs int8/CPU (single) | accuracy (cosine vs int8) | accelerator utilization |
|---|---|---|---|---|---|---|
| **int8/CPU (shipped default)** | ort CPU EP | **~18.6-20.7** | n/a | 1.00x (reference) | reference | 100% CPU |
| ort -> CoreML, dynamic (prior spike) | ort CoreML EP | 13.2 | n/a | 0.64x (slower) | ~0.997 (when finite) | ~3% accel, 24 partitions, NaN-prone |
| ort -> CoreML, static pad64 (prior spike) | ort CoreML EP | 10.0 | n/a | 0.48x (slower) | NaN-prone | ~3% accel |
| **MLX, bf16** | mlx-embeddings, **GPU (Metal)** | **~73-85** | **~430-480** (bs 32/64) | **~3.9x (faster)** | **0.996** (min 0.993) | GPU; MLX has no ANE backend |
| Native Core ML, fp16 | coremltools .mlpackage | ~68 | -- | ~3.6x | **0.81 (min 0.76) -- UNSAFE** | **96.2% GPU + 3.8% ANE** |
| **Native Core ML, fp32** | coremltools .mlpackage | **~65** | **~207** (bs 32) | **~3.5x** | **0.997** (min 0.995) | **100% GPU** |

Reading the table:

- **MLX is the best native path**: fastest single-item (~73-85/s), fastest
  batched (~450/s), retrieval-safe accuracy (0.996), simplest to reproduce. It
  runs on the GPU (`mx.default_device()` -> `Device(gpu,0)`; MLX exposes no ANE
  backend at all).
- **Native Core ML matches MLX on single-item but only at fp32.** The fp16
  conversion (the ANE-eligible precision) drops cosine to **0.81** -- the same
  fp16 RMSNorm instability the prior ONNX spike flagged (reason #3 there),
  reproduced on the native path. fp32 is retrieval-safe (0.997) but then the
  package is ~1.1 GB and runs 100% on the GPU. Core ML batched (207/s) is less
  than half MLX's (450/s).
- **The ANE never shows up.** The whole point of trying native was "maybe the
  ANE takes the transformer." It does not: 96.2% GPU / 3.8% ANE at fp16, 100%
  GPU at fp32. That 3.8% is the same order as the ort bridge's ~3%. The
  transformer body simply is not an ANE-shaped graph.

### How the ANE-utilization number was obtained

Core ML's own compute plan, read per-op via
`MLComputePlan.load_from_path(...).get_compute_device_usage_for_mlprogram_operation(op)`
over the compiled `.mlmodelc` (the `.mlpackage` had to be compiled first via
`_MLModelProxy.compileModel` -- the Python compute-plan API on coremltools 9.0
crashes if handed the uncompiled package). This is strictly more authoritative
than the ort bridge's `GetCapability` node-count dump: it is Core ML reporting
where it WILL run each op. Result: GPU dominates, ANE is a rounding error.

## Rust-integration cost analysis (the deciding factor)

The connector is **in-process Rust** today (`ort` 2.0.0-rc.12, the §3.3 defended
exception). Any native path must either link an in-process Rust runtime or add a
subprocess. Assessed options:

| Option | What it is | Maturity (June 2026) | Effort / risk |
|---|---|---|---|
| **mlx-rs** (`mlx-rs` 0.25.3, oxideai) | Rust bindings to Apple MLX + an early `mlx-lm` crate | "in active development, API may change"; ~60k downloads, 341 stars, last release Dec 2025. LOW-LEVEL array/ops binding; the high-level model loaders (mlx-lm-rs) are nascent. | **HIGH.** We would re-implement the gemma3 forward + mean-pool in Rust against an unstable array API, OR port mlx-embeddings' Python model by hand. Weight loading, the bidirectional mask, and rope all become our code. New non-trivial dep on a pre-1.0 crate, macOS-only. |
| **Native Core ML via objc2-core-ml** (0.3.2, mature FFI) | objc2 FFI to the Core ML framework; load the `.mlpackage`, `MLModel.predict` | objc2-core-ml is MATURE (0.3.2, ~92k downloads, maintained). But it is raw Obj-C FFI: we hand-build `MLFeatureProvider`/`MLMultiArray`, marshal tokens in and `last_hidden_state` out, mean-pool in Rust. | **MEDIUM-HIGH.** FFI marshalling is fiddly and unsafe; fp32 (the only safe precision) means a 1.1 GB model + GPU-only placement (so no ANE benefit anyway). Plus a conversion-pipeline dependency (coremltools, torch) to PRODUCE the `.mlpackage` ships nothing but adds a build artifact to manage. |
| **Sidecar subprocess** (Python mlx-embeddings) | A small Python process the host talks to over stdio/IPC | Trivially available (this spike IS that, minus the IPC) | **MEDIUM.** Avoids in-process-runtime risk but breaks the "in-process Rust" posture the connector deliberately holds, adds a Python runtime to the desktop bundle (packaging + startup + a second process to supervise), and the IPC hop eats into the speedup on single-item calls. Architecturally a regression for a non-bottleneck. |

Across all three, the integration is non-trivial and macOS-only, and it buys a
GPU speedup on a seam that is already fast enough. The CLIP path justified its
Core ML cost because the visual tower (a) was the real bottleneck and (b) Core
ML took the WHOLE conv/matmul graph onto the accelerator (8.8x). Neither holds
here: text-embed is not the bottleneck, and the accelerator placement is "GPU,
same as MLX", not "ANE".

## Recommendation

**DON'T-SHIP for now. int8/CPU stays the shipped text-embed path.** It is fast
enough (~18-21/s on a non-bottleneck seam), already the space every PPVEC vector
was embedded under, zero new deps, cross-platform.

**IF text-embed ever becomes a measured bottleneck** (e.g. a giant
journal-backfill where ~20/s hurts), the path is **MLX**, not Core ML:
- MLX is the fastest AND simplest native option (best single AND batched,
  retrieval-safe at bf16, no fp16 corruption, no 1.1 GB fp32 package).
- Integrate via a **sidecar** first (lowest risk, the Python mlx-embeddings
  process already works), and only move to in-process `mlx-rs` once that crate
  stabilizes past 1.0. Do NOT pursue native Core ML for this seam: it is no
  faster than MLX, its safe precision (fp32) is GPU-only and huge, and its
  ANE-eligible precision (fp16) corrupts the embedding.
- Whichever path: it changes the vector space (cosine ~0.996 vs int8, NOT
  identical), so it requires a full re-embed of the corpus, same as any model
  swap. Budget that as part of the "ship it" cost.

## Reproduce (this machine)

Python prototypes only; no Rust was added (gate unaffected). Two venvs because
the runtimes split across Python versions:

- **MLX + int8 reference**: Python 3.14 venv, `pip install onnxruntime numpy
  mlx mlx-lm mlx-embeddings tokenizers`. `ref_int8.py` builds the reference
  vectors + baseline; `mlx_bench.py` runs MLX single + batched + accuracy. MLX
  model: `mlx-community/embeddinggemma-300m-bf16` (HF).
- **Native Core ML**: Python 3.12 venv (coremltools 9.0's native runtime has no
  3.14 wheel), `pip install coremltools torch==2.7.0 transformers==4.57.1 numpy
  tokenizers`. `coreml_convert.py` traces `unsloth/embeddinggemma-300m` (open
  mirror of the gated base model) and converts to a `.mlpackage`;
  `coreml_bench.py` measures speed + accuracy + the compute plan.

Conversion gotchas worth recording (cost the most time):
- transformers 5.x / 4.57 build the attention mask with a vmap
  `autograd_function` that does NOT survive `torch.jit.trace`
  (`RuntimeError: unordered_map::at: key not found`). Fix: pass a precomputed
  **4D additive mask built with plain tensor ops**, bypassing the mask builder.
- coremltools 9.0's torch frontend trips on some Gemma3 `aten::Int` shape ops
  (`only 0-dimensional arrays can be converted to Python scalars`). Fix: a small
  monkeypatch of `_cast` to coerce 0-d arrays to scalars.
- `MLModel(pkg, compute_units=...)` and `MLComputePlan.load_from_path(pkg)`
  CRASH the process on the raw `.mlpackage` (ct 9.0, `coremldata.bin not a valid
  .mlmodelc`). Load the model with the DEFAULT compute units (already ALL), and
  compile to `.mlmodelc` via `_MLModelProxy.compileModel` before the compute
  plan.

A retrieval-safe fp32 `.mlpackage` (~1.1 GB) is left staged at
`models/embeddinggemma-300m-coreml-fp32/model.mlpackage` for a possible
follow-up; it is NOT committed and nothing in the app selects it. The fp16
package and batched scratch variants were deleted (fp16 is accuracy-unsafe).

## Constraint check

- **No production model-selection wired.** This is a measurement spike. The text
  embedder still loads via `OrtEmbedder::text(coreml=false)` on int8/CPU; no
  model id selects an MLX or Core ML EmbeddingGemma. The DON'T-SHIP verdict is
  the reason to leave it that way.
- **No model files committed.** The staged fp32 `.mlpackage` lives only under the
  app-data `models/` dir. The MLX bf16 weights live in the HF cache. Scratch
  venvs were removed.
- **Gate green / no Rust added.** No Rust code was written for this spike
  (Python prototypes sufficed), so `cargo fmt --check` passes and the connector
  build is untouched. No em-dashes in this doc.
