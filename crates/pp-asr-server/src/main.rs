//! pp-asr-server — the owned P2 wrapper child (RUNTIME §3.2, B67).
//!
//! Wraps the official sherpa-onnx Rust crate around the Nemotron
//! streaming transducer and serves the SAME wire contract as the
//! reference websocket server — raw little-endian f32 sample frames in,
//! result JSON out (`text`, `tokens`, `timestamps`, `segment`,
//! `start_time`, `is_final`), a `"Done"` text sentinel ending the
//! stream — with the one behavioral fix that motivated owning this
//! binary (B67): **finals are minted from the LAST DECODED STATE.** The
//! vendored reference server dropped text its own partials had already
//! carried (spike, reproduced ×4); CAPTURE mints journal events from
//! finals, so a final may never know less than its partials did.
//!
//! Spike-mandated posture (docs/SPIKE-P6.3.md): ONNX intra-op threads
//! ≥ 4 (the default of 1 decodes slower than real time) — enforced by
//! the launcher (`runtime::launch::asr_wrapper_args`) and clamped again
//! here; clients pad ~0.8 s of silence before "Done" (the cache-aware
//! model holds right context — connector-side duty).
//!
//! Supervision contract: prints `READY port={port}` on stdout once
//! listening (the §8.1 health gate's stdout half); dies with the parent
//! per the §8.4 mechanisms the supervisor installs at spawn.

use std::net::TcpListener;
use std::thread;

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig,
};
use tungstenite::Message;

/// Sample rate declared to sherpa for every inbound binary frame. WHY:
/// the P2 wire contract (RUNTIME §3.2: 16 kHz mono f32) and the Nemotron
/// model's required input rate; must match the connector side
/// (photoproof-connectors sherpa.rs and silero.rs).
const SAMPLE_RATE_HZ: i32 = 16_000;

/// Minimum ONNX intra-op thread count — both the `--num-threads` default
/// and the clamp floor. WHY: below 4 threads the Nemotron transducer
/// decodes slower than real time (docs/SPIKE-P6.3.md), so this is a
/// correctness floor, not a tuning default.
const MIN_INTRA_OP_THREADS: i32 = 4;

/// Default endpointing rules (CAPTURE §6.3 — endpointing authority lives
/// in this server). WHY these values: sherpa's canonical defaults, in
/// SECONDS. Runtime-overridable via `--rule1/2/3` for the pp_voice_bench
/// sweep; production runs these.
///
/// Rule 1: minimum trailing silence when NOTHING has been decoded yet —
/// the dead-air endpoint. Tuning this does not change how fast an
/// endpoint lands after speech; that is rule 2's job.
const DEFAULT_RULE1_TRAILING_SILENCE_S: f32 = 2.4;
/// Rule 2: minimum trailing silence AFTER decoded speech — the
/// post-utterance endpoint latency a user actually feels after they stop
/// talking. This is the knob to sweep for snappier finals.
const DEFAULT_RULE2_TRAILING_SILENCE_S: f32 = 1.2;
/// Rule 3: minimum utterance length that forces an endpoint regardless
/// of silence.
const DEFAULT_RULE3_MIN_UTTERANCE_S: f32 = 20.0;

struct Args {
    port: u16,
    encoder: String,
    decoder: String,
    joiner: String,
    tokens: String,
    num_threads: i32,
    /// Endpoint rules (CAPTURE §6.3): sherpa's canonical defaults; the
    /// chunking-tuning harness (pp_voice_bench) sweeps them. The launcher
    /// (runtime::launch) passes none — production runs the defaults.
    rule1: f32,
    rule2: f32,
    rule3: f32,
}

fn parse_args() -> Args {
    let mut a = Args {
        port: 0,
        encoder: String::new(),
        decoder: String::new(),
        joiner: String::new(),
        tokens: String::new(),
        num_threads: MIN_INTRA_OP_THREADS,
        rule1: DEFAULT_RULE1_TRAILING_SILENCE_S,
        rule2: DEFAULT_RULE2_TRAILING_SILENCE_S,
        rule3: DEFAULT_RULE3_MIN_UTTERANCE_S,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--port" => a.port = val().parse().expect("--port"),
            "--encoder" => a.encoder = val(),
            "--decoder" => a.decoder = val(),
            "--joiner" => a.joiner = val(),
            "--tokens" => a.tokens = val(),
            // Accepted for launcher symmetry; chunking is the client's.
            "--chunk-ms" => {
                let _ = val();
            }
            "--num-threads" => a.num_threads = val().parse().expect("--num-threads"),
            "--rule1" => a.rule1 = val().parse().expect("--rule1"),
            "--rule2" => a.rule2 = val().parse().expect("--rule2"),
            "--rule3" => a.rule3 = val().parse().expect("--rule3"),
            other => {
                eprintln!("pp-asr-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    // The spike's real-time floor, clamped even if the launcher changes.
    a.num_threads = a.num_threads.max(MIN_INTRA_OP_THREADS);
    a
}

fn recognizer(a: &Args) -> OnlineRecognizer {
    let config = OnlineRecognizerConfig {
        model_config: OnlineModelConfig {
            transducer: OnlineTransducerModelConfig {
                encoder: Some(a.encoder.clone()),
                decoder: Some(a.decoder.clone()),
                joiner: Some(a.joiner.clone()),
            },
            tokens: Some(a.tokens.clone()),
            num_threads: a.num_threads,
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        // Endpointing authority lives HERE (CAPTURE §6.3) — sherpa's
        // canonical rule values by default; the in-process VAD only gates
        // and stamps onsets.
        enable_endpoint: true,
        rule1_min_trailing_silence: a.rule1,
        rule2_min_trailing_silence: a.rule2,
        rule3_min_utterance_length: a.rule3,
        ..Default::default()
    };
    OnlineRecognizer::create(&config).unwrap_or_else(|| {
        eprintln!("pp-asr-server: recognizer create failed (model paths/threads?)");
        std::process::exit(1);
    })
}

/// Little-endian f32 frames, per the reference server's wire format.
fn samples_of(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn result_json(r: &sherpa_onnx::RecognizerResult, segment: u32, is_final: bool) -> String {
    serde_json::json!({
        "text": r.text,
        "tokens": r.tokens,
        "timestamps": r.timestamps.clone().unwrap_or_default(),
        "segment": segment,
        "start_time": r.start_time.unwrap_or(0.0),
        "is_final": is_final,
    })
    .to_string()
}

fn main() {
    let args = parse_args();
    let rec = recognizer(&args);
    let listener = TcpListener::bind(("127.0.0.1", args.port)).unwrap_or_else(|e| {
        eprintln!("pp-asr-server: bind 127.0.0.1:{}: {e}", args.port);
        std::process::exit(1);
    });
    let port = listener.local_addr().expect("local addr").port();
    // The §8.1 readiness signal: model loaded, socket listening.
    println!("READY port={port}");

    // One engine; a thread per connection keeps a dying client from
    // wedging accept (the capture engine holds one stream at a time).
    let rec = std::sync::Arc::new(rec);
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let rec = std::sync::Arc::clone(&rec);
        thread::spawn(move || {
            let Ok(mut ws) = tungstenite::accept(conn) else {
                return;
            };
            let stream = rec.create_stream();
            let mut segment: u32 = 0;
            loop {
                let msg = match ws.read() {
                    Ok(m) => m,
                    Err(_) => return, // client gone; stream drops with us
                };
                match msg {
                    Message::Binary(bytes) => {
                        stream.accept_waveform(SAMPLE_RATE_HZ, &samples_of(&bytes));
                        while rec.is_ready(&stream) {
                            rec.decode(&stream);
                        }
                        if let Some(r) = rec.get_result(&stream)
                            && !r.text.is_empty()
                        {
                            let _ = ws.send(Message::text(result_json(&r, segment, false)));
                        }
                        // B67: an endpoint mints the final FROM THE LAST
                        // DECODED STATE — the same result the newest
                        // partial carried — then resets for the next
                        // segment. Nothing decoded is ever dropped.
                        if rec.is_endpoint(&stream) {
                            if let Some(r) = rec.get_result(&stream)
                                && !r.text.is_empty()
                            {
                                let _ = ws.send(Message::text(result_json(&r, segment, true)));
                                segment += 1;
                            }
                            rec.reset(&stream);
                        }
                    }
                    Message::Text(t) if t.trim() == "Done" => {
                        // Flush: drain whatever the feature pipeline still
                        // holds, then the B67 final from the last state.
                        stream.input_finished();
                        while rec.is_ready(&stream) {
                            rec.decode(&stream);
                        }
                        if let Some(r) = rec.get_result(&stream)
                            && !r.text.is_empty()
                        {
                            let _ = ws.send(Message::text(result_json(&r, segment, true)));
                        }
                        let _ = ws.send(Message::text("Done".to_string()));
                        let _ = ws.close(None);
                        return;
                    }
                    Message::Close(_) => return,
                    _ => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the spike-mandated posture (docs/SPIKE-P6.3.md): below 4
    /// intra-op threads the transducer decodes slower than real time, and
    /// 16 kHz is the P2 wire contract shared with the connector side.
    /// These are correctness invariants, not tunables — a deliberate
    /// change must update the spike doc and the connector together.
    #[test]
    fn spike_and_wire_contract_values_are_pinned() {
        assert_eq!(MIN_INTRA_OP_THREADS, 4);
        assert_eq!(SAMPLE_RATE_HZ, 16_000);
    }

    #[test]
    fn wire_samples_decode_little_endian_f32() {
        let mut bytes = Vec::new();
        for v in [0.0f32, 0.5, -1.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(samples_of(&bytes), vec![0.0, 0.5, -1.0]);
        assert_eq!(
            samples_of(&bytes[..5]),
            vec![0.0],
            "trailing partial dropped"
        );
    }

    #[test]
    fn result_json_carries_the_contract_fields() {
        let r = sherpa_onnx::RecognizerResult {
            text: "after early nightfall".into(),
            tokens: vec![" after".into(), " early".into()],
            timestamps: Some(vec![0.48, 0.8]),
            segment: Some(0),
            start_time: Some(0.0),
            is_final: false,
        };
        let v: serde_json::Value = serde_json::from_str(&result_json(&r, 3, true)).unwrap();
        assert_eq!(v["text"], "after early nightfall");
        assert_eq!(v["segment"], 3);
        assert_eq!(v["is_final"], true);
        let t0 = v["timestamps"][0].as_f64().unwrap();
        assert!((t0 - 0.48).abs() < 1e-6, "f32 round-trip: {t0}");
    }
}
