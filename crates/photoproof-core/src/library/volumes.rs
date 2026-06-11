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

/// Real mount probing. Implemented for Linux (`/proc/self/mountinfo` +
/// `/dev/disk/by-uuid`); macOS (DiskArbitration) and Windows
/// (`GetVolumeInformationW`) need FFI verified on the founder machine and
/// currently fall back to the heuristic kind — flagged in the packet report.
///
/// On btrfs the mountinfo root field carries the subvolume path
/// (e.g. `/@home`), which disambiguates mounts that share a block device.
/// Non-btrfs filesystems always report `/`, so `platform_id` remains the
/// bare UUID on those — backward compat is handled by the caller.
#[derive(Debug, Default)]
pub struct PlatformVolumeProbe;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const COARSE_MTIME_FS: &[&str] = &["vfat", "msdos", "exfat", "fat", "fat32"];

impl VolumeProbe for PlatformVolumeProbe {
    #[cfg(target_os = "linux")]
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        let raw = std::fs::read_to_string("/proc/self/mountinfo")?;
        let uuid_by_device = linux_uuid_by_device();
        let mut out = Vec::new();
        for line in raw.lines() {
            // mountinfo(5) format:
            //   id parent_id major:minor root mount_point options [opt...] - fs_type source super_opts
            // The root (field 4) is the mount root within the filesystem — on
            // btrfs this is the subvolume path (e.g. /@home, /@);
            // everywhere else it's just `/`.
            // The tricky part: fields 7..N before `-` are variable-length
            // optional tags (e.g. shared:XXX, master:XXX). Split everything,
            // find the lone `-`, and index from there.
            let all: Vec<&str> = line.split_whitespace().collect();
            let sep = match all.iter().position(|&t| t == "-") {
                Some(i) => i,
                None => continue,
            };
            let (before, after) = all.split_at(sep);
            let after = &after[1..]; // skip the `-` itself

            let (Some(&root_raw), Some(&mount_raw), Some(&opts)) =
                (before.get(3), before.get(4), before.get(5))
            else {
                continue;
            };
            let (Some(&fs_type), Some(&device)) = (after.first(), after.get(1)) else {
                continue;
            };

            // Only real block-device-backed (or network) filesystems can host
            // photo libraries; skip pseudo filesystems.
            if !device.starts_with('/') && !matches!(fs_type, "nfs" | "nfs4" | "cifs" | "smb3") {
                continue;
            }
            let root = unescape_mount_path(root_raw);
            let mount_point = PathBuf::from(unescape_mount_path(mount_raw));
            let read_only = opts.split(',').any(|o| o == "ro");
            let uuid = uuid_by_device.get(device).cloned();
            let capacity = statvfs_capacity(&mount_point);
            let (platform_id, platform_kind) = match uuid {
                Some(u) => {
                    // Disambiguate btrfs subvolumes by appending the mount
                    // root when it differs from `/`.
                    let pid = if root != "/" {
                        format!("{u}:{root}")
                    } else {
                        u
                    };
                    (Some(pid), PlatformIdKind::LinuxFsUuid)
                }
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

    #[cfg(target_os = "macos")]
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        // MNT_NOWAIT: the 30 s pump probe must never hang on a dead
        // network mount; slightly stale statfs data is fine — identity
        // fields (UUID, fs type) don't drift between syncs.
        let mut raw: *mut libc::statfs = std::ptr::null_mut();
        let n = unsafe { libc::getmntinfo(&mut raw, libc::MNT_NOWAIT) };
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mounts = unsafe { std::slice::from_raw_parts(raw, n as usize) };
        Ok(mounts.iter().filter_map(macos::probed_from_statfs).collect())
    }

    /// macOS override of the default longest-prefix lookup: statfs the
    /// path itself. Firmlinks make the prefix heuristic WRONG here — a
    /// path under /Users lives on the writable Data volume mounted at
    /// /System/Volumes/Data, but textually starts with "/" (the sealed
    /// system snapshot, whose UUID churns with every OS update).
    #[cfg(target_os = "macos")]
    fn probe_path(&self, path: &Path) -> io::Result<ProbedVolume> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
        let mut sfs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(c.as_ptr(), sfs.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let sfs = unsafe { sfs.assume_init() };
        let mut probed = macos::probed_from_statfs(&sfs).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no probeable volume contains {}", path.display()),
            )
        })?;
        // Firmlinked path (e.g. /Users/… on the Data volume mounted at
        // /System/Volumes/Data): rel-path math needs a mount the path is
        // actually UNDER. macOS firmlinks are root-level same-name links,
        // so "/" reaches the same files; identity fields stay the Data
        // volume's (stable UUID — unlike the sealed snapshot's).
        if !path.starts_with(&probed.mount_point) {
            probed.mount_point = PathBuf::from("/");
        }
        Ok(probed)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn list_mounts(&self) -> io::Result<Vec<ProbedVolume>> {
        // Founder-machine work (Windows): GetVolumeInformationW.
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

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    /// Pseudo-filesystems that can never host a watched root.
    const PSEUDO_FS: &[&str] = &["devfs", "autofs", "nullfs", "lifs", "synthfs"];

    fn cstr(field: &[libc::c_char]) -> Option<String> {
        let s = unsafe { std::ffi::CStr::from_ptr(field.as_ptr()) };
        s.to_str().ok().map(str::to_owned)
    }

    pub(super) fn probed_from_statfs(sfs: &libc::statfs) -> Option<ProbedVolume> {
        let fs_type = cstr(&sfs.f_fstypename)?;
        if PSEUDO_FS.contains(&fs_type.as_str()) {
            return None;
        }
        let mount_point = PathBuf::from(cstr(&sfs.f_mntonname)?);
        // libc's apple module doesn't re-export MNT_RDONLY; the value is
        // ABI-stable BSD (sys/mount.h).
        const MNT_RDONLY: u32 = 0x0000_0001;
        let uuid = volume_uuid(&mount_point);
        Some(ProbedVolume {
            // "/" is the sealed system snapshot; user data lives on the
            // firmlinked Data volume — both are "the system", neither is
            // a removable candidate.
            is_system_root: mount_point == Path::new("/")
                || mount_point == Path::new("/System/Volumes/Data"),
            coarse_mtime: super::COARSE_MTIME_FS.contains(&fs_type.as_str()),
            platform_kind: if uuid.is_some() {
                PlatformIdKind::MacosUuid
            } else {
                PlatformIdKind::Heuristic
            },
            platform_id: uuid,
            // External volumes mount under /Volumes/<name> — that name is
            // the user-visible label; system mounts get none.
            label: mount_point
                .parent()
                .filter(|p| *p == Path::new("/Volumes"))
                .and_then(|_| mount_point.file_name())
                .and_then(|n| n.to_str())
                .map(str::to_owned),
            fs_type: Some(fs_type),
            capacity_bytes: Some((sfs.f_blocks as i64).saturating_mul(i64::from(sfs.f_bsize))),
            read_only_flag: sfs.f_flags & MNT_RDONLY != 0,
            mount_point,
        })
    }

    /// §4.1 level 2 identity: the volume UUID via getattrlist
    /// ATTR_VOL_UUID (no DiskArbitration dependency). None on
    /// filesystems without one (some FAT/network mounts) — those fall to
    /// the level-3 heuristic fingerprint.
    fn volume_uuid(mount_point: &Path) -> Option<String> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(mount_point.as_os_str().as_bytes()).ok()?;
        let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
        attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrs.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
        // getattrlist's reply: u32 total length, then the requested
        // attributes in canonical order — here exactly one uuid_t.
        #[repr(C)]
        struct VolUuidBuf {
            len: u32,
            uuid: [u8; 16],
        }
        let mut buf = VolUuidBuf {
            len: 0,
            uuid: [0; 16],
        };
        let rc = unsafe {
            libc::getattrlist(
                c.as_ptr(),
                (&raw mut attrs).cast(),
                (&raw mut buf).cast(),
                std::mem::size_of::<VolUuidBuf>(),
                0,
            )
        };
        if rc != 0 || (buf.len as usize) < std::mem::size_of::<VolUuidBuf>() {
            return None;
        }
        if buf.uuid == [0u8; 16] {
            return None;
        }
        let u = buf.uuid;
        Some(format!(
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7], u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15]
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Real-syscall smoke: the mount list is non-empty, contains the
        /// system root, and APFS volumes carry a UUID identity.
        #[test]
        fn list_mounts_reports_real_volumes() {
            let mounts = PlatformVolumeProbe.list_mounts().expect("getmntinfo");
            assert!(!mounts.is_empty());
            assert!(mounts.iter().any(|m| m.mount_point == Path::new("/")));
            let root = mounts
                .iter()
                .find(|m| m.mount_point == Path::new("/"))
                .unwrap();
            assert!(root.is_system_root);
            assert_eq!(root.fs_type.as_deref(), Some("apfs"));
            assert!(root.platform_id.is_some(), "APFS root carries a UUID");
            assert_eq!(root.platform_kind, PlatformIdKind::MacosUuid);
        }

        /// probe_path binds a home path to the WRITABLE Data volume's
        /// identity (the firmlink target), never the sealed read-only
        /// snapshot's — while reporting a mount the path is actually
        /// under, so rel-path math works.
        #[test]
        fn probe_path_resolves_through_firmlinks() {
            let home = std::env::var("HOME").expect("HOME");
            let v = PlatformVolumeProbe
                .probe_path(Path::new(&home))
                .expect("statfs");
            assert!(
                !v.read_only_flag,
                "home must sit on a writable volume, got {v:?}"
            );
            assert!(v.platform_id.is_some());
            assert!(Path::new(&home).starts_with(&v.mount_point));
        }
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

/// Extract the bare UUID from a platform_id that may carry a subvol suffix
/// (`"UUID:/@home"` → `"UUID"`), or return the string unchanged.
/// This is used for backward-compat matching when upgrading from bare-UUID
/// rows to subvol-qualified rows.
pub fn bare_platform_uuid(pid: &str) -> &str {
    pid.split_once(':').map_or(pid, |(u, _)| u)
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

    #[test]
    fn bare_platform_uuid_passthrough() {
        assert_eq!(bare_platform_uuid("abc-123"), "abc-123");
        assert_eq!(bare_platform_uuid("abc-123:/@home"), "abc-123");
        assert_eq!(bare_platform_uuid("abc-123:/"), "abc-123");
    }
}
