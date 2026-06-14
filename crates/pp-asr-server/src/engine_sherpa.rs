//! The sherpa-onnx engine (DEFAULT) — the live, shipped ASR path.
//!
//! This is the original `pp-asr-server` engine, unchanged in behavior:
//! the sherpa-onnx `OnlineRecognizer` over the four-file Nemotron
//! transducer export (encoder/decoder/joiner.int8.onnx + tokens.txt),
//! with sherpa's own rule1/2/3 endpointer (CAPTURE §6.3 — endpointing
//! authority lives in this server) and B67 finals-from-last-state. It is
//! factored behind the shared [`crate::Engine`] trait so the generic
//! connection loop can drive it identically to the parakeet engine, but
//! the decode logic itself is byte-for-byte what shipped.

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};

use crate::{Args, Engine, EngineResult, SAMPLE_RATE_HZ};

/// Build the recognizer from the four-file transducer export. Exits 1 on
/// failure (bad model paths / threads) — the supervisor reads that as a
/// failed §8.1 health gate (no READY).
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

fn result_of(r: &sherpa_onnx::RecognizerResult) -> EngineResult {
    EngineResult {
        text: r.text.clone(),
        tokens: r.tokens.clone(),
        timestamps: r.timestamps.clone().unwrap_or_default(),
        start_time: r.start_time.unwrap_or(0.0),
    }
}

/// One per-connection sherpa session: its own recognizer + stream. WHY a
/// recognizer per connection (the original shared one across connections):
/// the capture engine holds one stream at a time, so per-connection
/// construction is functionally identical and keeps the [`Engine`] factory
/// uniform with parakeet (whose `Nemotron` MUST be per-stream).
struct SherpaSession {
    rec: OnlineRecognizer,
    stream: OnlineStream,
}

impl Engine for SherpaSession {
    fn accept(&mut self, samples: &[f32]) {
        self.stream.accept_waveform(SAMPLE_RATE_HZ, samples);
        while self.rec.is_ready(&self.stream) {
            self.rec.decode(&self.stream);
        }
    }

    fn result(&mut self) -> Option<EngineResult> {
        // get_result is itself Optional; map the non-empty inner result.
        self.rec
            .get_result(&self.stream)
            .filter(|r| !r.text.is_empty())
            .map(|r| result_of(&r))
    }

    fn is_endpoint(&mut self) -> bool {
        self.rec.is_endpoint(&self.stream)
    }

    fn reset(&mut self) {
        self.rec.reset(&self.stream);
    }

    fn flush(&mut self) -> Option<EngineResult> {
        self.stream.input_finished();
        while self.rec.is_ready(&self.stream) {
            self.rec.decode(&self.stream);
        }
        self.result()
    }
}

pub fn new_session(args: &Args) -> Box<dyn Engine> {
    let rec = recognizer(args);
    let stream = rec.create_stream();
    Box::new(SherpaSession { rec, stream })
}
