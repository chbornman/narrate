//! silero-vad v5 in-process via ONNX Runtime — the §3.3 defended `ort`
//! exception's capture-path carve-out (RUNTIME §3.3; CAPTURE §6.2).
//!
//! Spike-verified numbers (docs/SPIKE-P6.3.md): 0.08 ms mean per
//! 512-sample chunk on the Tier-1 floor (25× under budget), onset error
//! +48 ms against CAPTURE §1's 250 ms combined budget.
//!
//! THE INTEGRATION TRAP (spike): silero v5's ONNX needs each 512-sample
//! window PREPENDED with the previous window's last 64 samples (input
//! shape `[1, 576]`). Without the context the model silently returns
//! probabilities ≈ 0 — no error, no warning, just a mic that never
//! detects speech. The official python wrapper hides this; we must not
//! forget it.
//!
//! The model (~2.3 MB, MIT — github.com/snakers4/silero-vad) is COMPILED
//! IN via include_bytes!: no download, no manifest entry, no license
//! gate; it exists wherever the binary does.

use ort::session::Session;
use ort::value::Tensor;

use crate::error::{ConnectorError, ConnectorResult};
use crate::transcriber::{AudioFrame, SAMPLE_RATE_HZ, StreamMs};
use crate::vad::{VadEvent, VadFrameResult, VoiceActivityDetector};

static SILERO_ONNX: &[u8] = include_bytes!("../assets/silero_vad.onnx");

const WINDOW: usize = 512; // 32 ms
const CONTEXT: usize = 64; // v5 contract: previous window's tail, prepended
/// Enter/exit thresholds per silero's reference hysteresis.
const ENTER: f32 = 0.5;
const EXIT: f32 = 0.35;
/// Below-EXIT windows before SpeechEnd: 15 × 32 ms = 480 ms hang — long
/// enough that intra-sentence pauses don't flap the gate, far below the
/// ASR's endpointing horizon (endpointing stays the ASR's, CAPTURE §6.3).
const HANG_WINDOWS: u32 = 15;
/// LSTM state tensor shape — the silero v5 model contract. WHY one
/// definition: the state allocation and the ort tensor shape must agree,
/// and a drift between them would surface only as a runtime ort error,
/// never a compile error.
const STATE_DIMS: [usize; 3] = [2, 1, 128];
/// Flat length of the state buffer, derived so it can never disagree
/// with the shape above.
const STATE_LEN: usize = STATE_DIMS[0] * STATE_DIMS[1] * STATE_DIMS[2];

pub struct SileroVad {
    session: Session,
    /// LSTM state, shape [`STATE_DIMS`]; zeroed on reset.
    state: Vec<f32>,
    ctx: [f32; CONTEXT],
    /// Tail samples not yet forming a full window.
    pending: Vec<f32>,
    /// Samples fully consumed through the model (the stream clock).
    consumed: u64,
    in_speech: bool,
    below_exit: u32,
    /// Stream ms of the last window that scored ≥ exit while in speech.
    last_speech_end: StreamMs,
    /// Hysteresis knobs — the product defaults are the consts above; the
    /// chunking-tuning harness (pp_voice_bench) sweeps them.
    enter: f32,
    exit: f32,
    hang_windows: u32,
}

impl SileroVad {
    pub fn new() -> ConnectorResult<Self> {
        Self::with_params(ENTER, EXIT, HANG_WINDOWS)
    }

    /// Explicit hysteresis parameters (the tuning seam; `new()` is the
    /// product posture).
    pub fn with_params(enter: f32, exit: f32, hang_windows: u32) -> ConnectorResult<Self> {
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_memory(SILERO_ONNX))
            .map_err(|e| ConnectorError::Decode(format!("silero session: {e}")))?;
        Ok(Self {
            session,
            state: vec![0.0; STATE_LEN],
            ctx: [0.0; CONTEXT],
            pending: Vec::with_capacity(WINDOW * 2),
            consumed: 0,
            in_speech: false,
            below_exit: 0,
            last_speech_end: 0,
            enter,
            exit,
            hang_windows,
        })
    }

    fn window_prob(&mut self, window: &[f32]) -> ConnectorResult<f32> {
        debug_assert_eq!(window.len(), WINDOW);
        let mut input = Vec::with_capacity(CONTEXT + WINDOW);
        input.extend_from_slice(&self.ctx);
        input.extend_from_slice(window);
        self.ctx.copy_from_slice(&window[WINDOW - CONTEXT..]);

        let x = Tensor::from_array(([1usize, CONTEXT + WINDOW], input))
            .map_err(|e| ConnectorError::Decode(format!("silero input: {e}")))?;
        let state = Tensor::from_array((STATE_DIMS, self.state.clone()))
            .map_err(|e| ConnectorError::Decode(format!("silero state: {e}")))?;
        let sr = Tensor::from_array(([] as [usize; 0], vec![i64::from(SAMPLE_RATE_HZ)]))
            .map_err(|e| ConnectorError::Decode(format!("silero sr: {e}")))?;
        let outputs = self
            .session
            .run(ort::inputs!["input" => x, "state" => state, "sr" => sr])
            .map_err(|e| ConnectorError::Decode(format!("silero run: {e}")))?;
        let (_, prob) = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| ConnectorError::Decode(format!("silero output: {e}")))?;
        let (_, next_state) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|e| ConnectorError::Decode(format!("silero stateN: {e}")))?;
        self.state.copy_from_slice(next_state);
        Ok(prob[0])
    }

    fn ms_at(&self, samples: u64) -> StreamMs {
        samples * 1000 / u64::from(SAMPLE_RATE_HZ)
    }
}

impl VoiceActivityDetector for SileroVad {
    fn process_frame(&mut self, frame: &AudioFrame) -> VadFrameResult {
        let mut events = Vec::new();
        self.pending.extend_from_slice(&frame.samples);
        while self.pending.len() >= WINDOW {
            let window: Vec<f32> = self.pending.drain(..WINDOW).collect();
            let window_start = self.consumed;
            self.consumed += WINDOW as u64;
            // A native failure mid-stream costs this window's verdict,
            // never the armed mic: treat as silence and keep going
            // (CAPTURE §6.6 disarms on STREAM failure, not VAD hiccups).
            let prob = self.window_prob(&window).unwrap_or(0.0);
            if self.in_speech {
                if prob >= self.exit {
                    self.below_exit = 0;
                    self.last_speech_end = self.ms_at(self.consumed);
                } else {
                    self.below_exit += 1;
                    if self.below_exit >= self.hang_windows {
                        self.in_speech = false;
                        events.push(VadEvent::SpeechEnd {
                            end: self.last_speech_end,
                        });
                    }
                }
            } else if prob >= self.enter {
                self.in_speech = true;
                self.below_exit = 0;
                self.last_speech_end = self.ms_at(self.consumed);
                // Estimated onset = the crossing window's START: detection
                // inherently lags true onset (spike: +48 ms with window-end
                // accounting; window-start accounting halves that), well
                // inside §6.2's ≤ 300 ms.
                events.push(VadEvent::SpeechStart {
                    onset: self.ms_at(window_start),
                });
            }
        }
        VadFrameResult {
            events,
            gate_open: self.in_speech,
        }
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.0);
        self.ctx = [0.0; CONTEXT];
        self.pending.clear();
        self.consumed = 0;
        self.in_speech = false;
        self.below_exit = 0;
        self.last_speech_end = 0;
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE_HZ
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal RIFF/PCM16 mono reader for the bundled fixture (sherpa's
    /// LibriSpeech-derived test clip, 16 kHz).
    fn fixture_speech() -> Vec<f32> {
        let bytes = include_bytes!("../assets/test-speech-16k.wav");
        let data_at = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("data chunk")
            + 8;
        let all: Vec<f32> = bytes[data_at..]
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
            .collect();
        // Trim the clip's OWN leading silence (≈0.4 s) so the test's
        // spliced 2.000 s boundary is the true onset — the same −35 dB
        // energy gate the spike's methodology used.
        let peak = all.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let gate = peak * 10f32.powf(-35.0 / 20.0);
        let first = all
            .windows(512)
            .position(|w| (w.iter().map(|v| v * v).sum::<f32>() / 512.0).sqrt() > gate)
            .unwrap_or(0);
        all[first..].to_vec()
    }

    fn feed(vad: &mut SileroVad, samples: &[f32], frame_len: usize) -> Vec<(StreamMs, bool)> {
        let mut out = Vec::new();
        for chunk in samples.chunks(frame_len) {
            let r = vad.process_frame(&AudioFrame {
                samples: chunk.to_vec(),
                captured_at: 0,
            });
            for e in r.events {
                match e {
                    VadEvent::SpeechStart { onset } => out.push((onset, true)),
                    VadEvent::SpeechEnd { end } => out.push((end, false)),
                }
            }
        }
        out
    }

    #[test]
    fn silence_never_fires_and_gate_stays_shut() {
        let mut vad = SileroVad::new().expect("session");
        let silence = vec![0.0f32; SAMPLE_RATE_HZ as usize * 2];
        let events = feed(&mut vad, &silence, 2560);
        assert!(events.is_empty(), "{events:?}");
    }

    /// Real speech after 2.000 s of digital silence: onset within the
    /// §6.2 ≤ 300 ms detection-lag budget of truth, and a SpeechEnd
    /// eventually follows. (The spike measured +48 ms on this fixture.)
    #[test]
    fn onset_lands_inside_the_capture_budget() {
        let mut vad = SileroVad::new().expect("session");
        let mut signal = vec![0.0f32; SAMPLE_RATE_HZ as usize * 2];
        signal.extend(fixture_speech());
        signal.extend(vec![0.0f32; SAMPLE_RATE_HZ as usize]); // trailing silence
        let events = feed(&mut vad, &signal, 2560);
        let (onset, is_start) = *events.first().expect("an onset fired");
        assert!(is_start);
        assert!(
            (1700..=2300).contains(&onset),
            "onset {onset} ms vs truth 2000 ms"
        );
        assert!(
            events.iter().any(|(_, s)| !s),
            "speech end fired after trailing silence: {events:?}"
        );
    }

    /// The v5 context-prepend regression guard: a detector that forgets
    /// the 64-sample context returns ~0 for everything — exactly the
    /// silence test passing AND the speech test failing. Re-running the
    /// speech check after reset() proves state fully rebuilds.
    #[test]
    fn reset_rebuilds_a_working_detector() {
        let mut vad = SileroVad::new().expect("session");
        let speech = fixture_speech();
        assert!(!feed(&mut vad, &speech, 2560).is_empty());
        vad.reset();
        assert!(
            !feed(&mut vad, &speech, 2560).is_empty(),
            "post-reset detector must still detect"
        );
    }
}
