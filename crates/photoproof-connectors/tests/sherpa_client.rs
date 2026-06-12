//! The sherpa-onnx online WebSocket `Transcriber` client against the
//! scripted stub WS server (spec/RUNTIME.md §3.2 wire contract; packet
//! P6.2). Real loopback sockets with scripted results; every wait is
//! bounded.

use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use futures_core::stream::{BoxStream, Stream};
use photoproof_connectors::mock::{AudioFeed, StubWsServer, WsAction};
use photoproof_connectors::openai::EndpointCell;
use photoproof_connectors::sherpa::SherpaOnlineTranscriber;
use photoproof_connectors::transcriber::{AudioFrame, SegmentKind, Transcriber, TranscriptSegment};
use photoproof_connectors::{ConnectorError, ConnectorResult};

const SR: u32 = 16_000;

type SegStream<'a> = BoxStream<'a, ConnectorResult<TranscriptSegment>>;

fn poll_once(stream: &mut SegStream<'_>) -> Poll<Option<ConnectorResult<TranscriptSegment>>> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    Pin::new(stream).poll_next(&mut cx)
}

/// Poll until an item arrives or `timeout` elapses (bounded: the stub
/// server's behavior is scripted, so the wait is tight in practice).
fn next_within(
    stream: &mut SegStream<'_>,
    timeout: Duration,
) -> Option<Option<ConnectorResult<TranscriptSegment>>> {
    let deadline = Instant::now() + timeout;
    loop {
        match poll_once(stream) {
            Poll::Ready(item) => return Some(item),
            Poll::Pending => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn frame(at: u64, ms: u64) -> AudioFrame {
    AudioFrame {
        samples: vec![0.01; (u64::from(SR) * ms / 1000) as usize],
        captured_at: at,
    }
}

fn result_json(segment: u64, text: &str, start_s: f64, is_final: bool) -> String {
    format!(
        r#"{{"text":"{text}","tokens":[],"timestamps":[0.0,0.4],"segment":{segment},"start_time":{start_s},"is_final":{is_final}}}"#
    )
}

#[test]
fn frames_in_results_out_partials_then_final_mapped_onto_the_contract() {
    let server = StubWsServer::start(
        SR,
        vec![
            WsAction::SendAt {
                at_ms: 400,
                json: result_json(0, "the hand", 0.1, false),
            },
            WsAction::SendAt {
                at_ms: 800,
                json: result_json(0, "the hand is the picture", 0.1, true),
            },
        ],
    );
    let transcriber =
        SherpaOnlineTranscriber::new("nemotron-en-0.6b-int8", EndpointCell::fixed(server.addr()));
    let (feed, audio) = AudioFeed::new();
    let mut out = transcriber.stream(audio).expect("stream opens");

    // 500 ms of audio: the 400 ms partial becomes due.
    for i in 0..10 {
        feed.push(frame(i * 50, 50));
    }
    let partial = next_within(&mut out, Duration::from_secs(2))
        .expect("partial within bound")
        .expect("stream open")
        .expect("ok segment");
    assert_eq!(partial.kind, SegmentKind::Partial);
    assert_eq!(partial.utterance_id, 0, "segment → utterance_id");
    assert_eq!(partial.text, "the hand");
    assert_eq!(partial.onset, 100, "start_time 0.1 s → 100 ms");

    // Another 500 ms: the final becomes due.
    for i in 10..20 {
        feed.push(frame(i * 50, 50));
    }
    let fin = next_within(&mut out, Duration::from_secs(2))
        .expect("final within bound")
        .expect("stream open")
        .expect("ok segment");
    assert_eq!(fin.kind, SegmentKind::Final);
    assert_eq!(fin.text, "the hand is the picture");
    assert_eq!(fin.end, 100 + 400, "onset + last token timestamp");
    // The final fired at 800 ms of covered audio, so at least that much
    // reached the wire (later frames may still be in flight).
    assert!(
        server.samples_received() >= u64::from(SR) * 800 / 1000,
        "audio reached the wire"
    );
}

/// B72 clock translation: the engine's silence gate withholds armed
/// silence, so the server's sample-count clock falls behind the capture
/// clock by the withheld stretch. Results must come back on the CAPTURE
/// clock (the `Transcriber` contract), or onset binding drifts by the
/// cumulative armed silence — the pp_voice_bench misbinding's quiet-
/// session variant.
#[test]
fn results_after_withheld_silence_translate_back_to_the_capture_clock() {
    let server = StubWsServer::start(
        SR,
        vec![WsAction::SendAt {
            // Shipped clock: 1 s of pre-gap audio + 0.5 s after the gap.
            at_ms: 1_500,
            json: result_json(0, "after the quiet stretch", 1.2, true),
        }],
    );
    let transcriber =
        SherpaOnlineTranscriber::new("nemotron-en-0.6b-int8", EndpointCell::fixed(server.addr()));
    let (feed, audio) = AudioFeed::new();
    let mut out = transcriber.stream(audio).expect("stream opens");

    // 1 s ships (captured 0..1000), then a 30 s armed-silence stretch the
    // gate withholds entirely, then 1 s more (captured 31_000..32_000).
    for i in 0..20 {
        feed.push(frame(i * 50, 50));
    }
    for i in 0..20 {
        feed.push(frame(31_000 + i * 50, 50));
    }
    let fin = next_within(&mut out, Duration::from_secs(2))
        .expect("final within bound")
        .expect("stream open")
        .expect("ok segment");
    assert_eq!(
        fin.onset, 31_200,
        "shipped 1.2 s = 0.2 s into the post-gap run on the capture clock"
    );
    assert_eq!(
        fin.end,
        31_200 + 400,
        "token end translates through the same run"
    );
}

#[test]
fn input_close_sends_done_trailing_final_arrives_then_the_stream_ends() {
    let server = StubWsServer::start(
        SR,
        vec![WsAction::SendAt {
            at_ms: 60_000, // never reached by audio: flushes on "Done"
            json: result_json(1, "trailing final", 0.05, true),
        }],
    );
    let transcriber =
        SherpaOnlineTranscriber::new("nemotron-en-0.6b-int8", EndpointCell::fixed(server.addr()));
    let (feed, audio) = AudioFeed::new();
    let mut out = transcriber.stream(audio).expect("stream opens");
    feed.push(frame(0, 200));
    feed.close(); // CAPTURE §6.4 disarm: end_stream

    let fin = next_within(&mut out, Duration::from_secs(2))
        .expect("trailing final within bound")
        .expect("stream open")
        .expect("ok");
    assert_eq!(fin.text, "trailing final");
    assert_eq!(fin.kind, SegmentKind::Final);
    let end = next_within(&mut out, Duration::from_secs(2)).expect("end within bound");
    assert!(end.is_none(), "stream ends after the last Final");
    server.join();
    // The wire contract's end-of-stream marker, asserted at the SERVER.
    // (join consumed the server — re-create the assertion from state
    // captured before join is not possible; done_seen checked pre-join.)
}

#[test]
fn done_marker_reaches_the_server() {
    let server = StubWsServer::start(SR, vec![]);
    let transcriber =
        SherpaOnlineTranscriber::new("nemotron-en-0.6b-int8", EndpointCell::fixed(server.addr()));
    let (feed, audio) = AudioFeed::new();
    let mut out = transcriber.stream(audio).expect("stream opens");
    feed.push(frame(0, 100));
    feed.close();
    let _ = next_within(&mut out, Duration::from_secs(2)).expect("stream settles");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.done_seen() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        server.done_seen(),
        "\"Done\" text message sent at end-of-stream (§3.2)"
    );
}

#[test]
fn mid_session_cut_yields_one_connection_lost_then_ends() {
    let server = StubWsServer::start(SR, vec![WsAction::CutAt { at_ms: 300 }]);
    let transcriber =
        SherpaOnlineTranscriber::new("nemotron-en-0.6b-int8", EndpointCell::fixed(server.addr()));
    let (feed, audio) = AudioFeed::new();
    let mut out = transcriber.stream(audio).expect("stream opens");
    for i in 0..10 {
        feed.push(frame(i * 50, 50));
    }
    let item = next_within(&mut out, Duration::from_secs(2))
        .expect("error within bound")
        .expect("stream yields the error, not silent end");
    assert!(
        matches!(item, Err(ConnectorError::ConnectionLost(_))),
        "ASR death mid-session = Err(ConnectionLost), got {item:?}"
    );
    let end = next_within(&mut out, Duration::from_secs(2)).expect("end after error");
    assert!(end.is_none(), "stream ends after the fatal error");
}

#[test]
fn not_ready_at_open_when_no_endpoint_or_nothing_listening() {
    // No endpoint set: the supervisor hasn't reported Ready (§8.2 ports
    // live in memory; None = not Ready).
    let transcriber = SherpaOnlineTranscriber::new("m", EndpointCell::new());
    let (_feed, audio) = AudioFeed::new();
    assert!(matches!(
        transcriber.stream(audio),
        Err(ConnectorError::NotReady(_))
    ));

    // A dead port: handshake fails, same quiet NotReady (the engine's
    // arm() lands in Disarmed(error) — CAPTURE §6.6).
    let dead: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let transcriber = SherpaOnlineTranscriber::new("m", EndpointCell::fixed(dead));
    let (_feed, audio) = AudioFeed::new();
    assert!(matches!(
        transcriber.stream(audio),
        Err(ConnectorError::NotReady(_))
    ));
}

#[test]
fn sample_rate_is_the_nemotron_16k_contract() {
    let t = SherpaOnlineTranscriber::new("m", EndpointCell::new());
    assert_eq!(t.sample_rate(), 16_000);
    assert_eq!(t.model_id(), "m");
}
