//! Minimal HTTP/1.1 + WebSocket wire plumbing for **localhost backends**
//! (spec/RUNTIME.md §1.6: all inference endpoints bind 127.0.0.1; §3.2 the
//! P2 WebSocket contract; §8.1 health probes; §5.2 the download manager).
//!
//! Deliberately small and dependency-free: the peers are processes we
//! spawn (or the user's explicitly configured OAI endpoint) on loopback,
//! so a hand-rolled client buys exact control over the one semantic the
//! spec leans on hard — **cancellation by TCP disconnect** (RUNTIME §9:
//! llama-server frees a slot when a streaming client disconnects). A
//! [`Connection`] can be shut down from another thread via [`Disconnect`].
//!
//! Not in scope: TLS (no real weight download happens in P6.2 — the
//! manifest's `hf:` URLs need a TLS client, a P6.3 decision), HTTP/2,
//! proxies, redirects.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

/// Transport-level errors. Distinguishes **refused** (nothing listening —
/// a RUNTIME §8.1 restart trigger) from **timeout** (Busy-not-Lost
/// territory) from mid-stream loss.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("connection refused")]
    Refused,
    #[error("timed out")]
    TimedOut,
    #[error("connection lost: {0}")]
    ConnectionLost(std::io::Error),
    #[error("malformed response: {0}")]
    Malformed(String),
    #[error("io: {0}")]
    Io(std::io::Error),
}

impl HttpError {
    fn from_io(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::ConnectionRefused => HttpError::Refused,
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => HttpError::TimedOut,
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof => HttpError::ConnectionLost(e),
            _ => HttpError::Io(e),
        }
    }
}

/// One request header.
pub type Header = (String, String);

/// A parsed response head plus a body reader still attached to the socket.
pub struct Response {
    pub status: u16,
    pub headers: Vec<Header>,
    reader: BodyReader,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Read the entire body (Content-Length, chunked, or to-EOF).
    pub fn body(mut self) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match self.read_some(&mut buf)? {
                0 => return Ok(out),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    /// Read some body bytes; `Ok(0)` = body complete. Mid-body EOF before
    /// the declared length is `ConnectionLost` (a cut download — the
    /// resume path's signal, RUNTIME §5.2).
    pub fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, HttpError> {
        self.reader.read_some(buf)
    }

    /// Read one SSE/log line (used by the streaming chat path). `None` at
    /// end of body.
    pub fn read_line(&mut self) -> Result<Option<String>, HttpError> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match self.read_some(&mut byte)? {
                0 => {
                    return if line.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(String::from_utf8_lossy(&line).into_owned()))
                    };
                }
                _ => {
                    if byte[0] == b'\n' {
                        if line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                    }
                    line.push(byte[0]);
                }
            }
        }
    }
}

enum BodyReader {
    /// Remaining bytes of a Content-Length body.
    Sized {
        stream: BufReader<TcpStream>,
        remaining: u64,
    },
    /// Transfer-Encoding: chunked.
    Chunked {
        stream: BufReader<TcpStream>,
        in_chunk: u64,
        done: bool,
    },
    /// No length information: read to EOF (HTTP/1.0-style close-delimited).
    Eof(BufReader<TcpStream>),
}

impl BodyReader {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, HttpError> {
        match self {
            BodyReader::Sized { stream, remaining } => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let want = buf
                    .len()
                    .min(usize::try_from(*remaining).unwrap_or(buf.len()));
                let n = stream.read(&mut buf[..want]).map_err(HttpError::from_io)?;
                if n == 0 {
                    return Err(HttpError::ConnectionLost(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "body truncated",
                    )));
                }
                *remaining -= n as u64;
                Ok(n)
            }
            BodyReader::Chunked {
                stream,
                in_chunk,
                done,
            } => {
                if *done {
                    return Ok(0);
                }
                if *in_chunk == 0 {
                    let mut size_line = String::new();
                    stream
                        .read_line(&mut size_line)
                        .map_err(HttpError::from_io)?;
                    let size_line = size_line.trim();
                    if size_line.is_empty() {
                        // CRLF between chunks.
                        let mut again = String::new();
                        stream.read_line(&mut again).map_err(HttpError::from_io)?;
                        return self.parse_chunk_size(again.trim().to_owned(), buf);
                    }
                    return self.parse_chunk_size(size_line.to_owned(), buf);
                }
                let want = buf
                    .len()
                    .min(usize::try_from(*in_chunk).unwrap_or(buf.len()));
                let n = stream.read(&mut buf[..want]).map_err(HttpError::from_io)?;
                if n == 0 {
                    return Err(HttpError::ConnectionLost(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "chunk truncated",
                    )));
                }
                *in_chunk -= n as u64;
                Ok(n)
            }
            BodyReader::Eof(stream) => match stream.read(buf) {
                Ok(n) => Ok(n),
                Err(e) => Err(HttpError::from_io(e)),
            },
        }
    }

    fn parse_chunk_size(&mut self, line: String, buf: &mut [u8]) -> Result<usize, HttpError> {
        let BodyReader::Chunked { in_chunk, done, .. } = self else {
            unreachable!("parse_chunk_size on non-chunked reader");
        };
        let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| HttpError::Malformed(format!("bad chunk size {line:?}")))?;
        if size == 0 {
            *done = true;
            return Ok(0);
        }
        *in_chunk = size;
        self.read_some(buf)
    }
}

/// A handle that can sever the connection from any thread — the spec's
/// cancellation primitive (RUNTIME §9: cancel = HTTP disconnect; llama
/// frees the slot for streaming requests).
#[derive(Clone)]
pub struct Disconnect {
    stream: std::sync::Arc<TcpStream>,
}

impl Disconnect {
    pub fn disconnect(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// The underlying socket clone — the sherpa reader thread reads frames
    /// off it directly (blocking, no timeout; unblocked by `disconnect`).
    pub fn raw_stream(&self) -> &TcpStream {
        &self.stream
    }
}

/// Issue one request to `127.0.0.1:port` (or any explicit host:port) and
/// parse the response head. Returns the response plus a [`Disconnect`]
/// handle cloned off the live socket.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[Header],
    body: Option<&[u8]>,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<(Response, Disconnect), HttpError> {
    let stream = TcpStream::connect_timeout(&addr, connect_timeout).map_err(HttpError::from_io)?;
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(HttpError::from_io)?;
    stream
        .set_write_timeout(Some(read_timeout))
        .map_err(HttpError::from_io)?;
    stream.set_nodelay(true).ok();
    let disconnect = Disconnect {
        stream: std::sync::Arc::new(stream.try_clone().map_err(HttpError::from_io)?),
    };
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    let mut w = &stream;
    w.write_all(head.as_bytes()).map_err(HttpError::from_io)?;
    if let Some(b) = body {
        w.write_all(b).map_err(HttpError::from_io)?;
    }
    let response = read_response(stream)?;
    Ok((response, disconnect))
}

/// Parse a response head off an established stream (used by `request` and
/// by the WebSocket handshake).
fn read_response(stream: TcpStream) -> Result<Response, HttpError> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    let n = reader
        .read_line(&mut status_line)
        .map_err(HttpError::from_io)?;
    if n == 0 {
        return Err(HttpError::ConnectionLost(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "no status line",
        )));
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| HttpError::Malformed(format!("bad status line {status_line:?}")))?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(HttpError::from_io)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<u64>().ok());
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    let reader = if chunked {
        BodyReader::Chunked {
            stream: reader,
            in_chunk: 0,
            done: false,
        }
    } else if let Some(len) = content_length {
        BodyReader::Sized {
            stream: reader,
            remaining: len,
        }
    } else {
        BodyReader::Eof(reader)
    };
    Ok(Response {
        status,
        headers,
        reader,
    })
}

// ---------------------------------------------------------------------------
// WebSocket (RFC 6455) — exactly the client subset the P2 wire contract
// needs: handshake, masked binary/text frames out, unmasked frames in.
// ---------------------------------------------------------------------------

/// Cap on the handshake response head, which is read byte-by-byte until
/// `\r\n\r\n`. WHY: a non-WebSocket peer that streams an unbounded body
/// would otherwise keep the byte-at-a-time reader growing forever; any
/// legitimate 101 head is a few hundred bytes, so past this the peer is
/// malformed, not slow.
const MAX_HANDSHAKE_HEAD_BYTES: usize = 16 * 1024;

/// Per-frame increment of the client mask counter. WHY: this is the 32-bit
/// golden-ratio (Weyl) constant, chosen so successive masks form a long
/// non-repeating sequence; any odd constant would work. Masking satisfies
/// RFC 6455's client-must-mask rule only — it is not a security property
/// on loopback (see [`WsClient::mask_counter`]).
const MASK_COUNTER_STEP: u32 = 0x9E37_79B9;

/// Maximum accepted inbound frame payload. WHY: the 8-byte extended length
/// field is attacker/corruption-controlled, and `read_ws_frame` allocates
/// the full payload up front — without a ceiling a single corrupt header
/// could drive a giant allocation. P2 result frames are small JSON, so
/// anything near this bound is malformed, not legitimate.
const MAX_WS_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Inbound WebSocket message.
#[derive(Debug, Clone, PartialEq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Close,
    Ping(Vec<u8>),
    Pong,
}

/// A connected client WebSocket. Reads block up to the socket read
/// timeout; writes are masked per RFC 6455 (client→server frames MUST be
/// masked).
pub struct WsClient {
    stream: TcpStream,
    /// Deterministic mask counter — masking hides nothing on loopback;
    /// it satisfies the protocol, not a security property.
    mask_counter: u32,
}

impl WsClient {
    /// Open + upgrade. `key_seed` keeps the handshake deterministic for
    /// tests; the server's `Sec-WebSocket-Accept` is verified (RFC 6455
    /// §4.1 step 4).
    pub fn connect(
        addr: SocketAddr,
        path: &str,
        connect_timeout: Duration,
        read_timeout: Duration,
        key_seed: u64,
    ) -> Result<Self, HttpError> {
        let stream =
            TcpStream::connect_timeout(&addr, connect_timeout).map_err(HttpError::from_io)?;
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(HttpError::from_io)?;
        stream
            .set_write_timeout(Some(read_timeout))
            .map_err(HttpError::from_io)?;
        stream.set_nodelay(true).ok();
        let key_bytes: [u8; 16] = {
            let mut b = [0u8; 16];
            let (a, c) = (
                key_seed.to_le_bytes(),
                key_seed.rotate_left(31).to_le_bytes(),
            );
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&c);
            b
        };
        let key = base64_encode(&key_bytes);
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        let mut w = &stream;
        w.write_all(head.as_bytes()).map_err(HttpError::from_io)?;

        // Read the 101 head directly (no body framing past the upgrade).
        let mut head_bytes = Vec::new();
        let mut byte = [0u8; 1];
        let mut r = &stream;
        while !head_bytes.ends_with(b"\r\n\r\n") {
            match r.read(&mut byte).map_err(HttpError::from_io)? {
                0 => {
                    return Err(HttpError::ConnectionLost(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "handshake cut",
                    )));
                }
                _ => head_bytes.push(byte[0]),
            }
            if head_bytes.len() > MAX_HANDSHAKE_HEAD_BYTES {
                return Err(HttpError::Malformed("oversized handshake response".into()));
            }
        }
        let head_text = String::from_utf8_lossy(&head_bytes);
        if !head_text.starts_with("HTTP/1.1 101") {
            return Err(HttpError::Malformed(format!(
                "expected 101 Switching Protocols, got {:?}",
                head_text.lines().next().unwrap_or("")
            )));
        }
        let accept = head_text
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("sec-websocket-accept")
                    .then(|| v.trim().to_owned())
            })
            .ok_or_else(|| HttpError::Malformed("missing Sec-WebSocket-Accept".into()))?;
        if accept != ws_accept_key(&key) {
            return Err(HttpError::Malformed("Sec-WebSocket-Accept mismatch".into()));
        }
        Ok(Self {
            stream,
            mask_counter: 0,
        })
    }

    /// A disconnect handle for severing the socket from another thread.
    pub fn disconnect_handle(&self) -> Result<Disconnect, HttpError> {
        Ok(Disconnect {
            stream: std::sync::Arc::new(self.stream.try_clone().map_err(HttpError::from_io)?),
        })
    }

    pub fn send_binary(&mut self, payload: &[u8]) -> Result<(), HttpError> {
        self.send_frame(0x2, payload)
    }

    pub fn send_text(&mut self, payload: &str) -> Result<(), HttpError> {
        self.send_frame(0x1, payload.as_bytes())
    }

    pub fn send_close(&mut self) -> Result<(), HttpError> {
        self.send_frame(0x8, &[])
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), HttpError> {
        self.mask_counter = self.mask_counter.wrapping_add(MASK_COUNTER_STEP);
        let mask = self.mask_counter.to_le_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode); // FIN + opcode
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        (&self.stream).write_all(&frame).map_err(HttpError::from_io)
    }

    /// Read one message (coalescing nothing: the P2 server sends whole
    /// JSON results per frame). Pings are answered transparently.
    pub fn read_message(&mut self) -> Result<WsMessage, HttpError> {
        let (opcode, payload) = read_ws_frame(&self.stream)?;
        match opcode {
            0x1 => Ok(WsMessage::Text(
                String::from_utf8_lossy(&payload).into_owned(),
            )),
            0x2 => Ok(WsMessage::Binary(payload)),
            0x8 => Ok(WsMessage::Close),
            0x9 => {
                self.send_frame(0xA, &payload)?;
                Ok(WsMessage::Ping(payload))
            }
            0xA => Ok(WsMessage::Pong),
            other => Err(HttpError::Malformed(format!(
                "unsupported ws opcode {other:#x}"
            ))),
        }
    }
}

/// Read one (possibly masked) WebSocket frame off a stream — shared by the
/// client (server frames, unmasked) and the test stub server (client
/// frames, masked).
pub fn read_ws_frame(mut stream: &TcpStream) -> Result<(u8, Vec<u8>), HttpError> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).map_err(HttpError::from_io)?;
    let opcode = hdr[0] & 0x0F;
    let masked = hdr[1] & 0x80 != 0;
    let mut len = u64::from(hdr[1] & 0x7F);
    if len == 126 {
        let mut b = [0u8; 2];
        stream.read_exact(&mut b).map_err(HttpError::from_io)?;
        len = u64::from(u16::from_be_bytes(b));
    } else if len == 127 {
        let mut b = [0u8; 8];
        stream.read_exact(&mut b).map_err(HttpError::from_io)?;
        len = u64::from_be_bytes(b);
    }
    if len > MAX_WS_FRAME_BYTES {
        return Err(HttpError::Malformed(format!("oversized ws frame: {len}")));
    }
    let mask = if masked {
        let mut m = [0u8; 4];
        stream.read_exact(&mut m).map_err(HttpError::from_io)?;
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; len as usize];
    stream
        .read_exact(&mut payload)
        .map_err(HttpError::from_io)?;
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    Ok((opcode, payload))
}

/// `Sec-WebSocket-Accept` for a given key (RFC 6455 §4.2.2 step 5.4) —
/// used by the client to verify and by the stub server to answer.
pub fn ws_accept_key(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let digest = sha1(format!("{key}{GUID}").as_bytes());
    base64_encode(&digest)
}

/// SHA-1 (RFC 3174) — needed only for the WebSocket accept key, which RFC
/// 6455 fixes to SHA-1; not used for any integrity purpose (downloads pin
/// SHA-256 in photoproof-core).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard base64 (RFC 4648, padded) — WebSocket key/accept and the
/// OpenAI client's image data URLs.
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_rfc_3174_vectors() {
        // RFC 3174 test vectors.
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ws_accept_key_matches_rfc_6455_example() {
        // RFC 6455 §1.3 worked example.
        assert_eq!(
            ws_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
