//! Startup / scheduled reconciliation scan (§7.3) plus the uniform
//! clock-shift detection (DECISIONS P10).
//!
//! Contract, restated:
//! - The size+mtime fast path covers ≥ 99% of a stable library: known +
//!   match → nothing beyond the `stat` (2 s mtime tolerance on FAT/exFAT).
//! - Re-hash happens exactly when: the path is new to the index, size
//!   differs, mtime differs beyond tolerance and the clock-shift rule did
//!   not absorb it, or a clock-shift confirmation sample. Never otherwise —
//!   a 50k-file no-change scan does zero hashing (§13.12, asserted via the
//!   hashing instrumentation).
//! - FAT/exFAT store local time: a DST transition shifts every mtime by the
//!   same round-hour delta. > 25% of a root's files shifted by one such
//!   delta (±2 s, sizes unchanged) = clock-shift event: stored mtimes updated
//!   in place, a random sample of 16 re-hashed to confirm, never a re-hash
//!   storm. Any sample mismatch aborts the shortcut.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use rayon::prelude::*;
use rusqlite::params;
use walkdir::WalkDir;

use crate::id::ContentHash;

use super::exclusions::{
    MAX_FILE_BYTES, classify_extension, is_excluded_dir_name, is_excluded_file_name,
};
use super::ingest::{self, PRIORITY_SCAN};
use super::{CancelFlag, Library, LibraryError, hashing, mtime_ns_of, paths, rel_path_str};

/// Clock-shift confirmation sample size (§7.3).
const CLOCK_SHIFT_SAMPLE: usize = 16;
const HOUR_NS: i64 = 3_600 * 1_000_000_000;
const CLOCK_SHIFT_TOLERANCE_NS: i64 = 2 * 1_000_000_000;
/// §7.3 gate: the shifted group must STRICTLY exceed 1/4 ("more than
/// 25%") of the root's files. Encoded multiplicatively
/// (`members * 4 <= loaded` rejects) to stay in exact integer math.
const CLOCK_SHIFT_MIN_DENOMINATOR: usize = 4;
/// Parallel-hash batch: hashes are computed on the §1.2 pool, results
/// applied to the DB sequentially between batches (cancel checks ride the
/// batch boundary).
const HASH_BATCH: usize = 64;
/// §7.3 step 4: one `last_verified_at` transaction per few thousand rows.
const VERIFY_BATCH: usize = 4_000;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Priority for M1 pass enqueues (§10.3): P1 for reconciliation/initial
    /// scans; the watcher uses P0 through its own path.
    pub priority: i64,
    pub cancel: Option<CancelFlag>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            priority: PRIORITY_SCAN,
            cancel: None,
        }
    }
}

/// The §7.3 clock-shift event, reported for the debug panel and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockShiftReport {
    /// The uniform round-hour delta, in hours (signed).
    pub delta_hours: i64,
    /// Rows whose stored `mtime_ns` was updated in place.
    pub rows_updated: usize,
    /// Confirmation hashes performed (≤ 16).
    pub samples_hashed: usize,
    /// A sample mismatched: the shortcut was abandoned and every mismatch
    /// fell back to ordinary per-file handling.
    pub aborted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Files observed on disk (after exclusions).
    pub files_seen: usize,
    /// Known + size & mtime match: nothing beyond the stat.
    pub fast_path: usize,
    /// Files whose bytes were hashed during this scan (any reason).
    pub rehashed: usize,
    /// Known rows updated in place (same hash, new size/mtime).
    pub updated: usize,
    /// Path rows inserted/reactivated for already-known hashes.
    pub relinked: usize,
    pub new_images: usize,
    /// §1.3 in-place overwrites.
    pub superseded: usize,
    /// Loaded rows not seen on disk → stale.
    pub went_stale: usize,
    /// Stale rows flipped `deleted` → `moved` by batch correlation.
    pub moved: usize,
    pub placeholders: usize,
    pub excluded_files: usize,
    pub skipped_non_utf8: usize,
    pub skipped_oversize: usize,
    pub io_errors: usize,
    pub clock_shift: Option<ClockShiftReport>,
    pub cancelled: bool,
}

/// A loaded `active` path row (the §7.3 step-1 map entry).
#[derive(Debug, Clone)]
struct LoadedRow {
    path_id: String,
    size: i64,
    mtime_ns: i64,
    image_hash: String,
}

/// A file observed on disk during the walk.
#[derive(Debug)]
struct WalkedFile {
    rel: String,
    abs: PathBuf,
    size: i64,
    mtime_ns: i64,
}

/// A known row whose size or mtime no longer matches.
#[derive(Debug)]
struct Mismatch {
    file: WalkedFile,
    row: LoadedRow,
}

enum HashJob {
    Mismatch(Mismatch),
    New(WalkedFile),
}

impl HashJob {
    fn size(&self) -> i64 {
        match self {
            HashJob::Mismatch(m) => m.file.size,
            HashJob::New(f) => f.size,
        }
    }

    fn abs(&self) -> &PathBuf {
        match self {
            HashJob::Mismatch(m) => &m.file.abs,
            HashJob::New(f) => &f.abs,
        }
    }
}

/// The §1.2 file-level hashing pool: `min(physical_cores, 8)`.
fn hash_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(hashing::hash_pool_size())
            .thread_name(|i| format!("pp-hash-{i}"))
            .build()
            .expect("hash pool")
    })
}

fn cancelled(opts: &ScanOptions) -> bool {
    opts.cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
}

pub(crate) fn scan_root(
    lib: &Library,
    root_id: &str,
    opts: &ScanOptions,
) -> Result<ScanReport, LibraryError> {
    let (root, volume, dir) = lib.root_location(root_id)?;
    let mount = PathBuf::from(volume.mount_point.clone().expect("online volume"));
    let tolerance_ns = lib.mtime_tolerance_ns(volume.fs_type.as_deref());
    let scan_start = lib.now();
    let mut report = ScanReport::default();

    // 1. Load all active path rows of this root into a map.
    let mut known: HashMap<String, LoadedRow> = {
        let conn = lib.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT rel_path, path_id, size, mtime_ns, image_hash
             FROM paths WHERE root_id = ?1 AND state = 'active'",
        )?;
        let rows = stmt.query_map(params![root_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                LoadedRow {
                    path_id: r.get(1)?,
                    size: r.get(2)?,
                    mtime_ns: r.get(3)?,
                    image_hash: r.get(4)?,
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let loaded_count = known.len();

    // Placeholder sentinels from earlier scans: a hydrated path clears its
    // row even when it lands on the fast path (§5.2 re-check).
    let sentinels: HashSet<String> = {
        let conn = lib.db.lock().expect("poisoned");
        ingest::placeholder_sentinels(&conn)?
    };

    // 2. Walk the filesystem, exclusions applied.
    let mut fast_rows: Vec<String> = Vec::new();
    let mut mismatches: Vec<Mismatch> = Vec::new();
    let mut unknown: Vec<WalkedFile> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hydrated: Vec<String> = Vec::new();

    let walker = WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            match e.file_name().to_str() {
                Some(name) => !is_excluded_dir_name(name),
                None => false, // non-UTF-8 directory: cannot index anything below
            }
        });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                report.io_errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue; // dirs handled above; symlinks are not followed (§3)
        }
        let Some(name) = entry.file_name().to_str() else {
            report.skipped_non_utf8 += 1;
            lib.log(format!(
                "skipped non-UTF-8 filename under {}",
                dir.display()
            ));
            continue;
        };
        if is_excluded_file_name(name) || classify_extension(name).is_none() {
            report.excluded_files += 1;
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                report.io_errors += 1;
                continue;
            }
        };
        if meta.len() > MAX_FILE_BYTES {
            report.skipped_oversize += 1;
            lib.log(format!(
                "skipped over-2GiB file {} ({} bytes)",
                entry.path().display(),
                meta.len()
            ));
            continue;
        }
        let Some(rel) = rel_path_str(&mount, entry.path()) else {
            report.skipped_non_utf8 += 1;
            lib.log(format!("skipped non-UTF-8 path {}", entry.path().display()));
            continue;
        };
        if lib.placeholders.is_placeholder(entry.path(), &meta) {
            // §5.2 / P11: never read, never hashed; deferred state visible in
            // the pass counters; re-checked at the next reconciliation.
            lib.record_placeholder(&volume.volume_id, &rel)?;
            report.placeholders += 1;
            // A previously ingested row that went dataless still exists on
            // disk; it must not be staled as deleted.
            if known.contains_key(&rel) {
                seen.insert(rel);
            }
            continue;
        }
        report.files_seen += 1;
        if !sentinels.is_empty() {
            let s = ingest::placeholder_sentinel(&volume.volume_id, &rel);
            if sentinels.contains(s.as_str()) {
                hydrated.push(s.as_str().to_owned());
            }
        }
        let size = meta.len() as i64;
        let mtime_ns = mtime_ns_of(&meta);
        let file = WalkedFile {
            rel: rel.clone(),
            abs: entry.into_path(),
            size,
            mtime_ns,
        };
        seen.insert(rel.clone());
        match known.get(&rel) {
            Some(row) if row.size == size && (row.mtime_ns - mtime_ns).abs() <= tolerance_ns => {
                fast_rows.push(row.path_id.clone());
                report.fast_path += 1;
            }
            Some(row) => mismatches.push(Mismatch {
                file,
                row: row.clone(),
            }),
            None => unknown.push(file),
        }
    }

    // 3. Uniform clock-shift detection — BEFORE any mismatch re-hashing.
    if let Some(shift) = detect_clock_shift(lib, loaded_count, &mut mismatches, &mut report)? {
        report.clock_shift = Some(shift);
    }

    // 4. Unknown paths: the §5 re-registration fast path first (a
    //    `root-removed` stale row at the same location with matching
    //    size+mtime relinks with zero hashing).
    let mut to_hash: Vec<HashJob> = Vec::new();
    let mut relinked_hashes: Vec<ContentHash> = Vec::new();
    for file in unknown {
        if cancelled(opts) {
            report.cancelled = true;
            return Ok(report);
        }
        if let Some(stale) = lib.reactivatable_row(
            &volume.volume_id,
            &file.rel,
            file.size,
            file.mtime_ns,
            tolerance_ns,
        )? {
            {
                let conn = lib.db.lock().expect("poisoned");
                paths::reactivate(&conn, &stale.path_id, root_id, lib.now())?;
            }
            report.relinked += 1;
            relinked_hashes.push(stale.image_hash);
        } else {
            to_hash.push(HashJob::New(file));
        }
    }
    to_hash.extend(mismatches.into_iter().map(HashJob::Mismatch));

    // 5. Hash queue in ascending size order (§1.2: the grid populates
    //    quickly), file-level parallelism on the §1.2 pool, DB application
    //    sequential.
    to_hash.sort_by_key(HashJob::size);
    for batch in to_hash.chunks(HASH_BATCH) {
        if cancelled(opts) {
            report.cancelled = true;
            return Ok(report);
        }
        let hashed: Vec<std::io::Result<(ContentHash, u64)>> = hash_pool().install(|| {
            batch
                .par_iter()
                .map(|j| hashing::hash_file(j.abs()))
                .collect()
        });
        for (job, result) in batch.iter().zip(hashed) {
            let (hash, hashed_size) = match result {
                Ok(h) => h,
                Err(e) => {
                    // Vanished or unreadable mid-scan: transient; the next
                    // scan or watcher event recovers.
                    report.io_errors += 1;
                    lib.log(format!("hash failed for {}: {e}", job.abs().display()));
                    continue;
                }
            };
            report.rehashed += 1;
            match job {
                HashJob::Mismatch(m) => {
                    if hash.as_str() == m.row.image_hash {
                        let conn = lib.db.lock().expect("poisoned");
                        paths::update_size_mtime(
                            &conn,
                            &m.row.path_id,
                            hashed_size as i64,
                            m.file.mtime_ns,
                            lib.now(),
                        )?;
                        report.updated += 1;
                    } else {
                        lib.supersede_tx(
                            &m.row.path_id,
                            &hash,
                            hashed_size as i64,
                            m.file.mtime_ns,
                            &volume.volume_id,
                            Some(root_id),
                            &m.file.rel,
                            opts.priority,
                        )?;
                        report.superseded += 1;
                    }
                }
                HashJob::New(f) => {
                    if lib.image_exists(&hash)? {
                        lib.relink_tx(
                            &hash,
                            &volume.volume_id,
                            Some(root_id),
                            &f.rel,
                            hashed_size as i64,
                            f.mtime_ns,
                            scan_start,
                        )?;
                        report.relinked += 1;
                        relinked_hashes.push(hash);
                    } else {
                        lib.new_image_tx(
                            &hash,
                            hashed_size as i64,
                            &volume.volume_id,
                            Some(root_id),
                            &f.rel,
                            f.mtime_ns,
                            opts.priority,
                        )?;
                        report.new_images += 1;
                    }
                }
            }
        }
    }

    // 6. Loaded rows not seen → stale (`deleted`), one transaction.
    known.retain(|rel, _| !seen.contains(rel));
    if !known.is_empty() {
        let stale_since = lib.now().to_rfc3339();
        let mut conn = lib.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        for row in known.values() {
            tx.execute(
                "UPDATE paths SET state = 'stale', stale_reason = 'deleted', stale_since = ?2
                 WHERE path_id = ?1 AND state = 'active'",
                params![row.path_id, stale_since],
            )?;
        }
        tx.commit()?;
        report.went_stale = known.len();
    }

    // 7. Batch move-correlation: a new path whose hash matches an image that
    //    just lost a path is a move, regardless of walk order.
    if !relinked_hashes.is_empty() {
        let conn = lib.db.lock().expect("poisoned");
        for hash in &relinked_hashes {
            report.moved += paths::correlate_move(&conn, hash, scan_start)?;
        }
    }

    // 8. Batch `last_verified_at` for fast-path rows (one transaction per few
    //    thousand rows; per-row writes would dominate the scan).
    for chunk in fast_rows.chunks(VERIFY_BATCH) {
        let now = lib.now().to_rfc3339();
        let mut conn = lib.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        for path_id in chunk {
            tx.execute(
                "UPDATE paths SET last_verified_at = ?2 WHERE path_id = ?1",
                params![path_id, now],
            )?;
        }
        tx.commit()?;
    }

    // 9. Hydrated placeholders that landed on the fast path still clear
    //    their sentinel rows.
    if !hydrated.is_empty() {
        let conn = lib.db.lock().expect("poisoned");
        ingest::clear_placeholder_sentinels(&conn, &hydrated)?;
    }

    let _ = root; // identity is carried by root_id; record kept for clarity
    Ok(report)
}

/// §7.3 clock-shift detection. Mutates `mismatches` (absorbed rows are
/// removed) and updates the matching rows' stored `mtime_ns` in place.
fn detect_clock_shift(
    lib: &Library,
    loaded_count: usize,
    mismatches: &mut Vec<Mismatch>,
    report: &mut ScanReport,
) -> Result<Option<ClockShiftReport>, LibraryError> {
    if loaded_count == 0 || mismatches.is_empty() {
        return Ok(None);
    }
    // Group size-unchanged mismatches by their round-hour delta (±2 s).
    let mut groups: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, m) in mismatches.iter().enumerate() {
        if m.file.size != m.row.size {
            continue;
        }
        let delta = m.file.mtime_ns - m.row.mtime_ns;
        let hours = (delta + HOUR_NS / 2).div_euclid(HOUR_NS);
        if hours == 0 {
            continue;
        }
        if (delta - hours * HOUR_NS).abs() <= CLOCK_SHIFT_TOLERANCE_NS {
            groups.entry(hours).or_default().push(i);
        }
    }
    let Some((&hours, members)) = groups.iter().max_by_key(|(_, v)| v.len()) else {
        return Ok(None);
    };
    // "More than 25% of a root's files."
    if members.len() * CLOCK_SHIFT_MIN_DENOMINATOR <= loaded_count {
        return Ok(None);
    }
    // Re-hash a random sample of 16 to confirm bytes are unchanged.
    let mut sample: Vec<usize> = members.clone();
    fastrand::shuffle(&mut sample);
    sample.truncate(CLOCK_SHIFT_SAMPLE);
    let mut samples_hashed = 0;
    for &i in &sample {
        let m = &mismatches[i];
        samples_hashed += 1;
        report.rehashed += 1;
        let confirmed = matches!(
            hashing::hash_file(&m.file.abs),
            Ok((h, _)) if h.as_str() == m.row.image_hash
        );
        if !confirmed {
            // Any sample mismatch aborts the shortcut: ordinary per-file
            // mismatch handling takes over for everything.
            lib.log(format!(
                "clock-shift candidate ({hours:+}h, {} files) aborted: sample {} mismatched",
                members.len(),
                m.file.rel
            ));
            return Ok(Some(ClockShiftReport {
                delta_hours: hours,
                rows_updated: 0,
                samples_hashed,
                aborted: true,
            }));
        }
    }
    // Confirmed: update stored mtimes in place, one transaction; absorb the
    // members out of the mismatch list (they are fast-path rows now).
    let member_set: HashSet<usize> = members.iter().copied().collect();
    {
        let now = lib.now().to_rfc3339();
        let mut conn = lib.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        for &i in members {
            let m = &mismatches[i];
            tx.execute(
                "UPDATE paths SET mtime_ns = ?2, last_verified_at = ?3 WHERE path_id = ?1",
                params![m.row.path_id, m.file.mtime_ns, now],
            )?;
        }
        tx.commit()?;
    }
    let rows_updated = member_set.len();
    let mut idx = 0;
    mismatches.retain(|_| {
        let keep = !member_set.contains(&idx);
        idx += 1;
        keep
    });
    report.fast_path += rows_updated;
    lib.log(format!(
        "clock-shift event: {hours:+}h across {rows_updated} files; stored mtimes updated in \
         place, {samples_hashed} samples confirmed, no re-hash storm (§7.3 / P10)"
    ));
    Ok(Some(ClockShiftReport {
        delta_hours: hours,
        rows_updated,
        samples_hashed,
        aborted: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_hour_grouping_math() {
        // +1 h exactly, +1 h within 2 s, and a non-round delta.
        let delta = HOUR_NS;
        let hours = (delta + HOUR_NS / 2).div_euclid(HOUR_NS);
        assert_eq!(hours, 1);
        assert!((delta - hours * HOUR_NS).abs() <= CLOCK_SHIFT_TOLERANCE_NS);

        let delta = HOUR_NS + 1_900_000_000;
        let hours = (delta + HOUR_NS / 2).div_euclid(HOUR_NS);
        assert_eq!(hours, 1);
        assert!((delta - hours * HOUR_NS).abs() <= CLOCK_SHIFT_TOLERANCE_NS);

        let delta = HOUR_NS / 2;
        let hours = (delta + HOUR_NS / 2).div_euclid(HOUR_NS);
        assert_eq!(hours, 1);
        assert!((delta - hours * HOUR_NS).abs() > CLOCK_SHIFT_TOLERANCE_NS);

        // -1 h (DST the other way).
        let delta = -HOUR_NS + 500_000_000;
        let hours = (delta + HOUR_NS / 2).div_euclid(HOUR_NS);
        assert_eq!(hours, -1);
        assert!((delta - hours * HOUR_NS).abs() <= CLOCK_SHIFT_TOLERANCE_NS);
    }
}
