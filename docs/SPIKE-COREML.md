# SPIKE-COREML — ONNX Runtime CoreML EP for the `ort` embedders (June 14, 2026)

PLAN-PERF item **P2**. Goal: wire ONNX Runtime's CoreML execution provider
behind a flag, MEASURE whether it speeds up the CLIP image embedder (the
~20 img/min CPU bottleneck) without hurting accuracy, and give a clear
ship / don't-ship / needs-FP16 recommendation. CoreML is **OFF by default**;
the CPU path is byte-identical to today and ships without CoreML.

Machine: Apple Silicon M-series, 10 cores (8 performance + 2 efficiency),
unified memory. `ort = 2.0.0-rc.12` (pinned; the CoreML builder API drifts
across rc tags). Models: the pinned DFN5B CLIP ViT-H-14-378 (1024-dim, int8)
and EmbeddingGemma-300m (768-dim, int8), at the app-data `models/` dir.

> Measurement noise caveat: a `pp-sweep search` (the COCO-1k beta sweep) was
> running concurrently at ~360% CPU during these measurements. That sweep is
> the TEXT-query search side (not image embedding), but it did contend for the
> performance cores, so the CPU img/s below is a conservative (slightly low)
> figure, not a clean-room best case. The CPU-vs-CoreML *conclusion* does not
> depend on the exact CPU number.

## VERDICT: DON'T-SHIP (as-is) — NEEDS an inlined-weights re-export first

CoreML cannot even **load** the DFN5B visual tower (the actual bottleneck) in
this onnxruntime build, and the int8 quantization means CoreML would fall back
to CPU even if it loaded. The wired-but-OFF code lands; turning it on is a
no-op-at-best / load-failure-at-worst today. Two concrete blockers, both must
clear before a CoreML retry is worth running:

1. **External-data load failure (hard blocker, the bottleneck tower).** The
   DFN5B visual export ships as ~397 sibling external-data files referenced
   relatively from `visual/model.onnx`. The CPU EP loads them fine. The CoreML
   EP mis-resolves the base path — it joins each external-data filename onto
   the model FILE path (`model.onnx/<tensor>`) instead of the model's
   directory, so the load dies with `open file ".../model.onnx/onnx__Add_5650"
   failed: Not a directory`. Confirmed from the model's own directory (cwd) too
   — same failure. **Fix: re-export the visual tower with weights INLINED into
   a single self-contained `model.onnx`** (no external data), or as one
   `.onnx` + one `.onnx_data` sidecar (the textual tower's single-file form
   loads; see below).

2. **int8 + CoreML = CPU fallback + pathological compile (the research call,
   confirmed-shaped).** Our models are int8. The CoreML EP does not accelerate
   int8 quantized graphs on the ANE; it partitions them to CPU. As a
   side-effect, compiling a large int8 graph to an MLProgram is extremely slow:
   loading the single-file 1.4 GB int8 textual tower under CoreML burned
   **10+ minutes of 100%-CPU compile and still had not finished (killed)** —
   the CPU EP loads the same file in seconds. So even after fixing the
   external-data blocker,
   an int8 CoreML run is expected to be a net LOSS (CPU-fallback inference plus
   a multi-minute per-session compile tax). **The path to an actual win is an
   FP16 re-export** (described below), not int8.

Net: the bottleneck (visual image embedding) gets **no CoreML speedup today**.
CPU stands at the measured baseline; CoreML is wired but should stay OFF.

## What was wired (the exact `ort` rc.12 API)

`crates/photoproof-connectors/Cargo.toml` — added the `coreml` feature:

```toml
ort = { version = "2.0.0-rc.12", features = ["half", "coreml"] }
```

`crates/photoproof-connectors/src/ort_embedder.rs::build_session` — the CoreML
EP is registered ONLY when the env knob `PHOTOPROOF_ORT_COREML` is truthy
(`1`/`true`/`on`/`yes`). Unset (the default) = today's CPU-only path, unchanged.
The registration chain (adjusted to the rc.12 API — `ep::CoreML`, not
`CoreMLExecutionProvider`; `with_compute_units` replaced `with_ane_only`;
`with_subgraphs` now takes a `bool`):

```rust
use ort::ep::CoreML;
use ort::ep::coreml::{ComputeUnits, ModelFormat};

builder.with_execution_providers([CoreML::default()
    .with_compute_units(ComputeUnits::CPUAndNeuralEngine)
    .with_model_format(ModelFormat::MLProgram)
    .with_subgraphs(true)
    .build()
    .error_on_failure()])
```

`.error_on_failure()` is deliberate: ort's default is to **silently** fall back
to CPU if an EP can't register, which would make a broken CoreML build look
like a passing spike. With it, a runtime that lacks the EP (or a model the EP
can't load) surfaces as an explicit `ConnectorError::Decode`, and the spike
also `eprintln!`s "registered CoreML EP for ..." so the path taken is visible.

## Does the linked onnxruntime even HAVE the CoreML EP?

**YES.** This was the headline open question (CoreML is a build-time
onnxruntime option; the prebuilt binary `ort` downloads might omit it). The
`coreml` feature pulls `ort-sys/coreml`, and the resulting prebuilt onnxruntime
**does** carry the CoreML EP:

```
[coreml-spike] CoreML EP present in linked onnxruntime: true
[coreml-spike] target_vendor=apple, supported_by_platform: true
```

(`ort::ep::CoreML::default().is_available()`, which queries
`GetAvailableProviders`.) So NO custom onnxruntime build and NO `load-dynamic`
dylib swap is needed — the gotcha the brief flagged does not bite here. The EP
is present and registers; it's the *model* (external data + int8) that blocks.

## Measured numbers

| EP | CLIP visual (image) embedding | Notes |
|----|-------------------------------|-------|
| **CPU (shipped default)** | **0.30 img/s ≈ 18.2 img/min** | 60 timed images, 378x378, GraphOptimizationLevel::Level3, 4 intra-op threads. Matches the SPIKE-P7-EMBED figure (2.96 s/image). |
| **CoreML (CPUAndNeuralEngine, MLProgram)** | **N/A — visual tower fails to load** | External-data path mis-resolution (blocker #1). No img/s obtainable on the bottleneck tower without an inlined re-export. |

ANE-vs-CPU reality: not reachable for the visual tower (it never loads). For
the single-file textual tower, CoreML's int8 compile did not complete in 6.5
min, consistent with the EP partitioning int8 to CPU and paying a heavy
MLProgram specialization tax — i.e. CoreML would run int8 ON CPU, not the ANE.

Embedding accuracy delta (cosine CPU-vs-CoreML): not measurable — there are no
CoreML embeddings to compare against, because the tower does not load. The
harness asserts a soft `mean cosine > 0.9` sanity gate for the day this is
re-run on a loadable FP16 export.

## int8 finding + the FP16 recommendation (next step, NOT done here)

The brief's research prediction held: **int8 wins nothing on CoreML** (CPU
fallback for the quantized ops, plus a slow compile). To get an actual CoreML
(ANE/GPU) win you need an **FP16** export of the DFN5B visual tower, single-file
(weights inlined, no external-data fan-out). Conversion path, for the follow-up
packet (needs the original fp32/fp16 source, which is NOT in this spike's
int8-only snapshot, so it was not performed here):

1. Get the fp32 DFN5B visual ONNX (the immich-app / OpenCLIP source export, not
   the int8 quantized variant we ship).
2. Cast to fp16 and INLINE external data into one file, e.g.:
   ```python
   import onnx
   from onnxconverter_common import float16
   m = onnx.load("visual_fp32.onnx")               # loads its external data
   m16 = float16.convert_float_to_float16(m, keep_io_types=True)
   onnx.save_model(
       m16, "visual_fp16.onnx",
       save_as_external_data=False,                 # inline -> fixes blocker #1
   )
   ```
   (`keep_io_types=True` keeps the f32 image input/output the connector feeds.)
3. Re-run `coreml_spike_clip_image_cpu_vs_coreml` against the fp16 single-file
   export. Expect both blockers cleared: it loads (inlined), and fp16 conv/
   matmul are ANE/GPU-eligible, so CoreML can actually accelerate.
4. Only if that shows a real speedup AND the cosine-vs-CPU delta is tiny
   (retrieval nDCG on the COCO golden set holds) does CoreML graduate from this
   env knob to a real config field + a re-embed of the PPVEC space under the
   new (fp16) vectors.

Also worth setting on the retry: `.with_model_cache_dir(...)` so the multi-
minute CoreML compile is paid once and cached across launches (it recompiles
every session otherwise).

## How to reproduce (this machine)

```bash
# 1. Confirm the linked onnxruntime carries the CoreML EP:
cargo test -p photoproof-connectors --test coreml_spike \
    coreml_spike_provider_available -- --ignored --nocapture

# 2. CPU baseline + the CoreML visual-tower load failure:
cargo test --release -p photoproof-connectors --test coreml_spike \
    coreml_spike_clip_image_cpu_vs_coreml -- --ignored --nocapture

# Probes that isolate the external-data blocker / the int8 compile cost:
cargo test --release -p photoproof-connectors --test coreml_spike \
    coreml_spike_visual_from_cwd          -- --ignored --nocapture
cargo test --release -p photoproof-connectors --test coreml_spike \
    coreml_spike_singlefile_session_builds -- --ignored --nocapture
```

The harness lives at `crates/photoproof-connectors/tests/coreml_spike.rs`
(all `#[ignore]`, measurements not gates; they skip cleanly without the local
DFN5B snapshot + COCO images). It drives CPU-vs-CoreML by toggling the same
`PHOTOPROOF_ORT_COREML` knob the shipped code reads.

## Constraint check

- **CPU default byte-identical:** yes — `build_session` is unchanged when
  `PHOTOPROOF_ORT_COREML` is unset; only an `if coreml_requested()` branch was
  added around the existing builder.
- **Ships without CoreML being functional:** yes — the default path never
  registers the EP. (The `coreml` Cargo feature links a CoreML-capable
  onnxruntime, but nothing activates it at runtime by default.)
- **Gate:** `cargo fmt --check`, `cargo clippy --all-targets` (with the coreml
  feature) clean; `cargo test` CPU path unchanged (only the known-ignored
  `s02_2_case_only_rename_relinks_sidecar` fails, pre-existing).
