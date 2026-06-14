//! pp-voice-bench — the headless e2e chunking harness (BACKLOG "Voice
//! chunking tuning"; founder request, June 2026): known recordings in,
//! the program's ACTUAL minted journal entries out. The whole production
//! pipeline runs for real — silero VAD in-process, the supervised
//! `pp-asr-server` wrapper child, the capture engine with its onset
//! binding and trailing-ship window — only the microphone is replaced by
//! a wav file and the wall clock by FakeClock.
//!
//! The VAD+ASR+engine wiring and the gated-vs-raw scoring now live in the
//! shared `photoproof_core::voice_bench` library module, so this bin and
//! `pp-sweep voice` drive the SAME pipeline (they MUST agree on what a config
//! scores). This bin is the thin CLI/printing wrapper plus the synth mode.
//!
//! Two modes:
//!
//! - `--synth out.wav --speech clip.wav --gaps 0.8,1.5,3.0`
//!   Build a test recording: N+1 copies of the speech clip separated by
//!   the given silence gaps, plus `out.wav.expected.txt` (one line per
//!   expected utterance) — segmentation truth by construction.
//!
//! - `--wav file.wav [--model-dir DIR] [--server PATH]
//!    [--rule1 S --rule2 S --rule3 S] [--enter P --exit P --hang N]
//!    [--json] [--expect FILE]`
//!   Feed the recording through the live pipeline and print every minted
//!   entry as `[start +dur] "text"`. `--json` emits machine-readable
//!   output for sweep scripts. Knobs map 1:1 to the BACKLOG item:
//!   rule1/rule2/rule3 = the server's endpoint rules; enter/exit/hang =
//!   silero hysteresis (hang in 32 ms windows).
//!
//!   `--expect FILE` (BACKLOG "Audiobook WER stress harness"): FILE is a
//!   reference transcript (Project Gutenberg chapter text). The bench
//!   scores word error rate two ways against it — the GATED feed (the
//!   --enter/--exit pass above, the production path) and a RAW feed (a
//!   second pass with the VAD gate forced open, the model-accuracy
//!   ceiling) — and prints both WERs plus the gating cost (their gap).
//!   Without --expect the run is unchanged (single pass, no scoring), so
//!   the sweep flows are untouched. The WER algorithm lives in
//!   `photoproof_core::voice_wer` (unit-tested there). To score Alice
//!   ch1 on the founder machine (the model + corpus are local, gitignored):
//!
//!   ```text
//!   cargo run -p photoproof-core --bin pp-voice-bench --release -- \
//!     --wav    "$PP_VOICE_CORPUS/alice_ch1_16k.wav" \
//!     --expect "$PP_VOICE_CORPUS/alice_ch1_gutenberg.txt" --json
//!   ```
//!
//!   where PP_VOICE_CORPUS points at test-corpora/voice-long (no path is
//!   hardcoded; the README in that dir names the exact wav/transcript
//!   files). This run needs the real ASR model, so it is NOT a CI test.
//!
//! Audio never persists (kernel K10 applies to the bench too): frames go
//! wav → ring → VAD → wire, all in memory; the journal db is a tempdir.

use std::io::Write;
use std::path::{Path, PathBuf};

use photoproof_core::voice_bench::{
    self, SR, Seg, VoiceKnobs, default_model_dir, default_server, read_wav, write_wav,
};

struct Args {
    // synth mode
    synth: Option<PathBuf>,
    speech: Option<PathBuf>,
    gaps: Vec<f64>,
    // run mode
    wav: Option<PathBuf>,
    model_dir: Option<PathBuf>,
    server: Option<PathBuf>,
    rule1: Option<f32>,
    rule2: Option<f32>,
    rule3: Option<f32>,
    endpoint_grace_ms: Option<u64>,
    enter: f32,
    exit: f32,
    hang: u32,
    json: bool,
    expect: Option<PathBuf>,
}

fn parse_args() -> Args {
    // Silero hysteresis defaults come from the shipped `[voice]` tuning (which
    // equals silero's own consts absent an override) so the bin's baseline
    // matches the app's; explicit flags override.
    let dflt = VoiceKnobs::default();
    let mut a = Args {
        synth: None,
        speech: None,
        gaps: Vec::new(),
        wav: None,
        model_dir: None,
        server: None,
        rule1: None,
        rule2: None,
        rule3: None,
        endpoint_grace_ms: None,
        enter: dflt.enter,
        exit: dflt.exit,
        hang: dflt.hang,
        json: false,
        expect: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--synth" => a.synth = Some(val().into()),
            "--speech" => a.speech = Some(val().into()),
            "--gaps" => {
                a.gaps = val()
                    .split(',')
                    .map(|s| s.trim().parse().expect("--gaps: seconds"))
                    .collect();
            }
            "--wav" => a.wav = Some(val().into()),
            "--model-dir" => a.model_dir = Some(val().into()),
            "--server" => a.server = Some(val().into()),
            "--rule1" => a.rule1 = Some(val().parse().expect("--rule1")),
            "--rule2" => a.rule2 = Some(val().parse().expect("--rule2")),
            "--rule3" => a.rule3 = Some(val().parse().expect("--rule3")),
            "--endpoint-grace-ms" => {
                a.endpoint_grace_ms = Some(val().parse().expect("--endpoint-grace-ms"));
            }
            "--enter" => a.enter = val().parse().expect("--enter"),
            "--exit" => a.exit = val().parse().expect("--exit"),
            "--hang" => a.hang = val().parse().expect("--hang"),
            "--json" => a.json = true,
            "--expect" => a.expect = Some(val().into()),
            other => {
                eprintln!("pp-voice-bench: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

impl Args {
    /// The VOICE DIALS this run drives (the shared harness's knob bundle).
    fn knobs(&self) -> VoiceKnobs {
        VoiceKnobs {
            rule1: self.rule1,
            rule2: self.rule2,
            rule3: self.rule3,
            endpoint_grace_ms: self.endpoint_grace_ms,
            enter: self.enter,
            exit: self.exit,
            hang: self.hang,
            // The bin reads the global pre-roll dial (no per-run override flag).
            pre_roll_ms: None,
        }
    }
}

/// Synth mode: build N+1 copies of the speech clip separated by the gaps, plus a
/// `<out>.expected.txt` (segmentation truth by construction).
fn synth(out: &Path, speech: &Path, gaps: &[f64]) {
    let clip = read_wav(speech).unwrap_or_else(|e| {
        eprintln!("pp-voice-bench: read {}: {e}", speech.display());
        std::process::exit(1);
    });
    // Trim the clip's own leading/trailing silence (-35 dB energy gate, the
    // spike methodology) so the scripted gaps are the ONLY silence.
    let peak = clip.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let gate = peak * 10f32.powf(-35.0 / 20.0);
    let rms_over = |w: &[f32]| (w.iter().map(|v| v * v).sum::<f32>() / w.len() as f32).sqrt();
    let first = clip
        .windows(512)
        .position(|w| rms_over(w) > gate)
        .unwrap_or(0);
    let last = clip.len()
        - clip
            .windows(512)
            .rev()
            .position(|w| rms_over(w) > gate)
            .unwrap_or(0);
    let clip = &clip[first..last];

    let mut samples = Vec::new();
    let mut expected = Vec::new();
    for (i, gap) in gaps.iter().chain(std::iter::once(&0.0)).enumerate() {
        let start = samples.len() as f64 / f64::from(SR);
        samples.extend_from_slice(clip);
        expected.push(format!(
            "utterance {} at {:.2}s..{:.2}s",
            i + 1,
            start,
            samples.len() as f64 / f64::from(SR)
        ));
        samples.extend(vec![0.0f32; (gap * f64::from(SR)) as usize]);
    }
    write_wav(out, &samples).expect("write wav");
    let expect_path = format!("{}.expected.txt", out.display());
    std::fs::write(&expect_path, expected.join("\n") + "\n").expect("write expected");
    println!(
        "synth: {} utterances, {:.1}s total -> {} + {}",
        gaps.len() + 1,
        samples.len() as f64 / f64::from(SR),
        out.display(),
        expect_path
    );
}

fn run(a: &Args) {
    let wav = a.wav.as_ref().expect("--wav required (or --synth)");
    let samples = read_wav(wav).unwrap_or_else(|e| {
        eprintln!("pp-voice-bench: read {}: {e}", wav.display());
        std::process::exit(1);
    });
    let server = a.server.clone().unwrap_or_else(default_server);
    let model_dir = a.model_dir.clone().unwrap_or_else(default_model_dir);
    let knobs = a.knobs();
    let (child, addr) =
        voice_bench::spawn_server(&server, &model_dir, &knobs).unwrap_or_else(|e| {
            eprintln!("pp-voice-bench: {e}");
            std::process::exit(1);
        });
    // Kill-on-drop: a panic anywhere below must not leak the server.
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _reap = Reap(child);

    // The production (GATED) pass; the sole pass without --expect.
    let (segs, abandoned) = voice_bench::drive_pipeline(
        &samples,
        addr,
        knobs.enter,
        knobs.exit,
        knobs.hang,
        knobs.pre_roll_ms,
    );

    // --expect upgrade: score gated + raw against the reference (the shared
    // `score_run` drives the gated pass AGAIN under the hood — kept for shape
    // parity with the no-expect path, where only the gated pass runs).
    let scoring = a.expect.as_ref().map(|expect| {
        let reference = std::fs::read_to_string(expect).unwrap_or_else(|e| {
            eprintln!("pp-voice-bench: read --expect {}: {e}", expect.display());
            std::process::exit(1);
        });
        voice_bench::score_run(&samples, addr, &knobs, &reference)
    });

    if a.json {
        let wer_json = scoring.as_ref().map(|r| {
            let feed = |s: &photoproof_core::voice_wer::WerScore| {
                serde_json::json!({
                    "wer": s.wer,
                    "sub": s.sub, "del": s.del, "ins": s.ins,
                    "ref_words": s.ref_words, "hyp_words": s.hyp_words,
                    "hits": s.hits, "hit_rate": s.hit_rate(),
                })
            };
            serde_json::json!({
                "expect": a.expect.as_ref().map(|p| p.display().to_string()),
                "raw": feed(&r.raw),
                "gated": feed(&r.gated),
            })
        });
        let json = serde_json::json!({
            "wav": wav.display().to_string(),
            "params": {
                "rule1": a.rule1, "rule2": a.rule2, "rule3": a.rule3,
                "endpoint_grace_ms": a.endpoint_grace_ms,
                "enter": a.enter, "exit": a.exit, "hang": a.hang,
            },
            "segments": segs.iter().map(|s| serde_json::json!({
                "onset_ms": s.onset_ms, "dur_ms": s.dur_ms, "text": s.text,
            })).collect::<Vec<_>>(),
            "abandoned": abandoned,
            "wer": wer_json,
        });
        println!("{json}");
    } else {
        print_segments(&segs, abandoned);
        if let Some(r) = &scoring {
            let line = |label: &str, s: &photoproof_core::voice_wer::WerScore| {
                println!(
                    "  {label:5} WER {:.4}  (S {} D {} I {} N {}  hits {}  hit_rate {:.4})",
                    s.wer,
                    s.sub,
                    s.del,
                    s.ins,
                    s.ref_words,
                    s.hits,
                    s.hit_rate(),
                );
            };
            println!("WER vs {}:", a.expect.as_ref().expect("checked").display());
            line("RAW", &r.raw);
            line("GATED", &r.gated);
            // The headline: how much WER the pipeline's gating ADDS on top of
            // the model's own error (positive = truncation cost).
            println!("  gating cost (gated - raw WER): {:+.4}", r.gating_cost());
        }
    }
    let _ = std::io::stdout().flush();
}

fn print_segments(segs: &[Seg], abandoned: u64) {
    println!("{} entries minted ({} abandoned):", segs.len(), abandoned);
    for s in segs {
        println!(
            "  [{:7.2}s +{:5.2}s] {:?}",
            s.onset_ms as f64 / 1000.0,
            f64::from(s.dur_ms) / 1000.0,
            s.text
        );
    }
}

fn main() {
    let a = parse_args();
    if let Some(out) = &a.synth {
        let speech = a.speech.as_ref().expect("--synth needs --speech");
        assert!(!a.gaps.is_empty(), "--synth needs --gaps");
        synth(out, speech, &a.gaps);
        return;
    }
    run(&a);
}
