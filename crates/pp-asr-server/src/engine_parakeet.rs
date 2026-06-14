//! The parakeet-rs Nemotron 3.5 streaming engine (gated `engine-parakeet`).
//!
//! `parakeet-rs` (crates.io 0.3.6) is a pure-Rust streaming ASR over `ort`
//! (the SAME ONNX Runtime rc the connectors crate already vendors) whose
//! published `Nemotron` type ships Nemotron 3.5 multilingual streaming —
//! the support the lagging sherpa-onnx Rust crate does NOT yet have
//! (docs/PLAN-NEMOTRON-35-SIDECAR.md). This engine serves the EXACT same
//! WS contract as the sherpa engine through the shared [`crate::Engine`]
//! trait + the generic connection loop; only the decode internals differ.
//!
//! CPU EP ONLY. The parakeet-rs README warns "CoreML is unstable with this
//! model ... use CPU; even CPU is faster than Whisper-metal", and our spec
//! keeps ASR on CPU regardless (the embedders/LLM own the accelerator). So
//! the Cargo dependency pulls NO coreml/cuda feature and we pin
//! [`ExecutionProvider::Cpu`] explicitly.
//!
//! THREE things this engine OWNS that the sherpa engine got for free:
//! 1. **Chunking.** `parakeet-rs` decodes in fixed 560 ms chunks
//!    ([`CHUNK_SAMPLES`]); the wire delivers arbitrary ~50 ms frames. We
//!    buffer inbound samples and feed full chunks (the leftover < 1 chunk
//!    waits for the next frame, or is zero-padded at flush).
//! 2. **Endpointing.** `parakeet-rs` has no rule1/2/3 endpointer, so we
//!    port CAPTURE §6.3's trailing-silence logic: a silence-duration
//!    counter on the f32 stream replaces sherpa's `is_endpoint`. rule2
//!    (post-speech silence) and rule1 (dead-air, pre-speech) map directly;
//!    rule3 forces an endpoint past a max utterance length.
//! 3. **B67 finals-from-last-state.** `transcribe_chunk` returns the
//!    INCREMENTAL text for that chunk; we accumulate it per utterance, so
//!    `result()` returns the running text-so-far and the minted final can
//!    never carry less than its partials did.

use std::path::{Path, PathBuf};

use parakeet_rs::{ExecutionConfig, ExecutionProvider, Nemotron, NemotronMode};

use crate::{Args, Engine, EngineResult, SAMPLE_RATE_HZ};

/// parakeet-rs Nemotron streaming chunk size: 560 ms at 16 kHz = 8960
/// samples (the export's cache-aware chunk; the streaming example uses
/// exactly this). The model decodes a whole chunk at a time; a partial
/// trailing chunk is zero-padded only at flush.
const CHUNK_SAMPLES: usize = 8_960;

/// Peak-amplitude floor below which a chunk counts as SILENCE for the
/// ported endpointer. WHY this value: identical to the connector's
/// observation that true mic silence peaks well under 0.005 even on laptop
/// mics while speech peaks two orders above (see crate::GRACE_SPEECH_PEAK);
/// 0.01 sits safely between, so a held mic never false-endpoints and a real
/// pause always does.
const SILENCE_PEAK: f32 = 0.01;

/// Resolve the model directory the parakeet engine loads. The launcher
/// passes `--model-dir`; if absent (an older launcher that only knows the
/// four-file flags), fall back to `--encoder`'s parent — the manifest lays
/// each ASR export under its own `models_dir/<id>/` root, so the encoder's
/// directory IS the model dir.
fn model_dir(a: &Args) -> PathBuf {
    if !a.model_dir.is_empty() {
        return PathBuf::from(&a.model_dir);
    }
    Path::new(&a.encoder)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Load the Nemotron model on the CPU EP and set the per-stream language.
/// Exits 1 on any failure (unloadable model / bad lang) so the supervisor
/// reads a failed §8.1 health gate (no READY), matching the sherpa engine.
fn load(a: &Args) -> Nemotron {
    let dir = model_dir(a);
    // CPU EP, explicitly — never CoreML (README: unstable for this model)
    // and ASR is CPU-by-design. intra-threads honors the spike's >= 4
    // real-time floor (already clamped in parse_args).
    let cfg = ExecutionConfig::new()
        .with_execution_provider(ExecutionProvider::Cpu)
        .with_intra_threads(a.num_threads.max(1) as usize);
    let mut model = Nemotron::from_pretrained(&dir, Some(cfg)).unwrap_or_else(|e| {
        eprintln!(
            "pp-asr-server: parakeet Nemotron load failed ({}): {e}",
            dir.display()
        );
        std::process::exit(1);
    });
    // Per-stream language only applies to the multilingual 3.5 export; the
    // English-only export has no language knob (set_target_lang would
    // error), so guard on the detected mode.
    if let NemotronMode::Multilingual = model.mode()
        && let Err(e) = model.set_target_lang(&a.lang)
    {
        eprintln!(
            "pp-asr-server: parakeet set_target_lang({}) failed: {e}",
            a.lang
        );
        std::process::exit(1);
    }
    model
}

/// The ported CAPTURE §6.3 endpointer state, in SAMPLES (the wire's clock).
/// Mirrors sherpa's rule1/2/3 so the generic loop's grace machinery sees
/// the same `is_endpoint` signal it does for sherpa.
struct Endpointer {
    rule1_silence: u64, // dead-air (nothing decoded yet) trailing silence
    rule2_silence: u64, // post-speech trailing silence
    rule3_max: u64,     // max utterance length, forces an endpoint
    /// Whether ANY speech has been decoded in the current utterance.
    has_speech: bool,
    /// Contiguous trailing-silence samples since the last non-silent chunk.
    trailing_silence: u64,
    /// Total samples consumed in the current utterance (for rule3).
    utterance_samples: u64,
}

impl Endpointer {
    fn new(a: &Args) -> Self {
        let s = |secs: f32| (secs.max(0.0) * SAMPLE_RATE_HZ as f32) as u64;
        Self {
            rule1_silence: s(a.rule1),
            rule2_silence: s(a.rule2),
            rule3_max: s(a.rule3),
            has_speech: false,
            trailing_silence: 0,
            utterance_samples: 0,
        }
    }

    /// Account one decoded chunk. `silent` = the chunk's peak fell below
    /// [`SILENCE_PEAK`]; `had_text` = the chunk produced incremental text
    /// (a stronger "speech happened" signal than energy alone).
    fn observe(&mut self, chunk_len: usize, silent: bool, had_text: bool) {
        let n = chunk_len as u64;
        self.utterance_samples += n;
        if had_text {
            self.has_speech = true;
        }
        if silent {
            self.trailing_silence += n;
        } else {
            self.trailing_silence = 0;
        }
    }

    /// Has an endpoint fired? rule3 (max length) OR the appropriate
    /// trailing-silence threshold (rule2 once speech was decoded, rule1
    /// for pre-speech dead air).
    fn fired(&self) -> bool {
        if self.utterance_samples >= self.rule3_max {
            return true;
        }
        let threshold = if self.has_speech {
            self.rule2_silence
        } else {
            self.rule1_silence
        };
        self.trailing_silence >= threshold
    }

    fn reset(&mut self) {
        self.has_speech = false;
        self.trailing_silence = 0;
        self.utterance_samples = 0;
    }
}

/// One per-connection parakeet session: the stateful `Nemotron`, the
/// sample buffer that re-chunks the wire to 560 ms, the per-utterance text
/// accumulator (B67), and the ported endpointer.
struct ParakeetSession {
    model: Nemotron,
    /// Leftover inbound samples not yet forming a full [`CHUNK_SAMPLES`].
    buf: Vec<f32>,
    /// Accumulated text for the CURRENT utterance (cleared on reset). The
    /// running partial; the final mints from this (never less than the
    /// partials carried).
    utterance_text: String,
    endpointer: Endpointer,
}

impl ParakeetSession {
    /// Feed every full 560 ms chunk currently buffered, accumulating its
    /// incremental text and updating the endpointer. Leftover < 1 chunk
    /// stays buffered for the next frame.
    fn drain_full_chunks(&mut self) {
        while self.buf.len() >= CHUNK_SAMPLES {
            let chunk: Vec<f32> = self.buf.drain(..CHUNK_SAMPLES).collect();
            self.feed_chunk(&chunk);
        }
    }

    /// Decode one chunk: run the model, accumulate the incremental text,
    /// and account it to the endpointer.
    fn feed_chunk(&mut self, chunk: &[f32]) {
        let silent = !chunk.iter().any(|s| s.abs() > SILENCE_PEAK);
        // transcribe_chunk returns the INCREMENTAL text for this chunk; a
        // decode error mid-stream is treated as "no new text" rather than
        // killing the connection (a dropped chunk is recoverable; the
        // sherpa engine likewise never aborts a stream on a decode hiccup).
        let inc = self.model.transcribe_chunk(chunk).unwrap_or_default();
        let had_text = !inc.is_empty();
        if had_text {
            self.utterance_text.push_str(&inc);
        }
        self.endpointer.observe(chunk.len(), silent, had_text);
    }

    fn current_result(&self) -> Option<EngineResult> {
        let text = self.utterance_text.trim();
        if text.is_empty() {
            None
        } else {
            // tokens/timestamps omitted: parakeet-rs Nemotron exposes
            // neither per-token text nor per-token times on the streaming
            // path, and both are Optional in the wire contract (CAPTURE
            // binds onsets to VAD, not token times, RUNTIME §3.2).
            Some(EngineResult {
                text: text.to_string(),
                ..Default::default()
            })
        }
    }
}

impl Engine for ParakeetSession {
    fn accept(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
        self.drain_full_chunks();
    }

    fn result(&mut self) -> Option<EngineResult> {
        self.current_result()
    }

    fn is_endpoint(&mut self) -> bool {
        self.endpointer.fired()
    }

    fn reset(&mut self) {
        // B67: the final has already been minted from utterance_text by the
        // loop; now clear the model's decode state and our accumulators for
        // the next utterance. Leftover buffered (< 1 chunk) audio is dropped
        // with the utterance boundary, exactly as sherpa's reset discards
        // the in-flight feature frames.
        self.model.reset();
        self.buf.clear();
        self.utterance_text.clear();
        self.endpointer.reset();
    }

    fn flush(&mut self) -> Option<EngineResult> {
        // Drain any full chunks still buffered, then zero-pad the final
        // partial chunk and push a few trailing zero chunks — the
        // cache-aware model emits the last words' tail tokens only once
        // enough audio FOLLOWS them (the streaming example flushes with 3
        // zero chunks; the connector also pads ~1.5 s of silence before
        // "Done", so this is belt-and-suspenders for the tail).
        self.drain_full_chunks();
        if !self.buf.is_empty() {
            let mut last: Vec<f32> = std::mem::take(&mut self.buf);
            last.resize(CHUNK_SAMPLES, 0.0);
            self.feed_chunk(&last);
        }
        for _ in 0..3 {
            self.feed_chunk(&[0.0; CHUNK_SAMPLES]);
        }
        self.current_result()
    }
}

pub fn new_session(args: &Args) -> Box<dyn Engine> {
    let model = load(args);
    Box::new(ParakeetSession {
        model,
        buf: Vec::with_capacity(CHUNK_SAMPLES * 2),
        utterance_text: String::new(),
        endpointer: Endpointer::new(args),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_args() -> Args {
        Args {
            port: 0,
            encoder: String::new(),
            decoder: String::new(),
            joiner: String::new(),
            tokens: String::new(),
            model_dir: String::new(),
            lang: "en".into(),
            num_threads: 4,
            rule1: 2.4,
            rule2: 1.2,
            rule3: 20.0,
            endpoint_grace_ms: 0,
        }
    }

    /// 560 ms at 16 kHz is exactly 8960 samples — the parakeet streaming
    /// chunk. A drift here silently mis-chunks every stream.
    #[test]
    fn chunk_is_560ms_at_16khz() {
        assert_eq!(CHUNK_SAMPLES, (0.560 * 16_000.0) as usize);
    }

    /// rule2 (post-speech silence) fires once speech was decoded and the
    /// trailing silence crosses 1.2 s; before any speech it is rule1's
    /// longer dead-air threshold that applies.
    #[test]
    fn endpointer_uses_rule2_after_speech_and_rule1_before() {
        let mut ep = Endpointer::new(&test_args());
        // 1.2 s of pre-speech silence: rule1 (2.4 s) NOT yet met.
        ep.observe(16_000 * 12 / 10, true, false);
        assert!(!ep.fired(), "pre-speech: rule1 not yet crossed");
        // Speech, then 1.2 s silence: rule2 (1.2 s) crosses.
        ep.observe(CHUNK_SAMPLES, false, true);
        ep.observe(16_000 * 12 / 10, true, false);
        assert!(ep.fired(), "post-speech: rule2 crossed");
    }

    /// rule3 forces an endpoint past the max utterance length even with no
    /// trailing silence (continuous speech).
    #[test]
    fn endpointer_rule3_forces_endpoint_on_long_utterance() {
        let mut ep = Endpointer::new(&test_args());
        // 21 s of continuous (non-silent) speech, past the 20 s rule3 cap.
        for _ in 0..(21 * 16_000 / CHUNK_SAMPLES as i32) {
            ep.observe(CHUNK_SAMPLES, false, true);
        }
        assert!(ep.fired(), "rule3 max-length endpoint");
    }

    /// reset clears all per-utterance endpointer state (B67 boundary).
    #[test]
    fn endpointer_reset_clears_utterance_state() {
        let mut ep = Endpointer::new(&test_args());
        ep.observe(CHUNK_SAMPLES, false, true);
        ep.observe(16_000 * 2, true, false);
        assert!(ep.fired());
        ep.reset();
        assert!(!ep.fired(), "fresh utterance after reset");
        assert!(!ep.has_speech);
    }

    /// model_dir prefers --model-dir, else falls back to --encoder's parent.
    #[test]
    fn model_dir_falls_back_to_encoder_parent() {
        let mut a = test_args();
        a.encoder = "/models/nemo35/encoder.int8.onnx".into();
        assert_eq!(model_dir(&a), PathBuf::from("/models/nemo35"));
        a.model_dir = "/explicit/dir".into();
        assert_eq!(model_dir(&a), PathBuf::from("/explicit/dir"));
    }
}
