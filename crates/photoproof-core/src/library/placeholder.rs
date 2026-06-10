//! Cloud-sync placeholder / dataless file detection.
//!
//! Contract: spec/LIBRARY.md §5.2 (DECISIONS P11). Placeholders MUST be
//! detected at stat level and never hashed — reading one forces a full
//! hydration download. Detection is injectable so the §13.15 acceptance test
//! can simulate `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` / dataless attributes
//! on any platform.

use std::path::Path;

/// Stat-level placeholder detection. Implementations MUST NOT read file
/// bytes — `metadata` is all the evidence allowed.
pub trait PlaceholderDetector: Send + Sync {
    fn is_placeholder(&self, path: &Path, metadata: &std::fs::Metadata) -> bool;
}

/// Platform detection: Windows `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` /
/// `FILE_ATTRIBUTE_OFFLINE`; macOS `SF_DATALESS` in `st_flags`. Linux has no
/// portable dataless flag — always `false` there.
#[derive(Debug, Default)]
pub struct PlatformPlaceholderDetector;

impl PlaceholderDetector for PlatformPlaceholderDetector {
    #[allow(unused_variables)]
    fn is_placeholder(&self, path: &Path, metadata: &std::fs::Metadata) -> bool {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
            const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
            const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
            let attrs = metadata.file_attributes();
            return attrs
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0;
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt;
            const SF_DATALESS: u32 = 0x4000_0000;
            return metadata.st_flags() & SF_DATALESS != 0;
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }
}

/// Test/simulation detector: a shared set of absolute paths currently
/// "dataless". Used by acceptance tests to simulate placeholder stat
/// attributes portably — "hydration" = removing the path from the set, with
/// the file path itself unchanged (exactly how the OS flag behaves).
#[derive(Debug, Default, Clone)]
pub struct SharedSetPlaceholderDetector {
    set: std::sync::Arc<std::sync::RwLock<std::collections::HashSet<std::path::PathBuf>>>,
}

impl SharedSetPlaceholderDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark(&self, path: impl Into<std::path::PathBuf>) {
        self.set.write().expect("poisoned").insert(path.into());
    }

    /// Simulate hydration: the stat-level attribute clears, path unchanged.
    pub fn hydrate(&self, path: &Path) {
        self.set.write().expect("poisoned").remove(path);
    }
}

impl PlaceholderDetector for SharedSetPlaceholderDetector {
    fn is_placeholder(&self, path: &Path, _metadata: &std::fs::Metadata) -> bool {
        self.set.read().expect("poisoned").contains(path)
    }
}
