//! The `photoproof://` custom URI scheme: thumbnails and Look images served
//! straight from the preview cache (spec/UI.md §3.3, DECISIONS P16), plus
//! the progressive full-resolution routes for Look (dogfood rounds 1+2):
//!
//!   /thumb/<hash>     cached WebP thumbnail
//!   /display/<hash>   cached WebP display preview (2560 px class)
//!   /original/<hash>  the ORIGINAL file — served ONLY for webview-decodable
//!                     stored formats (jpeg/png/webp, by the images-table
//!                     format column, never extension sniffing), resolved
//!                     through the library's best ONLINE path; offline or
//!                     missing files refuse with 404. RAW/TIFF/HEIC sources
//!                     404 here by design — RAW falls through to /embedded;
//!                     TIFF/HEIC keep the display preview silently (the
//!                     full decode is the M1.5 backfill).
//!   /embedded/<hash>  the RAW's embedded full-resolution JPEG at NATIVE
//!                     size (dogfood round 2) — extracted on demand, served
//!                     display-oriented per the SAME §9.3.1 policy as the
//!                     cached preview (strokes live in display-oriented
//!                     space; the library refuses geometry disagreement).
//!                     Non-RAW, offline, placeholder, and small/no-embedded
//!                     sources refuse with the same uniform 404.
//!   /full-decode/<hash>  the on-demand NEUTRAL RAW develop at native sensor
//!                     resolution (OD-1) — the deepest Look zoom rung. Served
//!                     straight off the `previews/` cache once the
//!                     full-raw-decode pass has written it (whichever
//!                     container: WebP, or JPEG for over-cap dimensions). A
//!                     404 is the "developing..." state: the frontend has
//!                     enqueued the develop (request_full_decode) and retries.
//!
//! Image bytes NEVER cross `invoke`/IPC and are never base64-encoded. URLs
//! are content-addressed, so every route carries the same immutable cache
//! headers and the webview's own HTTP cache does the rest.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use photoproof_core::ContentHash;
use photoproof_core::library::{
    ArtifactKind, ImageFormat, Library, artifact_path, existing_full_artifact, touch_full_artifact,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Cached preview artifact (always WebP).
    Artifact(ArtifactKind),
    /// The original source file (allowlisted formats only).
    Original,
    /// The RAW's embedded full-resolution JPEG at native size.
    Embedded,
    /// The on-demand full-decode artifact at NATIVE sensor resolution
    /// (OD-1): the neutral RAW develop Look's 100%-zoom rung serves. 404s
    /// until the develop pass has written it (the "developing..." state).
    FullDecode,
}

/// Parse `/thumb/<hash>` | `/display/<hash>` (a trailing `.webp` is
/// tolerated) | `/original/<hash>` | `/embedded/<hash>` |
/// `/full-decode/<hash>`. Returns the route and the validated content hash —
/// hash validation is the traversal guard.
pub fn parse_path(path: &str) -> Option<(Route, ContentHash)> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let route = match parts.next()? {
        "micro" => Route::Artifact(ArtifactKind::Micro),
        "thumb" => Route::Artifact(ArtifactKind::Thumb),
        "display" => Route::Artifact(ArtifactKind::Display),
        "original" => Route::Original,
        "embedded" => Route::Embedded,
        "full-decode" => Route::FullDecode,
        _ => return None,
    };
    let rest = parts.next()?;
    let hash_str = match route {
        Route::Artifact(_) => rest.strip_suffix(".webp").unwrap_or(rest),
        // Content-addressed, no extension tolerance (the format is resolved
        // from the cache, not the URL).
        Route::Original | Route::Embedded | Route::FullDecode => rest,
    };
    let hash = ContentHash::from_hex(hash_str).ok()?;
    Some((route, hash))
}

/// Resolve an ARTIFACT request path to the cached WebP (existence-checked).
pub fn resolve(cache_dir: &Path, path: &str) -> Option<PathBuf> {
    match parse_path(path)? {
        (Route::Artifact(kind), hash) => {
            let file = artifact_path(cache_dir, &hash, kind);
            file.exists().then_some(file)
        }
        // Originals and embedded natives resolve through the library.
        // The full-decode artifact resolves straight off disk in `serve`
        // (its existence is the cache-hit signal), so it returns None here.
        (Route::Original | Route::Embedded | Route::FullDecode, _) => None,
    }
}

/// THE /original allowlist — by STORED format (the images-table column the
/// scan classified), never by sniffing the request or the file name. Only
/// formats every webview decodes natively are served; everything else
/// (RAW, TIFF, HEIC) stays on the display preview until the M1.5 backfill.
pub fn original_content_type(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Webp => Some("image/webp"),
        ImageFormat::Tiff | ImageFormat::Heic | ImageFormat::Raw => None,
    }
}

/// Resolve an /original request through the library: stored-format
/// allowlist → best path (LIBRARY §3.1) → ONLINE only → existence check.
/// Any refusal is a uniform `None` (the protocol answers 404; the frontend
/// keeps the preview silently).
pub fn resolve_original(library: &Library, hash: &ContentHash) -> Option<(PathBuf, &'static str)> {
    let record = library.image(hash).ok().flatten()?;
    let content_type = original_content_type(record.format)?;
    let best = library.best_path(hash).ok().flatten()?;
    if !best.online {
        return None;
    }
    let mount = best.mount_point?;
    let file = if best.row.rel_path.is_empty() {
        PathBuf::from(mount)
    } else {
        Path::new(&mount).join(&best.row.rel_path)
    };
    file.exists().then_some((file, content_type))
}

/// Serve any `photoproof://` request path against the library (artifacts
/// resolve under its preview cache; originals through its path tables).
pub fn serve(library: &Library, path: &str) -> http::Response<Vec<u8>> {
    match parse_path(path) {
        Some((Route::Artifact(_), _)) => resolve(library.cache_dir(), path)
            .and_then(|file| std::fs::read(file).ok())
            .map(|bytes| respond_ok(bytes, "image/webp"))
            .unwrap_or_else(respond_not_found),
        Some((Route::Original, hash)) => resolve_original(library, &hash)
            .and_then(|(file, content_type)| {
                std::fs::read(file).ok().map(|bytes| (bytes, content_type))
            })
            .map(|(bytes, content_type)| respond_ok(bytes, content_type))
            .unwrap_or_else(respond_not_found),
        // The library owns the whole embedded-native policy (RAW-only,
        // online-only, placeholder skip, §9.3.1 orientation, pixel-gain +
        // geometry agreement); any refusal is the same uniform 404.
        Some((Route::Embedded, hash)) => library
            .embedded_native(&hash)
            .ok()
            .flatten()
            .map(|native| respond_ok(native.jpeg, "image/jpeg"))
            .unwrap_or_else(respond_not_found),
        // The on-demand full-decode artifact (OD-1): served straight off
        // disk if the develop pass has written it (whichever container —
        // WebP, or JPEG for over-cap dimensions). A 404 here is the
        // "developing..." state — the frontend has already enqueued the
        // develop via `request_full_decode` and retries.
        Some((Route::FullDecode, hash)) => existing_full_artifact(library.cache_dir(), &hash)
            .and_then(|(file, format)| {
                // Touch-on-serve: bump the artifact's mtime to NOW so the 1:1
                // cache's LRU evictor (DESIGN-PREVIEW-POLICY.md) treats this as
                // freshly VIEWED. We touch on the read path rather than trust
                // atime, which is unreliable across the mounts we target
                // (noatime/relatime, macOS volumes). Best-effort: a touch
                // failure must never block the serve.
                touch_full_artifact(&file);
                std::fs::read(file).ok().map(|bytes| (bytes, format))
            })
            .map(|(bytes, format)| respond_ok(bytes, format.content_type()))
            .unwrap_or_else(respond_not_found),
        None => respond_not_found(),
    }
}

pub fn respond_ok(bytes: Vec<u8>, content_type: &'static str) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(200)
        .header("content-type", content_type)
        // Content-addressed: immutable forever (every route).
        .header("cache-control", "public, max-age=31536000, immutable")
        .body(bytes)
        .expect("static response")
}

pub fn respond_not_found() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(404)
        .header("cache-control", "no-store")
        .body(Vec::new())
        .expect("static response")
}

/// A bounded protocol queue refused or superseded this request before any
/// filesystem/database work began. The response is deliberately not cached:
/// mounted grid cells retry through their existing error/backoff path, while
/// cells recycled away by a fling disappear without leaving stale work behind.
pub fn respond_overloaded() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(503)
        .header("cache-control", "no-store")
        .header("retry-after", "1")
        .body(Vec::new())
        .expect("static response")
}

// ---- bounded priority serve pool --------------------------------------------

/// Scheduling class for one protocol request.
///
/// Look's visible display/full-resolution ladder must not sit behind a burst of
/// grid thumbnails. Micro/thumb requests are intentionally one lower class;
/// within that class, overload keeps recent requests because they are most
/// likely to belong to the viewport where a fling settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServePriority {
    Interactive,
    Thumbnail,
}

/// Outcome delivered exactly once to every submitted job. Keeping overload in
/// the job callback matters for Tauri's one-shot responder: even an evicted
/// request receives a prompt, explicit 503 instead of being silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeDisposition {
    Execute,
    Overloaded,
}

/// Classify a custom-protocol request from its content-addressed route.
pub fn priority_for_path(path: &str) -> ServePriority {
    match parse_path(path) {
        Some((Route::Artifact(ArtifactKind::Micro | ArtifactKind::Thumb), _)) => {
            ServePriority::Thumbnail
        }
        // The displayed Look image and every deeper source rung are
        // interactive. Invalid paths are cheap 404s and are kept out of the
        // thumbnail replacement policy.
        Some((Route::Artifact(ArtifactKind::Display), _))
        | Some((Route::Original | Route::Embedded | Route::FullDecode, _))
        | None => ServePriority::Interactive,
    }
}

/// The backend artifact-cache outcome observable at this protocol boundary.
///
/// Webview HTTP-cache hits never invoke this handler, so this must not claim
/// to measure that cache. Micro/thumb/display and full-decode routes do read a
/// content-addressed backend artifact: 2xx is a hit and 404 is a miss. Original
/// files and on-demand embedded RAW extraction are not artifact-cache reads.
/// "Stale" is impossible here because artifact URLs are immutable by hash.
pub fn backend_cache_status(
    path: &str,
    status: http::StatusCode,
) -> Option<crate::performance::CacheStatus> {
    match parse_path(path) {
        Some((Route::Artifact(_), _)) | Some((Route::FullDecode, _)) => {
            if status.is_success() {
                Some(crate::performance::CacheStatus::Hit)
            } else if status == http::StatusCode::NOT_FOUND {
                Some(crate::performance::CacheStatus::Miss)
            } else {
                None
            }
        }
        Some((Route::Original | Route::Embedded, _)) | None => None,
    }
}

type JobCallback = Box<dyn FnOnce(ServeDisposition) + Send>;

struct Job {
    priority: ServePriority,
    enqueued_at: Instant,
    callback: JobCallback,
}

struct ServeQueue {
    interactive: VecDeque<Job>,
    thumbnails: VecDeque<Job>,
    closed: bool,
}

impl ServeQueue {
    fn len(&self) -> usize {
        self.interactive.len() + self.thumbnails.len()
    }

    fn next(&mut self) -> Option<Job> {
        self.interactive
            .pop_front()
            .or_else(|| self.thumbnails.pop_front())
    }
}

struct SharedQueue {
    state: Mutex<ServeQueue>,
    ready: Condvar,
    capacity: usize,
    metrics: ServeMetrics,
}

#[derive(Default)]
struct PriorityMetrics {
    accepted: AtomicU64,
    completed: AtomicU64,
    overloaded: AtomicU64,
    superseded: AtomicU64,
    queue_wait_ns: AtomicU64,
    max_queue_wait_ns: AtomicU64,
    queue_wait_histogram: LatencyHistogram,
    service_ns: AtomicU64,
    max_service_ns: AtomicU64,
    service_histogram: LatencyHistogram,
}

// Conservative upper bounds. The final bucket uses the observed maximum so
// even pathological stalls remain finite and truthful in Application Health.
const LATENCY_BUCKET_UPPER_NS: [u64; 21] = [
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_000_000,
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    2_000_000_000,
    5_000_000_000,
    10_000_000_000,
    60_000_000_000,
    u64::MAX,
];

struct LatencyHistogram {
    buckets: [AtomicU64; LATENCY_BUCKET_UPPER_NS.len()],
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl LatencyHistogram {
    fn record(&self, duration_ns: u64) {
        let bucket = LATENCY_BUCKET_UPPER_NS
            .partition_point(|upper| duration_ns > *upper)
            .min(LATENCY_BUCKET_UPPER_NS.len() - 1);
        // Publish total/max updates made before this sample is visible to an
        // Acquire snapshot of the bucket.
        self.buckets[bucket].fetch_add(1, Ordering::Release);
    }

    fn percentile_ms(&self, quantile: f64, observed_max_ns: u64) -> Option<f64> {
        let counts = self
            .buckets
            .each_ref()
            .map(|bucket| bucket.load(Ordering::Acquire));
        let total = counts.iter().copied().sum::<u64>();
        if total == 0 {
            return None;
        }
        let rank = (quantile * total as f64).ceil().max(1.0) as u64;
        let mut cumulative = 0u64;
        for (index, count) in counts.into_iter().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                let upper_ns = LATENCY_BUCKET_UPPER_NS[index];
                let conservative_ns = if upper_ns == u64::MAX {
                    observed_max_ns
                } else {
                    upper_ns
                };
                return Some(conservative_ns as f64 / 1_000_000.0);
            }
        }
        Some(observed_max_ns as f64 / 1_000_000.0)
    }
}

#[derive(Default)]
struct ServeMetrics {
    interactive: PriorityMetrics,
    thumbnail: PriorityMetrics,
    queued: AtomicUsize,
    peak_queued: AtomicUsize,
    running: AtomicUsize,
    peak_running: AtomicUsize,
}

impl ServeMetrics {
    fn priority(&self, priority: ServePriority) -> &PriorityMetrics {
        match priority {
            ServePriority::Interactive => &self.interactive,
            ServePriority::Thumbnail => &self.thumbnail,
        }
    }

    fn set_queued(&self, queued: usize) {
        self.queued.store(queued, Ordering::Relaxed);
        self.peak_queued.fetch_max(queued, Ordering::Relaxed);
    }

    fn started(&self, job: &Job) -> u64 {
        let wait_ns = duration_ns(job.enqueued_at.elapsed());
        let running = self.running.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_running.fetch_max(running, Ordering::Relaxed);
        wait_ns
    }

    fn completed(&self, priority: ServePriority, queue_wait_ns: u64, service: Duration) {
        let metrics = self.priority(priority);
        let service_ns = duration_ns(service);
        metrics
            .queue_wait_ns
            .fetch_add(queue_wait_ns, Ordering::Relaxed);
        metrics
            .max_queue_wait_ns
            .fetch_max(queue_wait_ns, Ordering::Relaxed);
        metrics.queue_wait_histogram.record(queue_wait_ns);
        metrics.service_ns.fetch_add(service_ns, Ordering::Relaxed);
        metrics
            .max_service_ns
            .fetch_max(service_ns, Ordering::Relaxed);
        metrics.service_histogram.record(service_ns);
        // Release publishes both latency samples before completion is visible.
        metrics.completed.fetch_add(1, Ordering::Release);
        self.running.fetch_sub(1, Ordering::Relaxed);
    }
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

/// Low-overhead, process-lifetime request accounting for one scheduling class.
/// Queue wait spans admission to worker dequeue. Service spans the worker-held
/// callback, including byte serving, the existing local telemetry append, and
/// resolving Tauri's responder—the time that actually occupies pool capacity.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServePrioritySnapshot {
    pub accepted: u64,
    pub completed: u64,
    pub overloaded: u64,
    pub superseded: u64,
    pub mean_queue_wait_ms: f64,
    pub queue_wait_p50_ms: Option<f64>,
    pub queue_wait_p95_ms: Option<f64>,
    pub queue_wait_p99_ms: Option<f64>,
    pub max_queue_wait_ms: f64,
    pub mean_service_ms: f64,
    pub service_p50_ms: Option<f64>,
    pub service_p95_ms: Option<f64>,
    pub service_p99_ms: Option<f64>,
    pub max_service_ms: f64,
}

impl ServePrioritySnapshot {
    fn load(metrics: &PriorityMetrics) -> Self {
        let completed = metrics.completed.load(Ordering::Acquire);
        let mean_ms = |total_ns: u64| {
            if completed == 0 {
                0.0
            } else {
                total_ns as f64 / completed as f64 / 1_000_000.0
            }
        };
        let max_queue_wait_ns = metrics.max_queue_wait_ns.load(Ordering::Relaxed);
        let max_service_ns = metrics.max_service_ns.load(Ordering::Relaxed);
        Self {
            accepted: metrics.accepted.load(Ordering::Relaxed),
            completed,
            overloaded: metrics.overloaded.load(Ordering::Relaxed),
            superseded: metrics.superseded.load(Ordering::Relaxed),
            mean_queue_wait_ms: mean_ms(metrics.queue_wait_ns.load(Ordering::Relaxed)),
            queue_wait_p50_ms: metrics
                .queue_wait_histogram
                .percentile_ms(0.50, max_queue_wait_ns),
            queue_wait_p95_ms: metrics
                .queue_wait_histogram
                .percentile_ms(0.95, max_queue_wait_ns),
            queue_wait_p99_ms: metrics
                .queue_wait_histogram
                .percentile_ms(0.99, max_queue_wait_ns),
            max_queue_wait_ms: max_queue_wait_ns as f64 / 1_000_000.0,
            mean_service_ms: mean_ms(metrics.service_ns.load(Ordering::Relaxed)),
            service_p50_ms: metrics
                .service_histogram
                .percentile_ms(0.50, max_service_ns),
            service_p95_ms: metrics
                .service_histogram
                .percentile_ms(0.95, max_service_ns),
            service_p99_ms: metrics
                .service_histogram
                .percentile_ms(0.99, max_service_ns),
            max_service_ms: max_service_ns as f64 / 1_000_000.0,
        }
    }
}

/// Scheduler truth surfaced through Application Health. Counters are
/// process-lifetime totals; current gauges and peaks cover queued callbacks
/// and actively occupied fixed workers.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServePoolSnapshot {
    pub initialized: bool,
    pub workers: usize,
    pub queue_capacity: usize,
    pub queued: usize,
    pub peak_queued: usize,
    pub running: usize,
    pub peak_running: usize,
    pub interactive: ServePrioritySnapshot,
    pub thumbnail: ServePrioritySnapshot,
}

/// Fixed-size worker pool the `photoproof://` protocol registration runs
/// `serve` on.
///
/// WHY a bound: the handler used to `std::thread::spawn` per request, so a
/// fling-scroll over a large grid burst dozens-to-hundreds of transient OS
/// threads, every one doing blocking `std::fs::read` — all competing for the
/// filesystem and (on the /original, /embedded, and /full-decode routes) the
/// library's db lock. The first fix made the workers fixed but left their FIFO
/// queue unbounded, so obsolete fling requests could still delay the settled
/// viewport and Look.
///
/// This pool bounds both resources. Interactive Look routes always dequeue
/// before thumbnails. When the queue is full, a new thumbnail supersedes the
/// oldest queued thumbnail (latest viewport approximation); an interactive
/// request also displaces the oldest queued thumbnail. If the queue is full of
/// interactive work, the new request receives overload instead. Running jobs
/// are never interrupted, preserving response correctness.
pub struct ServePool {
    shared: Arc<SharedQueue>,
    workers: usize,
}

impl ServePool {
    fn new(workers: usize) -> Self {
        // Sixteen queued reads per worker absorbs ordinary layout churn while
        // placing a hard ceiling under a sustained fling. The process pool is
        // clamped to 2..8 workers, so this is 32..128 queued callbacks.
        Self::new_with_capacity(workers, workers.saturating_mul(16).max(1))
    }

    fn new_with_capacity(workers: usize, capacity: usize) -> Self {
        assert!(workers > 0, "protocol pool needs at least one worker");
        assert!(capacity > 0, "protocol queue needs at least one slot");
        let shared = Arc::new(SharedQueue {
            state: Mutex::new(ServeQueue {
                interactive: VecDeque::new(),
                thumbnails: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
            capacity,
            metrics: ServeMetrics::default(),
        });
        for i in 0..workers {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name(format!("pp-protocol-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let Ok(mut queue) = shared.state.lock() else {
                                break;
                            };
                            loop {
                                if let Some(job) = queue.next() {
                                    shared.metrics.set_queued(queue.len());
                                    break Some(job);
                                }
                                if queue.closed {
                                    break None;
                                }
                                let Ok(next) = shared.ready.wait(queue) else {
                                    break None;
                                };
                                queue = next;
                            }
                        };
                        match job {
                            Some(job) => {
                                let queue_wait_ns = shared.metrics.started(&job);
                                let priority = job.priority;
                                let started = Instant::now();
                                finish_callback(job.callback, ServeDisposition::Execute);
                                shared.metrics.completed(
                                    priority,
                                    queue_wait_ns,
                                    started.elapsed(),
                                );
                            }
                            None => break,
                        }
                    }
                })
                .expect("spawn photoproof protocol worker");
        }
        Self { shared, workers }
    }

    /// The bound itself — the number of worker threads. Exposed so the
    /// preview_serve_latency perf test can pin that the shipping mechanism
    /// is a fixed pool (F1's regression guard).
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Maximum number of requests waiting behind the fixed workers.
    pub fn queue_capacity(&self) -> usize {
        self.shared.capacity
    }

    pub fn snapshot(&self) -> ServePoolSnapshot {
        let metrics = &self.shared.metrics;
        ServePoolSnapshot {
            initialized: true,
            workers: self.workers,
            queue_capacity: self.shared.capacity,
            queued: metrics.queued.load(Ordering::Relaxed),
            peak_queued: metrics.peak_queued.load(Ordering::Relaxed),
            running: metrics.running.load(Ordering::Relaxed),
            peak_running: metrics.peak_running.load(Ordering::Relaxed),
            interactive: ServePrioritySnapshot::load(&metrics.interactive),
            thumbnail: ServePrioritySnapshot::load(&metrics.thumbnail),
        }
    }

    /// Submit a request without waiting for filesystem/database serve work.
    ///
    /// The callback always runs exactly once: on a worker with `Execute`, or
    /// promptly inline with `Overloaded` when this request (or an older queued
    /// thumbnail it supersedes) cannot remain queued. Production overload
    /// callbacks only resolve the lightweight Tauri responder.
    pub fn run(
        &self,
        priority: ServePriority,
        job: impl FnOnce(ServeDisposition) + Send + 'static,
    ) {
        let mut incoming = Some(Job {
            priority,
            enqueued_at: Instant::now(),
            callback: Box::new(job),
        });
        let (rejected, superseded) = match self.shared.state.lock() {
            Ok(mut queue) if !queue.closed => {
                let displaced = if queue.len() >= self.shared.capacity {
                    // Both a newer thumbnail and interactive work supersede
                    // the oldest still-queued thumbnail. Never evict an
                    // interactive request in favor of thumbnail work.
                    queue.thumbnails.pop_front()
                } else {
                    None
                };
                if queue.len() < self.shared.capacity {
                    let job = incoming.take().expect("incoming job present");
                    self.shared
                        .metrics
                        .priority(job.priority)
                        .accepted
                        .fetch_add(1, Ordering::Relaxed);
                    match priority {
                        ServePriority::Interactive => queue.interactive.push_back(job),
                        ServePriority::Thumbnail => queue.thumbnails.push_back(job),
                    }
                    self.shared.metrics.set_queued(queue.len());
                    self.shared.ready.notify_one();
                }
                let superseded = displaced.is_some();
                (displaced.or(incoming), superseded)
            }
            // A poisoned/closed pool must still resolve Tauri's responder.
            _ => (incoming, false),
        };
        if let Some(job) = rejected {
            let metrics = self.shared.metrics.priority(job.priority);
            if superseded {
                // An accepted queued thumbnail was displaced by newer work.
                metrics.superseded.fetch_add(1, Ordering::Relaxed);
            } else {
                // The incoming request could not be admitted.
                metrics.overloaded.fetch_add(1, Ordering::Relaxed);
            }
            finish_callback(job.callback, ServeDisposition::Overloaded);
        }
    }
}

impl Drop for ServePool {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.shared.state.lock() {
            queue.closed = true;
            // Resolve callbacks that will never reach a worker.
            let mut rejected: Vec<_> = queue.interactive.drain(..).collect();
            rejected.extend(queue.thumbnails.drain(..));
            self.shared.metrics.set_queued(0);
            self.shared.ready.notify_all();
            drop(queue);
            for job in rejected {
                self.shared
                    .metrics
                    .priority(job.priority)
                    .superseded
                    .fetch_add(1, Ordering::Relaxed);
                finish_callback(job.callback, ServeDisposition::Overloaded);
            }
        }
    }
}

fn finish_callback(job: JobCallback, disposition: ServeDisposition) {
    // One panicking request (including an overload responder) cannot shrink a
    // fixed pool or prevent another displaced callback from resolving.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(disposition)));
}

/// The process-wide pool the protocol registration in `lib.rs` drives — and
/// the SAME instance the preview_serve_latency perf test fires 200
/// concurrent requests through, so the test locks in exactly the mechanism
/// that ships.
pub fn serve_pool() -> &'static ServePool {
    SERVE_POOL.get_or_init(|| ServePool::new(configured_workers()))
}

/// Read scheduler health without causing Settings/Application Health to start
/// the worker threads before the first real protocol request.
pub fn serve_pool_snapshot() -> ServePoolSnapshot {
    SERVE_POOL.get().map_or_else(
        || {
            let workers = configured_workers();
            ServePoolSnapshot {
                initialized: false,
                workers,
                queue_capacity: workers * 16,
                queued: 0,
                peak_queued: 0,
                running: 0,
                peak_running: 0,
                interactive: ServePrioritySnapshot::load(&PriorityMetrics::default()),
                thumbnail: ServePrioritySnapshot::load(&PriorityMetrics::default()),
            }
        },
        ServePool::snapshot,
    )
}

static SERVE_POOL: OnceLock<ServePool> = OnceLock::new();

fn configured_workers() -> usize {
    // Sized to core count, clamped to [2, 8]: serve is blocking-I/O dominated
    // (fs::read of small cached WebPs), so a few threads saturate the disk and
    // more only add FS + db-lock contention. The floor of 2 keeps a slow
    // /embedded extraction from head-of-line blocking every thumb on tiny
    // machines; the cap of 8 stops many-core desktops from re-creating the F1
    // stampede at pool size.
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .clamp(2, 8)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use photoproof_core::library::{
        EmbeddedPreviewExtractor, ExtractedPreview, FakeVolumeProbe, LibraryOptions,
        PlatformIdKind, PreviewError, ProbedVolume, QueueOptions, ScanOptions,
    };

    use super::*;

    fn hash() -> ContentHash {
        ContentHash::from_bytes_of(b"x")
    }

    #[test]
    fn route_priority_keeps_look_ahead_of_grid_thumbnails() {
        let h = hash();
        for route in ["micro", "thumb"] {
            assert_eq!(
                priority_for_path(&format!("/{route}/{}", h.as_str())),
                ServePriority::Thumbnail
            );
        }
        for route in ["display", "original", "embedded", "full-decode"] {
            assert_eq!(
                priority_for_path(&format!("/{route}/{}", h.as_str())),
                ServePriority::Interactive
            );
        }
        assert_eq!(
            priority_for_path("/invalid/not-a-hash"),
            ServePriority::Interactive
        );
    }

    #[test]
    fn overload_response_is_retryable_and_never_cached() {
        let response = respond_overloaded();
        assert_eq!(response.status(), 503);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["retry-after"], "1");
        assert!(response.body().is_empty());
    }

    #[test]
    fn cache_status_only_describes_backend_artifact_routes() {
        let h = hash();
        for route in ["micro", "thumb", "display", "full-decode"] {
            let path = format!("/{route}/{}", h.as_str());
            assert_eq!(
                backend_cache_status(&path, http::StatusCode::OK),
                Some(crate::performance::CacheStatus::Hit)
            );
            assert_eq!(
                backend_cache_status(&path, http::StatusCode::NOT_FOUND),
                Some(crate::performance::CacheStatus::Miss)
            );
            assert_eq!(
                backend_cache_status(&path, http::StatusCode::SERVICE_UNAVAILABLE),
                None
            );
        }
        for route in ["original", "embedded"] {
            assert_eq!(
                backend_cache_status(&format!("/{route}/{}", h.as_str()), http::StatusCode::OK),
                None
            );
        }
    }

    #[test]
    fn latency_histogram_is_empty_until_a_request_completes() {
        let snapshot = ServePrioritySnapshot::load(&PriorityMetrics::default());
        assert_eq!(snapshot.queue_wait_p50_ms, None);
        assert_eq!(snapshot.queue_wait_p95_ms, None);
        assert_eq!(snapshot.queue_wait_p99_ms, None);
        assert_eq!(snapshot.service_p50_ms, None);
        assert_eq!(snapshot.service_p95_ms, None);
        assert_eq!(snapshot.service_p99_ms, None);
    }

    #[test]
    fn latency_histogram_reports_conservative_tail_bounds() {
        let histogram = LatencyHistogram::default();
        for duration_ns in [1, 20_000, 600_000, 20_000_000_000] {
            histogram.record(duration_ns);
        }
        assert_eq!(histogram.percentile_ms(0.50, 20_000_000_000), Some(0.025));
        assert_eq!(
            histogram.percentile_ms(0.95, 20_000_000_000),
            Some(60_000.0)
        );
        assert_eq!(
            histogram.percentile_ms(0.99, 20_000_000_000),
            Some(60_000.0)
        );

        let overflow = LatencyHistogram::default();
        overflow.record(61_000_000_000);
        assert_eq!(
            overflow.percentile_ms(0.99, 61_000_000_000),
            Some(61_000.0),
            "the open-ended bucket reports the observed maximum, not infinity"
        );
    }

    /// Occupy the sole worker until the test releases it, making the queued
    /// ordering/overload behavior deterministic rather than timing-based.
    fn block_only_worker(pool: &ServePool) -> mpsc::Sender<()> {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.run(ServePriority::Interactive, move |disposition| {
            assert_eq!(disposition, ServeDisposition::Execute);
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started blocker");
        release_tx
    }

    #[test]
    fn full_thumbnail_queue_supersedes_oldest_work_with_latest() {
        let pool = ServePool::new_with_capacity(1, 2);
        let release = block_only_worker(&pool);
        let (done_tx, done_rx) = mpsc::channel();

        for id in 1..=3 {
            let done_tx = done_tx.clone();
            pool.run(ServePriority::Thumbnail, move |disposition| {
                done_tx.send((id, disposition)).unwrap();
            });
        }

        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (1, ServeDisposition::Overloaded),
            "the oldest queued thumbnail is the stale fling work"
        );
        release.send(()).unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (2, ServeDisposition::Execute)
        );
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (3, ServeDisposition::Execute)
        );
        let snapshot = pool.snapshot();
        assert_eq!(snapshot.queue_capacity, 2);
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.peak_queued, 2);
        assert_eq!(snapshot.running, 0);
        assert_eq!(snapshot.peak_running, 1);
        assert_eq!(snapshot.thumbnail.accepted, 3);
        assert_eq!(snapshot.thumbnail.completed, 2);
        assert_eq!(snapshot.thumbnail.superseded, 1);
        assert_eq!(snapshot.thumbnail.overloaded, 0);
        assert!(snapshot.thumbnail.queue_wait_p50_ms.is_some());
        assert!(snapshot.thumbnail.queue_wait_p95_ms.is_some());
        assert!(snapshot.thumbnail.queue_wait_p99_ms.is_some());
        assert!(snapshot.thumbnail.service_p50_ms.is_some());
        assert!(snapshot.thumbnail.service_p95_ms.is_some());
        assert!(snapshot.thumbnail.service_p99_ms.is_some());
        assert!(snapshot.thumbnail.max_queue_wait_ms > 0.0);
        assert!(snapshot.thumbnail.max_service_ms > 0.0);
    }

    #[test]
    fn interactive_request_displaces_thumbnail_and_runs_first() {
        let pool = ServePool::new_with_capacity(1, 2);
        let release = block_only_worker(&pool);
        let (done_tx, done_rx) = mpsc::channel();

        for id in 1..=2 {
            let done_tx = done_tx.clone();
            pool.run(ServePriority::Thumbnail, move |disposition| {
                done_tx.send((id, disposition)).unwrap();
            });
        }
        let done_interactive = done_tx.clone();
        pool.run(ServePriority::Interactive, move |disposition| {
            done_interactive.send((9, disposition)).unwrap();
        });

        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (1, ServeDisposition::Overloaded)
        );
        release.send(()).unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (9, ServeDisposition::Execute),
            "Look work must dequeue before the remaining thumbnail"
        );
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (2, ServeDisposition::Execute)
        );
        let snapshot = pool.snapshot();
        assert_eq!(snapshot.interactive.accepted, 2);
        assert_eq!(snapshot.interactive.completed, 2);
        assert_eq!(snapshot.thumbnail.accepted, 2);
        assert_eq!(snapshot.thumbnail.completed, 1);
        assert_eq!(snapshot.thumbnail.superseded, 1);
    }

    #[test]
    fn thumbnail_cannot_displace_a_full_interactive_queue() {
        let pool = ServePool::new_with_capacity(1, 1);
        let release = block_only_worker(&pool);
        let (done_tx, done_rx) = mpsc::channel();

        let interactive_tx = done_tx.clone();
        pool.run(ServePriority::Interactive, move |disposition| {
            interactive_tx.send((1, disposition)).unwrap();
        });
        pool.run(ServePriority::Thumbnail, move |disposition| {
            done_tx.send((2, disposition)).unwrap();
        });

        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (2, ServeDisposition::Overloaded)
        );
        release.send(()).unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (1, ServeDisposition::Execute)
        );
        let snapshot = pool.snapshot();
        assert_eq!(snapshot.thumbnail.accepted, 0);
        assert_eq!(snapshot.thumbnail.completed, 0);
        assert_eq!(snapshot.thumbnail.overloaded, 1);
        assert_eq!(snapshot.thumbnail.superseded, 0);
    }

    #[test]
    fn parses_thumb_and_display_paths() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/thumb/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Thumb));
        assert_eq!(parsed, h);
        let (r, _) = parse_path(&format!("/display/{}.webp", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Display));
    }

    #[test]
    fn parses_the_micro_graph_tier_path() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/micro/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Micro));
        assert_eq!(parsed, h);
        // Same artifact discipline: a trailing .webp is tolerated, a bad hash 404s.
        let (r, _) = parse_path(&format!("/micro/{}.webp", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Micro));
        assert!(parse_path("/micro/not-a-hash").is_none());
        assert!(parse_path("/micro/../../etc/passwd").is_none());
    }

    #[test]
    fn parses_the_original_route() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/original/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Original);
        assert_eq!(parsed, h);
        // No suffix tolerance on originals (content-addressed, no extension).
        assert!(parse_path(&format!("/original/{}.webp", h.as_str())).is_none());
        assert!(parse_path(&format!("/original/{}.jpg", h.as_str())).is_none());
    }

    #[test]
    fn parses_the_embedded_route() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/embedded/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Embedded);
        assert_eq!(parsed, h);
        // Same discipline as /original: no suffix tolerance, no traversal,
        // never resolved as a cache artifact.
        assert!(parse_path(&format!("/embedded/{}.jpg", h.as_str())).is_none());
        assert!(parse_path("/embedded/not-a-hash").is_none());
        assert!(parse_path("/embedded/../../etc/passwd").is_none());
        assert!(parse_path("/embedded/").is_none());
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(dir.path(), &format!("/embedded/{}", h.as_str())).is_none());
    }

    #[test]
    fn parses_the_full_decode_route() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/full-decode/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::FullDecode);
        assert_eq!(parsed, h);
        // Same discipline: no suffix tolerance, no traversal, hash-validated.
        assert!(parse_path(&format!("/full-decode/{}.webp", h.as_str())).is_none());
        assert!(parse_path("/full-decode/not-a-hash").is_none());
        assert!(parse_path("/full-decode/../../etc/passwd").is_none());
        assert!(parse_path("/full-decode/").is_none());
        // Resolves straight off disk in `serve`, not via `resolve`.
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(dir.path(), &format!("/full-decode/{}", h.as_str())).is_none());
    }

    /// A full-decode artifact sitting in the cache is served with its
    /// container's content-type; a missing one 404s (the "developing..."
    /// state). The develop pass writes the bytes; here we drop them in
    /// directly to test the wire contract.
    #[test]
    fn serves_the_full_decode_artifact_when_present_else_404() {
        use photoproof_core::library::{FullDecodeFormat, full_artifact_path};
        let (_tmp, lib, _probe) = raw_env();
        let h = hash_with_format(&lib, ImageFormat::Raw);
        // Not developed yet: 404 (the develop pass has not written it).
        assert_eq!(
            serve(&lib, &format!("/full-decode/{}", h.as_str())).status(),
            404
        );
        // Drop a WebP full artifact in the cache slot the route reads. The
        // route serves the bytes verbatim with the slot's content-type (it
        // never decodes), so opaque bytes suffice for the wire contract.
        let dest = full_artifact_path(lib.cache_dir(), &h, FullDecodeFormat::Webp);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let body = b"RIFF....WEBPbytes".to_vec();
        std::fs::write(&dest, &body).unwrap();
        let resp = serve(&lib, &format!("/full-decode/{}", h.as_str()));
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "image/webp");
        assert_eq!(
            resp.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(resp.body().as_slice(), body.as_slice());
    }

    #[test]
    fn rejects_unknown_kinds_bad_hashes_and_traversal() {
        let h = hash();
        assert!(parse_path(&format!("/full/{}", h.as_str())).is_none());
        assert!(parse_path("/thumb/not-a-hash").is_none());
        assert!(parse_path("/thumb/../../etc/passwd").is_none());
        assert!(parse_path("/original/not-a-hash").is_none());
        assert!(parse_path("/original/../../etc/passwd").is_none());
        assert!(parse_path("/original/").is_none());
        assert!(parse_path("/thumb/").is_none());
        assert!(parse_path("/").is_none());
    }

    #[test]
    fn resolve_requires_existing_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let h = hash();
        assert!(resolve(dir.path(), &format!("/thumb/{}", h.as_str())).is_none());
        let file = artifact_path(dir.path(), &h, ArtifactKind::Thumb);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"webp-bytes").unwrap();
        assert_eq!(
            resolve(dir.path(), &format!("/thumb/{}", h.as_str())),
            Some(file)
        );
    }

    #[test]
    fn the_original_allowlist_is_stored_format_only() {
        assert_eq!(original_content_type(ImageFormat::Jpeg), Some("image/jpeg"));
        assert_eq!(original_content_type(ImageFormat::Png), Some("image/png"));
        assert_eq!(original_content_type(ImageFormat::Webp), Some("image/webp"));
        // Not webview-decodable: the preview stands (M1.5 backfill).
        assert_eq!(original_content_type(ImageFormat::Tiff), None);
        assert_eq!(original_content_type(ImageFormat::Heic), None);
        assert_eq!(original_content_type(ImageFormat::Raw), None);
    }

    // ---- /original against a real temp library ----------------------------

    fn probed(mount: &Path) -> ProbedVolume {
        ProbedVolume {
            mount_point: mount.to_path_buf(),
            platform_id: Some("uuid-protocol-test".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("ShootDisk".into()),
            fs_type: Some("ext4".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }
    }

    /// Temp library with one scanned root holding one JPEG and one TIFF
    /// (garbage bytes — hashing and extension classification do not care).
    fn env() -> (tempfile::TempDir, Library, FakeVolumeProbe) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        let dir = mount.join("shoot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_0001.jpg"), b"jpeg-original-bytes").unwrap();
        std::fs::write(dir.join("IMG_0002.tif"), b"tiff-original-bytes").unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let lib = Library::open_with(
            tmp.path().join("photoproof.db"),
            tmp.path().join("previews"),
            LibraryOptions {
                probe: Arc::new(probe.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let root_id = lib.register_root(&dir, Some("shoot")).unwrap();
        lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
        (tmp, lib, probe)
    }

    fn hash_with_format(lib: &Library, format: ImageFormat) -> ContentHash {
        lib.image_hashes()
            .unwrap()
            .into_iter()
            .find(|h| lib.image(h).unwrap().unwrap().format == format)
            .expect("scan ingested the format")
    }

    #[test]
    fn serves_the_original_with_its_stored_format_content_type() {
        let (_tmp, lib, _probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Jpeg);
        let resp = serve(&lib, &format!("/original/{}", h.as_str()));
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "image/jpeg");
        assert_eq!(
            resp.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(resp.body().as_slice(), b"jpeg-original-bytes");
    }

    #[test]
    fn refuses_off_allowlist_stored_formats_with_404() {
        let (_tmp, lib, _probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Tiff);
        // Online and present on disk — refused purely by stored format.
        let resp = serve(&lib, &format!("/original/{}", h.as_str()));
        assert_eq!(resp.status(), 404);
    }

    // ---- /embedded against a real temp library -----------------------------

    /// Scripted extractor: one full-resolution preview for every RAW path
    /// (the §9.3.1 orientation-policy depth is covered by core's
    /// library_acceptance fixtures; here the wire contract is under test).
    struct ScriptedExtractor;

    impl EmbeddedPreviewExtractor for ScriptedExtractor {
        fn extract(&self, _path: &Path) -> Result<Option<ExtractedPreview>, PreviewError> {
            Ok(Some(ExtractedPreview {
                image: image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    3000,
                    2000,
                    image::Rgb([40, 90, 160]),
                )),
                raw_width: Some(3000),
                raw_height: Some(2000),
                exif_orientation: 1,
                preview_orientation: None,
            }))
        }
    }

    /// Temp library with one scanned root holding a JPEG and a RAW, previews
    /// generated (the embedded route requires the cached display artifact).
    fn raw_env() -> (tempfile::TempDir, Library, FakeVolumeProbe) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        let dir = mount.join("shoot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_0001.jpg"), b"jpeg-original-bytes").unwrap();
        std::fs::write(dir.join("IMG_0003.nef"), b"synthetic nef body").unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let lib = Library::open_with(
            tmp.path().join("photoproof.db"),
            tmp.path().join("previews"),
            LibraryOptions {
                probe: Arc::new(probe.clone()),
                extractor: Arc::new(ScriptedExtractor),
                ..Default::default()
            },
        )
        .unwrap();
        let root_id = lib.register_root(&dir, Some("shoot")).unwrap();
        lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
        lib.process_queue(&QueueOptions::default()).unwrap();
        (tmp, lib, probe)
    }

    #[test]
    fn serves_the_raw_embedded_native_jpeg() {
        let (_tmp, lib, _probe) = raw_env();
        let h = hash_with_format(&lib, ImageFormat::Raw);
        // The original route still refuses RAW (U12 allowlist unchanged)…
        assert_eq!(
            serve(&lib, &format!("/original/{}", h.as_str())).status(),
            404
        );
        // …and the embedded route serves the native-size JPEG instead.
        let resp = serve(&lib, &format!("/embedded/{}", h.as_str()));
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "image/jpeg");
        assert_eq!(
            resp.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        let decoded = image::load_from_memory(resp.body()).expect("decodable jpeg");
        use image::GenericImageView;
        // Native size — NOT the 2560-class display preview.
        assert_eq!(decoded.dimensions(), (3000, 2000));
    }

    #[test]
    fn embedded_refuses_non_raw_offline_and_unknown_with_404() {
        let (_tmp, lib, probe) = raw_env();
        // Non-RAW stored format: /original owns it; /embedded refuses.
        let jpeg = hash_with_format(&lib, ImageFormat::Jpeg);
        assert_eq!(
            serve(&lib, &format!("/embedded/{}", jpeg.as_str())).status(),
            404
        );
        // Unknown (never-ingested) hash.
        assert_eq!(
            serve(&lib, &format!("/embedded/{}", hash().as_str())).status(),
            404
        );
        // Offline volume (disk pulled): uniform refusal.
        let raw = hash_with_format(&lib, ImageFormat::Raw);
        probe.set_mounts(vec![]);
        lib.probe_volumes().unwrap();
        assert_eq!(
            serve(&lib, &format!("/embedded/{}", raw.as_str())).status(),
            404
        );
    }

    #[test]
    fn refuses_offline_volumes_and_unknown_hashes_with_404() {
        let (_tmp, lib, probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Jpeg);
        probe.set_mounts(vec![]); // disk pulled
        lib.probe_volumes().unwrap();
        assert_eq!(
            serve(&lib, &format!("/original/{}", h.as_str())).status(),
            404
        );
        // Unknown (never-ingested) hash.
        assert_eq!(
            serve(&lib, &format!("/original/{}", hash().as_str())).status(),
            404
        );
    }
}
