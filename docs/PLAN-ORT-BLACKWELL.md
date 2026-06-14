# PLAN-ORT-BLACKWELL — GPU inference on the RTX 5080 (Blackwell sm_120 + CUDA 13.3)

Status: READY-TO-EXECUTE on margo (Arch Linux, Ryzen 9900X, RTX 5080, CUDA 13.3,
driver 610, `paru`, sudo). No code change required — PhotoProof's `cuda-dynamic`
feature already loads a system `libonnxruntime.so` via `ORT_DYLIB_PATH`
(`crates/photoproof-connectors/Cargo.toml:62`, `ort/load-dynamic`). This plan
finds, installs, and validates a `libonnxruntime.so` that carries **real sm_120
(Blackwell) CUDA kernels**.

## THE PROBLEM (recap, now diagnosed to the exact cause)

Every prebuilt onnxruntime we tried tops out at **sm_90 (Hopper)** -> the RTX 5080
is **sm_120** -> `cudaErrorNoKernelImageForDevice`. The root cause is verified in
ORT's own CI build script: the **CUDA 12.8** wheel line is built with arch list
`60-real;70-real;75-real;80-real;86-real;90a-real;90-virtual` (max sm_90), while
the **CUDA 13.0** line is built with `75-real;80-real;86-real;89-real;90-real;100-real;120-real;120-virtual`
— **`120-real` = compiled SASS for sm_120**.
Source (pinned to the release tag):
<https://raw.githubusercontent.com/microsoft/onnxruntime/v1.26.0/tools/ci_build/github/linux/build_linux_python_package.sh>

So "ORT 1.26 tops out at sm_90" is true ONLY for the default PyPI cu12 wheel and
ort 2.0.0-rc.12's bundled binary. **The CUDA-13 build of ORT 1.26 ships Blackwell
kernels.** That is the win — no source build.

---

## THE BIG ANSWER: a prebuilt sm_120 onnxruntime EXISTS

**Yes.** ONNX Runtime 1.26.0's **CUDA-13 build** includes `120-real` (real sm_120
SASS) + `120-virtual` (PTX fallback). It ships two ways:

1. A **C/C++ release tarball**: `onnxruntime-linux-x64-gpu_cuda13-1.26.0.tgz`
   (confirmed a real asset on the v1.26.0 release — `gh release view v1.26.0`).
   This is the cleanest source of a `libonnxruntime.so` for `ORT_DYLIB_PATH`.
2. A **pip wheel** on the official CUDA-13 nightly feed (Linux x86_64, **cp314**):
   index `https://aiinfra.pkgs.visualstudio.com/PublicPackages/_packaging/ort-cuda-13-nightly/pypi/simple/`.

CUDA 13.2/**13.3** builds were explicitly enabled in ORT commit #28736
("enable CUDA 13.3 builds", 2026-06-01) — matches margo's 13.3 exactly. CUDA minor
versions are forward-compatible, so a cu13.0-built artifact runs on CUDA 13.3 /
driver 610.
Sources: <https://onnxruntime.ai/docs/install/> ·
<https://github.com/microsoft/onnxruntime/releases/tag/v1.26.0> ·
<https://github.com/microsoft/onnxruntime/issues/26177> (sm_120 PTX-vs-SASS history)

> One caveat carried by the whole ORT-on-Blackwell ecosystem: ORT's **flash-attention**
> kernels are sm_80-specific and can error on sm_120. CLIP ViT does not use ORT's
> fused flash-attention path, so this should not bite us; if a model ever hits it,
> that's the culprit, not a missing arch.

---

## RANKED OPTIONS  (likelihood-of-working x speed-to-result)

| # | Option | Works on sm_120? | Speed to result | Risk | Verdict |
|---|--------|------------------|-----------------|------|---------|
| **1** | **Prebuilt ORT 1.26 cu13 tarball** (`gpu_cuda13`) -> `ORT_DYLIB_PATH` | **Yes** (`120-real` SASS) | **~5 min** (download + extract) | Low | **DO THIS FIRST** |
| 2 | Prebuilt ORT 1.26 cu13 **pip wheel** (cp314 nightly feed) | Yes (same build) | ~5 min | Low | Fallback A (already have a py3.14 venv) |
| 3 | TensorRT EP (AUR/tar TRT 10.13+ cuda-13) on top of the cu13 ORT | Yes (JIT per-device) | ~30-60 min | Med | Fallback B / **perf upgrade** once #1 runs |
| 4 | Build ORT v1.26.0 from source, `CMAKE_CUDA_ARCHITECTURES=120` | Yes (if patched) | **45-90 min build** | Med-High | Last resort |

The CUDA EP alone (#1) is enough to get GPU inference working and unblock the
`cuda_spike`. TensorRT (#3) is the *faster* runtime but is an optimization, not a
prerequisite — pursue it only after #1 proves the GPU path.

---

## ★ RECOMMENDATION #1 — Prebuilt ORT 1.26 cu13 tarball (copy-paste for margo)

The `gpu_cuda13` C/C++ tarball contains exactly the files `load-dynamic` needs:
`lib/libonnxruntime.so` (+ `libonnxruntime_providers_shared.so`,
`libonnxruntime_providers_cuda.so`, and `libonnxruntime_providers_tensorrt.so` if
built). We point `ORT_DYLIB_PATH` at the main `.so` and put the dir on
`LD_LIBRARY_PATH` so the provider `.so`s and CUDA/cuDNN runtime resolve.

```bash
# --- 0. prerequisites (CUDA runtime + cuDNN 9.x already match CUDA 13 on Arch) ---
#   margo already has CUDA 13.3 at /opt/cuda. cuDNN is needed by the CUDA EP:
sudo pacman -S --needed cudnn          # extra/cudnn is built for CUDA >= 13
#   sanity: the loader must see /opt/cuda/lib64 + cuDNN (in /usr/lib)
ls /opt/cuda/lib64/libcudart.so*  /usr/lib/libcudnn.so*

# --- 1. download + extract the CUDA-13 ORT 1.26 tarball (sm_120 kernels) ---
ORT_VER=1.26.0
cd ~
curl -L -O https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VER}/onnxruntime-linux-x64-gpu_cuda13-${ORT_VER}.tgz
tar -xzf onnxruntime-linux-x64-gpu_cuda13-${ORT_VER}.tgz
ORT_DIR=~/onnxruntime-linux-x64-gpu_cuda13-${ORT_VER}
ls "$ORT_DIR/lib"        # expect libonnxruntime.so(.1.26.0), *_providers_cuda.so, *_providers_shared.so

# --- 2. PROVE it carries sm_120 SASS before trusting it (the load-bearing check) ---
#   Real sm_120 entries (not just compute_120 PTX) must be present:
cuobjdump --list-elf "$ORT_DIR/lib/libonnxruntime_providers_cuda.so" | grep -i sm_120
#   -> you MUST see sm_120 ELF lines. If you only see sm_90 / compute_120, STOP and
#      use Fallback B (TensorRT) or #4 (source build). (cuobjdump ships with CUDA.)

# --- 3. point PhotoProof's load-dynamic at it ---
export ORT_DYLIB_PATH="$ORT_DIR/lib/libonnxruntime.so"
#   provider + CUDA + cuDNN runtime libs must resolve at dlopen:
export LD_LIBRARY_PATH="$ORT_DIR/lib:/opt/cuda/lib64:/usr/lib:${LD_LIBRARY_PATH:-}"
```

If `libonnxruntime.so` is a versioned symlink target only (e.g. the dir has
`libonnxruntime.so.1.26.0` but no bare `libonnxruntime.so`), point `ORT_DYLIB_PATH`
straight at the versioned file:
`export ORT_DYLIB_PATH="$ORT_DIR/lib/libonnxruntime.so.${ORT_VER}"`.

### VALIDATION (the payoff — run from the repo root on margo)

The `cuda_spike` test is gated `#![cfg(feature = "cuda")]` and `cuda-dynamic`
pulls in `cuda` transitively (`cuda-dynamic = ["tensorrt", "ort/load-dynamic"]`,
which depends on `cuda`), so the feature flag below compiles and runs the spike.
The model + COCO sample paths default to `~/models/...` and `~/coco-sample`
(override with `PP_FP16_MODEL_DIR` / `COCO_IMAGES_DIR`).

```bash
cd ~/photoproof          # the repo on margo

# env from steps 2-3 above must be exported in THIS shell.
cargo test -p photoproof-connectors --features cuda-dynamic --test cuda_spike \
    -- --ignored --nocapture
```

Expected: the test prints `CPU(fp16): ... img/s`, then
`[ort nvidia] registered NVIDIA EP ladder ...` and a GPU line with a multi-x
speedup, then `cosine CPU-vs-GPU: mean ~0.9999`. A pass means sm_120 GPU inference
is live. If you instead see `NVIDIA load FAILED: ... NoKernelImageForDevice`, the
`.so` did not actually carry sm_120 — go to Fallback B or #4.

> Note: `cuda-dynamic` enables the `tensorrt` feature, so `build_session_with_nvidia`
> tries the **TensorRT EP first**, then CUDA, then CPU. If TensorRT libs are NOT
> installed yet (Fallback B not done), the TRT EP registration is best-effort and
> the ladder falls through to the **CUDA EP** — which is the sm_120 path we just
> validated. That is the intended #1 behavior: CUDA EP carries the GPU work, TRT is
> a later upgrade. (If TRT-EP registration is *fatal* rather than soft on this build
> and blocks the fall-through, build with `--features cuda` instead of
> `cuda-dynamic` for the spike — but then ort uses its OWN bundled sm_90 binary, so
> for the dynamic sm_120 `.so` you want `cuda-dynamic`. Keep TRT libs absent or
> install them per Fallback B so the ladder is happy.)

---

## Fallback A — the cu13 pip wheel (you already have a py3.14 venv)

margo has `~/ort-gpu-venv` (py3.14) with the wrong cu12 wheel installed. Swap it
for the cu13 wheel from the official nightly feed, then point `ORT_DYLIB_PATH` at
the `.so` inside the installed package.

```bash
source ~/ort-gpu-venv/bin/activate
pip uninstall -y onnxruntime-gpu onnxruntime
pip install coloredlogs flatbuffers numpy packaging protobuf sympy
pip install --pre \
  --index-url https://aiinfra.pkgs.visualstudio.com/PublicPackages/_packaging/ort-cuda-13-nightly/pypi/simple/ \
  onnxruntime-gpu

# locate the .so the wheel installed:
ORT_PKG=$(python -c "import onnxruntime, os; print(os.path.dirname(onnxruntime.__file__))")
ls "$ORT_PKG/capi"/libonnxruntime*.so*
cuobjdump --list-elf "$ORT_PKG/capi/libonnxruntime_providers_cuda.so" | grep -i sm_120   # must show sm_120

export ORT_DYLIB_PATH="$ORT_PKG/capi/libonnxruntime.so"     # or the versioned name ls shows
export LD_LIBRARY_PATH="$ORT_PKG/capi:/opt/cuda/lib64:/usr/lib:${LD_LIBRARY_PATH:-}"
```

Then the same `cargo test ... --features cuda-dynamic --test cuda_spike` validation.
The wheel and the tarball are the same build; use whichever lands first. The
tarball (#1) is preferred because it has no Python coupling and cleaner `.so` paths.
Source: <https://onnxruntime.ai/docs/install/>

---

## Fallback B — TensorRT EP (perf upgrade; JIT sidesteps any kernel gap)

TensorRT JIT-compiles a per-device engine at runtime, so it works on sm_120 even
if a CUDA-EP `.so` somehow lacked the kernels. It's also the *fastest* runtime.
This layers ON TOP of the cu13 ORT from #1 (ORT 1.26's TRT-EP links
`libnvinfer.so.10`). Keep everything on **CUDA 13** — never mix a cuda-12 TensorRT
with a `gpu_cuda13` ORT.

```bash
# TensorRT 10.13+ supports Blackwell sm_120 (added in 10.8) and ships a CUDA-13
# build. AUR builds it against Arch's system CUDA (13.x):
paru -Si tensorrt        # CONFIRM pkgver >= 10.13 and depends on cuda (13.x)
paru -S tensorrt         # installs libnvinfer.so.10 etc into /usr/lib (no LD path needed)

# If AUR lags < 10.13, use the official NVIDIA tar (pick the CUDA-13 variant):
#   TensorRT-10.16.x.x.Linux.x86_64-gnu.cuda-13.0.tar.gz  (from developer.nvidia.com/tensorrt, login-gated)
#   tar -xzf TensorRT-10.16.*.cuda-13.0.tar.gz
#   export LD_LIBRARY_PATH="$PWD/TensorRT-10.16.*/lib:$LD_LIBRARY_PATH"

ls /usr/lib/libnvinfer.so.10*    # ORT 1.26 TRT-EP wants the .so.10 SONAME
```

cuDNN is **optional** for TensorRT 10.x ONNX inference — skip unless a model needs
it. With TRT libs present, the same `cuda_spike` run will register TensorRT-FP16
first in the ladder; first inference is slow (engine build) then cached
(`trt_engine_cache_enable`, wired in `build_session_with_nvidia`,
`crates/photoproof-connectors/src/ort_embedder.rs:519`).
Sources: <https://docs.nvidia.com/deeplearning/tensorrt/latest/getting-started/support-matrix.html>
(sm_120) · <https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html>
(TRT-EP wants libnvinfer.so.10) ·
<https://docs.nvidia.com/deeplearning/tensorrt/latest/getting-started/release-notes-10/10.13.2.html>
(CUDA 13 build) · AUR: <https://aur.archlinux.org/packages/tensorrt>

---

## #4 — Build ORT v1.26.0 from source for sm_120 (last resort)

Only if BOTH prebuilt paths fail the `cuobjdump`/runtime check. ORT **1.26** is the
first release with first-class CUDA-13 support; build it against margo's CUDA 13.3
+ Arch cuDNN 9.x, targeting `CMAKE_CUDA_ARCHITECTURES=120`.

```bash
sudo pacman -S --needed cuda cudnn cmake ninja python gcc
export CUDA_HOME=/opt/cuda
export CUDNN_HOME=/usr                       # Arch cudnn lives under /usr

git clone --recursive --branch v1.26.0 https://github.com/microsoft/onnxruntime
cd onnxruntime

# CRITICAL PATCH: ORT rewrites 120 -> 120a (accelerated arch). Blackwell CONSUMER
# GPUs have NO 120a kernels -> NoKernelImageForDevice at runtime. Drop 90 and 120
# from the accel list in cmake/external/cuda_configuration.cmake so plain sm_120 is
# emitted:
#   set(ARCHITECTURES_WITH_ACCEL "100" "101")   # was: "90" "100" "101" "120"

./build.sh \
  --config Release --build_shared_lib \
  --parallel 16 --nvcc_threads 1 \
  --use_cuda --cuda_home "$CUDA_HOME" --cudnn_home "$CUDNN_HOME" \
  --skip_tests --allow_running_as_root \
  --cmake_generator Ninja \
  --cmake_extra_defines CMAKE_CUDA_ARCHITECTURES=120 \
  --cmake_extra_defines onnxruntime_USE_FLASH_ATTENTION=OFF
# output: build/Linux/Release/libonnxruntime.so (+ provider .so's) -> ORT_DYLIB_PATH
```

- **cuDNN 9.x is mandatory** for the CUDA EP; Arch `extra/cudnn` (9.23, built for
  CUDA >=13) matches. CUDA 13.3's nvcc accepts `sm_120` (added in 12.8, carried in
  13.x). Arch gcc 15 is within CUDA 13's supported range.
- **`--nvcc_threads 1`** is the OOM guard: ORT CUDA builds can OOM under ~64 GB RAM
  (`cicc died, signal 9`). If CCCL include errors appear under CUDA 13, add
  `--cmake_extra_defines CMAKE_CUDA_FLAGS="-I/opt/cuda/include/cccl"`.
- **Build time: ~45-90 min** on the 9900X (single arch, Release). Reserve ~50 GB disk.
- Validate identically: `cuobjdump ... | grep sm_120`, set `ORT_DYLIB_PATH`, run the
  `cuda_spike`.

Sources: <https://github.com/microsoft/onnxruntime/releases/tag/v1.26.0> (CUDA 13) ·
<https://onnxruntime.ai/docs/build/eps.html> (build flags) ·
<https://github.com/microsoft/onnxruntime/issues/26177> +
<https://github.com/microsoft/onnxruntime/issues/26245> (the 120a patch) ·
<https://archlinux.org/packages/extra/x86_64/cudnn/> (Arch cuDNN for CUDA 13) ·
<https://github.com/microsoft/onnxruntime/issues/23844> (nvcc OOM / `--nvcc_threads`).

---

## Quick decision tree for margo

1. Run #1 (tarball). `cuobjdump ... | grep sm_120` shows sm_120? -> set env -> run
   `cuda_spike`. **If it passes, you are done.**
2. Tarball missing the bare `.so` symlink? point at `libonnxruntime.so.1.26.0`.
3. Spike fails with `NoKernelImageForDevice` despite cuobjdump showing sm_120, or
   cuobjdump shows NO sm_120? -> do Fallback B (TensorRT) for the JIT escape hatch.
4. Both prebuilt paths fail the kernel check -> #4 source build (45-90 min).

## What "done" looks like

`cargo test -p photoproof-connectors --features cuda-dynamic --test cuda_spike --
--ignored --nocapture` prints a GPU img/s line with a multi-x speedup over CPU and
`cosine CPU-vs-GPU: mean ~0.9999`, with the env pointing at a cu13 `libonnxruntime.so`
that `cuobjdump` confirms carries sm_120 SASS.
