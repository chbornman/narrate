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

use fast_image_resize::{self as fir, ResizeAlg, ResizeOptions, Resizer};
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
// DISPLAY_EDGE moved to the centralized tuning config (`crate::tuning`,
// DESIGN-TUNING-CONFIG.md): it is a display-size judgment knob, file-
// overridable without a rebuild. `write_artifacts` reads
// `tuning().preview.display_edge` (code default 2560). NOTE: THUMB_EDGE and
// the WebP qualities/method stay fixed consts here — they are versioned
// ENCODE parameters tied to `GENERATOR_VERSION` (changing them changes
// artifact bytes and must bump the cache version), not runtime feel knobs.
pub const DISPLAY_QUALITY: f32 = 87.0;
/// libwebp effort level for both artifacts (see `encode_webp` for the
/// pp-bench finding behind 2). Sits in this cluster because it is a
/// versioned encode parameter: changing it changes artifact bytes and
/// MUST bump [`GENERATOR_VERSION`]. Not a config knob — runtime tuning
/// would silently fork artifact reproducibility.
const WEBP_METHOD: i32 = 2;

// §9.3 acceptability threshold (embedded preview longest edge ≥ N px) moved to
// the centralized tuning config (`crate::tuning`): it is a "when is the in-RAW
// JPEG good enough" judgment knob, file-overridable. The accept decision in
// `library/mod.rs` reads `tuning().preview.embedded_accept_edge` (code default
// 2048).

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

/// libwebp's hard maximum dimension on either axis (VP8L spec, 14-bit field
/// → 16383). Full-sensor develops up to ~16k px on the long edge encode as
/// WebP; anything wider (panorama stitches, future high-MP sensors) falls
/// back to JPEG. See [`FullDecodeFormat`] and [`encode_full_decode`].
pub const WEBP_MAX_DIMENSION: u32 = 16383;

/// WebP quality for the full-resolution develop artifact. A touch above the
/// 2560 display tier (87) because this IS the 100%-zoom surface — the founder
/// reviews real resolution here, so compression artifacts at 1:1 matter more
/// than the few extra KB on a cache that never evicts (§9.8 L5).
pub const FULL_DECODE_WEBP_QUALITY: f32 = 90.0;

/// JPEG quality for the full-resolution fallback (only when a dimension
/// exceeds [`WEBP_MAX_DIMENSION`]). Matches the embedded-native route's 90 —
/// the same "high-quality JPEG" tier Lightroom's 1:1 preview cache uses.
pub const FULL_DECODE_JPEG_QUALITY: u8 = 90;

/// Cache-busting version stamp EMBEDDED in the full-decode artifact's filename
/// (`<hash>-full-v<N>.{webp,jpg}`). The full-resolution analog of
/// [`GENERATOR_VERSION`]: it versions the DEVELOP algorithm (`raw_develop.rs`),
/// not the encode tier. Bumping it is the SANCTIONED way to force every cached
/// 1:1 to re-develop after a develop change — a new `RAW_DEVELOP_VERSION` makes
/// the current name a cache MISS ([`existing_full_artifact`] only looks for the
/// current version), so the view-time trigger re-develops in the corrected
/// pipeline and [`write_full_decode_artifact`] sweeps the stale file away.
///
/// WHY it starts at 2 (not 1): the pre-fix artifacts were written UNVERSIONED
/// (`<hash>-full.{webp,jpg}`) by the black-develop bug (degenerate colour
/// matrix). Treating those as "v1" and shipping the post-fix develop as v2 means
/// every black artifact on disk is now a miss and re-develops in colour, with no
/// manual cache clear. Any future develop-algorithm change MUST bump this.
pub const RAW_DEVELOP_VERSION: i64 = 2;

/// Which container the full-resolution artifact landed in (WebP preferred for
/// cache consistency; JPEG only when WebP's dimension cap bites). The serve
/// route needs the content-type, and the file suffix differs, so the choice
/// is surfaced rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullDecodeFormat {
    Webp,
    Jpeg,
}

impl FullDecodeFormat {
    /// File suffix WITHOUT the dot (`webp` / `jpg`).
    pub fn ext(self) -> &'static str {
        match self {
            FullDecodeFormat::Webp => "webp",
            FullDecodeFormat::Jpeg => "jpg",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            FullDecodeFormat::Webp => "image/webp",
            FullDecodeFormat::Jpeg => "image/jpeg",
        }
    }

    /// Pick the format for an image of these (already display-oriented)
    /// dimensions: WebP unless an axis exceeds libwebp's cap.
    pub fn for_dimensions(width: u32, height: u32) -> Self {
        if width > WEBP_MAX_DIMENSION || height > WEBP_MAX_DIMENSION {
            FullDecodeFormat::Jpeg
        } else {
            FullDecodeFormat::Webp
        }
    }
}

/// The full-decode artifact's filename for the CURRENT develop version:
/// `<hash>-full-v<N>.{webp,jpg}` (N = [`RAW_DEVELOP_VERSION`]). The `-v<N>`
/// stamp is what auto-invalidates the cache when the develop changes: a bump
/// changes this name, so the old file is no longer found and a re-develop runs.
fn full_artifact_filename(hash: &ContentHash, format: FullDecodeFormat) -> String {
    format!(
        "{}-full-v{}.{}",
        hash.as_str(),
        RAW_DEVELOP_VERSION,
        format.ext()
    )
}

/// `<app_data>/previews/<h[0..2]>/<h[2..4]>/<hash>-full-v<N>.{webp,jpg}` — the
/// NATIVE-resolution full-decode artifact (OD-1) at the CURRENT develop version
/// (`-v<N>`, see [`RAW_DEVELOP_VERSION`]). Distinct from the `-disp`/`-thumb`
/// slots: those hard-resize to 2560/512, this holds the full sensor resolution
/// that Look's 100%-zoom rung serves. It lives ON DISK ONLY (not in
/// `preview_artifacts`) so its mere existence is the view-time trigger's
/// cache-hit signal — no schema migration, no second table to keep coherent
/// with the file. The version stamp in the name is the cache-bust: change the
/// develop, bump the version, and this path stops matching the stale file.
pub fn full_artifact_path(
    cache_dir: &Path,
    hash: &ContentHash,
    format: FullDecodeFormat,
) -> PathBuf {
    let h = hash.as_str();
    cache_dir
        .join("previews")
        .join(&h[0..2])
        .join(&h[2..4])
        .join(full_artifact_filename(hash, format))
}

/// Locate an existing full-decode artifact for `hash` AT THE CURRENT DEVELOP
/// VERSION, whichever format it was written in (WebP first — the common case;
/// JPEG only for over-cap dimensions). Returns the path and its content-type
/// for the serve route. An OLD-version (or unversioned, pre-fix) file is
/// deliberately NOT a hit: it does not carry the current `-v<N>` stamp, so the
/// caller re-develops in the corrected pipeline instead of serving the stale
/// (e.g. black) artifact.
pub fn existing_full_artifact(
    cache_dir: &Path,
    hash: &ContentHash,
) -> Option<(PathBuf, FullDecodeFormat)> {
    for format in [FullDecodeFormat::Webp, FullDecodeFormat::Jpeg] {
        let p = full_artifact_path(cache_dir, hash, format);
        if p.exists() {
            return Some((p, format));
        }
    }
    None
}

/// Delete every STALE full-decode file for `hash`: any `<hash>-full*.{webp,jpg}`
/// that is not the current-version name. Covers both old-version artifacts
/// (`<hash>-full-v1.*` from a prior develop) and the unversioned pre-fix files
/// (`<hash>-full.*` the black-develop bug wrote). Called when the new-version
/// artifact is written so the stale (black) file does not linger and waste cache
/// budget. Cheap + safe: it touches ONLY this one hash's `previews/<h0..2>/<h2..4>/`
/// shard, matches only this hash's full-decode prefix, and never the
/// `-disp`/`-thumb` tiers. Best-effort — a removal failure is not worth failing
/// a develop over (the evictor/clear will still reap it later).
fn remove_stale_full_artifacts(cache_dir: &Path, hash: &ContentHash) {
    let h = hash.as_str();
    // The hash's two-level shard; full-decode files for a hash live ONLY here.
    let shard = cache_dir.join("previews").join(&h[0..2]).join(&h[2..4]);
    let Ok(entries) = std::fs::read_dir(&shard) else {
        return;
    };
    // Names we must keep: the current-version file in either container.
    let keep_webp = full_artifact_filename(hash, FullDecodeFormat::Webp);
    let keep_jpg = full_artifact_filename(hash, FullDecodeFormat::Jpeg);
    // This hash's full-decode prefix; only files starting with it are ours.
    let full_prefix = format!("{h}-full");
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // Must be THIS hash's full-decode artifact (the prefix gate keeps us off
        // every other hash and off the small tiers), AND a full-decode container.
        if !name.starts_with(&full_prefix) {
            continue;
        }
        if !(name.ends_with(".webp") || name.ends_with(".jpg")) {
            continue;
        }
        // Keep the current version; remove everything else (old `-v*` + the
        // unversioned pre-fix `-full.*`).
        if name == keep_webp || name == keep_jpg {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
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
// 1:1 full-decode cache policy (DESIGN-PREVIEW-POLICY.md)
// ---------------------------------------------------------------------------
//
// The thumb + display tiers are small and always kept; only the full-res
// `-full.{webp,jpg}` artifacts are large and user-variable, so they are the
// only tier that gets a retention policy: a single size budget, evict
// LEAST-RECENTLY-VIEWED when the on-disk cache exceeds it. Eviction is always
// SAFE — a discarded 1:1 re-derives on next view and strokes live in
// display-oriented VECTOR coords, never in the cached artifact — so we just
// delete files.
//
// WHY mtime is the LRU signal (not atime): atime is unreliable across the
// platforms/mounts we target — `noatime`/`relatime` (Linux), and macOS volumes
// frequently do not update it on read. We therefore TOUCH each `-full.*` file's
// mtime when it is SERVED (see `touch_full_artifact` / the `/full-decode` route)
// so "least-recently-modified" tracks "least-recently-VIEWED" reliably,
// independent of the OS atime policy.

/// Whether a directory entry is a full-decode 1:1 artifact OF ANY VERSION:
/// the current versioned name (`<hash>-full-v<N>.{webp,jpg}`), an old-version
/// one (`<hash>-full-v1.*`), or the unversioned pre-fix shape
/// (`<hash>-full.{webp,jpg}`). The cache policy (stats/evict/clear) governs the
/// WHOLE 1:1 tier regardless of develop version — a stale old-version file still
/// costs disk and must be counted and reapable — while the small `-thumb`/`-disp`
/// tiers are always kept and are never matched here.
///
/// The match is "contains `-full` immediately before the extension", which
/// covers `-full.webp`, `-full-v2.webp`, etc. without enumerating versions.
fn is_full_artifact(name: &str) -> bool {
    let stem = name
        .strip_suffix(".webp")
        .or_else(|| name.strip_suffix(".jpg"));
    match stem {
        // `<hash>-full` (unversioned) or `<hash>-full-v<N>` (any version).
        Some(stem) => {
            stem.ends_with("-full")
                || stem
                    .rsplit_once("-full-v")
                    .is_some_and(|(_, v)| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

/// One full-decode artifact on disk: its path, byte size, and last-VIEWED
/// signal (mtime, see the module note on why not atime). Used to rank the
/// LRU eviction order.
#[derive(Debug, Clone)]
struct FullArtifactEntry {
    path: PathBuf,
    bytes: u64,
    /// Last-modified time; we touch it on serve so it tracks last-viewed.
    /// `SystemTime::UNIX_EPOCH` when the filesystem cannot report it (treated
    /// as oldest → evicted first, which is the safe direction).
    modified: std::time::SystemTime,
}

/// Walk the `previews/` tree once and collect every full-decode 1:1 artifact
/// with its size + last-viewed mtime. The small thumb/display tiers are
/// skipped (they are always kept). Missing tree → empty vec, never an error.
fn collect_full_artifacts(cache_dir: &Path) -> Vec<FullArtifactEntry> {
    let previews = cache_dir.join("previews");
    let mut out = Vec::new();
    if !previews.exists() {
        return out;
    }
    for entry in walkdir::WalkDir::new(&previews).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if !is_full_artifact(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        out.push(FullArtifactEntry {
            path: entry.path().to_path_buf(),
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        });
    }
    out
}

/// On-disk size + file count of the preview cache, split by tier. Surfaced to
/// Settings → Previews (the cache-size readout) so the user can see what the
/// 1:1 cache costs without opening a file manager.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewCacheStats {
    /// Bytes held by the full-res 1:1 (`-full.*`) artifacts — the tier the
    /// budget governs.
    pub full_bytes: u64,
    /// Count of full-res 1:1 artifacts.
    pub full_files: u64,
    /// Bytes held by ALL previews (1:1 + display + thumb) under `previews/`.
    pub total_bytes: u64,
}

/// Compute [`PreviewCacheStats`]: the full-res 1:1 cache size/count plus the
/// total previews footprint. One pass over `previews/` (cheap — it stats, it
/// does not read bytes). A missing tree reports all-zero.
pub fn preview_cache_stats(cache_dir: &Path) -> PreviewCacheStats {
    let previews = cache_dir.join("previews");
    let mut stats = PreviewCacheStats::default();
    if !previews.exists() {
        return stats;
    }
    for entry in walkdir::WalkDir::new(&previews).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        // Temp files are mid-write scratch, not cache the user "has" — skip.
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if name.starts_with(TMP_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let len = meta.len();
        stats.total_bytes += len;
        if is_full_artifact(name) {
            stats.full_bytes += len;
            stats.full_files += 1;
        }
    }
    stats
}

/// Which tier(s) a manual clear removes. `Full` = just the big 1:1 cache;
/// `All` = 1:1 + display + thumb (the whole `previews/` tree). Both are SAFE:
/// every tier re-derives from the original on next view, and strokes never
/// live in an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearKind {
    /// Only the full-res 1:1 (`-full.*`) artifacts.
    Full,
    /// Every preview artifact (1:1 + display + thumb).
    All,
}

impl ClearKind {
    /// Parse the IPC string (`"full"` | `"all"`); unknown values are rejected
    /// so a typo cannot silently nuke the whole cache. (Named `parse`, not
    /// `from_str`, to avoid shadowing the `FromStr` trait method — this returns
    /// `Option`, not `Result`, and is only ever called on the IPC string.)
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(ClearKind::Full),
            "all" => Some(ClearKind::All),
            _ => None,
        }
    }
}

/// Manually clear the preview cache: the full-res 1:1 tier only ([`ClearKind::Full`])
/// or every preview artifact ([`ClearKind::All`]). Returns the number of files
/// removed. SAFE — every removed artifact re-derives on next view. Temp files
/// (`.pp-tmp-*`) are left to `sweep_temp_files` (a clear should not race a
/// concurrent write's scratch file). A missing tree removes nothing.
pub fn clear_preview_cache(cache_dir: &Path, kind: ClearKind) -> io::Result<u64> {
    let previews = cache_dir.join("previews");
    if !previews.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in walkdir::WalkDir::new(&previews).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if name.starts_with(TMP_PREFIX) {
            continue;
        }
        let target = match kind {
            ClearKind::Full => is_full_artifact(name),
            ClearKind::All => true,
        };
        if target {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Trim the full-res 1:1 cache to `budget_bytes` by evicting
/// LEAST-RECENTLY-VIEWED artifacts first (oldest mtime; see the module note on
/// the mtime-as-last-viewed choice). No-op when already under budget — it never
/// evicts a cache that fits. Returns the number of files evicted. SAFE: each
/// evicted 1:1 re-derives on next view, strokes are unaffected. Errors from a
/// single `remove_file` are swallowed (a racing clear/develop may have removed
/// it); the pass still drives the rest of the cache under budget.
pub fn evict_preview_cache(cache_dir: &Path, budget_bytes: u64) -> u64 {
    let mut entries = collect_full_artifacts(cache_dir);
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    // Under budget: nothing to do — the common, hot path (most reviews never
    // breach the budget, and we run this after every develop).
    if total <= budget_bytes {
        return 0;
    }
    // Oldest-viewed first: ascending mtime is the LRU eviction order.
    entries.sort_by_key(|e| e.modified);
    let mut live = total;
    let mut evicted = 0;
    for e in &entries {
        if live <= budget_bytes {
            break;
        }
        // remove_file failures (a concurrent clear/develop won the race) do
        // not stop the pass — we still account the bytes as gone and move on.
        let _ = std::fs::remove_file(&e.path);
        live = live.saturating_sub(e.bytes);
        evicted += 1;
    }
    evicted
}

/// Bump a full-decode artifact's mtime to NOW — the explicit touch-on-serve
/// that makes "least-recently-modified" mean "least-recently-VIEWED" (see the
/// module note: we cannot trust atime across the platforms/mounts we target).
/// Called by the `/full-decode` serve route. Best-effort: a failure (the file
/// vanished, read-only volume) is not worth failing a serve over — the LRU
/// signal degrades gracefully, the image still loads.
pub fn touch_full_artifact(path: &Path) {
    // `set_modified` to the current instant; filetime keeps this one syscall.
    let _ = filetime::set_file_mtime(path, filetime::FileTime::now());
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

/// Encode one linear-light [0,1] component to an 8-bit sRGB value using the
/// IEC 61966-2-1 transfer function (the 12.92 linear toe + 2.4 power curve).
///
/// WHY this is a shared `pub(crate)` helper: the raw-develop pipeline
/// (`raw_develop.rs`, OD-1) and the AdobeRGB→sRGB conversion below both need
/// the EXACT same encode — a divergent gamma curve between the two would make
/// a full-decode artifact tonally disagree with its embedded preview, which
/// the §9.4 "neutral but plausible" contract forbids. Centralizing it keeps
/// one transfer function for every sRGB artifact the cache holds.
pub(crate) fn srgb_encode_u8(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let e = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0).round() as u8
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
    for px in rgba.chunks_exact_mut(4) {
        let r = decode[px[0] as usize];
        let g = decode[px[1] as usize];
        let b = decode[px[2] as usize];
        for (i, row) in M.iter().enumerate() {
            px[i] = srgb_encode_u8(row[0] * r + row[1] * g + row[2] * b);
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
    // WHY fast_image_resize: image-rs's resampler is scalar; this crate runs a
    // NEON-SIMD separable convolution (~7x on the resize step, PLAN-PERF P1).
    // We keep FilterType::CatmullRom — the SAME kernel image::resize used — so
    // output stays at near-parity (a convolution can differ by ±1 LSB, never
    // geometry). The `image` Cargo feature gives DynamicImage the
    // IntoImageView/IntoImageViewMut impls, so we resize straight into a
    // destination DynamicImage of the source's own pixel type (preview feeds
    // Rgb8/Rgba8; we preserve whatever it is rather than force a conversion).
    //
    // WHY this aspect math: image::resize(edge, edge, Fit) does NOT stretch to
    // a square — it scales so the longest side lands on `edge` and preserves
    // aspect. We replicate image-rs's exact `resize_dimensions` arithmetic
    // (ratio = min(edge/w, edge/h); round; clamp >= 1) so the cached artifact
    // geometry is byte-identical to before. The overflow guard image-rs adds is
    // irrelevant here: this is a pure downscale (the early return covers the
    // no-upscale case), so the scaled dims never exceed the source.
    let ratio = f64::min(
        f64::from(edge) / f64::from(w),
        f64::from(edge) / f64::from(h),
    );
    let tw = ((f64::from(w) * ratio).round() as u64).max(1) as u32;
    let th = ((f64::from(h) * ratio).round() as u64).max(1) as u32;

    // Destination must carry the source's pixel type for the IntoImageViewMut
    // impl to round-trip without a lossy conversion. Match the source variant;
    // fall back to image-rs's scalar resize for any exotic type fast_image_resize
    // cannot view (belt-and-suspenders — preview only ever produces Rgb8/Rgba8).
    let mut dst = match img {
        DynamicImage::ImageRgb8(_) => DynamicImage::new_rgb8(tw, th),
        DynamicImage::ImageRgba8(_) => DynamicImage::new_rgba8(tw, th),
        DynamicImage::ImageLuma8(_) => DynamicImage::new_luma8(tw, th),
        DynamicImage::ImageLumaA8(_) => DynamicImage::new_luma_a8(tw, th),
        DynamicImage::ImageRgb16(_) => DynamicImage::new_rgb16(tw, th),
        DynamicImage::ImageRgba16(_) => DynamicImage::new_rgba16(tw, th),
        DynamicImage::ImageLuma16(_) => DynamicImage::new_luma16(tw, th),
        DynamicImage::ImageLumaA16(_) => DynamicImage::new_luma_a16(tw, th),
        DynamicImage::ImageRgb32F(_) => DynamicImage::new_rgb32f(tw, th),
        DynamicImage::ImageRgba32F(_) => DynamicImage::new_rgba32f(tw, th),
        _ => return img.resize(edge, edge, image::imageops::FilterType::CatmullRom),
    };

    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(fir::FilterType::CatmullRom));
    let mut resizer = Resizer::new();
    match resizer.resize(img, &mut dst, &opts) {
        Ok(()) => dst,
        // Should not happen for the supported pixel types above; if the SIMD
        // path ever rejects an input, fall back to the scalar resize rather
        // than panic mid-ingest.
        Err(_) => img.resize(edge, edge, image::imageops::FilterType::CatmullRom),
    }
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
    config.method = WEBP_METHOD;
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
    // Display-tier longest edge from the centralized tuning config (code
    // default 2560; file-overridable via `tuning.toml`).
    let display_edge = crate::tuning::tuning().preview.display_edge;
    let display = metrics
        .resize
        .time(|| resize_to_edge(display_oriented, display_edge));
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

/// Encode a display-oriented image at its NATIVE resolution for the
/// full-decode artifact (OD-1). WebP unless a dimension exceeds libwebp's cap
/// ([`WEBP_MAX_DIMENSION`]), then high-quality JPEG. NO resize — the whole
/// point of this slot is real 1:1 pixels (the `-disp`/`-thumb` slots already
/// carry the down-scaled tiers). Returns the encoded bytes and the format so
/// the caller writes the correct file suffix and the route serves the right
/// content-type.
pub fn encode_full_decode(img: &DynamicImage) -> Result<(Vec<u8>, FullDecodeFormat), PreviewError> {
    let (w, h) = img.dimensions();
    let format = FullDecodeFormat::for_dimensions(w, h);
    match format {
        FullDecodeFormat::Webp => Ok((encode_webp(img, FULL_DECODE_WEBP_QUALITY), format)),
        FullDecodeFormat::Jpeg => {
            // Reuse the native-JPEG encoder (drops alpha — a develop output is
            // always opaque RGB) at the full-decode quality.
            let rgb = img.to_rgb8();
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut std::io::Cursor::new(&mut out),
                FULL_DECODE_JPEG_QUALITY,
            )
            .encode_image(&rgb)
            .map_err(|e| PreviewError::Decode(e.to_string()))?;
            Ok((out, format))
        }
    }
}

/// Encode and atomically write the native-resolution full-decode artifact,
/// returning the format written (so the worker records which container the
/// serve route will find). The `-disp`/`-thumb` tiers are written separately
/// by [`write_artifacts`]; this is the additional 1:1 slot (OD-1).
pub fn write_full_decode_artifact(
    cache_dir: &Path,
    hash: &ContentHash,
    display_oriented: &DynamicImage,
) -> Result<FullDecodeFormat, PreviewError> {
    let (bytes, format) = encode_full_decode(display_oriented)?;
    let dest = full_artifact_path(cache_dir, hash, format);
    atomic_write(&dest, &bytes)?;
    // Write-then-sweep: the new-version artifact is on disk, so now drop any
    // OLD-version / unversioned full file for this hash (e.g. the black v1 the
    // pre-fix develop wrote). Done AFTER the atomic rename so a crash never
    // leaves the hash with neither the new nor the old artifact. Best-effort and
    // scoped to this hash's shard (see `remove_stale_full_artifacts`).
    remove_stale_full_artifacts(cache_dir, hash);
    Ok(format)
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
/// `pub(crate)`: the full-decode pass reuses it as the geometry-safety
/// assertion threshold against the display artifact (OD-1).
///
/// FIXED const, deliberately NOT in the tuning config (founder decision): this
/// is a geometry CONTRACT that protects stroke coordinate safety, not a feel
/// knob. A `tuning.toml` edit must never be able to silently loosen it and let
/// a 90°-rotated preview serve under annotations. Stays "fixed" in tuning.html.
pub(crate) const EMBEDDED_NATIVE_ASPECT_TOLERANCE: f64 = 0.02;

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

    // ---- 1:1 full-decode cache policy (DESIGN-PREVIEW-POLICY.md) ----------

    /// Write a `<hash>-full.<ext>` artifact of `bytes` length into the fanned
    /// cache layout, with an explicit mtime so the LRU order is deterministic
    /// (tests cannot rely on write-order timestamps at filesystem resolution).
    fn write_full(cache_dir: &Path, seed: &[u8], ext: &str, bytes: usize, mtime_secs: i64) {
        let h = ContentHash::from_bytes_of(seed);
        let dest = full_artifact_path(
            cache_dir,
            &h,
            if ext == "jpg" {
                FullDecodeFormat::Jpeg
            } else {
                FullDecodeFormat::Webp
            },
        );
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, vec![0u8; bytes]).unwrap();
        filetime::set_file_mtime(&dest, filetime::FileTime::from_unix_time(mtime_secs, 0)).unwrap();
    }

    /// Write a small `<hash>-{disp,thumb}.webp` artifact (the always-kept
    /// tiers) so tests can prove the policy NEVER touches them.
    fn write_small(cache_dir: &Path, seed: &[u8], kind: ArtifactKind, bytes: usize) {
        let h = ContentHash::from_bytes_of(seed);
        let dest = artifact_path(cache_dir, &h, kind);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn evictor_is_a_noop_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        write_full(dir.path(), b"a", "webp", 1000, 100);
        write_full(dir.path(), b"b", "webp", 1000, 200);
        // 2000 bytes, budget 5000: nothing evicted, both survive.
        assert_eq!(evict_preview_cache(dir.path(), 5000), 0);
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 2);
        assert_eq!(stats.full_bytes, 2000);
    }

    #[test]
    fn evictor_trims_oldest_first_to_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Three 1000-byte 1:1s, distinct mtimes: a oldest, c newest.
        write_full(dir.path(), b"a", "webp", 1000, 100);
        write_full(dir.path(), b"b", "jpg", 1000, 200);
        write_full(dir.path(), b"c", "webp", 1000, 300);
        // Budget 1500: must drop the two oldest (a, then b) to land at 1000.
        let evicted = evict_preview_cache(dir.path(), 1500);
        assert_eq!(evicted, 2);
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 1);
        assert_eq!(stats.full_bytes, 1000);
        // The survivor is the NEWEST-viewed (c) — least-recently-viewed went.
        let c = full_artifact_path(
            dir.path(),
            &ContentHash::from_bytes_of(b"c"),
            FullDecodeFormat::Webp,
        );
        assert!(c.exists(), "newest-viewed 1:1 must survive");
        let a = full_artifact_path(
            dir.path(),
            &ContentHash::from_bytes_of(b"a"),
            FullDecodeFormat::Webp,
        );
        assert!(!a.exists(), "oldest-viewed 1:1 must be evicted first");
    }

    #[test]
    fn evictor_never_touches_small_tiers() {
        let dir = tempfile::tempdir().unwrap();
        write_full(dir.path(), b"a", "webp", 5000, 100);
        write_small(dir.path(), b"a", ArtifactKind::Display, 400);
        write_small(dir.path(), b"a", ArtifactKind::Thumb, 100);
        // Budget 0: evict the entire 1:1 cache…
        assert_eq!(evict_preview_cache(dir.path(), 0), 1);
        // …but the thumb/display tiers are untouched (always kept).
        assert!(
            artifact_path(
                dir.path(),
                &ContentHash::from_bytes_of(b"a"),
                ArtifactKind::Display
            )
            .exists()
        );
        assert!(
            artifact_path(
                dir.path(),
                &ContentHash::from_bytes_of(b"a"),
                ArtifactKind::Thumb
            )
            .exists()
        );
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 0);
        assert_eq!(stats.full_bytes, 0);
        // total still counts the surviving small tiers.
        assert_eq!(stats.total_bytes, 500);
    }

    #[test]
    fn stats_split_full_from_total_and_ignore_temp() {
        let dir = tempfile::tempdir().unwrap();
        write_full(dir.path(), b"a", "webp", 1000, 100);
        write_full(dir.path(), b"b", "jpg", 2000, 200);
        write_small(dir.path(), b"a", ArtifactKind::Display, 300);
        // A mid-write temp scratch file must NOT count toward either total.
        let tmp = dir
            .path()
            .join("previews")
            .join(format!("{TMP_PREFIX}999-x-full.webp"));
        std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
        std::fs::write(&tmp, vec![0u8; 9999]).unwrap();
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 2);
        assert_eq!(stats.full_bytes, 3000);
        assert_eq!(stats.total_bytes, 3300, "small tier counted, temp ignored");
    }

    #[test]
    fn stats_on_missing_tree_is_all_zero() {
        let dir = tempfile::tempdir().unwrap();
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats, PreviewCacheStats::default());
        // And the evictor is a no-op on an empty/absent cache.
        assert_eq!(evict_preview_cache(dir.path(), 0), 0);
    }

    #[test]
    fn clear_full_removes_only_the_1to1_tier() {
        let dir = tempfile::tempdir().unwrap();
        write_full(dir.path(), b"a", "webp", 1000, 100);
        write_full(dir.path(), b"b", "jpg", 2000, 200);
        write_small(dir.path(), b"a", ArtifactKind::Display, 300);
        write_small(dir.path(), b"a", ArtifactKind::Thumb, 100);
        // Clear "full": both 1:1s go, the small tiers stay.
        assert_eq!(clear_preview_cache(dir.path(), ClearKind::Full).unwrap(), 2);
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 0);
        assert_eq!(stats.total_bytes, 400, "display+thumb survive a Full clear");
    }

    #[test]
    fn clear_all_removes_every_artifact() {
        let dir = tempfile::tempdir().unwrap();
        write_full(dir.path(), b"a", "webp", 1000, 100);
        write_small(dir.path(), b"a", ArtifactKind::Display, 300);
        write_small(dir.path(), b"a", ArtifactKind::Thumb, 100);
        // Clear "all": every preview artifact removed.
        assert_eq!(clear_preview_cache(dir.path(), ClearKind::All).unwrap(), 3);
        assert_eq!(
            preview_cache_stats(dir.path()),
            PreviewCacheStats::default()
        );
    }

    #[test]
    fn clear_kind_parses_known_and_rejects_typos() {
        assert_eq!(ClearKind::parse("full"), Some(ClearKind::Full));
        assert_eq!(ClearKind::parse("all"), Some(ClearKind::All));
        assert_eq!(ClearKind::parse("everything"), None);
        assert_eq!(ClearKind::parse(""), None);
    }

    #[test]
    fn touch_on_serve_makes_a_file_freshly_viewed() {
        let dir = tempfile::tempdir().unwrap();
        // Two 1:1s; "old" is the least-recently-viewed by initial mtime.
        write_full(dir.path(), b"old", "webp", 1000, 100);
        write_full(dir.path(), b"new", "webp", 1000, 200);
        let old = full_artifact_path(
            dir.path(),
            &ContentHash::from_bytes_of(b"old"),
            FullDecodeFormat::Webp,
        );
        // Serving "old" touches it → it becomes the most-recently-viewed.
        touch_full_artifact(&old);
        // Now "new" is the oldest. A 1500 budget must evict "new", keep "old".
        assert_eq!(evict_preview_cache(dir.path(), 1500), 1);
        assert!(old.exists(), "the just-touched (served) 1:1 must survive");
        let new = full_artifact_path(
            dir.path(),
            &ContentHash::from_bytes_of(b"new"),
            FullDecodeFormat::Webp,
        );
        assert!(
            !new.exists(),
            "the un-served 1:1 is now least-recently-viewed"
        );
    }

    // ---- full-decode develop versioning (cache auto-invalidation) ---------

    /// The versioned path round-trips: writing at the CURRENT develop version
    /// is found by `existing_full_artifact`, and the name carries the `-v<N>`
    /// stamp (the cache-bust hook).
    #[test]
    fn versioned_full_artifact_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let img =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 6, image::Rgb([10, 20, 30])));
        let fmt = write_full_decode_artifact(dir.path(), &ContentHash::from_bytes_of(b"v"), &img)
            .unwrap();
        assert_eq!(fmt, FullDecodeFormat::Webp);
        let h = ContentHash::from_bytes_of(b"v");
        let (found, found_fmt) =
            existing_full_artifact(dir.path(), &h).expect("current-version artifact is a hit");
        assert_eq!(found_fmt, FullDecodeFormat::Webp);
        // The on-disk name carries the version stamp.
        let name = found.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            name,
            format!("{}-full-v{}.webp", h.as_str(), RAW_DEVELOP_VERSION)
        );
    }

    /// An OLD-version (or unversioned, pre-fix) artifact is NOT a cache hit —
    /// the develop FIX must force a re-develop, not serve the stale black file.
    #[test]
    fn old_or_unversioned_full_artifact_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let h = ContentHash::from_bytes_of(b"stale");
        let shard = dir
            .path()
            .join("previews")
            .join(&h.as_str()[0..2])
            .join(&h.as_str()[2..4]);
        std::fs::create_dir_all(&shard).unwrap();
        // The unversioned pre-fix black file…
        std::fs::write(shard.join(format!("{}-full.webp", h.as_str())), b"old").unwrap();
        // …and an explicit older version.
        std::fs::write(shard.join(format!("{}-full-v1.webp", h.as_str())), b"older").unwrap();
        // Neither is the CURRENT version: no cache hit, the caller re-develops.
        assert!(
            existing_full_artifact(dir.path(), &h).is_none(),
            "a stale-version full artifact must not satisfy the cache check"
        );
        // But the cache policy still SEES them as 1:1 cache (so the evictor and
        // stats account for the disk they cost).
        let stats = preview_cache_stats(dir.path());
        assert_eq!(
            stats.full_files, 2,
            "stale full artifacts still count as 1:1"
        );
    }

    /// Writing the new-version artifact sweeps the stale old-version + the
    /// unversioned pre-fix file for that SAME hash, and leaves other hashes and
    /// the small tiers untouched.
    #[test]
    fn writing_new_version_removes_stale_full_files() {
        let dir = tempfile::tempdir().unwrap();
        let h = ContentHash::from_bytes_of(b"sweep");
        let shard = dir
            .path()
            .join("previews")
            .join(&h.as_str()[0..2])
            .join(&h.as_str()[2..4]);
        std::fs::create_dir_all(&shard).unwrap();
        // Pre-fix unversioned + an older version, both for our hash.
        std::fs::write(shard.join(format!("{}-full.webp", h.as_str())), b"black").unwrap();
        std::fs::write(shard.join(format!("{}-full-v1.jpg", h.as_str())), b"older").unwrap();
        // A DIFFERENT hash's stale file must survive (scoped cleanup).
        let other = ContentHash::from_bytes_of(b"other");
        let other_shard = dir
            .path()
            .join("previews")
            .join(&other.as_str()[0..2])
            .join(&other.as_str()[2..4]);
        std::fs::create_dir_all(&other_shard).unwrap();
        let other_stale = other_shard.join(format!("{}-full.webp", other.as_str()));
        std::fs::write(&other_stale, b"keep-me").unwrap();
        // The small tiers for our hash must survive (never full-decode files).
        write_small(dir.path(), b"sweep", ArtifactKind::Display, 100);

        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3])));
        write_full_decode_artifact(dir.path(), &h, &img).unwrap();

        // Stale full files for OUR hash are gone…
        assert!(!shard.join(format!("{}-full.webp", h.as_str())).exists());
        assert!(!shard.join(format!("{}-full-v1.jpg", h.as_str())).exists());
        // …the new current-version file is present…
        assert!(existing_full_artifact(dir.path(), &h).is_some());
        // …a different hash's stale file is untouched…
        assert!(other_stale.exists(), "cleanup must not cross hash shards");
        // …and the small display tier survives.
        assert!(
            artifact_path(dir.path(), &h, ArtifactKind::Display).exists(),
            "the small tiers are never swept by full-decode cleanup"
        );
    }

    /// The cache policy (stats/evict/clear) counts and reaps VERSIONED full
    /// artifacts, not just the legacy unversioned shape.
    #[test]
    fn cache_policy_handles_versioned_full_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        // Two current-version 1:1s via the real writer (distinct hashes).
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(6, 6, image::Rgb([5, 5, 5])));
        write_full_decode_artifact(dir.path(), &ContentHash::from_bytes_of(b"p1"), &img).unwrap();
        write_full_decode_artifact(dir.path(), &ContentHash::from_bytes_of(b"p2"), &img).unwrap();
        // Stats see both versioned files as the 1:1 cache.
        let stats = preview_cache_stats(dir.path());
        assert_eq!(stats.full_files, 2, "versioned full artifacts are counted");
        assert!(stats.full_bytes > 0);
        // A Full clear removes the versioned files (the glob matches `-full-v*`).
        assert_eq!(
            clear_preview_cache(dir.path(), ClearKind::Full).unwrap(),
            2,
            "clear removes versioned full artifacts"
        );
        assert_eq!(preview_cache_stats(dir.path()).full_files, 0);
        // And the evictor reaps a versioned file when over budget.
        write_full_decode_artifact(dir.path(), &ContentHash::from_bytes_of(b"p3"), &img).unwrap();
        assert_eq!(
            evict_preview_cache(dir.path(), 0),
            1,
            "evictor reaps versioned full artifacts"
        );
    }

    #[test]
    fn is_full_artifact_matches_versioned_and_legacy() {
        assert!(is_full_artifact("abc-full.webp"));
        assert!(is_full_artifact("abc-full.jpg"));
        assert!(is_full_artifact("abc-full-v1.webp"));
        assert!(is_full_artifact("abc-full-v2.jpg"));
        assert!(is_full_artifact("abc-full-v17.webp"));
        // Small tiers and non-full names never match.
        assert!(!is_full_artifact("abc-disp.webp"));
        assert!(!is_full_artifact("abc-thumb.webp"));
        assert!(!is_full_artifact("abc-full-vX.webp"), "non-numeric version");
        assert!(!is_full_artifact("abc-full-v.webp"), "empty version");
        assert!(!is_full_artifact("abc-fullish.webp"));
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
