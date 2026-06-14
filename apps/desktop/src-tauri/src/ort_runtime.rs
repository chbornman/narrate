//! onnxruntime runtime-resolution for the NVIDIA (`cuda-dynamic`) build.
//!
//! WHAT: on a Blackwell/NVIDIA desktop the bundled `onnxruntime` that `ort`
//! ships tops out at sm_90 (Hopper) kernels, so the RTX 5080 (sm_120) hits
//! `cudaErrorNoKernelImageForDevice`. The validated fix (docs/PLAN-ORT-BLACKWELL.md,
//! confirmed live at 85.79x with TensorRT) is to load a HARDWARE-MATCHED
//! `libonnxruntime.so` — the official `onnxruntime-linux-x64-gpu_cuda13` tarball,
//! which carries real `sm_120` SASS — via `ort`'s `load-dynamic` feature, pointed
//! at by `ORT_DYLIB_PATH`, with its lib dir + the TensorRT libs + the CUDA libs on
//! `LD_LIBRARY_PATH` so the provider `.so`s and CUDA/cuDNN runtime resolve at
//! dlopen.
//!
//! WHY HERE / WHY EARLY: `ort` reads `ORT_DYLIB_PATH` and dlopen's the library the
//! FIRST time a session is built (the embedder converge thread, seconds into
//! launch). The env therefore has to be set BEFORE that — and, because
//! `std::env::set_var` is `unsafe` in a multi-threaded process (it races other
//! threads reading the environment), it must be set while the process is still
//! effectively single-threaded. So `resolve()` is called as the very first thing
//! in `main()` (the same slot the WebKit DMABUF workaround uses), before Tauri,
//! the converge loop, or any ort session exists.
//!
//! PLATFORM/FEATURE GATING: the whole mechanism is behind the `cuda-dynamic`
//! cargo feature, which is an NVIDIA-only build (Linux x86_64). On macOS and on
//! the default CPU build the feature is OFF, [`resolve`] is a documented no-op,
//! and NOTHING sets `ORT_DYLIB_PATH` — those builds keep `ort`'s bundled binary
//! and the CoreML/CPU execution providers untouched (`select_clip_accel` in
//! `photoproof-connectors` never returns `Nvidia` off a `cuda` build anyway).
//!
//! RESOLUTION ORDER (first hit wins; a miss is non-fatal — we fall through to
//! `ort`'s default/bundled library, which still works on CPU):
//!   1. `PHOTOPROOF_ORT_DYLIB` — an explicit path to the `libonnxruntime.so`
//!      (the escape hatch for a hand-staged or non-conventional location; e.g.
//!      the margo dev shell that exports the tarball path directly).
//!   2. `{data_dir}/runtime/onnxruntime-cuda/lib/` — the CONVENTIONAL staging
//!      location the distribution drops/fetches the cuda13 tarball into (the
//!      analog of the models dir). We pick the bare `libonnxruntime.so` if the
//!      tarball shipped the symlink, else the versioned `libonnxruntime.so.N`.
//!
//! When (1) or (2) resolves a library, we ALSO extend `LD_LIBRARY_PATH` with the
//! onnxruntime lib dir (its provider `.so`s), the TensorRT libs (conventional
//! `{data_dir}/runtime/tensorrt/lib/`, plus any `PHOTOPROOF_TRT_LIBS` override),
//! and the system CUDA libs (`/opt/cuda/lib64`, `/usr/lib`) so the EP ladder's
//! TensorRT -> CUDA rungs find `libnvinfer.so.10`, `libcudart.so`, `libcudnn.so`.

/// Resolve and stage the onnxruntime runtime for the current build. Call this
/// as the FIRST statement in `main()` (before any thread spawns).
///
/// On a non-`cuda-dynamic` build this is a no-op: the body compiles away and the
/// macOS/CPU launch is byte-for-byte unaffected.
pub fn resolve() {
    #[cfg(feature = "cuda-dynamic")]
    nvidia::resolve();
}

#[cfg(feature = "cuda-dynamic")]
mod nvidia {
    use std::path::{Path, PathBuf};

    /// Explicit override: a full path to the `libonnxruntime.so` to load. Wins
    /// over the conventional location. WHY an env knob: the margo dev/test shell
    /// exports the extracted-tarball path directly (docs/PLAN-NVIDIA-LAUNCH.md),
    /// and a sysadmin staging the lib outside app-data needs a no-rebuild hook.
    const ORT_DYLIB_OVERRIDE: &str = "PHOTOPROOF_ORT_DYLIB";

    /// Optional override for the TensorRT lib dir to prepend to `LD_LIBRARY_PATH`
    /// (the `pip 'tensorrt-cu12<11'` `tensorrt_libs/` dir on margo). When unset we
    /// use the conventional `{data_dir}/runtime/tensorrt/lib/` if it exists.
    const TRT_LIBS_OVERRIDE: &str = "PHOTOPROOF_TRT_LIBS";

    /// Conventional CUDA toolkit lib dirs to put on `LD_LIBRARY_PATH` so the CUDA
    /// EP's `libcudart`/`libcudnn` resolve. `/opt/cuda/lib64` is Arch's layout
    /// (margo); `/usr/lib` carries the distro cuDNN. Missing dirs are harmless —
    /// the loader just skips a non-existent path component.
    const CUDA_LIB_DIRS: [&str; 2] = ["/opt/cuda/lib64", "/usr/lib"];

    /// Stage the runtime: pick a `libonnxruntime.so`, export `ORT_DYLIB_PATH`,
    /// and extend `LD_LIBRARY_PATH` with the lib dirs the EP ladder needs.
    pub fn resolve() {
        let Some(dylib) = locate_dylib() else {
            // No hardware-matched lib staged: leave `ORT_DYLIB_PATH` unset so
            // `ort` uses its bundled binary. On a `cuda-dynamic` build with the
            // CUDA EP types compiled in, that bundled lib may still bring up the
            // CUDA EP on supported (<= sm_90) GPUs; on Blackwell it falls to CPU.
            // Either way the app launches — degraded GPU, never a crash.
            eprintln!(
                "[ort runtime] no staged onnxruntime found (set {ORT_DYLIB_OVERRIDE} or stage \
                 the cuda13 tarball at {{app_data}}/runtime/onnxruntime-cuda/lib/); \
                 using ort's bundled library"
            );
            return;
        };
        let lib_dir = dylib.parent().map(Path::to_path_buf);

        // SAFETY: called as the first statement of `main()`, before Tauri starts
        // and before any worker thread spawns — the process is effectively
        // single-threaded here, so no other thread can be reading the environment
        // concurrently (the documented precondition for `set_var`'s soundness).
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &dylib);
        }
        eprintln!("[ort runtime] ORT_DYLIB_PATH = {}", dylib.display());

        // Build the LD_LIBRARY_PATH prefix: the onnxruntime lib dir (its provider
        // .so's), then TensorRT libs, then the CUDA toolkit libs. Order is
        // first-match-wins for the loader; the onnxruntime dir leads so its own
        // bundled deps win over any system copy.
        let mut prefix: Vec<PathBuf> = Vec::new();
        if let Some(dir) = lib_dir {
            prefix.push(dir);
        }
        if let Some(trt) = tensorrt_lib_dir() {
            prefix.push(trt);
        }
        for cuda in CUDA_LIB_DIRS {
            let p = PathBuf::from(cuda);
            if p.is_dir() {
                prefix.push(p);
            }
        }
        extend_ld_library_path(&prefix);
    }

    /// Find the `libonnxruntime.so` to load: the explicit override first, else
    /// the conventional `{data_dir}/runtime/onnxruntime-cuda/lib/` directory.
    fn locate_dylib() -> Option<PathBuf> {
        if let Some(over) = std::env::var_os(ORT_DYLIB_OVERRIDE) {
            let p = PathBuf::from(over);
            if p.is_file() {
                return Some(p);
            }
            eprintln!(
                "[ort runtime] {ORT_DYLIB_OVERRIDE} set to {} but no such file; \
                 falling back to the conventional location",
                p.display()
            );
        }
        let lib_dir = data_dir()?.join("runtime/onnxruntime-cuda/lib");
        pick_onnxruntime_so(&lib_dir)
    }

    /// Choose the onnxruntime shared object inside `lib_dir`: prefer the bare
    /// `libonnxruntime.so` (the tarball's symlink), else the highest versioned
    /// `libonnxruntime.so.N[.M.P]`. Returns `None` if the dir has neither (so a
    /// half-staged dir falls through to ort's bundled lib instead of pointing at
    /// a path that fails to dlopen).
    fn pick_onnxruntime_so(lib_dir: &Path) -> Option<PathBuf> {
        let bare = lib_dir.join("libonnxruntime.so");
        if bare.is_file() {
            return Some(bare);
        }
        // No bare symlink: take the versioned file (e.g. libonnxruntime.so.1.26.0).
        // If several exist, the lexicographically-largest is the newest for the
        // single-version tarball layout we stage (one runtime per dir).
        let mut versioned: Vec<PathBuf> = std::fs::read_dir(lib_dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("libonnxruntime.so."))
            })
            .collect();
        versioned.sort();
        versioned.pop()
    }

    /// The TensorRT lib dir to stage: the explicit override, else the
    /// conventional `{data_dir}/runtime/tensorrt/lib/` when it exists. `None`
    /// means "no TensorRT staged" — the EP ladder then skips the TRT rung and the
    /// CUDA EP carries the GPU work (`fail_silently` in ort_embedder.rs).
    fn tensorrt_lib_dir() -> Option<PathBuf> {
        if let Some(over) = std::env::var_os(TRT_LIBS_OVERRIDE) {
            let p = PathBuf::from(over);
            if p.is_dir() {
                return Some(p);
            }
        }
        let dir = data_dir()?.join("runtime/tensorrt/lib");
        dir.is_dir().then_some(dir)
    }

    /// The Tauri app-data directory for `com.photoproof.desktop`, resolved WITHOUT
    /// the Tauri handle (which does not exist yet at `main()`'s first statement).
    /// On Linux that is `$XDG_DATA_HOME/com.photoproof.desktop` (Tauri's
    /// `app_data_dir`), falling back to `$HOME/.local/share/...` per the XDG base
    /// spec. `cuda-dynamic` is Linux-only, so only the Linux layout is computed.
    fn data_dir() -> Option<PathBuf> {
        const APP_ID: &str = "com.photoproof.desktop";
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let xdg = PathBuf::from(xdg);
            if xdg.is_absolute() {
                return Some(xdg.join(APP_ID));
            }
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".local/share").join(APP_ID))
    }

    /// Prepend `dirs` to `LD_LIBRARY_PATH`, preserving any existing value and
    /// dropping empty/duplicate components. Prepending (not appending) lets the
    /// staged cuda13 runtime + TensorRT win over a system onnxruntime/CUDA that a
    /// distro may also expose.
    fn extend_ld_library_path(dirs: &[PathBuf]) {
        if dirs.is_empty() {
            return;
        }
        let existing = std::env::var_os("LD_LIBRARY_PATH");
        let mut parts: Vec<PathBuf> = dirs.to_vec();
        if let Some(existing) = &existing {
            parts.extend(std::env::split_paths(existing));
        }
        // De-dup while preserving first-seen order (the staged dirs lead).
        let mut seen = std::collections::HashSet::new();
        parts.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
        let joined = std::env::join_paths(parts).expect("LD_LIBRARY_PATH join");
        // SAFETY: same single-threaded `main()` precondition as `resolve()`.
        unsafe {
            std::env::set_var("LD_LIBRARY_PATH", &joined);
        }
        eprintln!(
            "[ort runtime] LD_LIBRARY_PATH = {}",
            joined.to_string_lossy()
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn picks_bare_so_over_versioned() {
            let tmp = std::env::temp_dir().join(format!("pp-ort-pick-{}", std::process::id()));
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(tmp.join("libonnxruntime.so"), b"").unwrap();
            std::fs::write(tmp.join("libonnxruntime.so.1.26.0"), b"").unwrap();
            assert_eq!(
                pick_onnxruntime_so(&tmp),
                Some(tmp.join("libonnxruntime.so"))
            );
            let _ = std::fs::remove_dir_all(&tmp);
        }

        #[test]
        fn falls_back_to_versioned_so() {
            let tmp = std::env::temp_dir().join(format!("pp-ort-ver-{}", std::process::id()));
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(tmp.join("libonnxruntime.so.1.26.0"), b"").unwrap();
            std::fs::write(tmp.join("libonnxruntime.so.1.25.0"), b"").unwrap();
            // Highest version wins (lexicographic sort is correct for this layout).
            assert_eq!(
                pick_onnxruntime_so(&tmp),
                Some(tmp.join("libonnxruntime.so.1.26.0"))
            );
            let _ = std::fs::remove_dir_all(&tmp);
        }

        #[test]
        fn none_when_no_onnxruntime_present() {
            let tmp = std::env::temp_dir().join(format!("pp-ort-empty-{}", std::process::id()));
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(tmp.join("libsomethingelse.so"), b"").unwrap();
            assert_eq!(pick_onnxruntime_so(&tmp), None);
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}
