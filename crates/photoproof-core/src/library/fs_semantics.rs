//! Filesystem path-equivalence seam.
//!
//! Case sensitivity is a property of the mounted volume, not the host OS or
//! filesystem name (APFS, SMB, and NTFS can all vary by volume/configuration).
//! The production implementation asks the live filesystem whether two
//! differently-spelled paths resolve to the same canonical entry. Tests inject
//! the opposite semantics deterministically on any host.

use std::fs::Metadata;
use std::io;
use std::path::{Component, Path, PathBuf};

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
        // Library roots never follow symlinks. Canonical equality through a
        // symlink is therefore not evidence of case-insensitive aliasing.
        if contains_symlink(stored) || contains_symlink(observed) {
            return false;
        }
        match (
            std::fs::canonicalize(stored),
            std::fs::canonicalize(observed),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

fn contains_symlink(path: &Path) -> bool {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                prefix.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Normal(_) => prefix.push(component.as_os_str()),
        }
        if std::fs::symlink_metadata(&prefix)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return true;
        }
    }
    false
}
