//! Filesystem path-equivalence seam.
//!
//! Case sensitivity is a property of the mounted volume, not the host OS or
//! filesystem name (APFS, SMB, and NTFS can all vary by volume/configuration).
//! The production implementation asks the live filesystem whether two
//! differently-spelled paths resolve to the same canonical entry. Tests inject
//! the opposite semantics deterministically on any host.

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// Answers whether two differently-spelled paths are aliases for one
/// directory entry. Callers first constrain candidates by case-folded
/// spelling; this seam supplies the filesystem proof.
pub trait FileSystemSemantics: Send + Sync {
    fn same_entry(&self, stored: &Path, observed: &Path) -> bool;

    /// Read metadata through the same injectable filesystem boundary used for
    /// path equivalence. Production delegates to the host filesystem; tests
    /// can fail one lookup deterministically to prove that a partial scan
    /// never treats an unseen indexed path as deleted.
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        std::fs::metadata(path)
    }
}

#[derive(Debug, Default)]
pub struct PlatformFileSystemSemantics;

impl FileSystemSemantics for PlatformFileSystemSemantics {
    fn same_entry(&self, stored: &Path, observed: &Path) -> bool {
        // The scanner does not follow symlink entries. Do not let metadata
        // identity through a final symlink manufacture case-alias evidence.
        // Common system ancestors may themselves be aliases (`/var` on
        // macOS), so rejecting every symlink component would incorrectly
        // disable default-APFS detection.
        if is_symlink(stored) || is_symlink(observed) {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            // `canonicalize` is not an entry-identity primitive on
            // case-insensitive APFS: it can preserve the caller's spelling,
            // so two aliases of one inode compare as different PathBufs.
            // Device + inode proves both lookups reach the same object, but a
            // case-sensitive directory may contain two differently-cased hard
            // links to that inode. Require one physical directory entry for
            // the folded basename as well. That distinguishes an APFS alias
            // from two real hard-link entries without guessing from the OS.
            let (Ok(a), Ok(b)) = (std::fs::metadata(stored), std::fs::metadata(observed)) else {
                return false;
            };
            if a.dev() != b.dev() || a.ino() != b.ino() {
                return false;
            }
            let (Some(stored_parent), Some(observed_parent)) = (stored.parent(), observed.parent())
            else {
                return false;
            };
            let (Ok(a_parent), Ok(b_parent)) = (
                std::fs::metadata(stored_parent),
                std::fs::metadata(observed_parent),
            ) else {
                return false;
            };
            if a_parent.dev() != b_parent.dev() || a_parent.ino() != b_parent.ino() {
                return false;
            }
            one_folded_directory_entry(observed_parent, stored, observed)
        }
        #[cfg(not(unix))]
        match (
            std::fs::canonicalize(stored),
            std::fs::canonicalize(observed),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[cfg(unix)]
fn one_folded_directory_entry(parent: &Path, stored: &Path, observed: &Path) -> bool {
    let (Some(stored_name), Some(observed_name)) = (
        stored.file_name().and_then(|name| name.to_str()),
        observed.file_name().and_then(|name| name.to_str()),
    ) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.eq_ignore_ascii_case(stored_name) || name.eq_ignore_ascii_case(observed_name)
        })
        .take(2)
        .count()
        == 1
}

#[cfg(all(test, unix))]
mod tests {
    use super::{FileSystemSemantics, PlatformFileSystemSemantics};

    #[test]
    fn case_distinct_hard_links_are_not_a_case_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let lower = tmp.path().join("photo.jpg");
        let upper = tmp.path().join("PHOTO.JPG");
        std::fs::write(&lower, b"same inode").unwrap();
        std::fs::hard_link(&lower, &upper).unwrap();

        assert!(!PlatformFileSystemSemantics.same_entry(&lower, &upper));
    }

    #[test]
    fn final_symlink_is_not_case_alias_evidence() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("photo.jpg");
        let alias = tmp.path().join("PHOTO.JPG");
        std::fs::write(&target, b"target").unwrap();
        symlink(&target, &alias).unwrap();

        assert!(!PlatformFileSystemSemantics.same_entry(&target, &alias));
    }
}
