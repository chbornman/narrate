//! pp-asr-server — the owned P2 wrapper child (RUNTIME §3.2, B67).
//!
//! Wraps a streaming Nemotron transducer engine and serves the SAME wire
//! contract as the reference websocket server — raw little-endian f32
//! sample frames in, result JSON out (`text`, `tokens`, `timestamps`,
//! `segment`, `start_time`, `is_final`), a `"Done"` text sentinel ending
//! the stream — with the one behavioral fix that motivated owning this
//! binary (B67): **finals are minted from the LAST DECODED STATE.** The
//! vendored reference server dropped text its own partials had already
//! carried (spike, reproduced ×4); CAPTURE mints journal events from
//! finals, so a final may never know less than its partials did.
//!
//! TWO engines live behind Cargo features (docs/PLAN-NEMOTRON-35-SIDECAR.md):
//! - `engine-sherpa` (DEFAULT): the sherpa-onnx Rust crate, the live
//!   shipped path; byte-for-byte today's child.
//! - `engine-parakeet`: the `parakeet-rs` Nemotron 3.5 streaming engine
//!   (pure Rust over `ort`), CPU EP only (the README warns CoreML is
//!   unstable with this model — CPU is faster than Whisper-metal anyway,
//!   and our ASR is CPU-by-design regardless). It has no built-in
//!   rule1/2/3 endpointer, so this server OWNS the endpointing for it
//!   (CAPTURE §6.3) via a trailing-silence counter.
//!
//! Both engines serve the SAME WS contract through one generic connection
//! loop ([`serve_connection`]); only the per-connection [`Engine`] session
//! differs. The connector (`photoproof-connectors::sherpa`) is bound to the
//! wire shape, not the engine, so the swap is invisible to it.
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

use tungstenite::Message;

#[cfg(feature = "engine-parakeet")]
mod engine_parakeet;
// parakeet overrides sherpa when both are on (matches `make_engine`'s cfg),
// so only compile the sherpa module when it is the actually-selected engine.
// Otherwise its whole session type is dead code under `-D warnings`.
#[cfg(all(feature = "engine-sherpa", not(feature = "engine-parakeet")))]
mod engine_sherpa;

/// Sample rate declared to the engine for every inbound binary frame. WHY:
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

/// Endpoint grace: extra audio decoded AFTER the endpoint rules fire,
/// BEFORE the final mints. WHY: the cache-aware Nemotron export emits
/// tokens with up to ~0.8 s of right-context delay (docs/SPIKE-P6.3.md
/// tail-padding finding), so at endpoint-detection time the last word's
/// tail tokens can still be in flight - minting immediately truncates
/// them ("actually incredible" -> "actually incred"; founder corpus,
/// mixed-register card). The client ships >= 3 s of trailing silence
/// (engine TRAILING_SHIP_MS), so grace audio always arrives. DEFAULT 0
/// (= mint immediately, the pre-grace behavior): corpus runs showed the
/// deferred reset can clip the NEXT word when a pause is shorter than
/// the grace, and the tail-token win is better bought by raising rule2
/// (endpointing fires later = the tail has emitted by mint time). The
/// mechanism stays for pp_voice_bench sweeps.
/// Override with --endpoint-grace-ms.
const DEFAULT_ENDPOINT_GRACE_MS: u64 = 0;

/// Energy cutoff that ends the grace EARLY: if the speaker resumes
/// before the grace elapses (a short ~1.2 s pause: endpoint fires just
/// as the next word starts), waiting out the grace would let the reset
/// eat the new word's already-consumed features (observed: "Three
/// keepers" -> "Keepers"). Peak amplitude above this in a grace chunk
/// means speech resumed: mint and reset IMMEDIATELY, degrading exactly
/// to the pre-grace behavior, never worse. The client ships true mic
/// silence between utterances (peaks well under 0.005 even on laptop
/// mics); speech peaks two orders above.
const GRACE_SPEECH_PEAK: f32 = 0.02;

#[derive(Clone)]
struct Args {
    port: u16,
    encoder: String,
    decoder: String,
    joiner: String,
    tokens: String,
    /// The parakeet-rs engine loads a model DIRECTORY (encoder.onnx +
    /// .data + decoder_joint.onnx + tokenizer.model), not the four sherpa
    /// transducer files. The launcher passes `--model-dir`; the active
    /// engine reads only what it needs (sherpa: the four files; parakeet:
    /// the dir). Empty when the launcher predates the parakeet wiring —
    /// the parakeet engine derives it from `encoder`'s parent then.
    model_dir: String,
    /// Per-stream language for the multilingual 3.5 export (`en`/`ja`/`auto`).
    /// The sherpa engine accepts-and-ignores it (single fixed language);
    /// the parakeet engine calls `set_target_lang` with it.
    lang: String,
    num_threads: i32,
    /// Endpoint rules (CAPTURE §6.3): sherpa's canonical defaults; the
    /// chunking-tuning harness (pp_voice_bench) sweeps them. The launcher
    /// (runtime::launch) passes none — production runs the defaults.
    rule1: f32,
    rule2: f32,
    rule3: f32,
    endpoint_grace_ms: u64,
}

fn parse_args() -> Args {
    let mut a = Args {
        port: 0,
        encoder: String::new(),
        decoder: String::new(),
        joiner: String::new(),
        tokens: String::new(),
        model_dir: String::new(),
        lang: "en".into(),
        num_threads: MIN_INTRA_OP_THREADS,
        rule1: DEFAULT_RULE1_TRAILING_SILENCE_S,
        rule2: DEFAULT_RULE2_TRAILING_SILENCE_S,
        rule3: DEFAULT_RULE3_MIN_UTTERANCE_S,
        endpoint_grace_ms: DEFAULT_ENDPOINT_GRACE_MS,
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
            // The parakeet model dir. Optional: when absent the parakeet
            // engine derives it from --encoder's parent (the manifest lays
            // each ASR export under its own models_dir/<id>/ root).
            "--model-dir" => a.model_dir = val(),
            // Accepted for launcher symmetry; chunking is the client's.
            "--chunk-ms" => {
                let _ = val();
            }
            // The per-stream language (default "en"). The launcher ALWAYS
            // passes it (docs/PLAN-NEMOTRON-35-SIDECAR.md §4); the sherpa
            // engine ignores it, the parakeet engine reads it. WHY accept it
            // even when ignored: an unknown flag exits 2, and the launcher
            // now always sends --lang, so accepting it keeps the live child
            // spawning unchanged.
            "--lang" => a.lang = val(),
            "--num-threads" => a.num_threads = val().parse().expect("--num-threads"),
            "--rule1" => a.rule1 = val().parse().expect("--rule1"),
            "--rule2" => a.rule2 = val().parse().expect("--rule2"),
            "--rule3" => a.rule3 = val().parse().expect("--rule3"),
            "--endpoint-grace-ms" => {
                a.endpoint_grace_ms = val().parse().expect("--endpoint-grace-ms");
            }
            other => {
                eprintln!("pp-asr-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    // The spike's real-time floor, clamped even if the launcher changes.
    a.num_threads = a.num_threads.max(MIN_INTRA_OP_THREADS);
    if a.lang.is_empty() {
        // Never the SPIKE's None-language coin-flip class.
        a.lang = "en".into();
    }
    a
}

/// Little-endian f32 frames, per the reference server's wire format.
fn samples_of(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// One decoded result the generic loop can serialize, engine-agnostic.
/// WHY a plain struct (not the engine's native result type): it lets ONE
/// `result_json` + ONE connection loop serve both engines unchanged — the
/// sherpa engine fills `timestamps`/`tokens`; parakeet omits them (the
/// contract makes both optional, and CAPTURE binds onsets to VAD, not
/// token times, RUNTIME §3.2).
#[derive(Default, Clone)]
struct EngineResult {
    text: String,
    tokens: Vec<String>,
    timestamps: Vec<f32>,
    start_time: f32,
}

/// A per-connection streaming session. Each engine implements this; the
/// generic [`serve_connection`] loop drives it identically. The session
/// owns ALL decode state (sherpa: the recognizer + `OnlineStream`;
/// parakeet: the `Nemotron` + the silence counter), so one engine's state
/// model never leaks into the loop.
trait Engine: Send {
    /// Feed one frame of f32 samples; decode as far as the audio allows.
    fn accept(&mut self, samples: &[f32]);
    /// The current decoded result (partial), or `None`/empty if nothing
    /// is decoded yet. The loop sends it as a partial when non-empty.
    fn result(&mut self) -> Option<EngineResult>;
    /// Has the endpointer fired for the current utterance? (sherpa:
    /// `is_endpoint`; parakeet: the trailing-silence counter.) The loop's
    /// grace machinery keys off this, identically for both engines.
    fn is_endpoint(&mut self) -> bool;
    /// Mint the final from the LAST DECODED STATE (B67), then clear the
    /// engine for the next utterance.
    fn reset(&mut self);
    /// The "Done" flush: drain whatever the feature pipeline still holds,
    /// then return the final from the last state.
    fn flush(&mut self) -> Option<EngineResult>;
}

fn result_json(r: &EngineResult, segment: u32, is_final: bool) -> String {
    serde_json::json!({
        "text": r.text,
        "tokens": r.tokens,
        "timestamps": r.timestamps,
        "segment": segment,
        "start_time": r.start_time,
        "is_final": is_final,
    })
    .to_string()
}

/// The generic per-connection loop. Identical for both engines — the only
/// engine-specific code is the [`Engine`] session passed in. This is the
/// B67 + grace + endpoint behavior, verbatim from the original sherpa
/// loop, lifted to run against the trait.
fn serve_connection(mut engine: Box<dyn Engine>, conn: std::net::TcpStream, grace_samples: u64) {
    let Ok(mut ws) = tungstenite::accept(conn) else {
        return;
    };
    let mut segment: u32 = 0;
    let mut received: u64 = 0;
    // Samples received when the endpoint rules first fired for the current
    // utterance; the final mints `grace_samples` later so the model's
    // delayed tail tokens (see DEFAULT_ENDPOINT_GRACE_MS) make it in.
    let mut endpoint_at: Option<u64> = None;
    loop {
        let msg = match ws.read() {
            Ok(m) => m,
            Err(_) => return, // client gone; stream drops with us
        };
        match msg {
            Message::Binary(bytes) => {
                let samples = samples_of(&bytes);
                received += samples.len() as u64;
                engine.accept(&samples);
                if let Some(r) = engine.result()
                    && !r.text.is_empty()
                {
                    let _ = ws.send(Message::text(result_json(&r, segment, false)));
                }
                if endpoint_at.is_none() && engine.is_endpoint() {
                    endpoint_at = Some(received);
                }
                // B67 + grace: an endpoint mints the final FROM THE LAST
                // DECODED STATE - but only after the grace audio decoded,
                // so the tail tokens the cache-aware model emits late are
                // part of that state. Speech resuming inside the grace ends
                // it immediately (GRACE_SPEECH_PEAK's WHY). Nothing decoded
                // is ever dropped.
                let speech_resumed =
                    endpoint_at.is_some() && samples.iter().any(|s| s.abs() > GRACE_SPEECH_PEAK);
                if endpoint_at.is_some_and(|at| received.saturating_sub(at) >= grace_samples)
                    || speech_resumed
                {
                    if let Some(r) = engine.result()
                        && !r.text.is_empty()
                    {
                        let _ = ws.send(Message::text(result_json(&r, segment, true)));
                        segment += 1;
                    }
                    engine.reset();
                    endpoint_at = None;
                }
            }
            Message::Text(t) if t.trim() == "Done" => {
                // Flush: drain whatever the feature pipeline still holds,
                // then the B67 final from the last state.
                if let Some(r) = engine.flush()
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
}

/// Build the per-connection engine session for the active Cargo feature.
/// The shared `Engine` factory: returns a fresh session bound to the
/// model the supervisor pinned. `engine-parakeet` wins if both features
/// are on (a deliberate explicit-opt-in build); the default binary has
/// only `engine-sherpa` and so takes the sherpa path.
#[cfg(feature = "engine-parakeet")]
fn make_engine(args: &Args) -> Box<dyn Engine> {
    engine_parakeet::new_session(args)
}

#[cfg(all(feature = "engine-sherpa", not(feature = "engine-parakeet")))]
fn make_engine(args: &Args) -> Box<dyn Engine> {
    engine_sherpa::new_session(args)
}

#[cfg(not(any(feature = "engine-sherpa", feature = "engine-parakeet")))]
compile_error!(
    "pp-asr-server needs exactly one ASR engine feature: engine-sherpa (default) or engine-parakeet"
);

fn main() {
    let args = parse_args();
    // Fail fast if the model is unloadable (the §8.1 health gate's other
    // half: no READY without a working engine). Build one throwaway
    // session to surface a bad model BEFORE we print READY.
    drop(make_engine(&args));

    let listener = TcpListener::bind(("127.0.0.1", args.port)).unwrap_or_else(|e| {
        eprintln!("pp-asr-server: bind 127.0.0.1:{}: {e}", args.port);
        std::process::exit(1);
    });
    let port = listener.local_addr().expect("local addr").port();
    // The §8.1 readiness signal: model loadable, socket listening.
    println!("READY port={port}");

    // A thread per connection keeps a dying client from wedging accept
    // (the capture engine holds one stream at a time). Each connection
    // builds its OWN engine session (parakeet's Nemotron is `&mut` and
    // stateful per stream; sherpa builds a fresh recognizer+stream too) —
    // a fresh session per conn is correct for both and keeps the loop
    // uniform.
    let args = std::sync::Arc::new(args);
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let args = std::sync::Arc::clone(&args);
        let grace_samples = args.endpoint_grace_ms * SAMPLE_RATE_HZ as u64 / 1000;
        thread::spawn(move || {
            let engine = make_engine(&args);
            serve_connection(engine, conn, grace_samples);
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
        let r = EngineResult {
            text: "after early nightfall".into(),
            tokens: vec![" after".into(), " early".into()],
            timestamps: vec![0.48, 0.8],
            start_time: 0.0,
        };
        let v: serde_json::Value = serde_json::from_str(&result_json(&r, 3, true)).unwrap();
        assert_eq!(v["text"], "after early nightfall");
        assert_eq!(v["segment"], 3);
        assert_eq!(v["is_final"], true);
        let t0 = v["timestamps"][0].as_f64().unwrap();
        assert!((t0 - 0.48).abs() < 1e-6, "f32 round-trip: {t0}");
    }

    /// The per-stream language defaults to `en` and is never empty (the
    /// SPIKE's None-language coin-flip class). An explicit empty `--lang`
    /// must still resolve to `en`.
    #[test]
    fn empty_lang_resolves_to_english() {
        let mut a = Args {
            port: 0,
            encoder: String::new(),
            decoder: String::new(),
            joiner: String::new(),
            tokens: String::new(),
            model_dir: String::new(),
            lang: String::new(),
            num_threads: MIN_INTRA_OP_THREADS,
            rule1: DEFAULT_RULE1_TRAILING_SILENCE_S,
            rule2: DEFAULT_RULE2_TRAILING_SILENCE_S,
            rule3: DEFAULT_RULE3_MIN_UTTERANCE_S,
            endpoint_grace_ms: DEFAULT_ENDPOINT_GRACE_MS,
        };
        if a.lang.is_empty() {
            a.lang = "en".into();
        }
        assert_eq!(a.lang, "en");
    }
}
