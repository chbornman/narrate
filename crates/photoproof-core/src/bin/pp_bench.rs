//! pp-bench — headless, re-runnable performance scenarios.
//!
//! The founder's benchmarking harness (June 2026): every run appends one
//! JSON line per scenario to a results file, so regressions and wins are
//! DIFFS over time, not recollections. Scenario-based by design — ingest
//! is the first tenant; search/fold/rebuild scenarios join as flags on
//! this same binary so one results file tells the whole story.
//!
//! Usage (scripts/bench.sh wraps the release build):
//!   pp-bench ingest --files 2000 [--edge 4000] [--label "smb-test"]
//!            [--source /path/to/real/folder] [--out bench-results.jsonl]
//!
//! Modes:
//! - SYNTHETIC (default): a deterministic corpus of JPEGs is generated
//!   into a tempdir (seeded per-index pixel patterns — identical bytes
//!   every run, so hash work is comparable across runs).
//! - --source <dir>: ingest a real folder IN PLACE, read-only — the
//!   library, cache, and database live in the tempdir and are discarded;
//!   the source files are only ever read. (No marker is ever written:
//!   the bench probe reports its volume as a system root, which
//!   maybe_write_marker skips by rule, plus read-only belt.)
//!
//! Output schema (one JSON object per line, schema=1):
//!   { schema, ts, label, scenario, mode, files, bytes,
//!     scan_ms, drain_ms, total_ms, files_per_s, mb_per_s,
//!     stages: [{stage, count, total_ms, mean_ms, max_ms}],
//!     host: {os, arch, cores} }
//!
//! Honest-numbers notes: run the RELEASE build (the dev profile's
//! opt-level table makes debug numbers meaningless); close other heavy
//! processes; synthetic JPEGs exercise hash + decode + resize + encode
//! but NOT RAW extraction — point --source at a RAW folder for that.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use photoproof_core::library::{
    FakeVolumeProbe, Library, LibraryOptions, PlatformIdKind, PlatformPlaceholderDetector,
    ProbedVolume, QueueOptions, RawlerExtractor, ScanOptions,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("ingest") {
        eprintln!(
            "usage: pp-bench ingest [--files N] [--edge PX] [--source DIR] [--label S] [--out FILE]"
        );
        std::process::exit(2);
    }
    let mut files: usize = 500;
    let mut edge: u32 = 3000;
    let mut source: Option<PathBuf> = None;
    let mut label = String::from("");
    let mut out = PathBuf::from("bench-results.jsonl");
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--files" => files = val().parse().expect("--files N"),
            "--edge" => edge = val().parse().expect("--edge PX"),
            "--source" => source = Some(PathBuf::from(val())),
            "--label" => label = val(),
            "--out" => out = PathBuf::from(val()),
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
    }

    // No tempfile dependency in the bin path: a pid-unique dir under the
    // OS tempdir, best-effort removed at the end (a crash leaves only
    // bench litter the OS tempdir policy reaps).
    let tmp = std::env::temp_dir().join(format!("pp-bench-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("bench tempdir");
    let (root_dir, mode) = match &source {
        Some(dir) => (dir.clone(), "source"),
        None => {
            let dir = tmp.join("corpus");
            std::fs::create_dir_all(&dir).expect("corpus dir");
            eprintln!("generating {files} synthetic JPEGs ({edge}px long edge)…");
            generate_corpus(&dir, files, edge);
            (dir, "synthetic")
        }
    };

    // The bench volume: the FAKE probe reports exactly one volume hosting
    // the corpus — hermetic (no marker writes on --source: read_only), and
    // identical identity every run.
    let probe = FakeVolumeProbe::new();
    probe.set_mounts(vec![ProbedVolume {
        mount_point: root_dir.clone(),
        platform_id: Some("pp-bench-volume".into()),
        platform_kind: PlatformIdKind::Heuristic,
        label: Some("pp-bench".into()),
        fs_type: None,
        capacity_bytes: None,
        read_only_flag: source.is_some(),
        // System roots never get a marker written (maybe_write_marker's
        // first gate) — the guarantee that --source stays untouched.
        is_system_root: true,
        coarse_mtime: false,
    }]);
    let lib = Arc::new(
        Library::open_with(
            tmp.join("photoproof.db"),
            tmp.join("cache"),
            LibraryOptions {
                probe: Arc::new(probe),
                placeholders: Arc::new(PlatformPlaceholderDetector),
                extractor: Arc::new(RawlerExtractor),
            },
        )
        .expect("open library"),
    );
    let root = lib
        .register_root(&root_dir, Some("bench"))
        .expect("register root");

    // ---- the measured section -------------------------------------------------
    let t0 = Instant::now();
    let scan = lib.scan_root(&root, &ScanOptions::default()).expect("scan");
    let scan_ms = t0.elapsed().as_millis() as u64;

    let t1 = Instant::now();
    let report = lib.process_queue(&QueueOptions::default()).expect("drain");
    let drain_ms = t1.elapsed().as_millis() as u64;
    let total_ms = t0.elapsed().as_millis() as u64;
    // -----------------------------------------------------------------------------

    let file_count = scan.files_seen;
    let bytes: u64 = walk_bytes(&root_dir);
    let secs = (total_ms as f64 / 1000.0).max(0.001);
    let stages: Vec<String> = lib
        .metrics_snapshot()
        .iter()
        .map(|s| {
            format!(
                r#"{{"stage":"{}","count":{},"total_ms":{:.1},"mean_ms":{:.2},"max_ms":{:.1}}}"#,
                s.stage, s.count, s.total_ms, s.mean_ms, s.max_ms
            )
        })
        .collect();
    let line = format!(
        r#"{{"schema":1,"ts":"{}","label":"{}","scenario":"ingest","mode":"{}","files":{},"bytes":{},"scan_ms":{},"drain_ms":{},"total_ms":{},"queue_done":{},"queue_errors":{},"files_per_s":{:.1},"mb_per_s":{:.1},"stages":[{}],"host":{{"os":"{}","arch":"{}","cores":{}}}}}"#,
        rfc3339_now(),
        label.replace('"', "'"),
        mode,
        file_count,
        bytes,
        scan_ms,
        drain_ms,
        total_ms,
        report.done,
        report.errors,
        file_count as f64 / secs,
        bytes as f64 / 1_000_000.0 / secs,
        stages.join(","),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0),
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .expect("open results file");
    writeln!(f, "{line}").expect("append result");
    println!("{line}");
    eprintln!(
        "\n{} files, {:.1} MB in {:.1}s — scan {:.1}s, drain {:.1}s → {:.1} files/s, {:.1} MB/s\nappended to {}",
        file_count,
        bytes as f64 / 1_000_000.0,
        total_ms as f64 / 1000.0,
        scan_ms as f64 / 1000.0,
        drain_ms as f64 / 1000.0,
        file_count as f64 / secs,
        bytes as f64 / 1_000_000.0 / secs,
        out.display()
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Deterministic corpus: per-index seeded gradient JPEGs — identical bytes
/// on every run (hash workload comparable), unique content per file (no
/// dedup collapse), realistic decode/resize/encode cost at `edge` px.
fn generate_corpus(dir: &Path, files: usize, edge: u32) {
    use image::codecs::jpeg::JpegEncoder;
    let (w, h) = (edge, edge * 2 / 3);
    for i in 0..files {
        let seed = (i as u32).wrapping_mul(2_654_435_761);
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            // Cheap per-pixel variety: enough entropy that WebP/JPEG do
            // real work, fully deterministic.
            let v = x
                .wrapping_mul(31)
                .wrapping_add(y.wrapping_mul(17))
                .wrapping_add(seed);
            image::Rgb([
                (v & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                ((v >> 4) & 0xff) as u8,
            ])
        });
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 88)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .expect("encode corpus jpeg");
        std::fs::write(dir.join(format!("bench_{i:05}.jpg")), bytes).expect("write corpus file");
    }
}

fn walk_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// RFC 3339 UTC without a chrono dependency in the bin path.
fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days-to-civil (Howard Hinnant's algorithm) — bench metadata, not
    // journal truth; leap seconds don't matter here.
    let days = (secs / 86_400) as i64;
    let mut y = (days + 719_468) / 146_097 * 400;
    let doe = (days + 719_468) % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    y += yoe;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let t = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}
