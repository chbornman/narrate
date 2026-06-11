//! Scriptable stub HTTP server for P6.2's wire tests (download manager,
//! OpenAI client, health probes). Real loopback sockets — the honest OS
//! boundary — with fully scripted behavior: routes answer from queues,
//! every request is recorded for assertions (the §13.7 license gate is
//! asserted **at the server**: zero requests for gated content).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One recorded request.
#[derive(Debug, Clone)]
pub struct StubRequest {
    pub method: String,
    /// Path including query string, exactly as sent.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl StubRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One scripted response step.
#[derive(Debug, Clone)]
pub enum StubResponse {
    /// Plain status + body (Content-Length framing).
    Body {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
        extra_headers: Vec<(String, String)>,
    },
    /// Send the head and `cut_after` body bytes, then sever the connection
    /// (the resumable-download / mid-call-crash scripts).
    CutBody {
        status: u16,
        body: Vec<u8>,
        cut_after: usize,
        extra_headers: Vec<(String, String)>,
    },
    /// SSE stream: each entry one `data: ...` line; `cut_after_events`
    /// severs mid-stream when set.
    Sse {
        events: Vec<String>,
        cut_after_events: Option<usize>,
    },
    /// SSE head + events, then hold the socket open without closing —
    /// "generation in progress"; the disconnect-cancellation tests sever
    /// from the client side while this hangs.
    SseHang { events: Vec<String> },
    /// Serve `file` honoring a `Range: bytes=N-` request header with 206
    /// (the download manager's resume path).
    RangedFile { file: Vec<u8> },
    /// Accept the request and never respond until the server shuts down
    /// (hanging /health while inference runs; the /slots trap).
    Hang,
}

impl StubResponse {
    pub fn ok_json(body: &str) -> Self {
        StubResponse::Body {
            status: 200,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }

    pub fn status(status: u16) -> Self {
        StubResponse::Body {
            status,
            content_type: "text/plain",
            body: Vec::new(),
            extra_headers: Vec::new(),
        }
    }
}

/// A scripted route: responses consumed FIFO; the LAST entry repeats once
/// the queue empties (steady-state behavior after a scripted prefix).
pub struct Route {
    queue: Mutex<Vec<StubResponse>>,
}

struct ServerState {
    routes: Mutex<HashMap<String, Arc<Route>>>,
    requests: Mutex<Vec<StubRequest>>,
    shutdown: AtomicBool,
}

/// The server. One thread per connection; `Connection: close` semantics
/// (matching the client in `crate::http`).
pub struct StubHttpServer {
    addr: SocketAddr,
    state: Arc<ServerState>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubHttpServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub server bind");
        let addr = listener.local_addr().expect("stub server addr");
        let state = Arc::new(ServerState {
            routes: Mutex::new(HashMap::new()),
            requests: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        });
        let accept_state = state.clone();
        let handle = std::thread::Builder::new()
            .name("pp-stub-http".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if accept_state.shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { break };
                    let conn_state = accept_state.clone();
                    std::thread::spawn(move || {
                        let _ = serve_connection(stream, &conn_state);
                    });
                }
            })
            .expect("spawn stub http server");
        Self {
            addr,
            state,
            handle: Some(handle),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Script a route (exact path match, ignoring the query string).
    /// Returns the route for appending more steps.
    pub fn route(&self, path: &str, response: StubResponse) -> Arc<Route> {
        let mut routes = self.state.routes.lock().unwrap();
        let route = routes
            .entry(path.to_owned())
            .or_insert_with(|| {
                Arc::new(Route {
                    queue: Mutex::new(Vec::new()),
                })
            })
            .clone();
        route.queue.lock().unwrap().push(response);
        route
    }

    /// Append another scripted step to an existing route.
    pub fn push(route: &Arc<Route>, response: StubResponse) {
        route.queue.lock().unwrap().push(response);
    }

    /// Every request seen so far (the license gate's byte-zero assertion
    /// reads THIS, not client-side state).
    pub fn requests(&self) -> Vec<StubRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn requests_for(&self, path_prefix: &str) -> Vec<StubRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.starts_with(path_prefix))
            .collect()
    }
}

impl Drop for StubHttpServer {
    fn drop(&mut self) {
        self.state.shutdown.store(true, Ordering::SeqCst);
        // Poke the accept loop awake.
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve_connection(stream: TcpStream, state: &Arc<ServerState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let full_path = parts.next().unwrap_or("/").to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let request = StubRequest {
        method,
        path: full_path.clone(),
        headers,
        body,
    };
    let range_offset = request.header("Range").and_then(|r| {
        r.strip_prefix("bytes=")
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u64>().ok())
    });
    state.requests.lock().unwrap().push(request);

    let path_only = full_path.split('?').next().unwrap_or("/").to_owned();
    let response = {
        let routes = state.routes.lock().unwrap();
        routes.get(&path_only).map(|route| {
            let mut q = route.queue.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q.first().cloned().unwrap_or(StubResponse::status(500))
            }
        })
    };
    let mut w = stream;
    match response {
        None => {
            w.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
        }
        Some(StubResponse::Body {
            status,
            content_type,
            body,
            extra_headers,
        }) => {
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in extra_headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            w.write_all(head.as_bytes())?;
            w.write_all(&body)?;
        }
        Some(StubResponse::CutBody {
            status,
            body,
            cut_after,
            extra_headers,
        }) => {
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in extra_headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            w.write_all(head.as_bytes())?;
            w.write_all(&body[..cut_after.min(body.len())])?;
            w.flush()?;
            let _ = w.shutdown(Shutdown::Both);
        }
        Some(StubResponse::Sse {
            events,
            cut_after_events,
        }) => {
            w.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )?;
            for (i, e) in events.iter().enumerate() {
                if let Some(cut) = cut_after_events
                    && i >= cut
                {
                    let _ = w.shutdown(Shutdown::Both);
                    return Ok(());
                }
                w.write_all(format!("data: {e}\r\n\r\n").as_bytes())?;
                w.flush()?;
            }
        }
        Some(StubResponse::RangedFile { file }) => {
            let from = range_offset.unwrap_or(0).min(file.len() as u64) as usize;
            let slice = &file[from..];
            let (status, range_header) = if range_offset.is_some() {
                (
                    206,
                    format!(
                        "Content-Range: bytes {from}-{}/{}\r\n",
                        file.len().saturating_sub(1),
                        file.len()
                    ),
                )
            } else {
                (200, String::new())
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{range_header}Accept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                slice.len()
            );
            w.write_all(head.as_bytes())?;
            w.write_all(slice)?;
        }
        Some(StubResponse::SseHang { events }) => {
            w.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )?;
            for e in &events {
                w.write_all(format!("data: {e}\r\n\r\n").as_bytes())?;
                w.flush()?;
            }
            let _ = w.set_read_timeout(Some(std::time::Duration::from_millis(50)));
            let mut buf = [0u8; 1];
            loop {
                if state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                match (&w).read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }
        Some(StubResponse::Hang) => {
            // Hold the socket open until the peer gives up or the server
            // drops; scripted probes time out against this.
            let _ = w.set_read_timeout(Some(std::time::Duration::from_millis(50)));
            let mut buf = [0u8; 1];
            loop {
                if state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                match (&w).read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}
