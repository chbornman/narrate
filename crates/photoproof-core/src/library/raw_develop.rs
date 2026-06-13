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
//! bilinear demosaic, RGGB/Bayer family only; clip-to-white highlights; no
//! denoise/sharpen/lens correction (those live in real editors — FEATURES
//! non-features). X-Trans / RGBE / CYGM and monochrome are skipped clean
//! (`UnsupportedCfa`) so the embedded preview always stands.
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
            Some(cfg.cfa.clone())
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
    // crop has no interior, which the bilinear kernel's edge-clamp tolerates,
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
    let cam_rgb_to_srgb = compose_cam_rgb_to_linear_srgb(&raw);

    // White balance, as-shot, RGBE order. NaN-guard mirrors rawler's
    // develop_params(): a camera that did not record WB substitutes neutral
    // [1,1,1,1] rather than producing NaN pixels.
    let wb = if raw.wb_coeffs[0].is_nan() {
        [1.0_f32, 1.0, 1.0, 1.0]
    } else {
        raw.wb_coeffs
    };

    let rgb = match &cfa {
        Some(cfa) => {
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
            let cropped_cfa = cfa.shift(crop_x, crop_y);
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

    let img = RgbImage::from_raw(out_w as u32, out_h as u32, rgb)
        .ok_or_else(|| DevelopError::Decode("rgb buffer size mismatch".into()))?;

    // ---- Stage 6: orient LAST, identical to the embedded path -------------
    let oriented = preview::apply_exif_orientation(DynamicImage::ImageRgb8(img), exif_orientation);
    Ok(oriented)
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
/// E-channel fold: `xyz_to_cam` is [[f32;3];4] (4 camera channels R,G,B,E × 3
/// XYZ). For an RGB Bayer sensor (CFA arity 3) we use ONLY the first three
/// rows — the E row maps a channel the demosaiced [R,G,B] pixel does not have.
/// The `normalize` step (each row summed to 1.0) is what guarantees neutral:
/// a flat camera grey maps to a flat sRGB grey. WHY NOT the literal
/// `cam_to_xyz_normalized()` the PLAN body names: that method normalizes so
/// neutral camera maps to XYZ(1,1,1), which is NOT D65 white, so composing it
/// with a real XYZ→sRGB matrix tints a neutral grey (verified: a 0.4 grey came
/// out ~RGB(184,166,162)). rawler's own pipeline avoids this exact trap by
/// composing through `SRGB_TO_XYZ_D65` and normalizing the result — so we
/// follow the working path, not the broken-by-name one. (OD-2: trust the
/// simple neutral path; documented deviation for the careful review.)
fn compose_cam_rgb_to_linear_srgb(raw: &RawImage) -> [[f32; 3]; 3] {
    use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
    // First three rows of xyz_to_cam: the R,G,B camera channels (drop E).
    let xyz_to_cam_rgb: [[f32; 3]; 3] = [raw.xyz_to_cam[0], raw.xyz_to_cam[1], raw.xyz_to_cam[2]];
    // rgb2cam: linear-sRGB → camera, row-normalized so camera-neutral is
    // sRGB-neutral; cam2rgb is its inverse (camera → linear sRGB).
    let rgb2cam = normalize(multiply(&xyz_to_cam_rgb, &SRGB_TO_XYZ_D65));
    pseudo_inverse(rgb2cam)
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
}
