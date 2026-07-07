//! T1 (AUDIT-2026-07-07 §5): preview-serve latency + concurrency budgets for
//! the `photoproof://` protocol — /thumb/<hash> over a temp library holding
//! 1k thumb artifacts, measured shuffled cold and warm, plus a 200-way
//! concurrency case fired through the SAME bounded serve pool the protocol
//! registration in lib.rs uses (locking in the F1 fix: a fixed worker pool,
//! not a transient OS thread per request).
//!
//! `#[ignore]`d, matching the search_latency.rs idiom: run it in release:
//!
//! ```text
//! cargo test -p photoproof-desktop --release --test preview_serve_latency -- --ignored --nocapture
//! ```
//!
//! Budgets only arm in release builds; debug runs still execute and print
//! the measured numbers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use photoproof_core::ContentHash;
use photoproof_core::library::{
    ArtifactKind, FakeVolumeProbe, Library, LibraryOptions, artifact_path,
};
use photoproof_desktop::protocol;

const IMAGES: usize = 1000;
/// The size class of a real 512 px WebP thumb (~15–40 KB in practice), so
/// the measured fs::read cost is representative, not a 4-byte toy.
const THUMB_BYTES: usize = 24 * 1024;

/// Deterministic xorshift64* (the search_latency.rs idiom) — reproducible
/// payloads and shuffles, no extra deps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Temp library with IMAGES thumb artifacts dropped straight into the cache
/// slots `artifact_path` addresses.
///
/// WHY direct writes rather than a real ingest + preview pass: the /thumb
/// route is content-addressed and serves the slot's bytes verbatim — it
/// never decodes and never touches the DB — so the latency contract needs
/// bytes in the right slots, nothing more. This is the same shortcut the
/// full-decode unit test in protocol.rs uses ("the route serves the bytes
/// verbatim ... so opaque bytes suffice for the wire contract"); a RIFF/WEBP
/// magic prefix keeps the payloads shaped like the real artifacts.
fn build_env() -> (tempfile::TempDir, Arc<Library>, Vec<ContentHash>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_with(
        tmp.path().join("photoproof.db"),
        tmp.path().join("previews"),
        LibraryOptions {
            // Hermetic: never probe the host's real volumes (the artifact
            // route needs only the cache dir, but open takes a probe).
            probe: Arc::new(FakeVolumeProbe::new()),
            ..Default::default()
        },
    )
    .expect("open temp library");
    let mut rng = Rng(0x0BAD_5EED_0BAD_5EED);
    let mut hashes = Vec::with_capacity(IMAGES);
    for i in 0..IMAGES {
        let h = ContentHash::from_bytes_of(format!("preview-serve-{i}").as_bytes());
        let file = artifact_path(lib.cache_dir(), &h, ArtifactKind::Thumb);
        std::fs::create_dir_all(file.parent().expect("artifact parent")).expect("cache subdir");
        let mut bytes = Vec::with_capacity(THUMB_BYTES + 8);
        bytes.extend_from_slice(b"RIFF\0\0\0\0WEBP");
        while bytes.len() < THUMB_BYTES {
            bytes.extend_from_slice(&rng.next().to_le_bytes());
        }
        // Exact size, so the served-length assertions are byte-precise.
        bytes.truncate(THUMB_BYTES);
        std::fs::write(&file, &bytes).expect("write thumb artifact");
        hashes.push(h);
    }
    (tmp, Arc::new(lib), hashes)
}

/// Fisher–Yates with the deterministic Rng: every pass visits every artifact
/// exactly once in a scattered order (a fling-scroll's access pattern, not a
/// directory-order walk that would flatter the FS).
fn shuffled(hashes: &[ContentHash], seed: u64) -> Vec<ContentHash> {
    let mut v = hashes.to_vec();
    let mut rng = Rng(seed);
    for i in (1..v.len()).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        v.swap(i, j);
    }
    v
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// One timed pass over a shuffled order; every response must be a 200 (a 404
/// would be measuring the error path, not a serve).
fn timed_pass(lib: &Library, order: &[ContentHash]) -> Vec<f64> {
    let mut samples_ms = Vec::with_capacity(order.len());
    for h in order {
        let path = format!("/thumb/{}", h.as_str());
        let t = Instant::now();
        let resp = protocol::serve(lib, &path);
        samples_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(resp.status(), 200, "thumb artifact must serve");
        assert_eq!(resp.body().len(), THUMB_BYTES, "full payload served");
    }
    samples_ms.sort_by(f64::total_cmp);
    samples_ms
}

#[test]
#[ignore = "builds a 1k-artifact preview cache; run in release with --ignored"]
fn thumb_serve_latency_p50_p99_cold_and_warm() {
    let (_tmp, lib, hashes) = build_env();

    // "Cold" = first serve of each artifact in this process (path assembly,
    // first open of each slot). True disk-cold is not portably reachable —
    // the artifacts were just written, so the OS page cache is warm — which
    // is WHY only the warm pass is budgeted: it is the steady state the
    // grid actually scrolls against, and the cold numbers are reported for
    // trend-watching only.
    let cold = timed_pass(&lib, &shuffled(&hashes, 0xC01D_C01D_C01D_C01D));
    let warm = timed_pass(&lib, &shuffled(&hashes, 0xAAAA_BBBB_CCCC_DDDD));

    println!(
        "thumb serve over {IMAGES} shuffled artifacts ({} KB each): \
         cold p50 {:.3} ms p99 {:.3} ms max {:.3} ms | \
         warm p50 {:.3} ms p99 {:.3} ms max {:.3} ms",
        THUMB_BYTES / 1024,
        percentile(&cold, 0.50),
        percentile(&cold, 0.99),
        cold[cold.len() - 1],
        percentile(&warm, 0.50),
        percentile(&warm, 0.99),
        warm[warm.len() - 1],
    );

    // Release-only budget (debug reports without failing, like
    // search_latency.rs). Measured warm p99 is ~0.02 ms on an M1; 2 ms is
    // ~100x headroom for CI jitter and slower disks while still tripping on
    // a real regression class — a per-request thread spawn, an accidental
    // DB hop, or an O(n) scan sneaking onto the serve path.
    if !cfg!(debug_assertions) {
        let p99_warm = percentile(&warm, 0.99);
        assert!(
            p99_warm < 2.0,
            "warm thumb-serve p99 {p99_warm:.3} ms exceeds the 2 ms budget"
        );
    }
}

/// F1's regression guard: 200 concurrent /thumb serves fired through the
/// SAME `protocol::serve_pool()` the lib.rs registration uses. This pins
/// (a) the shipping mechanism is a FIXED pool — the worker bound is asserted
/// directly, so a return to unbounded per-request spawn fails here — and
/// (b) `protocol::serve` is sound and timely under 200-way submission:
/// every request answers 200 with the full payload, inside a generous
/// wall-clock budget.
#[test]
#[ignore = "builds a 1k-artifact preview cache; run in release with --ignored"]
fn two_hundred_concurrent_serves_stay_bounded_and_timely() {
    const CONCURRENT: usize = 200;
    let (_tmp, lib, hashes) = build_env();

    let pool = protocol::serve_pool();
    // The bound itself is the contract: core-count clamped to [2, 8]
    // (WHY on serve_pool). CONCURRENT >> workers is the point — the burst
    // must QUEUE, not spawn.
    assert!(
        (2..=8).contains(&pool.workers()),
        "serve pool must stay a small fixed size, got {}",
        pool.workers()
    );

    // Warm the slots so the wall-clock number measures pool throughput,
    // not first-touch noise.
    for h in hashes.iter().take(CONCURRENT) {
        protocol::serve(&lib, &format!("/thumb/{}", h.as_str()));
    }

    let (done_tx, done_rx) = std::sync::mpsc::channel::<(u16, usize)>();
    let t0 = Instant::now();
    for h in shuffled(&hashes, 0xF1F1_F1F1_F1F1_F1F1)
        .into_iter()
        .take(CONCURRENT)
    {
        let lib = Arc::clone(&lib);
        let tx = done_tx.clone();
        pool.run(move || {
            let resp = protocol::serve(&lib, &format!("/thumb/{}", h.as_str()));
            // A send failure means the collector already gave up; nothing
            // useful to do from a worker.
            let _ = tx.send((resp.status().as_u16(), resp.body().len()));
        });
    }
    drop(done_tx); // collector's recv ends when the last job's clone drops

    let mut completed = 0usize;
    // Timeout instead of a bare recv: a wedged pool must fail the test, not
    // hang the suite.
    while let Ok((status, len)) = done_rx.recv_timeout(Duration::from_secs(30)) {
        assert_eq!(status, 200, "every concurrent serve must succeed");
        assert_eq!(len, THUMB_BYTES, "full payload under concurrency");
        completed += 1;
        if completed == CONCURRENT {
            break;
        }
    }
    let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(completed, CONCURRENT, "all queued serves must complete");

    println!(
        "{CONCURRENT} concurrent thumb serves through the {}-worker pool: \
         {wall_ms:.2} ms wall ({:.3} ms/serve amortized)",
        pool.workers(),
        wall_ms / CONCURRENT as f64
    );

    // Release-only sanity budget. Measured ~2 ms wall on an M1 (200 warm
    // 24 KB reads across up to 8 workers); 1000 ms is huge headroom but
    // still catches gross serialization — e.g. the dequeue lock held across
    // a job, or the queue wedging — which is the failure class this guards.
    if !cfg!(debug_assertions) {
        assert!(
            wall_ms < 1000.0,
            "200-way concurrent serve took {wall_ms:.0} ms; the pool is serializing or wedged"
        );
    }
}
