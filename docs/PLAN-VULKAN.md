# Plan: GPU acceleration for the `ort` embedders on non-Apple, non-NVIDIA hardware

The gap. PhotoProof's two `ort` embedders (CLIP DFN5B ViT-H-14 and EmbeddingGemma-300m
text) have a GPU story on exactly two platforms: CoreML on Apple (LANDED, 8.77x) and
CUDA on NVIDIA (planned, the 5080 desktop). Every OTHER GPU - AMD or Intel, on Windows
or Linux - currently falls to the CPU int8 path. This doc researches how to close that
gap and recommends a sequence.

The LLM does NOT have this gap: llama.cpp vendors a Vulkan backend, so the LLM already
runs on any GPU (`docs/RUNTIME-MATRIX.md`, llama.cpp table). The embedders are the gap
because `ort` / ONNX Runtime has no Vulkan execution provider - confirmed below.

Scope and honesty up front. This is RESEARCH + PLAN only. None of it can be built or
measured on the M1 Mac (CoreML is the Mac path). The named, funded targets are the M1
(done) and the Ryzen + RTX 5080 (TensorRT/CUDA next). This whole document is LOWER
PRIORITY than those two - it is the "other hardware" bucket from `docs/RUNTIME-MATRIX.md`
and the Vulkan backlog item. Treat it as a map for when a non-NVIDIA/non-Apple machine
becomes a real target, not work to schedule now.

## The headline answer

Raw Vulkan is NOT the right framing. ONNX Runtime has no Vulkan EP and never shipped
one. The two real answers are both EPs that keep our ONNX format and mirror the
existing CoreML/CUDA gating:

1. **DirectML EP** closes the WINDOWS non-NVIDIA case (any DX12 GPU: AMD, Intel,
   NVIDIA). It is an `ort` cargo feature + EP registration - the cheapest real win,
   directly analogous to how CoreML/CUDA are wired. It accepts our FP16 ONNX (the
   accelerator format we already build). This is the near-term recommendation.

2. **The ONNX Runtime WebGPU EP** (native, Dawn-backed: DX12 on Windows, Vulkan on
   Linux, Metal on Mac) is the STRATEGIC cross-platform bet - one EP, one ONNX format,
   all non-Apple/non-NVIDIA GPUs including Linux-AMD/Intel. It exists in `ort` 2.0 as
   `WebGPUExecutionProvider`, but as of mid-2026 it is younger and less proven than
   DirectML for our specific models. It is the right thing to WATCH and spike, not yet
   the thing to ship.

3. Non-`ort` runtimes (ncnn, MNN, TVM, Burn) are the only path to GPU on Linux-AMD
   TODAY, but every one of them costs a model conversion away from ONNX and a weaker
   Rust integration. Reserve them for if-and-only-if Linux-AMD becomes a hard
   requirement before the WebGPU EP matures.

So: DirectML for Windows now, WebGPU EP as the cross-platform strategic bet, and the
non-`ort` runtimes only as a last resort for Linux-AMD specifically. Raw Vulkan is not
on the table as a `ort`/ONNX path.

## Why "Vulkan" is the wrong word

The backlog phrased this as "Vulkan GPU path for the `ort` embedders." That framing is
slightly off, and the correction matters for sequencing:

- ONNX Runtime ships no Vulkan execution provider. The EP list in `ort` 2.0
  (`CUDAExecutionProvider`, `TensorRTExecutionProvider`, `CoreMLExecutionProvider`,
  `DirectMLExecutionProvider`, `ROCmExecutionProvider`, `MIGraphXExecutionProvider`,
  `OpenVINOExecutionProvider`, `WebGPUExecutionProvider`, ...) has no `Vulkan` member.
  [1][2]
- Vulkan reaches our hardware INDIRECTLY two ways: (a) through the WebGPU EP, whose
  Dawn backend selects Vulkan on Linux; or (b) through a non-ONNX runtime that targets
  Vulkan natively (ncnn, TVM, Burn/wgpu).
- llama.cpp is different: its Vulkan backend is hand-written GGML compute shaders, not
  an ONNX path. We cannot borrow it for the embedders (the models are ONNX, not GGUF).

The decision is therefore never "add the Vulkan EP" (it does not exist). It is "pick
the EP or runtime that gets us onto these GPUs," and the leverage is to keep ONNX and
reuse the existing per-model gating wherever possible.

## Option 1 - DirectML EP (Windows, DX12) - RECOMMENDED near-term

What it is. ONNX Runtime's DirectML EP runs any ONNX model on any DirectX 12 GPU on
Windows: AMD, Intel, NVIDIA, integrated or discrete. It is vendor-neutral on Windows by
construction (DX12 is the abstraction). [3]

Confirmed facts.

- It exists in `ort` 2.0 (`...rc.12`) as `DirectMLExecutionProvider`, behind the
  `directml` cargo feature. [1][2]
- pyke's prebuilt download binaries INCLUDE DirectML - "DirectML, xnnpack, and coreml
  are available in any build if the platform supports it." So enabling it is a feature
  flag, NOT a custom ONNX Runtime build. [4]
- DirectML is Windows-only by design (`build.bat --use_dml`; "DirectML is only
  supported on Windows"). [5] It does NOT help Linux - that is Option 2/3's job.
- Precision fit is EXACTLY right for us: DirectML runs FP16 well but does NOT support
  INT8 quantized models ("DirectML backend doesn't support 8-bit precision"). [6] Our
  accelerator format is already FP16 (the single-file inlined FP16 ONNX that serves
  CoreML and CUDA per `docs/RUNTIME-MATRIX.md`), and int8 is explicitly our CPU-only
  fallback. So DirectML consumes the SAME FP16 artifact we already build and host for
  the other accelerators - no new conversion. (Do NOT try to feed it the int8 tower;
  that is the CPU path.)

API shape (mirrors CoreML/CUDA). v2 uses a builder; registration is the analog of the
existing CoreML wiring:

```rust
// sketch only - not validated, Windows-only build
use ort::execution_providers::DirectMLExecutionProvider;

Session::builder()?
    .with_execution_providers([
        DirectMLExecutionProvider::default()
            .with_device_id(0)   // pick the DX12 adapter
            .build(),
    ])?
    .commit_from_file(fp16_clip_path)?;
```

This slots into `OrtEmbedder::clip` exactly where CoreML/CUDA gating already lives. The
gate today is `cfg!(macos) && id.ends_with("-fp16")` (per `docs/RUNTIME-MATRIX.md` and
the CUDA backlog item); DirectML adds a `cfg!(windows)` arm that picks DirectML when the
detected GPU is non-NVIDIA (or unconditionally on Windows if we prefer one Windows GPU
path - see "open question" below).

Op coverage. ViT and small-transformer ops (conv patch embed, MatMul/Gemm, LayerNorm,
attention, softmax) are well-trodden on DirectML; it is a mature, Microsoft-shipped EP
used broadly for vision and transformer models. RMSNorm/RoPE in EmbeddingGemma export to
ONNX primitives DirectML handles. The realistic risk is the usual one - an occasional op
falls back to CPU within the partitioned graph - which our per-model load/validate ->
CPU fallback ladder already absorbs.

Effort: LOW. A cargo feature + an EP-registration arm + extending the gate, plus a build
on a Windows machine to validate and measure. The conversion cost is ZERO (reuses the
FP16 artifact). The one true cost is that it MUST be built and measured on a Windows
non-NVIDIA box - it cannot be validated from the Mac.

Maturity: HIGH. DirectML is a first-class, long-lived ONNX Runtime EP with prebuilt
binaries in the very crate we already use.

Caveat. It does NOTHING for Linux. A Windows-AMD/Intel laptop is covered; a Linux-AMD
workstation is not. For Linux you need Option 2 or 3.

## Option 2 - ONNX Runtime WebGPU EP (native, Dawn) - STRATEGIC, watch + spike

This is the one to investigate seriously, because IF mature it is the single portable
GPU EP across every non-Apple/non-NVIDIA target while keeping ONNX.

What it is. ONNX Runtime now has a native (non-browser) WebGPU execution provider built
on Dawn (Chromium's WebGPU implementation). Dawn dispatches to the platform's native
GPU API: DX12 on Windows, Vulkan on Linux, Metal on Mac. So ONE EP, fed our ONNX,
reaches AMD and Intel GPUs on BOTH Windows and Linux - the exact gap this doc is about.
[7][8]

Maturity as of mid-2026 - the honest read. The evidence is mixed and worth stating
precisely, because it decides whether this is "ship now" or "watch":

- It IS real and present in our toolchain: `ort` 2.0 exposes `WebGPUExecutionProvider`
  behind a `webgpu` cargo feature, and pyke states "binaries with the WebGPU EP are
  available on Windows & Linux." [1][2][4] So from `ort`'s side it is a feature flag,
  like DirectML.
- BUT the native WebGPU EP is younger and less battle-tested than DirectML. The
  onnxruntime.ai EP summary table and the v1.25 release notes still present WebGPU
  primarily through the Web/JavaScript lens, and ORT users on desktop have hit "WebGPU
  not in get_available_providers()" because it is not enabled in the DEFAULT prebuilt
  packages - it needs a build/package that includes it (`--use_webgpu`, Dawn). [9][10]
  The native EP began as a Microsoft feature request (issue #22077) and has been
  landing incrementally since. [11]
- Op coverage is broad and explicitly transformer/vision oriented: the WebGPU EP ships
  native kernels for Conv variants, MatMul/Gemm, normalizations, Attention /
  MultiHeadAttention / GroupQueryAttention, rotary (RoPE) embeddings, and quantized
  matmul - i.e. the ops a ViT and a modern text transformer need. [9] This is the
  encouraging part: the op set is designed for exactly our two model shapes.

So WebGPU EP's PROMISE (one ONNX EP, all non-Apple/non-NVIDIA GPUs, the transformer ops
we need) is real, but its desktop MATURITY for production - especially Linux-Vulkan
throughput parity with DirectML/CUDA and freedom from per-op CPU fallback on ViT-H-14 -
is not yet proven for us and cannot be assumed. It needs a spike on real hardware.

API shape. Same builder pattern; the differentiator is it is the only `ort` EP that
would also work on Linux:

```rust
// sketch only - requires an ort/onnxruntime build with WebGPU enabled
use ort::execution_providers::WebGPUExecutionProvider;

Session::builder()?
    .with_execution_providers([ WebGPUExecutionProvider::default().build() ])?
    .commit_from_file(fp16_clip_path)?;
```

Effort: MEDIUM. The wiring is feature-flag-shaped like DirectML, BUT (a) we must confirm
the prebuilt binary actually carries WebGPU for our target triples or build ORT with
`--use_webgpu` + Dawn ourselves (heavier - Dawn is a large dependency), and (b) it must
be spiked for op-coverage and throughput on real AMD/Intel hardware, on BOTH Windows and
Linux, since the value proposition is the cross-platform reach. Conversion cost is ZERO
(keeps our FP16 ONNX).

Why it could be THE answer. If a spike shows the WebGPU EP runs our FP16 CLIP + text
towers fully on the GPU at a worthwhile speedup on AMD/Intel under Vulkan, it
SUBSUMES the Linux-AMD problem AND overlaps DirectML on Windows - one code path, one
artifact, every non-Apple/non-NVIDIA GPU. That is strictly simpler than maintaining
DirectML + a non-ort Linux runtime. The reason it is not the near-term pick is only
maturity/proof, not architecture.

## Option 3 - non-`ort` runtimes (only if Linux-AMD becomes hard, before WebGPU matures)

If Linux-AMD/Intel is a HARD requirement before the WebGPU EP is proven, you leave ONNX
Runtime entirely. Every option here costs a model conversion (away from our ONNX) and a
weaker Rust story. Summary of the field (full per-runtime assessment was researched;
condensed here):

| runtime | GPU backend | op coverage for ViT + RoPE/RMSNorm transformer | Rust | ONNX in? | verdict |
|---|---|---|---|---|---|
| **ncnn** (Tencent) | **Vulkan** (first-class) | BEST - RMSNorm + RoPE are native ops; MHA, LayerNorm, GLU present; Vulkan SDPA path recent | WEAK - stale crates (2021-23), realistically FFI the C API yourself | No - convert via PNNX (prefers PyTorch source); onnx2ncnn unreliable for transformers | Best Vulkan op story; cost is self-maintained Rust FFI + conversion loop |
| **Burn** (tracel-ai) + wgpu | **Vulkan** (AMD + Intel, one backend) | transformer ops largely present; no fused RMSNorm (Gemma usually exports as primitives - verify); attention coalesced | BEST - it generates Rust | `burn-onnx` codegens from ONNX; importer is the weak link ("limited operators") | Only pure-Rust one-runtime-both-vendors-via-Vulkan path; highest upside, highest risk |
| **MNN** (Alibaba) | OpenCL (Vulkan weak from Rust) | good; LLM RMSNorm/attention fusion | `mnn` crate exists but Vulkan unimplemented from Rust -> effectively OpenCL only | MNNConvert imports ONNX (not turnkey for Gemma) | OpenCL not Vulkan from Rust; lopsided GPU op coverage |
| **TVM** (Apache) | Vulkan (SPIR-V) | compilable, but Relax ONNX frontend has gaps (e.g. Attention attn_mask) | abandoned crate (0.1.1-alpha, 2021) -> FFI libtvm_runtime | ONNX in, then fight frontend gaps + autotuning | heavy, research-flavored; weeks of effort |
| **wonnx** | WebGPU/Vulkan, pure Rust | MISSING LayerNorm, fused attention, RMSNorm, RoPE; no dynamic shapes | native Rust | yes but models would FAIL to load | DEAD END - repo archived read-only May 2025 |

Sources for this table: [12][13][14][15][16][17][18].

Also ruled OUT for our case: DirectML (Windows-only - that is Option 1, not a Linux
answer); candle (HuggingFace Rust - no Vulkan/AMD); MLC-LLM (decoder-LLM-only, no ONNX,
no vision tower); Kompute (a Vulkan-compute framework, not an inference runtime - using
it means writing our own engine).

Vendor-specific `ort` EPs as a middle path. Worth noting because they keep ONNX AND
keep `ort`: `ort` also exposes `OpenVINOExecutionProvider` (Intel iGPU/Arc/NPU, mature,
Intel-maintained) and `MIGraphXExecutionProvider` (AMD ROCm). These are NOT Vulkan and
NOT cross-vendor, but they run our ONNX on Linux with Rust support today. The cost is a
per-vendor split (OpenVINO for Intel, MIGraphX for AMD) and, for AMD, ROCm's narrow
consumer-GPU support matrix (RDNA3 official; older/odd SKUs spotty). If Linux-AMD or
Linux-Intel must work before WebGPU matures and we want to stay on ONNX/`ort`, these
beat leaving ONNX for ncnn/Burn. [1][14][15] Effort: medium, and gated on the user
having a ROCm/OpenVINO-supported GPU and runtime libs installed.

## Recommended path + sequencing

Priority is LOW relative to the funded targets (M1 done, 5080 = TensorRT/CUDA next).
Sequence when this bucket is actually picked up:

1. **DirectML EP for Windows non-NVIDIA (do first when this bucket opens).** Cheapest
   real win, zero conversion, mirrors CoreML/CUDA wiring, reuses the FP16 artifact, and
   covers every Windows AMD/Intel/iGPU machine. Add the `directml` cargo feature, a
   Windows EP-registration arm in `OrtEmbedder::clip`, extend the per-model gate, then
   build + measure on a Windows non-NVIDIA box. This is the analog the matrix already
   lists as "OPTION (any DX12 GPU, no CUDA needed)."

2. **Spike the WebGPU EP as the strategic cross-platform bet (in parallel / right
   after).** Confirm whether pyke's prebuilt carries WebGPU for our triples or whether
   we must build ORT with `--use_webgpu` + Dawn. Then spike our FP16 CLIP + text towers
   under it on real AMD AND Intel hardware, on Windows AND Linux, measuring (a) full-GPU
   execution vs per-op CPU fallback and (b) speedup vs CPU. IF it holds, WebGPU EP
   becomes the single non-Apple/non-NVIDIA GPU path and can RETIRE the need for both a
   separate DirectML arm (it overlaps on Windows) and any non-ort Linux runtime. This is
   the one that could collapse the whole matrix to one extra EP. Decision gate: ship
   WebGPU only if the spike shows real, full-GPU speedup; otherwise keep DirectML for
   Windows and defer Linux.

3. **Linux-AMD/Intel only if it becomes a hard requirement before WebGPU is proven.**
   Prefer the `ort`-native vendor EPs (OpenVINO for Intel, MIGraphX for AMD) to stay on
   ONNX. Only drop to a non-ort runtime (ncnn via PNNX, or a Burn/wgpu spike) if those
   do not cover the target GPU - and budget for the conversion + Rust-FFI cost
   explicitly. Do NOT pursue wonnx (archived) or Kompute (not a runtime).

Everything here lands on the EXISTING fallback ladder: best EP per model at startup ->
on any load/validate failure, CPU int8 -> if CPU fails, semantic search degrades to
keyword, journal unaffected (Tier 0). None of these options change that floor.

## Open questions to resolve at build time (not now)

- Windows GPU policy: when the WebGPU EP is proven, do we run ALL Windows GPUs on it
  (one path) or keep DirectML for non-NVIDIA + CUDA for NVIDIA (best-of-breed per
  vendor)? Decide from the spike numbers - CUDA likely wins on NVIDIA regardless.
- Prebuilt vs self-built ORT: does the pyke download binary carry WebGPU for our exact
  target triples, or must we ship a custom ORT + Dawn? Dawn is a heavy dependency; this
  materially affects the WebGPU effort estimate.
- The text embedder: EmbeddingGemma is BEST on CPU int8 even on Apple (the transformer
  graph barely partitions to the ANE - `docs/SPIKE-COREML-TEXT.md`). Before wiring ANY
  GPU EP for it on AMD/Intel, re-ask whether it is worth it at all, or whether ONLY CLIP
  graduates to these GPUs (as on Apple, where only CLIP went to CoreML). Likely: CLIP
  yes, text stays CPU - measure to confirm.
- DirectML int8: confirmed DirectML does not do int8 [6] - so the int8 tower stays the
  CPU path on Windows too, exactly as elsewhere. No action, just do not feed int8 to it.

## Sources

[1] ort 2.0 execution_providers module - struct list incl. DirectMLExecutionProvider,
WebGPUExecutionProvider, CUDA/CoreML/ROCm/MIGraphX/OpenVINO; no Vulkan member.
https://docs.rs/ort/2.0.0-rc.10/ort/execution_providers/index.html
[2] ort 2.0 EP registration / cargo feature model (per-EP feature flags; v2 builder
`ep::DirectML::default()...build()`); pykeio/ort. https://deepwiki.com/pykeio/ort
[3] DirectML EP - any DX12 GPU on Windows (AMD/Intel/NVIDIA).
https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html
[4] pyke prebuilt binaries include DirectML "in any build if the platform supports it";
"binaries with the WebGPU EP are available on Windows & Linux"; CUDA+WebGPU combo falls
back to CPU. https://ort.pyke.io/perf/execution-providers (via crates.io/deepwiki index)
[5] ORT build docs - DirectML is Windows-only (`build.bat --use_dml`).
https://onnxruntime.ai/docs/build/eps.html
[6] DirectML supports FP16 (and INT4) but NOT INT8 ("DirectML backend doesn't support
8-bit precision"); FP16 ONNX deploys on DirectML.
https://nvidia.github.io/TensorRT-Model-Optimizer/deployment/2_directml.html ;
https://github.com/microsoft/onnxruntime/issues/10604
[7] Dawn reaches the GPU via DX12 or Vulkan; Intel used Vulkan to accelerate the WebGPU
EP. (Intel) https://www.intel.com/content/www/us/en/developer/articles/community/boost-ai-inference-performance-with-webgpu.html
[8] WebGPU EP overview / usage. https://onnxruntime.ai/docs/tutorials/web/ep-webgpu.html
[9] WebGPU EP native kernels - Conv, MatMul/Gemm, normalizations, Attention/MHA/GQA,
rotary (RoPE), quantized matmul. (ORT v1.25 release notes / EP op support)
https://github.com/microsoft/onnxruntime/releases/tag/v1.25.0
[10] Native WebGPU EP not in default get_available_providers() on desktop; needs an
enabling build/package. https://github.com/microsoft/onnxruntime/issues/26295
[11] Native (non-web) WebGPU EP feature request + incremental landing.
https://github.com/microsoft/onnxruntime/issues/22077
[12] ncnn operators (RMSNorm, RotaryEmbed/RoPE, MultiHeadAttention native) + Vulkan FAQ.
https://github.com/Tencent/ncnn/wiki/operators ;
https://github.com/Tencent/ncnn/blob/master/docs/how-to-use-and-FAQ/FAQ-ncnn-vulkan.md
[13] Burn ONNX op support + wgpu(Vulkan) backend.
https://github.com/tracel-ai/burn-onnx/blob/main/SUPPORTED-ONNX-OPS.md ;
https://burn.dev/books/burn/import/onnx-model.html
[14] ORT OpenVINO EP (Intel iGPU/Arc/NPU) + Rust.
https://onnxruntime.ai/docs/execution-providers/OpenVINO-ExecutionProvider.html ;
https://github.com/intel/openvino-rs
[15] ORT MIGraphX EP (AMD ROCm) + ROCm consumer-GPU support matrix.
https://onnxruntime.ai/docs/execution-providers/MIGraphX-ExecutionProvider.html ;
https://rocm.docs.amd.com/projects/install-on-linux/en/latest/reference/system-requirements.html
[16] MNN (Rust crate Vulkan unimplemented -> OpenCL; lopsided GPU op coverage).
https://github.com/alibaba/MNN ; https://docs.rs/mnn/latest/mnn/
[17] TVM Vulkan target + Relax ONNX frontend gaps; Rust crate stale.
https://tvm.apache.org/docs/arch/runtimes/vulkan.html ;
https://tvm.apache.org/docs/reference/api/python/relax/frontend.html
[18] wonnx archived read-only May 2025; missing LayerNorm/attention/RMSNorm/RoPE.
https://github.com/webonnx/wonnx
