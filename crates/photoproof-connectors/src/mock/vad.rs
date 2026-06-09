//! Scripted mock `VoiceActivityDetector`.
//!
//! Speech is scripted as stream-clock spans. A span's `SpeechStart` is
//! emitted while processing the frame that reaches `onset +
//! detection_delay_ms` — modeling silero's detection latency (CAPTURE
//! §6.2: ≤ 300 ms) — but the event's `onset` value is always the span
//! start (the *estimated onset*, not detection time). Fully deterministic;
//! no wall clock.

use crate::transcriber::{AudioFrame, StreamMs};
use crate::vad::{VadEvent, VadFrameResult, VoiceActivityDetector};

/// One scripted speech span on the stream clock, half-open `[onset, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSpan {
    pub onset: StreamMs,
    pub end: StreamMs,
}

pub struct MockVad {
    sample_rate: u32,
    spans: Vec<SpeechSpan>,
    detection_delay_ms: u64,
    pos: StreamMs,
    next_start: usize,
    next_end: usize,
}

impl MockVad {
    /// Panics (test bug) if spans are unordered, overlapping, or empty.
    pub fn new(sample_rate: u32, spans: Vec<SpeechSpan>) -> Self {
        let mut prev_end = 0;
        for span in &spans {
            assert!(span.onset < span.end, "MockVad: span {span:?} is empty");
            assert!(
                span.onset >= prev_end,
                "MockVad: spans must be sorted and non-overlapping ({span:?})"
            );
            prev_end = span.end;
        }
        Self {
            sample_rate,
            spans,
            detection_delay_ms: 0,
            pos: 0,
            next_start: 0,
            next_end: 0,
        }
    }

    /// Delay between a span boundary and the frame whose processing emits
    /// its event (simulated detection latency; default 0).
    #[must_use]
    pub fn with_detection_delay(mut self, delay_ms: u64) -> Self {
        self.detection_delay_ms = delay_ms;
        self
    }
}

impl VoiceActivityDetector for MockVad {
    fn process_frame(&mut self, frame: &AudioFrame) -> VadFrameResult {
        let frame_start = frame.captured_at;
        let frame_end = frame_start + frame.duration_ms(self.sample_rate);
        self.pos = self.pos.max(frame_end);

        // Emit boundary events whose detection point now falls inside the
        // processed audio, interleaved in stream-clock order.
        let mut events = Vec::new();
        loop {
            let start_due = self
                .spans
                .get(self.next_start)
                .map(|span| span.onset + self.detection_delay_ms)
                .filter(|&at| at < self.pos);
            let end_due = self
                .spans
                .get(self.next_end)
                .map(|span| span.end + self.detection_delay_ms)
                .filter(|&at| at < self.pos);
            match (start_due, end_due) {
                (Some(s), _) if end_due.is_none_or(|e| s <= e) => {
                    events.push(VadEvent::SpeechStart {
                        onset: self.spans[self.next_start].onset,
                    });
                    self.next_start += 1;
                }
                (_, Some(_)) => {
                    events.push(VadEvent::SpeechEnd {
                        end: self.spans[self.next_end].end,
                    });
                    self.next_end += 1;
                }
                _ => break,
            }
        }

        // Silence gate: ship the frame iff it overlaps any speech span.
        let gate_open = self
            .spans
            .iter()
            .any(|span| frame_start < span.end && frame_end > span.onset);

        VadFrameResult { events, gate_open }
    }

    fn reset(&mut self) {
        self.pos = 0;
        self.next_start = 0;
        self.next_end = 0;
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}
