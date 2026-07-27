//! Offline, checksummed app-data snapshot primitives.
//!
//! Live Tauri commands may only arm the private helper in this module. The
//! helper blocks on an inherited pipe until the desktop process has exited, so
//! copying an active WAL or replacing files beneath open SQLite connections is
//! impossible by construction. `docs/BACKUP-RESTORE.md` is the product
//! contract.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const MANIFEST_NAME: &str = "backup-manifest.json";
const PAYLOAD_DIR: &str = "app-data";
const RECEIPT_NAME: &str = "backup-restore-receipt.json";
const HELPER_ARG: &str = "--photoproof-backup-helper";
const HELPER_SMOKE_ARG: &str = "--photoproof-backup-helper-smoke";
const MAX_HELPER_REQUEST_BYTES: usize = 128 * 1024;
static HELPER_STDIN: OnceLock<Mutex<Option<ChildStdin>>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup source does not exist: {0}")]
    MissingSource(PathBuf),
    #[error("backup destination must not already exist: {0}")]
    DestinationExists(PathBuf),
    #[error("backup path is inside its source: {0}")]
    DestinationInsideSource(PathBuf),
    #[error("invalid backup-relative path: {0}")]
    InvalidRelativePath(PathBuf),
    #[error("backup checksum mismatch for {path}: expected {expected}, got {actual}")]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("backup I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("a backup or restore handoff is already waiting for this app to quit")]
    HandoffPending,
    #[error("could not start the offline backup/restore helper: {0}")]
    Helper(String),
    #[error("backup/restore helper request is invalid: {0}")]
    InvalidRequest(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "operation")]
pub enum OfflineOperation {
    Backup {
        app_data: PathBuf,
        destination: PathBuf,
    },
    Restore {
        app_data: PathBuf,
        backup: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationReceipt {
    pub operation: String,
    pub succeeded: bool,
    pub completed_at: String,
    pub backup_path: PathBuf,
    pub rollback_path: Option<PathBuf>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub format: String,
    pub version: u32,
    pub files: Vec<BackupFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
}

/// Start the private offline helper and keep its request pipe open for the
/// lifetime of this process. The helper reads the framed request immediately,
/// then blocks waiting for EOF. OS process teardown closes the retained pipe,
/// which is a stronger "the app is gone" boundary than a timer or a Tauri
/// window event: only then may it copy SQLite/WAL or replace app data.
pub fn spawn_offline_helper(operation: &OfflineOperation) -> Result<(), BackupError> {
    validate_operation(operation)?;
    let slot = HELPER_STDIN.get_or_init(|| Mutex::new(None));
    let mut slot = slot
        .lock()
        .map_err(|_| BackupError::Helper("handoff mutex poisoned".into()))?;
    if slot.is_some() {
        return Err(BackupError::HandoffPending);
    }
    let executable = std::env::current_exe()
        .map_err(|error| BackupError::Helper(format!("resolve executable: {error}")))?;
    let mut child = std::process::Command::new(executable)
        .arg(HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| BackupError::Helper(error.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| BackupError::Helper("helper stdin was not piped".into()))?;
    write_helper_request(&mut stdin, operation)?;
    // Dropping Child deliberately does not terminate it. Retaining ChildStdin
    // in process-global state keeps the helper behind the true process-exit
    // boundary, including on crash. Static storage is closed by the OS, not by
    // Rust unwinding before App's database handles have been destroyed.
    drop(child);
    *slot = Some(stdin);
    Ok(())
}

pub fn is_offline_helper_arg(arg: Option<&std::ffi::OsStr>) -> bool {
    arg == Some(std::ffi::OsStr::new(HELPER_ARG))
}

pub fn is_offline_helper_smoke_arg(arg: Option<&std::ffi::OsStr>) -> bool {
    arg == Some(std::ffi::OsStr::new(HELPER_SMOKE_ARG))
}

/// Private executable entry point. The request exists only on an inherited
/// anonymous pipe, not command-line arguments or a forgeable handoff file.
pub fn run_offline_helper() -> i32 {
    match run_offline_helper_from_reader(std::io::stdin().lock(), true) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("offline backup/restore helper failed: {error}");
            1
        }
    }
}

pub fn run_offline_helper_smoke() -> i32 {
    match run_offline_helper_from_reader(std::io::stdin().lock(), false) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("offline backup/restore smoke helper failed: {error}");
            1
        }
    }
}

/// Installed-bundle acceptance seam. Unlike the production arming call, the
/// caller has already shut down and dropped App, so it may close the pipe,
/// wait for the packaged child, and inspect the result in the same test
/// process. The child still exercises the exact framed protocol and operation.
pub(crate) fn run_packaged_helper_smoke(operation: &OfflineOperation) -> Result<(), BackupError> {
    validate_operation(operation)?;
    let executable = std::env::current_exe()
        .map_err(|error| BackupError::Helper(format!("resolve executable: {error}")))?;
    let mut child = std::process::Command::new(executable)
        .arg(HELPER_SMOKE_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| BackupError::Helper(error.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| BackupError::Helper("smoke helper stdin was not piped".into()))?;
    write_helper_request(&mut stdin, operation)?;
    drop(stdin);
    let status = child
        .wait()
        .map_err(|error| BackupError::Helper(format!("wait for smoke helper: {error}")))?;
    if !status.success() {
        return Err(BackupError::Helper(format!(
            "packaged helper exited with {status}"
        )));
    }
    Ok(())
}

fn write_helper_request(
    output: &mut impl Write,
    operation: &OfflineOperation,
) -> Result<(), BackupError> {
    let request = serde_json::to_vec(operation)?;
    if request.len() > MAX_HELPER_REQUEST_BYTES {
        return Err(BackupError::InvalidRequest(
            "serialized request exceeds the protocol limit".into(),
        ));
    }
    let length = u32::try_from(request.len())
        .map_err(|_| BackupError::InvalidRequest("request length overflow".into()))?;
    output
        .write_all(&length.to_le_bytes())
        .and_then(|()| output.write_all(&request))
        .and_then(|()| output.flush())
        .map_err(|error| BackupError::Helper(format!("send request: {error}")))
}

fn run_offline_helper_from_reader(mut input: impl Read, relaunch: bool) -> Result<(), BackupError> {
    let mut length = [0_u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_HELPER_REQUEST_BYTES {
        return Err(BackupError::InvalidRequest(format!(
            "request length {length} is outside the protocol limit"
        )));
    }
    let mut request = vec![0_u8; length];
    input.read_exact(&mut request)?;
    let operation: OfflineOperation = serde_json::from_slice(&request)?;
    validate_operation(&operation)?;

    // This read is the safety barrier. It cannot return EOF while the live
    // desktop process still owns HELPER_STDIN.
    let mut unexpected = Vec::new();
    input.read_to_end(&mut unexpected)?;
    if !unexpected.is_empty() {
        return Err(BackupError::InvalidRequest(
            "unexpected bytes followed the framed request".into(),
        ));
    }

    let app_data = operation.app_data().to_owned();
    let result = execute_operation(&operation);
    let receipt = match &result {
        Ok(receipt) => receipt.clone(),
        Err(error) => OperationReceipt {
            operation: operation.name().into(),
            succeeded: false,
            completed_at: photoproof_core::UtcMillis::now().to_rfc3339(),
            backup_path: operation.backup_path().to_owned(),
            rollback_path: None,
            detail: error.to_string(),
        },
    };
    // Best effort because the primary operation result is more important than
    // its status note. A failed restore attempts rollback before reaching here,
    // so app_data normally exists on both success and failure.
    let receipt_result = write_receipt(&app_data, &receipt);
    if relaunch {
        let executable = std::env::current_exe()
            .map_err(|error| BackupError::Helper(format!("resolve executable: {error}")))?;
        std::process::Command::new(executable)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|error| BackupError::Helper(format!("relaunch app: {error}")))?;
    }
    result?;
    receipt_result?;
    Ok(())
}

impl OfflineOperation {
    fn app_data(&self) -> &Path {
        match self {
            Self::Backup { app_data, .. } | Self::Restore { app_data, .. } => app_data,
        }
    }

    fn backup_path(&self) -> &Path {
        match self {
            Self::Backup { destination, .. } => destination,
            Self::Restore { backup, .. } => backup,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Backup { .. } => "backup",
            Self::Restore { .. } => "restore",
        }
    }
}

pub fn read_operation_receipt(app_data: &Path) -> Result<Option<OperationReceipt>, BackupError> {
    let path = app_data.join(RECEIPT_NAME);
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn validate_operation(operation: &OfflineOperation) -> Result<(), BackupError> {
    let app_data = operation.app_data();
    if !app_data.is_absolute() || !app_data.is_dir() {
        return Err(BackupError::InvalidRequest(format!(
            "app-data path is not an existing absolute directory: {}",
            app_data.display()
        )));
    }
    match operation {
        OfflineOperation::Backup { destination, .. } => {
            if !destination.is_absolute() {
                return Err(BackupError::InvalidRequest(
                    "backup destination is not absolute".into(),
                ));
            }
            if destination.exists() {
                return Err(BackupError::DestinationExists(destination.clone()));
            }
            let source = app_data.canonicalize()?;
            if nearest_existing(destination)?
                .canonicalize()?
                .starts_with(&source)
            {
                return Err(BackupError::DestinationInsideSource(destination.clone()));
            }
        }
        OfflineOperation::Restore { backup, .. } => {
            if !backup.is_absolute() || !backup.is_dir() {
                return Err(BackupError::InvalidRequest(format!(
                    "backup is not an existing absolute directory: {}",
                    backup.display()
                )));
            }
            verify_offline_backup(backup)?;
        }
    }
    Ok(())
}

fn execute_operation(operation: &OfflineOperation) -> Result<OperationReceipt, BackupError> {
    match operation {
        OfflineOperation::Backup {
            app_data,
            destination,
        } => {
            let manifest = create_offline_backup(app_data, destination)?;
            Ok(OperationReceipt {
                operation: "backup".into(),
                succeeded: true,
                completed_at: photoproof_core::UtcMillis::now().to_rfc3339(),
                backup_path: destination.clone(),
                rollback_path: None,
                detail: format!(
                    "Complete app-data backup verified ({} files).",
                    manifest.files.len()
                ),
            })
        }
        OfflineOperation::Restore { app_data, backup } => {
            let manifest = verify_offline_backup(backup)?;
            let rollback = rollback_path(app_data);
            std::fs::rename(app_data, &rollback)?;
            if let Some(parent) = app_data.parent() {
                sync_dir(parent)?;
            }
            match restore_offline_backup(backup, app_data) {
                Ok(restored) => Ok(OperationReceipt {
                    operation: "restore".into(),
                    succeeded: true,
                    completed_at: photoproof_core::UtcMillis::now().to_rfc3339(),
                    backup_path: backup.clone(),
                    rollback_path: Some(rollback),
                    detail: format!(
                        "Backup restored and verified ({} files). The previous app data was retained.",
                        restored.files.len()
                    ),
                }),
                Err(restore_error) => {
                    std::fs::rename(&rollback, app_data).map_err(|rollback_error| {
                        BackupError::Helper(format!(
                            "restore failed ({restore_error}); rollback directory {} could not be reinstated: {rollback_error}",
                            rollback.display()
                        ))
                    })?;
                    if let Some(parent) = app_data.parent() {
                        sync_dir(parent)?;
                    }
                    Err(BackupError::Helper(format!(
                        "restore failed before publication and the previous app data was reinstated: {restore_error}; verified backup contained {} files",
                        manifest.files.len()
                    )))
                }
            }
        }
    }
}

fn rollback_path(app_data: &Path) -> PathBuf {
    let name = app_data
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("photoproof-app-data");
    let stamp = photoproof_core::UtcMillis::now().epoch_ms();
    app_data.with_file_name(format!("{name}.pre-restore-{stamp}-{}", std::process::id()))
}

fn write_receipt(app_data: &Path, receipt: &OperationReceipt) -> Result<(), BackupError> {
    std::fs::create_dir_all(app_data)?;
    let target = app_data.join(RECEIPT_NAME);
    let temporary = app_data.join(format!(".{RECEIPT_NAME}.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(receipt)?;
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, target)?;
    sync_dir(app_data)?;
    Ok(())
}

/// Create a complete checksummed copy of app data. Caller must ensure the app
/// is fully quit; see the module contract.
pub fn create_offline_backup(
    app_data: &Path,
    destination: &Path,
) -> Result<BackupManifest, BackupError> {
    if !app_data.is_dir() {
        return Err(BackupError::MissingSource(app_data.to_owned()));
    }
    if destination.exists() {
        return Err(BackupError::DestinationExists(destination.to_owned()));
    }
    let source = app_data.canonicalize()?;
    if nearest_existing(destination)?
        .canonicalize()?
        .starts_with(&source)
    {
        return Err(BackupError::DestinationInsideSource(destination.to_owned()));
    }
    let mut relative_files = list_files(app_data)?;
    relative_files.sort();
    std::fs::create_dir_all(destination.join(PAYLOAD_DIR))?;
    let mut files = Vec::with_capacity(relative_files.len());
    for relative in relative_files {
        validate_relative(&relative)?;
        let source = app_data.join(&relative);
        let target = destination.join(PAYLOAD_DIR).join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, &target)?;
        let (bytes, blake3) = hash_file(&target)?;
        files.push(BackupFile {
            path: relative,
            bytes,
            blake3,
        });
    }
    let manifest = BackupManifest {
        format: "photoproof-app-data-backup".into(),
        version: 1,
        files,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut manifest_file = std::fs::File::create(destination.join(MANIFEST_NAME))?;
    manifest_file.write_all(&manifest_bytes)?;
    manifest_file.sync_all()?;
    sync_dir(destination)?;
    verify_offline_backup(destination)?;
    Ok(manifest)
}

/// Verify manifest shape, every listed checksum, and absence of unmanifested
/// payload files before a restore is attempted.
pub fn verify_offline_backup(backup: &Path) -> Result<BackupManifest, BackupError> {
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(backup.join(MANIFEST_NAME))?)?;
    if manifest.format != "photoproof-app-data-backup" || manifest.version != 1 {
        return Err(BackupError::Manifest(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported backup format/version",
            ),
        )));
    }
    let mut listed = Vec::with_capacity(manifest.files.len());
    for file in &manifest.files {
        validate_relative(&file.path)?;
        let path = backup.join(PAYLOAD_DIR).join(&file.path);
        let (bytes, actual) = hash_file(&path)?;
        if bytes != file.bytes || actual != file.blake3 {
            return Err(BackupError::Checksum {
                path: file.path.clone(),
                expected: format!("{} bytes / {}", file.bytes, file.blake3),
                actual: format!("{bytes} bytes / {actual}"),
            });
        }
        listed.push(file.path.clone());
    }
    listed.sort();
    let mut actual = list_files(&backup.join(PAYLOAD_DIR))?;
    actual.sort();
    if actual != listed {
        return Err(BackupError::Manifest(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "payload contains missing or unmanifested files",
            ),
        )));
    }
    Ok(manifest)
}

/// Restore into a path that does not exist. Copy goes to a sibling temporary
/// directory and is verified there before the final atomic rename.
pub fn restore_offline_backup(
    backup: &Path,
    destination: &Path,
) -> Result<BackupManifest, BackupError> {
    if destination.exists() {
        return Err(BackupError::DestinationExists(destination.to_owned()));
    }
    let manifest = verify_offline_backup(backup)?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("photoproof-app-data");
    let staging = destination.with_file_name(format!(".{name}.restore-{}", std::process::id()));
    if staging.exists() {
        return Err(BackupError::DestinationExists(staging));
    }
    std::fs::create_dir_all(&staging)?;
    for file in &manifest.files {
        let target = staging.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(backup.join(PAYLOAD_DIR).join(&file.path), target)?;
    }
    verify_restored_files(&staging, &manifest)?;
    sync_tree(&staging)?;
    std::fs::rename(&staging, destination)?;
    if let Some(parent) = destination.parent() {
        sync_dir(parent)?;
    }
    Ok(manifest)
}

fn verify_restored_files(root: &Path, manifest: &BackupManifest) -> Result<(), BackupError> {
    for file in &manifest.files {
        let (bytes, actual) = hash_file(&root.join(&file.path))?;
        if bytes != file.bytes || actual != file.blake3 {
            return Err(BackupError::Checksum {
                path: file.path.clone(),
                expected: format!("{} bytes / {}", file.bytes, file.blake3),
                actual: format!("{bytes} bytes / {actual}"),
            });
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), BackupError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BackupError::InvalidRelativePath(path.to_owned()));
    }
    Ok(())
}

fn list_files(root: &Path) -> Result<Vec<PathBuf>, BackupError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let entry_path = entry.path();
            if file_type.is_symlink() {
                return Err(BackupError::InvalidRelativePath(
                    entry_path
                        .strip_prefix(root)
                        .unwrap_or(entry_path.as_path())
                        .to_owned(),
                ));
            }
            if file_type.is_dir() {
                pending.push(entry_path);
            } else if file_type.is_file() {
                files.push(
                    entry_path
                        .strip_prefix(root)
                        .map_err(std::io::Error::other)?
                        .to_owned(),
                );
            }
        }
    }
    Ok(files)
}

fn hash_file(path: &Path) -> Result<(u64, String), BackupError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

fn nearest_existing(path: &Path) -> Result<&Path, BackupError> {
    let mut current = path;
    while !current.exists() {
        current = current
            .parent()
            .ok_or_else(|| BackupError::MissingSource(path.to_owned()))?;
    }
    Ok(current)
}

fn sync_tree(root: &Path) -> Result<(), BackupError> {
    let mut pending = vec![root.to_owned()];
    let mut dirs = Vec::new();
    while let Some(dir) = pending.pop() {
        dirs.push(dir.clone());
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                std::fs::File::open(entry.path())?.sync_all()?;
            }
        }
    }
    for dir in dirs.into_iter().rev() {
        sync_dir(&dir)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), BackupError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EofBarrierReader<R> {
        inner: R,
        framed_bytes_remaining: usize,
        barrier_entered: Option<std::sync::mpsc::Sender<()>>,
    }

    impl<R: Read> Read for EofBarrierReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.framed_bytes_remaining == 0
                && let Some(entered) = self.barrier_entered.take()
            {
                let _ = entered.send(());
            }
            let read = self.inner.read(buffer)?;
            self.framed_bytes_remaining = self.framed_bytes_remaining.saturating_sub(read);
            Ok(read)
        }
    }

    fn framed(operation: &OfflineOperation) -> Vec<u8> {
        let body = serde_json::to_vec(operation).unwrap();
        let mut wire = (body.len() as u32).to_le_bytes().to_vec();
        wire.extend(body);
        wire
    }

    #[test]
    fn full_backup_destroy_restore_drill_preserves_every_truth_and_cache_byte() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live-app-data");
        std::fs::create_dir_all(source.join("photoproof/journal/overflow/aa")).unwrap();
        std::fs::create_dir_all(source.join("photoproof/journal/sessions")).unwrap();
        std::fs::create_dir_all(source.join("vectors")).unwrap();
        std::fs::create_dir_all(source.join("previews/previews/aa/bb")).unwrap();
        for (path, bytes) in [
            ("photoproof.db", b"database".as_slice()),
            ("photoproof.db-wal", b"wal".as_slice()),
            ("settings.json", br#"{"theme":"dark"}"#),
            ("device-id", b"stable-device"),
            ("config.toml", b"[runtime]\ntier=0\n"),
            ("tuning.toml", b"[preview]\ndisplay_edge=2560\n"),
            ("collections.photoproof.json", br#"{"collections":[]}"#),
            (
                "topics.photoproof.json",
                br#"{"format":"photoproof-topics","version":1,"topics":[]}"#,
            ),
            (
                "photoproof/journal/overflow/aa/event.photoproof.json",
                b"overflow-journal",
            ),
            (
                "photoproof/journal/sessions/session.photoproof.json",
                b"session-journal",
            ),
            ("vectors/image.ppvec", b"derived-vector"),
            ("previews/previews/aa/bb/hash-disp.webp", b"derived-preview"),
        ] {
            std::fs::write(source.join(path), bytes).unwrap();
        }
        let backup = temp.path().join("backup");
        let before = create_offline_backup(&source, &backup).unwrap();
        std::fs::remove_dir_all(&source).unwrap();
        let after = restore_offline_backup(&backup, &source).unwrap();
        assert_eq!(before, after);
        for file in before.files {
            let (_, restored_hash) = hash_file(&source.join(&file.path)).unwrap();
            assert_eq!(restored_hash, file.blake3, "{}", file.path.display());
        }
    }

    #[test]
    fn verification_rejects_tampering_and_restore_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("device-id"), b"one").unwrap();
        let backup = temp.path().join("backup");
        create_offline_backup(&source, &backup).unwrap();
        std::fs::write(backup.join(PAYLOAD_DIR).join("device-id"), b"two").unwrap();
        assert!(matches!(
            verify_offline_backup(&backup),
            Err(BackupError::Checksum { .. })
        ));
        let existing = temp.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        assert!(matches!(
            restore_offline_backup(&backup, &existing),
            Err(BackupError::DestinationExists(_))
        ));
    }

    #[test]
    fn backup_refuses_destination_inside_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live");
        std::fs::create_dir(&source).unwrap();
        assert!(matches!(
            create_offline_backup(&source, &source.join("backup")),
            Err(BackupError::DestinationInsideSource(_))
        ));
    }

    #[test]
    fn helper_pipe_eof_creates_verified_backup_and_relaunch_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("photoproof.db"), b"database").unwrap();
        let destination = temp.path().join("Photoproof Backup.ppbackup");
        let operation = OfflineOperation::Backup {
            app_data: source.clone(),
            destination: destination.clone(),
        };

        run_offline_helper_from_reader(framed(&operation).as_slice(), false).unwrap();

        let manifest = verify_offline_backup(&destination).unwrap();
        assert_eq!(manifest.files.len(), 1);
        let receipt = read_operation_receipt(&source).unwrap().unwrap();
        assert!(receipt.succeeded);
        assert_eq!(receipt.operation, "backup");
        assert_eq!(receipt.backup_path, destination);
    }

    #[test]
    fn live_held_pipe_performs_no_operation_before_eof() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("photoproof.db"), b"database").unwrap();
        let destination = temp.path().join("backup");
        let operation = OfflineOperation::Backup {
            app_data: source.clone(),
            destination: destination.clone(),
        };
        let wire = framed(&operation);
        let wire_len = wire.len();
        let (reader, mut writer) = std::io::pipe().unwrap();
        let (barrier_tx, barrier_rx) = std::sync::mpsc::channel();
        let helper = std::thread::spawn(move || {
            run_offline_helper_from_reader(
                EofBarrierReader {
                    inner: reader,
                    framed_bytes_remaining: wire_len,
                    barrier_entered: Some(barrier_tx),
                },
                false,
            )
        });

        writer.write_all(&wire).unwrap();
        barrier_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("helper reached the post-request EOF barrier");
        assert!(
            !destination.exists(),
            "the helper must not create even the backup directory while the parent holds the pipe"
        );
        assert!(
            read_operation_receipt(&source).unwrap().is_none(),
            "no operation receipt may publish before the parent pipe closes"
        );

        drop(writer);
        helper.join().unwrap().unwrap();
        verify_offline_backup(&destination).unwrap();
        assert!(read_operation_receipt(&source).unwrap().unwrap().succeeded);
    }

    #[test]
    fn installed_restore_replaces_only_after_verification_and_retains_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let desired = temp.path().join("desired");
        std::fs::create_dir(&desired).unwrap();
        std::fs::write(desired.join("settings.json"), b"restored").unwrap();
        std::fs::write(desired.join("topics.photoproof.json"), b"topics").unwrap();
        let backup = temp.path().join("backup");
        create_offline_backup(&desired, &backup).unwrap();

        let live = temp.path().join("live");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("settings.json"), b"current").unwrap();
        let operation = OfflineOperation::Restore {
            app_data: live.clone(),
            backup: backup.clone(),
        };
        run_offline_helper_from_reader(framed(&operation).as_slice(), false).unwrap();

        assert_eq!(
            std::fs::read(live.join("settings.json")).unwrap(),
            b"restored"
        );
        assert_eq!(
            std::fs::read(live.join("topics.photoproof.json")).unwrap(),
            b"topics"
        );
        let receipt = read_operation_receipt(&live).unwrap().unwrap();
        assert!(receipt.succeeded);
        let rollback = receipt
            .rollback_path
            .expect("successful restore retains rollback");
        assert_eq!(
            std::fs::read(rollback.join("settings.json")).unwrap(),
            b"current"
        );
        assert_eq!(receipt.backup_path, backup);
    }

    #[test]
    fn tampered_restore_is_rejected_before_live_app_data_moves() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("settings.json"), b"backup").unwrap();
        let backup = temp.path().join("backup");
        create_offline_backup(&source, &backup).unwrap();
        std::fs::write(backup.join(PAYLOAD_DIR).join("settings.json"), b"tampered").unwrap();

        let live = temp.path().join("live");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("settings.json"), b"current").unwrap();
        let operation = OfflineOperation::Restore {
            app_data: live.clone(),
            backup,
        };
        assert!(run_offline_helper_from_reader(framed(&operation).as_slice(), false).is_err());
        assert_eq!(
            std::fs::read(live.join("settings.json")).unwrap(),
            b"current"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verification_rejects_a_payload_symlink_before_restore_publication() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("settings.json"), b"trusted bytes").unwrap();
        let backup = temp.path().join("backup");
        create_offline_backup(&source, &backup).unwrap();

        let external = temp.path().join("external-settings.json");
        std::fs::write(&external, b"trusted bytes").unwrap();
        let payload = backup.join(PAYLOAD_DIR).join("settings.json");
        std::fs::remove_file(&payload).unwrap();
        symlink(&external, &payload).unwrap();

        assert!(matches!(
            verify_offline_backup(&backup),
            Err(BackupError::InvalidRelativePath(path)) if path == Path::new("settings.json")
        ));
        let destination = temp.path().join("restored");
        assert!(restore_offline_backup(&backup, &destination).is_err());
        assert!(
            !destination.exists(),
            "symlink rejection occurs before restore staging is published"
        );
    }

    #[test]
    fn verification_rejects_unmanifested_payload_before_restore_publication() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("settings.json"), b"trusted").unwrap();
        let backup = temp.path().join("backup");
        create_offline_backup(&source, &backup).unwrap();
        std::fs::write(backup.join(PAYLOAD_DIR).join("injected.bin"), b"extra").unwrap();

        assert!(matches!(
            verify_offline_backup(&backup),
            Err(BackupError::Manifest(_))
        ));
        let destination = temp.path().join("restored");
        assert!(restore_offline_backup(&backup, &destination).is_err());
        assert!(
            !destination.exists(),
            "unmanifested bytes cannot reach a restore staging directory"
        );
    }

    #[test]
    fn helper_rejects_trailing_protocol_data_without_touching_app_data() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("settings.json"), b"current").unwrap();
        let destination = temp.path().join("backup");
        let operation = OfflineOperation::Backup {
            app_data: source,
            destination: destination.clone(),
        };
        let mut wire = framed(&operation);
        wire.push(1);
        assert!(matches!(
            run_offline_helper_from_reader(wire.as_slice(), false),
            Err(BackupError::InvalidRequest(_))
        ));
        assert!(!destination.exists());
    }
}
