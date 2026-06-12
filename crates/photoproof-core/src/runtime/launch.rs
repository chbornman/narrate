//! Child launch recipes — the §3.1/§3.2 command lines as data, encoding
//! every P6.3 spike finding that is LOAD-BEARING for correctness
//! (docs/SPIKE-P6.3.md). These are pure argv builders feeding
//! [`super::process::SpawnSpec`]; the `{port}` placeholder is substituted
//! by the spawner per attempt (§8.2).
//!
//! Spike-mandated flags, each pinned by a test below so a refactor cannot
//! silently drop them:
//! - llama-server: `--reasoning-budget 0` — Gemma 4 E2B/E4B are reasoning
//!   models; without it every constrained-output token goes to
//!   `reasoning_content` and the filter parse yields EMPTY content
//!   (probe went 0/50 → 50/50 on this one flag).
//! - ASR: ONNX intra-op threads ≥ 4 — the sherpa default of 1 decodes
//!   SLOWER than real time (lag grew ~1.1 s per audio second).

use std::path::Path;

/// llama.cpp `-ngl` convention: any value above the model's layer count
/// means "offload every layer", and 99 is the conventional sentinel
/// (greater than any shipped model's depth). It is NOT a real layer
/// count — do not "correct" it to one.
pub const NGL_OFFLOAD_ALL: u32 = 99;

/// §3.1 — `llama-server` argv. `mmproj`: the vision projector when the
/// model entry ships one (captions); `gpu_layers`: `None` = offload all
/// (`-ngl 99`, the Metal/CUDA default posture).
pub fn llama_server_args(
    model: &Path,
    mmproj: Option<&Path>,
    ctx_size: u32,
    parallel_slots: u32,
    gpu_layers: Option<u32>,
) -> Vec<String> {
    let mut args = vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        "{port}".into(),
        "--model".into(),
        model.display().to_string(),
        "--ctx-size".into(),
        ctx_size.to_string(),
        "--parallel".into(),
        parallel_slots.to_string(),
        "-ngl".into(),
        gpu_layers.unwrap_or(NGL_OFFLOAD_ALL).to_string(),
        // SPIKE-MANDATED (do not remove): see module docs.
        "--reasoning-budget".into(),
        "0".into(),
    ];
    if let Some(p) = mmproj {
        args.push("--mmproj".into());
        args.push(p.display().to_string());
    }
    args
}

/// Minimum ONNX intra-op threads for real-time streaming decode (spike:
/// 1 falls behind; 4 streams at p95 ~650 ms partial lag on the Tier-1
/// floor at ~2.5 cores).
pub const ASR_MIN_THREADS: u32 = 4;

/// §3.2 — the owned ASR wrapper child (B67). The wrapper binary speaks
/// the same wire contract as the reference server (raw f32 frames in,
/// result JSON out, "Done" sentinel) but mints finals from the LAST
/// DECODED STATE — the vendored server drops text its own partials
/// already carried, which is disqualifying when CAPTURE mints events
/// from finals. Model files are the four-file transducer layout exactly
/// as the manifest pins them.
pub fn asr_wrapper_args(model_dir: &Path, chunk_ms: u32, threads: u32) -> Vec<String> {
    vec![
        "--port".into(),
        "{port}".into(),
        "--encoder".into(),
        model_dir.join("encoder.int8.onnx").display().to_string(),
        "--decoder".into(),
        model_dir.join("decoder.int8.onnx").display().to_string(),
        "--joiner".into(),
        model_dir.join("joiner.int8.onnx").display().to_string(),
        "--tokens".into(),
        model_dir.join("tokens.txt").display().to_string(),
        "--chunk-ms".into(),
        chunk_ms.to_string(),
        "--num-threads".into(),
        threads.max(ASR_MIN_THREADS).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The spike's 0/50→50/50 flag can never be dropped silently.
    #[test]
    fn llama_args_pin_the_reasoning_budget_off() {
        let args = llama_server_args(Path::new("/m/model.gguf"), None, 16384, 2, None);
        let joined = args.join(" ");
        assert!(
            joined.contains("--reasoning-budget 0"),
            "SPIKE-MANDATED flag missing: {joined}"
        );
        assert!(joined.contains("--port {port}"), "port placeholder");
        assert!(joined.contains("-ngl 99"), "offload-all default");
        assert!(!joined.contains("--mmproj"), "no projector unless shipped");
    }

    #[test]
    fn llama_args_carry_the_projector_when_present() {
        let args = llama_server_args(
            Path::new("/m/model.gguf"),
            Some(Path::new("/m/mmproj.gguf")),
            8192,
            1,
            Some(0),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--mmproj /m/mmproj.gguf"));
        assert!(joined.contains("--ctx-size 8192"));
        assert!(joined.contains("-ngl 0"), "explicit CPU-only respected");
    }

    /// The sherpa default of 1 thread decodes slower than real time —
    /// the builder refuses to go below the spike floor.
    #[test]
    fn asr_args_never_go_below_the_realtime_thread_floor() {
        let args = asr_wrapper_args(&PathBuf::from("/models/asr"), 160, 1);
        let joined = args.join(" ");
        assert!(
            joined.contains("--num-threads 4"),
            "thread floor not enforced: {joined}"
        );
        assert!(joined.contains("--chunk-ms 160"));
        assert!(joined.contains("/models/asr/encoder.int8.onnx"));
        assert!(joined.contains("/models/asr/tokens.txt"));
    }
}
