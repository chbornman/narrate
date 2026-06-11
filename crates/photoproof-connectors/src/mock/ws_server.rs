//! Scripted stub WebSocket server — the P2 wire contract's test double
//! (spec/RUNTIME.md §3.2). Speaks the sherpa-onnx online server's shape:
//! accepts raw float32 binary frames, recognizes the `"Done"` text
//! message, and emits scripted result-JSON messages keyed on consumed
//! audio (stream-clock determinism, like `MockTranscriber`).

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use crate::http::{read_ws_frame, ws_accept_key};

/// One scripted server action.
#[derive(Debug, Clone)]
pub enum WsAction {
    /// Send this text message once consumed audio covers `at_ms` of
    /// stream time (or immediately after "Done" arrives).
    SendAt { at_ms: u64, json: String },
    /// Sever the connection once consumed audio covers `at_ms`
    /// (mid-utterance ASR death — CAPTURE §13.7 / RUNTIME §13.2).
    CutAt { at_ms: u64 },
}

struct WsState {
    script: Mutex<Vec<WsAction>>,
    /// Total f32 samples received (across binary frames).
    samples: Mutex<u64>,
    done_seen: Mutex<bool>,
    refuse: Mutex<bool>,
}

/// The server: one connection at a time (the contract is one stream per
/// armed session).
pub struct StubWsServer {
    addr: SocketAddr,
    state: Arc<WsState>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubWsServer {
    /// `sample_rate` converts received samples to stream-clock ms for the
    /// script's `at_ms` triggers.
    pub fn start(sample_rate: u32, script: Vec<WsAction>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub ws bind");
        let addr = listener.local_addr().expect("stub ws addr");
        let state = Arc::new(WsState {
            script: Mutex::new(script),
            samples: Mutex::new(0),
            done_seen: Mutex::new(false),
            refuse: Mutex::new(false),
        });
        let accept_state = state.clone();
        let handle = std::thread::Builder::new()
            .name("pp-stub-ws".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    if *accept_state.refuse.lock().unwrap() {
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }
                    let _ = serve(stream, &accept_state, sample_rate);
                    // One scripted session per server instance.
                    break;
                }
            })
            .expect("spawn stub ws server");
        Self {
            addr,
            state,
            handle: Some(handle),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Refuse (immediately close) any further connections — the
    /// not-Ready/dead-child shape.
    pub fn refuse_connections(&self) {
        *self.state.refuse.lock().unwrap() = true;
    }

    /// Total f32 samples the server received.
    pub fn samples_received(&self) -> u64 {
        *self.state.samples.lock().unwrap()
    }

    /// Whether the "Done" end-of-stream text message arrived.
    pub fn done_seen(&self) -> bool {
        *self.state.done_seen.lock().unwrap()
    }

    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StubWsServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // Unblock accept if nothing ever connected.
            let _ = TcpStream::connect(self.addr);
            let _ = h.join();
        }
    }
}

fn serve(stream: TcpStream, state: &Arc<WsState>, sample_rate: u32) -> std::io::Result<()> {
    // --- handshake -----------------------------------------------------------
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    let mut r = &stream;
    while !head.ends_with(b"\r\n\r\n") {
        if r.read(&mut byte)? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
        if head.len() > 16 * 1024 {
            return Ok(());
        }
    }
    let head_text = String::from_utf8_lossy(&head);
    let key = head_text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("sec-websocket-key")
                .then(|| v.trim().to_owned())
        })
        .unwrap_or_default();
    let accept = ws_accept_key(&key);
    let mut w = &stream;
    w.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;

    // --- scripted session ----------------------------------------------------
    loop {
        // Fire everything due at the current stream position.
        let pos_ms = {
            let samples = *state.samples.lock().unwrap();
            samples * 1000 / u64::from(sample_rate)
        };
        let done = *state.done_seen.lock().unwrap();
        loop {
            let next = {
                let script = state.script.lock().unwrap();
                script.first().cloned()
            };
            match next {
                Some(WsAction::SendAt { at_ms, json }) if done || at_ms <= pos_ms => {
                    state.script.lock().unwrap().remove(0);
                    send_text(&stream, &json)?;
                }
                Some(WsAction::CutAt { at_ms }) if done || at_ms <= pos_ms => {
                    let _ = stream.shutdown(Shutdown::Both);
                    return Ok(());
                }
                _ => break,
            }
        }
        if done && state.script.lock().unwrap().is_empty() {
            // Clean end-of-session: close frame then shutdown.
            let _ = send_close(&stream);
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(());
        }
        // Read the next client frame (audio, Done, or close).
        let Ok((opcode, payload)) = read_ws_frame(&stream) else {
            return Ok(());
        };
        match opcode {
            0x2 => {
                *state.samples.lock().unwrap() += (payload.len() / 4) as u64;
            }
            0x1 => {
                if String::from_utf8_lossy(&payload).trim() == "Done" {
                    *state.done_seen.lock().unwrap() = true;
                }
            }
            0x8 => {
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(());
            }
            _ => {}
        }
    }
}

fn send_text(mut stream: &TcpStream, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81); // FIN + text
    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

fn send_close(mut stream: &TcpStream) -> std::io::Result<()> {
    stream.write_all(&[0x88, 0x00])
}
