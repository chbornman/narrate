//! Synthetic photo-library generator (P4.1 deliverable B).
//!
//! Generates realistic trees on a real temp filesystem: nested
//! year/shoot folders, JPEG/PNG/TIFF files with EXIF (orientation variants,
//! capture dates, camera make/model), byte-duplicate pairs, and plain files
//! suitable for rename/move scenarios. No binary fixtures: everything is
//! generated.
//!
//! Speed: JPEGs are template-based — a handful of images are encoded once
//! per (orientation × camera) and each file gets a unique COM segment
//! spliced in (decodable, unique bytes, no per-file encode). TIFFs append
//! unique trailing bytes (TIFF is offset-addressed; trailers are inert).
//! PNGs are encoded per file (small share, tiny dimensions). The full-50k
//! tree generates in seconds; CI-suite variants use 2–5k.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbImage};
use photoproof_core::ContentHash;

// ---------------------------------------------------------------------------
// Spec & report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SynthSpec {
    /// Total files generated (duplicates included).
    pub files: usize,
    /// Every n-th file is a byte-identical copy of its predecessor, placed
    /// in a different folder (K13: one image, N paths). 0 = none.
    pub dup_every: usize,
    /// Deterministic content seed.
    pub seed: u64,
}

impl SynthSpec {
    pub fn n(files: usize) -> Self {
        Self {
            files,
            dup_every: 100,
            seed: 0x5eed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SynthFile {
    /// `/`-separated path relative to the generation root.
    pub rel_path: String,
    /// BLAKE3 of the file bytes — independent ground truth for ingest.
    pub hash: ContentHash,
    pub byte_len: usize,
    /// "jpeg" | "png" | "tiff".
    pub format: &'static str,
    /// EXIF orientation written into the file (1 when absent).
    pub orientation: u16,
    /// EXIF DateTimeOriginal as the ingest pipeline will store it
    /// (RFC 3339 UTC), when the file carries one.
    pub capture_ts: Option<String>,
    pub camera_make: Option<&'static str>,
    pub camera_model: Option<&'static str>,
    /// Index into `SynthTree::files` of the file this is a byte-duplicate
    /// of, if any.
    pub duplicate_of: Option<usize>,
}

#[derive(Debug)]
pub struct SynthTree {
    pub root: PathBuf,
    pub files: Vec<SynthFile>,
    /// Count of distinct content hashes (= expected `images` rows).
    pub unique_hashes: usize,
}

impl SynthTree {
    /// Ground truth: expected (rel_path → hash) for every generated file.
    pub fn expected(&self) -> impl Iterator<Item = (&str, &ContentHash)> {
        self.files.iter().map(|f| (f.rel_path.as_str(), &f.hash))
    }
}

// ---------------------------------------------------------------------------
// Deterministic RNG (splitmix64 — no dev-dep needed here)
// ---------------------------------------------------------------------------

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

const CAMERAS: &[(&str, &str)] = &[
    ("NIKON CORPORATION", "NIKON Z 8"),
    ("FUJIFILM", "X-T5"),
    ("Canon", "Canon EOS R5"),
];

const ORIENTATIONS: &[u16] = &[1, 1, 1, 1, 6, 6, 8, 3]; // landscape-heavy, like real libraries

const SHOOTS: &[&str] = &[
    "harbor",
    "iceland",
    "studio",
    "rough-water",
    "portraits",
    "street",
    "garden",
    "workshop",
];

/// Generate the tree under `root` (created if missing). Deterministic for a
/// given spec.
pub fn generate(root: &Path, spec: &SynthSpec) -> SynthTree {
    let mut rng = Rng::new(spec.seed);
    let mut files: Vec<SynthFile> = Vec::with_capacity(spec.files);
    let mut made_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut templates: HashMap<(u16, usize), Vec<u8>> = HashMap::new();
    let mut unique = 0usize;

    for i in 0..spec.files {
        // Folder: <year>/<MM>-<shoot>/ — nested, dated, realistic.
        let year = 2023 + (i / 1_500) % 4;
        let month = 1 + (i / 120) % 12;
        let shoot = SHOOTS[(i / 120) % SHOOTS.len()];
        let dir_rel = format!("{year}/{month:02}-{shoot}");

        let is_dup = spec.dup_every > 0 && i > 0 && i % spec.dup_every == 0;
        let (rel_path, bytes, meta) = if is_dup {
            // Byte-identical copy of the previous file under a sibling dir.
            let prev = files.last().expect("i > 0");
            let dup_rel = format!("{dir_rel}/copies/{}", file_name_of(&prev.rel_path));
            let bytes = std::fs::read(root.join(&prev.rel_path)).expect("prev file exists");
            let meta = FileMeta {
                format: prev.format,
                orientation: prev.orientation,
                capture_ts: prev.capture_ts.clone(),
                camera: prev
                    .camera_make
                    .zip(prev.camera_model)
                    .map(|(m, mo)| CAMERAS.iter().position(|c| c.0 == m && c.1 == mo).unwrap())
                    .unwrap_or(0),
                duplicate_of: Some(files.len() - 1),
            };
            (dup_rel, bytes, meta)
        } else {
            unique += 1;
            let orientation = ORIENTATIONS[(rng.below(ORIENTATIONS.len() as u64)) as usize];
            let camera = (rng.below(CAMERAS.len() as u64)) as usize;
            // Capture time: spread through the folder's month.
            let day = 1 + (i % 28);
            let (hh, mm, ss) = (8 + i % 12, i % 60, (i * 7) % 60);
            let exif_dt = format!("{year}:{month:02}:{day:02} {hh:02}:{mm:02}:{ss:02}");
            let rfc3339 = format!("{year}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}.000Z");

            // Format mix: ~90% JPEG / 5% PNG / 5% TIFF.
            let pick = rng.below(20);
            if pick == 18 {
                // PNG: unique pixels, no EXIF (undated files are a real case).
                let bytes = unique_png(spec.seed ^ i as u64);
                (
                    format!("{dir_rel}/IMG_{i:05}.png"),
                    bytes,
                    FileMeta {
                        format: "png",
                        orientation: 1,
                        capture_ts: None,
                        camera,
                        duplicate_of: None,
                    },
                )
            } else if pick == 19 {
                // TIFF: orientation tag in IFD0 + unique trailing bytes.
                let base = templates
                    .entry((orientation, 100)) // 100 = tiff template slot
                    .or_insert_with(|| tiff_template(orientation))
                    .clone();
                let mut bytes = base;
                bytes.extend_from_slice(
                    format!("pp-synth-{:016x}", spec.seed ^ i as u64).as_bytes(),
                );
                (
                    format!("{dir_rel}/IMG_{i:05}.tif"),
                    bytes,
                    FileMeta {
                        format: "tiff",
                        orientation,
                        capture_ts: None, // tag-only TIFF carries no datetime
                        camera,
                        duplicate_of: None,
                    },
                )
            } else {
                // JPEG: template per (orientation, camera) + unique COM
                // segment + per-file EXIF datetime.
                let template = templates
                    .entry((orientation, camera))
                    .or_insert_with(|| jpeg_template(orientation))
                    .clone();
                let (make, model) = CAMERAS[camera];
                let bytes = jpeg_with_exif_and_com(
                    &template,
                    make,
                    model,
                    orientation,
                    &exif_dt,
                    &format!("pp-synth-{:016x}", spec.seed ^ i as u64),
                );
                (
                    format!("{dir_rel}/IMG_{i:05}.jpg"),
                    bytes,
                    FileMeta {
                        format: "jpeg",
                        orientation,
                        capture_ts: Some(rfc3339),
                        camera,
                        duplicate_of: None,
                    },
                )
            }
        };

        let abs = root.join(&rel_path);
        let dir = abs.parent().expect("has parent").to_path_buf();
        if made_dirs.insert(dir.clone()) {
            std::fs::create_dir_all(&dir).expect("mkdir");
        }
        std::fs::write(&abs, &bytes).expect("write synth file");
        let (make, model) = CAMERAS[meta.camera];
        files.push(SynthFile {
            rel_path,
            hash: ContentHash::from_bytes_of(&bytes),
            byte_len: bytes.len(),
            format: meta.format,
            orientation: meta.orientation,
            capture_ts: meta.capture_ts,
            camera_make: (meta.format == "jpeg").then_some(make),
            camera_model: (meta.format == "jpeg").then_some(model),
            duplicate_of: meta.duplicate_of,
        });
    }

    SynthTree {
        root: root.to_path_buf(),
        files,
        unique_hashes: unique,
    }
}

struct FileMeta {
    format: &'static str,
    orientation: u16,
    capture_ts: Option<String>,
    camera: usize,
    duplicate_of: Option<usize>,
}

fn file_name_of(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

// ---------------------------------------------------------------------------
// Pixel content (display-oriented test pattern, inverse-oriented for storage)
// ---------------------------------------------------------------------------

/// Display-oriented pattern: blue field, red square top-left — the same
/// convention as the LIBRARY orientation fixtures, so preview-orientation
/// assertions can reuse `assert_upright`-style probes.
fn display_pattern(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::from_pixel(w, h, image::Rgb([20, 40, 200]));
    let m = (w.min(h) / 5).max(4);
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
        6 => d.rotate270().into_rgb8(),
        8 => d.rotate90().into_rgb8(),
        o => panic!("unsupported synth orientation {o}"),
    }
}

fn jpeg_template(orientation: u16) -> Vec<u8> {
    // Portrait display geometry for rotated orientations, landscape for 1/3.
    let display = if orientation == 6 || orientation == 8 {
        display_pattern(96, 144)
    } else {
        display_pattern(144, 96)
    };
    let stored = store_for_orientation(&display, orientation);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut Cursor::new(&mut out), 88)
        .encode_image(&DynamicImage::ImageRgb8(stored))
        .expect("jpeg encode");
    out
}

fn unique_png(seed: u64) -> Vec<u8> {
    let mut img = RgbImage::new(16, 16);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = seed
            .wrapping_mul(0x2545F4914F6CDD1D)
            .wrapping_add(u64::from(x * 31 + y * 7));
        *p = image::Rgb([
            (v & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
        ]);
    }
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png encode");
    out
}

/// Hand-rolled uncompressed RGB TIFF with an Orientation tag in IFD0 (the
/// §9.6 kamadak-exif fallback path, B18).
fn tiff_template(orientation: u16) -> Vec<u8> {
    let display = if orientation == 6 || orientation == 8 {
        display_pattern(24, 36)
    } else {
        display_pattern(36, 24)
    };
    let stored = store_for_orientation(&display, orientation);
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

// ---------------------------------------------------------------------------
// EXIF (APP1) builder: IFD0 {Make, Model, Orientation, ExifIFD*} →
// Exif sub-IFD {DateTimeOriginal} — exactly the §9.6 subset fields the
// metadata pass extracts via kamadak-exif.
// ---------------------------------------------------------------------------

fn exif_app1(make: &str, model: &str, orientation: u16, datetime: &str) -> Vec<u8> {
    const ASCII: u16 = 2;
    const SHORT: u16 = 3;
    const LONG: u16 = 4;

    let make_v = format!("{make}\0").into_bytes();
    let model_v = format!("{model}\0").into_bytes();
    let dt_v = format!("{datetime}\0").into_bytes();

    // Layout (offsets relative to the TIFF header start):
    // 8: IFD0 (4 entries) → 8 + 2 + 48 + 4 = 62: value area (make, model)
    // → Exif sub-IFD (1 entry) → its value area (datetime).
    let ifd0_off = 8u32;
    let ifd0_len = 2 + 4 * 12 + 4;
    let val0_off = ifd0_off + ifd0_len;
    let make_off = val0_off;
    let model_off = make_off + make_v.len() as u32;
    let exif_ifd_off = model_off + model_v.len() as u32;
    let exif_ifd_len = 2 + 12 + 4;
    let dt_off = exif_ifd_off + exif_ifd_len;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&ifd0_off.to_le_bytes());
    let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    };
    // IFD0, tags ascending.
    tiff.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut tiff, 0x010F, ASCII, make_v.len() as u32, make_off); // Make
    entry(&mut tiff, 0x0110, ASCII, model_v.len() as u32, model_off); // Model
    entry(&mut tiff, 0x0112, SHORT, 1, u32::from(orientation)); // Orientation
    entry(&mut tiff, 0x8769, LONG, 1, exif_ifd_off); // ExifIFD pointer
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    tiff.extend_from_slice(&make_v);
    tiff.extend_from_slice(&model_v);
    // Exif sub-IFD.
    tiff.extend_from_slice(&1u16.to_le_bytes());
    entry(&mut tiff, 0x9003, ASCII, dt_v.len() as u32, dt_off); // DateTimeOriginal
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(&dt_v);

    let mut seg = Vec::new();
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    let mut out = vec![0xFF, 0xE1];
    out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&seg);
    out
}

/// Splice EXIF (after SOI) and a unique COM segment into a template JPEG.
fn jpeg_with_exif_and_com(
    template: &[u8],
    make: &str,
    model: &str,
    orientation: u16,
    datetime: &str,
    unique: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(template.len() + 256);
    out.extend_from_slice(&template[..2]); // SOI
    out.extend_from_slice(&exif_app1(make, model, orientation, datetime));
    // COM segment: unique bytes, skipped by every decoder.
    let com = unique.as_bytes();
    out.extend_from_slice(&[0xFF, 0xFE]);
    out.extend_from_slice(&((com.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(com);
    out.extend_from_slice(&template[2..]);
    out
}
