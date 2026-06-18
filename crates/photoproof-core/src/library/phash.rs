//! Tier-1 near-duplicate detection: a **dHash (gradient)** perceptual hash plus
//! Hamming-distance grouping over a scope.
//!
//! Contract / design: `docs/DESIGN-DEDUP-AND-SIMILARITY.md` §"Tier 1 —
//! perceptual hash". This is the precise "is this the SAME photo" cull that sits
//! between Tier 0 (exact BLAKE3, already collapsed by K13) and Tier 2 (CLIP
//! cosine "looks alike", already built).
//!
//! ## WHY dHash (gradient) and not aHash or pHash
//!
//! The design doc says dHash *or* pHash, NOT aHash. We pick **dHash** because:
//!   * It is robust to upscaling and low-quality JPEG re-encode — it downscales
//!     to a tiny grid first, so JPEG block noise and resampling wash out (DFRWS
//!     2023, cited in the design doc), which is EXACTLY the "re-save / resize /
//!     light edit" transform Tier 1 targets.
//!   * It keys off the *gradient* (the sign of the brightness change between
//!     adjacent cells), so a global exposure/brightness shift — which moves
//!     every cell the same way — leaves the gradient untouched. aHash compares
//!     each cell to the whole-image mean and so flips bits under exactly that
//!     shift; that is the documented reason to avoid it.
//!   * pHash (DCT) is an acceptable alternative but needs a DCT pass and a
//!     median; dHash is a handful of comparisons over a 9×8 grayscale grid —
//!     trivially correct, no float DCT, no new crate. We implement it directly
//!     rather than pull `image_hasher`/`img_hash`: the algorithm is ~20 lines,
//!     and adding a crate that itself re-depends on `image` risks a version
//!     skew against the exact `image` 0.25.x we pin for decode. Documented
//!     deviation from the doc's "use the crate" suggestion — flagged for review.
//!
//! ## The hash
//!
//! 64 bits. We reduce the (already display-oriented, sRGB) preview to a 9×8
//! luma grid and emit one bit per row for each of the 8 adjacent-column pairs:
//! bit = (left cell brighter than right). Identical bytes always yield an
//! identical hash (determinism is a tested invariant). Compared by **Hamming
//! distance** = popcount of the XOR (`u64::count_ones`).

use std::collections::HashMap;

use image::{DynamicImage, imageops::FilterType};

/// dHash works on a (W+1)×H luma grid; one bit per horizontal adjacency. With
/// 8×8 = 64 output bits the source grid is 9×8. These are the canonical dHash
/// dimensions — NOT a tunable: changing them changes the hash space and would
/// invalidate every stored hash, so they are fixed structural constants.
const DHASH_W: u32 = 8; // output bits per row (adjacent-column comparisons)
const DHASH_H: u32 = 8; // rows; DHASH_W * DHASH_H = 64 = the bit width

/// Compute the 64-bit dHash of an already-decoded preview image.
///
/// The caller passes the SAME `DynamicImage` it already decoded for the preview
/// pass (display-oriented, sRGB), so this is near-free: a downscale to 72 px²
/// of work plus 64 comparisons. Orientation matters — a 90°-rotated re-save is
/// deliberately NOT a near-dup match here (mirroring/rotation is Tier 2's job,
/// per the design doc), and hashing the display-oriented pixels makes that
/// boundary crisp and consistent with how the image is shown.
pub fn dhash(img: &DynamicImage) -> u64 {
    // Grayscale first (luma8), then resize to (W+1)×H. We resize the grayscale
    // (1 channel) rather than the full image: cheaper, and the gradient is a
    // luma property anyway. Triangle filter: a light low-pass that smooths JPEG
    // ringing without the cost of Lanczos — exactly the robustness dHash wants.
    let small =
        image::imageops::resize(&img.to_luma8(), DHASH_W + 1, DHASH_H, FilterType::Triangle);

    let mut bits: u64 = 0;
    let mut bit_index = 0u32;
    for y in 0..DHASH_H {
        for x in 0..DHASH_W {
            // get_pixel is in-bounds for every (x, x+1) here by construction
            // (x < DHASH_W, so x+1 <= DHASH_W < width). Luma8 = single channel.
            let left = small.get_pixel(x, y).0[0];
            let right = small.get_pixel(x + 1, y).0[0];
            // Bit set when the left cell is strictly brighter: the SIGN of the
            // horizontal gradient. Ties (left == right) are 0 deterministically.
            if left > right {
                bits |= 1u64 << bit_index;
            }
            bit_index += 1;
        }
    }
    bits
}

/// Hamming distance between two dHashes: the number of differing bits, i.e. the
/// popcount of the XOR. 0 = identical hash; 64 = every bit flipped. This is the
/// metric the near-dup threshold compares against.
#[inline]
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// One near-duplicate group: the set of image hashes whose perceptual hashes
/// are transitively within the Hamming threshold of one another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// Member image content-hashes (BLAKE3 hex), sorted ascending for a stable,
    /// deterministic order that does not depend on input order or union-find
    /// internals.
    pub image_hashes: Vec<String>,
}

/// Group images into near-duplicate clusters by **union-find over all pairs**
/// whose Hamming distance ≤ `threshold`.
///
/// Input: `(image_hash, perceptual_hash)` for every image in the scope that
/// has a hash. A **linear O(n²) scan** over the scope is deliberate for v1: the
/// design doc notes our scale is tens of thousands and that "a BK-tree or even
/// linear scan is adequate" — the BK-tree / multi-index-hashing optimizations
/// are documented as optional, not required, so we take the simple, obviously
/// correct path.
///
/// Output: only groups of size ≥ 2 (a lone image is not a "duplicate"), each
/// with members sorted, and the groups themselves sorted by their first member
/// for a fully deterministic result.
pub fn group_near_duplicates(items: &[(String, u64)], threshold: u32) -> Vec<DuplicateGroup> {
    let n = items.len();
    // Union-find (disjoint-set) with path-compression + union-by-size. Indices
    // are positions in `items`. This is what makes the grouping TRANSITIVE: if
    // A~B and B~C but A and C are just over threshold, all three still land in
    // one group because the union of edges connects them.
    let mut parent: Vec<usize> = (0..n).collect();
    let mut size: Vec<usize> = vec![1; n];

    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]]; // path-halving compression
            i = parent[i];
        }
        i
    }

    // All pairs within threshold get unioned. i<j avoids self-pairs and double
    // work; the dist==0 case (exact same perceptual hash) is just threshold≥0.
    for i in 0..n {
        for j in (i + 1)..n {
            if hamming(items[i].1, items[j].1) <= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    // Union by size: attach the smaller tree under the larger.
                    let (small, big) = if size[ri] < size[rj] {
                        (ri, rj)
                    } else {
                        (rj, ri)
                    };
                    parent[small] = big;
                    size[big] += size[small];
                }
            }
        }
    }

    // Collect members by their set root.
    let mut by_root: HashMap<usize, Vec<String>> = HashMap::new();
    for (idx, (image_hash, _)) in items.iter().enumerate() {
        let root = find(&mut parent, idx);
        by_root.entry(root).or_default().push(image_hash.clone());
    }

    let mut groups: Vec<DuplicateGroup> = by_root
        .into_values()
        // A "duplicate group" needs ≥ 2 members; singletons are not duplicates.
        .filter(|members| members.len() >= 2)
        .map(|mut members| {
            members.sort();
            DuplicateGroup {
                image_hashes: members,
            }
        })
        .collect();
    // Deterministic group order: by first (smallest) member hash.
    groups.sort_by(|a, b| a.image_hashes[0].cmp(&b.image_hashes[0]));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};

    /// A simple deterministic gradient image, parameterized so we can make
    /// near-identical and visually-distinct variants without external fixtures.
    fn gradient(w: u32, h: u32, shift: i32) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // A diagonal gradient; `shift` nudges the whole field so we can
                // build a "re-encoded / slightly different" variant.
                let v = (((x as i32 + y as i32) * 3 + shift).rem_euclid(256)) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn hash_is_deterministic_same_bytes_same_hash() {
        // Determinism invariant: identical pixels MUST yield an identical hash.
        let a = gradient(64, 64, 0);
        let b = gradient(64, 64, 0);
        assert_eq!(dhash(&a), dhash(&b));
        // And stable across repeated calls on the same image.
        assert_eq!(dhash(&a), dhash(&a));
    }

    #[test]
    fn near_identical_pair_is_within_a_small_radius() {
        // A tiny global brightness nudge (the "re-saved / lightly edited"
        // case): dHash keys off the gradient SIGN, so a small uniform shift
        // leaves almost every bit untouched. Hamming should be well under the
        // default near-dup threshold.
        let original = gradient(128, 128, 0);
        let reencoded = gradient(128, 128, 2);
        let d = hamming(dhash(&original), dhash(&reencoded));
        assert!(
            d <= 6,
            "near-identical pair Hamming {d} should be small (<=6)"
        );
    }

    #[test]
    fn visually_distinct_images_do_not_group() {
        // A smooth gradient vs a high-frequency checkerboard: structurally
        // unrelated, so the gradient signs disagree widely. They must NOT land
        // in the same group at the default threshold.
        let smooth = dhash(&gradient(128, 128, 0));
        let mut checker = RgbImage::new(128, 128);
        for y in 0..128 {
            for x in 0..128 {
                let on = ((x / 8) + (y / 8)) % 2 == 0;
                let v = if on { 255 } else { 0 };
                checker.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let checker = dhash(&DynamicImage::ImageRgb8(checker));
        let d = hamming(smooth, checker);
        assert!(d > 10, "distinct images Hamming {d} should be large (>10)");

        // And confirm the grouper keeps them apart at a sane threshold.
        let groups = group_near_duplicates(
            &[("aaa".into(), smooth), ("bbb".into(), checker)],
            8, // the default near-dup threshold
        );
        assert!(
            groups.is_empty(),
            "distinct images must not form a duplicate group"
        );
    }

    #[test]
    fn union_find_groups_transitively() {
        // A~B (dist 2), B~C (dist 2), but A~C is 4 — all under threshold 4 here,
        // but the point of the chain is transitivity even when the ENDS are
        // farther apart than the threshold. Build hashes so A-C distance > 4
        // while A-B and B-C are each <= 4.
        let a: u64 = 0b0000_0000;
        let b: u64 = 0b0000_1111; // 4 bits from A
        let c: u64 = 0b1111_1111; // 4 bits from B, 8 bits from A
        assert_eq!(hamming(a, c), 8); // ends are far apart
        let groups = group_near_duplicates(&[("c".into(), c), ("a".into(), a), ("b".into(), b)], 4);
        // One transitive group of all three, members sorted.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].image_hashes, vec!["a", "b", "c"]);
    }

    #[test]
    fn two_separate_groups_and_a_singleton() {
        // {a,b} are a pair; {c,d} are another pair; e is alone. The singleton
        // must be dropped, and we expect exactly two groups in deterministic
        // order.
        let items = vec![
            ("a".to_string(), 0u64),
            ("b".to_string(), 1u64), // 1 bit from a
            ("c".to_string(), 0xFF00u64),
            ("d".to_string(), 0xFF01u64), // 1 bit from c
            ("e".to_string(), 0x0F0Fu64), // far from everything
        ];
        let groups = group_near_duplicates(&items, 2);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].image_hashes, vec!["a", "b"]);
        assert_eq!(groups[1].image_hashes, vec!["c", "d"]);
    }

    #[test]
    fn empty_and_single_inputs_yield_no_groups() {
        assert!(group_near_duplicates(&[], 8).is_empty());
        assert!(group_near_duplicates(&[("only".into(), 42u64)], 8).is_empty());
    }
}
