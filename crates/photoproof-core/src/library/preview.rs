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

/// Compile-time constant covering encoder, sizes, and the color pipeline
/// (§9.8). Orientation or ICC changes MUST bump this.
pub const GENERATOR_VERSION: i64 = 1;

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
fn resize_to_edge(img: &DynamicImage, edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= edge {
        return img.clone();
    }
    img.resize(edge, edge, image::imageops::FilterType::CatmullRom)
}

fn encode_webp(img: &DynamicImage, quality: f32) -> Vec<u8> {
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
    encoder.encode(quality).to_vec()
}

/// Generate and atomically write both artifacts from a display-oriented,
/// sRGB image. Idempotent (overwrites, §10.4).
pub fn write_artifacts(
    cache_dir: &Path,
    hash: &ContentHash,
    display_oriented: &DynamicImage,
) -> io::Result<Vec<GeneratedArtifact>> {
    let mut out = Vec::with_capacity(2);
    // Derive the thumb from the display-size render: one large resize, one
    // small one — and bitwise stability between the two artifacts' geometry.
    let display = resize_to_edge(display_oriented, DISPLAY_EDGE);
    let thumb = resize_to_edge(&display, THUMB_EDGE);
    for (kind, img, quality) in [
        (ArtifactKind::Display, &display, DISPLAY_QUALITY),
        (ArtifactKind::Thumb, &thumb, THUMB_QUALITY),
    ] {
        let encoded = encode_webp(img, quality);
        let dest = artifact_path(cache_dir, hash, kind);
        atomic_write(&dest, &encoded)?;
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
        // Largest embedded preview first; fall back down the ladder.
        let preview = decoder
            .full_image(&source, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.preview_image(&source, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(&source, &params).ok().flatten());
        let Some(image) = preview else {
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
            preview_orientation: None,
        }))
    }
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
    fn never_upscale() {
        let img = DynamicImage::new_rgba8(100, 60);
        let resized = resize_to_edge(&img, 512);
        assert_eq!(resized.dimensions(), (100, 60));
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
