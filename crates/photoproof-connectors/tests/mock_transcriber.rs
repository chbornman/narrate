//! The scripted-timing mock Transcriber: segments emitted per script on the
//! stream clock (audio-driven, no wall clock), trailing finals flushed on
//! end-of-input, scripted connection loss, NotReady, determinism.

use std::task::{Context, Poll, Waker};

use photoproof_connectors::mock::{
    AudioFeed, MockTranscriber, ScriptEntry, ScriptedEvent, collect, frames_stream, silent_frame,
};
use photoproof_connectors::{
    ConnectorError, SegmentKind, StreamMs, Transcriber, TranscriptSegment,
};

const RATE: u32 = 16_000;

fn segment(
    utterance_id: u64,
    kind: SegmentKind,
    text: &str,
    onset: StreamMs,
    end: StreamMs,
) -> TranscriptSegment {
    TranscriptSegment {
        utterance_id,
        kind,
        text: text.to_owned(),
        onset,
        end,
        confidence: Some(0.91),
        language: Some("en".to_owned()),
    }
}

/// A script shaped like CAPTURE §6.5: partials then a final per utterance,
/// stable utterance_id, final replaces partials.
fn two_utterance_script() -> Vec<ScriptEntry> {
    vec![
        ScriptEntry {
            at: 700,
            event: ScriptedEvent::Segment(segment(1, SegmentKind::Partial, "lovely", 200, 600)),
        },
        ScriptEntry {
            at: 1500,
            event: ScriptedEvent::Segment(segment(
                1,
                SegmentKind::Final,
                "lovely gesture here",
                200,
                1400,
            )),
        },
        ScriptEntry {
            at: 2600,
            event: ScriptedEvent::Segment(segment(
                2,
                SegmentKind::Final,
                "but this one has better light",
                1900,
                2500,
            )),
        },
    ]
}

#[test]
fn scripted_segments_arrive_in_order_with_script_fields() {
    let transcriber = MockTranscriber::new("mock-asr", RATE).with_script(two_utterance_script());
    assert_eq!(transcriber.sample_rate(), RATE);
    assert_eq!(transcriber.model_id(), "mock-asr");

    // 3 s of audio in 100 ms frames covers every scripted offset.
    let frames = (0..30).map(|i| silent_frame(i * 100, 100, RATE)).collect();
    let stream = transcriber.stream(frames_stream(frames)).unwrap();
    let output: Vec<_> = pollster::block_on(collect(stream));

    let segments: Vec<_> = output.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(segments.len(), 3);
    assert_eq!(
        segments[0],
        segment(1, SegmentKind::Partial, "lovely", 200, 600)
    );
    assert_eq!(
        segments[1],
        segment(1, SegmentKind::Final, "lovely gesture here", 200, 1400)
    );
    // Partial and final share utterance_id; the next utterance gets a new one.
    assert_eq!(segments[0].utterance_id, segments[1].utterance_id);
    assert_eq!(segments[2].utterance_id, 2);
}

#[test]
fn segments_are_gated_on_consumed_audio_not_wall_clock() {
    let transcriber = MockTranscriber::new("mock-asr", RATE).with_script(two_utterance_script());
    let (feed, audio) = AudioFeed::new();
    let mut stream = transcriber.stream(audio).unwrap();
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    // No audio yet → nothing due, stream pends.
    assert!(stream.as_mut().poll_next(&mut cx).is_pending());

    // 600 ms of audio: still before the first scripted offset (700 ms).
    feed.push(silent_frame(0, 600, RATE));
    assert!(stream.as_mut().poll_next(&mut cx).is_pending());

    // Cross 700 ms → the partial (and only it) becomes due.
    feed.push(silent_frame(600, 200, RATE));
    match stream.as_mut().poll_next(&mut cx) {
        Poll::Ready(Some(Ok(seg))) => {
            assert_eq!((seg.utterance_id, seg.kind), (1, SegmentKind::Partial));
        }
        other => panic!("expected the scripted partial, got {other:?}"),
    }
    assert!(stream.as_mut().poll_next(&mut cx).is_pending());

    // Cross 1500 ms → the first final.
    feed.push(silent_frame(800, 800, RATE));
    match stream.as_mut().poll_next(&mut cx) {
        Poll::Ready(Some(Ok(seg))) => {
            assert_eq!((seg.utterance_id, seg.kind), (1, SegmentKind::Final));
        }
        other => panic!("expected the first final, got {other:?}"),
    }
    assert!(stream.as_mut().poll_next(&mut cx).is_pending());

    // end_stream() with the second final still pending → it flushes as a
    // trailing final (CAPTURE §6.4 drain), then the stream ends.
    feed.close();
    match stream.as_mut().poll_next(&mut cx) {
        Poll::Ready(Some(Ok(seg))) => {
            assert_eq!((seg.utterance_id, seg.kind), (2, SegmentKind::Final));
        }
        other => panic!("expected the trailing final, got {other:?}"),
    }
    assert!(matches!(
        stream.as_mut().poll_next(&mut cx),
        Poll::Ready(None)
    ));
}

#[test]
fn scripted_connection_loss_yields_err_then_ends() {
    let script = vec![
        ScriptEntry {
            at: 300,
            event: ScriptedEvent::Segment(segment(1, SegmentKind::Partial, "the fog", 100, 250)),
        },
        ScriptEntry {
            at: 500,
            event: ScriptedEvent::ConnectionLost,
        },
        // Unreachable: the stream ends at the connection loss.
        ScriptEntry {
            at: 600,
            event: ScriptedEvent::Segment(segment(1, SegmentKind::Final, "the fog bank", 100, 550)),
        },
    ];
    let transcriber = MockTranscriber::new("mock-asr", RATE).with_script(script);
    let frames = (0..10).map(|i| silent_frame(i * 100, 100, RATE)).collect();
    let stream = transcriber.stream(frames_stream(frames)).unwrap();
    let output = pollster::block_on(collect(stream));

    assert_eq!(output.len(), 2, "Err ends the stream: {output:?}");
    assert!(output[0].is_ok());
    assert!(matches!(output[1], Err(ConnectorError::ConnectionLost(_))));
}

#[test]
fn not_ready_fails_stream_open() {
    let transcriber = MockTranscriber::new("mock-asr", RATE)
        .with_script(two_utterance_script())
        .not_ready();
    let result = transcriber.stream(frames_stream(vec![]));
    assert!(matches!(result, Err(ConnectorError::NotReady(_))));
}

#[test]
fn empty_script_ends_after_input_closes() {
    let transcriber = MockTranscriber::new("mock-asr", RATE);
    let stream = transcriber
        .stream(frames_stream(vec![silent_frame(0, 100, RATE)]))
        .unwrap();
    let output = pollster::block_on(collect(stream));
    assert!(output.is_empty());
}

#[test]
fn identical_runs_are_identical() {
    let run = || {
        let transcriber =
            MockTranscriber::new("mock-asr", RATE).with_script(two_utterance_script());
        let frames = (0..30).map(|i| silent_frame(i * 100, 100, RATE)).collect();
        let stream = transcriber.stream(frames_stream(frames)).unwrap();
        pollster::block_on(collect(stream))
            .into_iter()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}
