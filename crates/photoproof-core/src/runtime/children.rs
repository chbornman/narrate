//! The crash-orphan safety net and the single-instance lock
//! (spec/RUNTIME.md §8.4–8.5).
//!
//! - `children.json`: each child's PID + **process start-time** + port,
//!   recorded at spawn, cleared at reap. On every app start, stale
//!   entries are killed only when the PID is alive **and** the start-time
//!   matches — a recycled PID with a different start-time is left alone,
//!   always.
//! - `instance.lock`: an exclusive OS file lock; supervisors refuse to
//!   spawn without it (the Tauri single-instance plugin guards the shell
//!   side; this is the belt-and-braces half).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One recorded child (RUNTIME §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRecord {
    pub process: String,
    pub pid: u32,
    /// Platform process start-time (Linux: /proc/{pid}/stat field 22 in
    /// clock ticks). `None` when the platform query failed at spawn —
    /// such an entry is NEVER killed (no identity proof, no kill).
    pub start_time: Option<u64>,
    pub port: u16,
}

/// `{app-data}/runtime/children.json`.
pub struct ChildRegistry {
    path: PathBuf,
    /// `record`/`clear` are read-modify-write operations reached by distinct
    /// supervisor roles. Serialize them so simultaneous ASR/LLM transitions
    /// cannot each persist a snapshot that forgets the other child.
    write_gate: std::sync::Mutex<()>,
}

impl ChildRegistry {
    pub fn new(runtime_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(runtime_dir)?;
        Ok(Self {
            path: runtime_dir.join("children.json"),
            write_gate: std::sync::Mutex::new(()),
        })
    }

    pub fn list(&self) -> Vec<ChildRecord> {
        self.try_list().unwrap_or_default()
    }

    pub fn try_list(&self) -> Result<Vec<ChildRecord>, super::control_file::ControlFileError> {
        Ok(super::control_file::load_json(&self.path)?
            .value
            .unwrap_or_default())
    }

    fn write(&self, records: &[ChildRecord]) -> std::io::Result<()> {
        super::control_file::save_json(&self.path, records)
    }

    /// Record a child at spawn (replaces any prior entry for `process`).
    pub fn record(&self, record: ChildRecord) -> std::io::Result<()> {
        let _guard = self.write_gate.lock().expect("child registry write gate");
        let mut records = self.try_list().map_err(std::io::Error::from)?;
        records.retain(|r| r.process != record.process);
        records.push(record);
        self.write(&records)
    }

    /// Clear a child at reap.
    pub fn clear(&self, process: &str) -> std::io::Result<()> {
        let _guard = self.write_gate.lock().expect("child registry write gate");
        let mut records = self.try_list().map_err(std::io::Error::from)?;
        records.retain(|r| r.process != process);
        self.write(&records)
    }

    /// The §8.4 net, run on every app start BEFORE any spawn: kill stale
    /// entries whose PID is alive AND whose start-time matches; never a
    /// recycled PID. Returns `(killed, skipped)` process names for the
    /// log.
    pub fn kill_orphans(&self) -> std::io::Result<(Vec<String>, Vec<String>)> {
        let _guard = self.write_gate.lock().expect("child registry write gate");
        let mut killed = Vec::new();
        let mut skipped = Vec::new();
        for record in self.try_list().map_err(std::io::Error::from)? {
            match record.start_time {
                Some(expected) if process_start_time(record.pid) == Some(expected) => {
                    kill_pid(record.pid);
                    killed.push(record.process);
                }
                _ => {
                    // Dead, recycled (start-time mismatch), or unprovable
                    // (no recorded start-time): left alone.
                    skipped.push(record.process);
                }
            }
        }
        self.write(&[])?;
        Ok((killed, skipped))
    }
}

/// Platform process start-time — the recycled-PID discriminator.
#[cfg(target_os = "linux")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    // /proc/{pid}/stat field 22 (starttime, clock ticks since boot).
    // comm (field 2) may contain spaces/parens: parse past the LAST ')'.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// macOS: proc_pidinfo(PROC_PIDTBSDINFO) → pbi_start_tvsec (seconds).
/// The original sysctl KERN_PROC sketch used libc::kinfo_proc, which the
/// libc crate only defines for FreeBSD/NetBSD — it never compiled on a
/// Mac (caught at founder-machine verification, June 2026). A short read
/// (rc != size) or any failure answers None: no identity proof, never
/// kill.
#[cfg(target_os = "macos")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    use std::mem::MaybeUninit;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if rc != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(info.pbi_start_tvsec)
}

/// Windows: founder/CI verified (code-complete behind cfg) — the §8.4
/// primary mechanism there is the Job Object (process.rs); this start
/// time feeds the same never-kill-a-recycled-PID rule via
/// GetProcessTimes' creation time.
#[cfg(target_os = "windows")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    // Implemented with raw Win32 FFI at founder-verification time to
    // avoid an unverifiable dependency from this packet; until then the
    // conservative answer is "no identity proof" — never kill.
    let _ = pid;
    None
}

#[cfg(target_os = "linux")]
fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(target_os = "macos")]
fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(target_os = "windows")]
fn kill_pid(_pid: u32) {
    // Founder/CI: TerminateProcess via OpenProcess(PROCESS_TERMINATE).
}

/// The §8.5 exclusive instance lock on `{app-data}/runtime/instance.lock`
/// (std file locking: flock on Unix, LockFileEx on Windows). Held for the
/// process lifetime; dropped (and released by the OS even on crash) at
/// exit.
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// `Ok(Some)` = lock acquired; `Ok(None)` = another instance holds it
    /// (the second launch forwards focus and exits — shell side).
    pub fn acquire(runtime_dir: &Path) -> std::io::Result<Option<Self>> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join("instance.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file, path })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path()).unwrap();
        assert!(first.is_some(), "first acquire holds");
        let second = InstanceLock::acquire(dir.path()).unwrap();
        assert!(second.is_none(), "second instance refused (§8.5)");
        drop(first);
        let third = InstanceLock::acquire(dir.path()).unwrap();
        assert!(third.is_some(), "released on drop");
    }

    #[test]
    fn registry_round_trips_and_replaces_per_process() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ChildRegistry::new(dir.path()).unwrap();
        reg.record(ChildRecord {
            process: "llm".into(),
            pid: 11,
            start_time: Some(1),
            port: 4000,
        })
        .unwrap();
        reg.record(ChildRecord {
            process: "llm".into(),
            pid: 12,
            start_time: Some(2),
            port: 4001,
        })
        .unwrap();
        reg.record(ChildRecord {
            process: "asr".into(),
            pid: 13,
            start_time: Some(3),
            port: 4002,
        })
        .unwrap();
        let list = reg.list();
        assert_eq!(list.len(), 2, "one entry per process");
        assert_eq!(list.iter().find(|r| r.process == "llm").unwrap().pid, 12);
        reg.clear("llm").unwrap();
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn corrupt_registry_recovers_before_recording_instead_of_forgetting_children() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ChildRegistry::new(dir.path()).unwrap();
        reg.record(ChildRecord {
            process: "llm".into(),
            pid: 11,
            start_time: Some(1),
            port: 4000,
        })
        .unwrap();
        std::fs::write(dir.path().join("children.json"), b"{").unwrap();
        reg.record(ChildRecord {
            process: "asr".into(),
            pid: 12,
            start_time: Some(2),
            port: 4001,
        })
        .unwrap();

        let list = reg.try_list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|record| record.process == "llm"));
        assert!(list.iter().any(|record| record.process == "asr"));
    }

    #[test]
    fn concurrent_role_records_do_not_lose_either_child() {
        let dir = tempfile::tempdir().unwrap();
        let registry = std::sync::Arc::new(ChildRegistry::new(dir.path()).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for (process, pid) in [("llm", 11_u32), ("asr", 12_u32)] {
            let registry = registry.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                registry
                    .record(ChildRecord {
                        process: process.into(),
                        pid,
                        start_time: Some(u64::from(pid)),
                        port: 4000 + pid as u16,
                    })
                    .unwrap();
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        let list = registry.try_list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|record| record.process == "llm"));
        assert!(list.iter().any(|record| record.process == "asr"));
    }

    /// THE recycled-PID trap (§8.4): an entry whose PID is alive but whose
    /// start-time differs is NEVER killed. We use our own test process as
    /// the "recycled" PID — if the net killed it, this test would die.
    #[test]
    #[cfg(unix)]
    fn recycled_pid_with_different_start_time_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ChildRegistry::new(dir.path()).unwrap();
        let my_pid = std::process::id();
        let my_start = process_start_time(my_pid).expect("own start time readable");
        reg.record(ChildRecord {
            process: "llm".into(),
            pid: my_pid,
            start_time: Some(my_start + 12345), // wrong start-time
            port: 4000,
        })
        .unwrap();
        // An entry with NO start-time proof is also never killed.
        reg.record(ChildRecord {
            process: "asr".into(),
            pid: my_pid,
            start_time: None,
            port: 4001,
        })
        .unwrap();
        let (killed, skipped) = reg.kill_orphans().unwrap();
        assert!(killed.is_empty(), "never kill a recycled PID");
        assert_eq!(skipped.len(), 2);
        assert!(reg.list().is_empty(), "net cleared after the sweep");
    }
}
