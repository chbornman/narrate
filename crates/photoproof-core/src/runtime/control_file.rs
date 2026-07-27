//! Durable runtime control-file persistence.
//!
//! These files are small but authoritative: losing consent, license
//! acceptances, child ownership, or a tier decision changes application
//! behavior. The shared primitive keeps missing distinct from invalid bytes,
//! quarantines corruption, maintains a last-known-good copy, and commits with
//! a unique adjacent temp file plus file/directory durability.

#[cfg(not(target_os = "windows"))]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFileSource {
    Primary,
    LastKnownGood,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFileErrorKind {
    Missing,
    Corrupt,
    PermissionDenied,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{kind:?} control file {path}: {detail}")]
pub struct ControlFileError {
    pub kind: ControlFileErrorKind,
    pub path: PathBuf,
    pub detail: String,
    pub quarantined_path: Option<PathBuf>,
}

impl ControlFileError {
    pub(crate) fn from_io(path: &Path, error: io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => ControlFileErrorKind::Missing,
            io::ErrorKind::PermissionDenied => ControlFileErrorKind::PermissionDenied,
            _ => ControlFileErrorKind::Io,
        };
        Self {
            kind,
            path: path.to_owned(),
            detail: error.to_string(),
            quarantined_path: None,
        }
    }

    fn corrupt(path: &Path, detail: impl Into<String>) -> Self {
        Self {
            kind: ControlFileErrorKind::Corrupt,
            path: path.to_owned(),
            detail: detail.into(),
            quarantined_path: None,
        }
    }
}

impl From<ControlFileError> for io::Error {
    fn from(issue: ControlFileError) -> Self {
        let kind = match issue.kind {
            ControlFileErrorKind::Missing => io::ErrorKind::NotFound,
            ControlFileErrorKind::Corrupt => io::ErrorKind::InvalidData,
            ControlFileErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
            ControlFileErrorKind::Io => io::ErrorKind::Other,
        };
        io::Error::new(kind, issue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFileRecovery {
    pub source: ControlFileSource,
    pub quarantined: Vec<PathBuf>,
    pub warnings: Vec<ControlFileError>,
}

impl ControlFileRecovery {
    fn new(source: ControlFileSource) -> Self {
        Self {
            source,
            quarantined: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControlFileLoad<T> {
    pub value: Option<T>,
    pub recovery: ControlFileRecovery,
}

pub fn lkg_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    path.with_file_name(format!("{name}.lkg"))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ControlFileError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ControlFileError::from_io(path, error)),
    }
}

fn parse_optional<T>(
    path: &Path,
    parse: &impl Fn(&[u8]) -> Result<T, String>,
) -> Result<Option<(T, Vec<u8>)>, ControlFileError> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    parse(&bytes)
        .map(|value| Some((value, bytes)))
        .map_err(|detail| ControlFileError::corrupt(path, detail))
}

/// Load and validate a control file. Missing yields `value: None`; corruption
/// never does. Invalid bytes are quarantined and a valid LKG is restored, or a
/// structured error is returned. A quarantine marker prevents the next launch
/// from mistaking prior corruption for a clean first launch.
pub fn load_control<T>(
    path: &Path,
    parse: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<ControlFileLoad<T>, ControlFileError> {
    match parse_optional(path, &parse) {
        Ok(Some((value, bytes))) => {
            let mut recovery = ControlFileRecovery::new(ControlFileSource::Primary);
            let lkg = lkg_path(path);
            match parse_optional(&lkg, &parse) {
                Err(issue) if issue.kind == ControlFileErrorKind::Corrupt => {
                    match quarantine(&lkg, issue) {
                        Ok(quarantined) => recovery.quarantined.push(quarantined),
                        Err(issue) => recovery.warnings.push(issue),
                    }
                }
                Err(issue) => recovery.warnings.push(issue),
                Ok(_) => {}
            }
            if let Err(error) = write_durable(&lkg, &bytes) {
                recovery
                    .warnings
                    .push(ControlFileError::from_io(&lkg, error));
            }
            Ok(ControlFileLoad {
                value: Some(value),
                recovery,
            })
        }
        Ok(None) => load_after_missing(path, &parse),
        Err(issue) if issue.kind == ControlFileErrorKind::Corrupt => {
            let quarantined = quarantine(path, issue.clone())?;
            let lkg = lkg_path(path);
            match parse_optional(&lkg, &parse) {
                Ok(Some((value, bytes))) => {
                    write_durable(path, &bytes)
                        .map_err(|error| ControlFileError::from_io(path, error))?;
                    let mut recovery = ControlFileRecovery::new(ControlFileSource::LastKnownGood);
                    recovery.quarantined.push(quarantined);
                    Ok(ControlFileLoad {
                        value: Some(value),
                        recovery,
                    })
                }
                Ok(None) => Err(ControlFileError {
                    quarantined_path: Some(quarantined),
                    ..issue
                }),
                Err(lkg_issue) if lkg_issue.kind == ControlFileErrorKind::Corrupt => {
                    let lkg_quarantine = quarantine(&lkg, lkg_issue)?;
                    Err(ControlFileError {
                        detail: format!(
                            "{}; last-known-good copy was also corrupt ({})",
                            issue.detail,
                            lkg_quarantine.display()
                        ),
                        quarantined_path: Some(quarantined),
                        ..issue
                    })
                }
                Err(lkg_issue) => Err(ControlFileError {
                    detail: format!(
                        "{}; last-known-good recovery failed at {}: {}",
                        issue.detail,
                        lkg_issue.path.display(),
                        lkg_issue.detail
                    ),
                    quarantined_path: Some(quarantined),
                    ..issue
                }),
            }
        }
        Err(issue) => Err(issue),
    }
}

fn load_after_missing<T>(
    path: &Path,
    parse: &impl Fn(&[u8]) -> Result<T, String>,
) -> Result<ControlFileLoad<T>, ControlFileError> {
    let lkg = lkg_path(path);
    match parse_optional(&lkg, parse) {
        Ok(Some((value, bytes))) => {
            write_durable(path, &bytes).map_err(|error| ControlFileError::from_io(path, error))?;
            Ok(ControlFileLoad {
                value: Some(value),
                recovery: ControlFileRecovery::new(ControlFileSource::LastKnownGood),
            })
        }
        Ok(None) => {
            if quarantine_exists(path)? {
                return Err(ControlFileError {
                    kind: ControlFileErrorKind::Missing,
                    path: path.to_owned(),
                    detail:
                        "control file is missing after a prior corruption quarantine and no last-known-good copy exists"
                            .into(),
                    quarantined_path: None,
                });
            }
            Ok(ControlFileLoad {
                value: None,
                recovery: ControlFileRecovery::new(ControlFileSource::Missing),
            })
        }
        Err(issue) if issue.kind == ControlFileErrorKind::Corrupt => {
            let quarantined = quarantine(&lkg, issue.clone())?;
            Err(ControlFileError {
                quarantined_path: Some(quarantined),
                ..issue
            })
        }
        Err(issue) => Err(issue),
    }
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<ControlFileLoad<T>, ControlFileError> {
    load_control(path, |bytes| {
        serde_json::from_slice(bytes).map_err(|error| error.to_string())
    })
}

pub fn save_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    save_control(path, &bytes)
}

/// Commit authoritative bytes and an LKG. LKG is written first so every
/// successful primary commit has a same-value recovery copy.
pub fn save_control(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_durable(&lkg_path(path), bytes)?;
    write_durable(path, bytes)
}

/// Publish a derived file atomically without an LKG.
pub fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("control file has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", ulid::Ulid::new()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&tmp, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(target_os = "windows")]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    move_file_ex(
        from,
        to,
        windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
            | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
    )
}

#[cfg(not(target_os = "windows"))]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(target_os = "windows")]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

fn quarantine(path: &Path, mut issue: ControlFileError) -> Result<PathBuf, ControlFileError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    let quarantine = path.with_file_name(format!("{name}.corrupt-{}", ulid::Ulid::new()));
    durable_move_to_new(path, &quarantine)
        .map_err(|error| ControlFileError::from_io(path, error))?;
    issue.quarantined_path = Some(quarantine.clone());
    Ok(quarantine)
}

#[cfg(not(target_os = "windows"))]
fn durable_move_to_new(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)?;
    sync_parent(to.parent().unwrap_or_else(|| Path::new(".")))
}

#[cfg(target_os = "windows")]
fn durable_move_to_new(from: &Path, to: &Path) -> io::Result<()> {
    move_file_ex(
        from,
        to,
        windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
    )
}

#[cfg(target_os = "windows")]
fn move_file_ex(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn quarantine_exists(path: &Path) -> Result<bool, ControlFileError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    let prefix = format!("{name}.corrupt-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ControlFileError::from_io(parent, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| ControlFileError::from_io(parent, error))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
    struct Fixture {
        value: String,
    }

    #[test]
    fn missing_corrupt_lkg_and_interrupted_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.json");
        let missing = load_json::<Fixture>(&path).unwrap();
        assert_eq!(missing.recovery.source, ControlFileSource::Missing);
        assert!(missing.value.is_none());

        let expected = Fixture {
            value: "committed".into(),
        };
        save_json(&path, &expected).unwrap();
        std::fs::write(&path, b"{").unwrap();
        std::fs::write(dir.path().join(".fixture.json.tmp-interrupted"), b"{").unwrap();
        let recovered = load_json::<Fixture>(&path).unwrap();
        assert_eq!(recovered.recovery.source, ControlFileSource::LastKnownGood);
        assert_eq!(recovered.value, Some(expected));
        assert_eq!(recovered.recovery.quarantined.len(), 1);
    }

    #[test]
    fn corruption_without_lkg_does_not_become_clean_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.json");
        std::fs::write(&path, b"{").unwrap();
        let first = load_json::<Fixture>(&path).unwrap_err();
        assert_eq!(first.kind, ControlFileErrorKind::Corrupt);
        assert!(first.quarantined_path.unwrap().exists());
        let second = load_json::<Fixture>(&path).unwrap_err();
        assert_eq!(second.kind, ControlFileErrorKind::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn permission_is_not_missing_or_corrupt() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let issue = load_json::<Fixture>(&path).unwrap_err();
        assert_eq!(issue.kind, ControlFileErrorKind::PermissionDenied);
    }
}
