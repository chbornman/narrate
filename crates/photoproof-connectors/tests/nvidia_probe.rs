//! NVIDIA execution-provider availability probe (the margo / RTX 5080 path).
//!
//! The #1 unknown before wiring the CUDA/TensorRT EPs (PLAN-TENSORRT.md): the
//! prebuilt onnxruntime that `ort` links may NOT carry the CUDA/TensorRT EPs -
//! they are a build-time onnxruntime option (the same gotcha the CoreML spike
//! had to clear). This probe answers it: does the linked onnxruntime advertise
//! the CUDA and TensorRT EPs on this machine? Run ON the NVIDIA box:
//!
//!   cargo test -p photoproof-connectors --features nvidia --test nvidia_probe \
//!       -- --ignored --nocapture
//!
//! Gated behind the `cuda` feature (the NVIDIA floor); the TensorRT check adds
//! itself only under `tensorrt`. The default macOS / CPU builds never compile it.
#![cfg(feature = "cuda")]

fn report(name: &str, r: ort::Result<bool>) {
    match r {
        Ok(b) => println!("[nvidia-probe] {name} EP available: {b}"),
        Err(e) => println!("[nvidia-probe] {name} EP query error: {e}"),
    }
}

/// Does the linked onnxruntime carry the CUDA (+ TensorRT) EPs? Prints them;
/// asserts CUDA is present (the floor - TensorRT is the optimization on top).
#[test]
#[ignore = "nvidia probe; run on the RTX 5080 machine with --features cuda (or tensorrt) -- --ignored --nocapture"]
fn nvidia_provider_available() {
    use ort::ep::CUDA;
    use ort::ep::ExecutionProvider;

    println!("[nvidia-probe] target_os = {}", std::env::consts::OS);
    report("CUDA", CUDA::default().is_available());

    #[cfg(feature = "tensorrt")]
    {
        use ort::ep::TensorRT;
        report("TensorRT", TensorRT::default().is_available());
    }

    // CUDA is the floor; if even it is absent the linked onnxruntime lacks GPU
    // support and we need a load-dynamic + vendored CUDA-enabled libonnxruntime.
    assert!(
        matches!(CUDA::default().is_available(), Ok(true)),
        "the linked onnxruntime does NOT carry the CUDA EP - see docs/PLAN-TENSORRT.md"
    );
}
