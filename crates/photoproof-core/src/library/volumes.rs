//! Volumes: identity recipe, `.photoproof-volume` marker files, the
//! online/offline state machine, and read-only detection.
//!
//! Contract: spec/LIBRARY.md §4 (DECISIONS L2). Volume identity must survive
//! mount-point changes; the marker file beats platform ids; read-only is
//! verified by probe, not flags.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MARKER_FILENAME: &str = ".photoproof-volume";

/// `platform_kind` column values (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformIdKind {
    MacosUuid,
    WinSerial,
    LinuxFsUuid,
    Heuristic,
}

impl PlatformIdKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PlatformIdKind::MacosUuid => "macos-uuid",
            PlatformIdKind::WinSerial => "win-serial",
            PlatformIdKind::LinuxFsUuid => "linux-fsuuid",
            PlatformIdKind::Heuristic => "heuristic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "macos-uuid" => PlatformIdKind::MacosUuid,
            "win-serial" => PlatformIdKind::WinSerial,
            "linux-fsuuid" => PlatformIdKind::LinuxFsUuid,
            "heuristic" => PlatformIdKind::Heuristic,
            _ => return None,
        })
    }
}

/// What a mount probe observed about one mounted volume.
#[derive(Debug, Clone)]
pub struct ProbedVolume {
    /// The volume root (mount point) as currently mounted.
    pub mount_point: PathBuf,
    /// Platform-native id (level 2 of the §4.1 recipe); `None` if unavailable.
    pub platform_id: Option<String>,
    pub platform_kind: PlatformIdKind,
    pub label: Option<String>,
    pub fs_type: Option<String>,
    pub capacity_bytes: Option<i64>,
    /// The mount's read-only *flag* (verified separately by probe, §4.3).
    pub read_only_flag: bool,
    /// System/boot volume root: the marker is never written there (§4.1).
    pub is_system_root: bool,
    /// FAT/exFAT-class timestamp resolution: 2 s mtime tolerance (§7.3).
    pub coarse_mtime: bool,
}

/// Source of mounted-volume facts. Injectable: tests simulate mounts,
/// remounts at new mount points, enclosure swaps, and clones without
/// platform privileges.
pub trait VolumeProbe: Send + Sync {
    /// All currently mounted volumes that could host watched roots.
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>>;

    /// The volume containing `path` (longest mount-point prefix match).
    fn probe_path(&self, path: &Path) -> io::Result<ProbedVolume> {
        let mounts = self.list_mounts()?;
        mounts
            .into_iter()
            .filter(|m| path.starts_with(&m.mount_point))
            .max_by_key(|m| m.mount_point.as_os_str().len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no mounted volume contains {}", path.display()),
                )
            })
    }
}

/// `.photoproof-volume` marker file contents (§4.1 level 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMarker {
    pub schema_version: u32,
    pub volume_ulid: String,
    pub created_at: String,
    pub created_by: String,
}

/// Read and parse the marker at a volume root. Unparseable or absent → `None`
/// (the identity recipe falls through to platform ids).
pub fn read_marker(mount_point: &Path) -> Option<VolumeMarker> {
    let bytes = std::fs::read(mount_point.join(MARKER_FILENAME)).ok()?;
    let marker: VolumeMarker = serde_json::from_slice(&bytes).ok()?;
    if marker.schema_version != 1 || marker.volume_ulid.len() != 26 {
        return None;
    }
    Some(marker)
}

/// Write a fresh marker. Failures are non-fatal (the volume may be unwritable
/// despite its flags); callers log and fall through to platform identity.
pub fn write_marker(mount_point: &Path, volume_ulid: &str, now_rfc3339: &str) -> io::Result<()> {
    let marker = VolumeMarker {
        schema_version: 1,
        volume_ulid: volume_ulid.to_owned(),
        created_at: now_rfc3339.to_owned(),
        created_by: format!("photoproof/{}", env!("CARGO_PKG_VERSION")),
    };
    let json = serde_json::to_vec_pretty(&marker).expect("marker serializes");
    let tmp = mount_point.join(format!(".photoproof-volume.tmp-{}", std::process::id()));
    std::fs::write(&tmp, &json)?;
    let dest = mount_point.join(MARKER_FILENAME);
    match std::fs::rename(&tmp, &dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// §4.3: flags lie on network mounts — verify writability with a real
/// create-and-delete probe in `dir`.
pub fn verify_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".photoproof-write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// §4.1 level 3: heuristic fingerprint, last resort.
pub fn heuristic_fingerprint(
    fs_type: Option<&str>,
    label: Option<&str>,
    capacity_bytes: Option<i64>,
) -> String {
    let input = format!(
        "{}|{}|{}",
        fs_type.unwrap_or(""),
        label.unwrap_or(""),
        capacity_bytes.unwrap_or(0)
    );
    blake3::hash(input.as_bytes()).to_hex()[..16].to_string()
}

// ---------------------------------------------------------------------------
// Platform probe
// ---------------------------------------------------------------------------

/// Real mount probing. Implemented for Linux (`/proc/self/mounts` +
/// `/dev/disk/by-uuid`); macOS (DiskArbitration) and Windows
/// (`GetVolumeInformationW`) need FFI verified on the founder machine and
/// currently fall back to the heuristic kind — flagged in the packet report.
#[derive(Debug, Default)]
pub struct PlatformVolumeProbe;

const COARSE_MTIME_FS: &[&str] = &["vfat", "msdos", "exfat", "fat", "fat32"];

impl VolumeProbe for PlatformVolumeProbe {
    #[cfg(target_os = "linux")]
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        let mounts = std::fs::read_to_string("/proc/self/mounts")?;
        let uuid_by_device = linux_uuid_by_device();
        let mut out = Vec::new();
        for line in mounts.lines() {
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount), Some(fs_type), Some(opts)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // Only real block-device-backed (or network) filesystems can host
            // photo libraries; skip pseudo filesystems.
            if !device.starts_with('/') && !matches!(fs_type, "nfs" | "nfs4" | "cifs" | "smb3") {
                continue;
            }
            let mount_point = PathBuf::from(unescape_mount_path(mount));
            let read_only = opts.split(',').any(|o| o == "ro");
            let uuid = uuid_by_device.get(device).cloned();
            let capacity = statvfs_capacity(&mount_point);
            let (platform_id, platform_kind) = match uuid {
                Some(u) => (Some(u), PlatformIdKind::LinuxFsUuid),
                None => (None, PlatformIdKind::Heuristic),
            };
            out.push(ProbedVolume {
                is_system_root: mount_point == Path::new("/"),
                coarse_mtime: COARSE_MTIME_FS.contains(&fs_type),
                platform_id,
                platform_kind,
                label: None,
                fs_type: Some(fs_type.to_owned()),
                capacity_bytes: capacity,
                read_only_flag: read_only,
                mount_point,
            });
        }
        Ok(out)
    }

    #[cfg(not(target_os = "linux"))]
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        // Founder-machine work: DADiskCopyDescription / GetVolumeInformationW.
        Ok(vec![ProbedVolume {
            mount_point: PathBuf::from("/"),
            platform_id: None,
            platform_kind: PlatformIdKind::Heuristic,
            label: None,
            fs_type: None,
            capacity_bytes: None,
            read_only_flag: false,
            is_system_root: true,
            coarse_mtime: false,
        }])
    }
}

/// /proc/self/mounts octal-escapes spaces and friends (`\040`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape_mount_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8)
        {
            out.push(v as char);
            i += 4;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(target_os = "linux")]
fn linux_uuid_by_device() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/dev/disk/by-uuid") {
        for entry in entries.flatten() {
            if let (Ok(target), Some(uuid)) = (
                std::fs::canonicalize(entry.path()),
                entry.file_name().to_str().map(str::to_owned),
            ) {
                map.insert(target.to_string_lossy().into_owned(), uuid);
            }
        }
    }
    map
}

#[cfg(target_os = "linux")]
fn statvfs_capacity(mount_point: &Path) -> Option<i64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(mount_point.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc == 0 {
        Some((stat.f_blocks as i64).saturating_mul(stat.f_frsize as i64))
    } else {
        None
    }
}

/// Test probe: a mutable list of mounted volumes.
#[derive(Debug, Default, Clone)]
pub struct FakeVolumeProbe {
    mounts: std::sync::Arc<std::sync::RwLock<Vec<ProbedVolume>>>,
}

impl FakeVolumeProbe {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_mounts(&self, mounts: Vec<ProbedVolume>) {
        *self.mounts.write().expect("poisoned") = mounts;
    }
}

impl VolumeProbe for FakeVolumeProbe {
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        Ok(self.mounts.read().expect("poisoned").clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        write_marker(
            dir.path(),
            "01JZ5C4R2GW8Q1T9M3N7P5XKDA",
            "2026-06-09T00:00:00.000Z",
        )
        .unwrap();
        let m = read_marker(dir.path()).unwrap();
        assert_eq!(m.volume_ulid, "01JZ5C4R2GW8Q1T9M3N7P5XKDA");
        assert_eq!(m.schema_version, 1);
        assert!(m.created_by.starts_with("photoproof/"));
    }

    #[test]
    fn unparseable_marker_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILENAME), b"not json").unwrap();
        assert!(read_marker(dir.path()).is_none());
    }

    #[test]
    fn fingerprint_is_stable_16_hex() {
        let a = heuristic_fingerprint(Some("exfat"), Some("Archive"), Some(1_000_000));
        let b = heuristic_fingerprint(Some("exfat"), Some("Archive"), Some(1_000_000));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mount_path_unescape() {
        assert_eq!(unescape_mount_path("/mnt/My\\040Drive"), "/mnt/My Drive");
        assert_eq!(unescape_mount_path("/plain"), "/plain");
    }
}
