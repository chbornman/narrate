//! Disk-capacity health and conservative admission for derived work.
//!
//! SQLite/journals remain admitted even when space is critically low: refusing
//! the user's small truth-bearing edit would be worse than trying it. Large,
//! safely reproducible writers (previews, full decodes, vectors, maintenance)
//! pause until capacity recovers. Model downloads have an additional
//! per-download size preflight in `runtime.rs`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Leave enough room for SQLite/WAL, logs, and ordinary desktop operation.
pub const WARNING_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const CRITICAL_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// A WAL this large outside an active burst deserves attention even while the
/// containing volume still has ample capacity.
pub const WAL_WARNING_BYTES: u64 = 64 * 1024 * 1024;
pub const WAL_CRITICAL_BYTES: u64 = 512 * 1024 * 1024;
/// Idle maintenance is due every six hours. A non-empty WAL older than that
/// means at least one maintenance opportunity has not truncated it.
pub const WAL_WARNING_AGE_MS: u64 = 6 * 60 * 60 * 1_000;
pub const WAL_CRITICAL_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapacityState {
    Healthy,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WalState {
    Healthy,
    Warning,
    Critical,
    Blocked,
    Unknown,
}

/// WAL-specific operational health. This deliberately does not hide inside the
/// combined database inventory: capacity, a stuck reader, and a WAL that has
/// simply missed maintenance require different recovery actions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalHealthSnapshot {
    pub path: String,
    pub size_bytes: Option<u64>,
    pub modified_at_ms: Option<u64>,
    pub age_ms: Option<u64>,
    pub state: WalState,
    pub warning_bytes: u64,
    pub critical_bytes: u64,
    pub warning_age_ms: u64,
    pub critical_age_ms: u64,
    pub inventory_error: Option<String>,
    pub last_maintenance_attempt_at_ms: Option<u64>,
    pub last_maintenance_success_at_ms: Option<u64>,
    pub last_maintenance_failure_at_ms: Option<u64>,
    pub last_maintenance_error: Option<String>,
    /// True only while the latest maintenance attempt is unresolved because a
    /// reader held a WAL snapshot. Historical failures remain in the fields
    /// above after recovery without leaving this active warning set.
    pub blocked_by_reader: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskStoreStatus {
    pub name: &'static str,
    pub path: String,
    /// `None` until the off-thread inventory has completed.
    pub used_bytes: Option<u64>,
    pub file_count: Option<u64>,
    pub available_bytes: Option<u64>,
    pub state: CapacityState,
    /// An unreadable subtree makes usage a lower bound, never a healthy zero.
    pub inventory_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskHealthSnapshot {
    pub observed_at_ms: u64,
    /// Worst state across app-data and configured models volumes.
    pub state: CapacityState,
    pub app_data_state: CapacityState,
    pub models_state: CapacityState,
    pub derived_work_paused: bool,
    pub warning_free_bytes: u64,
    pub critical_free_bytes: u64,
    pub wal: WalHealthSnapshot,
    pub stores: Vec<DiskStoreStatus>,
}

#[derive(Debug)]
pub struct DiskGovernor {
    app_data: PathBuf,
    models_dir: PathBuf,
    derived_paused: AtomicBool,
    snapshot: Mutex<DiskHealthSnapshot>,
}

impl DiskGovernor {
    /// Cheap construction: capacity is sampled, but no directory tree is
    /// walked on Tauri's setup thread.
    pub fn new(app_data: PathBuf, models_dir: PathBuf) -> Self {
        let app_free = photoproof_core::runtime::available_disk_bytes(&app_data);
        let models_free = photoproof_core::runtime::available_disk_bytes(&models_dir);
        let app_state = capacity_state(app_free);
        let models_state = capacity_state(models_free);
        let paused = app_state == CapacityState::Critical;
        let observed_at_ms = now_ms();
        let wal = wal_snapshot(
            &app_data.join("photoproof.db-wal"),
            observed_at_ms,
            WalMaintenanceHistory::default(),
        );
        let stores = store_specs(&app_data, &models_dir)
            .into_iter()
            .map(|spec| DiskStoreStatus {
                name: spec.name,
                path: spec.path.display().to_string(),
                used_bytes: None,
                file_count: None,
                available_bytes: if spec.on_models_volume {
                    models_free
                } else {
                    app_free
                },
                state: capacity_state(if spec.on_models_volume {
                    models_free
                } else {
                    app_free
                }),
                inventory_errors: 0,
            })
            .collect();
        Self {
            app_data,
            models_dir,
            derived_paused: AtomicBool::new(paused),
            snapshot: Mutex::new(DiskHealthSnapshot {
                observed_at_ms,
                state: worst_state(app_state, models_state),
                app_data_state: app_state,
                models_state,
                derived_work_paused: paused,
                warning_free_bytes: WARNING_FREE_BYTES,
                critical_free_bytes: CRITICAL_FREE_BYTES,
                wal,
                stores,
            }),
        }
    }

    pub fn derived_work_paused(&self) -> bool {
        self.derived_paused.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> DiskHealthSnapshot {
        self.snapshot.lock().expect("disk snapshot").clone()
    }

    #[cfg(test)]
    fn inject_capacity_for_test(&self, app_free: Option<u64>, models_free: Option<u64>) {
        let app_state = capacity_state(app_free);
        let models_state = capacity_state(models_free);
        let paused = app_state == CapacityState::Critical;
        self.derived_paused.store(paused, Ordering::Relaxed);
        let mut snapshot = self.snapshot.lock().expect("disk snapshot");
        snapshot.state = worst_state(app_state, models_state);
        snapshot.app_data_state = app_state;
        snapshot.models_state = models_state;
        snapshot.derived_work_paused = paused;
        for store in &mut snapshot.stores {
            let free = if store.name == "models" || store.name == "download-parts" {
                models_free
            } else {
                app_free
            };
            store.available_bytes = free;
            store.state = capacity_state(free);
        }
    }

    /// Fast capacity refresh for the 30-second volume lane. Retains the last
    /// inventory so low-space admission never requires a recursive walk.
    pub fn refresh_capacity(&self) -> DiskHealthSnapshot {
        let app_free = photoproof_core::runtime::available_disk_bytes(&self.app_data);
        let models_free = photoproof_core::runtime::available_disk_bytes(&self.models_dir);
        let app_state = capacity_state(app_free);
        let models_state = capacity_state(models_free);
        let paused = app_state == CapacityState::Critical;
        self.derived_paused.store(paused, Ordering::Relaxed);
        let mut snapshot = self.snapshot.lock().expect("disk snapshot");
        let observed_at_ms = now_ms();
        snapshot.observed_at_ms = observed_at_ms;
        snapshot.state = worst_state(app_state, models_state);
        snapshot.app_data_state = app_state;
        snapshot.models_state = models_state;
        snapshot.derived_work_paused = paused;
        let history = WalMaintenanceHistory::from(&snapshot.wal);
        snapshot.wal = wal_snapshot(
            &self.app_data.join("photoproof.db-wal"),
            observed_at_ms,
            history,
        );
        for store in &mut snapshot.stores {
            let free = if store.name == "models" || store.name == "download-parts" {
                models_free
            } else {
                app_free
            };
            store.available_bytes = free;
            store.state = capacity_state(free);
        }
        snapshot.clone()
    }

    /// Full component inventory. This can be expensive on a large cache, so it
    /// runs only in the managed disk-monitor lane, never setup or ingest.
    pub fn refresh_inventory(&self) -> DiskHealthSnapshot {
        let app_free = photoproof_core::runtime::available_disk_bytes(&self.app_data);
        let models_free = photoproof_core::runtime::available_disk_bytes(&self.models_dir);
        let preview_usage = preview_usage(&self.app_data.join("previews"));
        let mut stores = Vec::new();
        for spec in store_specs(&self.app_data, &self.models_dir) {
            let usage = match spec.name {
                "database-and-wal" => database_usage(&self.app_data),
                "previews" => preview_usage.derived,
                "full-decode-cache" => preview_usage.full_decode,
                "vectors" => tree_usage(&spec.path, |_| true),
                "models" => tree_usage(&spec.path, |_| true),
                "download-parts" => tree_usage(&spec.path, |path| {
                    path.extension()
                        .is_some_and(|extension| extension == "part")
                }),
                _ => Usage::default(),
            };
            let free = if spec.on_models_volume {
                models_free
            } else {
                app_free
            };
            stores.push(DiskStoreStatus {
                name: spec.name,
                path: spec.path.display().to_string(),
                used_bytes: Some(usage.bytes),
                file_count: Some(usage.files),
                available_bytes: free,
                state: capacity_state(free),
                inventory_errors: usage.errors,
            });
        }
        let app_state = capacity_state(app_free);
        let models_state = capacity_state(models_free);
        let paused = app_state == CapacityState::Critical;
        self.derived_paused.store(paused, Ordering::Relaxed);
        let observed_at_ms = now_ms();
        let history = {
            let snapshot = self.snapshot.lock().expect("disk snapshot");
            WalMaintenanceHistory::from(&snapshot.wal)
        };
        let next = DiskHealthSnapshot {
            observed_at_ms,
            state: worst_state(app_state, models_state),
            app_data_state: app_state,
            models_state,
            derived_work_paused: paused,
            warning_free_bytes: WARNING_FREE_BYTES,
            critical_free_bytes: CRITICAL_FREE_BYTES,
            wal: wal_snapshot(
                &self.app_data.join("photoproof.db-wal"),
                observed_at_ms,
                history,
            ),
            stores,
        };
        *self.snapshot.lock().expect("disk snapshot") = next.clone();
        next
    }

    /// Record a successful idle checkpoint/optimize pass and immediately
    /// resample the WAL. The last failure remains available for diagnostics,
    /// while `blocked_by_reader` clears because recovery is now proven.
    pub fn record_wal_maintenance_success(&self) -> DiskHealthSnapshot {
        self.record_wal_maintenance_outcome(None, false)
    }

    /// Record an idle maintenance failure. `blocked_by_reader` distinguishes
    /// SQLite's explicit busy checkpoint verdict from unrelated I/O faults.
    pub fn record_wal_maintenance_failure(
        &self,
        error: impl Into<String>,
        blocked_by_reader: bool,
    ) -> DiskHealthSnapshot {
        self.record_wal_maintenance_outcome(Some(error.into()), blocked_by_reader)
    }

    fn record_wal_maintenance_outcome(
        &self,
        failure: Option<String>,
        blocked_by_reader: bool,
    ) -> DiskHealthSnapshot {
        let observed_at_ms = now_ms();
        let mut snapshot = self.snapshot.lock().expect("disk snapshot");
        let mut history = WalMaintenanceHistory::from(&snapshot.wal);
        history.last_attempt_at_ms = Some(observed_at_ms);
        match failure {
            Some(error) => {
                history.last_failure_at_ms = Some(observed_at_ms);
                history.last_error = Some(error);
                history.blocked_by_reader = blocked_by_reader;
            }
            None => {
                history.last_success_at_ms = Some(observed_at_ms);
                history.blocked_by_reader = false;
            }
        }
        snapshot.observed_at_ms = observed_at_ms;
        snapshot.wal = wal_snapshot(
            &self.app_data.join("photoproof.db-wal"),
            observed_at_ms,
            history,
        );
        snapshot.clone()
    }
}

#[derive(Debug, Clone, Default)]
struct WalMaintenanceHistory {
    last_attempt_at_ms: Option<u64>,
    last_success_at_ms: Option<u64>,
    last_failure_at_ms: Option<u64>,
    last_error: Option<String>,
    blocked_by_reader: bool,
}

impl From<&WalHealthSnapshot> for WalMaintenanceHistory {
    fn from(wal: &WalHealthSnapshot) -> Self {
        Self {
            last_attempt_at_ms: wal.last_maintenance_attempt_at_ms,
            last_success_at_ms: wal.last_maintenance_success_at_ms,
            last_failure_at_ms: wal.last_maintenance_failure_at_ms,
            last_error: wal.last_maintenance_error.clone(),
            blocked_by_reader: wal.blocked_by_reader,
        }
    }
}

fn wal_snapshot(
    path: &Path,
    observed_at_ms: u64,
    history: WalMaintenanceHistory,
) -> WalHealthSnapshot {
    let (size_bytes, modified_at_ms, inventory_error) = match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => (
            Some(metadata.len()),
            metadata.modified().ok().map(system_time_ms),
            None,
        ),
        Ok(_) => (
            None,
            None,
            Some("WAL path is not a regular file".to_owned()),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Some(0), None, None),
        Err(error) => (None, None, Some(error.to_string())),
    };
    let age_ms = match (size_bytes, modified_at_ms) {
        (Some(0), _) | (_, None) => None,
        (_, Some(modified_at_ms)) => Some(observed_at_ms.saturating_sub(modified_at_ms)),
    };
    let state = wal_state(size_bytes, age_ms, inventory_error.as_deref(), &history);
    WalHealthSnapshot {
        path: path.display().to_string(),
        size_bytes,
        modified_at_ms,
        age_ms,
        state,
        warning_bytes: WAL_WARNING_BYTES,
        critical_bytes: WAL_CRITICAL_BYTES,
        warning_age_ms: WAL_WARNING_AGE_MS,
        critical_age_ms: WAL_CRITICAL_AGE_MS,
        inventory_error,
        last_maintenance_attempt_at_ms: history.last_attempt_at_ms,
        last_maintenance_success_at_ms: history.last_success_at_ms,
        last_maintenance_failure_at_ms: history.last_failure_at_ms,
        last_maintenance_error: history.last_error,
        blocked_by_reader: history.blocked_by_reader,
    }
}

fn wal_state(
    size_bytes: Option<u64>,
    age_ms: Option<u64>,
    inventory_error: Option<&str>,
    history: &WalMaintenanceHistory,
) -> WalState {
    if history.blocked_by_reader {
        return WalState::Blocked;
    }
    if inventory_error.is_some() || size_bytes.is_none() {
        return WalState::Unknown;
    }
    if size_bytes.is_some_and(|bytes| bytes >= WAL_CRITICAL_BYTES)
        || age_ms.is_some_and(|age| age >= WAL_CRITICAL_AGE_MS)
    {
        return WalState::Critical;
    }
    let latest_attempt_failed = history.last_failure_at_ms.is_some_and(|failure| {
        history
            .last_success_at_ms
            .is_none_or(|success| failure > success)
    });
    if latest_attempt_failed
        || size_bytes.is_some_and(|bytes| bytes >= WAL_WARNING_BYTES)
        || age_ms.is_some_and(|age| age >= WAL_WARNING_AGE_MS)
    {
        return WalState::Warning;
    }
    WalState::Healthy
}

pub fn capacity_state(available: Option<u64>) -> CapacityState {
    match available {
        None => CapacityState::Unknown,
        Some(bytes) if bytes < CRITICAL_FREE_BYTES => CapacityState::Critical,
        Some(bytes) if bytes < WARNING_FREE_BYTES => CapacityState::Warning,
        Some(_) => CapacityState::Healthy,
    }
}

fn worst_state(left: CapacityState, right: CapacityState) -> CapacityState {
    fn severity(state: CapacityState) -> u8 {
        match state {
            CapacityState::Healthy => 0,
            CapacityState::Unknown => 1,
            CapacityState::Warning => 2,
            CapacityState::Critical => 3,
        }
    }
    if severity(left) >= severity(right) {
        left
    } else {
        right
    }
}

#[derive(Clone)]
struct StoreSpec {
    name: &'static str,
    path: PathBuf,
    on_models_volume: bool,
}

fn store_specs(app_data: &Path, models_dir: &Path) -> Vec<StoreSpec> {
    vec![
        StoreSpec {
            name: "database-and-wal",
            path: app_data.join("photoproof.db"),
            on_models_volume: false,
        },
        StoreSpec {
            name: "previews",
            path: app_data.join("previews"),
            on_models_volume: false,
        },
        StoreSpec {
            name: "full-decode-cache",
            path: app_data.join("previews"),
            on_models_volume: false,
        },
        StoreSpec {
            name: "vectors",
            path: app_data.join("vectors"),
            on_models_volume: false,
        },
        StoreSpec {
            name: "models",
            path: models_dir.to_owned(),
            on_models_volume: true,
        },
        StoreSpec {
            name: "download-parts",
            path: models_dir.to_owned(),
            on_models_volume: true,
        },
    ]
}

#[derive(Debug, Default, Clone, Copy)]
struct Usage {
    bytes: u64,
    files: u64,
    errors: u64,
}

impl Usage {
    fn add_file(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.files = self.files.saturating_add(1);
    }
}

fn database_usage(app_data: &Path) -> Usage {
    let mut usage = Usage::default();
    for name in ["photoproof.db", "photoproof.db-wal", "photoproof.db-shm"] {
        match std::fs::metadata(app_data.join(name)) {
            Ok(metadata) if metadata.is_file() => usage.add_file(metadata.len()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => usage.errors = usage.errors.saturating_add(1),
        }
    }
    usage
}

#[derive(Default)]
struct PreviewUsage {
    derived: Usage,
    full_decode: Usage,
}

fn preview_usage(root: &Path) -> PreviewUsage {
    let mut usage = PreviewUsage::default();
    let mut errors = 0_u64;
    walk_files(
        root,
        &mut |path, bytes| {
            let is_full = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("-full-v"));
            if is_full {
                usage.full_decode.add_file(bytes);
            } else {
                usage.derived.add_file(bytes);
            }
        },
        &mut |count| errors = errors.saturating_add(count),
    );
    usage.derived.errors = errors;
    usage.full_decode.errors = errors;
    usage
}

fn tree_usage(root: &Path, include: impl Fn(&Path) -> bool) -> Usage {
    let mut usage = Usage::default();
    let mut errors = 0_u64;
    walk_files(
        root,
        &mut |path, bytes| {
            if include(path) {
                usage.add_file(bytes);
            }
        },
        &mut |count| errors = errors.saturating_add(count),
    );
    usage.errors = errors;
    usage
}

fn walk_files(
    root: &Path,
    visit: &mut impl FnMut(&Path, u64),
    record_errors: &mut impl FnMut(u64),
) {
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                record_errors(1);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    record_errors(1);
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    record_errors(1);
                    continue;
                }
            };
            // Never follow directory-entry symlinks: an app-data link can
            // escape the owned tree or form a cycle. The configured models
            // root itself may still be a symlink; `read_dir(root)` follows
            // that explicit operator choice once.
            if file_type.is_symlink() {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    record_errors(1);
                    continue;
                }
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                visit(&entry.path(), metadata.len());
            }
        }
    }
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_thresholds_are_strict_and_unknown_is_not_zero() {
        assert_eq!(capacity_state(None), CapacityState::Unknown);
        assert_eq!(
            capacity_state(Some(CRITICAL_FREE_BYTES - 1)),
            CapacityState::Critical
        );
        assert_eq!(
            capacity_state(Some(CRITICAL_FREE_BYTES)),
            CapacityState::Warning
        );
        assert_eq!(
            capacity_state(Some(WARNING_FREE_BYTES)),
            CapacityState::Healthy
        );
        assert_eq!(
            worst_state(CapacityState::Healthy, CapacityState::Warning),
            CapacityState::Warning
        );
        assert_eq!(
            worst_state(CapacityState::Unknown, CapacityState::Critical),
            CapacityState::Critical
        );
    }

    #[test]
    fn injected_critical_disk_pauses_derived_writers_but_allows_exif_and_authored_truth() {
        use std::io::Cursor;
        use std::sync::Arc;

        use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
        use photoproof_core::library::{
            ArtifactKind, FakeVolumeProbe, Library, LibraryOptions, PlatformIdKind, ProbedVolume,
            QueueOptions, ScanOptions, artifact_path,
        };
        use photoproof_core::{EventDraft, EventStore, RemarkSource, SessionContext};

        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let models = temp.path().join("models");
        let mount = temp.path().join("mount");
        let photos = mount.join("photos");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::create_dir_all(&photos).unwrap();

        let mut jpeg = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 6, Rgb([32, 64, 96])))
            .write_to(&mut jpeg, ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(photos.join("photo.jpg"), jpeg.get_ref()).unwrap();

        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![ProbedVolume {
            mount_point: mount,
            platform_id: Some("low-disk-fixture".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("Fixture".into()),
            fs_type: Some("fixture".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }]);
        let db = app_data.join("photoproof.db");
        let cache = app_data.join("previews");
        let library = Library::open_with(
            &db,
            &cache,
            LibraryOptions {
                probe: Arc::new(probe),
                ..LibraryOptions::default()
            },
        )
        .unwrap();
        let root = library.register_root(&photos, Some("photos")).unwrap();
        library.scan_root(&root, &ScanOptions::default()).unwrap();
        let hash = library.image_hashes().unwrap().pop().unwrap();

        let disk = DiskGovernor::new(app_data.clone(), models);
        disk.inject_capacity_for_test(Some(CRITICAL_FREE_BYTES - 1), Some(WARNING_FREE_BYTES));
        assert!(disk.derived_work_paused());
        assert_eq!(disk.snapshot().app_data_state, CapacityState::Critical);

        // This is the production admission split: essential ingest is always
        // allowed; preview/RAW/vector lanes do not enter while the governor is
        // critical.
        let exif = library
            .process_essential_queue(&QueueOptions::default())
            .unwrap();
        assert_eq!(exif.done, 1, "small EXIF truth remains admitted");
        assert!(
            library.image(&hash).unwrap().unwrap().pixel_width.is_some(),
            "the EXIF/image metadata row committed under critical capacity"
        );
        if !disk.derived_work_paused() {
            library
                .process_preview_queue(&QueueOptions::default())
                .unwrap();
        }
        for kind in [
            ArtifactKind::Micro,
            ArtifactKind::Thumb,
            ArtifactKind::Display,
        ] {
            assert!(
                !artifact_path(&cache, &hash, kind).exists(),
                "derived preview writer stayed paused"
            );
        }

        let store = EventStore::open(&db).unwrap();
        let session = store
            .open_session(SessionContext {
                app_version: "disk-admission-test".into(),
                device_id: "0123456789abcdef0123456789abcdef".into(),
                root_context: None,
            })
            .unwrap();
        store
            .append(
                &session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "authored under low disk".into(),
                    targets: vec![hash.clone()],
                },
                None,
            )
            .unwrap();
        assert_eq!(
            store.folded_journal(&hash).unwrap().len(),
            1,
            "authored journal truth remains admitted under critical capacity"
        );
    }

    #[test]
    fn wal_thresholds_distinguish_size_age_blocking_and_unknown_inventory() {
        let clean = WalMaintenanceHistory::default();
        assert_eq!(wal_state(Some(0), None, None, &clean), WalState::Healthy);
        assert_eq!(
            wal_state(Some(WAL_WARNING_BYTES), None, None, &clean),
            WalState::Warning
        );
        assert_eq!(
            wal_state(Some(WAL_CRITICAL_BYTES), None, None, &clean),
            WalState::Critical
        );
        assert_eq!(
            wal_state(Some(1), Some(WAL_WARNING_AGE_MS), None, &clean),
            WalState::Warning
        );
        assert_eq!(
            wal_state(Some(1), Some(WAL_CRITICAL_AGE_MS), None, &clean),
            WalState::Critical
        );
        assert_eq!(
            wal_state(Some(1), None, Some("permission denied"), &clean),
            WalState::Unknown
        );
        assert_eq!(
            wal_state(
                Some(1),
                None,
                None,
                &WalMaintenanceHistory {
                    blocked_by_reader: true,
                    ..WalMaintenanceHistory::default()
                }
            ),
            WalState::Blocked
        );
    }

    #[test]
    fn wal_is_reported_separately_and_maintenance_history_survives_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        let models = dir.path().join("models");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(app_data.join("photoproof.db-wal"), vec![0; 17]).unwrap();

        let disk = DiskGovernor::new(app_data, models);
        let initial = disk.snapshot();
        assert_eq!(initial.wal.size_bytes, Some(17));
        assert_eq!(initial.wal.state, WalState::Healthy);
        assert!(
            initial
                .stores
                .iter()
                .any(|store| store.name == "database-and-wal"),
            "the compatibility inventory remains while WAL health is distinct"
        );

        let blocked = disk.record_wal_maintenance_failure("checkpoint blocked by reader", true);
        assert_eq!(blocked.wal.state, WalState::Blocked);
        assert!(blocked.wal.blocked_by_reader);
        assert!(blocked.wal.last_maintenance_failure_at_ms.is_some());
        assert_eq!(
            blocked.wal.last_maintenance_error.as_deref(),
            Some("checkpoint blocked by reader")
        );

        let recovered = disk.record_wal_maintenance_success();
        assert_eq!(recovered.wal.state, WalState::Healthy);
        assert!(!recovered.wal.blocked_by_reader);
        assert!(recovered.wal.last_maintenance_success_at_ms.is_some());
        assert!(
            recovered.wal.last_maintenance_failure_at_ms.is_some(),
            "the recovered snapshot retains when the last failure occurred"
        );
        assert_eq!(
            recovered.wal.last_maintenance_error.as_deref(),
            Some("checkpoint blocked by reader"),
            "the recovered snapshot retains diagnostic detail"
        );
    }

    #[test]
    fn inventory_separates_db_wal_previews_full_decodes_vectors_and_parts() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        let models = dir.path().join("elsewhere-models");
        std::fs::create_dir_all(app_data.join("previews/previews/aa/bb")).unwrap();
        std::fs::create_dir_all(app_data.join("vectors")).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(app_data.join("photoproof.db"), vec![0; 11]).unwrap();
        std::fs::write(app_data.join("photoproof.db-wal"), vec![0; 7]).unwrap();
        std::fs::write(
            app_data.join("previews/previews/aa/bb/hash-disp.webp"),
            vec![0; 13],
        )
        .unwrap();
        std::fs::write(
            app_data.join("previews/previews/aa/bb/hash-full-v2.webp"),
            vec![0; 17],
        )
        .unwrap();
        std::fs::write(app_data.join("vectors/space.ppvec"), vec![0; 19]).unwrap();
        std::fs::write(models.join("model.onnx"), vec![0; 23]).unwrap();
        std::fs::write(models.join("model.onnx.part"), vec![0; 29]).unwrap();

        let snapshot = DiskGovernor::new(app_data, models).refresh_inventory();
        let used = |name| {
            snapshot
                .stores
                .iter()
                .find(|store| store.name == name)
                .and_then(|store| store.used_bytes)
                .unwrap()
        };
        assert_eq!(used("database-and-wal"), 18);
        assert_eq!(used("previews"), 13);
        assert_eq!(used("full-decode-cache"), 17);
        assert_eq!(used("vectors"), 19);
        assert_eq!(used("models"), 52);
        assert_eq!(used("download-parts"), 29);
        assert_eq!(snapshot.wal.size_bytes, Some(7));
        assert_eq!(snapshot.wal.state, WalState::Healthy);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_marks_an_unreadable_or_invalid_subtree_as_incomplete() {
        // A regular file where a directory is expected is a portable
        // deterministic read_dir failure (permissions are unreliable as root).
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(app_data.join("vectors"), b"not a directory").unwrap();
        let snapshot =
            DiskGovernor::new(app_data.clone(), app_data.join("models")).refresh_inventory();
        let vectors = snapshot
            .stores
            .iter()
            .find(|store| store.name == "vectors")
            .unwrap();
        assert_eq!(vectors.used_bytes, Some(0));
        assert_eq!(vectors.inventory_errors, 1);
    }
}
