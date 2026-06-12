//! ASR connector boundary (spec/RUNTIME.md §4, normative; consumed by
//! spec/CAPTURE.md §5–6).

use futures_core::stream::BoxStream;

use crate::error::ConnectorResult;

/// Milliseconds relative to the stream clock: 0 = the instant `stream()`
/// accepted its first audio frame. CAPTURE.md maps stream time to wall
/// time and to selection snapshots (VAD-onset binding, gap B1).
///
/// There is ONE stream clock: the capture-side frame clock of
/// [`AudioFrame::captured_at`]. Segment times share it (B72). The capture
/// gate withholds armed silence (CAPTURE §6.2), so a backend whose native
/// clock counts only the samples it RECEIVED runs behind this clock by the
/// cumulative withheld time; its connector must translate segment times
/// back to the capture positions of the frames it shipped (see `sherpa`'s
/// `ShipClock`) — the engine binds and associates on this clock and never
/// compensates.
pub type StreamMs = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    /// Stable id per utterance: partials and the final for one utterance
    /// share an `utterance_id`; the final replaces all partials.
    pub utterance_id: u64,
    pub kind: SegmentKind,
    pub text: String,
    /// The backend's own (token-derived) onset for this utterance, on the
    /// capture stream clock ([`StreamMs`]). CAPTURE §5.1 treats it as a
    /// cross-check and an association hint (B72) — never the binding
    /// authority; the capture-side VAD onset is.
    pub onset: StreamMs,
    /// End of speech covered by this segment so far.
    pub end: StreamMs,
    /// exp(mean token log-prob), in [0,1]. OPTIONAL: `None` when the
    /// backend exposes no token log-probs (sherpa-onnx `ys_probs` only
    /// recently landed — k2-fsa/sherpa-onnx#2897 — and has hotword quirks,
    /// #2937). Explicitly UNCALIBRATED: a score, not a probability; never
    /// compare values across model versions. Stored per event per the
    /// kernel when present. Partials may carry a provisional value.
    pub confidence: Option<f32>,
    /// BCP-47 of the recognized language, when the model reports it.
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Mutable hypothesis; UI may use it for the capture pulse, never stored.
    Partial,
    /// Immutable; becomes an annotation event. Audio for this utterance
    /// is discarded after finalization per the kernel.
    Final,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    /// f32 samples, mono, at `Transcriber::sample_rate()` — matches both
    /// CAPTURE's resampled feed and the P2 float32 wire format (RUNTIME
    /// §3.2); no i16 round-trip anywhere in the path.
    pub samples: Vec<f32>,
    pub captured_at: StreamMs,
}

impl AudioFrame {
    /// Duration of this frame in stream-clock milliseconds at `sample_rate`.
    pub fn duration_ms(&self, sample_rate: u32) -> StreamMs {
        debug_assert!(sample_rate > 0);
        (self.samples.len() as u64 * 1000) / u64::from(sample_rate)
    }
}

pub trait Transcriber: Send + Sync {
    /// Open one streaming session. Frames go in; segments come out.
    /// The output stream ends after the input closes and the last Final
    /// is emitted, or yields Err and ends on connection loss (the caller
    /// re-arms the mic only after the supervisor reports Ready again).
    ///
    /// Implementations MUST NOT re-emit or renumber segments
    /// (CAPTURE §6.2); all segment times are stream-clock ms offsets on
    /// the CAPTURE frame clock — see [`StreamMs`] for the translation
    /// obligation when the backend counts received samples.
    fn stream<'a>(
        &'a self,
        audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>>;

    /// Required input sample rate (Nemotron: 16_000).
    fn sample_rate(&self) -> u32;
    fn model_id(&self) -> &str;
}
