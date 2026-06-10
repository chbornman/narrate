//! Sidecar placement and naming (spec/SIDECARS.md §2, §7.2, §8.1, §9.3,
//! §10.3, §12.1).

use std::path::{Path, PathBuf};

use crate::id::{ContentHash, SessionId, UtcMillis};

use super::serialize::compact_timestamp;

/// §2.1: the suffix, always written lowercase.
pub const SIDECAR_SUFFIX: &str = ".photoproof.json";

/// §2.1: adjacent sidecar path for an image file: `F` → `F.photoproof.json`
/// in the same directory, prefix byte-for-byte as on disk.
pub fn sidecar_path_for_image(image_path: &Path) -> PathBuf {
    let name = image_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    image_path.with_file_name(format!("{name}{SIDECAR_SUFFIX}"))
}

/// §2.2: a file qualifies as a sidecar if its name ends with the suffix
/// compared case-insensitively (defensive against FAT/exFAT case mangling).
pub fn is_sidecar_filename(name: &str) -> bool {
    name.len() > SIDECAR_SUFFIX.len()
        && name
            .get(name.len() - SIDECAR_SUFFIX.len()..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(SIDECAR_SUFFIX))
}

/// The image-filename prefix of a sidecar name (§2.1), when the name
/// qualifies (§2.2).
pub fn image_filename_of_sidecar(name: &str) -> Option<&str> {
    if is_sidecar_filename(name) {
        Some(&name[..name.len() - SIDECAR_SUFFIX.len()])
    } else {
        None
    }
}

/// §9.3: orphaned temp files `*.photoproof.json.tmp-*` — ignored by all
/// scanners, never parsed, never merged.
pub fn is_temp_orphan(name: &str) -> bool {
    name.to_ascii_lowercase()
        .contains(&format!("{SIDECAR_SUFFIX}.tmp-"))
}

/// §10.3: corrupt files renamed aside, preserved for manual inspection.
pub fn corrupt_aside_path(path: &Path, now: UtcMillis) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.corrupt-{}", compact_timestamp(now)))
}

/// §8.1 / §12.1: two-level content-hash fan-out: `<h0..2>/<h2..4>/<hash>`.
pub fn fanout_rel_path(hash: &ContentHash) -> PathBuf {
    let h = hash.as_str();
    PathBuf::from(&h[0..2])
        .join(&h[2..4])
        .join(format!("{h}{SIDECAR_SUFFIX}"))
}

/// §8.1: `<app-data>/photoproof/journal/overflow`.
pub fn overflow_dir(app_data: &Path) -> PathBuf {
    app_data.join("photoproof").join("journal").join("overflow")
}

/// §8.1: full overflow path for one image.
pub fn overflow_path(app_data: &Path, hash: &ContentHash) -> PathBuf {
    overflow_dir(app_data).join(fanout_rel_path(hash))
}

/// §7.2: `<app-data>/photoproof/journal/sessions`.
pub fn sessions_dir(app_data: &Path) -> PathBuf {
    app_data.join("photoproof").join("journal").join("sessions")
}

/// §7.2: session journal path.
pub fn session_journal_path(app_data: &Path, session: &SessionId) -> PathBuf {
    sessions_dir(app_data).join(format!("{}{SIDECAR_SUFFIX}", session.as_str()))
}

/// §12.1: default export directory name.
pub fn default_export_dir_name(now: UtcMillis) -> String {
    format!("photoproof-export-{}", compact_timestamp(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_rules() {
        assert_eq!(
            sidecar_path_for_image(Path::new("/x/IMG_4471.arw")),
            PathBuf::from("/x/IMG_4471.arw.photoproof.json")
        );
        assert_eq!(
            sidecar_path_for_image(Path::new("/x/frame17")),
            PathBuf::from("/x/frame17.photoproof.json")
        );
        assert!(is_sidecar_filename("a.JPG.PhotoProof.Json")); // case-insensitive scan
        assert!(!is_sidecar_filename(".photoproof.json")); // empty prefix
        assert!(!is_sidecar_filename("a.photoproof.json.tmp-1a2b3c4d"));
        assert!(!is_sidecar_filename(
            "a.photoproof.json.corrupt-20260609T193000Z"
        ));
        assert!(is_temp_orphan("a.photoproof.json.tmp-1a2b3c4d"));
        assert_eq!(
            image_filename_of_sidecar("IMG_4471.arw.photoproof.json"),
            Some("IMG_4471.arw")
        );
    }
}
