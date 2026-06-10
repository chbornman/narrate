//! Exclusion rules and the format allowlist.
//!
//! Contract: spec/LIBRARY.md §5.1 (exclusions), §9.1 (allowlist). Ships as a
//! built-in constant; user-editable exclusions are post-v1.

/// Sanity ceiling: files above this are skipped and logged (§5.1).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// File format as stored in `images.format` (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Tiff,
    Webp,
    Heic,
    Raw,
}

impl ImageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Png => "png",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Webp => "webp",
            ImageFormat::Heic => "heic",
            ImageFormat::Raw => "raw",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "jpeg" => ImageFormat::Jpeg,
            "png" => ImageFormat::Png,
            "tiff" => ImageFormat::Tiff,
            "webp" => ImageFormat::Webp,
            "heic" => ImageFormat::Heic,
            "raw" => ImageFormat::Raw,
            _ => return None,
        })
    }
}

/// §9.1 RAW extensions (also the `raw_subtype` values).
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "raf", "orf", "ori", "rw2", "pef", "srw",
    "x3f", "3fr", "fff", "iiq", "mos", "mrw", "kdc", "dcr", "sr2", "srf", "erf",
];

/// Classify a filename by extension against the §9.1 allowlist.
/// Returns `(format, raw_subtype)`; `None` = off the allowlist.
pub fn classify_extension(file_name: &str) -> Option<(ImageFormat, Option<&'static str>)> {
    let ext = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some((ImageFormat::Jpeg, None)),
        "png" => Some((ImageFormat::Png, None)),
        "tif" | "tiff" => Some((ImageFormat::Tiff, None)),
        "webp" => Some((ImageFormat::Webp, None)),
        "heic" | "heif" => Some((ImageFormat::Heic, None)),
        _ => RAW_EXTENSIONS
            .iter()
            .find(|&&r| r == ext)
            .map(|&r| (ImageFormat::Raw, Some(r))),
    }
}

/// Known tool/cache directory names, matched case-insensitively (§5.1).
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "@eadir",
    "__macosx",
    "$recycle.bin",
    "system volume information",
    "lost+found",
    "node_modules",
    // Capture One session subfolders:
    "cache",
    "proxies",
    "thumbnails",
];

/// Is this directory entry name skipped during walks / ignored by the
/// watcher? (§5.1 — applies to directories.)
pub fn is_excluded_dir_name(name: &str) -> bool {
    // Hidden entries (covers .photoproof-volume, .DS_Store, .dtrash, .git,
    // .thumbnails, .dropbox.cache).
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    if EXCLUDED_DIR_NAMES.contains(&lower.as_str()) {
        return true;
    }
    // Suffix/prefix patterns.
    lower.ends_with(".lrdata")
        || lower.ends_with(".cocatalogdb")
        || lower.starts_with("lightroom catalog")
}

/// Is this file name skipped during walks / ignored by the watcher?
/// (§5.1 — applies to files; the format allowlist and size ceiling are
/// checked separately.)
pub fn is_excluded_file_name(name: &str) -> bool {
    if name.starts_with('.') {
        return true; // hidden; covers `.~*` partials and `.photoproof-volume`
    }
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".photoproof.json")
        || lower.ends_with(".tmp.drivedownload")
        || lower.ends_with(".tmp.driveupload")
        || lower.starts_with("~$")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_classification() {
        assert_eq!(classify_extension("a.JPG"), Some((ImageFormat::Jpeg, None)));
        assert_eq!(
            classify_extension("b.heif"),
            Some((ImageFormat::Heic, None))
        );
        assert_eq!(
            classify_extension("c.NEF"),
            Some((ImageFormat::Raw, Some("nef")))
        );
        assert_eq!(
            classify_extension("d.cr3"),
            Some((ImageFormat::Raw, Some("cr3")))
        );
        assert_eq!(classify_extension("e.gif"), None);
        assert_eq!(classify_extension("f.psd"), None);
        assert_eq!(classify_extension("noext"), None);
    }

    #[test]
    fn excluded_names() {
        assert!(is_excluded_dir_name(".git"));
        assert!(is_excluded_dir_name("@eaDir"));
        assert!(is_excluded_dir_name("__MACOSX"));
        assert!(is_excluded_dir_name("Catalog.lrdata"));
        assert!(is_excluded_dir_name("Lightroom Catalog 2"));
        assert!(is_excluded_dir_name("$RECYCLE.BIN"));
        assert!(is_excluded_dir_name("System Volume Information"));
        assert!(is_excluded_dir_name("Cache"));
        assert!(!is_excluded_dir_name("Photos"));

        assert!(is_excluded_file_name(".DS_Store"));
        assert!(is_excluded_file_name(".photoproof-volume"));
        assert!(is_excluded_file_name("IMG_1.jpg.photoproof.json"));
        assert!(is_excluded_file_name("x.tmp.drivedownload"));
        assert!(is_excluded_file_name("~$doc.jpg"));
        assert!(is_excluded_file_name(".~lock.jpg"));
        assert!(!is_excluded_file_name("IMG_1.jpg"));
    }
}
