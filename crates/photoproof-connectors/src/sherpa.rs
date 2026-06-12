//! The sherpa-onnx **online (streaming) WebSocket** `Transcriber` client —
//! spec/RUNTIME.md §3.2's wire contract, verbatim:
//!
//! - **Out**: raw float32 sample frames as binary WebSocket messages
//!   (16 kHz mono — the 16-bit language in sherpa's docs describes wave
//!   *files*, not the socket), then a `"Done"` text message at
//!   end-of-stream.
//! - **In**: result JSON per message — `text`, `tokens`, `timestamps`,
//!   `segment`, `start_time`, `is_final` — mapped onto the P1.2
//!   [`Transcriber`] contract: `segment` → `utterance_id`, `start_time`
//!   (s) → `onset` (stream ms), `is_final` → [`SegmentKind`].
//!
//! **Clock translation (B72)**: the wire carries raw samples only, so the
//! server's times count SHIPPED audio — but the capture engine's silence
//! gate withholds armed silence (CAPTURE §6.2), and every withheld stretch
//! makes the shipped clock fall further behind the capture clock. The
//! Transcriber contract requires segment times on the capture stream clock
//! (the clock of `AudioFrame::captured_at`), so this adapter records which
//! capture instant each shipped position corresponds to ([`ShipClock`])
//! and translates result times back before yielding them. Without this,
//! onset-proximity association (CAPTURE §5.1) silently degrades after the
//! first long armed silence.
//!
//! Readiness is answered at `stream()` open (CAPTURE §6.4 Arming confirms
//! ASR readiness with RUNTIME): a refused/unset endpoint returns
//! `NotReady`, and the engine lands in `Disarmed(error)` quietly. The
//! supervisor writes the live port into the [`EndpointCell`] in memory —
//! never config (RUNTIME §8.2).
//!
//! `confidence` maps from `ys_probs` (token log-probs) when the server
//! emits them — exp(mean), explicitly UNCALIBRATED — and is omitted
//! otherwise (sherpa-onnx#2897 landed recently and has quirks; the
//! contract makes the field optional).

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_core::stream::{BoxStream, Stream};

use crate::error::{ConnectorError, ConnectorResult};
use crate::http::{self, WsClient, WsMessage};
use crate::openai::{EndpointCell, SUPERVISED_CONNECT_TIMEOUT};
use crate::transcriber::{AudioFrame, SegmentKind, StreamMs, Transcriber, TranscriptSegment};

/// Input sample rate shipped to the server. WHY: the Nemotron streaming
/// model's contract (transcriber.rs: "Nemotron: 16_000") and the P2 wire
/// contract (RUNTIME §3.2: 16 kHz mono f32). Must agree with
/// silero's SAMPLE_RATE and pp-asr-server's accept_waveform argument.
const SAMPLE_RATE: u32 = 16_000;

/// Initial WebSocket handshake key seed, incremented per connection. WHY:
/// the value is arbitrary (ASCII "PERRO16A") and exists only so handshakes
/// are deterministic for tests; the key has no security role on loopback.
const INITIAL_WS_KEY_SEED: u64 = 0x5045_5252_4F31_3641;

/// Socket write timeout for shipping audio frames. WHY: a localhost ASR
/// that cannot drain a small binary frame within 10 s is dead, and this
/// timeout is what converts that into ConnectionLost so the engine disarms
/// (CAPTURE §6.6). Reads are unaffected — the reader thread clears its
/// read half, because an armed-but-silent mic can sit for minutes.
const WS_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SherpaOnlineTranscriber {
    endpoint: EndpointCell,
    model_id: String,
    sample_rate: u32,
    connect_timeout: Duration,
    /// Per-connection WS key seed (deterministic handshakes in tests).
    key_seed: AtomicU64,
}

impl SherpaOnlineTranscriber {
    pub fn new(model_id: impl Into<String>, endpoint: EndpointCell) -> Self {
        Self {
            endpoint,
            model_id: model_id.into(),
            sample_rate: SAMPLE_RATE,
            connect_timeout: SUPERVISED_CONNECT_TIMEOUT,
            key_seed: AtomicU64::new(INITIAL_WS_KEY_SEED),
        }
    }
}

impl Transcriber for SherpaOnlineTranscriber {
    /// Open one streaming session. `Err` here IS the readiness answer:
    /// the engine's `arm()` treats it as ASR-not-ready (§6.6).
    fn stream<'a>(
        &'a self,
        audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
        let Some(addr) = self.endpoint.get() else {
            return Err(ConnectorError::NotReady("asr endpoint not set"));
        };
        let seed = self.key_seed.fetch_add(1, Ordering::SeqCst);
        let ws = WsClient::connect(
            addr,
            "/",
            self.connect_timeout,
            // Writes use this timeout; the READER thread clears it below.
            WS_WRITE_TIMEOUT,
            seed,
        )
        .map_err(|_| ConnectorError::NotReady("asr websocket handshake failed"))?;

        let inbox = Inbox::default();
        let reader_inbox = inbox.clone();
        let reader_stream = ws
            .disconnect_handle()
            .map_err(|_| ConnectorError::NotReady("asr socket clone failed"))?;
        // Dedicated blocking reader: result JSON frames in, parsed and
        // queued; the poll side never blocks on the socket.
        let raw = reader_stream.clone();
        std::thread::Builder::new()
            .name("pp-sherpa-reader".into())
            .spawn(move || reader_loop(&raw, &reader_inbox))
            .map_err(|_| ConnectorError::NotReady("asr reader thread spawn failed"))?;

        Ok(Box::pin(SherpaStream {
            audio,
            ws,
            inbox,
            disconnect: reader_stream,
            ship_clock: ShipClock::new(self.sample_rate),
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

/// Reader→poller queue. Waker-correct so the stream works under real
/// executors as well as the engine's manual pump.
#[derive(Clone, Default)]
struct Inbox(Arc<Mutex<InboxState>>);

#[derive(Default)]
struct InboxState {
    queue: VecDeque<ConnectorResult<TranscriptSegment>>,
    /// Reader exited (socket closed or error already queued).
    closed: bool,
    waker: Option<Waker>,
}

impl Inbox {
    fn push(&self, item: ConnectorResult<TranscriptSegment>) {
        let mut s = self.0.lock().expect("inbox mutex");
        s.queue.push_back(item);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    fn close(&self) {
        let mut s = self.0.lock().expect("inbox mutex");
        s.closed = true;
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }
}

fn reader_loop(raw: &http::Disconnect, inbox: &Inbox) {
    // The Disconnect handle wraps a cloned TcpStream; reads block without
    // a timeout (silence is normal), unblocked by socket shutdown.
    let stream = raw.raw_stream();
    let _ = stream.set_read_timeout(None);
    loop {
        match read_one(stream) {
            Ok(WsMessage::Text(text)) => match parse_result(&text) {
                Ok(seg) => inbox.push(Ok(seg)),
                Err(e) => {
                    inbox.push(Err(e));
                    break;
                }
            },
            Ok(WsMessage::Close) => break,
            // Binary/Ping/Pong from the server: nothing in the contract —
            // ignored (the peer is a process we spawn on loopback).
            Ok(_) => continue,
            Err(_) => {
                // Mid-session socket loss = the ASR process died (§6.6):
                // one Err(ConnectionLost), then the stream ends. A loss
                // AFTER "Done" was acknowledged surfaces the same way; the
                // engine's drain treats both as fatal-or-finished.
                inbox.push(Err(ConnectorError::ConnectionLost(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "asr websocket closed unexpectedly",
                ))));
                break;
            }
        }
    }
    inbox.close();
}

fn read_one(stream: &std::net::TcpStream) -> Result<WsMessage, http::HttpError> {
    let (opcode, payload) = http::read_ws_frame(stream)?;
    Ok(match opcode {
        0x1 => WsMessage::Text(String::from_utf8_lossy(&payload).into_owned()),
        0x2 => WsMessage::Binary(payload),
        0x8 => WsMessage::Close,
        0x9 => WsMessage::Ping(payload),
        _ => WsMessage::Pong,
    })
}

/// §3.2 result JSON → the P1.2 segment contract.
fn parse_result(text: &str) -> ConnectorResult<TranscriptSegment> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| ConnectorError::Decode(format!("asr result JSON: {e}")))?;
    let segment = v["segment"]
        .as_u64()
        .ok_or_else(|| ConnectorError::Decode("asr result missing `segment`".into()))?;
    let start_time_s = v["start_time"].as_f64().unwrap_or(0.0);
    let onset = (start_time_s * 1000.0).round().max(0.0) as u64;
    // `timestamps` are token times relative to segment start (seconds).
    let last_token_s = v["timestamps"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|t| t.as_f64())
        .unwrap_or(0.0);
    let end = onset + (last_token_s * 1000.0).round().max(0.0) as u64;
    // exp(mean token log-prob) when the export emits ys_probs; omitted
    // otherwise. Uncalibrated by contract.
    let confidence = v["ys_probs"].as_array().filter(|a| !a.is_empty()).map(|a| {
        let mean = a.iter().filter_map(|p| p.as_f64()).sum::<f64>() / a.len() as f64;
        (mean.exp() as f32).clamp(0.0, 1.0)
    });
    Ok(TranscriptSegment {
        utterance_id: segment,
        kind: if v["is_final"].as_bool().unwrap_or(false) {
            SegmentKind::Final
        } else {
            SegmentKind::Partial
        },
        text: v["text"].as_str().unwrap_or("").to_owned(),
        onset,
        end,
        confidence,
        language: v["lang"].as_str().map(str::to_owned),
    })
}

/// One contiguous run of shipped audio: the capture-clock instant at which
/// the run's first shipped sample was captured.
struct ShipRun {
    /// Shipped-sample position (samples sent to the server before this run).
    shipped_samples: u64,
    /// `AudioFrame::captured_at` of the run's first frame.
    captured_at: StreamMs,
}

/// Shipped-clock → capture-clock translation (module docs: the server
/// counts received samples; the capture gate withholds silence, so the two
/// clocks diverge by the cumulative withheld time). Runs grow while frames
/// arrive back-to-back on the capture clock; a gap starts a new run.
struct ShipClock {
    sample_rate: u32,
    /// Total samples shipped so far.
    shipped_samples: u64,
    runs: Vec<ShipRun>,
}

impl ShipClock {
    fn new(sample_rate: u32) -> Self {
        debug_assert!(sample_rate > 0);
        Self {
            sample_rate,
            shipped_samples: 0,
            runs: Vec::new(),
        }
    }

    fn run_start_ms(&self, run: &ShipRun) -> StreamMs {
        run.shipped_samples * 1000 / u64::from(self.sample_rate)
    }

    /// Record one frame about to ship. A frame contiguous with the open
    /// run extends it; anything else (the first frame, or a frame after a
    /// withheld stretch) starts a new run at its `captured_at`.
    fn record(&mut self, frame: &AudioFrame) {
        let contiguous = self.runs.last().is_some_and(|run| {
            let run_elapsed =
                (self.shipped_samples - run.shipped_samples) * 1000 / u64::from(self.sample_rate);
            run.captured_at + run_elapsed == frame.captured_at
        });
        if !contiguous {
            self.runs.push(ShipRun {
                shipped_samples: self.shipped_samples,
                captured_at: frame.captured_at,
            });
        }
        self.shipped_samples += frame.samples.len() as u64;
    }

    /// Shipped-clock ms → capture-clock ms: find the run containing the
    /// position and add the run's capture anchor back.
    fn to_capture_ms(&self, shipped_ms: StreamMs) -> StreamMs {
        let i = self
            .runs
            .partition_point(|run| self.run_start_ms(run) <= shipped_ms);
        match i.checked_sub(1).and_then(|i| self.runs.get(i)) {
            Some(run) => run.captured_at + (shipped_ms - self.run_start_ms(run)),
            // A result before any audio shipped: nothing to anchor to,
            // pass the position through unchanged.
            None => shipped_ms,
        }
    }

    /// Translate a parsed segment's times onto the capture clock.
    fn translate(&self, mut seg: TranscriptSegment) -> TranscriptSegment {
        seg.onset = self.to_capture_ms(seg.onset);
        // Translate the end through the same map; clamp so a degenerate
        // mapping can never produce end < onset.
        seg.end = self.to_capture_ms(seg.end).max(seg.onset);
        seg
    }
}

struct SherpaStream<'a> {
    audio: BoxStream<'a, AudioFrame>,
    ws: WsClient,
    inbox: Inbox,
    disconnect: http::Disconnect,
    ship_clock: ShipClock,
    input_done: bool,
    finished: bool,
}

impl Drop for SherpaStream<'_> {
    fn drop(&mut self) {
        // Sever the socket so the reader thread unblocks and exits; the
        // engine closes the stream entirely on disarm — never
        // paused-but-open (CAPTURE §6.4).
        self.disconnect.disconnect();
    }
}

impl Stream for SherpaStream<'_> {
    type Item = ConnectorResult<TranscriptSegment>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        // 1. Ship every available audio frame; input close sends "Done"
        //    exactly once (§3.2 end-of-stream).
        while !this.input_done {
            match this.audio.as_mut().poll_next(cx) {
                Poll::Ready(Some(frame)) => {
                    // Record BEFORE shipping: the server's clock advances by
                    // exactly these samples, and any result it produces is
                    // translated through this map on delivery below.
                    this.ship_clock.record(&frame);
                    let mut bytes = Vec::with_capacity(frame.samples.len() * 4);
                    for s in &frame.samples {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    if this.ws.send_binary(&bytes).is_err() {
                        this.finished = true;
                        return Poll::Ready(Some(Err(ConnectorError::ConnectionLost(
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "asr websocket write failed",
                            ),
                        ))));
                    }
                }
                Poll::Ready(None) => {
                    this.input_done = true;
                    if this.ws.send_text("Done").is_err() {
                        this.finished = true;
                        return Poll::Ready(Some(Err(ConnectorError::ConnectionLost(
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "asr websocket Done failed",
                            ),
                        ))));
                    }
                }
                Poll::Pending => break,
            }
        }
        // 2. Deliver whatever the reader has queued, with segment times
        //    translated from the server's shipped clock onto the capture
        //    clock (module docs). The map only ever covers MORE audio than
        //    the result that prompted it, so the lookup is always anchored.
        let mut s = this.inbox.0.lock().expect("inbox mutex");
        if let Some(item) = s.queue.pop_front() {
            if item.is_err() {
                this.finished = true;
            }
            drop(s);
            return Poll::Ready(Some(item.map(|seg| this.ship_clock.translate(seg))));
        }
        if s.closed {
            drop(s);
            this.finished = true;
            return Poll::Ready(None);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the P2 wire contract (RUNTIME §3.2: 16 kHz mono f32). The
    /// server side (pp-asr-server) and the in-process VAD (silero) carry
    /// the same value; a change here is a cross-crate protocol change,
    /// not a local tweak.
    #[test]
    fn sample_rate_is_the_p2_wire_contract() {
        let t = SherpaOnlineTranscriber::new("nemotron", EndpointCell::new());
        assert_eq!(t.sample_rate(), 16_000);
    }

    #[test]
    fn parse_maps_the_3_2_wire_fields_onto_the_contract() {
        let seg = parse_result(
            r#"{"text":"lovely gesture","tokens":["love","ly"],
                "timestamps":[0.0,0.48],"segment":3,"start_time":12.4,
                "is_final":true}"#,
        )
        .unwrap();
        assert_eq!(seg.utterance_id, 3, "segment → utterance_id");
        assert_eq!(seg.kind, SegmentKind::Final);
        assert_eq!(seg.text, "lovely gesture");
        assert_eq!(seg.onset, 12_400, "start_time s → onset ms");
        assert_eq!(seg.end, 12_880, "onset + last token timestamp");
        assert_eq!(seg.confidence, None, "no ys_probs → omitted");
    }

    #[test]
    fn parse_confidence_is_exp_mean_log_prob_when_present() {
        let seg = parse_result(
            r#"{"text":"x","timestamps":[],"segment":0,"start_time":0,
                "is_final":false,"ys_probs":[-0.1,-0.3]}"#,
        )
        .unwrap();
        let expected = ((-0.2f64).exp()) as f32;
        assert!((seg.confidence.unwrap() - expected).abs() < 1e-6);
        assert_eq!(seg.kind, SegmentKind::Partial);
    }

    #[test]
    fn parse_rejects_results_without_a_segment_id() {
        assert!(matches!(
            parse_result(r#"{"text":"x"}"#),
            Err(ConnectorError::Decode(_))
        ));
    }

    fn frame(captured_at: u64, ms: u64) -> AudioFrame {
        AudioFrame {
            samples: vec![0.0; (16 * ms) as usize], // 16 kHz
            captured_at,
        }
    }

    #[test]
    fn ship_clock_is_identity_while_audio_ships_contiguously_from_zero() {
        let mut c = ShipClock::new(16_000);
        for i in 0..10 {
            c.record(&frame(i * 100, 100));
        }
        assert_eq!(c.to_capture_ms(0), 0);
        assert_eq!(c.to_capture_ms(450), 450);
        assert_eq!(c.to_capture_ms(999), 999);
    }

    #[test]
    fn ship_clock_restores_the_pre_speech_silence_never_shipped() {
        // Nothing ships before the first gate-open frame: the first
        // shipped sample was captured 30 s after arm.
        let mut c = ShipClock::new(16_000);
        c.record(&frame(30_000, 100));
        c.record(&frame(30_100, 100));
        assert_eq!(c.to_capture_ms(0), 30_000);
        assert_eq!(c.to_capture_ms(150), 30_150);
    }

    #[test]
    fn ship_clock_restores_withheld_silence_between_runs() {
        let mut c = ShipClock::new(16_000);
        // 1 s ships, then the gate withholds 57 s, then 1 s more ships.
        for i in 0..10 {
            c.record(&frame(i * 100, 100));
        }
        for i in 0..10 {
            c.record(&frame(58_000 + i * 100, 100));
        }
        // Positions inside the first run are untouched...
        assert_eq!(c.to_capture_ms(800), 800);
        // ...and positions in the second run regain the withheld 57 s.
        assert_eq!(c.to_capture_ms(1_000), 58_000);
        assert_eq!(c.to_capture_ms(1_700), 58_700);
    }

    #[test]
    fn ship_clock_translate_keeps_end_at_least_at_onset() {
        let mut c = ShipClock::new(16_000);
        c.record(&frame(5_000, 100));
        let seg = c.translate(TranscriptSegment {
            utterance_id: 0,
            kind: SegmentKind::Final,
            text: "x".into(),
            onset: 50,
            end: 50,
            confidence: None,
            language: None,
        });
        assert_eq!(seg.onset, 5_050);
        assert_eq!(seg.end, 5_050);
    }
}
