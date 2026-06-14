//! Neutral RAW develop: rawler `RawImage` → display-oriented sRGB image
//! (spec/LIBRARY.md §9.3 / §9.4, PLAN-RAW-DECODE OD-1/OD-2).
//!
//! This is the one new piece the full-raw-decode pass needs: the arithmetic
//! that turns rawler's decoded sensor mosaic into a viewable sRGB image.
//! rawler hands us every camera-specific input (CFA pattern, black/white
//! levels, white-balance coefficients, the camera color matrix, crop);
//! everything here is the pipeline that consumes them, in darktable's
//! load-bearing order (highlight/black correction → white balance → demosaic
//! → camera→sRGB matrix → gamma), then orientation last so geometry matches
//! the embedded substrate EXACTLY (§9.4: a full decode MAY alter tone/color
//! but MUST preserve display-oriented geometry — strokes land where drawn).
//!
//! SCOPE (OD-2, "typical neutral decode, just need real resolution"):
//! PPG demosaic (rawler's Patterned Pixel Grouping; bilinear fallback only for
//! crops too small for PPG's bordered interior — PLAN-PERF P4), RGGB/Bayer
//! family only; clip-to-white highlights; no denoise/sharpen/lens correction
//! (those live in real editors — FEATURES non-features). X-Trans / RGBE / CYGM
//! and monochrome are skipped clean (`UnsupportedCfa`) so the embedded preview
//! always stands.
//!
//! rawler 0.7.2 API hazards this code routes AROUND (verified against the
//! crate source — several sibling methods are `todo!()` panics):
//! - `cropped_cfa()` and `linearize()` PANIC — never called. The CFA pattern
//!   comes from `camera.cfa` + `CFA::shift`; scaling from `apply_scaling()`.
//! - `pixels_u16()` PANICS on float-data DNGs — pixels are read via
//!   `data.as_f32()`, which handles both Integer and Float backings.
//! - `apply_scaling()` itself `todo!()`s on `BlackIsZero` (monochrome), so
//!   that branch is guarded out BEFORE we call it.

use image::{DynamicImage, RgbImage};
use rawler::RawImage;
use rawler::cfa::CFA;
use rawler::imgop::Rect;
use rawler::rawimage::RawPhotometricInterpretation;

use super::preview;

/// Why a develop could not be produced. Anything here marks the pass row
/// `skipped`/`failed` (the embedded preview stands); none of these crash the
/// pool thread (PLAN best-effort discipline).
#[derive(Debug, thiserror::Error)]
pub enum DevelopError {
    /// A CFA we do not (yet) demosaic: X-Trans (Fuji), RGBE, CYGM, or
    /// monochrome. Phase 1 is Bayer/RGGB only (OD-2); the row is SKIPPED, not
    /// failed — the embedded preview is correct, just not 1:1.
    #[error("unsupported CFA: {0}")]
    UnsupportedCfa(String),
    /// The decoded data did not match the photometric interpretation, or some
    /// other structural surprise (degenerate dims, empty buffer). Permanent.
    #[error("develop: {0}")]
    Decode(String),
}

/// Fixed linear-sRGB → XYZ(D65) matrix (sRGB primaries, D65 white point —
/// IEC 61966-2-1). The fixed colour anchor the PLAN calls for; we compose the
/// camera's `xyz_to_cam` with it and normalize so a neutral camera grey lands
/// on neutral sRGB grey. Identical constant to rawler's own
/// `imgop::xyz::SRGB_TO_XYZ_D65`, inlined here so the develop math is
/// self-contained and reviewable.
// Exact literals matching rawler's own SRGB_TO_XYZ_D65 (which carries the same
// allow); f32 cannot represent all 7 digits but the rounding is identical, so
// keeping the literals byte-for-byte equal to rawler's keeps the two color
// paths bit-comparable.
/// Minimum crop dimension (per axis) for which we run rawler's PPG demosaic.
/// PPG's interior interpolation loops assume a 3-pixel border on every side
/// (`.skip(3).take(dim - 6)`, plus a `dim - 3` border bound) and UNDERFLOW-
/// PANIC for crops smaller than that. Real sensor crops are far larger; only
/// the synthetic develop fixtures are this small, and a sub-8px patch has no
/// PPG interior to improve, so below this we use our bilinear path (still
/// correct, just no quality upgrade where there is nothing to upgrade). 8 (not
/// 7) gives a one-pixel margin over PPG's exact `dim >= 7` lower bound.
const PPG_MIN_DIM: usize = 8;

#[allow(clippy::excessive_precision)]
const SRGB_TO_XYZ_D65: [[f32; 3]; 3] = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.0721750],
    [0.0193339, 0.1191920, 0.9503041],
];

/// Develop a rawler `RawImage` to a display-oriented sRGB `DynamicImage`.
///
/// `exif_orientation` is the §9.6 EXIF tag the metadata pass stored — applied
/// LAST so the develop output is oriented identically to the embedded
/// artifact it augments (the strokes-land-where-drawn invariant, §9.4/§9.7).
/// rawler's own `raw.orientation` is forced to `Normal` in 0.7.2 (a known
/// TODO in the crate), so we rely on the EXIF tag exactly as the embedded
/// path does, NOT on rawler's orientation field.
pub fn develop_to_display_oriented(
    mut raw: RawImage,
    exif_orientation: u16,
) -> Result<DynamicImage, DevelopError> {
    // ---- Stage 0: classify the photometric interpretation -----------------
    //
    // This is the load-bearing guard the PLAN calls out: feeding a linear
    // (already-demosaiced) DNG through a Bayer demosaic is the classic
    // corruption, and `apply_scaling()` panics (`todo!()`) on BlackIsZero
    // (monochrome), so we MUST branch before touching the data.
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => {
            // Phase 1 demosaics only the RGB Bayer family (RGGB/BGGR/…). A
            // 4-colour RGBE, a CYGM pattern, or X-Trans (6×6) returns clean —
            // the embedded preview stands, never a crash or a wrong develop.
            if !cfg.cfa.is_rgb() || cfg.cfa.width != 2 || cfg.cfa.height != 2 {
                return Err(DevelopError::UnsupportedCfa(format!(
                    "pattern {} ({}x{})",
                    cfg.cfa.name, cfg.cfa.width, cfg.cfa.height
                )));
            }
            // Keep the PlaneColor next to the CFA: rawler's PPG demosaic takes
            // it (the channel-name map) alongside the pattern. PPG's own body
            // ignores it, but the trait signature requires it, so we carry the
            // real one rather than synthesize a fake.
            Some((cfg.cfa.clone(), cfg.colors.clone()))
        }
        // A linear DNG is ALREADY demosaiced (cpp == 3): WB + matrix + gamma
        // only, NO demosaic. We still run it through the same scaling + colour
        // path so the develop is neutral and geometry-exact.
        RawPhotometricInterpretation::LinearRaw => None,
        // Monochrome / BlackIsZero: no colour develop, and `apply_scaling()`
        // would panic on it. Skip clean.
        RawPhotometricInterpretation::BlackIsZero => {
            return Err(DevelopError::UnsupportedCfa(
                "monochrome (BlackIsZero)".into(),
            ));
        }
    };

    // ---- Stage 1: linearize + black/white levels → float [0,1] ------------
    //
    // `apply_scaling()` subtracts the per-CFA-cell black level and divides by
    // (white - black), clamping negatives, leaving every sample in [0,1]. It
    // operates on the FULL sensor raster in full-sensor CFA phase (its bayer
    // arrays are raster-ordered row0col0/row0col1/row1col0/row1col1), which is
    // exactly why we scale BEFORE cropping — the crop only shifts which cell a
    // pixel reads, it does not change the level correction.
    raw.apply_scaling()
        .map_err(|e| DevelopError::Decode(format!("apply_scaling: {e}")))?;
    let data = raw.data.as_f32();
    let full_w = raw.width;
    let full_h = raw.height;
    if full_w == 0 || full_h == 0 {
        return Err(DevelopError::Decode("zero sensor dimensions".into()));
    }

    // DIAGNOSTIC (once per develop, debug level): the scaled-data range. After
    // apply_scaling this MUST sit in [0,1]; if the founder's log shows a max of
    // ~65535 the scaling no-op'd (wrong black/white levels), and if min==max==0
    // the sensor data itself is black BEFORE any colour math. This disambiguates
    // a black DEVELOP-INPUT from a black colour-matrix collapse. `sample_stats`
    // strides the buffer so it stays O(few thousand reads) on a 50MP raster.
    let scaled = sample_stats(&data);
    tracing::debug!(
        target: "photoproof::raw_develop",
        min = scaled.min,
        max = scaled.max,
        mean = scaled.mean,
        samples = scaled.count,
        "raw develop: scaled sensor data range (expect [0,1])"
    );

    // ---- Stage 2: crop to the recommended/active area ---------------------
    //
    // We crop to the camera's recommended area (`crop_area`, the DNG
    // DefaultCrop) when present, else the `active_area` (non-black sensor),
    // else the full frame. Cropping FIRST makes our develop geometry match the
    // embedded camera JPEG's framing, and — for a CFA sensor — shifts the CFA
    // phase by the crop origin (handled in Stage 3).
    let crop = raw
        .crop_area
        .or(raw.active_area)
        .unwrap_or_else(|| Rect::new(rawler::imgop::Point::zero(), raw.dim()));
    let (crop_x, crop_y) = (crop.p.x, crop.p.y);
    let (out_w, out_h) = (crop.d.w, crop.d.h);
    if out_w == 0 || out_h == 0 {
        return Err(DevelopError::Decode("zero crop dimensions".into()));
    }
    // Defensive: a malformed crop rect (origin + size past the sensor) would
    // index the raster out of bounds. Reject cleanly rather than panic — the
    // embedded preview stands (best-effort discipline).
    if crop_x + out_w > full_w || crop_y + out_h > full_h {
        return Err(DevelopError::Decode(format!(
            "crop {out_w}x{out_h}+{crop_x}+{crop_y} exceeds sensor {full_w}x{full_h}"
        )));
    }
    // Demosaic reads the 4 orthogonal/diagonal neighbours; a 2×2-or-smaller
    // crop has no interior, which the bilinear fallback's edge-clamp tolerates
    // (PPG is only used for crops >= PPG_MIN_DIM, see the demosaic dispatch),
    // but a degenerate 1-px crop is not a meaningful develop. Guard it.
    if cfa.is_some() && (out_w < 2 || out_h < 2) {
        return Err(DevelopError::Decode("crop too small to demosaic".into()));
    }

    // ---- Stage 3 & 4: white balance, demosaic, camera→sRGB matrix ---------
    //
    // The camera→XYZ→sRGB matrix is composed ONCE up front and reused per
    // pixel. cam_to_xyz_normalized() is [[f32;4];3]: 3 XYZ rows × 4 camera
    // input channels (R,G,B,E). For an RGB Bayer sensor there is no E channel,
    // so we fold by taking only the first 3 columns — the E coefficients
    // multiply a channel that does not exist in the demosaiced [R,G,B] pixel.
    // We then compose XYZ→linear-sRGB so the whole camera-RGB → linear-sRGB
    // collapse is a single 3×3 multiply in the inner loop.
    let (cam_rgb_to_srgb, matrix_source) = compose_cam_rgb_to_linear_srgb(&raw);
    // DIAGNOSTIC + GUARD-VISIBILITY (once per develop): name the matrix source.
    // A real founder DNG should log `color_matrix`; a fall-through to `identity`
    // is the warning that BOTH the DNG ColorMatrix tag and the deprecated
    // xyz_to_cam field were degenerate (the develop is still VISIBLE/neutral,
    // not black, but colour is approximate). This is the single most useful
    // line for triaging a black/wrong-colour develop from the log.
    if matrix_source == MatrixSource::Identity {
        tracing::warn!(
            target: "photoproof::raw_develop",
            source = matrix_source.label(),
            "raw develop: colour matrix degenerate; using neutral identity fallback (image stays visible, colour approximate)"
        );
    } else {
        tracing::debug!(
            target: "photoproof::raw_develop",
            source = matrix_source.label(),
            matrix = ?cam_rgb_to_srgb,
            "raw develop: composed camera-RGB to linear-sRGB matrix"
        );
    }

    // White balance, as-shot, RGBE order. NaN-guard mirrors rawler's
    // develop_params(): a camera that did not record WB substitutes neutral
    // [1,1,1,1] rather than producing NaN pixels.
    let wb = if raw.wb_coeffs[0].is_nan() {
        [1.0_f32, 1.0, 1.0, 1.0]
    } else {
        raw.wb_coeffs
    };

    let rgb = match &cfa {
        Some((cfa, colors)) => {
            // CFA-PHASE ALIGNMENT (the fiddly bit). `raw.camera.cfa` describes
            // the pattern at the FULL-sensor origin (0,0). After cropping by
            // (crop_x, crop_y), the cropped pixel (0,0) sits on full-sensor
            // pixel (crop_x, crop_y), so its colour is `cfa.color_at(crop_y,
            // crop_x)`. `CFA::shift(x, y)` returns exactly the pattern whose
            // origin is that shifted cell — per rawler's own doc, "the
            // equivalent pattern of the crop when it's not a multiple of the
            // pattern size". So we shift by (crop_x, crop_y) and then index the
            // shifted pattern with CROPPED coordinates. (color_at takes (row,
            // col) = (y, x); shift takes (x, y) — note the argument order.)
            //
            // DEMOSAIC CHOICE (PLAN-PERF P4). We prefer rawler 0.7.2's built-in
            // PPG (Patterned Pixel Grouping, Chuan-kai Lin) over our hand-rolled
            // bilinear: PPG interpolates green along the minimum-gradient
            // direction (gradient-of-gradients) and then reconstructs R/B from
            // the completed green plane via hue-transit, which removes the
            // zipper/maze artifacts bilinear leaves on edges and fine texture.
            // We keep the REST of our pipeline (WB, the camera->sRGB matrix fix,
            // gamma, EXIF orientation) intact; only the interpolation changes.
            //
            // WHY a size guard, not pure PPG: rawler's PPG interior loops use
            // `.skip(3).take(h - 6)` / `.take(w - 6)` and a `h - 3` border
            // bound, which UNDERFLOW-PANIC (debug) for crops smaller than the
            // 6/3-pixel borders they assume. Real sensor crops are millions of
            // pixels, so this never bites in production; but our synthetic
            // develop tests (and the property test) drive tiny 3x3/4x4 crops.
            // For anything below PPG's safe interior we fall back to our proven
            // bilinear (a 4x4 patch has no PPG interior to improve anyway), so
            // PPG's quality win lands on real images while tiny crops stay
            // correct and panic-free.
            let cropped_cfa = cfa.shift(crop_x, crop_y);
            if out_w >= PPG_MIN_DIM && out_h >= PPG_MIN_DIM {
                demosaic_ppg_rggb(
                    &data,
                    full_w,
                    full_h,
                    crop_x,
                    crop_y,
                    out_w,
                    out_h,
                    cfa,
                    colors,
                    &wb,
                    &cam_rgb_to_srgb,
                )
            } else {
                demosaic_bilinear_rggb(
                    &data,
                    full_w,
                    crop_x,
                    crop_y,
                    out_w,
                    out_h,
                    &cropped_cfa,
                    &wb,
                    &cam_rgb_to_srgb,
                )
            }
        }
        None => {
            // Linear DNG: cpp == 3, already demosaiced, interleaved R,G,B per
            // pixel in the full raster. WB on a linear DNG uses the first three
            // coefficients (the channels ARE R,G,B). No demosaic.
            if raw.cpp != 3 {
                return Err(DevelopError::Decode(format!(
                    "LinearRaw with cpp={} (expected 3)",
                    raw.cpp
                )));
            }
            develop_linear_rgb(
                &data,
                full_w,
                crop_x,
                crop_y,
                out_w,
                out_h,
                &wb,
                &cam_rgb_to_srgb,
            )?
        }
    };

    // DIAGNOSTIC (once per develop): the developed-pixel u8 range. This is the
    // FINAL black-screen tell: if max == 0 here the develop produced an all-
    // black buffer (the founder's bug) even though the artifact then encodes/
    // serves fine — so a black 1:1 with max==0 here points at the develop, while
    // a non-zero range here with a black screen points downstream (encode/serve/
    // display). Cheap byte stats over the whole RGB buffer (already in memory).
    let dev = byte_stats(&rgb);
    tracing::debug!(
        target: "photoproof::raw_develop",
        min = dev.0,
        max = dev.1,
        mean = dev.2,
        out_w,
        out_h,
        matrix_source = matrix_source.label(),
        "raw develop: developed sRGB u8 pixel range (max==0 means black develop)"
    );

    let img = RgbImage::from_raw(out_w as u32, out_h as u32, rgb)
        .ok_or_else(|| DevelopError::Decode("rgb buffer size mismatch".into()))?;

    // ---- Stage 6: orient LAST, identical to the embedded path -------------
    let oriented = preview::apply_exif_orientation(DynamicImage::ImageRgb8(img), exif_orientation);
    Ok(oriented)
}

/// Which source the camera→sRGB matrix was built from, for one-shot logging.
/// The founder's log line names this so a black/wrong develop is diagnosable
/// without a debugger: `XyzToCam` is the rawler-deprecated field, `ColorMatrix`
/// is the DNG-populated map (the real-DNG path), `Identity` is the last-resort
/// neutral fallback that fires only when both sources are degenerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatrixSource {
    /// Built from `raw.xyz_to_cam[0..3]` (rawler-deprecated; populated by the
    /// in-catalog decoders, all-zero for catalog-miss DNGs).
    XyzToCam,
    /// Built from `raw.color_matrix` (DNG `ColorMatrix1/2` tags) — the path a
    /// real founder DNG takes, since the DNG decoder leaves `xyz_to_cam` zero.
    ColorMatrix,
    /// Neutral identity passthrough: both real sources were degenerate (zero,
    /// non-finite, or singular after composition). Keeps the develop NEUTRAL
    /// instead of NaN-black; colour will be approximate but the image is real.
    Identity,
}

impl MatrixSource {
    /// Stable label for the diagnostic log line (no em-dashes; user-facing-ish).
    fn label(self) -> &'static str {
        match self {
            MatrixSource::XyzToCam => "xyz_to_cam (rawler deprecated field)",
            MatrixSource::ColorMatrix => "color_matrix (DNG ColorMatrix tag)",
            MatrixSource::Identity => "identity fallback (both sources degenerate)",
        }
    }
}

/// A 3×3 is usable as a colour matrix only if every entry is finite AND every
/// row carries real signal. An all-zero row makes `normalize` leave it zero,
/// which then makes `pseudo_inverse` divide by a zero pivot and emit NaN/inf
/// for the WHOLE matrix (rawler's `pseudo_inverse` has no pivoting) — the exact
/// path that turned a real DNG develop all-black. Reject early.
fn matrix_3x3_is_usable(m: &[[f32; 3]; 3]) -> bool {
    let all_finite = m.iter().all(|row| row.iter().all(|v| v.is_finite()));
    if !all_finite {
        return false;
    }
    // Every row must have a non-zero sum: `normalize` divides each row by its
    // sum and skips (leaves zero) any row that sums to zero, which feeds a
    // degenerate matrix straight into the inverse. Require real signal per row.
    m.iter()
        .all(|row| row.iter().sum::<f32>().abs() > f32::EPSILON)
}

/// Extract the camera's XYZ→cameraRGB matrix (3 XYZ-input rows × 3 RGB camera
/// channels) from the best available source, preferring the non-deprecated
/// `color_matrix` map and validating each candidate.
///
/// WHY this ordering: rawler's DNG decoder sets `xyz_to_cam: Default::default()`
/// (all zeros) and ONLY populates `color_matrix` from the DNG `ColorMatrix1/2`
/// tags (verified in rawler 0.7.2 `decoders/dng.rs::make_camera`). So for a
/// real founder DNG, `xyz_to_cam` is zero and the previous code composed a
/// zero matrix -> NaN inverse -> every pixel black. The in-catalog Bayer
/// decoders (CR3/ARW/NEF) populate `xyz_to_cam` instead. Trying `color_matrix`
/// first, then `xyz_to_cam`, covers both real-world shapes; the synthetic tests
/// (which set `xyz_to_cam` and leave `color_matrix` empty) still land on the
/// `xyz_to_cam` branch, so their golden values are unchanged.
///
/// `color_matrix` is a flat `Vec<f32>` of `ColorMatrix` values: 9 entries for a
/// 3×3 (RGB) profile, 12 for a 3×4 (RGBE) profile (3 XYZ rows × N camera
/// channels, row-major). We take the first 3 columns of each XYZ row (drop the
/// E channel the demosaiced [R,G,B] pixel does not have), matching the existing
/// `xyz_to_cam[0..3]` fold.
fn xyz_to_cam_rgb_from_source(raw: &RawImage) -> ([[f32; 3]; 3], MatrixSource) {
    // 1) Preferred: the DNG-populated color_matrix map. Pick D65 if present
    //    (the develop's sRGB target white point), else any illuminant — a
    //    single profile is the common DNG case and is better than zero.
    if !raw.color_matrix.is_empty() {
        let flat = raw
            .color_matrix
            .get(&rawler::imgop::xyz::Illuminant::D65)
            .or_else(|| raw.color_matrix.values().next());
        if let Some(flat) = flat {
            // Need at least a 3×3 (9 entries). 12 entries = 3×4 RGBE; we read
            // the first 3 of each 3- or 4-wide row. Anything else is junk.
            let cols = flat.len() / 3;
            if flat.len() % 3 == 0 && (3..=4).contains(&cols) {
                let mut m = [[0.0f32; 3]; 3];
                for (i, row) in m.iter_mut().enumerate() {
                    for (j, cell) in row.iter_mut().enumerate() {
                        *cell = flat[i * cols + j];
                    }
                }
                if matrix_3x3_is_usable(&m) {
                    return (m, MatrixSource::ColorMatrix);
                }
            }
        }
    }

    // 2) Fallback: the rawler-deprecated xyz_to_cam (first 3 RGB rows). This is
    //    what the in-catalog Bayer decoders and the synthetic tests populate.
    let from_xyz: [[f32; 3]; 3] = [raw.xyz_to_cam[0], raw.xyz_to_cam[1], raw.xyz_to_cam[2]];
    if matrix_3x3_is_usable(&from_xyz) {
        return (from_xyz, MatrixSource::XyzToCam);
    }

    // 3) Both sources degenerate: signal the caller to use the identity
    //    passthrough. We return zeros + Identity so the caller logs and swaps
    //    in IDENTITY_MATRIX_3 (never composing the zero matrix into a NaN).
    ([[0.0; 3]; 3], MatrixSource::Identity)
}

/// Compose the per-pixel camera-RGB → linear-sRGB 3×3 matrix once.
///
/// This mirrors rawler's OWN neutral-develop composition (`imgop::raw::
/// map_3ch_to_rgb`), which is the correct realization of the PLAN's "fold the
/// E channel per CFA arity, compose with a fixed XYZ(D65)→sRGB matrix":
///
///   rgb2cam = normalize( xyz_to_cam[R,G,B rows] · SRGB_TO_XYZ_D65 )
///   cam2rgb = pseudo_inverse(rgb2cam)
///
/// E-channel fold: the source matrix is 3 XYZ rows × (3 or 4) camera channels;
/// we use ONLY the first three camera columns — the E column maps a channel the
/// demosaiced [R,G,B] pixel does not have (see `xyz_to_cam_rgb_from_source`).
/// The `normalize` step (each row summed to 1.0) is what guarantees neutral:
/// a flat camera grey maps to a flat sRGB grey. WHY NOT the literal
/// `cam_to_xyz_normalized()` the PLAN body names: that method normalizes so
/// neutral camera maps to XYZ(1,1,1), which is NOT D65 white, so composing it
/// with a real XYZ→sRGB matrix tints a neutral grey (verified: a 0.4 grey came
/// out ~RGB(184,166,162)). rawler's own pipeline avoids this exact trap by
/// composing through `SRGB_TO_XYZ_D65` and normalizing the result — so we
/// follow the working path, not the broken-by-name one. (OD-2: trust the
/// simple neutral path; documented deviation for the careful review.)
///
/// ROBUSTNESS (the real-DNG-black fix): we (a) source the matrix from the best
/// non-degenerate of `color_matrix` / `xyz_to_cam`, and (b) validate the
/// COMPOSED result is finite and non-degenerate before returning it. If any
/// stage yields NaN/inf/singular, we fall back to the identity matrix (a
/// neutral, slightly-off-colour but VISIBLE develop) rather than emitting NaN
/// pixels that clamp to all-black. The chosen source is returned so the caller
/// can log it ONCE per develop (the founder's black-screen debug surface).
fn compose_cam_rgb_to_linear_srgb(raw: &RawImage) -> ([[f32; 3]; 3], MatrixSource) {
    use rawler::imgop::matrix::{IDENTITY_MATRIX_3, multiply, normalize, pseudo_inverse};

    let (xyz_to_cam_rgb, source) = xyz_to_cam_rgb_from_source(raw);
    // Both real sources were degenerate: skip composition entirely (composing
    // the zero matrix is exactly what produced NaN) and use identity.
    if source == MatrixSource::Identity {
        return (IDENTITY_MATRIX_3, MatrixSource::Identity);
    }

    // rgb2cam: linear-sRGB → camera, row-normalized so camera-neutral is
    // sRGB-neutral; cam2rgb is its inverse (camera → linear sRGB).
    let rgb2cam = normalize(multiply(&xyz_to_cam_rgb, &SRGB_TO_XYZ_D65));
    // Guard the COMPOSED matrix before inverting: even a finite, non-zero
    // source can compose to a row that sums to zero (then normalize leaves it
    // zero -> pseudo_inverse divides by zero -> NaN). Catch it here.
    if !matrix_3x3_is_usable(&rgb2cam) {
        return (IDENTITY_MATRIX_3, MatrixSource::Identity);
    }
    let cam2rgb = pseudo_inverse(rgb2cam);
    // Final guard: pseudo_inverse has no pivoting, so a near-singular rgb2cam
    // can still slip a NaN/inf through. If the inverse is not all-finite, fall
    // back to identity rather than ship NaN pixels (the all-black failure).
    if !cam2rgb.iter().all(|row| row.iter().all(|v| v.is_finite())) {
        return (IDENTITY_MATRIX_3, MatrixSource::Identity);
    }
    (cam2rgb, source)
}

/// Min/max/mean of a float buffer, sampled with a stride so the diagnostic is
/// cheap on a 50MP raster (a few thousand reads, not 50M). Used ONCE per
/// develop to log the scaled-data range — not in any per-pixel hot loop.
struct SampleStats {
    min: f32,
    max: f32,
    mean: f32,
    count: usize,
}

/// Stride the buffer to roughly `TARGET` samples and report range/mean. NaN/inf
/// samples are folded in honestly (min/max via partial compares) so a NaN in
/// the scaled data still shows up as a non-finite mean rather than being hidden.
fn sample_stats(data: &[f32]) -> SampleStats {
    const TARGET: usize = 4096;
    if data.is_empty() {
        return SampleStats {
            min: 0.0,
            max: 0.0,
            mean: 0.0,
            count: 0,
        };
    }
    let stride = (data.len() / TARGET).max(1);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for &v in data.iter().step_by(stride) {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
        sum += v as f64;
        count += 1;
    }
    SampleStats {
        min,
        max,
        mean: (sum / count as f64) as f32,
        count,
    }
}

/// Min/max/mean of the developed u8 RGB buffer (whole buffer; it is already in
/// memory and the develop is not in a tight loop here). `max == 0` is the
/// all-black tell the founder's log surfaces.
fn byte_stats(rgb: &[u8]) -> (u8, u8, f32) {
    if rgb.is_empty() {
        return (0, 0, 0.0);
    }
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut sum = 0u64;
    for &b in rgb {
        if b < min {
            min = b;
        }
        if b > max {
            max = b;
        }
        sum += b as u64;
    }
    (min, max, (sum as f64 / rgb.len() as f64) as f32)
}

/// Apply WB (per-channel multiply), the camera→sRGB matrix, and the sRGB gamma
/// to one linear-light [R,G,B], producing 3 sRGB-encoded u8s. The single
/// colour/tone kernel shared by the demosaic and linear-DNG paths.
#[inline]
fn finish_pixel(r: f32, g: f32, b: f32, wb: &[f32; 4], m: &[[f32; 3]; 3]) -> [u8; 3] {
    // White balance as-shot (RGBE order; B uses index 2).
    let r = r * wb[0];
    let g = g * wb[1];
    let b = b * wb[2];
    // Camera RGB → linear sRGB.
    let lr = m[0][0] * r + m[0][1] * g + m[0][2] * b;
    let lg = m[1][0] * r + m[1][1] * g + m[1][2] * b;
    let lb = m[2][0] * r + m[2][1] * g + m[2][2] * b;
    // Clip-to-white highlights (OD-2: clip is honest, not wrong) + sRGB gamma.
    [
        preview::srgb_encode_u8(lr as f64),
        preview::srgb_encode_u8(lg as f64),
        preview::srgb_encode_u8(lb as f64),
    ]
}

/// PPG (Patterned Pixel Grouping) demosaic of an RGGB-family Bayer crop via
/// rawler 0.7.2's built-in `PPGDemosaic`, then OUR WB + matrix + gamma. `data`
/// is the FULL-sensor scaled float raster (`full_w` x `full_h`); we develop the
/// `out_w x out_h` window at origin (`crop_x`, `crop_y`).
///
/// WHY PPG over our bilinear: bilinear averages same-colour neighbours
/// isotropically, which smears across edges and produces the classic
/// zipper/maze artifacts on high-frequency detail. PPG instead (a) fills green
/// along the direction of MINIMUM gradient (a gradient-of-gradients test, so
/// edges are interpolated ALONG them, not across), then (b) reconstructs R/B
/// from the now-complete green plane using hue-transit interpolation, which
/// keeps colour edges aligned with luminance edges. Same family of algorithm
/// real RAW developers ship; markedly fewer artifacts for ~no extra code on our
/// side (rawler owns the kernel). We keep our colour-matrix fix, gamma, and
/// orientation exactly as-is — only the interpolation step changes.
///
/// CFA PHASE: we hand PPG the FULL-sensor pattern (`cfa`, origin (0,0)) and the
/// crop as a `Rect`; PPG does its OWN `cfa.shift(roi.x, roi.y)` internally
/// (verified in rawler's `ppg.rs`), so phase alignment matches our bilinear
/// path's `cfa.shift(crop_x, crop_y)` exactly. Output is camera-RGB linear,
/// out_w x out_h, which we then run through the shared `finish_pixel` kernel.
///
/// The caller guarantees `out_w >= PPG_MIN_DIM && out_h >= PPG_MIN_DIM` so
/// PPG's border-assuming interior loops never underflow.
#[allow(clippy::too_many_arguments)]
fn demosaic_ppg_rggb(
    data: &[f32],
    full_w: usize,
    full_h: usize,
    crop_x: usize,
    crop_y: usize,
    out_w: usize,
    out_h: usize,
    cfa: &CFA,
    colors: &rawler::cfa::PlaneColor,
    wb: &[f32; 4],
    m: &[[f32; 3]; 3],
) -> Vec<u8> {
    use rawler::imgop::sensor::bayer::Demosaic;
    use rawler::imgop::sensor::bayer::ppg::PPGDemosaic;
    use rawler::imgop::{Dim2, Point, Rect};
    use rawler::pixarray::Pix2D;

    // PPG needs an owned full-sensor PixF32 (it reads with full-sensor
    // coordinates inside the ROI). `data` is a borrow of rawler's Cow buffer,
    // so we hand PPG its own copy; the extra full-sensor f32 buffer is fine for
    // an on-demand develop and is freed as soon as PPG returns.
    let pix = Pix2D::new_with(data.to_vec(), full_w, full_h);
    let roi = Rect::new(Point::new(crop_x, crop_y), Dim2::new(out_w, out_h));

    // rawler's PPG: green-first via minimum-gradient, then R/B from green. The
    // result is an out_w x out_h camera-RGB linear image ([R,G,B] per pixel).
    let rgb = PPGDemosaic::new().demosaic(&pix, cfa, colors, roi);
    let pixels = rgb.pixels();

    let mut out = vec![0u8; out_w * out_h * 3];
    for (i, px) in pixels.iter().enumerate() {
        // px is camera-RGB linear; finish_pixel applies WB + camera->sRGB
        // matrix + sRGB gamma (the unchanged tail of our pipeline).
        let dev = finish_pixel(px[0], px[1], px[2], wb, m);
        let o = i * 3;
        out[o..o + 3].copy_from_slice(&dev);
    }
    out
}

/// Bilinear demosaic of an RGGB-family Bayer crop, fused with WB + matrix +
/// gamma. `data` is the FULL-sensor scaled float raster (`full_w` wide); we
/// read a `out_w × out_h` window at origin (`crop_x`, `crop_y`). `cropped_cfa`
/// is the pattern already shifted to the crop origin, indexed with CROPPED
/// coordinates.
///
/// Bilinear reconstruction, per channel, at each output pixel:
///
/// - the channel native to that CFA cell is taken directly;
/// - a missing channel is the average of its same-colour neighbours in the
///   relevant direction (horizontal, vertical, or the 4 diagonals), the
///   standard bilinear Bayer interpolation.
///
/// Edge pixels clamp their sample coordinates into the crop (replicate
/// border) so the 1-pixel frame never reads outside the window.
#[allow(clippy::too_many_arguments)]
fn demosaic_bilinear_rggb(
    data: &[f32],
    full_w: usize,
    crop_x: usize,
    crop_y: usize,
    out_w: usize,
    out_h: usize,
    cropped_cfa: &CFA,
    wb: &[f32; 4],
    m: &[[f32; 3]; 3],
) -> Vec<u8> {
    // Sample the full-sensor raster at cropped coord (x, y), clamped to the
    // crop window (replicate-border so edges never read out of range).
    let sample = |x: isize, y: isize| -> f32 {
        let cx = x.clamp(0, out_w as isize - 1) as usize + crop_x;
        let cy = y.clamp(0, out_h as isize - 1) as usize + crop_y;
        data[cy * full_w + cx]
    };
    // CFA colour at a cropped coordinate. color_at takes (row, col) = (y, x).
    let color_at = |x: isize, y: isize| -> usize {
        let cx = x.clamp(0, out_w as isize - 1) as usize;
        let cy = y.clamp(0, out_h as isize - 1) as usize;
        cropped_cfa.color_at(cy, cx)
    };

    let mut out = vec![0u8; out_w * out_h * 3];
    // CFA channel indices: 0=R, 1=G, 2=B (CFA_COLOR_R/G/B).
    for y in 0..out_h as isize {
        for x in 0..out_w as isize {
            let here = color_at(x, y);
            let center = sample(x, y);
            // The four orthogonal and four diagonal neighbours, with their
            // CFA colours — enough to reconstruct all three channels for any
            // RGGB-family phase.
            let (mut r, mut g, mut b) = (0.0_f32, 0.0_f32, 0.0_f32);
            match here {
                // On a RED or BLUE site: G is the 4-orthogonal-neighbour mean
                // (they are always green on a Bayer grid); the opposite
                // primary is the 4-diagonal mean; this primary is the centre.
                0 | 2 => {
                    let g_mean =
                        (sample(x - 1, y) + sample(x + 1, y) + sample(x, y - 1) + sample(x, y + 1))
                            / 4.0;
                    let diag_mean = (sample(x - 1, y - 1)
                        + sample(x + 1, y - 1)
                        + sample(x - 1, y + 1)
                        + sample(x + 1, y + 1))
                        / 4.0;
                    g = g_mean;
                    if here == 0 {
                        r = center;
                        b = diag_mean;
                    } else {
                        b = center;
                        r = diag_mean;
                    }
                }
                // On a GREEN site: green is the centre. One of R/B lies along
                // the rows, the other along the columns — distinguished by the
                // colour of the horizontal neighbour.
                1 => {
                    g = center;
                    let horiz = (sample(x - 1, y) + sample(x + 1, y)) / 2.0;
                    let vert = (sample(x, y - 1) + sample(x, y + 1)) / 2.0;
                    // The horizontal neighbour's colour decides the mapping.
                    if color_at(x - 1, y) == 0 {
                        r = horiz;
                        b = vert;
                    } else {
                        b = horiz;
                        r = vert;
                    }
                }
                // is_rgb() + 2×2 guard upstream guarantees only 0/1/2 here.
                _ => {}
            }
            let px = finish_pixel(r, g, b, wb, m);
            let o = ((y as usize) * out_w + x as usize) * 3;
            out[o..o + 3].copy_from_slice(&px);
        }
    }
    out
}

/// Develop an already-demosaiced linear-RGB crop (a linear DNG): white
/// balance, the camera matrix, and sRGB gamma, with NO demosaic. `data` is
/// the full raster, interleaved R,G,B per pixel (cpp == 3), `full_w` wide.
#[allow(clippy::too_many_arguments)]
fn develop_linear_rgb(
    data: &[f32],
    full_w: usize,
    crop_x: usize,
    crop_y: usize,
    out_w: usize,
    out_h: usize,
    wb: &[f32; 4],
    m: &[[f32; 3]; 3],
) -> Result<Vec<u8>, DevelopError> {
    let stride = full_w * 3;
    let mut out = vec![0u8; out_w * out_h * 3];
    for y in 0..out_h {
        for x in 0..out_w {
            let src = (crop_y + y) * stride + (crop_x + x) * 3;
            if src + 2 >= data.len() {
                return Err(DevelopError::Decode("linear DNG crop out of range".into()));
            }
            let px = finish_pixel(data[src], data[src + 1], data[src + 2], wb, m);
            let o = (y * out_w + x) * 3;
            out[o..o + 3].copy_from_slice(&px);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use proptest::prelude::*;
    use rawler::cfa::{CFA, PlaneColor};
    use rawler::decoders::Camera;
    use rawler::rawimage::{BlackLevel, CFAConfig, RawImageData, WhiteLevel};

    /// Build a RawImage develop INPUT directly (cropped_cfa() panics, so we
    /// never go through a real file — we hand-build the struct the develop fn
    /// accepts, mirroring preview.rs::largest_chained_jpeg's hand-rolled
    /// fixtures). `xyz_to_cam` identity-ish so the colour matrix is wiring,
    /// not a real camera profile; black 0, white 65535; the 4th E row is the
    /// pseudoinverse's spare and left zero.
    fn raw_input(
        width: usize,
        height: usize,
        data: RawImageData,
        photometric: RawPhotometricInterpretation,
        cpp: usize,
        wb: [f32; 4],
        crop: Option<Rect>,
    ) -> RawImage {
        // Camera colourspace == linear sRGB for the fixtures: set xyz_to_cam
        // (XYZ → camera) to the XYZ(D65)→linear-sRGB matrix (the inverse of
        // the module's SRGB_TO_XYZ_D65). Then compose_cam_rgb_to_linear_srgb
        // collapses to (near) identity, so a develop with wb=[1,1,1,1]
        // round-trips a known linear value back to itself (within sRGB-encode
        // rounding). This isolates the CFA-phase + matrix WIRING from a real
        // camera profile, which is the point of a synthetic test.
        #[allow(clippy::excessive_precision)]
        const XYZ_D65_TO_LINEAR_SRGB: [[f32; 3]; 4] = [
            [3.2404542, -1.5371385, -0.4985314],
            [-0.9692660, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
            [0.0, 0.0, 0.0],
        ];
        let camera = Camera {
            xyz_to_cam: XYZ_D65_TO_LINEAR_SRGB,
            ..Camera::default()
        };
        // Black/white level shape must match the photometric path's expected
        // arity: the CFA scaler reads a 2×2 bayer array (4 cells); the linear
        // scaler reads `correct_blacklevel(black.as_vec(), white.as_vec())`,
        // which panics unless the two vecs are the SAME length — so a linear
        // (cpp=3) fixture uses a single-cell black+white, the CFA fixture the
        // 2×2 bayer cells. Both are all-zero black, full-scale white (no level
        // correction beyond the [0,1] normalize the develop relies on).
        let black = if cpp == 1 {
            BlackLevel::new(&[0u16, 0, 0, 0], 2, 2, 1)
        } else {
            BlackLevel::new(&[0u16], 1, 1, 1)
        };
        let mut raw = RawImage::new_with_data(
            camera,
            data,
            width * cpp,
            height,
            cpp,
            wb,
            photometric,
            Some(black),
            Some(WhiteLevel::new(vec![u16::MAX as u32; 1])),
            false,
        );
        raw.crop_area = crop;
        raw.active_area = crop;
        raw
    }

    fn rggb_config() -> RawPhotometricInterpretation {
        let cfa = CFA::new("RGGB");
        let colors = PlaneColor::new("RGB");
        RawPhotometricInterpretation::Cfa(CFAConfig::new(&cfa, &colors))
    }

    /// A known-colour 4×4 RGGB mosaic with a uniform value per channel: every
    /// R cell = R_VAL, every G cell = G_VAL, every B cell = B_VAL. After a
    /// neutral develop (wb=1, identity cam matrix), the centre pixel must
    /// demosaic back to (R_VAL, G_VAL, B_VAL) in linear light, then sRGB-
    /// encode — verifying CFA phase + matrix wiring to ±1 LSB.
    #[test]
    fn rggb_known_colors_demosaic_to_expected() {
        // RGGB phase: (0,0)=R (1,0)=G (0,1)=G (1,1)=B  [color_at(row,col)]
        let (w, h) = (4usize, 4usize);
        let r_lin = 0.5_f32;
        let g_lin = 0.25_f32;
        let b_lin = 0.75_f32;
        let to_u16 = |v: f32| (v * u16::MAX as f32) as u16;
        let mut pix = vec![0u16; w * h];
        let cfa = CFA::new("RGGB");
        for row in 0..h {
            for col in 0..w {
                pix[row * w + col] = match cfa.color_at(row, col) {
                    0 => to_u16(r_lin),
                    1 => to_u16(g_lin),
                    _ => to_u16(b_lin),
                };
            }
        }
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            rggb_config(),
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 1).expect("develops");
        assert_eq!(img.dimensions(), (4, 4));
        // Center pixel (2,2): uniform field means every reconstructed channel
        // equals its source value. Expected sRGB encode of each linear value.
        let px = img.to_rgb8();
        let (er, eg, eb) = (
            preview::srgb_encode_u8(r_lin as f64),
            preview::srgb_encode_u8(g_lin as f64),
            preview::srgb_encode_u8(b_lin as f64),
        );
        let c = px.get_pixel(2, 2);
        let close = |a: u8, b: u8| (a as i16 - b as i16).abs() <= 1;
        assert!(close(c[0], er), "R {} vs {}", c[0], er);
        assert!(close(c[1], eg), "G {} vs {}", c[1], eg);
        assert!(close(c[2], eb), "B {} vs {}", c[2], eb);
    }

    /// A uniform GRAY patch (all channels equal in linear light) must develop
    /// to a near-neutral gray: |R-G| and |G-B| within a small epsilon. This is
    /// the WB + matrix neutrality sanity check (a non-neutral matrix would
    /// tint a flat gray).
    #[test]
    fn gray_patch_stays_neutral() {
        let (w, h) = (4usize, 4usize);
        let v = 0.4_f32;
        let val = (v * u16::MAX as f32) as u16;
        let pix = vec![val; w * h];
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            rggb_config(),
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 1).expect("develops");
        let c = img.to_rgb8();
        let p = c.get_pixel(2, 2);
        let dr = (p[0] as i16 - p[1] as i16).abs();
        let db = (p[1] as i16 - p[2] as i16).abs();
        // Identity matrix → exactly neutral; allow a couple LSB of rounding.
        assert!(dr <= 2 && db <= 2, "non-neutral gray: {:?}", p);
    }

    /// THE PPG-PATH TEST (PLAN-PERF P4). A crop at/above `PPG_MIN_DIM` routes
    /// through rawler's PPG demosaic instead of bilinear. On a uniform RGGB
    /// mosaic (every R=R_VAL, G=G_VAL, B=B_VAL) PPG's interior must reconstruct
    /// each channel back to its source value (a flat field has no gradients, so
    /// every interpolation direction agrees), then the unchanged matrix+gamma
    /// tail sRGB-encodes it. This proves: (a) the PPG path runs without the
    /// border-underflow panic, (b) CFA phase wiring through rawler is correct,
    /// and (c) our colour tail is unchanged. We read an interior pixel (PPG's
    /// 3px border uses plain bilinear, the interior is the PPG win).
    #[test]
    fn ppg_path_demosaics_uniform_field() {
        // 16x16 is comfortably above PPG_MIN_DIM (8), giving a real interior.
        let (w, h) = (16usize, 16usize);
        let r_lin = 0.5_f32;
        let g_lin = 0.25_f32;
        let b_lin = 0.75_f32;
        let to_u16 = |v: f32| (v * u16::MAX as f32) as u16;
        let cfa = CFA::new("RGGB");
        let mut pix = vec![0u16; w * h];
        for row in 0..h {
            for col in 0..w {
                pix[row * w + col] = match cfa.color_at(row, col) {
                    0 => to_u16(r_lin),
                    1 => to_u16(g_lin),
                    _ => to_u16(b_lin),
                };
            }
        }
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            rggb_config(),
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 1).expect("develops via PPG");
        assert_eq!(img.dimensions(), (16, 16));
        let px = img.to_rgb8();
        let (er, eg, eb) = (
            preview::srgb_encode_u8(r_lin as f64),
            preview::srgb_encode_u8(g_lin as f64),
            preview::srgb_encode_u8(b_lin as f64),
        );
        // An interior pixel (8,8): well inside PPG's 3px border.
        let c = px.get_pixel(8, 8);
        let close = |a: u8, b: u8| (a as i16 - b as i16).abs() <= 1;
        assert!(close(c[0], er), "PPG R {} vs {}", c[0], er);
        assert!(close(c[1], eg), "PPG G {} vs {}", c[1], eg);
        assert!(close(c[2], eb), "PPG B {} vs {}", c[2], eb);
    }

    /// A non-identity orientation (6 = 90° CW) must rotate the output so the
    /// oriented aspect equals the source crop aspect, swapped — geometry
    /// safety (§9.4: strokes land where drawn). A 4×2 source under tag 6 must
    /// come out 2×4.
    #[test]
    fn orientation_swaps_aspect_within_tolerance() {
        let (w, h) = (4usize, 2usize);
        let pix = vec![(0.5 * u16::MAX as f32) as u16; w * h];
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            rggb_config(),
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 6).expect("develops");
        // 90° rotation swaps w/h.
        assert_eq!(img.dimensions(), (2, 4));
        let source_aspect = w as f64 / h as f64; // 2.0
        let oriented_aspect = img.height() as f64 / img.width() as f64; // 4/2 = 2.0
        assert!(
            (source_aspect - oriented_aspect).abs() < preview::EMBEDDED_NATIVE_ASPECT_TOLERANCE,
            "aspect drift: {source_aspect} vs {oriented_aspect}"
        );
    }

    /// A LinearRaw (cpp=3) input is NOT demosaiced and yields correct RGB:
    /// interleaved R,G,B passes straight through WB+matrix+gamma.
    #[test]
    fn linear_dng_is_not_demosaiced() {
        let (w, h) = (3usize, 3usize);
        let (r, g, b) = (0.6_f32, 0.3_f32, 0.9_f32);
        let to_u16 = |v: f32| (v * u16::MAX as f32) as u16;
        let mut pix = Vec::with_capacity(w * h * 3);
        for _ in 0..(w * h) {
            pix.push(to_u16(r));
            pix.push(to_u16(g));
            pix.push(to_u16(b));
        }
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            RawPhotometricInterpretation::LinearRaw,
            3,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 1).expect("develops");
        assert_eq!(img.dimensions(), (3, 3));
        let c = img.to_rgb8();
        let p = c.get_pixel(1, 1);
        let close = |a: u8, e: u8| (a as i16 - e as i16).abs() <= 1;
        assert!(close(p[0], preview::srgb_encode_u8(r as f64)));
        assert!(close(p[1], preview::srgb_encode_u8(g as f64)));
        assert!(close(p[2], preview::srgb_encode_u8(b as f64)));
    }

    /// An X-Trans (6×6) pattern returns UnsupportedCfa, never panics.
    #[test]
    fn xtrans_is_unsupported_not_panic() {
        let (w, h) = (6usize, 6usize);
        let pix = vec![0u16; w * h];
        // The canonical Fuji X-Trans 6×6 string.
        let cfa = CFA::new("GBGGRGRGRBGBGBGGRGGRGGBGBGGRGRGRBGBG");
        let colors = PlaneColor::new("RGB");
        let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new(&cfa, &colors));
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            photometric,
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        match develop_to_display_oriented(raw, 1) {
            Err(DevelopError::UnsupportedCfa(_)) => {}
            other => panic!("expected UnsupportedCfa, got {other:?}"),
        }
    }

    /// An RGBE 4-colour pattern is unsupported (not RGB), returns clean.
    #[test]
    fn rgbe_is_unsupported() {
        let (w, h) = (4usize, 4usize);
        let pix = vec![0u16; w * h];
        let cfa = CFA::new("RGEB");
        let colors = PlaneColor::new("RGBE");
        let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new(&cfa, &colors));
        let raw = raw_input(
            w,
            h,
            RawImageData::Integer(pix),
            photometric,
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        assert!(matches!(
            develop_to_display_oriented(raw, 1),
            Err(DevelopError::UnsupportedCfa(_))
        ));
    }

    /// Float-backed data (some DNGs decode to RawImageData::Float) develops
    /// via data.as_f32() without the pixels_u16() panic.
    #[test]
    fn float_data_develops() {
        let (w, h) = (4usize, 4usize);
        let cfa = CFA::new("RGGB");
        let mut pix = vec![0f32; w * h];
        for row in 0..h {
            for col in 0..w {
                pix[row * w + col] = match cfa.color_at(row, col) {
                    0 => 0.5,
                    1 => 0.25,
                    _ => 0.75,
                };
            }
        }
        let raw = raw_input(
            w,
            h,
            RawImageData::Float(pix),
            rggb_config(),
            1,
            [1.0, 1.0, 1.0, 1.0],
            None,
        );
        let img = develop_to_display_oriented(raw, 1).expect("develops float data");
        assert_eq!(img.dimensions(), (4, 4));
    }

    /// THE REAL-DNG-BLACK REGRESSION GUARD. rawler's DNG decoder leaves
    /// `xyz_to_cam` all-zero and populates only `color_matrix`; the old code
    /// composed the zero matrix into a NaN inverse, so every pixel clamped to
    /// 0 (all-black). Here we build that exact pathology: a non-black RGGB
    /// mosaic with `xyz_to_cam` all-zero AND `color_matrix` empty (the
    /// worst-case catalog-miss). The develop MUST NOT be all-black and MUST NOT
    /// contain NaN-derived zeros — the identity fallback keeps it visible.
    #[test]
    fn degenerate_xyz_to_cam_does_not_develop_black() {
        let (w, h) = (4usize, 4usize);
        let cfa = CFA::new("RGGB");
        // A clearly non-black mosaic (mid-grey-ish per channel).
        let to_u16 = |v: f32| (v * u16::MAX as f32) as u16;
        let mut pix = vec![0u16; w * h];
        for row in 0..h {
            for col in 0..w {
                pix[row * w + col] = match cfa.color_at(row, col) {
                    0 => to_u16(0.5),
                    1 => to_u16(0.5),
                    _ => to_u16(0.5),
                };
            }
        }
        // Camera with ZERO xyz_to_cam and EMPTY color_matrix: the degenerate
        // catalog-miss DNG shape. (Camera::default() already zeroes both, but
        // we spell it out so the intent is unmissable.)
        let camera = Camera {
            xyz_to_cam: [[0.0; 3]; 4],
            color_matrix: std::collections::HashMap::new(),
            ..Camera::default()
        };
        let black = BlackLevel::new(&[0u16], 1, 1, 1);
        let mut raw = RawImage::new_with_data(
            camera,
            RawImageData::Integer(pix),
            w,
            h,
            1,
            [1.0, 1.0, 1.0, 1.0],
            rggb_config(),
            Some(black),
            Some(WhiteLevel::new(vec![u16::MAX as u32; 1])),
            false,
        );
        raw.crop_area = None;
        raw.active_area = None;
        let img = develop_to_display_oriented(raw, 1).expect("develops via fallback, not black");
        let c = img.to_rgb8();
        // Not all-black: at least the centre pixel must be non-zero, and no
        // channel may be NaN-derived (NaN -> 0 via clamp, so "non-zero" is the
        // observable guard). A 0.5 input through identity -> ~188 sRGB.
        let p = c.get_pixel(2, 2);
        assert!(
            p[0] > 0 && p[1] > 0 && p[2] > 0,
            "degenerate matrix produced black/NaN pixel: {:?}",
            p
        );
    }

    /// The non-degenerate guard unit: `matrix_3x3_is_usable` rejects an all-zero
    /// matrix, a single zero row, and any non-finite entry, but accepts a real
    /// matrix. This is the predicate that stands between a real DNG and a black
    /// screen, so it gets its own direct test.
    #[test]
    fn matrix_usable_predicate_rejects_degenerate() {
        // Healthy: rows sum non-zero, all finite.
        assert!(matrix_3x3_is_usable(&[
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0]
        ]));
        // All-zero (the catalog-miss xyz_to_cam): rejected.
        assert!(!matrix_3x3_is_usable(&[[0.0; 3]; 3]));
        // One zero row (would make pseudo_inverse divide by zero): rejected.
        assert!(!matrix_3x3_is_usable(&[
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0]
        ]));
        // Non-finite entry (a NaN already in the source): rejected.
        assert!(!matrix_3x3_is_usable(&[
            [f32::NAN, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0]
        ]));
    }

    /// A real `color_matrix` (DNG ColorMatrix shape) is used as the matrix
    /// source when `xyz_to_cam` is zero, and develops non-black. Uses the
    /// XYZ(D65)->camera identity profile (so the develop round-trips a neutral
    /// grey) installed via `color_matrix`, NOT `xyz_to_cam` — proving the
    /// real-DNG path (color_matrix, xyz_to_cam empty) produces a real image.
    #[test]
    fn color_matrix_source_develops_when_xyz_to_cam_zero() {
        use rawler::imgop::xyz::Illuminant;
        let (w, h) = (4usize, 4usize);
        let cfa = CFA::new("RGGB");
        let to_u16 = |v: f32| (v * u16::MAX as f32) as u16;
        let mut pix = vec![0u16; w * h];
        for row in 0..h {
            for col in 0..w {
                pix[row * w + col] = match cfa.color_at(row, col) {
                    0 => to_u16(0.4),
                    1 => to_u16(0.4),
                    _ => to_u16(0.4),
                };
            }
        }
        // XYZ(D65) -> linear-sRGB as a flat 3x3 ColorMatrix (row-major). This is
        // the inverse of SRGB_TO_XYZ_D65, so compose_cam_rgb_to_linear_srgb
        // collapses to ~identity and a neutral grey stays neutral.
        #[allow(clippy::excessive_precision)]
        let flat: Vec<f32> = vec![
            3.2404542, -1.5371385, -0.4985314, -0.9692660, 1.8760108, 0.0415560, 0.0556434,
            -0.2040259, 1.0572252,
        ];
        let mut color_matrix = std::collections::HashMap::new();
        color_matrix.insert(Illuminant::D65, flat);
        let camera = Camera {
            xyz_to_cam: [[0.0; 3]; 4], // zero: force the color_matrix path
            color_matrix,
            ..Camera::default()
        };
        let black = BlackLevel::new(&[0u16], 1, 1, 1);
        let mut raw = RawImage::new_with_data(
            camera,
            RawImageData::Integer(pix),
            w,
            h,
            1,
            [1.0, 1.0, 1.0, 1.0],
            rggb_config(),
            Some(black),
            Some(WhiteLevel::new(vec![u16::MAX as u32; 1])),
            false,
        );
        raw.crop_area = None;
        raw.active_area = None;
        let img = develop_to_display_oriented(raw, 1).expect("develops via color_matrix");
        let c = img.to_rgb8();
        let p = c.get_pixel(2, 2);
        // Non-black and near-neutral (identity-equivalent profile).
        assert!(
            p[0] > 0 && p[1] > 0 && p[2] > 0,
            "color_matrix path black: {:?}",
            p
        );
        let dr = (p[0] as i16 - p[1] as i16).abs();
        let db = (p[1] as i16 - p[2] as i16).abs();
        assert!(
            dr <= 3 && db <= 3,
            "color_matrix neutral grey tinted: {:?}",
            p
        );
    }

    // NOTE (founder-machine, #[ignore]): a real-RAW timing + visual check —
    // develop the founder's failing DNG plus a Bayer set (CR3/ARW/NEF),
    // assert seconds-not-minutes and eyeball neutrality. Mirrors the embedding
    // e2e #[ignore] stub: it needs real fixtures that do not live in CI.
    #[test]
    #[ignore = "founder-machine: needs real RAW fixtures (DNG + Bayer set)"]
    fn real_raw_develops_neutral_and_fast() {
        // Drive a real file through:
        //   let source = rawler::rawsource::RawSource::new(path).unwrap();
        //   let decoder = rawler::get_decoder(&source).unwrap();
        //   let raw = decoder
        //       .raw_image(&source, &RawDecodeParams::default(), false)
        //       .unwrap();
        //   let img = develop_to_display_oriented(raw, exif_orientation).unwrap();
        // then time it and eyeball / checksum-pin one fixture.
    }

    // -----------------------------------------------------------------------
    // Property tests (proptest): the black-fix invariant under generated
    // matrices and sensor data.
    //
    // WHY: the example tests pin specific degenerate shapes (all-zero, one
    // zero row, a NaN). These sweep ARBITRARY 3x3 matrices (zeros, NaN, Inf,
    // extreme magnitudes) plus random sensor pixels and assert the two
    // load-bearing invariants: (a) `matrix_3x3_is_usable` is a CONSISTENT
    // predicate (its own definition holds), and (b) the full develop path
    // NEVER panics and NEVER ships a NaN/Inf-derived matrix; it falls back to
    // the neutral identity path (the strokes-stay-visible black-fix).
    //
    // The develop path is the expensive one (allocates + per-pixel loops), so
    // it runs FEWER cases (64) on small rasters; the cheap predicate runs the
    // default 256. Fixed ProptestConfig (no persistence file) keeps CI stable.
    // -----------------------------------------------------------------------

    /// A strategy over f32 cells that deliberately includes the pathological
    /// values the black-fix exists for: zeros, NaN, +/-Inf, tiny and huge
    /// magnitudes, alongside ordinary finite values.
    fn pathological_f32() -> impl Strategy<Value = f32> {
        prop_oneof![
            Just(0.0f32),
            Just(f32::NAN),
            Just(f32::INFINITY),
            Just(f32::NEG_INFINITY),
            Just(f32::EPSILON),
            Just(f32::MIN),
            Just(f32::MAX),
            -10.0f32..10.0f32,
            -1.0e30f32..1.0e30f32,
        ]
    }

    /// A 3x3 matrix of pathological-or-ordinary cells.
    fn pathological_matrix() -> impl Strategy<Value = [[f32; 3]; 3]> {
        prop::array::uniform3(prop::array::uniform3(pathological_f32()))
    }

    /// Build an RGGB develop input whose camera `xyz_to_cam` is an arbitrary
    /// 3x3 (4th E-row zero), with a uniform mosaic. This drives the matrix-
    /// sourcing + compose + develop path with a generated colour matrix.
    fn raw_with_xyz_to_cam(m: [[f32; 3]; 3], px_val: u16) -> RawImage {
        let camera = Camera {
            xyz_to_cam: [m[0], m[1], m[2], [0.0; 3]],
            ..Camera::default()
        };
        let (w, h) = (4usize, 4usize);
        let pix = vec![px_val; w * h];
        let black = BlackLevel::new(&[0u16], 1, 1, 1);
        let mut raw = RawImage::new_with_data(
            camera,
            RawImageData::Integer(pix),
            w,
            h,
            1,
            [1.0, 1.0, 1.0, 1.0],
            rggb_config(),
            Some(black),
            Some(WhiteLevel::new(vec![u16::MAX as u32; 1])),
            false,
        );
        raw.crop_area = None;
        raw.active_area = None;
        raw
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        /// INVARIANT: `matrix_3x3_is_usable` is exactly "all entries finite AND
        /// every row sums to a non-zero magnitude". The predicate must agree
        /// with its own definition for ANY generated matrix; this is the gate
        /// that stands between a degenerate source and a NaN inverse.
        #[test]
        fn matrix_usable_predicate_is_consistent(m in pathological_matrix()) {
            let all_finite = m.iter().all(|row| row.iter().all(|v| v.is_finite()));
            let rows_have_signal = m
                .iter()
                .all(|row| row.iter().sum::<f32>().abs() > f32::EPSILON);
            let expected = all_finite && rows_have_signal;
            prop_assert_eq!(matrix_3x3_is_usable(&m), expected);
        }

        /// INVARIANT: `compose_cam_rgb_to_linear_srgb` NEVER returns a matrix
        /// with a non-finite entry, for ANY generated `xyz_to_cam`. A
        /// degenerate/singular source must fall back to the finite identity
        /// matrix rather than emit a NaN/Inf matrix (which would clamp pixels
        /// to all-black). When it does fall back, the source is `Identity`.
        #[test]
        fn composed_matrix_is_always_finite(m in pathological_matrix()) {
            let raw = raw_with_xyz_to_cam(m, 32768);
            let (composed, source) = compose_cam_rgb_to_linear_srgb(&raw);
            let finite = composed.iter().all(|row| row.iter().all(|v| v.is_finite()));
            prop_assert!(finite, "composed matrix has a non-finite entry: {composed:?}");
            // If the source matrix was not usable, the result MUST be the
            // identity fallback (never a composed NaN).
            if source == MatrixSource::Identity {
                use rawler::imgop::matrix::IDENTITY_MATRIX_3;
                prop_assert_eq!(composed, IDENTITY_MATRIX_3);
            }
        }
    }

    proptest! {
        // The develop path allocates + loops per pixel, so fewer cases.
        #![proptest_config(ProptestConfig {
            cases: 64,
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        /// THE BLACK-FIX INVARIANT under generated inputs: for ANY 3x3 colour
        /// matrix (zeros, NaN, Inf, extreme) and ANY uniform sensor value, the
        /// full develop NEVER panics and ALWAYS produces a real image of the
        /// expected size (u8 pixels are finite by construction; the real guard
        /// is "no panic and a correctly sized buffer came back"). A degenerate
        /// matrix routes through the identity fallback, not a NaN-black crash.
        #[test]
        fn develop_never_panics_and_pixels_are_finite(
            m in pathological_matrix(),
            px_val in any::<u16>(),
            orientation in 1u16..=8,
        ) {
            let raw = raw_with_xyz_to_cam(m, px_val);
            // Must not panic; a develop either succeeds with a sized image or
            // returns a clean DevelopError (never a crash on the pool thread).
            match develop_to_display_oriented(raw, orientation) {
                Ok(img) => {
                    let rgb = img.to_rgb8();
                    prop_assert_eq!(rgb.len(), (img.width() * img.height() * 3) as usize);
                    prop_assert!(img.width() >= 2 && img.height() >= 2);
                }
                Err(_) => {
                    // A clean error is acceptable; the contract is "no panic",
                    // which reaching here proves.
                }
            }
        }
    }
}
