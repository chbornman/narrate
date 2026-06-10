//! The hashing pipeline: BLAKE3-256 of complete file bytes.
//!
//! Contract: spec/LIBRARY.md §1.1–1.2. Files ≥ 1 MiB are memory-mapped;
//! BLAKE3-internal rayon only for files ≥ 64 MiB (avoids oversubscription
//! against file-level parallelism). Every invocation is counted so tests can
//! assert the §7.3 "zero hashing on a no-change scan" and §13.14 "exactly 16
//! sampled files" criteria via instrumentation, not trust.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::id::ContentHash;

const MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MiB
const RAYON_THRESHOLD: u64 = 64 * 1024 * 1024; // 64 MiB

static HASH_INVOCATIONS: AtomicU64 = AtomicU64::new(0);
static HASHED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Number of file-hash invocations since process start (test/debug
/// instrumentation; §13.12, §13.14).
pub fn hash_invocation_count() -> u64 {
    HASH_INVOCATIONS.load(Ordering::Relaxed)
}

/// Total bytes hashed since process start (throughput reporting, §10.6/§12.1).
pub fn hashed_byte_count() -> u64 {
    HASHED_BYTES.load(Ordering::Relaxed)
}

/// Hash a file's complete bytes. Returns the hash and the byte size actually
/// hashed (callers compare it against the stat size to detect races).
pub fn hash_file(path: &Path) -> std::io::Result<(ContentHash, u64)> {
    HASH_INVOCATIONS.fetch_add(1, Ordering::Relaxed);
    let meta = std::fs::metadata(path)?;
    let size = meta.len();
    let mut hasher = blake3::Hasher::new();
    if size >= RAYON_THRESHOLD {
        hasher.update_mmap_rayon(path)?;
    } else if size >= MMAP_THRESHOLD {
        hasher.update_mmap(path)?;
    } else {
        let mut file = std::fs::File::open(path)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let hashed = hasher.count();
    HASHED_BYTES.fetch_add(hashed, Ordering::Relaxed);
    let hash = ContentHash::from_hex(hasher.finalize().to_hex().as_str())
        .expect("blake3 hex is canonical");
    Ok((hash, hashed))
}

/// File-level hashing parallelism: `min(physical_cores, 8)` (§1.2).
/// `available_parallelism` reports logical cores; halving on machines with
/// SMT would mis-count non-SMT machines, so we use it directly and rely on
/// the cap — flagged in the packet report.
pub fn hash_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_reference_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, b"hello photoproof").unwrap();
        let before = hash_invocation_count();
        let (h, size) = hash_file(&p).unwrap();
        assert_eq!(size, 16);
        assert_eq!(h, ContentHash::from_bytes_of(b"hello photoproof"));
        assert_eq!(hash_invocation_count(), before + 1);
    }

    #[test]
    fn mmap_path_matches_buffered_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let data: Vec<u8> = (0..2 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let (h, size) = hash_file(&p).unwrap();
        assert_eq!(size, data.len() as u64);
        assert_eq!(h, ContentHash::from_bytes_of(&data));
    }
}
