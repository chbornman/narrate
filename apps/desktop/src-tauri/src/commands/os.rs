//! OS-integration commands (D4) — STAGE A OWNS THIS FILE.
//!
//! - `image_abs_path`         → ImagePathsDto (best online path per LIBRARY
//!   §3.1 `best_path`; "Copy file path" pairs this with
//!   navigator.clipboard in the frontend).
//! - `reveal_in_file_manager` → show the image's file in the OS file
//!   manager (`open -R` / `explorer /select,`; Linux has no portable
//!   select-in-manager verb without new deps, so the containing directory
//!   opens via `xdg-open`).
//! - `reveal_folder`          → show a watched folder.
//! - `open_with_default`      → open the image with the OS default app.
//!
//! All launches are the `xdg-open` class — fire-and-forget child
//! processes, no new dependencies (INTEGRATION may swap in
//! tauri-plugin-opener without touching the command surface).
//!
//! NO deletion verbs exist here, by decision (D3): the app never deletes
//! or trashes files — "Show in file manager" covers it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use photoproof_core::ContentHash;
use photoproof_core::library::Library;

use super::S;
use crate::dto::ImagePathsDto;
use crate::error::{CmdError, CmdResult};

/// `mount_point` + volume-relative path → absolute path.
fn join_mount(mount: &str, rel: &str) -> PathBuf {
    if rel.is_empty() {
        PathBuf::from(mount)
    } else {
        Path::new(mount).join(rel)
    }
}

/// Pure resolution: content hash → ImagePathsDto over `Library::best_path`
/// (online beats offline; an offline-only image keeps its rel path and
/// volume label so the UI can say where it lives, with `abs_path = None`).
fn image_paths(library: &Library, hash: &str) -> CmdResult<ImagePathsDto> {
    let hash = ContentHash::from_hex(hash)
        .map_err(|e| CmdError::Invalid(format!("bad image hash: {e}")))?;
    let best = library
        .best_path(&hash)?
        .ok_or_else(|| CmdError::Invalid("image has no known path".into()))?;
    let volume_label = library.volume(&best.row.volume_id)?.and_then(|v| v.label);
    let abs_path = if best.online {
        best.mount_point.as_deref().map(|m| {
            join_mount(m, &best.row.rel_path)
                .to_string_lossy()
                .into_owned()
        })
    } else {
        None
    };
    Ok(ImagePathsDto {
        abs_path,
        rel_path: best.row.rel_path,
        volume_label,
        online: best.online,
    })
}

/// Online absolute path or a one-line offline explanation (the volume
/// label names the disk to connect).
fn online_abs(library: &Library, hash: &str) -> CmdResult<PathBuf> {
    let dto = image_paths(library, hash)?;
    match dto.abs_path {
        Some(p) => Ok(PathBuf::from(p)),
        None => Err(CmdError::Invalid(match dto.volume_label {
            Some(label) => format!("image is on an offline volume ({label})"),
            None => "image is on an offline volume".into(),
        })),
    }
}

// ---------------------------------------------------------------------------
// Process specs — pure builders (unit-tested) + a detached spawner
// ---------------------------------------------------------------------------

/// Reveal = show the FILE in the file manager.
#[cfg(target_os = "linux")]
fn reveal_spec(path: &Path) -> CmdResult<(&'static str, Vec<OsString>)> {
    // No portable "select in file manager" on Linux without new deps:
    // open the containing directory instead.
    let dir = path
        .parent()
        .ok_or_else(|| CmdError::Invalid("path has no parent directory".into()))?;
    Ok(("xdg-open", vec![dir.as_os_str().to_owned()]))
}

#[cfg(target_os = "macos")]
fn reveal_spec(path: &Path) -> CmdResult<(&'static str, Vec<OsString>)> {
    Ok((
        "open",
        vec![OsString::from("-R"), path.as_os_str().to_owned()],
    ))
}

#[cfg(target_os = "windows")]
fn reveal_spec(path: &Path) -> CmdResult<(&'static str, Vec<OsString>)> {
    let mut arg = OsString::from("/select,");
    arg.push(path.as_os_str());
    Ok(("explorer", vec![arg]))
}

/// Open = hand the path (file or directory) to the OS default handler.
fn open_spec(path: &Path) -> (&'static str, Vec<OsString>) {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    (program, vec![path.as_os_str().to_owned()])
}

/// "Open in external editor" (BACKLOG "Configurable external editor, D4
/// revisit"): hand the FILE to the configured editor as SEPARATE argv
/// entries — never a shell string. On macOS, `open -a <editor> <path>`
/// launches the named application against the file; elsewhere the editor IS
/// the program and the file its sole arg. Keeping editor and path distinct
/// argv slots is the injection guard: an editor name carrying shell
/// metacharacters (or a path that does) is data passed straight to execvp,
/// never re-parsed by a shell.
fn editor_spec(editor: &str, path: &Path) -> (OsString, Vec<OsString>) {
    #[cfg(target_os = "macos")]
    {
        (
            OsString::from("open"),
            vec![
                OsString::from("-a"),
                OsString::from(editor),
                path.as_os_str().to_owned(),
            ],
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        (OsString::from(editor), vec![path.as_os_str().to_owned()])
    }
}

/// Fire-and-forget launch; a detached reaper thread waits the child so
/// short-lived openers never accumulate as zombies. `P: AsRef<OsStr>` so it
/// takes either a `&'static str` default-handler program (reveal/open-with-
/// default) or an OWNED editor program snapshotted out of settings.
fn spawn_detached<P: AsRef<OsStr>>((program, args): (P, Vec<OsString>)) -> CmdResult<()> {
    let mut child = Command::new(program).args(args).spawn()?;
    std::thread::Builder::new()
        .name("pp-os-open".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// "Copy file path" (D4): the frontend writes `abs_path` to the clipboard.
#[tauri::command]
pub async fn image_abs_path(app: S<'_>, hash: String) -> CmdResult<ImagePathsDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        image_paths(&app.library, &hash)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// "Show in file manager" (D4) for the active image.
#[tauri::command]
pub async fn reveal_in_file_manager(app: S<'_>, hash: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        spawn_detached(reveal_spec(&online_abs(&app.library, &hash)?)?)
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// "Show in file manager" for a rail folder (root-relative `folder`;
/// "" = the root itself). Directories open directly.
#[tauri::command]
pub async fn reveal_folder(app: S<'_>, root_id: String, folder: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let record = app
            .library
            .root(&root_id)?
            .ok_or_else(|| CmdError::Invalid(format!("unknown root: {root_id}")))?;
        let dto = super::library::root_dto(&app, &record)?;
        let base = dto
            .abs_path
            .ok_or_else(|| CmdError::Invalid("folder is on an offline volume".into()))?;
        spawn_detached(open_spec(&join_mount(&base, &folder)))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// "Open with default app" (D4 — no configurable editor yet).
#[tauri::command]
pub async fn open_with_default(app: S<'_>, hash: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        spawn_detached(open_spec(&online_abs(&app.library, &hash)?))
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// "Open in external editor" (BACKLOG "Configurable external editor, D4
/// revisit"; on-thesis: review here, edit there). Resolves the readable
/// ORIGINAL via the same online_abs/best_path chain reveal/open-with-default
/// use, REFUSES a missing or unreadable file WITHOUT spawning (a stale index
/// row pointing at a deleted file would otherwise launch the editor on
/// nothing), snapshots the configured editor under the settings lock then
/// DROPS the lock before spawning (never hold a mutex across a process
/// launch), and dispatches: a configured editor → editor_spec; unset → the
/// open_spec OS-default path (the empty-pref fallback that keeps the single
/// menu seat useful).
#[tauri::command]
pub async fn open_in_external_editor(app: S<'_>, hash: String) -> CmdResult<()> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let path = online_abs(&app.library, &hash)?;
        // Refuse the missing/unreadable file before any spawn: an offline
        // volume already errored in online_abs; this catches a path that
        // resolved but no longer points at a real file.
        if !path.is_file() {
            return Err(CmdError::Invalid(
                "image file is missing or unreadable".into(),
            ));
        }
        // Snapshot the pref, then DROP the lock — the launch happens outside
        // the critical section.
        let editor = {
            let s = app.settings.lock().expect("settings mutex");
            s.external_editor.clone()
        };
        match editor {
            Some(editor) => spawn_detached(editor_spec(&editor, &path)),
            None => spawn_detached(open_spec(&path)),
        }
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

// ---------------------------------------------------------------------------
// Tests — path resolution over a temp Library (injected volume probe so
// online/offline flips are deterministic), plus the pure process specs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use photoproof_core::library::{
        FakeVolumeProbe, Library, LibraryOptions, PlatformIdKind, ProbedVolume, ScanOptions,
    };

    use super::*;

    fn probed(mount: &Path) -> ProbedVolume {
        ProbedVolume {
            mount_point: mount.to_path_buf(),
            platform_id: Some("uuid-os-test".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("ShootDisk".into()),
            fs_type: Some("ext4".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }
    }

    /// Temp library with one scanned root (`mount/shoot`) holding one
    /// image file. Returns (tempdir, library, probe, file path).
    fn env() -> (tempfile::TempDir, Library, FakeVolumeProbe, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        let dir = mount.join("shoot");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("IMG_0001.jpg");
        std::fs::write(&file, b"not a real jpeg, hashing does not care").unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let lib = Library::open_with(
            tmp.path().join("photoproof.db"),
            tmp.path().join("previews"),
            LibraryOptions {
                probe: Arc::new(probe.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let root_id = lib.register_root(&dir, Some("shoot")).unwrap();
        lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
        (tmp, lib, probe, file)
    }

    fn only_hash(lib: &Library) -> String {
        let hashes = lib.image_hashes().unwrap();
        assert_eq!(hashes.len(), 1, "the scan ingests exactly one image");
        hashes[0].as_str().to_owned()
    }

    #[test]
    fn image_paths_resolves_the_best_online_path() {
        let (_tmp, lib, _probe, file) = env();
        let dto = image_paths(&lib, &only_hash(&lib)).unwrap();
        assert!(dto.online);
        assert_eq!(dto.abs_path.as_deref(), file.to_str());
        assert_eq!(dto.rel_path, "shoot/IMG_0001.jpg"); // volume-relative
        assert_eq!(dto.volume_label.as_deref(), Some("ShootDisk"));
    }

    #[test]
    fn offline_volume_keeps_identity_but_yields_no_abs_path() {
        let (_tmp, lib, probe, _file) = env();
        let hash = only_hash(&lib);
        probe.set_mounts(vec![]); // disk pulled
        lib.probe_volumes().unwrap();
        let dto = image_paths(&lib, &hash).unwrap();
        assert!(!dto.online);
        assert_eq!(dto.abs_path, None);
        assert_eq!(dto.rel_path, "shoot/IMG_0001.jpg");
        assert_eq!(dto.volume_label.as_deref(), Some("ShootDisk"));
        // The reveal/open verbs refuse with the volume named.
        let err = online_abs(&lib, &hash).unwrap_err().to_string();
        assert!(err.contains("offline"), "got: {err}");
        assert!(err.contains("ShootDisk"), "got: {err}");
    }

    #[test]
    fn unknown_and_malformed_hashes_are_invalid() {
        let (_tmp, lib, _probe, _file) = env();
        assert!(image_paths(&lib, &"0".repeat(64)).is_err()); // unknown
        assert!(image_paths(&lib, "not-hex").is_err()); // malformed
    }

    #[test]
    fn join_mount_handles_the_volume_root() {
        assert_eq!(join_mount("/mnt/vol", ""), PathBuf::from("/mnt/vol"));
        assert_eq!(
            join_mount("/mnt/vol", "shoot/IMG_0001.jpg"),
            PathBuf::from("/mnt/vol/shoot/IMG_0001.jpg")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_specs_are_the_xdg_open_class() {
        let (program, args) = reveal_spec(Path::new("/mnt/vol/shoot/IMG_0001.jpg")).unwrap();
        assert_eq!(program, "xdg-open");
        // Reveal opens the CONTAINING directory (no select verb sans deps).
        assert_eq!(args, vec![OsString::from("/mnt/vol/shoot")]);

        let (program, args) = open_spec(Path::new("/mnt/vol/shoot/IMG_0001.jpg"));
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec![OsString::from("/mnt/vol/shoot/IMG_0001.jpg")]);
    }

    // editor_spec injection safety: a shell-metacharacter editor name and
    // the path must each be their OWN argv entry, never a joined shell
    // string. execvp(program, argv) never re-parses the metacharacters, so
    // an editor name like `evil; rm -rf ~` is inert data, not a command.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_editor_spec_keeps_editor_and_path_distinct_argv() {
        let evil = "evil; rm -rf ~ #";
        let (program, args) = editor_spec(evil, Path::new("/mnt/vol/shoot/IMG_0001.jpg"));
        assert_eq!(program, OsString::from("open"));
        assert_eq!(
            args,
            vec![
                OsString::from("-a"),
                OsString::from(evil),
                OsString::from("/mnt/vol/shoot/IMG_0001.jpg"),
            ]
        );
        // The metacharacters live in ONE argv slot, undiluted — they were
        // never split into a path or a second program.
        assert_eq!(args[1], OsString::from(evil));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_editor_spec_keeps_editor_and_path_distinct_argv() {
        let evil = "evil; rm -rf ~ #";
        let (program, args) = editor_spec(evil, Path::new("/mnt/vol/shoot/IMG_0001.jpg"));
        // The editor IS the program; the path is its lone, separate arg.
        assert_eq!(program, OsString::from(evil));
        assert_eq!(args, vec![OsString::from("/mnt/vol/shoot/IMG_0001.jpg")]);
    }
}
