//! Log capture (spec/RUNTIME.md §8.6): child stdout/stderr piped to
//! `{app-data}/logs/runtime/{process}.log`, rotated at 5 MB × 5 files;
//! supervisor decisions log to the same directory. Release builds keep
//! writing these (a support artifact); only the debug-panel UI is
//! dev-gated.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_MAX_FILES: u32 = 5;

/// A line-oriented rotating log file: `{name}.log`, rotating into
/// `{name}.log.1` … `{name}.log.{max_files - 1}`.
#[derive(Clone)]
pub struct RotatingLog {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    max_bytes: u64,
    max_files: u32,
    file: Option<File>,
    written: u64,
}

impl RotatingLog {
    /// `path` is the live file (e.g. `…/logs/runtime/llm.log`); parents
    /// are created.
    pub fn open(path: PathBuf, max_bytes: u64, max_files: u32) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path,
                max_bytes,
                max_files: max_files.max(1),
                file: Some(file),
                written,
            })),
        })
    }

    pub fn open_default(path: PathBuf) -> std::io::Result<Self> {
        Self::open(path, DEFAULT_MAX_BYTES, DEFAULT_MAX_FILES)
    }

    /// Append one line (newline added). Rotation errors are swallowed
    /// after a best effort — logging never takes the runtime down.
    pub fn line(&self, text: &str) {
        let mut inner = self.inner.lock().expect("log mutex");
        let len = text.len() as u64 + 1;
        if inner.written + len > inner.max_bytes {
            let _ = inner.rotate();
        }
        if let Some(f) = inner.file.as_mut()
            && writeln!(f, "{text}").is_ok()
        {
            inner.written += len;
        }
    }

    pub fn path(&self) -> PathBuf {
        self.inner.lock().expect("log mutex").path.clone()
    }
}

impl Inner {
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        // Shift name.log.(n-1) ← … ← name.log.1 ← name.log
        let rotated = |n: u32| {
            let mut p = self.path.clone().into_os_string();
            p.push(format!(".{n}"));
            PathBuf::from(p)
        };
        let _ = std::fs::remove_file(rotated(self.max_files - 1));
        for n in (1..self.max_files - 1).rev() {
            let _ = std::fs::rename(rotated(n), rotated(n + 1));
        }
        if self.max_files > 1 {
            let _ = std::fs::rename(&self.path, rotated(1));
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.written = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_at_the_byte_cap_and_keeps_at_most_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proc.log");
        let log = RotatingLog::open(path.clone(), 64, 3).unwrap();
        for i in 0..40 {
            log.line(&format!("decision line {i:04}"));
        }
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"proc.log".to_owned()));
        assert!(names.contains(&"proc.log.1".to_owned()));
        assert!(names.contains(&"proc.log.2".to_owned()));
        assert!(
            !names.contains(&"proc.log.3".to_owned()),
            "max 3 files total"
        );
        // Every surviving file respects the cap (+1 line of slack).
        for n in &names {
            let len = std::fs::metadata(dir.path().join(n)).unwrap().len();
            assert!(len <= 64 + 32, "{n} is {len} bytes");
        }
    }

    #[test]
    fn reopens_and_appends_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.log");
        RotatingLog::open_default(path.clone()).unwrap().line("one");
        RotatingLog::open_default(path.clone()).unwrap().line("two");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "one\ntwo\n");
    }
}
