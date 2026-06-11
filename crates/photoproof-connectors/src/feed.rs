//! Audio plumbing: a push-style frame source (the shape of CAPTURE §6.2's
//! `push_audio` / `end_stream`) and small stream helpers.
//!
//! Production plumbing, not mock behavior — the capture engine feeds its
//! Transcriber stream through an [`AudioFeed`] whether the transcriber is
//! the supervised sherpa client or a scripted mock (P6.2 obligation:
//! moved out of the mock namespace; `mock` re-exports for back-compat).

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures_core::stream::{BoxStream, Stream};

use crate::transcriber::{AudioFrame, StreamMs};

/// Push side of a test audio stream. `push` = CAPTURE's `push_audio`,
/// `close` = `end_stream`. Waker-correct, so it works under any executor
/// as well as under manual polling.
#[derive(Clone)]
pub struct AudioFeed {
    state: Arc<Mutex<FeedState>>,
}

#[derive(Default)]
struct FeedState {
    queue: VecDeque<AudioFrame>,
    closed: bool,
    waker: Option<Waker>,
}

impl AudioFeed {
    /// Create a feed plus the stream end to hand to `Transcriber::stream`.
    pub fn new() -> (Self, BoxStream<'static, AudioFrame>) {
        let state = Arc::new(Mutex::new(FeedState::default()));
        let feed = Self {
            state: Arc::clone(&state),
        };
        (feed, Box::pin(FeedStream { state }))
    }

    /// Push one frame (`push_audio`). Panics if the feed is closed —
    /// that is a test bug.
    pub fn push(&self, frame: AudioFrame) {
        let mut state = self.state.lock().unwrap();
        assert!(!state.closed, "AudioFeed::push after close");
        state.queue.push_back(frame);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Close the stream (`end_stream`): queued frames still drain, then
    /// the stream ends.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

struct FeedStream {
    state: Arc<Mutex<FeedState>>,
}

impl Stream for FeedStream {
    type Item = AudioFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<AudioFrame>> {
        let mut state = self.state.lock().unwrap();
        if let Some(frame) = state.queue.pop_front() {
            return Poll::Ready(Some(frame));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// A fixed sequence of frames as an already-closed stream.
pub fn frames_stream(frames: Vec<AudioFrame>) -> BoxStream<'static, AudioFrame> {
    let (feed, stream) = AudioFeed::new();
    for frame in frames {
        feed.push(frame);
    }
    feed.close();
    stream
}

/// A frame of silence: `duration_ms` of zero samples at `sample_rate`,
/// starting at stream position `captured_at`.
pub fn silent_frame(captured_at: StreamMs, duration_ms: u64, sample_rate: u32) -> AudioFrame {
    let samples = vec![0.0; (duration_ms * u64::from(sample_rate) / 1000) as usize];
    AudioFrame {
        samples,
        captured_at,
    }
}

/// Drain a stream to completion (drive with `pollster::block_on` or any
/// executor).
pub async fn collect<T>(mut stream: BoxStream<'_, T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Some(item) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        out.push(item);
    }
    out
}
