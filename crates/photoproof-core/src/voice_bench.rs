//! voice_bench — the SHARED voice-pipeline harness (DESIGN-TUNING-LOOP.md
//! "voice" arm). The live production pipeline run headless against a recording:
//! silero VAD in-process, the supervised `pp-asr-server` wrapper child, and the
//! capture engine with its onset binding + trailing-ship window — only the
//! microphone is a wav file and the wall clock a `FakeClock`.
//!
//! WHY a library module (refactored out of the `pp-voice-bench` bin): two
//! callers now drive the SAME wiring — the bin (`pp-voice-bench`, a founder
//! diagnostic) and the sweep arm (`pp-sweep voice`, which runs this once per
//! grid config). Duplicating the VAD+ASR+engine wiring across both would let
//! them drift; the bin and the sweep MUST agree on what a config scores. So the
//! pipeline, the WAV io, and the gated-vs-raw scoring live here, unit-tested for
//! the pure parts (WAV round-trip, hypothesis join); the model-dependent run is
//! exercised by the bin and the sweep's env-guarded integration test.
//!
//! Audio never persists (kernel K10 applies to the bench too): frames go
//! wav -> ring -> VAD -> wire, all in memory; the journal db is a tempdir.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use std::io::{BufRead, BufReader};

use photoproof_connectors::silero::SileroVad;
use photoproof_connectors::transcriber::AudioFrame;
use photoproof_connectors::{EndpointCell, SherpaOnlineTranscriber};

use crate::capture::{CaptureEngine, FakeClock};
use crate::voice_wer::{self, WerScore};
use crate::{ContentHash, Event, EventStore, Payload, SessionContext};

/// Sample rate of every fixture (the silero/Nemotron wire contract: 16 kHz mono).
pub const SR: u32 = 16_000;
/// Capture-frame length fed to the engine (the connector's 50 ms cadence).
pub const FRAME_MS: u64 = 50;

/// The VOICE DIALS one bench run uses. `rule1/2/3` are `Option` so a caller can
/// pass NONE to let the server fall back to its own const defaults (the bin's
/// back-compat behavior); the sweep always sets them from the grid/baseline.
/// `enter`/`exit`/`hang` are the silero hysteresis; `endpoint_grace_ms` is the
/// server's tail-token grace (kept for sweep symmetry, default off).
#[derive(Debug, Clone, Copy)]
pub struct VoiceKnobs {
    pub rule1: Option<f32>,
    pub rule2: Option<f32>,
    pub rule3: Option<f32>,
    pub endpoint_grace_ms: Option<u64>,
    pub enter: f32,
    pub exit: f32,
    pub hang: u32,
    /// Per-run pre-roll cap override (ms). `None` lets the capture engine read
    /// the `tuning().voice.pre_roll_ms` global; `pp-sweep voice` sets it per
    /// config (the global is install-once, so a per-config pre-roll routes
    /// through this explicit override instead).
    pub pre_roll_ms: Option<u64>,
}

impl Default for VoiceKnobs {
    /// The shipped voice defaults (the `[voice]` tuning defaults): rules unset
    /// (server falls back to its own equal consts), silero 0.5 / 0.35 / 15,
    /// pre-roll unset (the engine reads the global dial).
    fn default() -> Self {
        let v = crate::tuning::tuning().voice;
        Self {
            rule1: None,
            rule2: None,
            rule3: None,
            endpoint_grace_ms: None,
            enter: v.vad_enter as f32,
            exit: v.vad_exit as f32,
            hang: v.vad_hang,
            pre_roll_ms: None,
        }
    }
}

/// One minted journal entry, flattened for scoring/printing.
#[derive(Debug, Clone)]
pub struct Seg {
    pub onset_ms: i64,
    pub dur_ms: u32,
    pub text: String,
}

fn seg_of(e: &Event, wall0: i64) -> Seg {
    let dur_ms = match &e.payload {
        Some(Payload::Voice { dur_ms, .. }) => *dur_ms,
        _ => 0,
    };
    Seg {
        onset_ms: e.ts.epoch_ms() - wall0,
        dur_ms,
        text: e.text.clone().unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Minimal RIFF/PCM16 mono 16 kHz wav io (the silero fixture format).
// ---------------------------------------------------------------------------

/// Read a PCM16 mono 16 kHz wav into normalized f32 samples. Panics with a
/// convert hint on a wrong format (the harness only ever reads its own fixtures
/// or `ffmpeg`-normalized corpus files).
pub fn read_wav(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    assert!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a wav: {}",
        path.display()
    );
    let mut at = 12usize;
    let mut data: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
        if id == b"fmt " {
            let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
            assert!(
                channels == 1 && rate == SR && bits == 16,
                "need PCM16 mono 16 kHz, got {channels}ch {rate}Hz {bits}bit \
                 (convert: ffmpeg -i in -ar 16000 -ac 1 -sample_fmt s16 out.wav)"
            );
        } else if id == b"data" {
            data = Some(body);
        }
        at += 8 + len + (len & 1);
    }
    Ok(data
        .expect("wav has no data chunk")
        .chunks_exact(2)
        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
        .collect())
}

/// Write normalized f32 samples as a PCM16 mono 16 kHz wav.
pub fn write_wav(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SR.to_le_bytes());
    out.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out)
}

// ---------------------------------------------------------------------------
// Server spawn (the supervisor's recipe, ephemeral port + READY handshake).
// ---------------------------------------------------------------------------

/// The default ASR model dir (founder machine layout; gitignored corpus/model).
pub fn default_model_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home).join(
        "Library/Application Support/com.photoproof.desktop/models/\
         nemotron-speech-streaming-en-0.6b-560ms-int8",
    )
}

/// The `pp-asr-server` binary beside the current exe (workspace bins land
/// side by side in target/{profile}/).
pub fn default_server() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent().expect("exe dir").join("pp-asr-server")
}

/// Spawn the wrapper child exactly as the supervisor does (ephemeral port,
/// READY handshake on stdout) with the knobs' endpoint rules, returning the
/// child + its bound address. `Err` carries a human message (no `process::exit`
/// here — the lib never kills the caller; the bin/sweep decide).
pub fn spawn_server(
    server: &Path,
    model_dir: &Path,
    knobs: &VoiceKnobs,
) -> Result<(Child, SocketAddr), String> {
    let mut args = vec![
        "--port".to_string(),
        "0".into(),
        "--encoder".into(),
        model_dir.join("encoder.int8.onnx").display().to_string(),
        "--decoder".into(),
        model_dir.join("decoder.int8.onnx").display().to_string(),
        "--joiner".into(),
        model_dir.join("joiner.int8.onnx").display().to_string(),
        "--tokens".into(),
        model_dir.join("tokens.txt").display().to_string(),
        "--num-threads".into(),
        "4".into(),
    ];
    for (flag, v) in [
        ("--rule1", knobs.rule1),
        ("--rule2", knobs.rule2),
        ("--rule3", knobs.rule3),
    ] {
        if let Some(v) = v {
            args.push(flag.into());
            args.push(v.to_string());
        }
    }
    if let Some(g) = knobs.endpoint_grace_ms {
        args.push("--endpoint-grace-ms".into());
        args.push(g.to_string());
    }
    let mut child = Command::new(server)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", server.display()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    // Model load takes a few seconds; READY is the §8.1 handshake.
    for line in &mut lines {
        let line = line.map_err(|e| format!("server stdout: {e}"))?;
        if let Some(port) = line.strip_prefix("READY port=") {
            let port: u16 = port
                .trim()
                .parse()
                .map_err(|e| format!("READY port parse: {e}"))?;
            return Ok((child, SocketAddr::from(([127, 0, 0, 1], port))));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("server exited before READY (model files present?)".into())
}

// ---------------------------------------------------------------------------
// The pipeline drive + scoring.
// ---------------------------------------------------------------------------

/// Drive the live pipeline once over `samples` with the given silero hysteresis,
/// returning the minted segments and the abandoned count. The fake clock IS the
/// stream clock, so the run is deterministic in audio time regardless of decode
/// speed (endpoint decisions live in received-audio time on the server).
///
/// Factored so `--expect`/the sweep can drive it TWICE against the SAME
/// recording — once GATED (the caller's VAD params, the production path that can
/// truncate) and once RAW (gate forced open) — and score both feeds against one
/// transcript. That raw-vs-gated WER gap is the founder's headline: it splits
/// MODEL error from PIPELINE truncation.
pub fn drive_pipeline(
    samples: &[f32],
    addr: SocketAddr,
    enter: f32,
    exit: f32,
    hang: u32,
    pre_roll_ms: Option<u64>,
) -> (Vec<Seg>, u64) {
    // Scratch journal db, unique per pass so a second drive in the same process
    // never reopens the first pass's store.
    let dir = std::env::temp_dir().join(format!(
        "pp-voice-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let store = EventStore::open(dir.join("bench.db")).expect("open store");
    let session = store
        .open_session(SessionContext {
            app_version: "pp-voice-bench".into(),
            device_id: "beefbeefbeefbeefbeefbeefbeefbeef".into(),
            root_context: None,
        })
        .expect("open session");

    let cell = EndpointCell::fixed(addr);
    let transcriber = SherpaOnlineTranscriber::new("bench", cell);
    let vad = SileroVad::with_params(enter, exit, hang).expect("silero");
    // Anchor the fake clock at REAL now: event ids stay monotonic with the
    // session row (no wall-regress warning); onsets report relative to it.
    let wall0 = crate::UtcMillis::now().epoch_ms();
    let clock = FakeClock::new(wall0);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    // A per-config pre-roll (the sweep) bypasses the install-once tuning global.
    if let Some(ms) = pre_roll_ms {
        engine = engine.with_pre_roll_ms(ms);
    }
    // One synthetic bound image, like a real selection.
    engine.set_scope(vec![ContentHash::from_hex(&"ab".repeat(32)).expect("hash")]);
    assert!(engine.arm().is_armed(), "arm failed (server reachable?)");

    let frame_len = (SR as u64 * FRAME_MS / 1000) as usize;
    let mut segs: Vec<Seg> = Vec::new();
    let mut at_ms: u64 = 0;
    for chunk in samples.chunks(frame_len) {
        let events = engine.push_audio(
            &store,
            AudioFrame {
                samples: chunk.to_vec(),
                captured_at: at_ms,
            },
        );
        segs.extend(events.iter().map(|e| seg_of(e, wall0)));
        clock.advance(FRAME_MS);
        at_ms += FRAME_MS;
    }
    // End of recording: disarm and drive the drain like the shell's
    // pp-mic-drain thread — real sleeps for the server's tail flush, SLOW
    // fake-clock advance so the 5 s deadline does not outrun it.
    segs.extend(engine.disarm(&store).iter().map(|e| seg_of(e, wall0)));
    while engine.stream_open() {
        std::thread::sleep(Duration::from_millis(100));
        clock.advance(50);
        segs.extend(engine.pump(&store).iter().map(|e| seg_of(e, wall0)));
    }
    let abandoned = engine.abandoned_count();
    (segs, abandoned)
}

/// Concatenate a feed's minted segment text into one hypothesis string for WER
/// scoring. Order is mint order (onset order), which is reading order — exactly
/// what the reference transcript is in.
pub fn hypothesis_of(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The gated + raw scoring of one recording against one reference, plus the
/// produced gated segments and abandoned count (for printing / the sweep's
/// segment-count column).
#[derive(Debug, Clone)]
pub struct ScoreResult {
    pub raw: WerScore,
    pub gated: WerScore,
    pub gated_segs: Vec<Seg>,
    pub abandoned: u64,
}

impl ScoreResult {
    /// The founder's headline: how much WER the pipeline's gating ADDS on top of
    /// the model's own error (gated - raw; positive = truncation cost). This is
    /// the pp-sweep voice PRIMARY metric (lower is better).
    pub fn gating_cost(&self) -> f64 {
        self.gated.wer - self.raw.wer
    }
}

/// Run the GATED pass (the caller's VAD params) and a RAW pass (gate forced open
/// with enter/exit = 0.0) over `samples`, scoring both against `reference`.
/// Reuses one already-spawned server `addr` for both passes (the rules are the
/// server's; the VAD params are client-side). This is the single callable both
/// the bin's `--expect` and the sweep arm drive.
pub fn score_run(
    samples: &[f32],
    addr: SocketAddr,
    knobs: &VoiceKnobs,
    reference: &str,
) -> ScoreResult {
    let (gated_segs, abandoned) = drive_pipeline(
        samples,
        addr,
        knobs.enter,
        knobs.exit,
        knobs.hang,
        knobs.pre_roll_ms,
    );
    let gated = voice_wer::score(reference, &hypothesis_of(&gated_segs));
    // Raw pass: same recording, gate wide open (every window scores >= 0.0).
    // The pre-roll override still applies (it is a separate dial from the gate).
    let (raw_segs, _) = drive_pipeline(samples, addr, 0.0, 0.0, knobs.hang, knobs.pre_roll_ms);
    let raw = voice_wer::score(reference, &hypothesis_of(&raw_segs));
    ScoreResult {
        raw,
        gated,
        gated_segs,
        abandoned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV round-trip: samples written then read back match within PCM16
    /// quantization. Pins the fixture format both the bin and the sweep rely on.
    #[test]
    fn wav_round_trips_pcm16() {
        let dir = std::env::temp_dir().join(format!("vb-wavtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.wav");
        let samples: Vec<f32> = (0..1600).map(|i| ((i as f32) / 1600.0) - 0.5).collect();
        write_wav(&path, &samples).unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.len(), samples.len());
        for (a, b) in samples.iter().zip(back.iter()) {
            // PCM16 round-trip: write scales by 32767 and truncates (`as i16`),
            // read divides by 32768, so up to ~2 LSB of asymmetric quantization
            // error is expected — assert "within a couple of quanta", not exact.
            assert!((a - b).abs() < 2.0 / 32767.0, "{a} vs {b}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hypothesis join is mint-order text joined by single spaces — the
    /// reading order the reference transcript is in.
    #[test]
    fn hypothesis_joins_segments_in_order() {
        let segs = vec![
            Seg {
                onset_ms: 0,
                dur_ms: 1,
                text: "three".into(),
            },
            Seg {
                onset_ms: 10,
                dur_ms: 1,
                text: "keepers".into(),
            },
        ];
        assert_eq!(hypothesis_of(&segs), "three keepers");
    }

    /// gating_cost is gated minus raw WER (the sweep's primary metric).
    #[test]
    fn gating_cost_is_gated_minus_raw() {
        let mk = |wer: f64| WerScore {
            wer,
            sub: 0,
            del: 0,
            ins: 0,
            ref_words: 10,
            hyp_words: 10,
            hits: 10,
        };
        let r = ScoreResult {
            raw: mk(0.10),
            gated: mk(0.18),
            gated_segs: Vec::new(),
            abandoned: 0,
        };
        assert!((r.gating_cost() - 0.08).abs() < 1e-9);
    }
}
