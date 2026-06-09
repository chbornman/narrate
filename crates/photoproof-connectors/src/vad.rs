//! Capture-side voice-activity-detection contract.
//!
//! spec/CAPTURE.md §6.2 and DECISIONS P2: an in-process silero-vad session
//! fronts the ASR on the cpal capture stream. It supplies exactly three
//! things — the authoritative speech onset for scope binding (CAPTURE §5),
//! silence gating (audio is not shipped to the ASR during silence), and the
//! "speaking" indicator affordance. The ASR keeps endpointing/segmentation
//! authority (CAPTURE §6.3); a VAD implementation never segments utterances.
//!
//! Neither RUNTIME §4 nor CAPTURE §6.2 gives a Rust signature for this
//! boundary; this trait is the minimal contract those sections require
//! (flagged in the P1.2 report). It is a push-mode, stateful, single-thread
//! component (it lives on the capture stream's thread), hence `&mut self`
//! and `Send` without `Sync`.

use crate::transcriber::{AudioFrame, StreamMs};

/// Events emitted by the detector, in stream-clock order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// Speech onset. `onset` is the *estimated onset* on the stream clock —
    /// not the detection time; detection MUST lag true onset by ≤ 300 ms
    /// (CAPTURE §6.2). Authoritative for scope binding: CAPTURE converts it
    /// to the capture clock and snapshots the write scope (CAPTURE §5.1;
    /// CAPTURE names this `SpeechStart { t_start_ms }`).
    SpeechStart { onset: StreamMs },
    /// End of the speech span that began at the preceding `SpeechStart`.
    /// With `SpeechStart`, this bounds the durable wall-clock VAD span in
    /// the event's capture payload (CAPTURE §6.5); it does NOT end an
    /// utterance — endpointing is the ASR's (CAPTURE §6.3).
    SpeechEnd { end: StreamMs },
}

/// Per-frame output of [`VoiceActivityDetector::process_frame`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VadFrameResult {
    /// Zero or more events whose detection point falls within the audio
    /// processed so far, in stream-clock order. Each event is emitted
    /// exactly once.
    pub events: Vec<VadEvent>,
    /// Silence gate: `true` iff this frame overlaps detected speech and so
    /// should be shipped to the ASR. `false` frames are withheld
    /// (CAPTURE §6.2 silence gating). Doubles as the "speaking" indicator
    /// affordance.
    pub gate_open: bool,
}

pub trait VoiceActivityDetector: Send {
    /// Process one capture-stream frame (mono f32 at `sample_rate()`),
    /// advancing the detector's stream clock by the frame's extent.
    fn process_frame(&mut self, frame: &AudioFrame) -> VadFrameResult;

    /// Reset all internal state (mic re-arm rebuilds the chain from
    /// scratch — CAPTURE §6.2 stream-failure handling).
    fn reset(&mut self);

    /// Required input sample rate (silero-vad: 16_000).
    fn sample_rate(&self) -> u32;
}
