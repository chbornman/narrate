//! P6.2 capture-engine obligations from the P6.1 review
//! (docs/BACKLOG.md): drain-deadline hardening against the REAL stream
//! (ready finals past the cap never mint; a never-pending stream cannot
//! defeat the cap), the §6.4 ArmedSpeaking guard pinned against the
//! guard-removal mutant, and the release-build partial-text cfg gate.

mod common;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use futures_core::stream::{BoxStream, Stream};
use photoproof_connectors::ConnectorResult;
use photoproof_connectors::mock::{
    MockTranscriber, MockVad, ScriptEntry, ScriptedEvent, SpeechSpan,
};
use photoproof_connectors::transcriber::{
    AudioFrame, SegmentKind, StreamMs, Transcriber, TranscriptSegment,
};
use photoproof_core::capture::{CaptureEngine, Clock, FakeClock, MicState};
use photoproof_core::{EventStore, SessionId};

use common::{DEVICE_ID, hash};

const SR: u32 = 16_000;
const WALL0: i64 = 1_780_000_000_000;

fn open_store(dir: &tempfile::TempDir) -> (EventStore, SessionId) {
    let store = EventStore::open(dir.path().join("journal.db")).expect("open store");
    let session = store
        .open_session(photoproof_core::SessionContext {
            app_version: "0.1.0-test".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .expect("open session");
    (store, session)
}

fn frame(at: StreamMs) -> AudioFrame {
    AudioFrame {
        samples: vec![0.0; (SR as u64 * 50 / 1000) as usize],
        captured_at: at,
    }
}

fn final_seg(utterance_id: u64, text: &str, onset: StreamMs, end: StreamMs) -> TranscriptSegment {
    TranscriptSegment {
        utterance_id,
        kind: SegmentKind::Final,
        text: text.into(),
        onset,
        end,
        confidence: None,
        language: None,
    }
}

// ---------------------------------------------------------------------------
// Obligation: "drain deadline only bites on Poll::Pending — ready finals
// past the cap still mint and a never-pending stream defeats it; harden
// against the real stream."
// ---------------------------------------------------------------------------

/// Pending until `release` flips, then one Final, then end — the real
/// wire's shape when a trailing final arrives LATE.
struct LateFinal {
    release: Arc<AtomicBool>,
}

struct LateFinalStream {
    release: Arc<AtomicBool>,
    emitted: bool,
}

impl Stream for LateFinalStream {
    type Item = ConnectorResult<TranscriptSegment>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.release.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        if this.emitted {
            return Poll::Ready(None);
        }
        this.emitted = true;
        Poll::Ready(Some(Ok(final_seg(1, "too late to mint", 100, 900))))
    }
}

impl Transcriber for LateFinal {
    fn stream<'a>(
        &'a self,
        _audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
        Ok(Box::pin(LateFinalStream {
            release: self.release.clone(),
            emitted: false,
        }))
    }
    fn sample_rate(&self) -> u32 {
        SR
    }
    fn model_id(&self) -> &str {
        "late-final"
    }
}

#[test]
fn ready_finals_past_the_drain_cap_do_not_mint() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let release = Arc::new(AtomicBool::new(false));
    let transcriber = LateFinal {
        release: release.clone(),
    };
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(1)]);
    engine.arm();
    for i in 0..20 {
        engine.push_audio(&store, frame(i * 50));
        clock.advance(50);
    }
    assert_eq!(engine.streaming_count(), 1);
    let committed = engine.disarm(&store); // stream still Pending
    assert!(committed.is_empty());
    assert!(engine.stream_open(), "drain window holds the stream open");

    // The 5 s window passes; only THEN does the final become ready.
    clock.advance(5_001);
    release.store(true, Ordering::SeqCst);
    let committed = engine.pump(&store);
    assert!(
        committed.is_empty(),
        "a final that is merely READY after the cap must not mint"
    );
    assert!(!engine.stream_open(), "drain finished at the cap");
    assert_eq!(engine.abandoned_count(), 1, "the utterance was abandoned");
    assert!(store.events_for_image(&hash(1)).unwrap().is_empty());
}

/// A stream that always has another Ready item (never Pending) — and
/// real time passes while it spins (the stream advances the fake clock
/// per poll, as the wall clock would).
struct NeverPending {
    clock: FakeClock,
}

struct NeverPendingStream {
    clock: FakeClock,
    n: u64,
}

impl Stream for NeverPendingStream {
    type Item = ConnectorResult<TranscriptSegment>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.n += 1;
        this.clock.advance(200); // each item costs real time on the wire
        Poll::Ready(Some(Ok(TranscriptSegment {
            utterance_id: this.n,
            kind: SegmentKind::Partial,
            text: format!("endless partial {}", this.n),
            onset: 100,
            end: 100 + this.n,
            confidence: None,
            language: None,
        })))
    }
}

impl Transcriber for NeverPending {
    fn stream<'a>(
        &'a self,
        _audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
        Ok(Box::pin(NeverPendingStream {
            clock: self.clock.clone(),
            n: 0,
        }))
    }
    fn sample_rate(&self) -> u32 {
        SR
    }
    fn model_id(&self) -> &str {
        "never-pending"
    }
}

#[test]
fn never_pending_stream_cannot_defeat_the_drain_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let transcriber = NeverPending {
        clock: clock.clone(),
    };
    let vad = MockVad::new(SR, vec![]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.arm();
    let before = clock.mono_ms();
    // Disarm starts the 5 s window; the stream yields Ready forever. The
    // hardened pump re-reads the clock per iteration, so this terminates
    // at the cap instead of spinning.
    engine.disarm(&store);
    assert!(!engine.stream_open(), "the cap closed the stream");
    assert!(
        clock.mono_ms() - before <= 6_000,
        "bounded by the drain window, not the stream's appetite"
    );
}

// ---------------------------------------------------------------------------
// Obligation: pin §6.4 "ArmedSpeaking holds while any utterance is in
// flight" — the guard-removal mutant (flipping to ArmedIdle on the FIRST
// final) must die here.
// ---------------------------------------------------------------------------

#[test]
fn armed_speaking_holds_while_any_utterance_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    // Two overlapping utterances: onsets 100 and 600; the FIRST finalizes
    // at stream 800 while the second is still in flight.
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 100,
                end: 500,
            },
            SpeechSpan {
                onset: 600,
                end: 1_300,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 800,
            event: ScriptedEvent::Segment(final_seg(1, "first utterance", 100, 500)),
        },
        ScriptEntry {
            // Due while speech is still gated through (span ends 1300).
            at: 1_250,
            event: ScriptedEvent::Segment(final_seg(2, "second utterance", 600, 1_200)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(7)]);
    engine.arm();

    let mut committed = Vec::new();
    let mut state_after_first_final = None;
    for i in 0..32 {
        let events = engine.push_audio(&store, frame(i * 50));
        if !events.is_empty() && committed.is_empty() {
            // The first final just landed; the second utterance (onset
            // 600) is still in flight.
            state_after_first_final = Some(engine.mic());
            assert_eq!(engine.streaming_count(), 1);
        }
        committed.extend(events);
        clock.advance(50);
    }
    assert_eq!(committed.len(), 2);
    assert_eq!(
        state_after_first_final,
        Some(MicState::ArmedSpeaking),
        "§6.4: ArmedSpeaking HOLDS while any utterance is in flight"
    );
    assert_eq!(
        engine.mic(),
        MicState::ArmedIdle,
        "…and settles only once the LAST final lands"
    );
}

// ---------------------------------------------------------------------------
// Obligation: cfg-gate partial TEXT out of the release debug-note ring
// (§6.5: partials are dev-build debug territory).
// ---------------------------------------------------------------------------

#[test]
fn partial_text_is_cfg_gated_out_of_the_release_debug_ring() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    let secret = "the quick brown partial";
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 400,
        event: ScriptedEvent::Segment(TranscriptSegment {
            utterance_id: 1,
            kind: SegmentKind::Partial,
            text: secret.into(),
            onset: 100,
            end: 400,
            confidence: None,
            language: None,
        }),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(2)]);
    engine.arm();
    for i in 0..12 {
        engine.push_audio(&store, frame(i * 50));
        clock.advance(50);
    }
    let ring = engine.debug_notes().join("\n");
    assert!(
        ring.contains("partial[1]"),
        "the ring keeps partial TIMING in every build: {ring}"
    );
    if cfg!(debug_assertions) {
        assert!(
            ring.contains(secret),
            "dev builds MAY show partial text (§6.5)"
        );
    } else {
        assert!(
            !ring.contains(secret),
            "release builds hold NO partial text in the ring"
        );
        assert!(ring.contains("<text elided in release>"));
    }
}
