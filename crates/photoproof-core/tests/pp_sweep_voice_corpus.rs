//! pp-sweep voice CORPUS-MANIFEST integration — proves the multi-file sweep
//! runs end to end: stage a tiny MANY-recording corpus (the same TSV format
//! `scripts/fetch-voice-corpus.sh` writes), invoke the real `pp-sweep voice
//! --corpus-manifest` binary, and confirm it scores every (wav, transcript)
//! pair through the shared `voice_bench` wiring and emits a token-weighted
//! corpus-level leaderboard.
//!
//! `#[ignore]` by default (like `voice_bench_synth`): it needs the real ASR
//! model snapshot and the `pp-asr-server` binary built beside the test (both
//! gitignored / founder-machine only). It SKIPS CLEANLY (early return, no
//! failure) when the model dir or the server binary is absent, so an
//! `--ignored` run stays green on other machines. Run with:
//!
//!   cargo build -p pp-asr-server --release   # the child the sweep spawns
//!   PP_VOICE_SPEECH=<a 16k mono wav with speech> \
//!   cargo test -p photoproof-core --release --test pp_sweep_voice_corpus \
//!     -- --ignored --nocapture
//!
//! The DETERMINISTIC parsing + token-weighted aggregation is unit-tested in
//! `src/bin/pp_sweep.rs` (no models); this test only proves the real multi-file
//! model run wires together.

use std::path::PathBuf;
use std::process::Command;

use photoproof_core::voice_bench::{self, SR};

/// Trim a clip's lead/trail silence (so the only silence is the gaps we add).
fn trim(clip: &[f32]) -> &[f32] {
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
    &clip[first..last]
}

#[test]
#[ignore = "needs the real ASR model + pp-asr-server binary; run with --ignored on the founder machine"]
fn pp_sweep_voice_scores_a_corpus_manifest() {
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
        Ok(c) => trim(&c).to_vec(),
        Err(e) => {
            eprintln!("skip: read PP_VOICE_SPEECH {}: {e}", speech.display());
            return;
        }
    };

    // Stage a TWO-recording corpus (two "chapters") in a tempdir + a manifest TSV
    // in exactly the format scripts/fetch-voice-corpus.sh writes. Chapter B is
    // longer (two utterances) than chapter A (one), so the token-weighting is
    // actually exercised (B's reference dominates the corpus WER).
    let dir = std::env::temp_dir().join(format!("pp-sweep-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Chapter A: one utterance.
    let wav_a = dir.join("chA.wav");
    voice_bench::write_wav(&wav_a, &clip).unwrap();
    let txt_a = dir.join("chA.txt");
    std::fs::write(&txt_a, "the quick brown fox\n").unwrap();

    // Chapter B: two utterances separated by a 1.5 s gap.
    let mut samples_b = clip.clone();
    samples_b.extend(vec![0.0f32; (1.5 * f64::from(SR)) as usize]);
    samples_b.extend_from_slice(&clip);
    let wav_b = dir.join("chB.wav");
    voice_bench::write_wav(&wav_b, &samples_b).unwrap();
    let txt_b = dir.join("chB.txt");
    std::fs::write(&txt_b, "the quick brown fox\nthe quick brown fox\n").unwrap();

    let manifest = dir.join("split-manifest.tsv");
    std::fs::write(
        &manifest,
        format!(
            "# id\twav\ttranscript\tutterances\n\
             chA\t{}\t{}\t1\n\
             chB\t{}\t{}\t2\n",
            wav_a.display(),
            txt_a.display(),
            wav_b.display(),
            txt_b.display(),
        ),
    )
    .unwrap();

    // Invoke the real pp-sweep binary over the manifest, two configs, JSON out.
    let exe = env!("CARGO_BIN_EXE_pp-sweep");
    let out = Command::new(exe)
        .args([
            "voice",
            "--corpus-manifest",
            manifest.to_str().unwrap(),
            "--grid",
            "rule2=0.8",
            "--json",
        ])
        .output()
        .expect("run pp-sweep");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "pp-sweep voice failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse JSON: {e}\n{stdout}"));
    let results = doc["results"].as_array().expect("results array");
    // baseline + cfg0 = 2 rows; each row must report the 2-file corpus and a
    // finite, token-weighted corpus WER plus the per-file breakdown.
    assert_eq!(results.len(), 2, "baseline + one config");
    for r in results {
        assert_eq!(r["files"].as_u64(), Some(2), "corpus is two files");
        assert!(r["gated_wer"].as_f64().unwrap().is_finite());
        assert!(r["raw_wer"].as_f64().unwrap().is_finite());
        assert!(r["gating_cost"].as_f64().unwrap().is_finite());
        let per_file = r["per_file"].as_array().expect("per_file array");
        assert_eq!(per_file.len(), 2, "per-file breakdown has both chapters");
        // total_utterances rolls up the manifest counts (1 + 2 = 3).
        assert_eq!(r["total_utterances"].as_u64(), Some(3));
    }
    eprintln!("pp-sweep voice corpus run OK:\n{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}
