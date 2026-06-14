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

## VERDICT (int8): DON'T-SHIP (as-is) -- NEEDS an inlined-weights re-export first

> UPDATE (June 14, 2026): the inlined-weights FP16 re-export was done and
> re-tested. It WORKS: CoreML loads the FP16 visual tower and runs it **8.77x**
> faster than CPU (0.31 -> 2.70 img/s) with near-lossless embeddings (mean cosine
> 0.9987 vs CPU). New verdict for fp16: **SHIP-WITH-FP16** with a model-cache
> caveat. See the "FP16 follow-up" section below for the recipe, validation
> cosines, measurement, and production-wiring notes.

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

# FP16 follow-up: CPU vs CoreML on the inlined single-file FP16 visual tower
# (needs the staged models/...-fp16/ dir; ~16.5 min first-load CoreML compile):
cargo test --release -p photoproof-connectors --test coreml_spike \
    coreml_spike_fp16_clip_image_cpu_vs_coreml -- --ignored --nocapture
```

The harness lives at `crates/photoproof-connectors/tests/coreml_spike.rs`
(all `#[ignore]`, measurements not gates; they skip cleanly without the local
DFN5B snapshot + COCO images). It drives CPU-vs-CoreML by toggling the same
`PHOTOPROOF_ORT_COREML` knob the shipped code reads.

## FP16 follow-up (June 14, 2026) -- VERDICT: SHIP-WITH-FP16 (one caveat)

The int8 spike above predicted an FP16 single-file re-export would clear BOTH
blockers. It does. CoreML now loads the visual tower and runs it ~8.8x faster
than CPU with near-lossless embeddings. The one caveat is a long first-launch
CoreML compile that production must cache.

### The two int8 blockers, re-tested on FP16

1. **External-data load failure -> CLEARED.** The FP16 re-export inlines all
   weights into ONE self-contained `visual/model.onnx` (no ~397 sibling files),
   so the path-misresolution that killed the int8 visual load cannot occur.
   CoreML accepted the single file and proceeded to compile it.
2. **int8 CPU-fallback + pathological compile -> CLEARED (was the int8 issue).**
   FP16 conv/matmul are ANE/GPU-eligible, so CoreML actually accelerates the
   graph instead of partitioning it to CPU. The visual tower went from 0.31 to
   2.70 img/s.

### The FP16 conversion recipe (this machine)

Source: `immich-app/ViT-H-14-378-quickgelu__dfn5b` FP32 (the SAME export lineage
our int8 ships -- so the graph I/O the connector is built against is preserved).
NOT RuteNL's repo (different lineage, different I/O). Env: a Python 3.12 venv
(system 3.14 had wheels, but 3.12 was used for stability) with `onnx==1.21`,
`onnxconverter_common`, `onnxruntime==1.26`, `huggingface_hub`, `tokenizers`,
`numpy`, `pillow`.

The naive `convert_float_to_float16(m, keep_io_types=True)` FAILED to load in ORT
with `Type Error: Concat bound to different types (float / float16) at
/visual/Concat`. Root cause: the graph carries 131 explicit `Cast(to=FLOAT)`
nodes (visual) / 98 (textual) that the converter leaves untouched; once their
neighbours go fp16 the Cast outputs a stray float32 into a Concat. `keep_io_types`
also could not run in-converter shape inference (the >2GB protobuf limit). The
recipe that worked:

```python
import onnx
from onnxconverter_common import float16
from onnx import helper, TensorProto

m = onnx.load(src)                       # resolves external data, any file count
g_in_name  = m.graph.input[0].name       # visual: 'image' (f32) / textual: 'text' (int32)
g_out_name = m.graph.output[0].name      # 'embedding' (f32)
in_is_float = m.graph.input[0].type.tensor_type.elem_type == TensorProto.FLOAT

# Convert EVERYTHING to fp16 (keep_io_types=False) to avoid the buggy boundary casts.
m16 = float16.convert_float_to_float16(m, keep_io_types=False, disable_shape_infer=True)
g = m16.graph

# Retarget the surviving explicit Cast(to=FLOAT) -> FLOAT16 (the Concat fix).
for n in g.node:
    if n.op_type == "Cast":
        for a in n.attribute:
            if a.name == "to" and a.i == TensorProto.FLOAT:
                a.i = TensorProto.FLOAT16

# Re-wrap f32 I/O BY HAND so the connector's f32 'image' feed + f32 'embedding'
# read (try_extract_tensor::<f32>) still match: Cast(f32->f16) after input,
# Cast(f16->f32) before output. (textual 'text' is int32 and stays int32.)
fp16_out = g_out_name + "_fp16"
for n in g.node:
    for i, o in enumerate(n.output):
        if o == g_out_name: n.output[i] = fp16_out
g.node.append(helper.make_node("Cast", [fp16_out], [g_out_name], to=TensorProto.FLOAT))
g.output[0].type.tensor_type.elem_type = TensorProto.FLOAT
if in_is_float:
    fp16_in = g_in_name + "_fp16"
    for n in g.node:
        for i, x in enumerate(n.input):
            if x == g_in_name: n.input[i] = fp16_in
    g.node.insert(0, helper.make_node("Cast", [g_in_name], [fp16_in], to=TensorProto.FLOAT16))
    g.input[0].type.tensor_type.elem_type = TensorProto.FLOAT

onnx.save_model(m16, dst, save_as_external_data=False)   # INLINE -> single file
```

Outputs (single-file, weights inlined): `visual/model.onnx` ~1.27 GB,
`textual/model.onnx` ~0.71 GB. I/O preserved exactly: visual `image`
f32 `[1,3,378,378]` -> `embedding` f32 `[1,1024]`; textual `text` int32 `[1,77]`
-> `embedding` f32 `[1,1024]` (matches `ort_embedder::run_clip_image` /
`run_clip_text`). Staged at
`models/ViT-H-14-378-quickgelu__dfn5b-fp16/{visual,textual}/model.onnx` (+ the
textual tokenizer/configs); the int8 dir is untouched as the CPU fallback.

### Validation (lossless? YES)

FP16 vs FP32 reference, both ORT CPU EP, real inputs (COCO images preprocessed
exactly like the connector: resize-shortest-side + center-crop 378, /255,
mean/std, CHW; CLIP-tokenized captions):

| tower   | mean cosine fp16-vs-fp32 | min cosine | n  |
|---------|--------------------------|------------|----|
| visual  | **0.999994**             | 0.999976   | 10 |
| textual | **1.000000**             | 1.000000   | 6  |

Comfortably past the >= 0.9995 lossless bar. (Conversion warnings -- a handful of
sub-1e-7 constants truncated to fp16 min, and the attention `-inf` mask fill
clamped to -10000 -- are the converter's standard, safe behavior.)

### CoreML measurement on the FP16 visual tower (the payoff)

`coreml_spike_fp16_clip_image_cpu_vs_coreml` (in
`crates/photoproof-connectors/tests/coreml_spike.rs`, `#[ignore]`), 60 timed
COCO images, 3 warmup, edge 378, CoreML `ComputeUnits::CPUAndNeuralEngine` +
`ModelFormat::MLProgram`:

| EP | CLIP visual (image) embedding | accuracy vs CPU |
|----|-------------------------------|-----------------|
| **CPU (int8 today / fp16)** | 0.31 img/s ~= 18.4 img/min | reference |
| **CoreML (fp16, MLProgram)** | **2.70 img/s ~= 161.7 img/min** | mean cosine **0.998656**, min 0.995628 |

- **Does the FP16 visual tower LOAD on CoreML?** YES -- the inlined single-file
  form loads where the int8 external-data form could not.
- **img/sec CPU vs CoreML?** 0.31 -> 2.70 = **8.77x** on the bottleneck tower.
  This directly addresses the ~18 img/min CPU ceiling: ~18 img/min -> ~162
  img/min.
- **ANE/GPU or CPU fallback?** The 8.8x speedup is only explicable by ANE/GPU
  execution (a CPU-partitioned graph would match the CPU row); CoreML accelerated
  the fp16 conv/matmul as predicted. (A per-op `ProfileComputePlan` partition
  dump was not separately captured; the throughput is the signal.)
- **Accuracy CoreML vs CPU?** mean cosine 0.998656, min 0.995628 -- retrieval
  -safe (35/60 sat just below 0.999, none below 0.995; this is normal fp16
  ANE-vs-CPU rounding, not a different embedding space).

**The one caveat -- first-launch compile cost.** CoreML spent **992 s (~16.5
min) compiling the fp16 visual MLProgram on the first load**, recompiled every
session. Production MUST set `.with_model_cache_dir(...)` so that compile is paid
once and cached across launches; without caching the 16.5 min tax would dwarf
the inference win on short runs.

**Disk note (environment, not the model).** The CoreML MLProgram compiler writes
the full ~1.3 GB of weights plus intermediates to `$TMPDIR`; on a near-full disk
(this machine sat at 95-99% used) the compile aborts with `NSCocoaErrorDomain
640 ... out of space`. It only succeeded after freeing headroom to ~17 GB. So
production wiring should also ensure adequate scratch/cache disk.

### Production wiring - status (June 14, 2026)

The measurement justified graduating CoreML+fp16 on macOS. The CODE-side wiring
that does not need infra or a vector-space decision is now LANDED; the remaining
steps are founder/infra-gated (hosting + the re-embed eval). Status per step:

1. **[DONE] `model_specs` fp16 CLIP entry.** `clip_spec("ViT-H-14-378-quickgelu__dfn5b-fp16")`
   resolves to the single-file `visual/textual/model.onnx` layout (1024-dim) -
   `crates/photoproof-connectors/src/model_specs.rs`. Own model_id = own PPVEC
   space, so selecting it triggers a clean re-embed (the int8 entry stays the
   CPU fallback). Buildable now by the offline eval rig and a config that names it.
2. **[DONE] CoreML compiled-model cache.** `build_session` now derives a
   `.coreml-cache` dir beside each tower and passes it to
   `with_model_cache_dir(...)`, so the ~16.5 min first compile is paid ONCE, not
   per launch - `crates/photoproof-connectors/src/ort_embedder.rs`
   (`coreml_cache_dir` + `build_session_with_coreml`). Active whenever CoreML is
   on (today the `PHOTOPROOF_ORT_COREML` env knob). Unit-tested; the CPU default
   is byte-identical (the cache is only touched on the CoreML branch).
3. **[FOUNDER - infra] Host the fp16 model + add a manifest entry.** The fp16
   model was converted LOCALLY; it is not hosted, so the downloader (SHA-pinned,
   consent-gated) cannot fetch it yet. To distribute: host the three files and add
   a `ModelEntry { id: "...__dfn5b-fp16", ... }` to `runtime/manifest.rs`. The
   local SHA-256 + sizes (ready to pin once hosted):

   | file | bytes | sha256 |
   |---|---|---|
   | `visual/model.onnx`     | 1265962399 | `e30e7613f2cdf1eda55fa685b467e1e04e261f20c5a15d22238682189e45ef99` |
   | `textual/model.onnx`    | 708726647  | `f2cc1e79707f394373083d26abd6a51a039e319cb1bd47c65a47f3786ba368d2` |
   | `textual/tokenizer.json`| 3642073    | `6d9109cc838977f3ca94a379eec36aecc7c807e1785cd729660ca2fc0171fb35` |
4. **[FOUNDER - flip] Prefer fp16+CoreML on macOS + graduate the env knob to a
   config field**, with int8+CPU as the fallback (the `RuntimePlan` selection in
   `runtime/plan.rs`). This is the default-flip; gated on step 3 + step 5.
5. **[FOUNDER - eval] Re-embed nDCG check.** A re-embed under the fp16 vectors is
   NOT required for correctness (cosine vs the int8/CPU space ~0.999), but run the
   COCO golden nDCG on the fp16 space before flipping the default, since the stored
   vectors were embedded under int8/CPU. The eval rig can do this NOW against the
   local fp16 model (step 1 makes the id buildable).

So: the speedup is wired and usable on this machine (env knob -> CoreML -> cached
compile); shipping it to all users needs the founder to host the model (3) and run
the eval + flip (4, 5).

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
