//! CLIP image preprocessing: full-resolution sRGB pixels -> the 378x378
//! square the DFN5B visual tower expects (spec/RETRIEVAL.md §3, B69).
//!
//! WHY this lives in core (not the connector): the connector boundary is
//! deliberately pixel-format dumb (photoproof_connectors depends on no
//! photoproof crate). Image GEOMETRY — decode, resize, crop — is the
//! library's job, exactly where the rest of the decode pipeline lives
//! (`preview.rs`, the embed path in `embedding.rs`). The connector then
//! only turns these 378x378 sRGB bytes into the normalized CHW f32 tensor
//! its specific ONNX export wants. Splitting it here keeps the geometry
//! constants in one place and lets the deterministic tests below pin the
//! crop math without a model.
//!
//! The transform MIRRORS the spike's clip_bench.py preprocessing exactly
//! (docs/SPIKE-P7-EMBED.md): resize the SHORTEST side to 378 px preserving
//! aspect ratio, then center-crop a 378x378 square. The OpenCLIP eval
//! transform the DFN5B export was trained/validated against does precisely
//! this; the preprocess_cfg's "squash" resize_mode is the training-time
//! augmentation, not the eval transform the spike used and the spike is
//! our verified ground truth.

use image::RgbImage;
use image::imageops::FilterType;
use photoproof_connectors::embedder::DecodedImage;

/// The DFN5B visual tower's fixed input edge (config.json image_size: 378).
/// WHY a constant: the connector's tensor shape and this crop size must
/// agree, and a drift would surface only as a runtime ort shape error.
pub const CLIP_IMAGE_EDGE: u32 = 378;

/// Resize-shortest-side + center-crop a decoded image to `CLIP_IMAGE_EDGE`
/// square sRGB pixels, ready for `Embedder::embed_image`.
///
/// The bicubic family is matched as closely as the `image` crate allows:
/// PIL's BICUBIC (spike) is a Catmull-Rom cubic, so `CatmullRom` here.
/// Exact-pixel parity with PIL is not required — the spike's quality bar
/// is sanity-grade (paraphrase/zero-shot margins), and the geometry is
/// what carries the signal.
#[must_use]
pub fn preprocess_clip_image(img: &DecodedImage) -> DecodedImage {
    let edge = CLIP_IMAGE_EDGE;
    // Reconstruct an image view over the borrowed pixels. The source is
    // already display-oriented sRGB RGB8 (LIBRARY §9.7 owns orientation).
    let Some(src) = RgbImage::from_raw(img.width, img.height, img.rgb8.clone()) else {
        // Malformed buffer (length != w*h*3) should never reach here —
        // decode produced it. Fail soft to a black square so the embed
        // path treats it as a (useless but non-crashing) vector rather
        // than panicking on the unwrap.
        return black_square(edge);
    };

    // Resize so the SHORTEST side becomes `edge`, preserving aspect (the
    // longer side overshoots and gets cropped away below).
    let (w, h) = (img.width.max(1), img.height.max(1));
    let scale = f64::from(edge) / f64::from(w.min(h));
    let rw = (f64::from(w) * scale).round().max(f64::from(edge)) as u32;
    let rh = (f64::from(h) * scale).round().max(f64::from(edge)) as u32;
    let resized = image::imageops::resize(&src, rw, rh, FilterType::CatmullRom);

    // Center-crop the `edge` x `edge` square.
    let left = (rw - edge) / 2;
    let top = (rh - edge) / 2;
    let cropped = image::imageops::crop_imm(&resized, left, top, edge, edge).to_image();

    DecodedImage {
        width: edge,
        height: edge,
        rgb8: cropped.into_raw(),
    }
}

fn black_square(edge: u32) -> DecodedImage {
    DecodedImage {
        width: edge,
        height: edge,
        rgb8: vec![0u8; (edge * edge * 3) as usize],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-color rectangle of arbitrary aspect must come out exactly
    /// 378x378 and (since the color is uniform) byte-identical to the
    /// input color everywhere — proving the resize+crop preserves pixels
    /// it should and produces the contract shape.
    #[test]
    fn output_is_always_the_clip_square() {
        for (w, h) in [(1000u32, 600u32), (600, 1000), (378, 378), (4000, 30)] {
            let img = DecodedImage {
                width: w,
                height: h,
                rgb8: solid(w, h, [120, 200, 40]),
            };
            let out = preprocess_clip_image(&img);
            assert_eq!(out.width, CLIP_IMAGE_EDGE);
            assert_eq!(out.height, CLIP_IMAGE_EDGE);
            assert_eq!(
                out.rgb8.len(),
                (CLIP_IMAGE_EDGE * CLIP_IMAGE_EDGE * 3) as usize
            );
            // Center pixel of a uniform image is unchanged by any
            // resampling — a cheap guard that we did not corrupt color.
            let mid = ((CLIP_IMAGE_EDGE / 2 * CLIP_IMAGE_EDGE + CLIP_IMAGE_EDGE / 2) * 3) as usize;
            assert_eq!(&out.rgb8[mid..mid + 3], &[120, 200, 40]);
        }
    }

    /// Center crop keeps the MIDDLE: a wide image with a distinct center
    /// stripe must surface that stripe's color at the output center, not
    /// the edges (the regression guard for a top-left crop bug).
    #[test]
    fn center_crop_keeps_the_center() {
        // 756 wide x 378 tall: after resize-shortest-side the height is
        // already 378, width stays 756; crop takes the middle 378 columns.
        let w = 756u32;
        let h = 378u32;
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        // Paint the middle third red, the outer thirds blue.
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 3) as usize;
                let middle = (w / 3..2 * w / 3).contains(&x);
                rgb[i..i + 3].copy_from_slice(if middle { &[255, 0, 0] } else { &[0, 0, 255] });
            }
        }
        let out = preprocess_clip_image(&DecodedImage {
            width: w,
            height: h,
            rgb8: rgb,
        });
        let mid = ((CLIP_IMAGE_EDGE / 2 * CLIP_IMAGE_EDGE + CLIP_IMAGE_EDGE / 2) * 3) as usize;
        assert_eq!(out.rgb8[mid], 255, "center should be the red stripe");
        assert_eq!(out.rgb8[mid + 2], 0);
    }

    /// A malformed buffer never panics — it yields the black fallback at
    /// the contract shape (the embed path stays alive on a torn artifact).
    #[test]
    fn malformed_buffer_is_soft_black_square() {
        let out = preprocess_clip_image(&DecodedImage {
            width: 100,
            height: 100,
            rgb8: vec![1, 2, 3], // far too short for 100x100x3
        });
        assert_eq!(out.width, CLIP_IMAGE_EDGE);
        assert!(out.rgb8.iter().all(|&b| b == 0));
    }

    fn solid(w: u32, h: u32, c: [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&c);
        }
        v
    }
}
