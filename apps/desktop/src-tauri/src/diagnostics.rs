//! Retained launch diagnostics.
//!
//! The active log is rotated before a new subscriber opens it, so relaunching
//! after a crash cannot erase the only useful evidence. A launch marker is
//! removed only after the coordinated shutdown barrier and final persistence
//! steps complete; finding it on the next launch is therefore an explicit
//! previous-unclean-launch signal.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

const RETAINED_LOGS: usize = 8;
const CURRENT_LOG: &str = "photoproof.log";
const LAUNCH_MARKER: &str = "launch-in-progress";

#[derive(Debug, Clone)]
pub struct CrashDiagnostics {
    pub logs_dir: PathBuf,
    pub current_log: PathBuf,
    pub previous_unclean_launch: bool,
    marker_path: PathBuf,
}

pub struct PreparedDiagnostics {
    pub diagnostics: CrashDiagnostics,
    pub log_file: File,
}

impl CrashDiagnostics {
    pub fn mark_clean_shutdown(&self) -> io::Result<()> {
        match fs::remove_file(&self.marker_path) {
            Ok(()) => sync_parent(&self.marker_path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

pub fn prepare(app_data: &Path) -> io::Result<PreparedDiagnostics> {
    let logs_dir = app_data.join("logs");
    fs::create_dir_all(&logs_dir)?;
    let current_log = logs_dir.join(CURRENT_LOG);
    rotate_current_log(&logs_dir, &current_log)?;
    prune_rotated_logs(&logs_dir)?;

    let marker_path = app_data.join(LAUNCH_MARKER);
    let previous_unclean_launch = marker_path.exists();
    write_launch_marker(&marker_path)?;
    let log_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&current_log)?;
    Ok(PreparedDiagnostics {
        diagnostics: CrashDiagnostics {
            logs_dir,
            current_log,
            previous_unclean_launch,
            marker_path,
        },
        log_file,
    })
}

fn rotate_current_log(logs_dir: &Path, current_log: &Path) -> io::Result<()> {
    let Some(metadata) = current_log.metadata().ok() else {
        return Ok(());
    };
    if metadata.len() == 0 {
        fs::remove_file(current_log)?;
        return Ok(());
    }
    let suffix = unix_millis(SystemTime::now());
    let mut sequence = 0u32;
    loop {
        let sequence_suffix = if sequence == 0 {
            String::new()
        } else {
            format!("-{sequence}")
        };
        let destination =
            logs_dir.join(format!("photoproof-previous-{suffix}{sequence_suffix}.log"));
        if !destination.exists() {
            fs::rename(current_log, destination)?;
            sync_parent(current_log)?;
            return Ok(());
        }
        sequence = sequence.saturating_add(1);
    }
}

fn prune_rotated_logs(logs_dir: &Path) -> io::Result<()> {
    let mut logs = fs::read_dir(logs_dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            name.to_str()
                .is_some_and(|name| {
                    name.starts_with("photoproof-previous-") && name.ends_with(".log")
                })
                .then(|| {
                    let modified = entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(UNIX_EPOCH);
                    (modified, entry.path())
                })
        })
        .collect::<Vec<_>>();
    logs.sort_by_key(|(modified, path)| (*modified, path.clone()));
    let remove_count = logs.len().saturating_sub(RETAINED_LOGS);
    for (_, path) in logs.into_iter().take(remove_count) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn write_launch_marker(marker_path: &Path) -> io::Result<()> {
    let stamp = unix_millis(SystemTime::now());
    let mut sequence = 0u32;
    let (temporary, mut file) = loop {
        let temporary =
            marker_path.with_extension(format!("tmp-{}-{stamp}-{sequence}", std::process::id()));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                sequence = sequence.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    };
    writeln!(
        file,
        "pid={}\nstarted_at_ms={}\nversion={}",
        std::process::id(),
        unix_millis(SystemTime::now()),
        env!("CARGO_PKG_VERSION")
    )?;
    file.sync_all()?;
    if marker_path.exists() {
        fs::remove_file(marker_path)?;
    }
    fs::rename(&temporary, marker_path)?;
    sync_parent(marker_path)
}

pub fn install_panic_recording(logs_dir: &Path) {
    static INSTALL: Once = Once::new();
    let logs_dir = logs_dir.to_path_buf();
    INSTALL.call_once(move || {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let path = logs_dir.join(format!(
                "panic-{}-{}.log",
                unix_millis(SystemTime::now()),
                std::process::id()
            ));
            if let Ok(mut file) = OpenOptions::new().create_new(true).write(true).open(path) {
                let _ = writeln!(file, "{info}");
                let _ = file.sync_all();
            }
            previous(info);
        }));
    });
}

fn unix_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_rotates_log_and_reports_an_unclean_marker() {
        let dir = tempfile::tempdir().unwrap();
        let first = prepare(dir.path()).unwrap();
        writeln!(&first.log_file, "first launch evidence").unwrap();
        first.log_file.sync_all().unwrap();
        drop(first.log_file);

        let second = prepare(dir.path()).unwrap();
        assert!(second.diagnostics.previous_unclean_launch);
        let previous = fs::read_dir(dir.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("photoproof-previous-")
            })
            .unwrap();
        assert_eq!(
            fs::read_to_string(previous.path()).unwrap(),
            "first launch evidence\n"
        );

        second.diagnostics.mark_clean_shutdown().unwrap();
        drop(second.log_file);
        let third = prepare(dir.path()).unwrap();
        assert!(!third.diagnostics.previous_unclean_launch);
        third.diagnostics.mark_clean_shutdown().unwrap();
    }

    #[test]
    fn empty_active_log_is_not_retained_as_evidence() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        File::create(dir.path().join("logs").join(CURRENT_LOG)).unwrap();
        let prepared = prepare(dir.path()).unwrap();
        let retained = fs::read_dir(dir.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("photoproof-previous-")
            })
            .count();
        assert_eq!(retained, 0);
        prepared.diagnostics.mark_clean_shutdown().unwrap();
    }
}
