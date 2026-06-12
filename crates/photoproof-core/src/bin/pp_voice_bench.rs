//! pp-voice-bench — the headless e2e chunking harness (BACKLOG "Voice
//! chunking tuning"; founder request, June 2026): known recordings in,
//! the program's ACTUAL minted journal entries out. The whole production
//! pipeline runs for real — silero VAD in-process, the supervised
//! `pp-asr-server` wrapper child (spawned directly here), the capture
//! engine with its onset binding and trailing-ship window — only the
//! microphone is replaced by a wav file and the wall clock by FakeClock.
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
//! Audio never persists (kernel K10 applies to the bench too): frames go
//! wav → ring → VAD → wire, all in memory; the journal db is a tempdir.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use photoproof_connectors::silero::SileroVad;
use photoproof_connectors::transcriber::AudioFrame;
use photoproof_connectors::{EndpointCell, SherpaOnlineTranscriber};
use photoproof_core::capture::{CaptureEngine, FakeClock};
use photoproof_core::{ContentHash, Event, EventStore, Payload, SessionContext};

const SR: u32 = 16_000;
const FRAME_MS: u64 = 50;

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
        enter: 0.5,
        exit: 0.35,
        hang: 15,
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

// ---------------------------------------------------------------------------
// Minimal RIFF/PCM16 mono 16 kHz wav io (the silero fixture format).
// ---------------------------------------------------------------------------

fn read_wav(path: &PathBuf) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("pp-voice-bench: read {}: {e}", path.display());
        std::process::exit(1);
    });
    // Chunk walk: verify fmt (PCM16 mono 16 kHz), find data.
    assert!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a wav"
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
    data.expect("wav has no data chunk")
        .chunks_exact(2)
        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
        .collect()
}

fn write_wav(path: &PathBuf, samples: &[f32]) {
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
    std::fs::write(path, out).expect("write wav");
}

fn synth(out: &PathBuf, speech: &PathBuf, gaps: &[f64]) {
    let clip = read_wav(speech);
    // Trim the clip's own leading/trailing silence (-35 dB energy gate,
    // the spike methodology) so the scripted gaps are the ONLY silence.
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
    write_wav(out, &samples);
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

// ---------------------------------------------------------------------------
// Run mode: the live pipeline against a recording.
// ---------------------------------------------------------------------------

fn default_model_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home).join(
        "Library/Application Support/com.photoproof.desktop/models/\
         nemotron-speech-streaming-en-0.6b-560ms-int8",
    )
}

fn default_server() -> PathBuf {
    // Workspace bins land side by side in target/{profile}/.
    let exe = std::env::current_exe().expect("current exe");
    exe.parent().expect("exe dir").join("pp-asr-server")
}

/// Spawn the wrapper child exactly as the supervisor does (ephemeral
/// port, READY handshake on stdout) and return (child, addr).
fn spawn_server(a: &Args) -> (Child, SocketAddr) {
    let server = a.server.clone().unwrap_or_else(default_server);
    let dir = a.model_dir.clone().unwrap_or_else(default_model_dir);
    let mut args = vec![
        "--port".into(),
        "0".into(),
        "--encoder".into(),
        dir.join("encoder.int8.onnx").display().to_string(),
        "--decoder".into(),
        dir.join("decoder.int8.onnx").display().to_string(),
        "--joiner".into(),
        dir.join("joiner.int8.onnx").display().to_string(),
        "--tokens".into(),
        dir.join("tokens.txt").display().to_string(),
        "--num-threads".into(),
        "4".into(),
    ];
    for (flag, v) in [
        ("--rule1", a.rule1),
        ("--rule2", a.rule2),
        ("--rule3", a.rule3),
    ] {
        if let Some(v) = v {
            args.push(flag.into());
            args.push(v.to_string());
        }
    }
    if let Some(g) = a.endpoint_grace_ms {
        args.push("--endpoint-grace-ms".into());
        args.push(g.to_string());
    }
    let mut child = Command::new(&server)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("pp-voice-bench: spawn {}: {e}", server.display());
            std::process::exit(1);
        });
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    // Model load takes a few seconds; READY is the §8.1 handshake.
    for line in &mut lines {
        let line = line.expect("server stdout");
        if let Some(port) = line.strip_prefix("READY port=") {
            let port: u16 = port.trim().parse().expect("READY port");
            return (child, SocketAddr::from(([127, 0, 0, 1], port)));
        }
    }
    eprintln!("pp-voice-bench: server exited before READY (model files present?)");
    std::process::exit(1);
}

struct Seg {
    onset_ms: i64,
    dur_ms: u32,
    text: String,
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

fn run(a: &Args) {
    let wav = a.wav.as_ref().expect("--wav required (or --synth)");
    let samples = read_wav(wav);
    let (child, addr) = spawn_server(a);
    // Kill-on-drop: a panic anywhere below must not leak the server (it
    // inherits our stderr and would hold the caller's pipe open forever).
    struct Reap(Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _reap = Reap(child);

    // Scratch journal db (tempfile is a dev-dependency; bins get std).
    let dir = std::env::temp_dir().join(format!("pp-voice-bench-{}", std::process::id()));
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
    let vad = SileroVad::with_params(a.enter, a.exit, a.hang).expect("silero");
    // Anchor the fake clock at REAL now: event ids stay monotonic with
    // the session row (no wall-regress warning); onsets report relative
    // to this anchor either way.
    let wall0 = photoproof_core::UtcMillis::now().epoch_ms();
    let clock = FakeClock::new(wall0);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    // One synthetic bound image, like a real selection.
    engine.set_scope(vec![ContentHash::from_hex(&"ab".repeat(32)).expect("hash")]);
    assert!(engine.arm().is_armed(), "arm failed (server reachable?)");

    // Feed 50 ms frames; the fake clock IS the stream clock, so the
    // run is deterministic in audio time regardless of decode speed
    // (endpoint decisions live in received-audio time on the server).
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
    // pp-mic-drain thread — real sleeps for the server's tail flush,
    // SLOW fake-clock advance so the 5 s deadline does not outrun it.
    segs.extend(engine.disarm(&store).iter().map(|e| seg_of(e, wall0)));
    while engine.stream_open() {
        std::thread::sleep(Duration::from_millis(100));
        clock.advance(50);
        segs.extend(engine.pump(&store).iter().map(|e| seg_of(e, wall0)));
    }
    let abandoned = engine.abandoned_count();

    if a.json {
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
        });
        println!("{json}");
    } else {
        println!("{} entries minted ({} abandoned):", segs.len(), abandoned);
        for s in &segs {
            println!(
                "  [{:7.2}s +{:5.2}s] {:?}",
                s.onset_ms as f64 / 1000.0,
                f64::from(s.dur_ms) / 1000.0,
                s.text
            );
        }
    }
    if let Some(expect) = &a.expect {
        let expected: Vec<String> = std::fs::read_to_string(expect)
            .expect("read --expect")
            .lines()
            .map(str::to_owned)
            .collect();
        let verdict = if expected.len() == segs.len() {
            "MATCH"
        } else {
            "MISMATCH"
        };
        println!(
            "expected {} utterances, got {} -> {verdict}",
            expected.len(),
            segs.len()
        );
    }
    // Flush before exit (println buffers under pipes).
    let _ = std::io::stdout().flush();
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
