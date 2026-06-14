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

/// MTP (multi-token prediction) speculative-decode launch inputs
/// (docs/PLAN-GEMMA-MTP.md). MTP is LOSSLESS — the target verifies every
/// drafted token, so output is byte-identical to plain decoding — and it
/// is a CUDA win but a Metal LOSS (ggml-org/llama.cpp #23752, closed:
/// 11-28% SLOWER on Apple Silicon, the draft-eval overhead exceeds the
/// gain). The supervisor therefore resolves this to `None` on macOS/Metal
/// and to `Some(..)` only when (a) the chosen model entry ships an `mtp-`
/// drafter file AND (b) the platform is non-Metal. When `None`, the argv is
/// EXACTLY the legacy path — no behavior change for the shipped E2B config.
#[derive(Debug, Clone)]
pub struct MtpDraft {
    /// Path to the `mtp-*.gguf` drafter (shares the target's KV cache).
    pub draft_model: std::path::PathBuf,
    /// `--spec-draft-n-max`: max drafted tokens per step (Unsloth card uses
    /// 4; 1-4 is the tested range). Higher helps high-acceptance CUDA runs,
    /// hurts low-acceptance ones.
    pub n_max: u32,
}

/// §3.1 — `llama-server` argv. `mmproj`: the vision projector when the
/// model entry ships one (captions); `gpu_layers`: `None` = offload all
/// (`-ngl 99`, the Metal/CUDA default posture). Back-compat wrapper around
/// [`llama_server_args_mtp`] with MTP disabled — every existing caller and
/// the shipped E2B config flow through here, byte-for-byte unchanged.
pub fn llama_server_args(
    model: &Path,
    mmproj: Option<&Path>,
    ctx_size: u32,
    parallel_slots: u32,
    gpu_layers: Option<u32>,
) -> Vec<String> {
    llama_server_args_mtp(model, mmproj, ctx_size, parallel_slots, gpu_layers, None)
}

/// §3.1 + MTP — `llama-server` argv with an OPTIONAL multi-token-prediction
/// drafter. When `mtp` is `Some`, appends the mainline llama.cpp MTP flags
/// (`--spec-type draft-mtp`, `--model-draft <mtp.gguf>`, `--spec-draft-n-max
/// <n>`); requires a vendored binary built after 2026-06-08 (#24282 added
/// the `gemma4-assistant` drafter arch + E2B/E4B support). When `None`, the
/// argv is IDENTICAL to the legacy path. The Metal-vs-CUDA gate lives in the
/// supervisor (which resolves `mtp` to `None` on Apple Silicon, #23752), so
/// this builder stays a pure, platform-agnostic argv mapping.
pub fn llama_server_args_mtp(
    model: &Path,
    mmproj: Option<&Path>,
    ctx_size: u32,
    parallel_slots: u32,
    gpu_layers: Option<u32>,
    mtp: Option<&MtpDraft>,
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
    // MTP draft decoding — lossless, gated to non-Metal by the supervisor.
    if let Some(m) = mtp {
        args.push("--spec-type".into());
        args.push("draft-mtp".into());
        args.push("--model-draft".into());
        args.push(m.draft_model.display().to_string());
        args.push("--spec-draft-n-max".into());
        args.push(m.n_max.to_string());
    }
    args
}

/// Minimum ONNX intra-op threads for real-time streaming decode (spike:
/// 1 falls behind; 4 streams at p95 ~650 ms partial lag on the Tier-1
/// floor at ~2.5 cores).
pub const ASR_MIN_THREADS: u32 = 4;

/// Default per-stream ASR language passed to the wrapper child
/// (docs/PLAN-NEMOTRON-35-SIDECAR.md). WHY a constant and not an
/// `Option`: the Nemotron 3.5 multilingual export expects a per-stream
/// language string (`en` / `ja` / `auto`) and the SPIKE hit a
/// `None-language` coin-flip class upstream, so the value must NEVER be
/// empty. English is the product today (SPIKE forced en-US), so `en` is
/// the floor. The CURRENT sherpa-onnx-crate child accepts-and-ignores
/// `--lang` (same posture as `--chunk-ms`), so passing it is a no-op
/// until a 3.5 engine that reads it ships - the live ASR path is
/// unchanged.
pub const ASR_DEFAULT_LANG: &str = "en";

/// §3.2 — the owned ASR wrapper child (B67). The wrapper binary speaks
/// the same wire contract as the reference server (raw f32 frames in,
/// result JSON out, "Done" sentinel) but mints finals from the LAST
/// DECODED STATE — the vendored server drops text its own partials
/// already carried, which is disqualifying when CAPTURE mints events
/// from finals. Model files are the four-file transducer layout exactly
/// as the manifest pins them — those entries are flat (path == basename),
/// so these basenames match the path-preserving on-disk layout that
/// download.rs writes under models_dir/<id>/<file.path>.
///
/// `--lang` carries the per-stream language for the staged Nemotron 3.5
/// export (PLAN-NEMOTRON-35-SIDECAR §4); it defaults to [`ASR_DEFAULT_LANG`]
/// and is inert for the current English model (the sherpa child ignores
/// it), so it never changes today's behavior.
pub fn asr_wrapper_args(model_dir: &Path, chunk_ms: u32, threads: u32) -> Vec<String> {
    // The endpoint rules are VOICE DIALS (DESIGN-TUNING-LOOP.md): they live in
    // `tuning().voice` so a committed `[voice]` config actually changes the
    // child's endpointing at launch, and pp-sweep voice can propose a winner.
    // The server keeps its own const defaults as a fallback (when no flags are
    // passed); we ALWAYS pass them so the live config is what runs. `tuning()`
    // returns the shipped defaults pre-init (tests, no tuning.toml), which equal
    // the server's own consts — so passing them is a no-op until overridden.
    let voice = crate::tuning::tuning().voice;
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
        // CAPTURE §6.3 endpoint rules, as f32 seconds (the server parses f32).
        "--rule1".into(),
        (voice.rule1 as f32).to_string(),
        "--rule2".into(),
        (voice.rule2 as f32).to_string(),
        "--rule3".into(),
        (voice.rule3 as f32).to_string(),
        // Per-stream language for the staged Nemotron 3.5 export
        // (PLAN-NEMOTRON-35-SIDECAR §4): never empty (the SPIKE's
        // None-language class), inert for the current English child.
        "--lang".into(),
        ASR_DEFAULT_LANG.to_string(),
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

    /// MTP `None` is byte-for-byte the legacy argv — the shipped E2B
    /// (and every existing caller) is provably unaffected by the new path.
    #[test]
    fn mtp_none_is_identical_to_the_legacy_argv() {
        let legacy = llama_server_args(Path::new("/m/model.gguf"), None, 16384, 2, None);
        let via_mtp = llama_server_args_mtp(Path::new("/m/model.gguf"), None, 16384, 2, None, None);
        assert_eq!(legacy, via_mtp, "no-MTP path must not drift");
        assert!(
            !via_mtp.join(" ").contains("draft-mtp"),
            "no MTP flags when disabled"
        );
    }

    /// When the supervisor passes a drafter (non-Metal, model ships `mtp-`),
    /// the mainline MTP flags land in order; the reasoning-budget + mmproj
    /// invariants survive alongside them.
    #[test]
    fn mtp_some_appends_the_draft_mtp_flags() {
        let draft = MtpDraft {
            draft_model: PathBuf::from("/m/mtp-gemma-4-E2B-it.gguf"),
            n_max: 4,
        };
        let args = llama_server_args_mtp(
            Path::new("/m/model.gguf"),
            Some(Path::new("/m/mmproj.gguf")),
            16384,
            2,
            None,
            Some(&draft),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--spec-type draft-mtp"), "{joined}");
        assert!(
            joined.contains("--model-draft /m/mtp-gemma-4-E2B-it.gguf"),
            "{joined}"
        );
        assert!(joined.contains("--spec-draft-n-max 4"), "{joined}");
        // The load-bearing existing flags are still present.
        assert!(joined.contains("--reasoning-budget 0"), "{joined}");
        assert!(joined.contains("--mmproj /m/mmproj.gguf"), "{joined}");
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

    /// The endpoint rules are now READ from `tuning().voice` and passed to the
    /// child, so a committed `[voice]` config changes the live endpointing.
    /// Absent a `tuning.toml` (the test case), `tuning()` yields the shipped
    /// defaults, which equal `pp-asr-server`'s own const fallbacks — so the
    /// args carry rule1=2.4 / rule2=1.2 / rule3=20 and nothing changes by
    /// construction (the behavior-unchanged contract).
    #[test]
    fn asr_args_carry_the_voice_endpoint_rules_from_tuning() {
        let args = asr_wrapper_args(&PathBuf::from("/models/asr"), 160, 4);
        let joined = args.join(" ");
        assert!(
            joined.contains("--rule1 2.4"),
            "rule1 from tuning: {joined}"
        );
        assert!(
            joined.contains("--rule2 1.2"),
            "rule2 from tuning: {joined}"
        );
        assert!(joined.contains("--rule3 20"), "rule3 from tuning: {joined}");
    }

    /// The per-stream language flag is ALWAYS passed and defaults to `en`
    /// (PLAN-NEMOTRON-35-SIDECAR §4/§6): never empty (the SPIKE's
    /// None-language class) and inert for the current English child, so
    /// this is a no-op today but in place for the 3.5 engine swap.
    #[test]
    fn asr_args_pin_the_per_stream_language_default() {
        let args = asr_wrapper_args(&PathBuf::from("/models/asr"), 560, 4);
        let joined = args.join(" ");
        assert!(joined.contains("--lang en"), "default lang: {joined}");
        assert_eq!(ASR_DEFAULT_LANG, "en", "english is the product floor");
    }
}
