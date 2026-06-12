//! Preview pipeline: display-oriented, sRGB WebP artifacts.
//!
//! Contract: spec/LIBRARY.md §9 (DECISIONS L5/L6, P9). The load-bearing
//! parts, stated plainly:
//!
//! - EXIF orientation is applied at cache time; cached artifacts are ALWAYS
//!   display-oriented (§9.7) — the stroke coordinate space depends on it.
//! - Embedded RAW previews are inconsistently pre-rotated across makers;
//!   blindly applying the EXIF tag double-rotates some files (§9.3.1). The
//!   dimension heuristic below decides, with the preview's own orientation
//!   tag as an override where the extractor supplies one.
//! - ICC → sRGB at cache time; no profile → assume sRGB; EXIF
//!   ColorSpace=AdobeRGB without ICC → built-in AdobeRGB primaries.
//! - Atomic writes: temp + rename; a crash leaves the old artifact or none,
//!   never a torn file (§9.8).

use std::io;
use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImageView, ImageDecoder};

use crate::id::ContentHash;
use crate::metrics::PipelineMetrics;

/// Compile-time constant covering encoder, sizes, and the color pipeline
/// (§9.8). Orientation or ICC changes MUST bump this.
/// v2 (June 2026): two-step resize + libwebp method 2 (pp-bench-driven) —
/// artifact bytes changed, existing caches regenerate at backfill
/// priority via the §9.8 machinery.
pub const GENERATOR_VERSION: i64 = 2;

pub const THUMB_EDGE: u32 = 512;
pub const THUMB_QUALITY: f32 = 75.0;
pub const DISPLAY_EDGE: u32 = 2560;
pub const DISPLAY_QUALITY: f32 = 87.0;

/// §9.3 acceptability threshold: embedded preview longest edge ≥ 2048 px.
pub const EMBEDDED_ACCEPT_EDGE: u32 = 2048;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// Transient (§10.5): I/O, volume offline mid-read.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// Permanent: corrupt file, unsupported variant.
    #[error("decode: {0}")]
    Decode(String),
}

/// `preview_artifacts.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Thumb,
    Display,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Thumb => "thumb",
            ArtifactKind::Display => "display",
        }
    }
}

/// `preview_artifacts.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    Embedded,
    FullDecode,
    Original,
}

impl PreviewSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewSource::Embedded => "embedded",
            PreviewSource::FullDecode => "full-decode",
            PreviewSource::Original => "original",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedArtifact {
    pub kind: ArtifactKind,
    /// Display-oriented dimensions.
    pub width: u32,
    pub height: u32,
    pub bytes: i64,
}

// ---------------------------------------------------------------------------
// Cache layout (§9.8)
// ---------------------------------------------------------------------------

/// `<app_data>/previews/<h[0..2]>/<h[2..4]>/<hash>-{thumb,disp}.webp`.
pub fn artifact_path(cache_dir: &Path, hash: &ContentHash, kind: ArtifactKind) -> PathBuf {
    let h = hash.as_str();
    let suffix = match kind {
        ArtifactKind::Thumb => "thumb",
        ArtifactKind::Display => "disp",
    };
    cache_dir
        .join("previews")
        .join(&h[0..2])
        .join(&h[2..4])
        .join(format!("{h}-{suffix}.webp"))
}

const TMP_PREFIX: &str = ".pp-tmp-";

fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = dest.parent().expect("artifact path has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        "{TMP_PREFIX}{}-{}",
        std::process::id(),
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("a")
    ));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Crash hygiene: remove stranded temp files (the non-`done` pass row
/// re-runs the work; finals are never torn by construction).
pub fn sweep_temp_files(cache_dir: &Path) -> io::Result<usize> {
    let previews = cache_dir.join("previews");
    if !previews.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in walkdir::WalkDir::new(previews).into_iter().flatten() {
        if entry.file_type().is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(TMP_PREFIX))
        {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Orientation
// ---------------------------------------------------------------------------

/// Apply an EXIF orientation value (1..8) to produce the display-oriented
/// image. Out-of-range values are treated as 1 (and rejected upstream).
pub fn apply_exif_orientation(mut img: DynamicImage, orientation: u16) -> DynamicImage {
    if let Some(o) = u8::try_from(orientation)
        .ok()
        .and_then(image::metadata::Orientation::from_exif)
    {
        img.apply_orientation(o);
    }
    img
}

/// Display-oriented dimensions for stored dims + orientation.
pub fn oriented_dims(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if matches!(orientation, 5..=8) {
        (height, width)
    } else {
        (width, height)
    }
}

/// Why the §9.3.1 policy decided what it decided (logged; fixture coverage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedOrientationReason {
    /// The extractor supplied the preview's own orientation tag — authoritative
    /// cross-check (quickraw-style), used directly.
    PreviewOwnTag,
    /// Preview geometry already matches the display-oriented shape: no
    /// further rotation (pre-rotated maker).
    AlreadyDisplayOriented,
    /// Preview geometry matches the sensor shape: the EXIF tag applies.
    TagReliant,
    /// Square-ish geometry — the heuristic cannot decide; default to the tag
    /// (§9.3.1), logged for fixture coverage.
    SquareDefault,
    /// No raw dimensions available; default to the tag.
    NoRawDims,
}

/// The §9.3.1 decision: which orientation value to apply to the extracted
/// preview (1 = none).
pub fn embedded_orientation_decision(
    preview_w: u32,
    preview_h: u32,
    raw_w: Option<u32>,
    raw_h: Option<u32>,
    exif_orientation: u16,
    preview_own_orientation: Option<u16>,
) -> (u16, EmbeddedOrientationReason) {
    if let Some(own) = preview_own_orientation.filter(|o| (1..=8).contains(o)) {
        return (own, EmbeddedOrientationReason::PreviewOwnTag);
    }
    let tag = if (1..=8).contains(&exif_orientation) {
        exif_orientation
    } else {
        1
    };
    // Only the 90°-rotating family changes shape; 1..4 keep the aspect, so
    // the dimension heuristic is silent there and the tag applies.
    if !matches!(tag, 5..=8) {
        return (tag, EmbeddedOrientationReason::TagReliant);
    }
    let (Some(rw), Some(rh)) = (raw_w, raw_h) else {
        return (tag, EmbeddedOrientationReason::NoRawDims);
    };
    const SQUARE_TOLERANCE: f64 = 0.05;
    let aspect = |w: u32, h: u32| f64::from(w) / f64::from(h);
    let pa = aspect(preview_w, preview_h);
    let ra = aspect(rw, rh);
    if (pa - 1.0).abs() < SQUARE_TOLERANCE || (ra - 1.0).abs() < SQUARE_TOLERANCE {
        return (tag, EmbeddedOrientationReason::SquareDefault);
    }
    // Display-oriented shape for a 90° rotation = sensor shape swapped.
    let preview_landscape = pa > 1.0;
    let raw_landscape = ra > 1.0;
    if preview_landscape == raw_landscape {
        // Preview still sensor-shaped → the tag is the truth.
        (tag, EmbeddedOrientationReason::TagReliant)
    } else {
        // Preview already display-shaped → already rotated; applying the tag
        // again would double-rotate (the Wikimedia bug). Apply nothing.
        (1, EmbeddedOrientationReason::AlreadyDisplayOriented)
    }
}

// ---------------------------------------------------------------------------
// Color (§9.7)
// ---------------------------------------------------------------------------

/// Convert decoded pixels to sRGB: embedded ICC → qcms transform; no
/// profile → assume sRGB; EXIF AdobeRGB hint without ICC → built-in
/// AdobeRGB primaries.
pub fn to_srgb(img: DynamicImage, icc: Option<&[u8]>, adobe_rgb_hint: bool) -> DynamicImage {
    let mut rgba = img.into_rgba8();
    if let Some(icc) = icc {
        if let Some(profile) = qcms::Profile::new_from_slice(icc, false) {
            let srgb = qcms::Profile::new_sRGB();
            if let Some(transform) = qcms::Transform::new(
                &profile,
                &srgb,
                qcms::DataType::RGBA8,
                qcms::Intent::Perceptual,
            ) {
                transform.apply(rgba.as_mut());
                return DynamicImage::ImageRgba8(rgba);
            }
        }
        // Unparseable profile: assume sRGB (logged upstream).
        return DynamicImage::ImageRgba8(rgba);
    }
    if adobe_rgb_hint {
        adobe_rgb_to_srgb_in_place(rgba.as_mut());
    }
    DynamicImage::ImageRgba8(rgba)
}

/// AdobeRGB (1998, D65, gamma 563/256) → sRGB, via linear-light matrix.
fn adobe_rgb_to_srgb_in_place(rgba: &mut [u8]) {
    const ADOBE_GAMMA: f64 = 563.0 / 256.0;
    // AdobeRGB linear → sRGB linear (shared white point/red/blue primaries).
    const M: [[f64; 3]; 3] = [
        [1.398_283_2, -0.398_283_2, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -0.042_938_3, 1.042_938_3],
    ];
    // LUTs: 8-bit decode (Adobe gamma) and sRGB encode over 4096 steps.
    let mut decode = [0f64; 256];
    for (i, d) in decode.iter_mut().enumerate() {
        *d = (i as f64 / 255.0).powf(ADOBE_GAMMA);
    }
    let srgb_encode = |v: f64| -> u8 {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as u8
    };
    for px in rgba.chunks_exact_mut(4) {
        let r = decode[px[0] as usize];
        let g = decode[px[1] as usize];
        let b = decode[px[2] as usize];
        for (i, row) in M.iter().enumerate() {
            px[i] = srgb_encode(row[0] * r + row[1] * g + row[2] * b);
        }
    }
}

// ---------------------------------------------------------------------------
// Resize + encode + write
// ---------------------------------------------------------------------------

/// Resize to a longest-edge target. Never upscale (§9.2).
///
/// Deliberately ONE CatmullRom pass. The classic two-step trick (cheap
/// Triangle prescale to 2× the target, CatmullRom for the last octave)
/// was tried and MEASURED 3.4× SLOWER here (pp-bench canon corpus, June
/// 2026: 408 ms vs 120 ms mean) — image-rs's separable resampler already
/// scales its kernel with the ratio, so the prescale only added a second
/// full pass and a ~70 MB intermediate per image, which across 8 parallel
/// workers thrashes the cache. Do not "optimize" this without a bench
/// diff against the frozen corpora.
fn resize_to_edge(img: &DynamicImage, edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= edge {
        return img.clone();
    }
    img.resize(edge, edge, image::imageops::FilterType::CatmullRom)
}

/// WebP at libwebp `method` 2 (pp-bench, June 2026): the default method 4
/// spent ~30% of total ingest wall-time inside the encoder; method 2
/// roughly halves encode time for a few percent larger artifacts. Cache
/// bytes are cheap (never evicts, L5) — ingest latency is the cost the
/// founder feels. Falls back to the simple encoder if libwebp rejects the
/// config (never observed; belt only).
fn encode_webp(img: &DynamicImage, quality: f32) -> Vec<u8> {
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
    let Ok(mut config) = webp::WebPConfig::new() else {
        return encoder.encode(quality).to_vec();
    };
    config.quality = quality;
    config.method = 2;
    match encoder.encode_advanced(&config) {
        Ok(mem) => mem.to_vec(),
        Err(_) => encoder.encode(quality).to_vec(),
    }
}

/// Generate and atomically write both artifacts from a display-oriented,
/// sRGB image. Idempotent (overwrites, §10.4). `metrics` splits the
/// fan-out into its resize/encode/write costs — the three knobs a perf
/// pass would actually turn (filter choice, WebP quality/effort, IO).
pub fn write_artifacts(
    cache_dir: &Path,
    hash: &ContentHash,
    display_oriented: &DynamicImage,
    metrics: &PipelineMetrics,
) -> io::Result<Vec<GeneratedArtifact>> {
    let mut out = Vec::with_capacity(2);
    // Derive the thumb from the display-size render: one large resize, one
    // small one — and bitwise stability between the two artifacts' geometry.
    let display = metrics
        .resize
        .time(|| resize_to_edge(display_oriented, DISPLAY_EDGE));
    let thumb = metrics.resize.time(|| resize_to_edge(&display, THUMB_EDGE));
    for (kind, img, quality) in [
        (ArtifactKind::Display, &display, DISPLAY_QUALITY),
        (ArtifactKind::Thumb, &thumb, THUMB_QUALITY),
    ] {
        let encoded = metrics.encode.time(|| encode_webp(img, quality));
        let dest = artifact_path(cache_dir, hash, kind);
        metrics.write.time(|| atomic_write(&dest, &encoded))?;
        let (w, h) = img.dimensions();
        out.push(GeneratedArtifact {
            kind,
            width: w,
            height: h,
            bytes: encoded.len() as i64,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoding routes
// ---------------------------------------------------------------------------

/// §9.5 original route (JPEG/PNG/TIFF/WebP): decode, ICC→sRGB, orient.
/// Returns the display-oriented sRGB image plus the applied orientation.
///
/// `fallback_orientation` is the §9.6 value the exif pass stored (kamadak
/// reads TIFF IFD orientation that some `image` decoders do not surface);
/// the decoder's own EXIF reading wins when it reports a non-identity value.
pub fn decode_original_display_oriented(
    path: &Path,
    fallback_orientation: u16,
) -> Result<(DynamicImage, u16), PreviewError> {
    let reader = image::ImageReader::open(path)?
        .with_guessed_format()
        .map_err(|e| PreviewError::Decode(e.to_string()))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| PreviewError::Decode(e.to_string()))?;
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = decoder
        .orientation()
        .ok()
        .map(|o| o.to_exif() as u16)
        .filter(|&o| o != 1)
        .or(Some(fallback_orientation).filter(|o| (1..=8).contains(o)))
        .unwrap_or(1);
    let img =
        DynamicImage::from_decoder(decoder).map_err(|e| PreviewError::Decode(e.to_string()))?;
    let srgb = to_srgb(img, icc.as_deref(), false);
    Ok((apply_exif_orientation(srgb, orientation), orientation))
}

// ---------------------------------------------------------------------------
// Embedded preview extraction (§9.3) — injectable seam
// ---------------------------------------------------------------------------

/// What a RAW container yielded.
pub struct ExtractedPreview {
    /// The decoded embedded preview, as stored (no orientation applied yet).
    pub image: DynamicImage,
    /// The RAW's stated sensor dimensions, when cheaply available.
    pub raw_width: Option<u32>,
    pub raw_height: Option<u32>,
    /// The RAW's EXIF orientation tag (1..8; 1 if absent).
    pub exif_orientation: u16,
    /// The preview's own orientation, when the container states one.
    pub preview_orientation: Option<u16>,
}

/// Metadata-only embedded-preview extraction. Injectable so acceptance
/// tests can drive the §9.3/§9.3.1 routing without real RAW fixtures
/// (per-format real-RAW verification is founder-machine work).
pub trait EmbeddedPreviewExtractor: Send + Sync {
    /// `Ok(None)` = container parsed but no usable (JPEG) preview — the
    /// caller enqueues `full-raw-decode` at elevated priority. CR3 HDR-PQ
    /// HEIF previews land here too (§9.3).
    fn extract(&self, path: &Path) -> Result<Option<ExtractedPreview>, PreviewError>;
}

/// A JPEG preview found by walking the RAW's TIFF IFD chain: where it
/// lives in the file, its header-probed pixel dims, and the orientation
/// tag of the IFD that holds it (None for the root IFD — see
/// [`largest_chained_jpeg`] for why root orientation is never trusted as
/// the preview's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainedJpegCandidate {
    pub offset: u64,
    pub len: u64,
    pub width: u32,
    pub height: u32,
    pub own_orientation: Option<u16>,
}

impl ChainedJpegCandidate {
    fn edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

/// Walk EVERY IFD of a TIFF-structured RAW — the chained next-IFD list
/// plus each IFD's sub-IFDs — for JPEGInterchangeFormat/-Length pairs and
/// return the largest embedded JPEG by pixel edge (header-probed; nothing
/// is fully decoded here).
///
/// WHY THIS EXISTS (founder bug, dogfood round 2): rawler's per-format
/// `full_image` implementations only consult the IFD their format
/// historically used. Sony ARW (A7C/A7R V/A7CR class) parks the
/// full-resolution camera JPEG in a CHAINED IFD — exiftool's IFD2
/// "JpgFromRaw", tags 0x0201/0x0202 — while the root IFD's same tag pair
/// points at the legacy 1616×1080 preview. rawler 0.7.2's ArwDecoder reads
/// only the root pair, so both the ingest preview pass and the on-demand
/// /embedded route saw a small preview, and the embedded-native pixel-gain
/// gate then refused (correctly, but the full-res JPEG was sitting right
/// there in the file). Verified against the founder's ILCE-7CR files.
///
/// Non-TIFF RAW containers (CR3/BMFF) fail the TIFF parse and yield None;
/// candidates whose byte range or JPEG header does not check out are
/// skipped — this is a best-effort sweep on top of rawler's ladder, never
/// a replacement for it.
pub fn largest_chained_jpeg(source: &rawler::rawsource::RawSource) -> Option<ChainedJpegCandidate> {
    use rawler::formats::tiff::reader::TiffReader;
    use rawler::formats::tiff::{GenericTiffReader, IFD};

    let mut reader = source.reader();
    let tiff = GenericTiffReader::new(&mut reader, 0, 0, None, &[]).ok()?;

    fn consider(
        source: &rawler::rawsource::RawSource,
        ifd: &IFD,
        is_root: bool,
        best: &mut Option<ChainedJpegCandidate>,
    ) {
        use rawler::tags::ExifTag;
        let (Some(off), Some(len)) = (
            ifd.get_entry(ExifTag::JPEGInterchangeFormat),
            ifd.get_entry(ExifTag::JPEGInterchangeFormatLength),
        ) else {
            return;
        };
        let (offset, len) = (off.force_u64(0), len.force_u64(0));
        if len == 0 {
            return;
        }
        // Out-of-range pointers (truncated/corrupt files) skip silently.
        let Ok(bytes) = source.subview(offset, len) else {
            return;
        };
        // Header-only probe: dimensions without decoding megapixels.
        let Ok((width, height)) =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg)
                .into_dimensions()
        else {
            return;
        };
        // The preview's OWN orientation tag — only trusted off the root
        // IFD: per TIFF/EP the root Orientation describes the main (raw)
        // image, which rawler already surfaces as the EXIF orientation;
        // claiming it as the preview's own would bypass the §9.3.1
        // pre-rotation heuristic for exactly the files that need it.
        let own_orientation = if is_root {
            None
        } else {
            ifd.get_entry(ExifTag::Orientation)
                .map(|e| e.force_u16(0))
                .filter(|o| (1..=8).contains(o))
        };
        let candidate = ChainedJpegCandidate {
            offset,
            len,
            width,
            height,
            own_orientation,
        };
        if best.is_none_or(|b| candidate.edge() > b.edge()) {
            *best = Some(candidate);
        }
    }

    let mut best: Option<ChainedJpegCandidate> = None;
    for (idx, ifd) in tiff.chains().iter().enumerate() {
        consider(source, ifd, idx == 0, &mut best);
        for subs in ifd.sub_ifds().values() {
            for sub in subs {
                consider(source, sub, false, &mut best);
            }
        }
    }
    best
}

/// rawler-backed extraction (§9.3: metadata-only parse, no demosaic).
#[derive(Debug, Default)]
pub struct RawlerExtractor;

impl EmbeddedPreviewExtractor for RawlerExtractor {
    fn extract(&self, path: &Path) -> Result<Option<ExtractedPreview>, PreviewError> {
        let source = rawler::rawsource::RawSource::new(path)?;
        let decoder = rawler::get_decoder(&source)
            .map_err(|e| PreviewError::Decode(format!("rawler: {e}")))?;
        let params = rawler::decoders::RawDecodeParams::default();
        let exif_orientation = decoder
            .raw_metadata(&source, &params)
            .ok()
            .and_then(|md| md.exif.orientation)
            .filter(|o| (1..=8).contains(o))
            .unwrap_or(1);
        // rawler's ladder first (format-specific knowledge lives there)…
        let decoder_preview = decoder
            .full_image(&source, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.preview_image(&source, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(&source, &params).ok().flatten());
        // …then the chained-IFD sweep, which wins only on a strictly
        // larger pixel edge (header-probed before paying for the decode).
        // Sony ARW: the ladder yields the 1616×1080 root preview while the
        // full 9504×6336 camera JPEG sits in chained IFD2.
        let chained = largest_chained_jpeg(&source).filter(|c| {
            decoder_preview.as_ref().is_none_or(|d| {
                use image::GenericImageView;
                let (dw, dh) = d.dimensions();
                c.edge() > dw.max(dh)
            })
        });
        let (image, preview_orientation) = match chained {
            Some(c) => {
                let decoded = source.subview(c.offset, c.len).ok().and_then(|bytes| {
                    image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()
                });
                match decoded {
                    // A header that probed fine but fails the full decode
                    // (truncated stream) falls back to rawler's ladder.
                    Some(img) => (Some(img), c.own_orientation),
                    None => (decoder_preview, None),
                }
            }
            None => (decoder_preview, None),
        };
        let Some(image) = image else {
            return Ok(None);
        };
        // Sensor dims via the dummy decode (no decompression).
        let (raw_width, raw_height) = match rawler::decode_dummy(&source) {
            Ok(raw) => (
                u32::try_from(raw.width).ok(),
                u32::try_from(raw.height).ok(),
            ),
            Err(_) => (None, None),
        };
        Ok(Some(ExtractedPreview {
            image,
            raw_width,
            raw_height,
            exif_orientation,
            preview_orientation,
        }))
    }
}

// ---------------------------------------------------------------------------
// Embedded-native full resolution (Look's progressive route — founder,
// dogfood round 2): the embedded JPEG served at NATIVE size, on demand
// ---------------------------------------------------------------------------

/// JPEG quality for the on-demand native-size re-encode (decoded by the
/// extractor, re-oriented per §9.3.1, encoded once per request — the
/// protocol's immutable cache headers let the webview's HTTP cache hold it).
pub const EMBEDDED_NATIVE_QUALITY: u8 = 90;

/// Aspect agreement tolerance between the oriented native image and the
/// cached display artifact. Resize rounding on a 2560-edge artifact moves
/// the aspect by well under 1%; a 90° disagreement inverts it entirely.
const EMBEDDED_NATIVE_ASPECT_TOLERANCE: f64 = 0.02;

/// The serve decision for the embedded-native route, pure: serve only when
/// the display-oriented embedded preview actually ADDS pixels over the
/// cached display artifact, AND its aspect agrees with that artifact.
/// Stated plainly (§9.3.1/§9.7): strokes live in display-oriented image
/// space — a native image whose geometry disagrees with the stroke
/// substrate would rotate/misplace every mark at deep zoom. Disagreement
/// or no gain = refuse; the 2560 preview stands silently.
pub fn embedded_native_acceptable(
    oriented_w: u32,
    oriented_h: u32,
    display_w: u32,
    display_h: u32,
) -> bool {
    if oriented_w == 0 || oriented_h == 0 || display_w == 0 || display_h == 0 {
        return false;
    }
    // No pixel gain (small embedded previews, sources at or below the
    // display edge): the cached artifact already carries every pixel.
    if oriented_w.max(oriented_h) <= display_w.max(display_h) {
        return false;
    }
    let oa = f64::from(oriented_w) / f64::from(oriented_h);
    let da = f64::from(display_w) / f64::from(display_h);
    ((oa - da) / da).abs() < EMBEDDED_NATIVE_ASPECT_TOLERANCE
}

/// Encode a display-oriented image as JPEG for the native-size route (JPEG
/// carries no alpha; the embedded source never had any).
pub fn encode_jpeg_native(img: &DynamicImage) -> Result<Vec<u8>, PreviewError> {
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut std::io::Cursor::new(&mut out),
        EMBEDDED_NATIVE_QUALITY,
    )
    .encode_image(&rgb)
    .map_err(|e| PreviewError::Decode(e.to_string()))?;
    Ok(out)
}

/// Orient an extracted preview per the §9.3.1 policy. Returns the
/// display-oriented image and the decision (for logging/tests).
pub fn orient_embedded_preview(
    extracted: ExtractedPreview,
) -> (DynamicImage, u16, EmbeddedOrientationReason) {
    let (pw, ph) = extracted.image.dimensions();
    let (apply, reason) = embedded_orientation_decision(
        pw,
        ph,
        extracted.raw_width,
        extracted.raw_height,
        extracted.exif_orientation,
        extracted.preview_orientation,
    );
    (
        apply_exif_orientation(extracted.image, apply),
        apply,
        reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_fan_out() {
        let h = ContentHash::from_bytes_of(b"x");
        let p = artifact_path(Path::new("/data"), &h, ArtifactKind::Thumb);
        let s = p.to_string_lossy();
        assert!(s.starts_with("/data/previews/"));
        assert!(s.ends_with(&format!("{}-thumb.webp", h.as_str())));
        let parts: Vec<&str> = s.split('/').collect();
        assert_eq!(parts[3], &h.as_str()[0..2]);
        assert_eq!(parts[4], &h.as_str()[2..4]);
    }

    #[test]
    fn oriented_dims_swap_for_rotating_family() {
        assert_eq!(oriented_dims(600, 400, 1), (600, 400));
        assert_eq!(oriented_dims(600, 400, 3), (600, 400));
        assert_eq!(oriented_dims(600, 400, 6), (400, 600));
        assert_eq!(oriented_dims(600, 400, 8), (400, 600));
    }

    #[test]
    fn policy_pre_rotated_preview_is_left_alone() {
        // Nikon-style: portrait shot, sensor 6000x4000, orientation 6, but
        // the preview was written already display-oriented (1080x1616).
        let (apply, reason) =
            embedded_orientation_decision(1080, 1616, Some(6000), Some(4000), 6, None);
        assert_eq!(apply, 1);
        assert_eq!(reason, EmbeddedOrientationReason::AlreadyDisplayOriented);
    }

    #[test]
    fn policy_sensor_shaped_preview_gets_the_tag() {
        // Fuji-style: preview still sensor-oriented (1616x1080), tag 6.
        let (apply, reason) =
            embedded_orientation_decision(1616, 1080, Some(6000), Some(4000), 6, None);
        assert_eq!(apply, 6);
        assert_eq!(reason, EmbeddedOrientationReason::TagReliant);
    }

    #[test]
    fn policy_square_defaults_to_tag_and_flags_it() {
        let (apply, reason) =
            embedded_orientation_decision(2000, 2010, Some(4000), Some(4020), 6, None);
        assert_eq!(apply, 6);
        assert_eq!(reason, EmbeddedOrientationReason::SquareDefault);
    }

    #[test]
    fn policy_own_tag_wins() {
        let (apply, reason) =
            embedded_orientation_decision(1616, 1080, Some(6000), Some(4000), 6, Some(1));
        assert_eq!(apply, 1);
        assert_eq!(reason, EmbeddedOrientationReason::PreviewOwnTag);
    }

    #[test]
    fn policy_no_raw_dims_defaults_to_tag() {
        let (apply, reason) = embedded_orientation_decision(1616, 1080, None, None, 8, None);
        assert_eq!(apply, 8);
        assert_eq!(reason, EmbeddedOrientationReason::NoRawDims);
    }

    #[test]
    fn native_route_serves_only_genuine_pixel_gain() {
        // Full-res embedded over a 2560-class artifact: serve.
        assert!(embedded_native_acceptable(6000, 4000, 2560, 1707));
        // Small embedded preview (older-Sony-ARW class): the display
        // artifact IS the embedded preview — no gain, preview stands.
        assert!(!embedded_native_acceptable(1616, 1080, 1616, 1080));
        // At or below the display edge, even when larger than the artifact
        // on one axis only: max-edge rule.
        assert!(!embedded_native_acceptable(2200, 1467, 2200, 1467));
        // Degenerate dims never serve.
        assert!(!embedded_native_acceptable(0, 0, 2560, 1707));
        assert!(!embedded_native_acceptable(6000, 4000, 0, 0));
    }

    #[test]
    fn native_route_refuses_geometry_disagreement() {
        // A 90° disagreement inverts the aspect — refused (stroke safety).
        assert!(!embedded_native_acceptable(4000, 6000, 2560, 1707));
        // Resize rounding stays well inside the tolerance.
        assert!(embedded_native_acceptable(6000, 4000, 2560, 1706));
        assert!(embedded_native_acceptable(3600, 2400, 2560, 1707));
    }

    #[test]
    fn native_encode_round_trips_dimensions() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(120, 80));
        let bytes = encode_jpeg_native(&img).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.dimensions(), (120, 80));
    }

    #[test]
    fn never_upscale() {
        let img = DynamicImage::new_rgba8(100, 60);
        let resized = resize_to_edge(&img, 512);
        assert_eq!(resized.dimensions(), (100, 60));
    }

    // ---- the chained-IFD JPEG sweep (founder bug: Sony ARW JpgFromRaw) ----

    /// Decodable JPEG bytes at the requested dimensions.
    fn jpeg_blob(w: u32, h: u32) -> Vec<u8> {
        let img =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([90, 120, 60])));
        encode_jpeg_native(&img).unwrap()
    }

    /// Hand-rolled little-endian classic TIFF: one chained IFD per element,
    /// each carrying a JPEGInterchangeFormat/-Length pair (and an
    /// Orientation tag when given) — the exact shape Sony ARW uses for its
    /// root preview + chained "JpgFromRaw" full-size JPEG.
    fn synthetic_tiff(ifds: &[(Vec<u8>, Option<u16>)]) -> Vec<u8> {
        fn entry(out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32) {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&typ.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }
        // Layout: header, then every JPEG blob, then the IFD chain.
        let mut blob_off = Vec::new();
        let mut cursor = 8u32;
        for (jpeg, _) in ifds {
            blob_off.push(cursor);
            cursor += jpeg.len() as u32;
        }
        let mut ifd_off = Vec::new();
        for (_, own) in ifds {
            ifd_off.push(cursor);
            let n = if own.is_some() { 3u32 } else { 2u32 };
            cursor += 2 + 12 * n + 4;
        }
        let mut out = Vec::with_capacity(cursor as usize);
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&ifd_off[0].to_le_bytes());
        for (jpeg, _) in ifds {
            out.extend_from_slice(jpeg);
        }
        for (i, (jpeg, own)) in ifds.iter().enumerate() {
            let n: u16 = if own.is_some() { 3 } else { 2 };
            out.extend_from_slice(&n.to_le_bytes());
            // Entries in ascending tag order (TIFF requires it; rawler is
            // lenient but the fixture should be honest).
            if let Some(o) = own {
                entry(&mut out, 0x0112, 3, 1, u32::from(*o)); // Orientation, SHORT
            }
            entry(&mut out, 0x0201, 4, 1, blob_off[i]); // JPEGInterchangeFormat
            entry(&mut out, 0x0202, 4, 1, jpeg.len() as u32); // …Length
            let next = ifd_off.get(i + 1).copied().unwrap_or(0);
            out.extend_from_slice(&next.to_le_bytes());
        }
        out
    }

    /// THE founder bug shape (Sony ILCE-7CR ARW): root IFD points at the
    /// legacy small preview, a chained IFD carries the full-resolution
    /// camera JPEG with its own Orientation tag. The sweep must pick the
    /// chained JPEG — rawler's root-only lookup is exactly what starved the
    /// preview pass AND the /embedded route down to 1616×1080.
    #[test]
    fn chained_ifd_full_jpeg_wins_over_root_preview() {
        let tiff = synthetic_tiff(&[
            (jpeg_blob(160, 100), None),    // root: small preview
            (jpeg_blob(320, 200), Some(8)), // chained: "JpgFromRaw"
        ]);
        let source = rawler::rawsource::RawSource::new_from_slice(&tiff);
        let c = largest_chained_jpeg(&source).expect("sweep found a JPEG");
        assert_eq!((c.width, c.height), (320, 200));
        assert_eq!(c.own_orientation, Some(8), "the holding IFD's own tag");
        // And the candidate's byte range round-trips through a real decode.
        let bytes = source.subview(c.offset, c.len).unwrap();
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap();
        assert_eq!(img.dimensions(), (320, 200));
    }

    /// Root-held previews never claim the root Orientation tag as their
    /// own: per TIFF/EP it describes the main (raw) image — rawler already
    /// surfaces it as the EXIF orientation, and treating it as the
    /// preview's own tag would bypass the §9.3.1 pre-rotation heuristic.
    #[test]
    fn root_preview_keeps_no_own_orientation() {
        let tiff = synthetic_tiff(&[(jpeg_blob(320, 200), Some(8))]);
        let source = rawler::rawsource::RawSource::new_from_slice(&tiff);
        let c = largest_chained_jpeg(&source).expect("root JPEG found");
        assert_eq!((c.width, c.height), (320, 200));
        assert_eq!(c.own_orientation, None);
        // A bigger root beats a smaller chained JPEG (the sweep is by pixel
        // edge, not by chain position).
        let tiff = synthetic_tiff(&[(jpeg_blob(320, 200), None), (jpeg_blob(160, 100), Some(8))]);
        let source = rawler::rawsource::RawSource::new_from_slice(&tiff);
        let c = largest_chained_jpeg(&source).expect("root JPEG found");
        assert_eq!((c.width, c.height), (320, 200));
        assert_eq!(c.own_orientation, None);
    }

    /// Best-effort discipline: non-TIFF containers (CR3/BMFF) and entries
    /// whose bytes are not a JPEG refuse quietly — rawler's own ladder
    /// remains the fallback, never an error.
    #[test]
    fn sweep_refuses_non_tiff_and_garbage_quietly() {
        let source = rawler::rawsource::RawSource::new_from_slice(b"not a tiff at all");
        assert!(largest_chained_jpeg(&source).is_none());
        // A well-formed TIFF whose "JPEG" bytes do not parse: skipped.
        let tiff = synthetic_tiff(&[(b"definitely not jpeg data".to_vec(), None)]);
        let source = rawler::rawsource::RawSource::new_from_slice(&tiff);
        assert!(largest_chained_jpeg(&source).is_none());
    }

    #[test]
    fn adobe_rgb_conversion_shifts_mixed_colors() {
        // A green-dominant mix loses red when AdobeRGB's wider green is
        // mapped into sRGB; alpha must pass through untouched.
        let mut px = vec![100u8, 200, 50, 255];
        adobe_rgb_to_srgb_in_place(&mut px);
        assert_eq!(px[3], 255);
        assert!(px[0] < 100, "red did not shift: {px:?}");
    }
}
