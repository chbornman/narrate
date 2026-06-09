//! Scripted-timing mock `Transcriber`.
//!
//! Segments are emitted at scripted **stream-clock** positions: an entry
//! becomes due once the consumed audio covers its `at` offset. There is no
//! wall clock anywhere — feeding the same frames always yields the same
//! segments at the same poll points, which is exactly what the capture
//! state-machine tests (BUILD-LOOP P6.1, binding gap B1) need.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::stream::{BoxStream, Stream};

use crate::error::{ConnectorError, ConnectorResult};
use crate::transcriber::{AudioFrame, StreamMs, Transcriber, TranscriptSegment};

/// One scripted occurrence on the stream clock.
#[derive(Debug, Clone)]
pub struct ScriptEntry {
    /// Stream-clock ms at which this entry becomes due: it is emitted once
    /// consumed audio reaches `at`, or unconditionally when the input
    /// closes (trailing finals after `end_stream`, CAPTURE §6.4).
    pub at: StreamMs,
    pub event: ScriptedEvent,
}

#[derive(Debug, Clone)]
pub enum ScriptedEvent {
    /// Yield this segment.
    Segment(TranscriptSegment),
    /// Yield `Err(ConnectorError::ConnectionLost)` and end the stream —
    /// the ASR process dying mid-utterance (CAPTURE §6.6, RUNTIME §13.2).
    ConnectionLost,
}

pub struct MockTranscriber {
    model_id: String,
    sample_rate: u32,
    script: Vec<ScriptEntry>,
    not_ready: bool,
}

impl MockTranscriber {
    pub fn new(model_id: impl Into<String>, sample_rate: u32) -> Self {
        Self {
            model_id: model_id.into(),
            sample_rate,
            script: Vec::new(),
            not_ready: false,
        }
    }

    /// Install the script (kept in `at` order; the sort is stable, so
    /// same-`at` entries emit in script order).
    #[must_use]
    pub fn with_script(mut self, mut script: Vec<ScriptEntry>) -> Self {
        script.sort_by_key(|entry| entry.at);
        self.script = script;
        self
    }

    /// Make `stream()` fail with `ConnectorError::NotReady` — the backing
    /// service is starting/restarting/Failed (supervision tests).
    #[must_use]
    pub fn not_ready(mut self) -> Self {
        self.not_ready = true;
        self
    }
}

impl Transcriber for MockTranscriber {
    fn stream<'a>(
        &'a self,
        audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
        if self.not_ready {
            return Err(ConnectorError::NotReady("mock transcriber"));
        }
        Ok(Box::pin(ScriptStream {
            audio,
            sample_rate: self.sample_rate,
            queue: self.script.clone().into(),
            pos: 0,
            input_done: false,
            finished: false,
        }))
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

struct ScriptStream<'a> {
    audio: BoxStream<'a, AudioFrame>,
    sample_rate: u32,
    queue: VecDeque<ScriptEntry>,
    /// Stream-clock position covered by consumed audio.
    pos: StreamMs,
    input_done: bool,
    finished: bool,
}

impl Stream for ScriptStream<'_> {
    type Item = ConnectorResult<TranscriptSegment>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.finished {
                return Poll::Ready(None);
            }
            // Emit every entry due at the current position; once the input
            // has closed, flush the remainder (trailing finals).
            let due = this
                .queue
                .front()
                .is_some_and(|entry| this.input_done || entry.at <= this.pos);
            if due {
                let entry = this.queue.pop_front().expect("front checked above");
                match entry.event {
                    ScriptedEvent::Segment(segment) => return Poll::Ready(Some(Ok(segment))),
                    ScriptedEvent::ConnectionLost => {
                        this.finished = true;
                        return Poll::Ready(Some(Err(ConnectorError::ConnectionLost(
                            std::io::Error::new(
                                std::io::ErrorKind::ConnectionReset,
                                "mock transcriber: scripted connection loss",
                            ),
                        ))));
                    }
                }
            }
            if this.input_done {
                // Queue is empty (a non-empty queue is always due once
                // input_done): the last Final has been emitted.
                this.finished = true;
                return Poll::Ready(None);
            }
            match this.audio.as_mut().poll_next(cx) {
                Poll::Ready(Some(frame)) => {
                    let end = frame.captured_at + frame.duration_ms(this.sample_rate);
                    this.pos = this.pos.max(end);
                }
                Poll::Ready(None) => this.input_done = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
