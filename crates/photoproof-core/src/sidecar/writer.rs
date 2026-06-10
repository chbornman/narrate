//! The sidecar write path: debounce state machine (§9.1), atomic
//! mtime-stable replacement (§9.2), partial-write recovery (§9.3), and the
//! transient-failure backoff schedule (§9.4).
//!
//! The debouncer is a pure state machine driven by explicit `now` values —
//! no wall-clock sleeps anywhere; the caller (engine/tests) supplies time.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

use ulid::Ulid;

use crate::id::{ContentHash, UtcMillis};

use super::paths::is_temp_orphan;

// ---------------------------------------------------------------------------
// Atomic mtime-stable write (§9.2)
// ---------------------------------------------------------------------------

/// Outcome of an atomic write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// §9.2 step 2: target already holds the identical bytes — no temp
    /// file, no rename, no mtime touch (P11 mtime-stable no-op).
    SkippedIdentical,
    /// Replaced whole via temp + fsync + rename.
    Written,
}

/// Fault-injection points for the §13.3 kill-during-write harness. A
/// simulated kill abandons the operation exactly where a `kill -9` would:
/// no cleanup runs, temp files are left behind, the target is untouched
/// (or already fully renamed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// Killed mid-write: the temp file holds only `partial` bytes.
    DuringTempWrite,
    /// Killed after the temp write, before fsync completed.
    BeforeTempFsync,
    /// Killed after fsync, before the rename.
    BeforeRename,
    /// Killed after the rename, before the parent-directory fsync.
    AfterRename,
}

/// A scripted fault for one write attempt.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub point: FaultPoint,
    /// Bytes actually written to the temp file for `DuringTempWrite`.
    pub partial: usize,
}

/// Error kind used for simulated kills, so tests can distinguish them.
pub const SIMULATED_KILL: io::ErrorKind = io::ErrorKind::Interrupted;

/// §9.2: serialize → compare → temp in same dir → fsync → rename → fsync
/// parent. On any real failure the temp is deleted and the target is left
/// untouched. At every instant the target holds either the previous or the
/// new complete document.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<WriteOutcome> {
    write_atomic_faulty(target, bytes, None)
}

/// `write_atomic` with an optional scripted fault (test harness only; the
/// production path passes `None`).
pub fn write_atomic_faulty(
    target: &Path,
    bytes: &[u8],
    fault: Option<Fault>,
) -> io::Result<WriteOutcome> {
    // Step 2: mtime-stable comparison.
    if let Ok(existing) = fs::read(target)
        && existing == bytes
    {
        // A skipped write still counts as "the same target written" for
        // §9.3 orphan cleanup purposes.
        let _ = clean_temps_for_target(target);
        return Ok(WriteOutcome::SkippedIdentical);
    }

    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Step 3: temp file in the same directory, `.tmp-<8-hex-random>`.
    let rand8 = format!("{:08x}", (Ulid::new().0 & 0xffff_ffff) as u32);
    let tmp = target.with_file_name(format!("{name}.tmp-{rand8}"));

    let kill = |written: &mut fs::File| -> io::Result<WriteOutcome> {
        // Simulated kill -9: abandon everything as-is.
        let _ = written;
        Err(io::Error::new(SIMULATED_KILL, "simulated kill"))
    };

    let result = (|| -> io::Result<WriteOutcome> {
        let mut f = fs::File::create(&tmp)?;
        if let Some(fault) = fault
            && fault.point == FaultPoint::DuringTempWrite
        {
            f.write_all(&bytes[..fault.partial.min(bytes.len())])?;
            return kill(&mut f);
        }
        f.write_all(bytes)?;
        if let Some(fault) = fault
            && fault.point == FaultPoint::BeforeTempFsync
        {
            return kill(&mut f);
        }
        // Step 4: flush + fsync the temp file.
        f.sync_all()?;
        drop(f);
        if let Some(fault) = fault
            && fault.point == FaultPoint::BeforeRename
        {
            return Err(io::Error::new(SIMULATED_KILL, "simulated kill"));
        }
        // Step 5: atomic rename over the target, then fsync the parent dir.
        fs::rename(&tmp, target)?;
        if let Some(fault) = fault
            && fault.point == FaultPoint::AfterRename
        {
            return Err(io::Error::new(SIMULATED_KILL, "simulated kill"));
        }
        fsync_dir(dir)?;
        Ok(WriteOutcome::Written)
    })();

    match result {
        Ok(o) => {
            // §9.3: orphan temps for this target are deleted when the same
            // target is next written.
            let _ = clean_temps_for_target(target);
            Ok(o)
        }
        Err(e) if e.kind() == SIMULATED_KILL => Err(e), // kill -9: no cleanup
        Err(e) => {
            // Step 6: on any failure, delete the temp, leave the target.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(unix)]
fn fsync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Delete `*.tmp-*` orphans belonging to one target (§9.3 "when the same
/// target is next written").
fn clean_temps_for_target(target: &Path) -> io::Result<usize> {
    let Some(dir) = target.parent() else {
        return Ok(0);
    };
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{name}.tmp-");
    let mut n = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let fname = entry.file_name().to_string_lossy().into_owned();
        if fname.starts_with(&prefix) && fs::remove_file(entry.path()).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// §9.3: delete orphaned `*.photoproof.json.tmp-*` files modified before
/// `cutoff` (the engine passes now − 1 h). Non-recursive: one directory.
pub fn clean_orphan_temps(dir: &Path, cutoff: SystemTime) -> io::Result<usize> {
    let mut n = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_temp_orphan(&name) {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified());
        if let Ok(m) = modified
            && m < cutoff
            && fs::remove_file(entry.path()).is_ok()
        {
            n += 1;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Debounce state machine (§9.1) + transient backoff (§9.4)
// ---------------------------------------------------------------------------

/// §9.1: debounce window 2 s of write-quiet per image, hard cap 5 s from
/// the first unflushed event (the "sidecar within 5 s of capture" promise).
pub const DEBOUNCE_QUIET_MS: i64 = 2_000;
pub const DEBOUNCE_CAP_MS: i64 = 5_000;

/// §9.4 transient-failure backoff: 1 s, 5 s, 30 s, 5 min, then once per
/// reconciliation scan (modeled as the last step repeating).
const BACKOFF_MS: [i64; 4] = [1_000, 5_000, 30_000, 300_000];

#[derive(Debug, Clone)]
struct DebounceEntry {
    /// First unflushed event for this image (drives the 5 s cap).
    first: i64,
    /// Last write activity (drives the 2 s quiet window).
    last: i64,
    /// Immediate-flush trigger (§9.1: redaction, session end, shutdown,
    /// export, overflow→adjacent migration).
    immediate: bool,
    /// Transient-failure attempts (§9.4).
    attempts: u32,
    /// Earliest retry time after a transient failure.
    retry_at: Option<i64>,
    pub last_error: Option<String>,
}

/// Per-image debounce/coalescing state. Pure: all methods take `now`.
#[derive(Debug, Default)]
pub struct Debouncer {
    entries: BTreeMap<ContentHash, DebounceEntry>,
    quiet_ms: i64,
    cap_ms: i64,
}

impl Debouncer {
    pub fn new() -> Self {
        Self::with_windows(DEBOUNCE_QUIET_MS, DEBOUNCE_CAP_MS)
    }

    pub fn with_windows(quiet_ms: i64, cap_ms: i64) -> Self {
        Self {
            entries: BTreeMap::new(),
            quiet_ms,
            cap_ms,
        }
    }

    /// An event was appended for this image.
    pub fn note_event(&mut self, image: &ContentHash, now: UtcMillis) {
        let now = now.epoch_ms();
        let e = self
            .entries
            .entry(image.clone())
            .or_insert_with(|| DebounceEntry {
                first: now,
                last: now,
                immediate: false,
                attempts: 0,
                retry_at: None,
                last_error: None,
            });
        e.last = now;
    }

    /// Immediate-flush trigger (§9.1) — bypasses the debounce windows.
    pub fn note_immediate(&mut self, image: &ContentHash, now: UtcMillis) {
        let now = now.epoch_ms();
        let e = self
            .entries
            .entry(image.clone())
            .or_insert_with(|| DebounceEntry {
                first: now,
                last: now,
                immediate: true,
                attempts: 0,
                retry_at: None,
                last_error: None,
            });
        e.immediate = true;
    }

    /// Crash recovery / cross-process appends: register durable
    /// `sidecar_dirty` rows this debouncer has not seen. `since_ts` from the
    /// row is the first-dirty time, so the 5 s cap applies from the original
    /// mark. `redaction` rows are immediate (§11 step 3).
    pub fn sync_from_queue(&mut self, rows: &[crate::store::DirtyImage], now: UtcMillis) {
        for row in rows {
            let immediate = row.reason == crate::store::DirtyReason::Redaction;
            let e = self
                .entries
                .entry(row.image.clone())
                .or_insert_with(|| DebounceEntry {
                    first: row.since_ts.epoch_ms(),
                    last: now.epoch_ms(),
                    immediate: false,
                    attempts: 0,
                    retry_at: None,
                    last_error: None,
                });
            if immediate {
                e.immediate = true;
            }
        }
    }

    /// Images due for flushing at `now`.
    pub fn due(&self, now: UtcMillis) -> Vec<ContentHash> {
        let now = now.epoch_ms();
        self.entries
            .iter()
            .filter(|(_, e)| {
                if let Some(r) = e.retry_at
                    && now < r
                {
                    return false;
                }
                e.immediate || now - e.last >= self.quiet_ms || now - e.first >= self.cap_ms
            })
            .map(|(h, _)| h.clone())
            .collect()
    }

    /// Everything pending, regardless of windows (shutdown/export drain).
    pub fn drain_all(&self) -> Vec<ContentHash> {
        self.entries.keys().cloned().collect()
    }

    pub fn is_pending(&self, image: &ContentHash) -> bool {
        self.entries.contains_key(image)
    }

    /// Successful flush: clear the entry.
    pub fn clear(&mut self, image: &ContentHash) {
        self.entries.remove(image);
    }

    /// Transient failure (§9.4): schedule the next retry on the backoff
    /// ladder; the entry (and the durable dirty row) persists.
    pub fn note_failure(&mut self, image: &ContentHash, now: UtcMillis, error: String) {
        let now = now.epoch_ms();
        if let Some(e) = self.entries.get_mut(image) {
            let idx = (e.attempts as usize).min(BACKOFF_MS.len() - 1);
            e.retry_at = Some(now + BACKOFF_MS[idx]);
            e.attempts += 1;
            e.last_error = Some(error);
        }
    }

    pub fn attempts(&self, image: &ContentHash) -> u32 {
        self.entries.get(image).map(|e| e.attempts).unwrap_or(0)
    }
}
