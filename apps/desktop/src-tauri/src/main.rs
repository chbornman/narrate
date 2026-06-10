//! Photoproof desktop shell. Contract: spec/UI.md, spec/CAPTURE.md §3–4.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
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
