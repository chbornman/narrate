//! spec/LIBRARY.md §13 acceptance criteria, one named test per criterion
//! (`l13_NN_*`). Headless, real temp filesystems, injected volume probes /
//! placeholder detectors / preview extractors, deterministic watcher clocks.
//!
//! Hashing assertions use the process-global instrumentation in
//! `library::hashing`; tests that read it serialize on `guard()` so parallel
//! test threads cannot pollute each other's deltas.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use image::{DynamicImage, RgbImage};
use photoproof_core::library::{
    ArtifactKind, DebounceConfig, EmbeddedPreviewExtractor, ExtractedPreview, FakeVolumeProbe,
    Library, LibraryOptions, MARKER_FILENAME, Observed, PipelineEffect, PlatformIdKind,
    PreviewError, PreviewSource, ProbedVolume, QueueOptions, RawWatchEvent, ScanOptions,
    SharedSetPlaceholderDetector, hash_invocation_count,
};
use photoproof_core::{
    ContentHash, EventDraft, EventStore, RemarkSource, SessionContext, SessionId, StrokePayload,
    StrokePoint, Tool, UtcMillis,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Serializes tests in this binary: the hashing instrumentation is process-
/// global, so concurrent tests would corrupt each other's invocation deltas.
fn guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn scaled(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Injectable embedded-preview extractor: per-filename scripted previews,
/// `None` for everything else (no embedded preview found).
#[derive(Default)]
struct FakeExtractor {
    specs: Mutex<std::collections::HashMap<String, FakeSpec>>,
}

#[derive(Clone)]
struct FakeSpec {
    /// The preview as stored in the container (pre-orientation).
    preview: RgbImage,
    raw_dims: Option<(u32, u32)>,
    exif_orientation: u16,
    preview_orientation: Option<u16>,
}

impl FakeExtractor {
    fn script(&self, file_name: &str, spec: FakeSpec) {
        self.specs
            .lock()
            .unwrap()
            .insert(file_name.to_owned(), spec);
    }
}

impl EmbeddedPreviewExtractor for FakeExtractor {
    fn extract(&self, path: &Path) -> Result<Option<ExtractedPreview>, PreviewError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let spec = self.specs.lock().unwrap().get(name).cloned();
        Ok(spec.map(|s| ExtractedPreview {
            image: DynamicImage::ImageRgb8(s.preview),
            raw_width: s.raw_dims.map(|(w, _)| w),
            raw_height: s.raw_dims.map(|(_, h)| h),
            exif_orientation: s.exif_orientation,
            preview_orientation: s.preview_orientation,
        }))
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    mount: PathBuf,
    db: PathBuf,
    cache: PathBuf,
    probe: FakeVolumeProbe,
    placeholders: SharedSetPlaceholderDetector,
    extractor: Arc<FakeExtractor>,
    lib: Arc<Library>,
}

fn probed(mount: &Path) -> ProbedVolume {
    ProbedVolume {
        mount_point: mount.to_path_buf(),
        platform_id: Some("uuid-test-0001".into()),
        platform_kind: PlatformIdKind::LinuxFsUuid,
        label: Some("TestVol".into()),
        fs_type: Some("ext4".into()),
        capacity_bytes: Some(1 << 30),
        read_only_flag: false,
        is_system_root: false,
        coarse_mtime: false,
    }
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        std::fs::create_dir_all(&mount).unwrap();
        let db = tmp.path().join("photoproof.db");
        let cache = tmp.path().join("cache");
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let placeholders = SharedSetPlaceholderDetector::new();
        let extractor = Arc::new(FakeExtractor::default());
        let lib = open_lib(&db, &cache, &probe, &placeholders, &extractor);
        Env {
            _tmp: tmp,
            mount,
            db,
            cache,
            probe,
            placeholders,
            extractor,
            lib,
        }
    }

    /// Simulate an app restart: reopen against the same DB + cache (running
    /// pass rows revert to pending, stranded cache temp files are swept).
    fn reopen(&mut self) {
        self.lib = open_lib(
            &self.db,
            &self.cache,
            &self.probe,
            &self.placeholders,
            &self.extractor,
        );
    }

    fn register(&self, sub: &str) -> String {
        let dir = self.mount.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        self.lib.register_root(&dir, Some(sub)).unwrap()
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.mount.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn scan(&self, root_id: &str) -> photoproof_core::library::ScanReport {
        self.lib
            .scan_root(root_id, &ScanOptions::default())
            .unwrap()
    }

    fn drain_queue(&self) -> photoproof_core::library::QueueReport {
        self.lib.process_queue(&QueueOptions::default()).unwrap()
    }

    fn conn(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .unwrap();
        conn
    }

    fn store(&self) -> (EventStore, SessionId) {
        let store = EventStore::open(&self.db).unwrap();
        let session = store
            .open_session(SessionContext {
                app_version: "0.1.0".into(),
                device_id: "0123456789abcdef0123456789abcdef".into(),
                root_context: None,
            })
            .unwrap();
        (store, session)
    }

    fn only_hash(&self) -> ContentHash {
        let hashes = self.lib.image_hashes().unwrap();
        assert_eq!(hashes.len(), 1);
        hashes.into_iter().next().unwrap()
    }

    fn volume_id(&self) -> String {
        let vols = self.lib.volumes().unwrap();
        assert_eq!(vols.len(), 1);
        vols[0].volume_id.clone()
    }
}

fn open_lib(
    db: &Path,
    cache: &Path,
    probe: &FakeVolumeProbe,
    placeholders: &SharedSetPlaceholderDetector,
    extractor: &Arc<FakeExtractor>,
) -> Arc<Library> {
    Arc::new(
        Library::open_with(
            db,
            cache,
            LibraryOptions {
                probe: Arc::new(probe.clone()),
                placeholders: Arc::new(placeholders.clone()),
                extractor: Arc::clone(extractor) as Arc<dyn EmbeddedPreviewExtractor>,
            },
        )
        .unwrap(),
    )
}

// -- fixtures ----------------------------------------------------------------

/// A small decodable JPEG whose bytes are unique per seed.
fn unique_jpeg(seed: u32) -> Vec<u8> {
    let mut img = RgbImage::new(16, 16);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = seed
            .wrapping_mul(2_654_435_761)
            .wrapping_add(x * 31 + y * 7);
        *p = image::Rgb([
            (v & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
        ]);
    }
    encode_jpeg(&img)
}

fn encode_jpeg(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut Cursor::new(&mut out), 95)
        .encode_image(&DynamicImage::ImageRgb8(img.clone()))
        .unwrap();
    out
}

/// Minimal EXIF APP1 segment carrying only the orientation tag.
fn exif_app1(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
    tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes()); // value padding
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    let mut seg = Vec::new();
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    let mut out = vec![0xFF, 0xE1];
    out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&seg);
    out
}

/// JPEG with an EXIF orientation tag spliced in after SOI.
fn jpeg_with_orientation(stored: &RgbImage, orientation: u16) -> Vec<u8> {
    let plain = encode_jpeg(stored);
    let mut out = Vec::with_capacity(plain.len() + 64);
    out.extend_from_slice(&plain[..2]); // SOI
    out.extend_from_slice(&exif_app1(orientation));
    out.extend_from_slice(&plain[2..]);
    out
}

/// Hand-rolled uncompressed RGB TIFF with an Orientation tag in IFD0.
fn tiff_with_orientation(stored: &RgbImage, orientation: u16) -> Vec<u8> {
    let (w, h) = (stored.width(), stored.height());
    let data = stored.as_raw();
    let data_off = 8u32;
    let bits_off = data_off + data.len() as u32;
    let ifd_off = bits_off + 6;
    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&ifd_off.to_le_bytes());
    out.extend_from_slice(data);
    for _ in 0..3 {
        out.extend_from_slice(&8u16.to_le_bytes()); // BitsPerSample values
    }
    let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    };
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    out.extend_from_slice(&10u16.to_le_bytes()); // entry count
    entry(&mut out, 256, LONG, 1, w); // ImageWidth
    entry(&mut out, 257, LONG, 1, h); // ImageLength
    entry(&mut out, 258, SHORT, 3, bits_off); // BitsPerSample
    entry(&mut out, 259, SHORT, 1, 1); // Compression = none
    entry(&mut out, 262, SHORT, 1, 2); // Photometric = RGB
    entry(&mut out, 273, LONG, 1, data_off); // StripOffsets
    entry(&mut out, 274, SHORT, 1, u32::from(orientation)); // Orientation
    entry(&mut out, 277, SHORT, 1, 3); // SamplesPerPixel
    entry(&mut out, 278, LONG, 1, h); // RowsPerStrip
    entry(&mut out, 279, LONG, 1, data.len() as u32); // StripByteCounts
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    out
}

/// Display-oriented test pattern: blue field, red square in the top-left.
fn display_pattern(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::from_pixel(w, h, image::Rgb([20, 40, 200]));
    let m = (w.min(h) / 5).max(8);
    for y in 0..m {
        for x in 0..m {
            img.put_pixel(x, y, image::Rgb([220, 30, 30]));
        }
    }
    img
}

/// Inverse-orient a display image so that applying `orientation` yields it.
fn store_for_orientation(display: &RgbImage, orientation: u16) -> RgbImage {
    let d = DynamicImage::ImageRgb8(display.clone());
    match orientation {
        1 => display.clone(),
        3 => d.rotate180().into_rgb8(),
        6 => d.rotate270().into_rgb8(), // applying 6 = rotate90 CW
        8 => d.rotate90().into_rgb8(),  // applying 8 = rotate270
        o => panic!("unsupported test orientation {o}"),
    }
}

fn is_red(p: image::Rgba<u8>) -> bool {
    p[0] > 130 && p[0] > p[2]
}

fn is_blue(p: image::Rgba<u8>) -> bool {
    p[2] > 130 && p[2] > p[0]
}

/// Decode a cached artifact and assert the display-oriented pattern: red
/// top-left, blue bottom-right, expected aspect (portrait vs landscape).
fn assert_upright(file: &Path, expect_portrait: bool, context: &str) -> (u32, u32) {
    let img = image::open(file)
        .unwrap_or_else(|e| panic!("{context}: artifact decode failed: {e}"))
        .into_rgba8();
    let (w, h) = (img.width(), img.height());
    assert_eq!(
        h > w,
        expect_portrait,
        "{context}: artifact is {w}x{h}, expected portrait={expect_portrait}"
    );
    let probe = (w.min(h) / 10).max(2);
    assert!(
        is_red(*img.get_pixel(probe, probe)),
        "{context}: top-left not red at {probe},{probe} in {w}x{h} — rotation wrong"
    );
    assert!(
        is_blue(*img.get_pixel(w - 1 - probe, h - 1 - probe)),
        "{context}: bottom-right not blue — rotation wrong"
    );
    (w, h)
}

fn set_mtime(path: &Path, t: std::time::SystemTime) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t))
        .unwrap();
}

fn shift_mtime(path: &Path, delta: std::time::Duration) {
    let m = std::fs::metadata(path).unwrap().modified().unwrap();
    set_mtime(path, m + delta);
}

fn d_remark(text: &str, target: &ContentHash) -> EventDraft {
    EventDraft::Remark {
        source: RemarkSource::Typed,
        text: text.into(),
        targets: vec![target.clone()],
    }
}

fn d_stroke(target: &ContentHash) -> EventDraft {
    EventDraft::Stroke {
        payload: StrokePayload {
            base_w: 40,
            orientation: 1,
            points: vec![
                StrokePoint {
                    x: 100,
                    y: 100,
                    p: 500,
                    t: 0,
                },
                StrokePoint {
                    x: 200,
                    y: 200,
                    p: 500,
                    t: 16,
                },
            ],
            tool: Tool::Pencil,
        },
        target: target.clone(),
        linked_event: None,
    }
}

fn at(base_ms: i64, offset_ms: i64) -> UtcMillis {
    UtcMillis::from_epoch_ms(base_ms + offset_ms)
}

// ---------------------------------------------------------------------------
// 1. Relink, running
// ---------------------------------------------------------------------------

#[test]
fn l13_01_relink_running() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(1));
    env.write("photos/b.jpg", &unique_jpeg(2));
    env.scan(&root);
    env.drain_queue();
    assert_eq!(env.lib.image_count().unwrap(), 2);

    let (store, session) = env.store();
    let conn = env.conn();
    let hash_at = |rel: &str| -> ContentHash {
        let h: String = conn
            .query_row(
                "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                [rel],
                |r| r.get(0),
            )
            .unwrap();
        ContentHash::from_hex(&h).unwrap()
    };
    let hash_a = hash_at("photos/a.jpg");
    let hash_b = hash_at("photos/b.jpg");
    store
        .append(&session, d_remark("the journal", &hash_a), None)
        .unwrap();

    let base = UtcMillis::now().epoch_ms();
    let mut pl = env
        .lib
        .watch_pipeline(&root, DebounceConfig::default())
        .unwrap();
    let before = hash_invocation_count();

    // Paired rename: direct relink, zero hashing.
    std::fs::create_dir_all(env.mount.join("photos/sub")).unwrap();
    let from = env.mount.join("photos/a.jpg");
    let to = env.mount.join("photos/sub/renamed.jpg");
    std::fs::rename(&from, &to).unwrap();
    let effects = pl
        .push(RawWatchEvent::Renamed { from, to }, at(base, 0))
        .unwrap();
    assert_eq!(
        effects,
        vec![PipelineEffect::RenameRelinked {
            from: "photos/a.jpg".into(),
            to: "photos/sub/renamed.jpg".into(),
        }]
    );
    assert_eq!(
        hash_invocation_count(),
        before,
        "a paired rename never hashes"
    );
    assert_eq!(hash_at("photos/sub/renamed.jpg"), hash_a, "same image_hash");
    let (state, reason): (String, String) = conn
        .query_row(
            "SELECT state, stale_reason FROM paths WHERE rel_path = 'photos/a.jpg'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), reason.as_str()), ("stale", "moved"));

    // Unpaired remove + create (same mutation through §7.2): correlated move.
    let from_b = env.mount.join("photos/b.jpg");
    let to_b = env.mount.join("photos/moved-b.jpg");
    std::fs::rename(&from_b, &to_b).unwrap();
    pl.push(RawWatchEvent::Removed(from_b), at(base, 100))
        .unwrap();
    pl.push(RawWatchEvent::Created(to_b), at(base, 100))
        .unwrap();
    let eff1 = pl.tick(at(base, 700)).unwrap();
    assert!(eff1.contains(&PipelineEffect::Removed {
        rel_path: "photos/b.jpg".into()
    }));
    let eff2 = pl.tick(at(base, 2800)).unwrap();
    assert_eq!(
        eff2,
        vec![PipelineEffect::Observed {
            rel_path: "photos/moved-b.jpg".into(),
            outcome: Observed::Relinked(hash_b.clone()),
        }]
    );
    let reason_b: String = conn
        .query_row(
            "SELECT stale_reason FROM paths WHERE rel_path = 'photos/b.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason_b, "moved", "remove→create within 10 s is a move");

    // Journal intact; zero new images rows.
    assert_eq!(env.lib.image_count().unwrap(), 2);
    let journal = store.folded_journal(&hash_a).unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(
        env.lib.availability(&hash_a).unwrap(),
        photoproof_core::library::Availability::Available
    );
}

// ---------------------------------------------------------------------------
// 2. Relink, stopped
// ---------------------------------------------------------------------------

#[test]
fn l13_02_relink_stopped() {
    let _g = guard();
    let mut env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(10));
    env.write("photos/b.jpg", &unique_jpeg(11));
    env.scan(&root);
    env.drain_queue();
    let conn = env.conn();
    let hash_a: String = conn
        .query_row(
            "SELECT image_hash FROM paths WHERE rel_path = 'photos/a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Stop the app; mutate the filesystem; restart; reconcile.
    env.reopen();
    std::fs::create_dir_all(env.mount.join("photos/sub")).unwrap();
    std::fs::rename(
        env.mount.join("photos/a.jpg"),
        env.mount.join("photos/sub/renamed.jpg"),
    )
    .unwrap();
    env.probe_online_and_scan(&root);

    // Identical end state as l13_01's paired rename.
    let conn = env.conn();
    let (state, reason): (String, String) = conn
        .query_row(
            "SELECT state, stale_reason FROM paths WHERE rel_path = 'photos/a.jpg'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), reason.as_str()), ("stale", "moved"));
    let new_hash: String = conn
        .query_row(
            "SELECT image_hash FROM paths WHERE rel_path = 'photos/sub/renamed.jpg' AND state = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        new_hash, hash_a,
        "same image_hash after startup reconciliation"
    );
    assert_eq!(env.lib.image_count().unwrap(), 2, "zero new images rows");
}

impl Env {
    /// Startup sequence: probe volumes online, then reconcile the root.
    fn probe_online_and_scan(&self, root: &str) {
        self.lib.probe_volumes().unwrap();
        self.scan(root);
    }
}

// ---------------------------------------------------------------------------
// 3. Interrupt / resume
// ---------------------------------------------------------------------------

#[test]
fn l13_03_interrupt_resume() {
    let _g = guard();
    // Scaled default for the debug-mode gate; raise via env to the spec's
    // 10k population (flagged in the packet report; full run is part of the
    // P4.1 perf pass / founder-machine checklist).
    let n = scaled("PHOTOPROOF_TEST_INGEST_FILES", 1000);
    let mut env = Env::new();
    let root = env.register("photos");
    let mut expected: Vec<(String, ContentHash)> = Vec::with_capacity(n);
    for i in 0..n {
        let rel = format!("photos/d{:02}/img{:05}.jpg", i % 17, i);
        let bytes = unique_jpeg(i as u32 + 1000);
        // Independent ground truth: hash computed from bytes, not via the
        // library's pipeline.
        expected.push((rel.clone(), ContentHash::from_bytes_of(&bytes)));
        env.write(&rel, &bytes);
    }

    // Interrupted rounds at varied points, with a simulated mid-preview-write
    // crash artifact each round; each restart must recover (running→pending,
    // temp sweep) and make monotone progress.
    for round in 0..6u64 {
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let delay = 30 + round * 120;
        let killer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            c2.store(true, Ordering::Relaxed);
        });
        let _ = env.lib.scan_root(
            &root,
            &ScanOptions {
                cancel: Some(Arc::clone(&cancel)),
                ..Default::default()
            },
        );
        let _ = env.lib.process_queue(&QueueOptions {
            cancel: Some(Arc::clone(&cancel)),
            max_items: None,
        });
        killer.join().unwrap();
        // Torn-write simulation: a stranded temp file in the cache.
        let torn_dir = env.cache.join("previews").join("ab").join("cd");
        std::fs::create_dir_all(&torn_dir).unwrap();
        std::fs::write(torn_dir.join(".pp-tmp-999-x.webp"), b"torn").unwrap();
        env.reopen();
    }

    // Final clean run to completion.
    let report = env.scan(&root);
    assert!(!report.cancelled);
    env.drain_queue();

    // Exactly one images row per unique hash; no misses vs the independent
    // walk; every pass done (full-raw-decode skipped: plain JPEGs).
    assert_eq!(env.lib.image_count().unwrap(), n as i64);
    let conn = env.conn();
    for (rel, hash) in &expected {
        let stored: String = conn
            .query_row(
                "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                [rel],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("missing active path for {rel}: {e}"));
        assert_eq!(&stored, hash.as_str(), "{rel}");
    }
    let counters = env.lib.pass_counters().unwrap();
    let c = |name: &str| {
        counters
            .get(&(name.to_string(), 1))
            .copied()
            .unwrap_or_default()
    };
    assert_eq!(c("hash").done, n as u64);
    assert_eq!(c("exif").done, n as u64);
    assert_eq!(c("preview").done, n as u64);
    assert_eq!(c("full-raw-decode").skipped, n as u64);
    assert_eq!(c("exif").pending + c("preview").pending, 0);
    assert_eq!(c("exif").running + c("preview").running, 0);

    // No torn cache files: temp files swept, every artifact decodes.
    let mut artifacts = 0;
    for entry in walk_files(&env.cache) {
        let name = entry.file_name().unwrap().to_str().unwrap();
        assert!(
            !name.starts_with(".pp-tmp-"),
            "stranded temp file {name} survived restart"
        );
        if name.ends_with(".webp") {
            artifacts += 1;
            image::open(&entry).unwrap_or_else(|e| panic!("torn artifact {name}: {e}"));
        }
    }
    assert_eq!(artifacts, 2 * n, "thumb + display per image");
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 4. Duplicates (K13)
// ---------------------------------------------------------------------------

#[test]
fn l13_04_duplicates() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    let bytes = unique_jpeg(4242);
    let n = 500;
    for i in 0..n {
        env.write(&format!("photos/d{i:03}/dup.jpg"), &bytes);
    }
    env.scan(&root);
    env.drain_queue();

    // One image, N active path rows.
    assert_eq!(env.lib.image_count().unwrap(), 1);
    let hash = env.only_hash();
    let rows = env.lib.paths_for_image(&hash).unwrap();
    assert_eq!(rows.len(), n);
    assert!(rows.iter().all(|r| r.active));

    // Annotating "via any path" lands in the one journal: every path row
    // resolves to the same hash, which is the journal key.
    let (store, session) = env.store();
    let via_any_path = rows[n / 2].image_hash.clone();
    assert_eq!(via_any_path, hash);
    store
        .append(&session, d_remark("one journal", &via_any_path), None)
        .unwrap();
    assert_eq!(store.folded_journal(&hash).unwrap().len(), 1);

    // One preview set.
    let thumb = env
        .lib
        .preview_artifact(&hash, ArtifactKind::Thumb)
        .unwrap();
    assert!(thumb.is_some_and(|t| t.file.exists()));

    // §3.1 deterministic best path: a no-change scan batch-stamps one
    // last_verified_at, so the tiebreak is the lexicographically smallest
    // (volume_id, rel_path).
    let report = env.scan(&root);
    assert_eq!(report.fast_path, n);
    let best = env.lib.best_path(&hash).unwrap().unwrap();
    assert_eq!(best.row.rel_path, "photos/d000/dup.jpg");
    assert!(best.online);
}

// ---------------------------------------------------------------------------
// 5. In-place overwrite (L1)
// ---------------------------------------------------------------------------

#[test]
fn l13_05_in_place_overwrite() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(50));
    env.scan(&root);
    env.drain_queue();
    let old_hash = env.only_hash();
    let (store, session) = env.store();
    store
        .append(&session, d_remark("years of reflection", &old_hash), None)
        .unwrap();

    // Re-export over the same path while running.
    let base = UtcMillis::now().epoch_ms();
    let mut pl = env
        .lib
        .watch_pipeline(&root, DebounceConfig::default())
        .unwrap();
    let abs = env.write("photos/a.jpg", &unique_jpeg(51));
    pl.push(RawWatchEvent::Modified(abs), at(base, 0)).unwrap();
    pl.tick(at(base, 600)).unwrap(); // first stability check
    let effects = pl.tick(at(base, 2700)).unwrap(); // stable → §1.3
    let new_hash = match &effects[..] {
        [
            PipelineEffect::Observed {
                outcome: Observed::Superseded { old, new },
                ..
            },
        ] => {
            assert_eq!(old, &old_hash);
            new.clone()
        }
        other => panic!("expected supersede, got {other:?}"),
    };

    // New image + passes; old journal untouched on the old hash.
    assert_eq!(env.lib.image_count().unwrap(), 2);
    let counters = env.lib.pass_counters().unwrap();
    assert_eq!(counters.get(&("exif".to_string(), 1)).unwrap().pending, 1);
    assert_eq!(store.folded_journal(&old_hash).unwrap().len(), 1);
    assert!(store.folded_journal(&new_hash).unwrap().is_empty());

    // Old path row stale/superseded; dormant-prior-version lookup finds the
    // old hash; old image availability is missing.
    let conn = env.conn();
    let (state, reason): (String, String) = conn
        .query_row(
            "SELECT state, stale_reason FROM paths
             WHERE rel_path = 'photos/a.jpg' AND image_hash = ?1",
            [old_hash.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), reason.as_str()), ("stale", "superseded"));
    let volume_id = env.volume_id();
    let dormant = env
        .lib
        .dormant_prior_versions(&volume_id, "photos/a.jpg")
        .unwrap();
    assert_eq!(dormant, vec![old_hash.clone()]);
    assert_eq!(
        env.lib.availability(&old_hash).unwrap(),
        photoproof_core::library::Availability::Missing
    );
    assert_eq!(
        env.lib.availability(&new_hash).unwrap(),
        photoproof_core::library::Availability::Available
    );
}

// ---------------------------------------------------------------------------
// 6. Volume remount at a new mount point
// ---------------------------------------------------------------------------

#[test]
fn l13_06_volume_remount_new_mount_point() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    for i in 0..20 {
        env.write(&format!("photos/img{i:02}.jpg"), &unique_jpeg(600 + i));
    }
    env.scan(&root);
    env.drain_queue();
    let volume_id = env.volume_id();
    assert!(
        env.mount.join(MARKER_FILENAME).exists(),
        "marker written on first ingest of a writable volume"
    );

    // Unplug.
    env.probe.set_mounts(vec![]);
    env.lib.probe_volumes().unwrap();
    assert!(!env.lib.volume(&volume_id).unwrap().unwrap().online);

    // Remount at a different mount point (marker + platform id unchanged).
    let new_mount = env._tmp.path().join("mount-B");
    std::fs::rename(&env.mount, &new_mount).unwrap();
    env.probe.set_mounts(vec![probed(&new_mount)]);
    let before = hash_invocation_count();
    let to_scan = env.lib.probe_volumes().unwrap();
    assert_eq!(
        to_scan,
        vec![root.clone()],
        "online transition schedules a scan"
    );

    let vol = env.lib.volume(&volume_id).unwrap().unwrap();
    assert!(vol.online);
    assert_eq!(
        vol.mount_point.as_deref(),
        new_mount.to_str(),
        "mount_point updated"
    );
    assert_eq!(env.lib.volumes().unwrap().len(), 1, "same volume identity");

    // All paths resolve; the scheduled reconciliation is a pure fast path:
    // no relink storm, no re-hash.
    let report = env.scan(&root);
    assert_eq!(report.fast_path, 20);
    assert_eq!(report.relinked + report.new_images + report.went_stale, 0);
    assert_eq!(hash_invocation_count(), before, "zero hashing on remount");
    let hash = env.lib.image_hashes().unwrap()[0].clone();
    let best = env.lib.best_path(&hash).unwrap().unwrap();
    let abs = new_mount.join(&best.row.rel_path);
    assert!(abs.exists(), "best path resolves under the new mount point");
}

// ---------------------------------------------------------------------------
// 7. Marker precedence (L2)
// ---------------------------------------------------------------------------

#[test]
fn l13_07_marker_precedence() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(70));
    env.scan(&root);
    let volume_id = env.volume_id();
    env.lib.take_debug_log();

    // New enclosure: platform id changes, marker intact.
    let mut p = probed(&env.mount);
    p.platform_id = Some("uuid-after-enclosure-swap".into());
    env.probe.set_mounts(vec![p]);
    env.lib.probe_volumes().unwrap();

    let vols = env.lib.volumes().unwrap();
    assert_eq!(vols.len(), 1, "same volume identity — no second volume row");
    assert_eq!(vols[0].volume_id, volume_id);
    assert_eq!(
        vols[0].platform_id.as_deref(),
        Some("uuid-after-enclosure-swap"),
        "platform id adopted under the marker's identity"
    );
    let log = env.lib.take_debug_log().join("\n");
    assert!(
        log.contains("marker precedence"),
        "warning logged; got: {log}"
    );
}

// ---------------------------------------------------------------------------
// 8. Offline browsing (automated state check)
// ---------------------------------------------------------------------------

#[test]
fn l13_08_offline_browsing() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(80));
    env.scan(&root);
    env.drain_queue();
    let hash = env.only_hash();
    let volume_id = env.volume_id();

    // Unplug the volume.
    env.probe.set_mounts(vec![]);
    env.lib.probe_volumes().unwrap();
    assert_eq!(
        env.lib.availability(&hash).unwrap(),
        photoproof_core::library::Availability::Offline
    );
    // Previews render from cache.
    let thumb = env
        .lib
        .preview_artifact(&hash, ArtifactKind::Thumb)
        .unwrap()
        .unwrap();
    assert!(thumb.file.exists());
    image::open(&thumb.file).unwrap();
    // Badge payload: best offline path + volume label.
    let best = env.lib.best_path(&hash).unwrap().unwrap();
    assert!(!best.online);
    let vol = env.lib.volume(&volume_id).unwrap().unwrap();
    assert_eq!(vol.label.as_deref(), Some("TestVol"));
    // Typed notes attach against the offline image (events target hashes).
    let (store, session) = env.store();
    store
        .append(&session, d_remark("offline note", &hash), None)
        .unwrap();
    // Pending passes fail transient and wait out the remount.
    env.write("photos/b.jpg", &unique_jpeg(81)); // exists but volume "offline"
    assert_eq!(store.folded_journal(&hash).unwrap().len(), 1);

    // Remount: identity match → online → reconciliation; no duplicate
    // events (the journal is keyed by hash; merge = union by event id).
    env.probe.set_mounts(vec![probed(&env.mount)]);
    let to_scan = env.lib.probe_volumes().unwrap();
    assert_eq!(to_scan, vec![root.clone()]);
    env.scan(&root);
    env.drain_queue();
    assert_eq!(
        env.lib.availability(&hash).unwrap(),
        photoproof_core::library::Availability::Available
    );
    assert_eq!(store.folded_journal(&hash).unwrap().len(), 1);
    assert_eq!(
        env.lib.image_count().unwrap(),
        2,
        "b.jpg ingested on remount"
    );
}

// ---------------------------------------------------------------------------
// 9. Root removal
// ---------------------------------------------------------------------------

#[test]
fn l13_09_root_removal() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    for i in 0..3 {
        env.write(&format!("photos/img{i}.jpg"), &unique_jpeg(900 + i));
    }
    env.scan(&root);
    env.drain_queue();
    let hash = env.lib.image_hashes().unwrap()[0].clone();
    let (store, session) = env.store();
    store
        .append(&session, d_remark("kept forever", &hash), None)
        .unwrap();
    let thumb_file = env
        .lib
        .preview_artifact(&hash, ArtifactKind::Thumb)
        .unwrap()
        .unwrap()
        .file;

    env.lib.remove_root(&root).unwrap();
    // Events untouched, previews retained, paths stale/root-removed,
    // availability missing.
    assert_eq!(store.folded_journal(&hash).unwrap().len(), 1);
    assert!(thumb_file.exists());
    let conn = env.conn();
    let (stale, reason_ok): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*),
                    COUNT(CASE WHEN stale_reason = 'root-removed' THEN 1 END)
             FROM paths WHERE state = 'stale'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((stale, reason_ok), (3, 3));
    assert_eq!(
        env.lib.availability(&hash).unwrap(),
        photoproof_core::library::Availability::Missing
    );

    // Re-register an overlapping root: full recovery via the fast path,
    // zero re-hashing of unchanged files.
    let before = hash_invocation_count();
    let root2 = env.register("photos");
    let report = env.scan(&root2);
    assert_eq!(report.relinked, 3, "stale rows reactivated");
    assert_eq!(report.new_images, 0);
    assert_eq!(hash_invocation_count(), before, "zero re-hashing");
    assert_eq!(
        env.lib.availability(&hash).unwrap(),
        photoproof_core::library::Availability::Available
    );
    assert_eq!(store.folded_journal(&hash).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// 10. Orientation correctness (P9)
// ---------------------------------------------------------------------------

#[test]
fn l13_10_orientation_correctness() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    let display = display_pattern(200, 300); // portrait, red top-left

    // JPEG fixtures for EXIF orientations 3, 6, 8 (and 1 as control), plus a
    // TIFF orientation-6 fixture (tag lives in IFD0; the §9.6 fallback path).
    for &o in &[1u16, 3, 6, 8] {
        let stored = store_for_orientation(&display, o);
        env.write(
            &format!("photos/o{o}.jpg"),
            &jpeg_with_orientation(&stored, o),
        );
    }
    env.write(
        "photos/o6.tif",
        &tiff_with_orientation(&store_for_orientation(&display, 6), 6),
    );
    env.scan(&root);
    env.drain_queue();
    assert_eq!(env.lib.image_count().unwrap(), 5);

    let conn = env.conn();
    let mut artifacts: Vec<(String, PathBuf, (u32, u32))> = Vec::new();
    for rel in [
        "photos/o1.jpg",
        "photos/o3.jpg",
        "photos/o6.jpg",
        "photos/o8.jpg",
        "photos/o6.tif",
    ] {
        let h: String = conn
            .query_row(
                "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                [rel],
                |r| r.get(0),
            )
            .unwrap();
        let hash = ContentHash::from_hex(&h).unwrap();
        for kind in [ArtifactKind::Thumb, ArtifactKind::Display] {
            let a = env
                .lib
                .preview_artifact(&hash, kind)
                .unwrap()
                .unwrap_or_else(|| panic!("{rel}: missing {kind:?}"));
            let dims = assert_upright(&a.file, true, &format!("{rel} {kind:?}"));
            assert_eq!(
                (a.width as u32, a.height as u32),
                dims,
                "{rel}: recorded dims are display-oriented"
            );
            artifacts.push((rel.to_string(), a.file, dims));
        }
        // Stored orientation recorded on images (the stroke contract input).
        let img = env.lib.image(&hash).unwrap().unwrap();
        let expect_o: u16 = match rel {
            "photos/o1.jpg" => 1,
            "photos/o3.jpg" => 3,
            "photos/o8.jpg" => 8,
            _ => 6,
        };
        assert_eq!(img.exif_orientation, expect_o, "{rel}");
    }

    // Stroke agreement: a mark at the red square (normalized display-
    // oriented coords ~ (0.1, 0.066)) must land on red after preview
    // regeneration and after a rebuild into a fresh index.
    let probe_red = |file: &Path| {
        let img = image::open(file).unwrap().into_rgba8();
        let (w, h) = (img.width(), img.height());
        let (x, y) = ((w as f64 * 0.1) as u32, (h as f64 * 0.066) as u32);
        assert!(
            is_red(*img.get_pixel(x, y)),
            "stroke anchor not red at {x},{y} in {}",
            file.display()
        );
        (w, h)
    };
    let hash6: String = conn
        .query_row(
            "SELECT image_hash FROM paths WHERE rel_path = 'photos/o6.jpg' AND state = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let hash6 = ContentHash::from_hex(&hash6).unwrap();
    let thumb6 = env
        .lib
        .preview_artifact(&hash6, ArtifactKind::Thumb)
        .unwrap()
        .unwrap()
        .file;
    let dims_before = probe_red(&thumb6);

    // Regeneration (clear → re-run preview pass).
    env.lib.clear_preview_cache(&hash6).unwrap();
    env.drain_queue();
    let dims_after = probe_red(&thumb6);
    assert_eq!(dims_before, dims_after, "regeneration preserves geometry");

    // Rebuild: a fresh index over the same files yields the same
    // display-oriented space.
    let env2 = Env::new();
    let root2 = env2.register("photos");
    env2.write(
        "photos/o6.jpg",
        &jpeg_with_orientation(&store_for_orientation(&display, 6), 6),
    );
    env2.scan(&root2);
    env2.drain_queue();
    let h2 = env2.only_hash();
    assert_eq!(h2, hash6, "same bytes, same identity after rebuild");
    let t2 = env2
        .lib
        .preview_artifact(&h2, ArtifactKind::Thumb)
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(probe_red(&t2), dims_before, "rebuild preserves geometry");
}

/// §9.3.1 per-format embedded-preview fixtures: a pre-rotated convention and
/// a tag-reliant convention, both portrait — each must come out upright with
/// no double rotation. (Real Nikon/Fuji RAW files: founder-machine checklist;
/// the conventions are exercised here through the injectable extractor.)
#[test]
fn l13_10_embedded_preview_orientation_fixtures() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    let display = display_pattern(2400, 3600); // portrait, ≥ threshold

    // "Nikon-style": preview already display-oriented; tag still says 6.
    env.extractor.script(
        "nikon-pre-rotated.nef",
        FakeSpec {
            preview: display.clone(),
            raw_dims: Some((6000, 4000)), // sensor landscape
            exif_orientation: 6,
            preview_orientation: None,
        },
    );
    // "Fuji-style": preview sensor-oriented; the tag must be applied.
    env.extractor.script(
        "fuji-tag-reliant.raf",
        FakeSpec {
            preview: store_for_orientation(&display, 6), // landscape as stored
            raw_dims: Some((6000, 4000)),
            exif_orientation: 6,
            preview_orientation: None,
        },
    );
    env.write("photos/nikon-pre-rotated.nef", b"synthetic nef body 1");
    env.write("photos/fuji-tag-reliant.raf", b"synthetic raf body 2");
    env.scan(&root);
    env.drain_queue();

    let conn = env.conn();
    let mut dims = Vec::new();
    for rel in [
        "photos/nikon-pre-rotated.nef",
        "photos/fuji-tag-reliant.raf",
    ] {
        let h: String = conn
            .query_row(
                "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                [rel],
                |r| r.get(0),
            )
            .unwrap();
        let hash = ContentHash::from_hex(&h).unwrap();
        for kind in [ArtifactKind::Thumb, ArtifactKind::Display] {
            let a = env.lib.preview_artifact(&hash, kind).unwrap().unwrap();
            assert_eq!(a.source, PreviewSource::Embedded, "{rel}");
            assert!(!a.needs_full_decode, "{rel}: ≥ 2048 px meets the threshold");
            // Upright = portrait with the marker top-left; a double rotation
            // would yield landscape or a displaced marker.
            dims.push(assert_upright(&a.file, true, &format!("{rel} {kind:?}")));
        }
    }
    // Pixel comparison across conventions: both render to identical
    // display-oriented geometry.
    assert_eq!(
        dims[0], dims[2],
        "thumb geometry identical across conventions"
    );
    assert_eq!(
        dims[1], dims[3],
        "display geometry identical across conventions"
    );
    // Threshold met → the backfill skips these (stroke-substrate safety).
    let counters = env.lib.pass_counters().unwrap();
    let frd = counters
        .get(&("full-raw-decode".to_string(), 1))
        .copied()
        .unwrap_or_default();
    assert_eq!(frd.skipped, 2);
    assert_eq!(frd.pending, 0);
}

// ---------------------------------------------------------------------------
// 11. Threshold routing (§9.3)
// ---------------------------------------------------------------------------

#[test]
fn l13_11_threshold_routing() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    // Older-Sony-ARW class: 1616×1080 embedded preview, below the 2048 px
    // acceptability threshold.
    let small = display_pattern(1616, 1080);
    env.extractor.script(
        "small-preview.arw",
        FakeSpec {
            preview: small.clone(),
            raw_dims: Some((6048, 4024)),
            exif_orientation: 1,
            preview_orientation: None,
        },
    );
    env.extractor.script(
        "small-preview-with-strokes.arw",
        FakeSpec {
            preview: small,
            raw_dims: Some((6048, 4024)),
            exif_orientation: 1,
            preview_orientation: None,
        },
    );
    // And one with no embedded preview at all.
    env.write("photos/small-preview.arw", b"synthetic arw body 1");
    env.write(
        "photos/small-preview-with-strokes.arw",
        b"synthetic arw body 2",
    );
    env.write("photos/no-preview.arw", b"synthetic arw body 3");
    env.scan(&root);

    // A stroke on the second image before the preview pass runs → the
    // §10.3 promotion rule lifts its backfill above plain P2.
    let conn = env.conn();
    let hash_of = |rel: &str| -> ContentHash {
        let h: String = conn
            .query_row(
                "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                [rel],
                |r| r.get(0),
            )
            .unwrap();
        ContentHash::from_hex(&h).unwrap()
    };
    let stroked = hash_of("photos/small-preview-with-strokes.arw");
    let plain = hash_of("photos/small-preview.arw");
    let none = hash_of("photos/no-preview.arw");
    let (store, session) = env.store();
    store.append(&session, d_stroke(&stroked), None).unwrap();

    env.drain_queue();

    // Small artifacts now (a small preview beats a placeholder), flagged.
    let a = env
        .lib
        .preview_artifact(&plain, ArtifactKind::Display)
        .unwrap()
        .unwrap();
    assert_eq!(a.source, PreviewSource::Embedded);
    assert_eq!(
        (a.width, a.height),
        (1616, 1080),
        "below-threshold preview is never upscaled"
    );
    assert!(a.needs_full_decode, "threshold miss flagged");

    // Queued full-raw-decode: plain at P2, stroked promoted to P1, the
    // no-preview case pending at elevated backfill priority with placeholder
    // artifacts absent.
    let prio = |hash: &ContentHash| -> i64 {
        conn.query_row(
            "SELECT priority FROM ingest_passes
             WHERE image_hash = ?1 AND pass_name = 'full-raw-decode' AND state = 'pending'",
            [hash.as_str()],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(prio(&plain), 2, "plain threshold miss queues at P2");
    assert_eq!(
        prio(&stroked),
        1,
        "stroke promotion lifts the backfill to P1"
    );
    assert_eq!(prio(&none), 1, "no embedded preview → elevated backfill");
    assert!(
        env.lib
            .preview_artifact(&none, ArtifactKind::Thumb)
            .unwrap()
            .is_none(),
        "no-preview RAW shows the UI placeholder until backfill"
    );
    // "After backfill, full-quality, flag clear" — the `full-raw-decode`
    // worker is the M1.5 packet; deferred there (the queue knows the pass).
}

// ---------------------------------------------------------------------------
// 12. Reconciliation budget
// ---------------------------------------------------------------------------

fn reconciliation_budget(n: usize) {
    let env = Env::new();
    let root = env.register("photos");
    for i in 0..n {
        // Scan-only fixture: unique bytes, no queue processing needed.
        env.write(
            &format!("photos/d{:03}/f{i:06}.jpg", i % 500),
            format!("synthetic-bytes-{i:06}-padding-padding-padding").as_bytes(),
        );
    }
    let r = env.scan(&root);
    assert_eq!(r.new_images, n);

    // The no-change scan: ≤ 10 s, zero hash invocations.
    let before = hash_invocation_count();
    let t0 = std::time::Instant::now();
    let r2 = env.scan(&root);
    let elapsed = t0.elapsed();
    assert_eq!(r2.fast_path, n);
    assert_eq!(
        hash_invocation_count(),
        before,
        "a no-change scan does zero hashing"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(10),
        "no-change scan of {n} paths took {elapsed:?} (> 10 s budget)"
    );
}

#[test]
fn l13_12_reconciliation_budget() {
    let _g = guard();
    // Debug-gate default; the spec's 50k fixture runs via the ignored
    // variant below (P4.1 perf pass / founder machine).
    reconciliation_budget(scaled("PHOTOPROOF_TEST_SCAN_PATHS", 5_000));
}

#[test]
#[ignore = "50k-path fixture: run explicitly (cargo test -- --ignored) on perf-pass hardware"]
fn l13_12_reconciliation_budget_50k_full() {
    let _g = guard();
    reconciliation_budget(50_000);
}

// ---------------------------------------------------------------------------
// 13. Exclusions
// ---------------------------------------------------------------------------

#[test]
fn l13_13_exclusions() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    // The §5.1 list: hidden entries, sidecars, tool/cache dirs, sync caches
    // and partials, off-allowlist formats, the size ceiling.
    env.write("photos/ok.jpg", &unique_jpeg(1300));
    env.write("photos/.hidden-dir/a.jpg", &unique_jpeg(1301));
    env.write("photos/.DS_Store", b"junk");
    env.write("photos/.photoproof-volume", b"{}");
    env.write("photos/ok.jpg.photoproof.json", b"{}");
    env.write("photos/@eaDir/b.jpg", &unique_jpeg(1302));
    env.write("photos/__MACOSX/c.jpg", &unique_jpeg(1303));
    env.write("photos/Catalog.lrdata/d.jpg", &unique_jpeg(1304));
    env.write("photos/session.cocatalogdb/e.jpg", &unique_jpeg(1305));
    env.write("photos/Lightroom Catalog 2/f.jpg", &unique_jpeg(1306));
    env.write("photos/$RECYCLE.BIN/g.jpg", &unique_jpeg(1307));
    env.write("photos/System Volume Information/h.jpg", &unique_jpeg(1308));
    env.write("photos/lost+found/i.jpg", &unique_jpeg(1309));
    env.write("photos/node_modules/j.jpg", &unique_jpeg(1310));
    env.write("photos/.thumbnails/k.jpg", &unique_jpeg(1311));
    env.write("photos/.dropbox.cache/l.jpg", &unique_jpeg(1312));
    env.write("photos/m.jpg.tmp.drivedownload", &unique_jpeg(1313));
    env.write("photos/n.jpg.tmp.driveupload", &unique_jpeg(1314));
    env.write("photos/~$o.jpg", &unique_jpeg(1315));
    env.write("photos/.~p.jpg", &unique_jpeg(1316));
    env.write("photos/q.gif", b"GIF89a junk");
    env.write("photos/r.psd", b"8BPS junk");
    // > 2 GiB sanity ceiling (sparse).
    {
        let f = std::fs::File::create(env.mount.join("photos/huge.jpg")).unwrap();
        f.set_len(2 * 1024 * 1024 * 1024 + 1).unwrap();
    }

    let before = hash_invocation_count();
    let report = env.scan(&root);
    env.drain_queue();

    assert_eq!(env.lib.image_count().unwrap(), 1, "only ok.jpg ingests");
    assert_eq!(
        env.only_hash(),
        ContentHash::from_bytes_of(&unique_jpeg(1300))
    );
    assert_eq!(report.new_images, 1);
    assert_eq!(report.skipped_oversize, 1);
    assert_eq!(
        hash_invocation_count(),
        before + 1,
        "excluded entries are never even hashed"
    );
    // The watcher applies the same rules (unit-level check through the
    // pipeline: an excluded path is ignored).
    let base = UtcMillis::now().epoch_ms();
    let mut pl = env
        .lib
        .watch_pipeline(&root, DebounceConfig::default())
        .unwrap();
    let sidecar = env.mount.join("photos/ok.jpg.photoproof.json");
    pl.push(RawWatchEvent::Modified(sidecar), at(base, 0))
        .unwrap();
    let effects = pl.tick(at(base, 600)).unwrap();
    assert_eq!(
        effects,
        vec![PipelineEffect::Ignored {
            rel_path: "photos/ok.jpg.photoproof.json".into()
        }]
    );
    assert_eq!(env.lib.image_count().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// 14. Clock shift (P10)
// ---------------------------------------------------------------------------

fn clock_shift(n: usize) {
    let env = Env::new();
    let root = env.register("photos");
    let mut rels = Vec::with_capacity(n);
    for i in 0..n {
        let rel = format!("photos/d{:02}/f{i:05}.jpg", i % 13);
        env.write(&rel, format!("clock-shift-fixture-{i:05}-bytes").as_bytes());
        rels.push(rel);
    }
    let r = env.scan(&root);
    assert_eq!(r.new_images, n);

    // DST: every mtime shifts by exactly +1 h, sizes unchanged...
    for rel in &rels {
        shift_mtime(&env.mount.join(rel), std::time::Duration::from_secs(3600));
    }
    // ...except one file whose bytes genuinely changed in the same scan
    // (a real edit: new content, natural new mtime).
    let edited = &rels[n / 2];
    env.write(edited, b"genuinely-different-bytes-now");

    let before = hash_invocation_count();
    let report = env.scan(&root);
    let hashed = hash_invocation_count() - before;

    let shift = report.clock_shift.expect("uniform shift detected");
    assert!(!shift.aborted);
    assert_eq!(shift.delta_hours, 1);
    assert_eq!(shift.rows_updated, n - 1, "stored mtimes updated in place");
    assert_eq!(shift.samples_hashed, 16, "exactly 16 sampled files");
    assert_eq!(
        hashed, 17,
        "16 confirmation samples + 1 genuinely changed file; no re-hash storm"
    );
    assert_eq!(report.superseded, 1, "the changed file is still caught");
    assert_eq!(env.lib.image_count().unwrap(), (n + 1) as i64);

    // The shift was absorbed: the next scan is a pure fast path.
    let before = hash_invocation_count();
    let r3 = env.scan(&root);
    assert_eq!(r3.fast_path, n);
    assert_eq!(hash_invocation_count(), before);
}

#[test]
fn l13_14_clock_shift() {
    let _g = guard();
    // Debug-gate default; the spec's 10k fixture runs via the ignored
    // variant below.
    clock_shift(scaled("PHOTOPROOF_TEST_CLOCK_SHIFT_PATHS", 2_500));
}

#[test]
#[ignore = "10k-path fixture: run explicitly (cargo test -- --ignored) on perf-pass hardware"]
fn l13_14_clock_shift_10k_full() {
    let _g = guard();
    clock_shift(10_000);
}

/// A mismatching confirmation sample aborts the shortcut (§7.3): every
/// mismatch falls back to ordinary handling — correctness over the shortcut.
#[test]
fn l13_14_clock_shift_sample_mismatch_aborts() {
    let _g = guard();
    let n = 40;
    let env = Env::new();
    let root = env.register("photos");
    let mut rels = Vec::new();
    for i in 0..n {
        let rel = format!("photos/f{i:03}.jpg");
        env.write(&rel, format!("abort-fixture-{i:03}-bytes!!").as_bytes());
        rels.push(rel);
    }
    env.scan(&root);
    // Shift every mtime +1 h, but silently corrupt every file's bytes at the
    // same size (the case the sample exists to catch).
    for (i, rel) in rels.iter().enumerate() {
        let p = env.mount.join(rel);
        let m = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::fs::write(&p, format!("ABORT-fixture-{i:03}-bytes!!").as_bytes()).unwrap();
        set_mtime(&p, m + std::time::Duration::from_secs(3600));
    }
    let report = env.scan(&root);
    let shift = report.clock_shift.expect("candidate detected");
    assert!(shift.aborted, "sample mismatch aborts the shortcut");
    assert_eq!(shift.rows_updated, 0);
    assert_eq!(
        report.superseded, n,
        "fallback re-hashes and supersedes every changed file"
    );
}

// ---------------------------------------------------------------------------
// 15. Placeholder files (P11)
// ---------------------------------------------------------------------------

#[test]
fn l13_15_placeholder_files() {
    let _g = guard();
    let env = Env::new();
    let root = env.register("photos");
    let bytes = unique_jpeg(1500);
    let abs = env.write("photos/cloud.jpg", &bytes);
    env.placeholders.mark(&abs);

    let before = hash_invocation_count();
    let report = env.scan(&root);
    env.drain_queue();

    // Never read, never hashed, no images row; a deferred/skipped state
    // visible in the pass counters.
    assert_eq!(
        hash_invocation_count(),
        before,
        "placeholder bytes never read"
    );
    assert_eq!(env.lib.image_count().unwrap(), 0);
    assert_eq!(report.placeholders, 1);
    let counters = env.lib.pass_counters().unwrap();
    let hash_pass = counters
        .get(&("hash".to_string(), 1))
        .copied()
        .unwrap_or_default();
    assert_eq!(
        hash_pass.skipped, 1,
        "skipped hash row with placeholder code"
    );
    let conn = env.conn();
    let err: String = conn
        .query_row(
            "SELECT error FROM ingest_passes WHERE pass_name = 'hash' AND state = 'skipped'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(err.starts_with("placeholder:"), "got {err}");

    // Re-checked at each reconciliation while still dataless.
    let report2 = env.scan(&root);
    assert_eq!(report2.placeholders, 1);
    assert_eq!(env.lib.image_count().unwrap(), 0);

    // Hydration: the stat-level attribute clears; the next reconciliation
    // ingests normally and clears the sentinel.
    env.placeholders.hydrate(&abs);
    let report3 = env.scan(&root);
    env.drain_queue();
    assert_eq!(report3.new_images, 1);
    assert_eq!(env.lib.image_count().unwrap(), 1);
    assert_eq!(env.only_hash(), ContentHash::from_bytes_of(&bytes));
    let counters = env.lib.pass_counters().unwrap();
    let hash_pass = counters
        .get(&("hash".to_string(), 1))
        .copied()
        .unwrap_or_default();
    assert_eq!(hash_pass.skipped, 0, "sentinel cleared after hydration");
    assert_eq!(hash_pass.done, 1);
}

// ---------------------------------------------------------------------------
// §9.8 regeneration: generator_version bump re-enqueues preview passes
// ---------------------------------------------------------------------------

#[test]
fn s9_8_generator_version_bump_reenqueues_previews() {
    let _g = guard();
    let mut env = Env::new();
    let root = env.register("photos");
    env.write("photos/a.jpg", &unique_jpeg(9800));
    env.scan(&root);
    env.drain_queue();
    let hash = env.only_hash();
    let v0 = env
        .lib
        .preview_artifact(&hash, ArtifactKind::Thumb)
        .unwrap()
        .unwrap()
        .generator_version;
    assert_eq!(
        v0,
        photoproof_core::library::GENERATOR_VERSION,
        "fresh artifacts carry the current generator version"
    );

    // Simulate artifacts from an older build (orientation/ICC changes MUST
    // bump the constant; this models the next bump).
    env.conn()
        .execute("UPDATE preview_artifacts SET generator_version = 0", [])
        .unwrap();
    env.reopen();
    let counters = env.lib.pass_counters().unwrap();
    assert_eq!(
        counters
            .get(&("preview".to_string(), 1))
            .copied()
            .unwrap_or_default()
            .pending,
        1,
        "stale-generator artifacts re-enqueue the preview pass at startup"
    );
    env.drain_queue();
    let v1 = env
        .lib
        .preview_artifact(&hash, ArtifactKind::Thumb)
        .unwrap()
        .unwrap()
        .generator_version;
    assert_eq!(v1, photoproof_core::library::GENERATOR_VERSION);
}
