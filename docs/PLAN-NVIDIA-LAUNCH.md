# PLAN-NVIDIA-LAUNCH — wiring the validated 85.79x GPU CLIP path into the app launch

> Companion to **docs/PLAN-ORT-BLACKWELL.md** (the validated recipe: cuda13
> onnxruntime tarball carries real sm_120 SASS; load it via `ort/load-dynamic`
> + `ORT_DYLIB_PATH` + `LD_LIBRARY_PATH`). PLAN-ORT-BLACKWELL proved the path in
> the `cuda_spike` harness. THIS plan moves it into the running desktop app.

## What landed (the code)

A small launch-time module, `apps/desktop/src-tauri/src/ort_runtime.rs`, plus
three desktop-crate cargo features (`cuda`, `tensorrt`, `cuda-dynamic`) that
forward to the connectors crate's existing GPU features.

`ort_runtime::resolve()` is called as the **first statement of `main()`**
(`apps/desktop/src-tauri/src/main.rs`), before the WebKit env workaround, before
`run()`, and therefore before Tauri starts, before the plan-converge thread
spawns, and before the in-process `ort` embedder builds its first session.

### Why first-statement-of-main

`ort` reads `ORT_DYLIB_PATH` and `dlopen`s the library the **first time a
session is built** — which on this app is the embedder converge thread, a couple
of seconds into launch. The env must be set before then. And `std::env::set_var`
is `unsafe` in a multi-threaded process (it races other threads reading the
environment), so it has to run while the process is still effectively
single-threaded. `main()`'s first statement is exactly that slot — the same one
the existing WebKit DMABUF workaround already uses for the same reason.

### The resolution mechanism (precedence)

On a `cuda-dynamic` build, `resolve()` locates a hardware-matched
`libonnxruntime.so` and stages the loader env. **First hit wins; a miss is
non-fatal** (the app falls through to `ort`'s bundled library, which still runs
on CPU — launch is never blocked):

1. **`PHOTOPROOF_ORT_DYLIB`** — an explicit full path to the `libonnxruntime.so`.
   The escape hatch: the margo dev/test shell exports the extracted-tarball path
   directly; a sysadmin staging the lib outside app-data uses this with no
   rebuild.
2. **`{app_data}/runtime/onnxruntime-cuda/lib/`** — the CONVENTIONAL staging
   location (the analog of the models dir). `resolve()` picks the bare
   `libonnxruntime.so` if the tarball shipped the symlink, else the highest
   versioned `libonnxruntime.so.N` (e.g. `libonnxruntime.so.1.26.0`).

On a hit, `ORT_DYLIB_PATH` is set to the chosen `.so` and `LD_LIBRARY_PATH` is
**prepended** (so the staged runtime wins over any system copy) with, in order:

- the onnxruntime lib dir (its `*_providers_cuda.so` / `*_providers_tensorrt.so`);
- the **TensorRT** lib dir — `PHOTOPROOF_TRT_LIBS` if set, else the conventional
  `{app_data}/runtime/tensorrt/lib/` when it exists;
- the **CUDA** toolkit libs — `/opt/cuda/lib64` and `/usr/lib` (Arch layout +
  distro cuDNN), skipped if absent.

`{app_data}` on Linux is `$XDG_DATA_HOME/com.photoproof.desktop` (falling back to
`$HOME/.local/share/com.photoproof.desktop`) — computed without the Tauri handle,
which does not exist yet at `main()`'s first statement, but matching Tauri's
`app_data_dir()` exactly so it lands beside `models/`, `runtime/`, `previews/`.

Once the env is set, the rest is already wired: the connectors crate's
`select_clip_accel` returns `Accel::Nvidia` for the `-fp16` CLIP model on a
`cuda`-feature build, and `build_session_with_nvidia` registers the
TensorRT-fp16 -> CUDA -> CPU `fail_silently` ladder with on-disk engine/timing
caches (`ort_embedder.rs`). The `cuda-dynamic` feature pulls `cuda` + `tensorrt`
transitively, so all three EP rungs are compiled in; whichever libs are actually
staged on `LD_LIBRARY_PATH` is what engages at runtime.

### Platform / feature gating (macOS + CPU are untouched)

The whole mechanism is behind the `cuda-dynamic` cargo feature, an **NVIDIA-only
(Linux x86_64) build**. On macOS and the default CPU build the feature is OFF,
`resolve()` is a compiled-away no-op, and **nothing sets `ORT_DYLIB_PATH`** — the
macOS CoreML path and the CPU path keep `ort`'s bundled binary, byte-for-byte
unchanged. `cargo fmt` / `clippy` / `test` stay green on the default build (the
`nvidia` submodule and its unit tests only compile under the feature).

---

## Distribution design — how the cuda13 onnxruntime + TensorRT libs ship

The cuda13 onnxruntime tarball is ~200 MB and the TensorRT libs are larger still
(`libnvinfer_builder_resource_sm120` alone is hundreds of MB). Bundling them into
every installer would bloat the CPU/AMD/Intel download for the minority of users
on an NVIDIA GPU, and TensorRT carries an NVIDIA license the user must accept.
So the runtime libs are **consent-gated, on-demand downloads — exactly like the
models** (RUNTIME §10 download flow), staged into the conventional path that
`resolve()` already reads. The recommended design, in order of preference:

### Option A (recommended) — consent-gated runtime download, mirroring models

Treat the onnxruntime-cuda runtime + the TensorRT libs as two more **manifest
entries** in the runtime download flow, offered only when the hardware probe
detects an NVIDIA Blackwell-class GPU (tier/hardware gate already exists in
`hardware.rs` / the runtime plan). The download manager already does pinned,
resumable, checksum-verified, license-gated fetches into `{app_data}` (see
`crates/photoproof-core/src/runtime/` + `runtime_download.rs`); these two
artifacts slot into the same machinery:

| artifact | source | staged to |
|----------|--------|-----------|
| onnxruntime-cuda13 | the official `onnxruntime-linux-x64-gpu_cuda13-<ver>.tgz` GitHub release asset, re-hosted on the PhotoProof model CDN (pinned by sha256) | `{app_data}/runtime/onnxruntime-cuda/` (so `lib/libonnxruntime.so*` lands where `resolve()` looks) |
| tensorrt | the NVIDIA TensorRT `tensorrt_libs/` (CUDA-13 build, sm_120), re-hosted with the NVIDIA license recorded as the gate the user accepts | `{app_data}/runtime/tensorrt/lib/` |

WHY re-host rather than fetch from GitHub/NVIDIA directly: the same reasons the
models are re-hosted — a single pinned sha256, resumable CDN, and an offline-
reproducible install. The TensorRT license acceptance reuses the existing
`Acceptances` / consent record (RUNTIME §10.3); onnxruntime (MIT) needs no gate.
TensorRT is OPTIONAL: if only the onnxruntime-cuda artifact is staged, the EP
ladder runs the CUDA rung (the validated 54x) and skips TensorRT (the 85.79x
upgrade) cleanly via `fail_silently`.

This is the cleanest design because it requires **no new code in this module** —
`resolve()` already reads the conventional path, so the only work is adding the
two manifest entries + the hardware-gated offer. That work belongs to the runtime
/ manifest owners (out of this packet's scope) and is noted here as the intended
follow-up.

### Option B — per-platform NVIDIA installer

Ship a separate `Photoproof-nvidia` installer (deb/AppImage) built with
`--features cuda-dynamic` that bundles the cuda13 onnxruntime + TensorRT libs and
unpacks them into `{app_data}/runtime/...` (or a bundled resource dir also placed
on the precedence list) on first run. Heavier per-download, but a single
self-contained artifact for managed/offline fleets. The CPU/AMD/Intel installer
stays the default build (feature off, no GPU libs).

### Option C — documented manual drop (the bootstrap / today's margo)

The user (or an admin) extracts the cuda13 tarball + TensorRT libs into
`{app_data}/runtime/onnxruntime-cuda/lib/` and `{app_data}/runtime/tensorrt/lib/`
by hand, or points `PHOTOPROOF_ORT_DYLIB` / `PHOTOPROOF_TRT_LIBS` at an existing
extraction. This is the bootstrap path and exactly what the margo test steps
below do. It needs zero distribution work and is the fallback whenever A/B have
not shipped.

All three land artifacts at the SAME conventional paths / env knobs `resolve()`
reads, so the code is identical regardless of which distribution channel wins.

---

## Testing on margo (exact steps)

margo: Arch Linux, Ryzen 9900X + RTX 5080 (sm_120), CUDA 13.3, driver 610. The
repo lives at `~/projects/photoproof`. **Do not run these from this agent — they
are the founder's manual margo steps** (the GPU path cannot build on the M1).

### 0. Get the branch onto margo

```bash
# from the dev machine: push this worktree branch, then on margo:
ssh caleb@margo.local 'bash -lc "cd ~/projects/photoproof && git fetch && git checkout <this-branch> && git pull"'
```

### 1. Stage the cuda13 onnxruntime (carries sm_120) — Option C bootstrap

Either point the override at an existing extraction, OR drop it at the
conventional app-data path. Conventional path (so the app finds it with no env):

```bash
APPDATA="$HOME/.local/share/com.photoproof.desktop"   # = Tauri app_data on Linux
ORT_VER=1.26.0
mkdir -p "$APPDATA/runtime/onnxruntime-cuda"
cd "$APPDATA/runtime/onnxruntime-cuda"
curl -L -O https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VER}/onnxruntime-linux-x64-gpu_cuda13-${ORT_VER}.tgz
tar --strip-components=1 -xzf onnxruntime-linux-x64-gpu_cuda13-${ORT_VER}.tgz
ls lib/libonnxruntime.so*        # expect libonnxruntime.so(.1.26.0) + *_providers_cuda.so
# PROVE it carries sm_120 SASS (the load-bearing check from PLAN-ORT-BLACKWELL):
cuobjdump --list-elf lib/libonnxruntime_providers_cuda.so | grep -i sm_120   # MUST print sm_120 lines
```

### 2. (optional, for the 85.79x TensorRT rung) stage the TensorRT libs

```bash
APPDATA="$HOME/.local/share/com.photoproof.desktop"
# the validated source: pip 'tensorrt-cu12<11' in a py3.12 venv ships
# tensorrt_libs/ with libnvinfer_builder_resource_sm120 (full Blackwell).
python3.12 -m venv ~/trt-venv && source ~/trt-venv/bin/activate
pip install 'tensorrt-cu12<11'
TRT_LIBS=~/trt-venv/lib/python3.12/site-packages/tensorrt_libs
mkdir -p "$APPDATA/runtime/tensorrt"
ln -sfn "$TRT_LIBS" "$APPDATA/runtime/tensorrt/lib"   # or copy the dir
ls "$APPDATA/runtime/tensorrt/lib"/libnvinfer.so.10*  # ORT 1.26 TRT-EP wants .so.10
```

If TensorRT is NOT staged, the app runs the CUDA rung (54x) and skips TensorRT
gracefully — staging it is the 85.79x upgrade, not a prerequisite.

### 3. Build + run the app with the GPU feature

```bash
cd ~/projects/photoproof/apps/desktop/src-tauri
# Dev run (tauri dev) — pass the feature through to cargo:
cargo tauri dev -- --features cuda-dynamic
# OR a plain build of the binary to inspect the launch logs:
cargo run -p photoproof-desktop --features cuda-dynamic
```

cuDNN must be present for the CUDA EP (`sudo pacman -S --needed cudnn`,
`/usr/lib/libcudnn.so*`) — `/usr/lib` is already on the staged `LD_LIBRARY_PATH`.

### 4. Confirm the GPU path engaged

On launch, stderr (and `{app_data}/logs/photoproof.log`) should show the
resolution, then the EP registration once the CLIP embedder builds:

```
[ort runtime] ORT_DYLIB_PATH = /home/caleb/.local/share/com.photoproof.desktop/runtime/onnxruntime-cuda/lib/libonnxruntime.so
[ort runtime] LD_LIBRARY_PATH = .../onnxruntime-cuda/lib:.../tensorrt/lib:/opt/cuda/lib64:/usr/lib
[ort nvidia] registered NVIDIA EP ladder (TensorRT-fp16 -> CUDA -> CPU) for .../ViT-H-14-378-quickgelu__dfn5b-fp16/visual/model.onnx
```

Then exercise an **image search** (forces the CLIP visual tower): the embedding
drain / search should run on the GPU. Cross-check with `nvidia-smi` (a python
process is not needed — the app itself shows GPU memory + util while embedding).
First TensorRT use builds the engine (~24 s, cached to `.trt-cache` beside the
model); subsequent launches are instant. A missing/incompatible TensorRT lib just
falls to the CUDA rung — the log line above still prints, the search still runs.

### Override quick-test (skip the app-data staging)

```bash
ORT_DIR=~/onnxruntime-linux-x64-gpu_cuda13-1.26.0          # an existing extraction
TRT_LIBS=~/trt-venv/lib/python3.12/site-packages/tensorrt_libs
PHOTOPROOF_ORT_DYLIB="$ORT_DIR/lib/libonnxruntime.so" \
PHOTOPROOF_TRT_LIBS="$TRT_LIBS" \
  cargo run -p photoproof-desktop --features cuda-dynamic
```

`resolve()` still prepends `/opt/cuda/lib64` + `/usr/lib` automatically, so only
the two overrides are needed.

### What "done on margo" looks like

The app launches, the `[ort runtime]` + `[ort nvidia]` log lines appear, an image
search runs on the GPU (visible in `nvidia-smi`), and the result quality matches
the CPU baseline (the `cuda_spike` already proved cosine ~0.9999). That is the
85.79x path live in the product, not just the spike.

---

## File map

- `apps/desktop/src-tauri/src/ort_runtime.rs` — the resolution module (`resolve()`).
- `apps/desktop/src-tauri/src/main.rs` — calls `ort_runtime::resolve()` first.
- `apps/desktop/src-tauri/src/lib.rs` — `pub mod ort_runtime;`.
- `apps/desktop/src-tauri/Cargo.toml` — `cuda` / `tensorrt` / `cuda-dynamic`
  features forwarding to `photoproof-connectors`.
- `crates/photoproof-connectors/src/ort_embedder.rs` (unchanged) — `select_clip_accel`
  + `build_session_with_nvidia` (the EP ladder this launch feeds).
- `docs/PLAN-ORT-BLACKWELL.md` — the validated recipe this wires in.
