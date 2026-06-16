//! spec/LIBRARY.md §7.1 watcher behavior: debounce coalescing, mid-copy
//! stability, paired renames, overflow degradation — driven through the
//! deterministic `WatchPipeline` with injected clocks (no sleeps, no flaky
//! timing), plus one minimal real-`notify` test with generous bounds.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use photoproof_core::library::{
    DebounceConfig, FakeVolumeProbe, Library, LibraryOptions, Observed, PipelineEffect,
    PlatformIdKind, ProbedVolume, QueueOptions, RawWatchEvent, ScanOptions,
    SharedSetPlaceholderDetector, hash_invocation_count,
};
use photoproof_core::{ContentHash, UtcMillis};

/// The hashing instrumentation is process-global; tests that read deltas
/// serialize on this.
fn guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    mount: PathBuf,
    lib: Arc<Library>,
    root: String,
}

fn probed(mount: &Path) -> ProbedVolume {
    ProbedVolume {
        mount_point: mount.to_path_buf(),
        platform_id: Some("uuid-watch-0001".into()),
        platform_kind: PlatformIdKind::LinuxFsUuid,
        label: None,
        fs_type: Some("ext4".into()),
        capacity_bytes: Some(1 << 30),
        read_only_flag: false,
        is_system_root: false,
        coarse_mtime: false,
    }
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("mount");
        std::fs::create_dir_all(mount.join("photos")).unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let lib = Arc::new(
            Library::open_with(
                tmp.path().join("photoproof.db"),
                tmp.path().join("cache"),
                LibraryOptions {
                    probe: Arc::new(probe),
                    placeholders: Arc::new(SharedSetPlaceholderDetector::new()),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let root = lib.register_root(&mount.join("photos"), None).unwrap();
        Env {
            _tmp: tmp,
            mount,
            lib,
            root,
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.mount.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }
}

fn at(base: i64, off: i64) -> UtcMillis {
    UtcMillis::from_epoch_ms(base + off)
}

/// Unique decodable JPEG bytes.
fn jpeg(seed: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(8, 8);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = seed.wrapping_mul(97).wrapping_add(x * 13 + y);
        *p = image::Rgb([
            (v & 0xff) as u8,
            ((v >> 3) & 0xff) as u8,
            ((v >> 5) & 0xff) as u8,
        ]);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut std::io::Cursor::new(&mut out))
        .encode_image(&image::DynamicImage::ImageRgb8(img))
        .unwrap();
    out
}

#[test]
fn w01_debounce_burst_coalesces_to_one_evaluation() {
    let _g = guard();
    let env = Env::new();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();
    let abs = env.write("photos/burst.jpg", &jpeg(1));
    let before = hash_invocation_count();

    // A write burst: many events inside one quiescence window.
    for i in 0..10 {
        pl.push(RawWatchEvent::Modified(abs.clone()), at(base, i * 40))
            .unwrap();
    }
    // Quiescence not yet reached relative to the last event.
    assert!(pl.tick(at(base, 800)).unwrap().is_empty());
    // Due: first stability check (records the stat, processes nothing).
    assert!(pl.tick(at(base, 900)).unwrap().is_empty());
    // Stable across two checks → exactly one evaluation, one hash.
    let effects = pl.tick(at(base, 3000)).unwrap();
    match &effects[..] {
        [
            PipelineEffect::Observed {
                rel_path,
                outcome: Observed::NewImage(_),
            },
        ] => assert_eq!(rel_path, "photos/burst.jpg"),
        other => panic!("expected one NewImage, got {other:?}"),
    }
    assert_eq!(
        hash_invocation_count(),
        before + 1,
        "one hash for the burst"
    );
    assert_eq!(pl.pending_paths(), 0);
}

#[test]
fn w02_mid_copy_file_waits_for_stability() {
    let _g = guard();
    let env = Env::new();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();

    // A card-reader copy in progress: the file grows between checks.
    let full = jpeg(2);
    let abs = env.mount.join("photos/copying.jpg");
    std::fs::write(&abs, &full[..full.len() / 2]).unwrap();
    pl.push(RawWatchEvent::Created(abs.clone()), at(base, 0))
        .unwrap();
    assert!(
        pl.tick(at(base, 600)).unwrap().is_empty(),
        "first check records"
    );
    // Still growing at the 2 s re-check.
    std::fs::write(&abs, &full).unwrap();
    assert!(
        pl.tick(at(base, 2700)).unwrap().is_empty(),
        "size changed between checks → keep waiting"
    );
    // Stable across the next two checks.
    let effects = pl.tick(at(base, 4800)).unwrap();
    match &effects[..] {
        [
            PipelineEffect::Observed {
                outcome: Observed::NewImage(h),
                ..
            },
        ] => assert_eq!(h, &ContentHash::from_bytes_of(&full), "final bytes hashed"),
        other => panic!("expected NewImage of the complete file, got {other:?}"),
    }
    // The stored path row carries the final size.
    let hash = ContentHash::from_bytes_of(&full);
    let rows = env.lib.paths_for_image(&hash).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].size, full.len() as i64);
}

#[test]
fn w03_rename_into_excluded_name_is_a_remove() {
    let _g = guard();
    let env = Env::new();
    let abs = env.write("photos/temp.jpg", &jpeg(3));
    env.lib
        .scan_root(&env.root, &ScanOptions::default())
        .unwrap();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();
    // Capture the hash while the image still has an active path: after the
    // rename-to-excluded its only path goes stale, and `image_hashes()` now
    // filters to images with at least one ACTIVE path (removed-folder
    // reconciliation), so it would no longer enumerate this orphaned image.
    let hash = env.lib.image_hashes().unwrap()[0].clone();

    let hidden = env.mount.join("photos/.temp.jpg.partial");
    std::fs::rename(&abs, &hidden).unwrap();
    pl.push(
        RawWatchEvent::Renamed {
            from: abs,
            to: hidden,
        },
        at(base, 0),
    )
    .unwrap();
    let effects = pl.tick(at(base, 600)).unwrap();
    assert_eq!(
        effects,
        vec![PipelineEffect::Removed {
            rel_path: "photos/temp.jpg".into()
        }]
    );
    // The image is now orphaned (path stale): availability is Missing and it
    // has dropped out of `image_hashes()`.
    assert!(
        env.lib.image_hashes().unwrap().is_empty(),
        "the orphaned image is no longer enumerated"
    );
    assert_eq!(
        env.lib.availability(&hash).unwrap(),
        photoproof_core::library::Availability::Missing
    );
}

#[test]
fn w04_overflow_triggers_scan_and_polled_mode() {
    let _g = guard();
    let env = Env::new();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();
    assert!(!pl.polled_mode());

    let effects = pl.push(RawWatchEvent::Overflow, at(base, 0)).unwrap();
    assert_eq!(effects, vec![PipelineEffect::ScanNeeded]);
    assert!(
        pl.polled_mode(),
        "overflow degrades the root to polled mode"
    );

    let effects = pl
        .push(RawWatchEvent::Error("backend died".into()), at(base, 10))
        .unwrap();
    assert_eq!(effects, vec![PipelineEffect::ScanNeeded]);
    let log = env.lib.take_debug_log().join("\n");
    assert!(log.contains("overflow"), "{log}");
    assert!(log.contains("backend died"), "{log}");

    // The recovery scan the caller runs picks up whatever was missed.
    env.write("photos/missed.jpg", &jpeg(4));
    let report = env
        .lib
        .scan_root(&env.root, &ScanOptions::default())
        .unwrap();
    assert_eq!(report.new_images, 1);
}

#[test]
fn w05_events_outside_the_root_are_ignored() {
    let _g = guard();
    let env = Env::new();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();
    let outside = env.mount.join("elsewhere/out.jpg");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, jpeg(5)).unwrap();
    pl.push(RawWatchEvent::Created(outside), at(base, 0))
        .unwrap();
    assert!(pl.tick(at(base, 5000)).unwrap().is_empty());
    assert_eq!(env.lib.image_count().unwrap(), 0);
}

#[test]
fn w06_directory_create_enqueues_children() {
    let _g = guard();
    let env = Env::new();
    let mut pl = env
        .lib
        .watch_pipeline(&env.root, DebounceConfig::default())
        .unwrap();
    let base = UtcMillis::now().epoch_ms();

    // A folder dragged into the root arrives as one directory event.
    env.write("photos/roll/a.jpg", &jpeg(6));
    env.write("photos/roll/b.jpg", &jpeg(7));
    pl.push(
        RawWatchEvent::Created(env.mount.join("photos/roll")),
        at(base, 0),
    )
    .unwrap();
    pl.tick(at(base, 600)).unwrap();
    let effects = pl.tick(at(base, 2700)).unwrap();
    assert_eq!(effects.len(), 2, "both children observed: {effects:?}");
    assert_eq!(env.lib.image_count().unwrap(), 2);
}

/// Real `notify` backend, minimal scope, generous bounds (the deterministic
/// state machine is covered above; this only proves the wiring end-to-end:
/// new stable file → image + queued passes).
#[test]
fn w07_real_watcher_ingests_a_new_file() {
    let env = Env::new();
    let handle = env.lib.start_watcher(&env.root).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300)); // backend settle
    env.write("photos/live.jpg", &jpeg(8));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if env.lib.image_count().unwrap() == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "watcher did not ingest the file within 30 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    handle.stop();
    env.lib.process_queue(&QueueOptions::default()).unwrap();
    let hash = env.lib.image_hashes().unwrap()[0].clone();
    let thumb = env
        .lib
        .preview_artifact(&hash, photoproof_core::library::ArtifactKind::Thumb)
        .unwrap();
    assert!(thumb.is_some_and(|t| t.file.exists()));
}
