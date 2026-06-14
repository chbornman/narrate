//! Photoproof desktop shell. Contract: spec/UI.md, spec/CAPTURE.md §3–4.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // onnxruntime runtime-resolution FIRST, before any thread spawns: on a
    // `cuda-dynamic` (NVIDIA) build this finds the hardware-matched cuda13
    // libonnxruntime.so (sm_120) and exports ORT_DYLIB_PATH + LD_LIBRARY_PATH so
    // the in-process ort embedder dlopen's it when it builds its first session
    // (docs/PLAN-NVIDIA-LAUNCH.md). A no-op on the macOS/CPU builds — they keep
    // ort's bundled binary + CoreML/CPU. Must precede `set_var` for the WebKit
    // workaround and `run()` for the same single-threaded soundness reason.
    photoproof_desktop::ort_runtime::resolve();

    // WebKitGTK's DMABUF renderer crashes on NVIDIA + Wayland (Gdk protocol
    // error 71). Disable it there unless the user already decided; AMD/Intel
    // and X11 keep the fast path.
    // SAFETY: single-threaded — first statement of main, before GTK init.
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/driver/nvidia").exists()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    photoproof_desktop::run();
}
