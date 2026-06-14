# PLAN-TENSORRT — `ort` embedders on NVIDIA via the ONNX Runtime TensorRT EP (Ryzen 9900X + RTX 5080)

Status: READY-TO-EXECUTE PLAN (no production code; cannot compile on the M1 Mac
without CUDA + TensorRT libraries). This is the NVIDIA analog of the landed CoreML
work (`docs/SPIKE-COREML.md`, `docs/RUNTIME-MATRIX.md`) and UPGRADES the prior
"CUDA EP" plan to the **TensorRT execution provider**. Execute on the 5080 desktop.

Target machine: Ryzen 9900X (Zen5, 12c/24t) + RTX 5080 (Blackwell, 16 GB GDDR7).
The same single-file FP16 DFN5B export that CoreML validated at 8.77x ALSO serves
TensorRT (one FP16 ONNX, two accelerators) - `docs/RUNTIME-MATRIX.md` lines 127-131.

The pattern to mirror is the per-model CoreML gating already shipped:
`OrtEmbedder::clip` selects CoreML iff `cfg!(target_os = "macos") && model_id.ends_with("-fp16")`
(`crates/photoproof-connectors/src/ort_embedder.rs:195`), and `build_session`
registers the EP + a compiled-model cache + an env-knob override
(`ort_embedder.rs:357-404`, `build_session_with_coreml` `:432-450`).

---

## 1. The `ort` 2.0.0-rc.12 TensorRT EP API (verified in source)

Source read at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ort-2.0.0-rc.12/src/ep/tensorrt.rs`.
The TensorRT EP is `ort::ep::TensorRT` (re-exported at `ort::ep::mod.rs:40-41`),
built with the same `.build()` -> `ExecutionProviderDispatch` -> `.error_on_failure()`
chain the CoreML path uses (`ort/src/ep/mod.rs:319-334` `impl_ep!`,
`:174-177` `error_on_failure`).

### Cargo feature

`tensorrt = ["ort-sys/tensorrt"]` (`ort/Cargo.toml:149`). It is a sibling of the
already-used `coreml`/`half`. Like `cuda`, registration is `#[cfg(any(feature =
"load-dynamic", feature = "tensorrt"))]` (`tensorrt.rs:269`); without the feature
`register` returns `RegisterError::MissingFeature` (`tensorrt.rs:293`).

### The exact builder methods (quoted from `tensorrt.rs`)

| call | line | sets the ORT option |
|---|---|---|
| `TensorRT::default()` | 6-11 | empty options |
| `.with_device_id(i32)` | 15-18 | `device_id` |
| `.with_fp16(bool)` | 48-51 | `trt_fp16_enable` — **the FP16 path** |
| `.with_engine_cache(bool)` | 84-87 | `trt_engine_cache_enable` — **turn caching ON** |
| `.with_engine_cache_path(impl ToString)` | 90-93 | `trt_engine_cache_path` — **the engine-cache DIR (the CoreML-cache analog)** |
| `.with_engine_cache_prefix(impl ToString)` | 102-105 | `trt_engine_cache_prefix` — per-tower prefix |
| `.with_timing_cache(bool)` | 150-153 | `trt_timing_cache_enable` — kernel-timing reuse (faster rebuilds) |
| `.with_timing_cache_path(impl ToString)` | 156-159 | `trt_timing_cache_path` |
| `.with_max_workspace_size(usize)` | 30-33 | `trt_max_workspace_size` — VRAM the builder may use |
| `.with_builder_optimization_level(u8)` | 186-189 | `trt_builder_optimization_level` |
| `.with_int8(bool)` | 54-57 | `trt_int8_enable` (NOT used: our int8 export is the CPU fallback) |
| `.with_detailed_build_log(bool)` | 168-171 | `trt_detailed_build_log` — surface the first-build cost |
| `.with_engine_hw_compatible(bool)` | 252-255 | `trt_engine_hw_compatible` (cross-arch engine; leave OFF, we build on-target) |
| `.build()` | via `impl_ep!` mod.rs:319-333 | -> `ExecutionProviderDispatch` |
| `.error_on_failure()` | mod.rs:174-177 | loud register failure (mirror CoreML) |

The EP also exposes `.with_arbitrary_config(key, value)` (`impl_ep!(arbitrary;
TensorRT)` at `tensorrt.rs:11`, trait at `mod.rs:131-133`) for any TensorRT option
`ort` has not surfaced - an escape hatch if a Blackwell-specific knob is needed.

`supported_by_platform()` is `cfg!(linux x86_64/aarch64 || windows x86_64)`
(`tensorrt.rs:263-265`) - it is **false on macOS**, so the M1 cannot even pretend
to register it. `is_available()` (`mod.rs:118-120` -> `GetAvailableProviders`)
returns whether the linked onnxruntime was compiled WITH TensorRT - the runtime
detection knob for the fallback ladder (section 4).

### The engine cache (the slow-first-build, like the CoreML compile)

TensorRT builds a per-GPU-architecture engine on first load by exhaustively
profiling kernels - this is SLOW (the official ORT docs cite 384 s -> 9 s once
cached) and DIRECTLY analogous to the ~16.5 min CoreML MLProgram compile the
CoreML path caches with `.with_model_cache_dir(...)`. The engine MUST be cached or
the build tax is paid every launch. The ort calls:

```rust
// the FP16 + engine-cache chain (the production NVIDIA path)
ort::ep::TensorRT::default()
    .with_device_id(0)
    .with_fp16(true)                                   // trt_fp16_enable
    .with_engine_cache(true)                           // trt_engine_cache_enable
    .with_engine_cache_path(cache_dir_string)          // trt_engine_cache_path
    .with_timing_cache(true)                            // trt_timing_cache_enable
    .with_timing_cache_path(cache_dir_string)          // trt_timing_cache_path
    .build()
    .error_on_failure()
```

CACHE INVALIDATION (official ORT TensorRT docs): the `.engine`/`.profile`/`.timing`
files are NOT portable and must be deleted when the model topology/opset, the ORT
version, the TensorRT version, OR the GPU hardware changes. Practical consequence
for PhotoProof: key the cache dir on the model id (own PPVEC space already, like the
`-fp16` suffix) and treat a stale cache as "recompile" - same posture as the CoreML
cache note (`ort_embedder.rs:407-415`: "CoreML re-keys on graph structure, so a
stale cache from a model swap simply recompiles"). Recommend the cache dir be a
`.trt-cache` subdir beside each tower (mirror `coreml_cache_dir`), which the
downloader ignores (no manifest match) and which follows a `models_dir` override.

NOTE the convenient detail: a TensorRT engine cache is keyed per-GPU-arch. Because
PhotoProof builds and runs on the SAME 5080, `trt_engine_hw_compatible` stays OFF
(building a portable cross-arch engine would be slower and is not needed).

### Blackwell (RTX 5080) note: TensorRT vs TensorRT-RTX

There are now TWO NVIDIA TensorRT EPs upstream: the classic `TensorrtExecutionProvider`
(what `ort` rc.12 exposes as `ep::TensorRT`) and the newer **TensorRT-RTX EP**
(`NvTensorRtRtxExecutionProvider`, AOT compile + a JIT runtime cache, RTX 30xx and
later, i.e. Blackwell-eligible). `ort` rc.12 does NOT carry a `TensorRT-RTX` struct
(only `nvrtx.rs` exists, a different thing); so the EXECUTE plan is the classic
TensorRT EP, which fully supports Blackwell via a recent TensorRT 10.x + CUDA 12.x
build. TensorRT-RTX is a future upgrade once `ort` exposes it (track in
`docs/BACKLOG.md`). The classic EP on the 5080 is the right, available choice now.

---

## 2. Expected performance

- **TensorRT EP vs CUDA EP:** TensorRT compiles the WHOLE graph to an optimized
  per-GPU engine rather than dispatching op-by-op like the CUDA EP, and typically
  delivers ~1.5x over the CUDA EP for transformer/conv inference (and the CUDA EP is
  itself far above CPU). This is the documented tradeoff: more throughput, at the
  cost of a slow first-build that MUST be cached (NVIDIA's ORT EP blog; the ORT
  TensorRT EP docs). DFN5B ViT-H-14-378 is exactly the conv+matmul+attention stack
  TensorRT optimizes best.
- **The FP16 path:** `with_fp16(true)` uses the 5080's tensor cores. FP16 was
  validated near-lossless on this same export under CoreML (visual mean cosine
  0.999994 vs fp32, `docs/SPIKE-COREML.md`); TensorRT FP16 is the same precision
  story and must be re-confirmed by the COCO nDCG eval (section 5).
- **Rough envelope (to be MEASURED, not promised):** CPU baseline is 0.31 img/s
  (~18 img/min, `docs/SPIKE-COREML.md`). CoreML/ANE on the M1 hit 2.70 img/s
  (8.77x). A desktop RTX 5080 with TensorRT-FP16 should comfortably exceed the ANE
  (more FLOPS, dedicated tensor cores, 16 GB) - expect well into the tens of img/s.
  The number is a 5080 measurement, not a prediction; the plan's job is to make it
  measurable, not to commit a figure.
- **Higher tier:** the 5080's 16 GB GDDR7 lets CLIP image-embed co-run with a
  larger LLM (RUNTIME-MATRIX "tier headroom", lines 77-83) and enables the
  per-model capture-pause relaxation already noted as gated on a GPU EP landing
  (`docs/RUNTIME-MATRIX.md` lines 196-200).

---

## 3. Build requirements on the 5080 (build-ON-the-target task)

This is a build-on-the-target-machine task (like llama.cpp's per-platform CUDA
vendoring, `docs/RUNTIME-MATRIX.md` lines 97-108). The M1 cannot produce or test
this binary. On the Ryzen + 5080 (Linux x86_64 or Windows x86_64 -
`supported_by_platform`, `tensorrt.rs:264`):

1. **NVIDIA driver** new enough for Blackwell + **CUDA Toolkit 12.x** (rc.12's
   CUDA dylibs are the `*_12` set: `libcudart.so.12`, `libcublas*.so.12`,
   `libnvrtc.so.12`, `libcurand.so.10`, `libcufft.so.11` - `ort/src/ep/cuda.rs:400-402`;
   on Windows the `*64_12.dll` equivalents `:399-400`).
2. **cuDNN 9.x** (the CUDA EP fallback's dependency set: `libcudnn*.so.9`,
   `cuda.rs:415-425`).
3. **TensorRT 10.x** libraries (matching the linked onnxruntime's TensorRT build;
   official docs minimum TensorRT 6.0 / CUDA 10.0, but Blackwell needs a recent
   TensorRT 10.x + CUDA 12.x). The TensorRT EP loads these at session-build time.
4. **The `ort` cargo features:** add `tensorrt` and `cuda` (CUDA is the EP one rung
   below TensorRT on the ladder, section 4) to the desktop build. ALSO add the
   `preload-dylibs` feature if the CUDA/TensorRT libs are vendored to a non-default
   path - `ort::ep::cuda::preload_dylibs(cuda_root, cudnn_root)` (`cuda.rs:452-467`,
   gated on `feature = "preload-dylibs"`) preloads them without touching `PATH`,
   the analog of how llama.cpp ships its CUDA dylibs beside the binary.

   onnxruntime linking: `ort-sys/tensorrt` expects a TensorRT-enabled onnxruntime.
   The prebuilt onnxruntime `ort` downloads may NOT carry the TensorRT EP (it is a
   build-time onnxruntime option, exactly the CoreML gotcha that turned out fine -
   `docs/SPIKE-COREML.md` "Does the linked onnxruntime even HAVE the CoreML EP?").
   FIRST STEP on the 5080: probe `ep::TensorRT::default().is_available()` and
   `ep::CUDA::default().is_available()` (`mod.rs:118-120`). If false, switch to
   `ort`'s `load-dynamic` feature and point it at a TensorRT-enabled
   `libonnxruntime` (the GPU package from onnxruntime.ai or an NVIDIA build). This
   is the single biggest unknown and must be settled before any wiring.

5. **Disk/scratch:** TensorRT writes engine + timing artifacts during the build;
   ensure the cache dir + `$TMPDIR` have headroom (the CoreML spike hit an
   out-of-space abort, `docs/SPIKE-COREML.md` "Disk note").

Vendoring: ship the CUDA/cuDNN/TensorRT runtime libs beside the desktop binary in
the NVIDIA distribution (analogous to the llama.cpp CUDA vendoring), preloaded via
`preload_dylibs` so users need no system CUDA install. This is the
per-platform packaging story to design alongside the wiring.

---

## 4. The wiring design (mirror the per-model CoreML gating)

The whole point is to land this as the EXACT analog of the CoreML gating so the two
accelerators share one shape. One binary serves all NVIDIA users, so EP choice is a
RUNTIME availability check, not a `cfg!` (CoreML could be `cfg!(macos)` because that
is compile-time; "is there a working CUDA/TensorRT" is not). Behind a `cuda` +
`tensorrt` cargo feature gate for the desktop NVIDIA build.

### 4a. The EP selector — `build_session` (`ort_embedder.rs:357-404`)

Today `build_session(model_path, coreml)` takes a bool. Generalize the second
argument to an accelerator choice (keep CPU byte-identical when none):

```rust
enum Accel { Cpu, CoreML, Nvidia }   // Cpu = today's exact path
fn build_session(model_path: &Path, accel: Accel) -> ConnectorResult<Session> { ... }
```

- `Accel::CoreML` -> existing `build_session_with_coreml` (`:432-450`), unchanged.
- `Accel::Nvidia` -> new `build_session_with_tensorrt(builder, cache_dir)` that
  registers a FALLBACK LADDER as ONE `with_execution_providers([...])` list. ORT
  tries them in order and the first that registers wins, CPU underneath
  (`ort/src/ep/mod.rs:336-369` `apply_execution_providers`):

```rust
fn build_session_with_tensorrt(
    builder: SessionBuilder,
    cache_dir: Option<&Path>,
) -> Result<SessionBuilder, String> {
    use ort::ep::{TensorRT, CUDA};
    let mut trt = TensorRT::default().with_device_id(0).with_fp16(true);
    if let Some(dir) = cache_dir {
        let p = dir.to_string_lossy().to_string();
        trt = trt
            .with_engine_cache(true)
            .with_engine_cache_path(p.clone())
            .with_timing_cache(true)
            .with_timing_cache_path(p);
    }
    builder
        .with_execution_providers([
            // 1. TensorRT FP16 (best). fail_silently -> fall through to CUDA.
            trt.build().fail_silently(),
            // 2. CUDA FP16 (next). fail_silently -> fall through to CPU.
            CUDA::default().with_device_id(0).build().fail_silently(),
            // 3. CPU is ORT's implicit floor when no EP registers.
        ])
        .map_err(|e| e.to_string())
}
```

KEY DIVERGENCE from the CoreML path: CoreML uses `.error_on_failure()` because the
spike WANTED a loud failure to avoid a false-positive "succeeded on CPU". Production
NVIDIA wants the OPPOSITE - a graceful TensorRT -> CUDA -> CPU descent
(`docs/RUNTIME-MATRIX.md` lines 63-74, the per-model fallback ladder). So use
`.fail_silently()` (the default, `mod.rs:166-169`) for the ladder, and `eprintln!`
which rung registered (mirroring the CoreML `eprintln!` at `ort_embedder.rs:392-399`).
For an on-target SPIKE harness, a `PHOTOPROOF_ORT_TRT_STRICT` knob can flip the
TensorRT rung to `.error_on_failure()` so a measurement run cannot silently fall to
CPU and look like a win (the false-positive guard the CoreML spike valued).

### 4b. Per-model gating — `OrtEmbedder::clip` (`ort_embedder.rs:180-208`)

Today (`:195`):
```rust
let coreml = cfg!(target_os = "macos") && model_id.ends_with("-fp16");
```
Replace the bool with the accelerator pick, runtime-detected on non-mac:

```rust
let accel = if cfg!(target_os = "macos") && model_id.ends_with("-fp16") {
    Accel::CoreML
} else if model_id.ends_with("-fp16") && nvidia_available() {
    Accel::Nvidia            // 5080: prefer TensorRT-FP16, ladder to CUDA, CPU
} else {
    Accel::Cpu               // int8 export, or no GPU -> today's path
};
let image_session = build_session(image_model_path, accel)?;
let text_session  = build_session(text_model_path, accel)?;
```

`nvidia_available()` = `cfg!(feature = "tensorrt") && (ep::TensorRT::default().is_available()? || ep::CUDA::default().is_available()?)`
(EP `is_available`, `mod.rs:118-120`). This is the runtime, single-binary detection.
The `-fp16` suffix already marks the single-file FP16 export and gives it its own
PPVEC space (`model_specs.rs:78-91`, `ort_embedder.rs:188-195`) - the SAME marker
serves CoreML and TensorRT, so the FP16 model is the one drop-in for both.

### 4c. `OrtEmbedder::text` — RE-MEASURE on TensorRT (the important one)

Today `text` hard-passes CPU (`ort_embedder.rs:157-160`): "int8 under CoreML hits a
pathological compile". That reasoning is CoreML-specific. On NVIDIA the calculus
INVERTS: CoreML lost text-embed because only ~3% of the EmbeddingGemma transformer
graph partitioned to the ANE (`docs/SPIKE-COREML-TEXT.md`,
`docs/RUNTIME-MATRIX.md:54`), but TensorRT takes the WHOLE graph as one engine. So
**text-embed may WIN on NVIDIA where it lost on CoreML** - it is worth re-measuring.

To measure it needs an FP16 single-file EmbeddingGemma export (the int8 export is
the CPU fallback; int8 is not the TensorRT-FP16 path), built with the SAME recipe as
the CLIP FP16 conversion (`docs/SPIKE-COREML.md` "FP16 conversion recipe"), under a
new `embeddinggemma-300m-fp16` id (own space, like the CLIP `-fp16`). Add it to
`text_spec` (`model_specs.rs:39-53`) and extend the `text` gate to choose
`Accel::Nvidia` for `-fp16` text ids when `nvidia_available()`. Gate this re-measure
behind the SAME spike harness as CLIP; only graduate if it beats int8/CPU on the
text-embed bench AND holds retrieval. RECOMMENDATION: do this as a measurement
arm, not a default flip, until the 5080 numbers exist.

### 4d. The engine-cache dir (the CoreML-cache analog)

Add `trt_cache_dir(model_path)` mirroring `coreml_cache_dir` (`:407-415`): a
`.trt-cache` subdir beside each tower, created best-effort, passed to
`with_engine_cache_path` + `with_timing_cache_path`. Per-tower, model-co-located,
dotdir (downloader ignores it), follows `models_dir`. Same null-safety: if it cannot
be created, proceed without (correct, just rebuilds the engine each launch).

### 4e. The fallback ladder (per model, `docs/RUNTIME-MATRIX.md:63-74`)

- CLIP on 5080: **TensorRT-FP16 -> CUDA-FP16 -> CPU-int8** -> embed pass
  deferred / search degrades to keyword.
- Text-embed on 5080: **CPU-int8 today; TensorRT-FP16 TBD** (4c) -> CPU-int8 -> keyword.
- VAD: CPU always (tiny). ASR: CPU by design. (`docs/RUNTIME-MATRIX.md:55-61`)

### 4f. Manifest + planner (founder/infra, like the CoreML `[NEXT]`)

- Host an `embeddinggemma-300m-fp16` entry IF 4c graduates, and re-pin the CLIP
  `-fp16` files (manifest entry already exists, `manifest.rs:302-328`, hosting
  still pending per `docs/SPIKE-COREML.md` step 3).
- `runtime/plan.rs` / the desktop config select the `-fp16` CLIP id on a detected
  NVIDIA machine - the same graduation the CoreML path needs (env knob ->
  detected-hardware select). `plan.rs` is pure (tier + installed -> plan); the EP
  choice stays INSIDE `OrtEmbedder` (4b), so `plan.rs` only needs to pick the
  `-fp16` model id when the machine is NVIDIA-capable, exactly as macOS picks it.

---

## 5. Validation steps (run ON the 5080)

Reuse the CLIP CoreML eval methodology verbatim (`docs/SPIKE-COREML.md`,
`docs/RUNTIME-MATRIX.md:185`):

1. **EP present?** Probe `is_available()` for TensorRT and CUDA (4a). Confirm the
   linked onnxruntime carries the EP (or switch to `load-dynamic`, section 3).
2. **Throughput.** A `tensorrt_spike.rs` `#[ignore]` harness modeled on
   `crates/photoproof-connectors/tests/coreml_spike.rs` (which toggles the EP and
   times 60 COCO images, 3 warmup, edge 378): measure CPU vs TensorRT-FP16 img/s on
   the CLIP visual tower; report the FIRST-build (engine compile) time separately
   from steady-state, and prove the cache makes the second launch fast.
3. **CLIP accuracy.** Mean/min cosine TensorRT-FP16 vs CPU-fp32 per the CoreML table
   (CoreML bar was mean >= 0.9987, retrieval-safe). Expect the same near-lossless
   FP16 story.
4. **Retrieval parity (the gate).** Re-run the COCO-1k golden nDCG that flipped
   CoreML: `pp-eval-ingest` with `PP_EVAL_CLIP_MODEL=ViT-H-14-378-quickgelu__dfn5b-fp16`
   (the override at `crates/photoproof-core/src/bin/pp_eval_ingest.rs:57-64`,
   default `:61`) re-embeds the corpus under the TensorRT-FP16 space; then `pp-sweep`
   scores nDCG@10 / R@10 / MRR vs the int8/CPU baseline. The CoreML bar held at
   nDCG 0.8212 vs 0.8225 (deltas < 0.3%, `docs/SPIKE-COREML.md`); TensorRT-FP16 must
   clear the same parity bar before any default flip. The eval rig already builds
   the SAME embedder stack as the desktop via `build_clip_embedder`
   (`pp_eval_ingest.rs:263`, `retrieval_eval.rs:377`,
   `model_specs::build_clip_embedder` `:130`), so the TensorRT path is exercised by
   selecting the `-fp16` id - no eval-rig changes needed for CLIP.
5. **Text-embed re-measure (4c).** Build `embeddinggemma-300m-fp16`, run the
   text-embed paraphrase-margin bench (the `coreml_spike_text.rs` shape) CPU-int8 vs
   TensorRT-FP16, AND the golden nDCG with that text space. Decide WIN/keep-CPU from
   the 5080 numbers. This is the open question the matrix flags (`docs/RUNTIME-MATRIX.md:54`).
6. **img/s + embeds/s headline.** Report CLIP img/s and text embeds/s vs CPU, the
   first-build cost, and the cached-launch cost (the production-relevant trio).

---

## 6. Effort / risk / sequencing

### Sequencing (on the 5080)
1. Build env: CUDA 12.x + cuDNN 9.x + TensorRT 10.x; confirm `is_available()` true
   (or wire `load-dynamic` to a TensorRT-enabled onnxruntime). *Gate: EP present.*
2. Add `tensorrt` + `cuda` (+ maybe `preload-dylibs`) features to the desktop build
   (`crates/photoproof-connectors/Cargo.toml:30`, mirror the `coreml` feature).
3. Wire `Accel` enum + `build_session_with_tensorrt` + `trt_cache_dir` + the
   `nvidia_available()` gate in `clip` (4a, 4b, 4d). CPU default stays byte-identical.
4. `tensorrt_spike.rs` harness; measure CLIP throughput + cosine + first-build/cache.
5. COCO-1k nDCG parity on the FP16 CLIP space (`PP_EVAL_CLIP_MODEL`). *Gate: parity.*
6. Decide CLIP default flip on NVIDIA (manifest host + plan select).
7. Text-embed: FP16 export + `embeddinggemma-300m-fp16` id + re-measure (4c, 5.5).
   Separate, lower-priority arm - it may keep CPU.

### Effort
- Wiring (steps 2-3): SMALL, ~a day - it is a deliberate clone of the landed CoreML
  shape (the `Accel` generalization touches `build_session`'s one bool + `clip`'s
  one line + two new small fns).
- Build env (step 1): MEDIUM and the real time sink - CUDA/TensorRT toolchain + the
  onnxruntime-carries-TensorRT question.
- Harness + eval (4-7): MEDIUM, mostly machine time (engine build + re-embed).

### Risk
- **HIGH / blocking:** does the linked onnxruntime carry the TensorRT EP? If not,
  `load-dynamic` + a vendored TensorRT-enabled `libonnxruntime`. Settle FIRST (the
  CoreML analog turned out fine, but TensorRT is a heavier build-time option).
- **MEDIUM:** first-build (engine compile) cost; mitigated by the engine + timing
  cache (`with_engine_cache`/`with_timing_cache`), which MUST be wired from day one
  (the CoreML 16.5 min lesson). Cache invalidation on TensorRT/ORT/driver/GPU change
  - delete-and-rebuild, keyed per model id.
- **MEDIUM:** an `ort` native crash crashes the app (no process boundary,
  `docs/RUNTIME-MATRIX.md:133-136`); same mitigation as today (pinned `ort`,
  background-only embed, CPU default). TensorRT adds native surface; the
  `.fail_silently()` ladder means a register failure degrades, but a mid-run native
  fault still aborts - keep embedding off the capture path.
- **LOW:** FP16 accuracy - already validated near-lossless on this export under
  CoreML; the nDCG gate (step 5) re-confirms on TensorRT.
- **LOW:** Blackwell support - the classic TensorRT EP supports Blackwell on a
  recent TensorRT 10.x; TensorRT-RTX EP is a future `ort`-exposure upgrade.

### Verdict on text-embed re-measure
WORTH RE-MEASURING on TensorRT. The reason text-embed lost on CoreML (only ~3% of
the transformer graph reaches the ANE) does NOT apply to TensorRT, which compiles
the whole graph to one engine. It is a cheap measurement arm (an FP16 EmbeddingGemma
export + the existing text bench + nDCG), the upside is a second seam graduating to
the GPU on the 5080, and the downside is bounded (keep CPU if it does not win). Do it
as a measurement, AFTER the CLIP path lands and parity holds - do not flip it by
assumption.

---

## Sources

- `ort` 2.0.0-rc.12 source (verified): `src/ep/tensorrt.rs`, `src/ep/cuda.rs`,
  `src/ep/mod.rs`, `Cargo.toml` (paths under
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ort-2.0.0-rc.12/`).
- ONNX Runtime TensorRT EP docs (options, cache, invalidation, versions):
  https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html
- NVIDIA: CUDA & TensorRT EPs in ONNX Runtime (whole-graph compile, FP16, the
  cost/throughput tradeoff):
  https://developer.nvidia.com/blog/end-to-end-ai-for-nvidia-based-pcs-cuda-and-tensorrt-execution-providers-in-onnx-runtime/
- NVIDIA TensorRT-RTX EP (future upgrade, AOT+JIT, RTX 30xx+):
  https://onnxruntime.ai/docs/execution-providers/TensorRTRTX-ExecutionProvider.html
- In-repo anchors: `docs/RUNTIME-MATRIX.md`, `docs/SPIKE-COREML.md`,
  `docs/SPIKE-COREML-TEXT.md`; wiring `crates/photoproof-connectors/src/ort_embedder.rs`,
  `crates/photoproof-connectors/src/model_specs.rs`,
  `crates/photoproof-core/src/runtime/manifest.rs`,
  `crates/photoproof-core/src/bin/pp_eval_ingest.rs`.
</content>
</invoke>
