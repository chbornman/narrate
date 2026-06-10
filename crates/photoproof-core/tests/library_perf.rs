//! Performance smoke checks for spec/LIBRARY.md §12. Ignored by default —
//! budgets are asserted on perf-pass hardware (P4.1 / founder machine); these
//! exist so the numbers are one `--ignored` run away.

use photoproof_core::library::{hash_file, hashed_byte_count};

/// §1.2 hashing throughput: prints MB/s for one large file through the real
/// pipeline path (mmap, rayon above 64 MiB). The ≥ 1 GB/s aggregate budget
/// applies to internal NVMe with file-level parallelism on top (§12).
#[test]
#[ignore = "perf smoke: run explicitly with --ignored --nocapture on target hardware"]
fn p12_hash_throughput_smoke() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.bin");
    let chunk: Vec<u8> = (0..8 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        for _ in 0..32 {
            f.write_all(&chunk).unwrap(); // 256 MiB total
        }
    }
    let before = hashed_byte_count();
    let t0 = std::time::Instant::now();
    let (_hash, size) = hash_file(&path).unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(size, 256 * 1024 * 1024);
    assert_eq!(hashed_byte_count() - before, size);
    let mbps = size as f64 / 1_048_576.0 / elapsed.as_secs_f64();
    println!("single-file hash throughput: {mbps:.0} MB/s ({elapsed:?} for 256 MiB)");
}
