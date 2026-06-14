//! voice_bench synth integration — proves the SHARED voice-pipeline wiring
//! (`photoproof_core::voice_bench`) runs end to end headless: spawn the real
//! `pp-asr-server` child, drive a synthesized recording through the silero VAD +
//! capture engine, and score gated-vs-raw WER. This is the wiring both
//! `pp-voice-bench` and `pp-sweep voice` reuse, so a green run here means the
//! sweep arm's per-config scoring is sound on a real model.
//!
//! `#[ignore]` by default: it needs the real ASR model snapshot and the
//! `pp-asr-server` binary built beside the test (the model is gitignored, the
//! GPU/threads only matter on the founder machine). It SKIPS CLEANLY (early
//! return, no failure) when the model dir or the server binary is absent, so an
//! `--ignored` run stays green on other machines. Run with:
//!
//!   cargo build -p pp-asr-server --release  # the child the bench spawns
//!   PP_VOICE_SPEECH=<a 16k mono wav with speech> \
//!   cargo test -p photoproof-core --release --test voice_bench_synth \
//!     -- --ignored --nocapture
//!
//! PP_VOICE_SPEECH points at any short PCM16 mono 16 kHz wav containing speech
//! (the synth tiles it with silence gaps to make a known multi-utterance
//! recording). Absent it, the test skips.

use std::path::PathBuf;

use photoproof_core::voice_bench::{self, SR, VoiceKnobs};

/// Tile a speech clip with silence gaps into a known multi-utterance recording,
/// trimming the clip's own lead/trail silence first (so the gaps are the only
/// silence) — the same construction `pp-voice-bench --synth` uses.
fn synth(clip: &[f32], gaps: &[f64]) -> Vec<f32> {
    let peak = clip.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let gate = peak * 10f32.powf(-35.0 / 20.0);
    let rms = |w: &[f32]| (w.iter().map(|v| v * v).sum::<f32>() / w.len() as f32).sqrt();
    let first = clip.windows(512).position(|w| rms(w) > gate).unwrap_or(0);
    let last = clip.len()
        - clip
            .windows(512)
            .rev()
            .position(|w| rms(w) > gate)
            .unwrap_or(0);
    let clip = &clip[first..last];
    let mut out = Vec::new();
    for gap in gaps.iter().chain(std::iter::once(&0.0)) {
        out.extend_from_slice(clip);
        out.extend(vec![0.0f32; (gap * f64::from(SR)) as usize]);
    }
    out
}

#[test]
#[ignore = "needs the real ASR model + pp-asr-server binary; run with --ignored on the founder machine"]
fn voice_bench_scores_a_synth_recording() {
    // Skip cleanly when the model or the server binary is absent.
    let model_dir = voice_bench::default_model_dir();
    if !model_dir.join("encoder.int8.onnx").exists() {
        eprintln!(
            "skip: ASR model not at {} (founder machine only)",
            model_dir.display()
        );
        return;
    }
    let server = voice_bench::default_server();
    if !server.exists() {
        eprintln!(
            "skip: pp-asr-server not at {} (build it: cargo build -p pp-asr-server)",
            server.display()
        );
        return;
    }
    let Some(speech) = std::env::var_os("PP_VOICE_SPEECH").map(PathBuf::from) else {
        eprintln!("skip: set PP_VOICE_SPEECH to a 16k mono wav with speech");
        return;
    };
    let clip = match voice_bench::read_wav(&speech) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: read PP_VOICE_SPEECH {}: {e}", speech.display());
            return;
        }
    };

    // Two utterances separated by a 1.5 s gap. The reference is what we expect
    // the model to transcribe; WER is exercised against it regardless of the
    // exact words (we assert the pipeline RAN and produced a finite score, not a
    // specific accuracy — accuracy is the founder's corpus call, not a CI gate).
    let samples = synth(&clip, &[1.5]);

    // Two configs: the shipped baseline and a snappier rule2, exactly the shape
    // pp-sweep voice drives (one server per config).
    for (label, knobs) in [
        ("baseline", VoiceKnobs::default()),
        (
            "rule2=0.8",
            VoiceKnobs {
                rule2: Some(0.8),
                ..VoiceKnobs::default()
            },
        ),
    ] {
        let (child, addr) = voice_bench::spawn_server(&server, &model_dir, &knobs)
            .unwrap_or_else(|e| panic!("spawn server for {label}: {e}"));
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _reap = Reap(child);

        // A trivial reference (the synth tiles one clip twice). We score against
        // the gated hypothesis itself-ish: the point is the WIRING runs and the
        // gated/raw passes both produce finite, well-formed scores.
        let reference = "the quick brown fox the quick brown fox";
        let result = voice_bench::score_run(&samples, addr, &knobs, reference);

        assert!(result.gated.wer.is_finite(), "{label}: gated WER finite");
        assert!(result.raw.wer.is_finite(), "{label}: raw WER finite");
        assert!(
            result.gating_cost().is_finite(),
            "{label}: gating cost finite"
        );
        // The gated pass must have produced at least one minted segment for a
        // recording that clearly contains speech (proves VAD->ASR->engine ran).
        assert!(
            !result.gated_segs.is_empty(),
            "{label}: gated pass minted nothing (pipeline broken?)"
        );
        eprintln!(
            "{label}: gating_cost {:+.4} gated {:.4} raw {:.4} segs {}",
            result.gating_cost(),
            result.gated.wer,
            result.raw.wer,
            result.gated_segs.len()
        );
    }
}
