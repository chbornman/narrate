//! The hashing pipeline: BLAKE3-256 of complete file bytes.
//!
//! Contract: spec/LIBRARY.md §1.1–1.2. Files ≥ 1 MiB are memory-mapped;
//! BLAKE3-internal rayon only for files ≥ 64 MiB (avoids oversubscription
//! against file-level parallelism). Every invocation is counted so tests can
//! assert the §7.3 "zero hashing on a no-change scan" and §13.14 "exactly 16
//! sampled files" criteria via instrumentation, not trust.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::PauseToken;
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

/// Hash with cooperative pause/cancel checks every 64 KiB. A cancelled hash
/// returns `Ok(None)` and never publishes a partial digest. The ordinary fast
/// mmap path remains in use when no control signal is supplied.
pub fn hash_file_controlled(
    path: &Path,
    cancel: Option<&AtomicBool>,
    pause: Option<&PauseToken>,
) -> std::io::Result<Option<(ContentHash, u64)>> {
    if cancel.is_none() && pause.is_none() {
        return hash_file(path).map(Some);
    }
    HASH_INVOCATIONS.fetch_add(1, Ordering::Relaxed);
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Ok(None);
        }
        if pause.is_some_and(|token| !token.wait_until_resumed(cancel)) {
            return Ok(None);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hashed = hasher.count();
    HASHED_BYTES.fetch_add(hashed, Ordering::Relaxed);
    let hash = ContentHash::from_hex(hasher.finalize().to_hex().as_str())
        .expect("blake3 hex is canonical");
    Ok(Some((hash, hashed)))
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
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn hash_matches_reference_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, b"hello photoproof").unwrap();
        let before = hash_invocation_count();
        let (h, size) = hash_file(&p).unwrap();
        assert_eq!(size, 16);
        assert_eq!(h, ContentHash::from_bytes_of(b"hello photoproof"));
        assert!(
            hash_invocation_count() > before,
            "this call advances the process counter (parallel hash tests may also advance it)"
        );
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

    #[test]
    fn controlled_hash_suspends_and_resumes_without_losing_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paused.bin");
        std::fs::write(&path, vec![7u8; 256 * 1024]).unwrap();
        let expected = ContentHash::from_bytes_of(&vec![7u8; 256 * 1024]);
        let pause = PauseToken::new(true);
        let worker_pause = pause.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(hash_file_controlled(&path, None, Some(&worker_pause)))
                .unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "no bytes are hashed through a live pause"
        );
        pause.set_paused(false);
        let (hash, bytes) = rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(hash, expected);
        assert_eq!(bytes, 256 * 1024);
        worker.join().unwrap();
    }

    #[test]
    fn cancellation_breaks_a_paused_hash_without_a_partial_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cancelled.bin");
        std::fs::write(&path, vec![3u8; 128 * 1024]).unwrap();
        let pause = PauseToken::new(true);
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(
            hash_file_controlled(&path, Some(&cancel), Some(&pause))
                .unwrap()
                .is_none()
        );
    }
}
