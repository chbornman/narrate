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
//! The transform is the OpenCLIP eval transform the DFN5B export was
//! trained/validated against, and the one the spike's clip_bench.py used
//! (docs/SPIKE-P7-EMBED.md): resize-shortest-side-to-378 then center-crop a
//! 378x378 square. (The preprocess_cfg's "squash" resize_mode is the
//! training-time augmentation, not the eval transform; the spike is our
//! verified ground truth.) We implement it crop-FIRST-then-resize — which is
//! geometrically the same window, the centered min(w,h) square — to bound the
//! intermediate buffer; see `preprocess_clip_image` for WHY (extreme aspect
//! ratios make the resize-first order allocate near a gigabyte).

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

    // Resize-shortest-side-to-`edge` then center-crop `edge`x`edge` keeps
    // exactly the centered `min(w,h)` x `min(w,h)` square of the SOURCE. We
    // do that crop FIRST and resize only the square, instead of resizing the
    // whole frame and cropping after. Equivalent up to filter edge effects,
    // but it bounds the intermediate buffer at the square's size rather than
    // letting it explode on extreme aspect ratios: a legal 65500x32 strip
    // would otherwise upscale BOTH axes by 378/32 ~= 11.8 to a ~774000x378x3
    // (~880 MB) transient that resize then walks pixel-by-pixel before the
    // crop throws all but 378 columns away. A transient OOM aborts the
    // process, which would breach the RUNTIME 3.3 never-crash posture from
    // inside the background embedding pump.
    let (w, h) = (img.width.max(1), img.height.max(1));
    let square = w.min(h);
    let left = (w - square) / 2;
    let top = (h - square) / 2;
    let centered = image::imageops::crop_imm(&src, left, top, square, square).to_image();
    let cropped = image::imageops::resize(&centered, edge, edge, FilterType::CatmullRom);

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

    /// An extreme-aspect strip must still produce the 378x378 square WITHOUT
    /// the resize-first ~880 MB intermediate (regression guard for the
    /// crop-first ordering). 9000x24 is a stand-in for a panorama/contact
    /// strip; resize-first would scale to ~141750x378 here.
    #[test]
    fn extreme_aspect_strip_stays_bounded() {
        let (w, h) = (9000u32, 24u32);
        let out = preprocess_clip_image(&DecodedImage {
            width: w,
            height: h,
            rgb8: solid(w, h, [10, 20, 30]),
        });
        assert_eq!(out.width, CLIP_IMAGE_EDGE);
        assert_eq!(out.height, CLIP_IMAGE_EDGE);
        // Uniform color survives the centered crop + resize unchanged.
        let mid = ((CLIP_IMAGE_EDGE / 2 * CLIP_IMAGE_EDGE + CLIP_IMAGE_EDGE / 2) * 3) as usize;
        assert_eq!(&out.rgb8[mid..mid + 3], &[10, 20, 30]);
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
