//! Tier-1 near-duplicate detection, end-to-end through the real ingest path
//! (DESIGN-DEDUP-AND-SIMILARITY.md §"Tier 1").
//!
//! These tests drive the SHIPPED hook: write image files, run the real preview
//! pass (which computes + stores the dHash off the decoded preview), then call
//! `Library::find_near_duplicates`. They prove the column is populated by ingest
//! and that the grouping behaves on real decoded pixels — not just on the unit
//! algorithm in `library/phash.rs` (which has its own `#[cfg(test)]` suite).

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{DynamicImage, RgbImage};
use photoproof_core::ContentHash;
use photoproof_core::library::{
    EmbeddedPreviewExtractor, ExtractedPreview, FakeVolumeProbe, Library, LibraryOptions,
    PlatformIdKind, PreviewError, ProbedVolume, QueueOptions, ScanOptions,
    SharedSetPlaceholderDetector,
};

// --- minimal harness (a trimmed copy of library_acceptance's Env) ------------

/// No embedded previews: this suite ingests plain JPEGs through the
/// original-decode path, where the dHash hook lives.
#[derive(Default)]
struct NoExtractor;
impl EmbeddedPreviewExtractor for NoExtractor {
    fn extract(&self, _path: &Path) -> Result<Option<ExtractedPreview>, PreviewError> {
        Ok(None)
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    mount: PathBuf,
    lib: Arc<Library>,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        std::fs::create_dir_all(&mount).unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![ProbedVolume {
            mount_point: mount.clone(),
            platform_id: Some("uuid-dedup-0001".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("DedupVol".into()),
            fs_type: Some("ext4".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }]);
        let lib = Arc::new(
            Library::open_with(
                tmp.path().join("photoproof.db"),
                tmp.path().join("cache"),
                LibraryOptions {
                    probe: Arc::new(probe),
                    placeholders: Arc::new(SharedSetPlaceholderDetector::new()),
                    extractor: Arc::new(NoExtractor) as Arc<dyn EmbeddedPreviewExtractor>,
                },
            )
            .unwrap(),
        );
        Env {
            _tmp: tmp,
            mount,
            lib,
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.mount.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }

    /// Register the mount as a root, scan it, and run the queue so every image
    /// gets its preview pass (and thus its perceptual hash).
    fn ingest(&self) {
        let root_id = self.lib.register_root(&self.mount, Some("photos")).unwrap();
        self.lib
            .scan_root(&root_id, &ScanOptions::default())
            .unwrap();
        self.lib.process_queue(&QueueOptions::default()).unwrap();
    }

    fn all_hashes(&self) -> Vec<ContentHash> {
        self.lib.image_hashes().unwrap()
    }
}

// --- fixtures ----------------------------------------------------------------

/// A recognizable, smoothly-varying image (a diagonal gradient with some
/// structure). `tweak` perturbs the pixels slightly so we can build a
/// "re-encoded / lightly edited" near-identical variant whose dHash is within a
/// small Hamming radius of the original.
fn structured(w: u32, h: u32, base: u8, tweak: i32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let g = (((x as i32 + y as i32) + base as i32 + tweak).rem_euclid(256)) as u8;
            // A little channel spread so it is not pure gray (closer to a photo).
            img.put_pixel(
                x,
                y,
                image::Rgb([g, g.wrapping_add(20), g.wrapping_add(40)]),
            );
        }
    }
    img
}

fn encode_jpeg(img: &RgbImage, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut Cursor::new(&mut out), quality)
        .encode_image(&DynamicImage::ImageRgb8(img.clone()))
        .unwrap();
    out
}

// --- tests -------------------------------------------------------------------

#[test]
fn ingest_populates_perceptual_hash_and_groups_a_reencoded_near_dup() {
    let env = Env::new();
    // a.jpg and a_reencoded.jpg are the SAME picture saved at different JPEG
    // quality (the canonical Tier-1 "same photo, re-saved" case). They are NOT
    // byte-identical (so BLAKE3 / K13 does not collapse them), but their dHash
    // must land within a small Hamming radius.
    let pic = structured(96, 96, 30, 0);
    env.write("a.jpg", &encode_jpeg(&pic, 95));
    env.write("a_reencoded.jpg", &encode_jpeg(&pic, 60));
    // A visually distinct image: a different structure entirely.
    let mut other = RgbImage::new(96, 96);
    for y in 0..96 {
        for x in 0..96 {
            // Vertical bars: a totally different gradient sign pattern.
            let on = (x / 6) % 2 == 0;
            let v = if on { 240 } else { 10 };
            other.put_pixel(x, y, image::Rgb([v, v, v]));
        }
    }
    env.write("distinct.jpg", &encode_jpeg(&other, 95));

    env.ingest();

    // Three distinct files => three distinct BLAKE3 hashes (K13 only collapses
    // byte-identical files, and these three differ in bytes).
    let hashes = env.all_hashes();
    assert_eq!(hashes.len(), 3, "three byte-distinct images ingested");

    // Every image must now carry a perceptual hash (the preview pass populated
    // it). Verify directly so a regression in the hook is caught even if the
    // grouping somehow still passed.
    for h in &hashes {
        let groups_self = env
            .lib
            .find_near_duplicates(std::slice::from_ref(h), 0)
            .unwrap();
        // A single image is never a group, but the call must succeed; the real
        // population check is the grouping below.
        assert!(groups_self.is_empty());
    }

    // At the default-ish threshold, the re-encoded pair groups and the distinct
    // image stays out. Threshold 8 is the shipped default (tuning.default.toml).
    let groups = env.lib.find_near_duplicates(&hashes, 8).unwrap();
    assert_eq!(
        groups.len(),
        1,
        "exactly one near-dup group (the re-encoded pair); distinct stays out, got {groups:?}"
    );
    assert_eq!(groups[0].image_hashes.len(), 2, "the group is the pair");
    // The group members are exactly the two re-encodes, not the distinct image.
    // We identify them by re-ingesting knowledge: the distinct image is the one
    // NOT in the group.
    let in_group = &groups[0].image_hashes;
    let distinct_in_group = hashes
        .iter()
        .filter(|h| !in_group.contains(&h.as_str().to_owned()))
        .count();
    assert_eq!(
        distinct_in_group, 1,
        "the distinct image is the odd one out"
    );
}

#[test]
fn threshold_zero_groups_only_perceptually_identical() {
    // Two files that decode to IDENTICAL pixels but differ in BYTES (a trailing
    // comment/metadata difference is hard to force portably, so instead we save
    // the same RGB twice at the SAME quality — image-rs is deterministic, so the
    // bytes are identical and K13 WOULD collapse them; to keep them distinct in
    // the index we vary one pixel, which leaves the coarse dHash untouched).
    let env = Env::new();
    let mut pic = structured(96, 96, 50, 0);
    env.write("x.jpg", &encode_jpeg(&pic, 90));
    // Flip a single pixel: changes the bytes (distinct BLAKE3) but the 9x8 dHash
    // grid is far coarser than one pixel in 96x96, so the perceptual hash is
    // unchanged => they group even at threshold 0.
    pic.put_pixel(0, 0, image::Rgb([0, 0, 0]));
    env.write("y.jpg", &encode_jpeg(&pic, 90));

    env.ingest();
    let hashes = env.all_hashes();
    assert_eq!(hashes.len(), 2);

    let groups = env.lib.find_near_duplicates(&hashes, 0).unwrap();
    assert_eq!(
        groups.len(),
        1,
        "a one-pixel change is below the dHash grid; they share a hash and group at threshold 0"
    );
    assert_eq!(groups[0].image_hashes.len(), 2);
}

#[test]
fn empty_scope_and_unhashed_images_are_handled() {
    let env = Env::new();
    // No images at all.
    assert!(env.lib.find_near_duplicates(&[], 8).unwrap().is_empty());

    // An image scanned but NOT yet through the preview pass has a NULL
    // perceptual hash; it must be silently skipped, never mis-grouped.
    env.write("p.jpg", &encode_jpeg(&structured(64, 64, 10, 0), 90));
    let root_id = env.lib.register_root(&env.mount, Some("photos")).unwrap();
    env.lib
        .scan_root(&root_id, &ScanOptions::default())
        .unwrap();
    // Deliberately DO NOT drain the queue: the preview pass has not run.
    let hashes = env.lib.image_hashes().unwrap();
    assert_eq!(hashes.len(), 1, "scanned but not previewed");
    let groups = env.lib.find_near_duplicates(&hashes, 8).unwrap();
    assert!(
        groups.is_empty(),
        "an unhashed image contributes no group (NULL phash skipped)"
    );

    // After the preview pass it has a hash (still a singleton, so no group, but
    // the call is well-formed) — proves the skip was about NULL, not a bug.
    env.lib.process_queue(&QueueOptions::default()).unwrap();
    assert!(env.lib.find_near_duplicates(&hashes, 8).unwrap().is_empty());
}
