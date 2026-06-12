//! The OpenAI-compatible `LanguageModel` client (spec/RUNTIME.md §3.1,
//! §4.4) — one client, two roles:
//!
//! - the **supervised local backend**: llama-server's OAI surface on a
//!   supervisor-allocated localhost port (the port arrives in memory via
//!   [`EndpointCell`], never via config — §8.2);
//! - the **external escape hatch** (`[llm.openai-compatible]`): any
//!   OAI-speaking endpoint, key material resolved through the keychain
//!   seam at call time (§4.4).
//!
//! Endpoints used: `/v1/chat/completions` (completion, caption via image
//! content parts, JSON-schema `response_format` passthrough) and
//! `/health` (§8.1 readiness/liveness).
//!
//! Transport notes, all spec-load-bearing:
//! - **Streaming requests are the only cancellable requests** (llama only
//!   detects disconnect mid-stream — §9); the scheduler enforces
//!   stream-on-background by type, this client just honors the flag.
//! - IO is **blocking** inside the `async fn` trait surface: callers are
//!   the pass scheduler's worker threads (photoproof-core), which own
//!   per-request threads; the trait shape is P1.2's contract, not an
//!   executor promise.
//! - The [`InFlightGauge`] and [`LostReports`] feed the supervisor's
//!   Busy-is-not-Lost rule (§8.1): a slow health probe with requests in
//!   flight is Busy; an in-flight `ConnectionLost` triggers an immediate
//!   check.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::embedder::DecodedImage;
use crate::error::{ConnectorError, ConnectorResult};
use crate::http::{self, Disconnect, Header, HttpError};
use crate::llm::{ChatRequest, ChatResponse, LanguageModel, Role};

/// Connect timeout for the supervised local backend. WHY: the peer is a
/// child process we spawned on loopback, so a connect that takes anywhere
/// near 2 s is already a liveness signal, not network latency. Expiry
/// feeds the supervisor's Busy-vs-Lost reasoning (RUNTIME §8.1). This is
/// the LLM knob only — the ASR handshake timeout is sherpa.rs's own
/// constant, because the two gate different contracts (§8.1 liveness here,
/// CAPTURE §6.6 arm readiness there) and must stay independently tunable.
/// Tuning happens through `with_timeouts()`, the deliberate programmatic
/// seam.
const SUPERVISED_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Connect timeout for a user-configured external OAI endpoint. WHY:
/// deliberately looser than [`SUPERVISED_CONNECT_TIMEOUT`] — an external
/// endpoint may sit across a LAN or VPN, so it gets more connect headroom.
const EXTERNAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Read timeout for chat completions, both supervised and external. WHY:
/// it must outlast the slowest legitimate background completion on the
/// Tier-1 floor, because expiry surfaces as `ConnectorError::Timeout` and
/// feeds the supervisor's Busy-not-Lost reasoning (RUNTIME §8.1) — a value
/// too low would misreport a healthy-but-loaded backend.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Token budget for the synthetic `caption_image` chat request. WHY:
/// captions are retrieval fuel, never user-facing prose (kernel posture),
/// so this bounds background-lane occupancy rather than quality; tuning it
/// is a spec decision, not a user preference (no RUNTIME §4.4 key).
const CAPTION_MAX_TOKENS: u32 = 256;

/// Sampling temperature for the caption request. WHY: low-but-nonzero
/// keeps captions factual and stable for embedding while avoiding the
/// repetition pathologies of pure greedy decoding.
const CAPTION_TEMPERATURE: f32 = 0.2;

/// Requests currently in flight against this backend. Shared with the
/// supervisor: health-probe timeouts while `count() > 0` mean **Busy, not
/// Lost** (RUNTIME §8.1).
#[derive(Clone, Default)]
pub struct InFlightGauge(Arc<AtomicU32>);

impl InFlightGauge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }

    /// RAII in-flight marker; public so the supervisor's tests (and any
    /// non-HTTP request path) can drive the Busy-not-Lost gauge.
    pub fn enter(&self) -> InFlightGuard {
        self.0.fetch_add(1, Ordering::SeqCst);
        InFlightGuard(self.0.clone())
    }
}

pub struct InFlightGuard(Arc<AtomicU32>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Mid-call connection losses reported by the client; the supervisor
/// drains this to trigger an **immediate** liveness check (§8.1).
#[derive(Clone, Default)]
pub struct LostReports(Arc<AtomicU32>);

impl LostReports {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Take all pending reports (returns the count, resets to zero).
    pub fn drain(&self) -> u32 {
        self.0.swap(0, Ordering::SeqCst)
    }
}

/// Late-bound backend address: the supervisor writes the allocated port
/// here after each (re)spawn — ports live in memory only, never in config
/// (RUNTIME §8.2). `None` = backend not Ready.
#[derive(Clone, Default)]
pub struct EndpointCell(Arc<Mutex<Option<SocketAddr>>>);

impl EndpointCell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fixed(addr: SocketAddr) -> Self {
        let cell = Self::new();
        cell.set(Some(addr));
        cell
    }

    pub fn set(&self, addr: Option<SocketAddr>) {
        *self.0.lock().expect("endpoint cell mutex") = addr;
    }

    pub fn get(&self) -> Option<SocketAddr> {
        *self.0.lock().expect("endpoint cell mutex")
    }
}

/// `/health` outcome, in the supervisor's vocabulary (RUNTIME §8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// 200.
    Ready,
    /// 503 — llama-server still loading the model.
    Loading,
    /// Connection refused: nothing listening (a restart trigger).
    Refused,
    /// Probe timed out (Busy-not-Lost territory while requests are in
    /// flight).
    TimedOut,
    Error(String),
}

pub struct OpenAiCompatClient {
    endpoint: EndpointCell,
    /// Path prefix up to the OAI namespace, e.g. "" or "/v1"-stripped
    /// base; requests append `/v1/...`.
    path_prefix: String,
    model: String,
    api_key: Option<String>,
    gauge: InFlightGauge,
    lost: LostReports,
    connect_timeout: Duration,
    read_timeout: Duration,
}

impl OpenAiCompatClient {
    /// Supervised-local construction: address late-bound via the cell.
    pub fn supervised(model: impl Into<String>, endpoint: EndpointCell) -> Self {
        Self {
            endpoint,
            path_prefix: String::new(),
            model: model.into(),
            api_key: None,
            gauge: InFlightGauge::new(),
            lost: LostReports::new(),
            connect_timeout: SUPERVISED_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
        }
    }

    /// External-endpoint construction from `[llm.openai-compatible]`
    /// (`base_url` like `http://127.0.0.1:11434/v1`; the `/v1` suffix is
    /// normalized away; key material resolved by the CALLER through the
    /// keychain seam — this type never sees a `keychain:` reference).
    pub fn external(
        base_url: &str,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> ConnectorResult<Self> {
        let (addr, prefix) = parse_base_url(base_url)?;
        Ok(Self {
            endpoint: EndpointCell::fixed(addr),
            path_prefix: prefix,
            model: model.into(),
            api_key,
            gauge: InFlightGauge::new(),
            lost: LostReports::new(),
            connect_timeout: EXTERNAL_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
        })
    }

    #[must_use]
    pub fn with_timeouts(mut self, connect: Duration, read: Duration) -> Self {
        self.connect_timeout = connect;
        self.read_timeout = read;
        self
    }

    /// The gauge the supervisor shares for Busy-not-Lost (§8.1).
    pub fn gauge(&self) -> InFlightGauge {
        self.gauge.clone()
    }

    /// The mid-call loss reports the supervisor drains (§8.1).
    pub fn lost_reports(&self) -> LostReports {
        self.lost.clone()
    }

    /// `GET /health` (§8.1). Uses a short read timeout supplied by the
    /// prober — liveness probes must not wait out a long completion.
    pub fn health(&self, timeout: Duration) -> HealthStatus {
        let Some(addr) = self.endpoint.get() else {
            return HealthStatus::Refused;
        };
        let path = format!("{}/health", self.path_prefix);
        match http::request(addr, "GET", &path, &[], None, timeout, timeout) {
            Ok((resp, _)) => match resp.status {
                200 => HealthStatus::Ready,
                503 => HealthStatus::Loading,
                other => HealthStatus::Error(format!("health returned {other}")),
            },
            Err(HttpError::Refused) => HealthStatus::Refused,
            Err(HttpError::TimedOut) => HealthStatus::TimedOut,
            Err(e) => HealthStatus::Error(e.to_string()),
        }
    }

    /// One chat completion over the explicit transport (`stream` chosen by
    /// the scheduler: background lane MUST stream — §9). `on_open` receives
    /// the disconnect handle the moment the socket exists, so the scheduler
    /// can cancel mid-flight from another thread.
    pub fn complete_transport(
        &self,
        req: &ChatRequest,
        stream: bool,
        on_open: &mut dyn FnMut(Disconnect),
    ) -> ConnectorResult<ChatResponse> {
        let Some(addr) = self.endpoint.get() else {
            return Err(ConnectorError::NotReady("llm endpoint not set"));
        };
        let _guard = self.gauge.enter();
        let body = self.chat_body(req, stream, None);
        let mut headers: Vec<Header> = vec![("Content-Type".into(), "application/json".into())];
        if let Some(key) = &self.api_key {
            headers.push(("Authorization".into(), format!("Bearer {key}")));
        }
        let path = format!("{}/v1/chat/completions", self.path_prefix);
        let (mut resp, disconnect) = http::request(
            addr,
            "POST",
            &path,
            &headers,
            Some(body.to_string().as_bytes()),
            self.connect_timeout,
            self.read_timeout,
        )
        .map_err(|e| self.map_transport(e))?;
        on_open(disconnect);
        match resp.status {
            200 => {}
            401 | 403 => {
                return Err(ConnectorError::Auth(format!(
                    "backend rejected credentials ({})",
                    resp.status
                )));
            }
            status => {
                let message = resp
                    .body()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                return Err(ConnectorError::Backend { status, message });
            }
        }
        if stream {
            self.read_sse(&mut resp)
        } else {
            let bytes = resp.body().map_err(|e| self.map_transport(e))?;
            self.parse_completion(&bytes)
        }
    }

    fn map_transport(&self, e: HttpError) -> ConnectorError {
        match e {
            HttpError::Refused => ConnectorError::NotReady("connection refused"),
            HttpError::TimedOut => ConnectorError::Timeout(self.read_timeout),
            HttpError::ConnectionLost(io) => {
                self.lost.report();
                ConnectorError::ConnectionLost(io)
            }
            HttpError::Malformed(m) => ConnectorError::Decode(m),
            HttpError::Io(io) => ConnectorError::ConnectionLost(io),
        }
    }

    fn chat_body(
        &self,
        req: &ChatRequest,
        stream: bool,
        image_data_url: Option<&str>,
    ) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                // The caption call carries the image as a content PART on
                // the (single) user message (llama.cpp mmproj OAI shape).
                if let (Some(url), Role::User) = (image_data_url, m.role) {
                    serde_json::json!({
                        "role": role,
                        "content": [
                            { "type": "text", "text": m.content },
                            { "type": "image_url", "image_url": { "url": url } }
                        ]
                    })
                } else {
                    serde_json::json!({ "role": role, "content": m.content })
                }
            })
            .collect();
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "stream": stream,
        });
        if stream {
            // llama-server reports usage on the final chunk only when asked.
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
        if let Some(schema) = &req.json_schema {
            // Verbatim passthrough (§4: response_format json_schema).
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": schema,
            });
        }
        body
    }

    fn parse_completion(&self, bytes: &[u8]) -> ConnectorResult<ChatResponse> {
        let v: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| ConnectorError::Decode(format!("chat completion JSON: {e}")))?;
        let text = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| ConnectorError::Decode("missing choices[0].message.content".into()))?
            .to_owned();
        Ok(ChatResponse {
            text,
            prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            model_id: v["model"].as_str().unwrap_or(&self.model).to_owned(),
        })
    }

    fn read_sse(&self, resp: &mut http::Response) -> ConnectorResult<ChatResponse> {
        let mut text = String::new();
        let mut prompt_tokens = 0u32;
        let mut completion_tokens = 0u32;
        let mut model_id = self.model.clone();
        let mut saw_done = false;
        loop {
            let line = match resp.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => return Err(self.map_transport(e)),
            };
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                saw_done = true;
                break;
            }
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| ConnectorError::Decode(format!("SSE chunk JSON: {e}")))?;
            if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                text.push_str(delta);
            }
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                completion_tokens = u["completion_tokens"].as_u64().unwrap_or(0) as u32;
            }
            if let Some(m) = v["model"].as_str() {
                model_id = m.to_owned();
            }
        }
        if !saw_done {
            // The stream ended without [DONE]: the backend died mid-call.
            self.lost.report();
            return Err(ConnectorError::ConnectionLost(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "SSE stream ended without [DONE]",
            )));
        }
        Ok(ChatResponse {
            text,
            prompt_tokens,
            completion_tokens,
            model_id,
        })
    }
}

impl LanguageModel for OpenAiCompatClient {
    async fn complete(&self, req: ChatRequest) -> ConnectorResult<ChatResponse> {
        // Background-lane requests MUST stream (RUNTIME §9 — disconnect-
        // cancellation only works on streaming requests); interactive
        // requests take the simple non-streaming path.
        let stream = matches!(req.priority, crate::llm::Lane::Background);
        self.complete_transport(&req, stream, &mut |_| {})
    }

    async fn caption_image(&self, img: &DecodedImage, prompt: &str) -> ConnectorResult<String> {
        let Some(addr) = self.endpoint.get() else {
            return Err(ConnectorError::NotReady("llm endpoint not set"));
        };
        let _guard = self.gauge.enter();
        let png = encode_png_rgb8(&img.rgb8, img.width, img.height)
            .map_err(|e| ConnectorError::Decode(format!("image encode: {e}")))?;
        let data_url = format!("data:image/png;base64,{}", http::base64_encode(&png));
        let req = ChatRequest {
            messages: vec![crate::llm::ChatMessage {
                role: Role::User,
                content: prompt.to_owned(),
            }],
            max_tokens: CAPTION_MAX_TOKENS,
            temperature: CAPTION_TEMPERATURE,
            json_schema: None,
            priority: crate::llm::Lane::Background,
        };
        // Caption = background lane = streaming, always (§3.1/§9).
        let body = self.chat_body(&req, true, Some(&data_url));
        let mut headers: Vec<Header> = vec![("Content-Type".into(), "application/json".into())];
        if let Some(key) = &self.api_key {
            headers.push(("Authorization".into(), format!("Bearer {key}")));
        }
        let path = format!("{}/v1/chat/completions", self.path_prefix);
        let (mut resp, _disconnect) = http::request(
            addr,
            "POST",
            &path,
            &headers,
            Some(body.to_string().as_bytes()),
            self.connect_timeout,
            self.read_timeout,
        )
        .map_err(|e| self.map_transport(e))?;
        if resp.status != 200 {
            let status = resp.status;
            let message = resp
                .body()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            return Err(ConnectorError::Backend { status, message });
        }
        Ok(self.read_sse(&mut resp)?.text)
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

/// Parse `http://host:port[/prefix]` (the §4.4 `base_url`, `/v1` suffix
/// normalized away — request paths re-append it).
fn parse_base_url(base_url: &str) -> ConnectorResult<(SocketAddr, String)> {
    let rest = base_url
        .strip_prefix("http://")
        .ok_or_else(|| ConnectorError::Decode(format!("base_url must be http:// : {base_url}")))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    let prefix = path.strip_suffix("/v1").unwrap_or(path).to_owned();
    let mut addrs = std::net::ToSocketAddrs::to_socket_addrs(&hostport)
        .map_err(|e| ConnectorError::Decode(format!("base_url address: {e}")))?;
    let addr = addrs
        .next()
        .ok_or_else(|| ConnectorError::Decode(format!("base_url resolves nowhere: {base_url}")))?;
    Ok((addr, prefix))
}

/// Minimal PNG encoder (RGB8, zlib "stored" blocks — zero compression).
/// Captions are retrieval fuel sent to a local process; size over the
/// loopback is irrelevant next to a real image codec dependency.
fn encode_png_rgb8(rgb8: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 3;
    if rgb8.len() != expected {
        return Err(format!(
            "rgb8 length {} != {expected} for {width}x{height}",
            rgb8.len()
        ));
    }
    // Raw scanlines, filter byte 0 per row.
    let mut raw = Vec::with_capacity(expected + height as usize);
    for row in rgb8.chunks(width as usize * 3) {
        raw.push(0u8);
        raw.extend_from_slice(row);
    }
    // zlib: 0x78 0x01 + stored deflate blocks + adler32.
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(block) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, color type 2 (RGB)
    push_png_chunk(&mut png, b"IHDR", &ihdr);
    push_png_chunk(&mut png, b"IDAT", &z);
    push_png_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_parsing_normalizes_v1() {
        let (addr, prefix) = parse_base_url("http://127.0.0.1:11434/v1").unwrap();
        assert_eq!(addr.port(), 11434);
        assert_eq!(prefix, "");
        let (_, prefix) = parse_base_url("http://127.0.0.1:8080/llm/v1").unwrap();
        assert_eq!(prefix, "/llm");
        assert!(parse_base_url("https://api.example.com/v1").is_err());
    }

    #[test]
    fn png_encoder_emits_well_formed_signature_and_ihdr() {
        let png = encode_png_rgb8(&[255, 0, 0, 0, 255, 0], 2, 1).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], 2u32.to_be_bytes()); // width
        assert_eq!(&png[20..24], 1u32.to_be_bytes()); // height
        assert!(png.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn gauge_counts_and_releases() {
        let g = InFlightGauge::new();
        assert_eq!(g.count(), 0);
        {
            let _a = g.enter();
            let _b = g.enter();
            assert_eq!(g.count(), 2);
        }
        assert_eq!(g.count(), 0);
    }
}
