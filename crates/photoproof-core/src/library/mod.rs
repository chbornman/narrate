//! Library: content-addressed identity, volumes, watched roots, watcher +
//! reconciliation, ingest passes, embedded-preview extraction, EXIF subset,
//! thumbnail cache.
//!
//! Contract: spec/LIBRARY.md. Owned by work packet P2.2.
//!
//! Everything here is the **index side** of the system: every table is
//! rebuildable from the filesystem plus the sidecar set. Identity is the
//! BLAKE3-256 of file bytes — images are known by hash, never by path.

mod exclusions;
mod hashing;
mod ingest;
mod metadata;
mod paths;
mod placeholder;
mod preview;
mod scan;
mod volumes;
mod watcher;

pub use exclusions::{
    ImageFormat, MAX_FILE_BYTES, classify_extension, is_excluded_dir_name, is_excluded_file_name,
};
pub use hashing::{hash_file, hash_invocation_count, hashed_byte_count};
pub use ingest::{
    PASS_VERSION, PRIORITY_BACKFILL, PRIORITY_GPU, PRIORITY_SCAN, PRIORITY_WATCHER, PassCounters,
    PassName, PassState, placeholder_sentinel,
};
pub use paths::{Availability, BestPath, PathRow, StaleReason};
pub use placeholder::{
    PlaceholderDetector, PlatformPlaceholderDetector, SharedSetPlaceholderDetector,
};
pub use preview::{
    ArtifactKind, DISPLAY_EDGE, EMBEDDED_ACCEPT_EDGE, EmbeddedOrientationReason,
    EmbeddedPreviewExtractor, ExtractedPreview, GENERATOR_VERSION, PreviewError, PreviewSource,
    RawlerExtractor, THUMB_EDGE, apply_exif_orientation, artifact_path,
    embedded_orientation_decision, oriented_dims,
};
pub use scan::{ClockShiftReport, ScanOptions, ScanReport};
pub use volumes::{
    FakeVolumeProbe, MARKER_FILENAME, PlatformIdKind, PlatformVolumeProbe, ProbedVolume,
    VolumeMarker, VolumeProbe, bare_platform_uuid, read_marker, verify_writable, write_marker,
};
pub use watcher::{
    DebounceConfig, DebounceCore, PipelineEffect, RawWatchEvent, RootWatcherHandle, Stability,
    WatchPipeline,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use thiserror::Error;

use crate::id::{ContentHash, UtcMillis};
use crate::metrics::{PipelineMetrics, StageSnapshot};
use crate::store::StoreError;

pub type VolumeId = String;
pub type RootId = String;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("watch error: {0}")]
    Watch(String),
    #[error("nested roots are forbidden: {0}")]
    NestedRoot(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("volume offline: {0}")]
    VolumeOffline(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// Cancellation for long operations (interrupt-safety tests drive this; a
/// real `kill -9` is strictly weaker than cancelling between work units plus
/// SQLite transactionality, since transactions are atomic either way).
pub type CancelFlag = Arc<AtomicBool>;

/// Injectable collaborators (defaults are the platform implementations).
pub struct LibraryOptions {
    pub probe: Arc<dyn VolumeProbe>,
    pub placeholders: Arc<dyn PlaceholderDetector>,
    pub extractor: Arc<dyn EmbeddedPreviewExtractor>,
}

impl Default for LibraryOptions {
    fn default() -> Self {
        Self {
            probe: Arc::new(PlatformVolumeProbe),
            placeholders: Arc::new(PlatformPlaceholderDetector),
            extractor: Arc::new(RawlerExtractor),
        }
    }
}

/// Strictly-increasing wall-clock timestamps: `(priority, enqueued_at)`
/// dequeue order (§10.3) and the ascending-size hash ordering (§1.2) need
/// distinct, ordered `enqueued_at` values even within one millisecond.
struct MonotonicMillis {
    last: Mutex<i64>,
}

impl MonotonicMillis {
    fn new() -> Self {
        Self {
            last: Mutex::new(0),
        }
    }

    fn now(&self) -> UtcMillis {
        let wall = UtcMillis::now().epoch_ms();
        let mut last = self.last.lock().expect("poisoned");
        let v = wall.max(*last + 1);
        *last = v;
        UtcMillis::from_epoch_ms(v)
    }
}

/// The library: one writer connection (all writes serialize through it), the
/// preview cache directory, and the injectable platform seams.
pub struct Library {
    db: Mutex<Connection>,
    cache_dir: PathBuf,
    probe: Arc<dyn VolumeProbe>,
    placeholders: Arc<dyn PlaceholderDetector>,
    extractor: Arc<dyn EmbeddedPreviewExtractor>,
    clock: MonotonicMillis,
    debug_log: Mutex<Vec<String>>,
    /// Ingest-stage timings (BACKLOG "measured, not vibes" — first slice).
    metrics: PipelineMetrics,
}

impl Library {
    /// Open the shared photoproof database (creating/migrating if necessary)
    /// with platform collaborators. `cache_dir` hosts the preview cache.
    pub fn open(
        db_path: impl AsRef<Path>,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, LibraryError> {
        Self::open_with(db_path, cache_dir, LibraryOptions::default())
    }

    pub fn open_with(
        db_path: impl AsRef<Path>,
        cache_dir: impl Into<PathBuf>,
        options: LibraryOptions,
    ) -> Result<Self, LibraryError> {
        let db_path = db_path.as_ref();
        // Schema + migrations (including LIBRARY_SCHEMA_SQL, user_version 3)
        // live in `store::schema`, which is private to the events engine;
        // opening the EventStore runs them. The library then opens its own
        // writer connection with the same §5.1 pragmas (flagged: a
        // `pub(crate) mod schema` would remove both the throwaway open and
        // the pragma duplicate below).
        drop(crate::store::EventStore::open(db_path)?);
        let conn = open_library_connection(db_path)?;
        // §10.2 crash recovery: every `running` row reverts to `pending`;
        // §10.5: `error` rows auto-retry on app restart.
        let recovered = ingest::recover_running(&conn)?;
        ingest::retry_errors(&conn)?;
        // §9.8 regeneration: a `generator_version` bump (compile-time
        // constant covering encoder, sizes, color pipeline) re-enqueues the
        // preview pass at backfill priority for every stale artifact.
        let regen = conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', not_before = NULL, priority = ?2
             WHERE pass_name = 'preview' AND state = 'done'
               AND image_hash IN (SELECT image_hash FROM preview_artifacts
                                  WHERE generator_version < ?1)",
            params![preview::GENERATOR_VERSION, ingest::PRIORITY_BACKFILL],
        )?;
        let cache_dir = cache_dir.into();
        // §9.8 crash hygiene: stranded temp files from a mid-write crash.
        let swept = preview::sweep_temp_files(&cache_dir)?;
        let lib = Self {
            db: Mutex::new(conn),
            cache_dir,
            probe: options.probe,
            placeholders: options.placeholders,
            extractor: options.extractor,
            clock: MonotonicMillis::new(),
            debug_log: Mutex::new(Vec::new()),
            metrics: PipelineMetrics::default(),
        };
        if recovered > 0 {
            lib.log(format!(
                "crash recovery: {recovered} running passes re-pended"
            ));
        }
        if regen > 0 {
            lib.log(format!(
                "generator_version bump: {regen} preview passes re-enqueued at backfill priority"
            ));
        }
        if swept > 0 {
            lib.log(format!("cache hygiene: {swept} temp files swept"));
        }
        Ok(lib)
    }

    /// §9.8 / §10.6: preview-cache size for settings and the debug panel.
    pub fn preview_cache_stats(&self) -> Result<(i64, i64), LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM preview_artifacts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn now(&self) -> UtcMillis {
        self.clock.now()
    }

    fn mint_ulid(&self) -> String {
        ulid::Ulid::new().to_string()
    }

    /// Debug-panel log line (§4.1 warnings, clock-shift events, exclusions).
    pub(crate) fn log(&self, msg: String) {
        self.debug_log.lock().expect("poisoned").push(msg);
    }

    /// Drain the debug log (tests assert "warning logged" through this).
    pub fn take_debug_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.debug_log.lock().expect("poisoned"))
    }

    /// Cumulative ingest-stage timings (debug panel; BACKLOG metrics).
    pub fn metrics_snapshot(&self) -> Vec<StageSnapshot> {
        self.metrics.snapshot()
    }

    // -----------------------------------------------------------------------
    // Volumes (§4)
    // -----------------------------------------------------------------------

    /// Resolve a probed mount to a volume identity per the §4.1 recipe
    /// (marker beats platform id beats heuristic), creating or updating the
    /// row and marking it online at the probed mount point.
    pub(crate) fn resolve_volume(&self, probed: &ProbedVolume) -> Result<VolumeId, LibraryError> {
        let now = self.now().to_rfc3339();
        let marker = volumes::read_marker(&probed.mount_point);
        let conn = self.db.lock().expect("poisoned");

        if let Some(marker) = &marker {
            let existing: Option<(String, Option<String>, Option<String>, String)> = conn
                .query_row(
                    "SELECT volume_id, platform_id, mount_point, state
                     FROM volumes WHERE marker_ulid = ?1",
                    params![marker.volume_ulid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((volume_id, platform_id, mount_point, state)) = existing {
                // Full-clone detection: the matched volume is online at a
                // *different* mount point that still carries the same marker
                // → this mount is a clone; it registers as a NEW volume with
                // a fresh marker (§4.1).
                let clone = state == "online"
                    && mount_point
                        .as_deref()
                        .is_some_and(|mp| Path::new(mp) != probed.mount_point)
                    && mount_point
                        .as_deref()
                        .and_then(|mp| volumes::read_marker(Path::new(mp)))
                        .is_some_and(|m| m.volume_ulid == marker.volume_ulid);
                if clone {
                    drop(conn);
                    self.log(format!(
                        "volume clone detected: marker {} mounted twice; registering {} as a new volume",
                        marker.volume_ulid,
                        probed.mount_point.display()
                    ));
                    return self.create_volume(probed, /* force_fresh_marker = */ true);
                }
                // Marker wins over platform ids (§4.1): a changed platform id
                // is adopted; any other row claiming it is cleared.
                if let Some(pid) = &probed.platform_id
                    && platform_id.as_deref() != Some(pid.as_str())
                {
                    conn.execute(
                        "UPDATE volumes SET platform_id = NULL
                             WHERE platform_id = ?1 AND volume_id <> ?2",
                        params![pid, volume_id],
                    )?;
                    conn.execute(
                        "UPDATE volumes SET platform_id = ?1, platform_kind = ?2
                             WHERE volume_id = ?3",
                        params![pid, probed.platform_kind.as_str(), volume_id],
                    )?;
                    self.log(format!(
                        "marker precedence: volume {volume_id} platform id changed to {pid} \
                             (marker {} wins)",
                        marker.volume_ulid
                    ));
                }
                self.mark_online_locked(&conn, &volume_id, probed, &now)?;
                return Ok(volume_id);
            }
            // Marker present but unknown to this DB: adopt the marker's ULID
            // as the volume identity — it survives DB rebuilds.
            drop(conn);
            return self.create_volume(probed, false);
        }
        // No marker: platform id (level 2), else heuristic (level 3).
        let platform_id = probed.platform_id.clone().unwrap_or_else(|| {
            volumes::heuristic_fingerprint(
                probed.fs_type.as_deref(),
                probed.label.as_deref(),
                probed.capacity_bytes,
            )
        });
        let kind = if probed.platform_id.is_some() {
            probed.platform_kind
        } else {
            PlatformIdKind::Heuristic
        };
        let existing: Option<VolumeId> = conn
            .query_row(
                "SELECT volume_id FROM volumes WHERE platform_id = ?1 AND platform_kind = ?2",
                params![platform_id, kind.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(volume_id) = existing {
            self.mark_online_locked(&conn, &volume_id, probed, &now)?;
            return Ok(volume_id);
        }
        // Backward compat: the probe may now emit a subvol-qualified
        // platform_id (`"UUID:/@home"`) but an existing row still stores the
        // bare UUID (`"UUID"`). Try a bare-UUID lookup and upgrade the row.
        if kind == PlatformIdKind::LinuxFsUuid {
            let bare = volumes::bare_platform_uuid(&platform_id);
            if bare != platform_id.as_str()
                && let Some(vid) = conn
                    .query_row(
                        "SELECT volume_id FROM volumes
                         WHERE platform_id = ?1 AND platform_kind = ?2",
                        params![bare, kind.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
            {
                conn.execute(
                    "UPDATE volumes SET platform_id = ?1 WHERE volume_id = ?2",
                    params![platform_id, &vid],
                )?;
                self.mark_online_locked(&conn, &vid, probed, &now)?;
                return Ok(vid);
            }
        }
        drop(conn);
        self.create_volume(probed, false)
    }

    fn create_volume(
        &self,
        probed: &ProbedVolume,
        force_fresh_marker: bool,
    ) -> Result<VolumeId, LibraryError> {
        let now = self.now().to_rfc3339();
        let marker = if force_fresh_marker {
            None
        } else {
            volumes::read_marker(&probed.mount_point)
        };
        let volume_id = marker
            .as_ref()
            .map(|m| m.volume_ulid.clone())
            .unwrap_or_else(|| self.mint_ulid());
        let (platform_id, kind) = match &probed.platform_id {
            Some(pid) => (Some(pid.clone()), probed.platform_kind),
            None => (
                Some(volumes::heuristic_fingerprint(
                    probed.fs_type.as_deref(),
                    probed.label.as_deref(),
                    probed.capacity_bytes,
                )),
                PlatformIdKind::Heuristic,
            ),
        };
        let conn = self.db.lock().expect("poisoned");
        conn.execute(
            "INSERT INTO volumes (volume_id, marker_ulid, platform_id, platform_kind, label,
                                  fs_type, capacity_bytes, read_only, state, mount_point,
                                  first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'online', ?9, ?10, ?10)",
            params![
                volume_id,
                marker.as_ref().map(|m| m.volume_ulid.clone()),
                platform_id,
                kind.as_str(),
                probed.label,
                probed.fs_type,
                probed.capacity_bytes,
                probed.read_only_flag as i64,
                probed.mount_point.to_string_lossy(),
                now,
            ],
        )?;
        drop(conn);
        if force_fresh_marker {
            self.maybe_write_marker(&volume_id, probed);
        }
        Ok(volume_id)
    }

    fn mark_online_locked(
        &self,
        conn: &Connection,
        volume_id: &str,
        probed: &ProbedVolume,
        now: &str,
    ) -> rusqlite::Result<()> {
        conn.execute(
            "UPDATE volumes SET state = 'online', mount_point = ?2, last_seen_at = ?3,
                                read_only = ?4, label = COALESCE(?5, label),
                                fs_type = COALESCE(?6, fs_type),
                                capacity_bytes = COALESCE(?7, capacity_bytes)
             WHERE volume_id = ?1",
            params![
                volume_id,
                probed.mount_point.to_string_lossy(),
                now,
                probed.read_only_flag as i64,
                probed.label,
                probed.fs_type,
                probed.capacity_bytes,
            ],
        )?;
        Ok(())
    }

    /// Marker policy (§4.1): written automatically on first ingest of any
    /// writable volume hosting a watched root; never on system roots;
    /// opportunistically on a later writable mount.
    fn maybe_write_marker(&self, volume_id: &str, probed: &ProbedVolume) {
        if probed.is_system_root {
            return;
        }
        let existing_marker: Option<String> = {
            let conn = self.db.lock().expect("poisoned");
            conn.query_row(
                "SELECT marker_ulid FROM volumes WHERE volume_id = ?1",
                params![volume_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        if existing_marker.is_some() && volumes::read_marker(&probed.mount_point).is_some() {
            return;
        }
        let now = self.now().to_rfc3339();
        match volumes::write_marker(&probed.mount_point, volume_id, &now) {
            Ok(()) => {
                let conn = self.db.lock().expect("poisoned");
                let _ = conn.execute(
                    "UPDATE volumes SET marker_ulid = ?2 WHERE volume_id = ?1",
                    params![volume_id, volume_id],
                );
            }
            Err(e) => self.log(format!(
                "marker write failed on {} (will retry on a writable mount): {e}",
                probed.mount_point.display()
            )),
        }
    }

    /// Startup / periodic mount probe (§4.2): match every known volume
    /// against the currently mounted set, flip states, update mount points,
    /// refresh read-only, and return the active roots of volumes that came
    /// online (each needs a reconciliation scan).
    pub fn probe_volumes(&self) -> Result<Vec<RootId>, LibraryError> {
        let mounts = self.probe.list_mounts()?;
        // Pre-read each mount's marker once.
        let mount_markers: Vec<(usize, Option<VolumeMarker>)> = mounts
            .iter()
            .enumerate()
            .map(|(i, m)| (i, volumes::read_marker(&m.mount_point)))
            .collect();
        type KnownVolume = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        );
        let known: Vec<KnownVolume> = {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare(
                "SELECT volume_id, marker_ulid, platform_id, platform_kind, state FROM volumes",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut claimed_mounts = vec![false; mounts.len()];
        let mut to_scan = Vec::new();
        for (volume_id, marker_ulid, platform_id, platform_kind, state) in known {
            // Marker first, then platform id.
            let mut matched: Option<usize> = None;
            if let Some(mu) = &marker_ulid {
                matched = mount_markers
                    .iter()
                    .find(|(i, m)| {
                        !claimed_mounts[*i] && m.as_ref().is_some_and(|m| &m.volume_ulid == mu)
                    })
                    .map(|(i, _)| *i);
            }
            if matched.is_none()
                && let Some(pid) = &platform_id
            {
                // Collect ALL candidates that match by platform id (exact or
                // backward-compat bare-UUID). When a stored bare-UUID row
                // matches both a bare mount and a subvol-qualified mount
                // (btrfs), prefer the qualified mount — it is unambiguous.
                let is_linux_fs = platform_kind.as_deref() == Some("linux-fsuuid");
                let candidates: Vec<usize> = mounts
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| {
                        if claimed_mounts[*i] || mount_markers[*i].1.is_some() {
                            return false;
                        }
                        if Some(m.platform_kind.as_str()) != platform_kind.as_deref() {
                            return false;
                        }
                        m.platform_id.as_deref() == Some(pid.as_str())
                            || (is_linux_fs
                                && m.platform_id.as_deref().is_some_and(|mp| {
                                    volumes::bare_platform_uuid(mp)
                                        == volumes::bare_platform_uuid(pid)
                                }))
                    })
                    .map(|(i, _)| i)
                    .collect();
                if candidates.len() == 1 {
                    matched = Some(candidates[0]);
                } else if candidates.len() > 1 {
                    let known_is_qualified = pid.contains(':');
                    let pick = if known_is_qualified {
                        candidates
                            .iter()
                            .find(|i| {
                                mounts[**i]
                                    .platform_id
                                    .as_deref()
                                    .is_some_and(|p| p.contains(':'))
                            })
                            .or_else(|| candidates.first())
                    } else {
                        candidates
                            .iter()
                            .find(|i| {
                                mounts[**i]
                                    .platform_id
                                    .as_deref()
                                    .is_some_and(|p| !p.contains(':'))
                            })
                            .or_else(|| candidates.first())
                    };
                    matched = pick.copied();
                }
            }
            // §4.1 level 3: a heuristic-identified volume (no marker, no
            // platform id at creation) re-matches by fingerprint against
            // mounts that ALSO lack both stronger identities. Ambiguity
            // (two identical-looking mounts) leaves the volume offline —
            // misbinding is worse than waiting for a marker.
            if matched.is_none()
                && platform_kind.as_deref() == Some("heuristic")
                && let Some(pid) = &platform_id
            {
                let candidates: Vec<usize> = mounts
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| {
                        !claimed_mounts[*i]
                            && mount_markers[*i].1.is_none()
                            && m.platform_id.is_none()
                            && volumes::heuristic_fingerprint(
                                m.fs_type.as_deref(),
                                m.label.as_deref(),
                                m.capacity_bytes,
                            ) == *pid
                    })
                    .map(|(i, _)| i)
                    .collect();
                if candidates.len() == 1 {
                    matched = Some(candidates[0]);
                }
            }
            match matched {
                Some(i) => {
                    claimed_mounts[i] = true;
                    let probed = &mounts[i];
                    let was_offline = state == "offline";
                    self.resolve_volume(probed)?;
                    self.refresh_read_only(&volume_id, probed)?;
                    self.maybe_write_marker_if_hosting_root(&volume_id, probed)?;
                    if was_offline {
                        to_scan.extend(self.active_roots_of(&volume_id)?);
                        // Re-queue errored ingest passes whose image sits on
                        // this volume — the error may have been caused by the
                        // volume being offline or misbound — and clear the
                        // offline-defer backoff on pending ones: the volume
                        // is BACK, waiting out a 10-minute not_before would
                        // be punishing the user for replugging.
                        let conn = self.db.lock().expect("poisoned");
                        let _ = conn.execute(
                            "UPDATE ingest_passes
                             SET state = 'pending', not_before = NULL
                             WHERE state IN ('error', 'pending')
                               AND image_hash IN (
                                 SELECT p.image_hash FROM paths p
                                 WHERE p.volume_id = ?1 AND p.state = 'active'
                               )",
                            params![volume_id],
                        );
                    }
                }
                None => {
                    if state == "online" {
                        let conn = self.db.lock().expect("poisoned");
                        conn.execute(
                            "UPDATE volumes SET state = 'offline', mount_point = NULL
                             WHERE volume_id = ?1",
                            params![volume_id],
                        )?;
                    }
                }
            }
        }
        Ok(to_scan)
    }

    /// §4.3: flags lie; verify with a create-and-delete probe in the watched
    /// root (or skip the probe when the volume hosts none).
    fn refresh_read_only(
        &self,
        volume_id: &str,
        probed: &ProbedVolume,
    ) -> Result<(), LibraryError> {
        let mut read_only = probed.read_only_flag;
        if !read_only
            && let Some(root_dir) = self.first_active_root_dir(volume_id, &probed.mount_point)?
        {
            read_only = !volumes::verify_writable(&root_dir);
        }
        let conn = self.db.lock().expect("poisoned");
        conn.execute(
            "UPDATE volumes SET read_only = ?2 WHERE volume_id = ?1",
            params![volume_id, read_only as i64],
        )?;
        Ok(())
    }

    fn maybe_write_marker_if_hosting_root(
        &self,
        volume_id: &str,
        probed: &ProbedVolume,
    ) -> Result<(), LibraryError> {
        let has_root = !self.active_roots_of(volume_id)?.is_empty();
        let marker_known: Option<String> = {
            let conn = self.db.lock().expect("poisoned");
            conn.query_row(
                "SELECT marker_ulid FROM volumes WHERE volume_id = ?1",
                params![volume_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten()
        };
        if has_root && marker_known.is_none() && !probed.read_only_flag {
            self.maybe_write_marker(volume_id, probed);
        }
        Ok(())
    }

    fn first_active_root_dir(
        &self,
        volume_id: &str,
        mount_point: &Path,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let rel: Option<String> = conn
            .query_row(
                "SELECT rel_path FROM roots WHERE volume_id = ?1 AND state = 'active'
                 ORDER BY root_id LIMIT 1",
                params![volume_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(rel.map(|r| join_rel(mount_point, &r)))
    }

    fn active_roots_of(&self, volume_id: &str) -> Result<Vec<RootId>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT root_id FROM roots WHERE volume_id = ?1 AND state = 'active' ORDER BY root_id",
        )?;
        let rows = stmt.query_map(params![volume_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn volume(&self, volume_id: &str) -> Result<Option<VolumeRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(conn
            .query_row(
                "SELECT volume_id, marker_ulid, platform_id, platform_kind, label, fs_type,
                        capacity_bytes, read_only, state, mount_point
                 FROM volumes WHERE volume_id = ?1",
                params![volume_id],
                volume_record,
            )
            .optional()?)
    }

    pub fn volumes(&self) -> Result<Vec<VolumeRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT volume_id, marker_ulid, platform_id, platform_kind, label, fs_type,
                    capacity_bytes, read_only, state, mount_point
             FROM volumes ORDER BY volume_id",
        )?;
        let rows = stmt.query_map([], volume_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -----------------------------------------------------------------------
    // Watched roots (§5)
    // -----------------------------------------------------------------------

    /// Register a watched root: resolve volume + relative path, write the
    /// marker if applicable, reject nesting, enqueue nothing — the caller
    /// runs `scan_root` for the initial scan.
    pub fn register_root(
        &self,
        dir: &Path,
        display_name: Option<&str>,
    ) -> Result<RootId, LibraryError> {
        let meta = std::fs::metadata(dir)?;
        if !meta.is_dir() {
            return Err(LibraryError::Invalid(format!(
                "not a directory: {}",
                dir.display()
            )));
        }
        let probed = self.probe.probe_path(dir)?;
        let volume_id = self.resolve_volume(&probed)?;
        let rel = rel_path_str(&probed.mount_point, dir).ok_or_else(|| {
            LibraryError::Invalid(format!(
                "path is not valid UTF-8 under its volume root: {}",
                dir.display()
            ))
        })?;
        // Nested roots are forbidden (§5): inside or above an existing
        // active root, on the same volume.
        {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn
                .prepare("SELECT rel_path FROM roots WHERE volume_id = ?1 AND state = 'active'")?;
            let existing: Vec<String> = stmt
                .query_map(params![volume_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for other in existing {
                if rel_contains(&other, &rel) || rel_contains(&rel, &other) {
                    return Err(LibraryError::NestedRoot(format!(
                        "'{rel}' overlaps existing active root '{other}'"
                    )));
                }
            }
        }
        let now = self.now().to_rfc3339();
        // Re-registering the exact location of a removed root revives that
        // row (§5: "re-registering an overlapping root later relinks
        // everything"; the §6 UNIQUE (volume_id, rel_path) makes the root's
        // location its identity). Flagged reading in the packet report.
        let revived: Option<RootId> = {
            let conn = self.db.lock().expect("poisoned");
            conn.query_row(
                "SELECT root_id FROM roots
                 WHERE volume_id = ?1 AND rel_path = ?2 AND state = 'removed'",
                params![volume_id, rel],
                |r| r.get(0),
            )
            .optional()?
        };
        let root_id = match revived {
            Some(root_id) => {
                let conn = self.db.lock().expect("poisoned");
                conn.execute(
                    "UPDATE roots SET state = 'active', removed_at = NULL,
                                      display_name = COALESCE(?2, display_name)
                     WHERE root_id = ?1",
                    params![root_id, display_name],
                )?;
                root_id
            }
            None => {
                let root_id = self.mint_ulid();
                let conn = self.db.lock().expect("poisoned");
                conn.execute(
                    "INSERT INTO roots (root_id, volume_id, rel_path, display_name, state, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
                    params![root_id, volume_id, rel, display_name, now],
                )?;
                root_id
            }
        };
        // Read-only verification probe in the new root (§4.3), then the
        // marker (§4.1: first ingest of a writable volume hosting a root).
        self.refresh_read_only(&volume_id, &probed)?;
        let read_only = self
            .volume(&volume_id)?
            .map(|v| v.read_only)
            .unwrap_or(true);
        if !read_only {
            self.maybe_write_marker(&volume_id, &probed);
        }
        // Cloud-sync advisory (§5.2): detection only; the root is allowed.
        if let Some(service) = sync_service_hint(dir) {
            self.log(format!(
                "cloud-sync advisory: root {} appears to live in {service}; sidecar writes \
                 will appear as synced revisions there",
                dir.display()
            ));
        }
        Ok(root_id)
    }

    /// Root removal (§5): state → removed, its path rows → stale
    /// (`root-removed`). Nothing else — journals kept, previews retained.
    pub fn remove_root(&self, root_id: &str) -> Result<(), LibraryError> {
        let now = self.now().to_rfc3339();
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE roots SET state = 'removed', removed_at = ?2
             WHERE root_id = ?1 AND state = 'active'",
            params![root_id, now],
        )?;
        if n == 0 {
            return Err(LibraryError::NotFound(format!("active root {root_id}")));
        }
        conn.execute(
            "UPDATE paths SET state = 'stale', stale_reason = 'root-removed', stale_since = ?2
             WHERE root_id = ?1 AND state = 'active'",
            params![root_id, now],
        )?;
        Ok(())
    }

    pub fn root(&self, root_id: &str) -> Result<Option<RootRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(conn
            .query_row(
                "SELECT root_id, volume_id, rel_path, display_name, state
                 FROM roots WHERE root_id = ?1",
                params![root_id],
                root_record,
            )
            .optional()?)
    }

    pub fn roots(&self) -> Result<Vec<RootRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT root_id, volume_id, rel_path, display_name, state FROM roots ORDER BY root_id",
        )?;
        let rows = stmt.query_map([], root_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The absolute directory of an active root on an online volume.
    pub(crate) fn root_location(
        &self,
        root_id: &str,
    ) -> Result<(RootRecord, VolumeRecord, PathBuf), LibraryError> {
        let root = self
            .root(root_id)?
            .ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
        let volume = self
            .volume(&root.volume_id)?
            .ok_or_else(|| LibraryError::NotFound(format!("volume {}", root.volume_id)))?;
        if !volume.online {
            return Err(LibraryError::VolumeOffline(root.volume_id.clone()));
        }
        let mount = volume
            .mount_point
            .clone()
            .ok_or_else(|| LibraryError::VolumeOffline(root.volume_id.clone()))?;
        let dir = join_rel(Path::new(&mount), &root.rel_path);
        Ok((root, volume, dir))
    }

    // -----------------------------------------------------------------------
    // Reconciliation (§7.3) and the single per-path algorithm (§7.2)
    // -----------------------------------------------------------------------

    pub fn scan_root(&self, root_id: &str, opts: &ScanOptions) -> Result<ScanReport, LibraryError> {
        scan::scan_root(self, root_id, opts)
    }

    /// Scan every active root on an online volume (startup, resume-from-
    /// sleep, the 6-hour tick).
    pub fn reconcile_all(
        &self,
        opts: &ScanOptions,
    ) -> Result<Vec<(RootId, ScanReport)>, LibraryError> {
        let roots = self.roots()?;
        let mut out = Vec::new();
        for root in roots {
            if root.state != "active" {
                continue;
            }
            match self.scan_root(&root.root_id, opts) {
                Ok(report) => out.push((root.root_id, report)),
                Err(LibraryError::VolumeOffline(_)) => {} // §11: waits out the remount
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Wake trigger (§7.3): watcher gaps across sleep are a canonical
    /// missed-event source.
    pub fn on_system_resume(&self) -> Result<Vec<(RootId, ScanReport)>, LibraryError> {
        self.probe_volumes()?;
        self.reconcile_all(&ScanOptions::default())
    }

    /// The 6-hour tick (§7.3, §10.5): error-row retry + the doctor's
    /// validate-and-heal sweep + full reconciliation. The doctor joined the
    /// tick per BACKLOG "Library doctor / self-check pass" (founder, dogfood
    /// round 3): mangled states keep happening; the library should HEAL on
    /// its own schedule, not just avoid poisoning.
    pub fn maintenance_tick(&self) -> Result<Vec<(RootId, ScanReport)>, LibraryError> {
        {
            let conn = self.db.lock().expect("poisoned");
            ingest::retry_errors(&conn)?;
        }
        self.doctor()?;
        self.probe_volumes()?;
        self.reconcile_all(&ScanOptions::default())
    }

    /// "Rebuild previews…" (BACKLOG, founder dogfood round 3): the
    /// `generator_version` machinery's MANUAL trigger — semantics
    /// deliberately different from Rescan (which reconciles files↔index and
    /// enqueues only MISSING passes, §7.3). This re-pends the preview pass
    /// for every image with an active path under the root, with a fresh
    /// retry budget, at backfill priority (§10.3 — recovery work never
    /// starves live-watcher discoveries). Regeneration is idempotent by
    /// construction: `write_artifacts` overwrites atomically (§9.8).
    ///
    /// `running` rows are left alone: that image is regenerating RIGHT NOW,
    /// and flipping the row back to pending mid-flight would let a second
    /// worker claim it concurrently. Returns the number of rows re-pended.
    pub fn rebuild_previews(&self, root_id: &str) -> Result<usize, LibraryError> {
        // A vanished root is caller error, not a quiet zero.
        self.root(root_id)?
            .ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
        let conn = self.db.lock().expect("poisoned");
        // Recycled rows (done/error/skipped) re-enter the queue AT backfill
        // priority — the same shape as the §9.8 generator_version bump.
        // Rows already pending keep their place if it is BETTER: the §10.3
        // promotion rule never demotes, so a P0 watcher discovery or a P1
        // scan enqueue is not pushed behind backfill work it already beats.
        let n = conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', attempts = 0, not_before = NULL,
                 error = NULL,
                 priority = CASE WHEN state = 'pending'
                                 THEN MIN(priority, ?2) ELSE ?2 END
             WHERE pass_name = 'preview' AND state != 'running'
               AND image_hash IN (SELECT image_hash FROM paths
                                  WHERE root_id = ?1 AND state = 'active')",
            params![root_id, ingest::PRIORITY_BACKFILL],
        )?;
        if n > 0 {
            self.log(format!("rebuild previews: {n} passes re-pended"));
        }
        Ok(n)
    }

    /// Library doctor v1 (BACKLOG "Library doctor / self-check pass"):
    /// validate the index against reality and repair what it can,
    /// CONSERVATIVELY — v1 re-pends and counts, it never deletes rows:
    ///
    /// - `done` preview passes whose thumb OR display artifact file is
    ///   missing on disk (a cache directory mangled outside the app) →
    ///   re-pend at backfill priority with a fresh budget; the next drain
    ///   regenerates (idempotent, §9.8).
    /// - stale path rows whose image has NO surviving active path →
    ///   COUNTED only (the §7.2 move-correlation window may still claim
    ///   them, and rows are cheap; sweeping is a later, bolder doctor).
    /// - stranded preview temp files → swept (`sweep_temp_files`, §9.8
    ///   crash hygiene — the same sweep `open_with` runs at startup). A
    ///   sweep racing a mid-write pass at worst fails that one rename;
    ///   the pass retries as transient and finals are never torn.
    pub fn doctor(&self) -> Result<DoctorReport, LibraryError> {
        // Snapshot the done rows, then probe the filesystem OUTSIDE the DB
        // lock: artifact existence checks on a big library are thousands of
        // stats, and the writer connection must not stall behind them.
        let done: Vec<String> = {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare(
                "SELECT image_hash FROM ingest_passes
                 WHERE pass_name = 'preview' AND state = 'done'",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut missing: Vec<String> = Vec::new();
        for h in done {
            let Ok(hash) = ContentHash::from_hex(&h) else {
                continue; // sentinel/garbage hashes are not preview rows
            };
            let intact = [ArtifactKind::Thumb, ArtifactKind::Display]
                .iter()
                .all(|&k| preview::artifact_path(&self.cache_dir, &hash, k).exists());
            if !intact {
                missing.push(h);
            }
        }
        let mut repended = 0usize;
        if !missing.is_empty() {
            let conn = self.db.lock().expect("poisoned");
            for h in &missing {
                // `state = 'done'` re-checked: a drain may have raced us.
                repended += conn.execute(
                    "UPDATE ingest_passes
                     SET state = 'pending', attempts = 0, not_before = NULL,
                         error = NULL, priority = ?2
                     WHERE image_hash = ?1 AND pass_name = 'preview'
                       AND state = 'done'",
                    params![h, ingest::PRIORITY_BACKFILL],
                )?;
            }
        }
        let stale_orphans: usize = {
            let conn = self.db.lock().expect("poisoned");
            conn.query_row(
                "SELECT COUNT(*) FROM paths s
                 WHERE s.state = 'stale'
                   AND NOT EXISTS (SELECT 1 FROM paths a
                                   WHERE a.image_hash = s.image_hash
                                     AND a.state = 'active')",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize
        };
        let temps_swept = preview::sweep_temp_files(&self.cache_dir)?;
        let report = DoctorReport {
            repended,
            stale_orphans,
            temps_swept,
        };
        if report.repended > 0 || report.stale_orphans > 0 || report.temps_swept > 0 {
            tracing::info!(
                repended = report.repended,
                stale_orphans = report.stale_orphans,
                temps_swept = report.temps_swept,
                "library doctor"
            );
            self.log(format!(
                "doctor: {} previews re-pended, {} orphaned stale paths, {} temp files swept",
                report.repended, report.stale_orphans, report.temps_swept
            ));
        }
        Ok(report)
    }

    /// §7.2 for one observed (stable) file. `move_window_start` bounds the
    /// remove→create move-correlation flip.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_file(
        &self,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        abs_path: &Path,
        size: i64,
        mtime_ns: i64,
        priority: i64,
        tolerance_ns: i64,
        move_window_start: UtcMillis,
    ) -> Result<Observed, LibraryError> {
        let existing = {
            let conn = self.db.lock().expect("poisoned");
            paths::active_row_at(&conn, volume_id, rel_path)?
        };
        match existing {
            Some(row) => {
                if row.size == size && (row.mtime_ns - mtime_ns).abs() <= tolerance_ns {
                    let conn = self.db.lock().expect("poisoned");
                    paths::touch_verified(&conn, &row.path_id, self.now())?;
                    return Ok(Observed::FastPath);
                }
                let (hash, hashed_size) = hashing::hash_file(abs_path)?;
                if hash == row.image_hash {
                    let conn = self.db.lock().expect("poisoned");
                    paths::update_size_mtime(
                        &conn,
                        &row.path_id,
                        hashed_size as i64,
                        mtime_ns,
                        self.now(),
                    )?;
                    Ok(Observed::Updated)
                } else {
                    // §1.3 in-place change protocol.
                    self.supersede_tx(
                        &row.path_id,
                        &hash,
                        hashed_size as i64,
                        mtime_ns,
                        volume_id,
                        root_id,
                        rel_path,
                        priority,
                    )?;
                    Ok(Observed::Superseded {
                        old: row.image_hash,
                        new: hash,
                    })
                }
            }
            None => {
                // §5 re-registration fast path: a `root-removed` stale row at
                // this location whose size+mtime still match relinks with
                // zero hashing (the hash is the one the row carries).
                if let Some(stale) =
                    self.reactivatable_row(volume_id, rel_path, size, mtime_ns, tolerance_ns)?
                {
                    let conn = self.db.lock().expect("poisoned");
                    paths::reactivate(&conn, &stale.path_id, root_id.unwrap_or(""), self.now())?;
                    return Ok(Observed::Relinked(stale.image_hash));
                }
                let (hash, hashed_size) = hashing::hash_file(abs_path)?;
                let known = self.image_exists(&hash)?;
                if known {
                    self.relink_tx(
                        &hash,
                        volume_id,
                        root_id,
                        rel_path,
                        hashed_size as i64,
                        mtime_ns,
                        move_window_start,
                    )?;
                    Ok(Observed::Relinked(hash))
                } else {
                    self.new_image_tx(
                        &hash,
                        hashed_size as i64,
                        volume_id,
                        root_id,
                        rel_path,
                        mtime_ns,
                        priority,
                    )?;
                    Ok(Observed::NewImage(hash))
                }
            }
        }
    }

    /// §7.2 step R: mark the active row stale (`deleted`); a later relink of
    /// the same hash within the correlation window flips it to `moved`. The
    /// reverse order (create observed before the remove — the copy-then-
    /// delete move pattern) is correlated here: the hash already gained an
    /// active path inside the window, so the reason is `moved` immediately.
    pub(crate) fn observe_removed(
        &self,
        volume_id: &str,
        rel_path: &str,
        move_window_start: UtcMillis,
    ) -> Result<bool, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        match paths::active_row_at(&conn, volume_id, rel_path)? {
            Some(row) => {
                paths::mark_stale(&conn, &row.path_id, StaleReason::Deleted, self.now())?;
                let relinked_recently: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM paths
                     WHERE image_hash = ?1 AND state = 'active' AND first_seen_at >= ?2",
                    params![row.image_hash.as_str(), move_window_start.to_rfc3339()],
                    |r| r.get(0),
                )?;
                if relinked_recently > 0 {
                    conn.execute(
                        "UPDATE paths SET stale_reason = 'moved' WHERE path_id = ?1",
                        params![row.path_id],
                    )?;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn reactivatable_row(
        &self,
        volume_id: &str,
        rel_path: &str,
        size: i64,
        mtime_ns: i64,
        tolerance_ns: i64,
    ) -> Result<Option<PathRow>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT path_id, image_hash, volume_id, root_id, rel_path, size, mtime_ns,
                    state, stale_reason
             FROM paths
             WHERE volume_id = ?1 AND rel_path = ?2 AND state = 'stale'
               AND stale_reason = 'root-removed'
             ORDER BY stale_since DESC",
        )?;
        let rows: Vec<PathRow> = stmt
            .query_map(params![volume_id, rel_path], paths::row_to_path)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .find(|r| r.size == size && (r.mtime_ns - mtime_ns).abs() <= tolerance_ns))
    }

    pub(crate) fn image_exists(&self, hash: &ContentHash) -> Result<bool, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(image_exists_on(&conn, hash)?)
    }

    /// Relink (§7.4): insert/activate a path row for an existing hash. Never
    /// touches images, events, previews, or sidecar content.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn relink_tx(
        &self,
        hash: &ContentHash,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        size: i64,
        mtime_ns: i64,
        move_window_start: UtcMillis,
    ) -> Result<(), LibraryError> {
        let mut conn = self.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        self.relink_in_tx(
            &tx,
            hash,
            volume_id,
            root_id,
            rel_path,
            size,
            mtime_ns,
            move_window_start,
        )?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn relink_in_tx(
        &self,
        tx: &Transaction<'_>,
        hash: &ContentHash,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        size: i64,
        mtime_ns: i64,
        move_window_start: UtcMillis,
    ) -> Result<(), LibraryError> {
        let path_id = self.mint_ulid();
        let now = self.now();
        paths::insert_active(
            tx, &path_id, hash, volume_id, root_id, rel_path, size, mtime_ns, now,
        )?;
        paths::correlate_move(tx, hash, move_window_start)?;
        ingest::clear_placeholder(tx, volume_id, rel_path)?;
        Ok(())
    }

    /// New image: `images` row + `hash` done-row + sibling enqueues + the
    /// first path row — one transaction (§10.4).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_image_tx(
        &self,
        hash: &ContentHash,
        byte_size: i64,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        mtime_ns: i64,
        priority: i64,
    ) -> Result<(), LibraryError> {
        let mut conn = self.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        self.new_image_in_tx(
            &tx, hash, byte_size, volume_id, root_id, rel_path, mtime_ns, priority,
        )?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn new_image_in_tx(
        &self,
        tx: &Transaction<'_>,
        hash: &ContentHash,
        byte_size: i64,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        mtime_ns: i64,
        priority: i64,
    ) -> Result<(), LibraryError> {
        let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
        let (format, raw_subtype) = classify_extension(file_name).ok_or_else(|| {
            LibraryError::Invalid(format!("off-allowlist file reached ingest: {rel_path}"))
        })?;
        let path_id = self.mint_ulid();
        let now = self.now();
        tx.execute(
            "INSERT INTO images (image_hash, byte_size, format, raw_subtype, exif_orientation,
                                 first_ingested_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(image_hash) DO NOTHING",
            params![
                hash.as_str(),
                byte_size,
                format.as_str(),
                raw_subtype,
                now.to_rfc3339()
            ],
        )?;
        // The hash pass row is written `done` atomically with the insert
        // (§10.1 — exists for uniform progress reporting).
        ingest::enqueue(
            tx,
            hash,
            PassName::Hash,
            PassState::Done,
            priority,
            None,
            now,
        )?;
        ingest::enqueue(
            tx,
            hash,
            PassName::Exif,
            PassState::Pending,
            priority,
            None,
            now,
        )?;
        match format {
            ImageFormat::Heic => {
                // §9.5: HEIC preview generation is deferred to the
                // libheif-capable backfill; placeholder until then.
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::Preview,
                    PassState::Skipped,
                    priority,
                    Some("deferred-heic"),
                    now,
                )?;
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::FullRawDecode,
                    PassState::Pending,
                    PRIORITY_BACKFILL,
                    None,
                    now,
                )?;
            }
            ImageFormat::Raw => {
                // The preview pass routes full-raw-decode (§9.3): pending on
                // threshold miss / no preview, skipped when the embedded
                // preview suffices.
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::Preview,
                    PassState::Pending,
                    priority,
                    None,
                    now,
                )?;
            }
            _ => {
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::Preview,
                    PassState::Pending,
                    priority,
                    None,
                    now,
                )?;
                // §10.1: `skipped` marks structurally inapplicable work so
                // progress math stays honest.
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::FullRawDecode,
                    PassState::Skipped,
                    PRIORITY_BACKFILL,
                    Some("inapplicable"),
                    now,
                )?;
            }
        }
        paths::insert_active(
            tx, &path_id, hash, volume_id, root_id, rel_path, byte_size, mtime_ns, now,
        )?;
        ingest::clear_placeholder(tx, volume_id, rel_path)?;
        Ok(())
    }

    /// §1.3 in-place change protocol, one transaction: old path row → stale
    /// (`superseded`), new active row binds the path to the new hash, new
    /// image + passes if the hash is unseen. The old image row, journal,
    /// previews, and sidecar content are untouched.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn supersede_tx(
        &self,
        old_path_id: &str,
        new_hash: &ContentHash,
        size: i64,
        mtime_ns: i64,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        priority: i64,
    ) -> Result<(), LibraryError> {
        let mut conn = self.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        paths::mark_stale(&tx, old_path_id, StaleReason::Superseded, self.now())?;
        if image_exists_on(&tx, new_hash)? {
            self.relink_in_tx(
                &tx,
                new_hash,
                volume_id,
                root_id,
                rel_path,
                size,
                mtime_ns,
                UtcMillis::from_epoch_ms(0),
            )?;
        } else {
            self.new_image_in_tx(
                &tx, new_hash, size, volume_id, root_id, rel_path, mtime_ns, priority,
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a placeholder sighting (§5.2): never read, never hashed; a
    /// skipped `hash` row with a `placeholder` error-code, re-checked at
    /// each reconciliation.
    pub(crate) fn record_placeholder(
        &self,
        volume_id: &str,
        rel_path: &str,
    ) -> Result<(), LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        ingest::record_placeholder(&conn, volume_id, rel_path, self.now())?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Queue processing (§10)
    // -----------------------------------------------------------------------

    /// Drain runnable pending passes (exif + preview — the M1 workers).
    /// `full-raw-decode` and the model passes stay queued by design: the
    /// queue knows their pass kinds; their workers are later milestones.
    ///
    /// Items drain in WAVES on the worker pool (one wave = one claim batch
    /// = pool width): decode/resize/encode run truly parallel while every
    /// DB touch stays serialized on the connection mutex exactly as
    /// before. Claim order still follows queue priority — a wave claims
    /// contiguously under one lock — and cancellation/max_items are
    /// checked between waves, so cancel latency is bounded by one wave.
    pub fn process_queue(&self, opts: &QueueOptions) -> Result<QueueReport, LibraryError> {
        use rayon::prelude::*;
        let drain_started = std::time::Instant::now();
        let mut report = QueueReport::default();
        loop {
            if let Some(cancel) = &opts.cancel
                && cancel.load(std::sync::atomic::Ordering::Relaxed)
            {
                report.cancelled = true;
                break;
            }
            let width = worker_pool().current_num_threads().max(1);
            let wave_cap = match opts.max_items {
                Some(max) => width.min(max.saturating_sub(report.processed)),
                None => width,
            };
            if wave_cap == 0 {
                break;
            }
            let items: Vec<ingest::QueueItem> = self.metrics.queue_claim.time(
                || -> Result<_, LibraryError> {
                    let conn = self.db.lock().expect("poisoned");
                    let mut claimed = Vec::with_capacity(wave_cap);
                    while claimed.len() < wave_cap {
                        match ingest::claim_next(&conn, self.now())? {
                            Some(item) => claimed.push(item),
                            None => break,
                        }
                    }
                    Ok(claimed)
                },
            )?;
            if items.is_empty() {
                break;
            }
            report.processed += items.len();
            // Per-item reports merge after the wave; the first hard error
            // (DB/IO plumbing — per-item pass failures are RECORDED, not
            // returned) propagates after the wave's successes are counted,
            // mirroring the sequential loop's abort semantics.
            let results: Vec<Result<QueueReport, LibraryError>> = worker_pool().install(|| {
                items
                    .par_iter()
                    .map(|item| {
                        let mut local = QueueReport::default();
                        self.run_pass(item, &mut local).map(|()| local)
                    })
                    .collect()
            });
            let mut first_err = None;
            for r in results {
                match r {
                    Ok(local) => report.absorb(&local),
                    Err(e) => first_err = first_err.or(Some(e)),
                }
            }
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        if report.processed > 0 {
            tracing::debug!(
                processed = report.processed,
                done = report.done,
                errors = report.errors,
                skipped = report.skipped,
                transient = report.transient_retries,
                elapsed_ms = drain_started.elapsed().as_millis() as u64,
                "ingest queue drain"
            );
        }
        Ok(report)
    }

    fn run_pass(
        &self,
        item: &ingest::QueueItem,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let image = match self.image(&item.image_hash)? {
            Some(image) => image,
            None => {
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, "missing-image-row", false, self.now())?;
                report.errors += 1;
                return Ok(());
            }
        };
        // Resolve a readable original (§3.1); offline = ordinary transient.
        let located = {
            let conn = self.db.lock().expect("poisoned");
            paths::best_path(&conn, &item.image_hash)?
        };
        let abs = located.as_ref().and_then(|bp| {
            if bp.online {
                bp.mount_point
                    .as_deref()
                    .map(|mp| join_rel(Path::new(mp), &bp.row.rel_path))
            } else {
                None
            }
        });
        let Some(abs) = abs else {
            // Not a strike against the file: defer without burning an
            // attempt (a flapping volume once killed two-thirds of a
            // folder's passes at the lifetime cap — founder machine,
            // June 2026).
            let conn = self.db.lock().expect("poisoned");
            ingest::defer_offline(&conn, item, self.now())?;
            report.transient_retries += 1;
            return Ok(());
        };
        match item.pass {
            PassName::Exif => self
                .metrics
                .exif_pass
                .time(|| self.run_exif_pass(item, &image, &abs, report)),
            PassName::Preview => self
                .metrics
                .preview_pass
                .time(|| self.run_preview_pass(item, &image, &abs, report)),
            _ => {
                // No M1 worker; claim_next never returns these.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, "no-worker", false, self.now())?;
                report.errors += 1;
                Ok(())
            }
        }
    }

    fn run_exif_pass(
        &self,
        item: &ingest::QueueItem,
        image: &ImageRecord,
        abs: &Path,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        match metadata::extract(abs, image.format) {
            Ok(subset) => {
                let conn = self.db.lock().expect("poisoned");
                conn.execute(
                    "UPDATE images SET
                       pixel_width = COALESCE(?2, pixel_width),
                       pixel_height = COALESCE(?3, pixel_height),
                       exif_orientation = ?4,
                       capture_ts = ?5, capture_tz_offset = ?6,
                       camera_make = ?7, camera_model = ?8, lens_model = ?9,
                       focal_length_mm = ?10, iso = ?11, f_number = ?12,
                       exposure_time = ?13, gps_lat = ?14, gps_lon = ?15
                     WHERE image_hash = ?1",
                    params![
                        item.image_hash.as_str(),
                        subset.pixel_width,
                        subset.pixel_height,
                        subset.orientation,
                        subset.capture_ts,
                        subset.capture_tz_offset,
                        subset.camera_make,
                        subset.camera_model,
                        subset.lens_model,
                        subset.focal_length_mm,
                        subset.iso,
                        subset.f_number,
                        subset.exposure_time,
                        subset.gps_lat,
                        subset.gps_lon,
                    ],
                )?;
                ingest::mark_done(&conn, item, self.now())?;
                report.done += 1;
            }
            Err(e) => {
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &format!("io: {e}"), true, self.now())?;
                report.transient_retries += 1;
            }
        }
        Ok(())
    }

    fn run_preview_pass(
        &self,
        item: &ingest::QueueItem,
        image: &ImageRecord,
        abs: &Path,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        match image.format {
            ImageFormat::Heic => {
                // Structurally deferred in M1 (§9.5); enqueue-time writes
                // `skipped`, so a claimed row here is a stray — keep honest.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_skipped(&conn, item, "deferred-heic", self.now())?;
                report.skipped += 1;
                Ok(())
            }
            ImageFormat::Raw => self.run_preview_pass_raw(item, abs, report),
            _ => self.run_preview_pass_original(item, image, abs, report),
        }
    }

    fn run_preview_pass_original(
        &self,
        item: &ingest::QueueItem,
        image: &ImageRecord,
        abs: &Path,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let decoded = self
            .metrics
            .decode
            .time(|| preview::decode_original_display_oriented(abs, image.exif_orientation));
        match decoded {
            Ok((img, _orientation)) => {
                let artifacts = preview::write_artifacts(
                    &self.cache_dir,
                    &item.image_hash,
                    &img,
                    &self.metrics,
                )?;
                self.metrics.db_record.time(|| -> Result<_, LibraryError> {
                    let conn = self.db.lock().expect("poisoned");
                    self.record_artifacts_locked(
                        &conn,
                        &item.image_hash,
                        &artifacts,
                        PreviewSource::Original,
                        false,
                    )?;
                    ingest::mark_done(&conn, item, self.now())?;
                    Ok(())
                })?;
                report.done += 1;
                report.completed_previews.push(item.image_hash.clone());
                Ok(())
            }
            Err(e) => self.fail_preview(item, e, report),
        }
    }

    fn run_preview_pass_raw(
        &self,
        item: &ingest::QueueItem,
        abs: &Path,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let extracted = match self.metrics.raw_extract.time(|| self.extractor.extract(abs)) {
            Ok(x) => x,
            Err(e) => return self.fail_preview(item, e, report),
        };
        let now = self.now();
        match extracted {
            None => {
                // No embedded preview at all (§9.3): UI placeholder;
                // full-raw-decode at elevated backfill priority.
                let conn = self.db.lock().expect("poisoned");
                ingest::enqueue(
                    &conn,
                    &item.image_hash,
                    PassName::FullRawDecode,
                    PassState::Pending,
                    PRIORITY_SCAN,
                    None,
                    now,
                )?;
                ingest::mark_skipped(&conn, item, "no-embedded-preview", now)?;
                report.skipped += 1;
                Ok(())
            }
            Some(extracted) => {
                let raw_dims = (extracted.raw_width, extracted.raw_height);
                let (oriented, _applied, reason) = preview::orient_embedded_preview(extracted);
                if matches!(reason, EmbeddedOrientationReason::SquareDefault) {
                    self.log(format!(
                        "embedded-preview orientation: square-ish geometry on {}, defaulted to \
                         the EXIF tag (§9.3.1 fixture-coverage log)",
                        item.image_hash
                    ));
                }
                use image::GenericImageView;
                let (pw, ph) = oriented.dimensions();
                let meets_threshold = pw.max(ph) >= EMBEDDED_ACCEPT_EDGE;
                let artifacts = preview::write_artifacts(
                    &self.cache_dir,
                    &item.image_hash,
                    &oriented,
                    &self.metrics,
                )?;
                let db_started = std::time::Instant::now();
                let conn = self.db.lock().expect("poisoned");
                self.record_artifacts_locked(
                    &conn,
                    &item.image_hash,
                    &artifacts,
                    PreviewSource::Embedded,
                    !meets_threshold,
                )?;
                if let (Some(w), Some(h)) = raw_dims {
                    conn.execute(
                        "UPDATE images SET pixel_width = COALESCE(pixel_width, ?2),
                                           pixel_height = COALESCE(pixel_height, ?3)
                         WHERE image_hash = ?1",
                        params![item.image_hash.as_str(), w, h],
                    )?;
                }
                if meets_threshold {
                    // §9.4: never flagged, skipped by the backfill — doubly
                    // load-bearing for images carrying journal strokes.
                    ingest::enqueue(
                        &conn,
                        &item.image_hash,
                        PassName::FullRawDecode,
                        PassState::Skipped,
                        PRIORITY_BACKFILL,
                        Some("threshold-met"),
                        now,
                    )?;
                } else {
                    // Threshold miss: small artifacts now beat a placeholder;
                    // flag and queue the upgrade (§9.3). Strokes promote it
                    // above plain P2 backfills (§10.3).
                    let priority = if self.image_has_strokes_locked(&conn, &item.image_hash)? {
                        PRIORITY_SCAN
                    } else {
                        PRIORITY_BACKFILL
                    };
                    ingest::enqueue(
                        &conn,
                        &item.image_hash,
                        PassName::FullRawDecode,
                        PassState::Pending,
                        priority,
                        None,
                        now,
                    )?;
                    ingest::promote(&conn, &item.image_hash, PassName::FullRawDecode, priority)?;
                }
                ingest::mark_done(&conn, item, now)?;
                self.metrics.db_record.record(db_started.elapsed());
                report.done += 1;
                report.completed_previews.push(item.image_hash.clone());
                Ok(())
            }
        }
    }

    fn fail_preview(
        &self,
        item: &ingest::QueueItem,
        e: PreviewError,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let transient = matches!(e, PreviewError::Io(_));
        tracing::warn!(
            hash = %item.image_hash,
            transient,
            error = %e,
            "preview pass failed"
        );
        let conn = self.db.lock().expect("poisoned");
        ingest::mark_failed(&conn, item, &e.to_string(), transient, self.now())?;
        if transient {
            report.transient_retries += 1;
        } else {
            report.errors += 1;
        }
        Ok(())
    }

    fn record_artifacts_locked(
        &self,
        conn: &Connection,
        hash: &ContentHash,
        artifacts: &[preview::GeneratedArtifact],
        source: PreviewSource,
        needs_full_decode: bool,
    ) -> rusqlite::Result<()> {
        let now = self.now().to_rfc3339();
        for a in artifacts {
            conn.execute(
                "INSERT INTO preview_artifacts
                   (image_hash, kind, source, width, height, bytes, format,
                    needs_full_decode, generator_version, generated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'webp', ?7, ?8, ?9)
                 ON CONFLICT(image_hash, kind) DO UPDATE SET
                   source = ?3, width = ?4, height = ?5, bytes = ?6,
                   needs_full_decode = ?7, generator_version = ?8, generated_at = ?9",
                params![
                    hash.as_str(),
                    a.kind.as_str(),
                    source.as_str(),
                    a.width,
                    a.height,
                    a.bytes,
                    needs_full_decode as i64,
                    preview::GENERATOR_VERSION,
                    now,
                ],
            )?;
        }
        Ok(())
    }

    /// Promotion-rule input (§10.3): live strokes from the events engine's
    /// derived stats (P1.1's `image_journal_stats`).
    fn image_has_strokes_locked(
        &self,
        conn: &Connection,
        hash: &ContentHash,
    ) -> rusqlite::Result<bool> {
        let n: Option<i64> = conn
            .query_row(
                "SELECT has_strokes FROM image_journal_stats WHERE image_hash = ?1",
                params![hash.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(n.unwrap_or(0) != 0)
    }

    pub fn pass_counters(
        &self,
    ) -> Result<std::collections::BTreeMap<(String, i64), PassCounters>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(ingest::pass_counters(&conn)?)
    }

    // -----------------------------------------------------------------------
    // Watcher entry points (§7.1)
    // -----------------------------------------------------------------------

    /// A deterministic watch pipeline for one root (the §7.1 state machine
    /// with an injected clock). The notify-backed thread drives the same
    /// pipeline with the real clock.
    pub fn watch_pipeline(
        self: &Arc<Self>,
        root_id: &str,
        cfg: DebounceConfig,
    ) -> Result<WatchPipeline, LibraryError> {
        watcher::WatchPipeline::new(Arc::clone(self), root_id, cfg)
    }

    /// Start a live `notify` watcher for a root. Errors/overflow trigger an
    /// immediate scan and degrade the root to polled mode (§7.1).
    pub fn start_watcher(
        self: &Arc<Self>,
        root_id: &str,
    ) -> Result<watcher::RootWatcherHandle, LibraryError> {
        watcher::start_root_watcher(self, root_id)
    }

    // -----------------------------------------------------------------------
    // Queries (availability §8, best path §3.1, records)
    // -----------------------------------------------------------------------

    pub fn availability(&self, hash: &ContentHash) -> Result<Availability, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(paths::availability(&conn, hash)?)
    }

    pub fn best_path(&self, hash: &ContentHash) -> Result<Option<BestPath>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(paths::best_path(&conn, hash)?)
    }

    pub fn dormant_prior_versions(
        &self,
        volume_id: &str,
        rel_path: &str,
    ) -> Result<Vec<ContentHash>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(paths::dormant_prior_versions(&conn, volume_id, rel_path)?)
    }

    pub fn paths_for_image(&self, hash: &ContentHash) -> Result<Vec<PathRow>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(paths::rows_for_image(&conn, hash)?)
    }

    pub fn image_count(&self) -> Result<i64, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(conn.query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))?)
    }

    pub fn image_hashes(&self) -> Result<Vec<ContentHash>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare("SELECT image_hash FROM images ORDER BY image_hash")?;
        let rows = stmt.query_map([], |r| {
            let h: String = r.get(0)?;
            ContentHash::from_hex(&h).map_err(|_| rusqlite::Error::InvalidQuery)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn image(&self, hash: &ContentHash) -> Result<Option<ImageRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(conn
            .query_row(
                "SELECT image_hash, byte_size, format, raw_subtype, pixel_width, pixel_height,
                        exif_orientation, capture_ts, capture_tz_offset, camera_make,
                        camera_model, lens_model, focal_length_mm, iso, f_number,
                        exposure_time, gps_lat, gps_lon, first_ingested_at
                 FROM images WHERE image_hash = ?1",
                params![hash.as_str()],
                image_record,
            )
            .optional()?)
    }

    pub fn preview_artifact(
        &self,
        hash: &ContentHash,
        kind: ArtifactKind,
    ) -> Result<Option<ArtifactRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let rec = conn
            .query_row(
                "SELECT source, width, height, bytes, needs_full_decode, generator_version
                 FROM preview_artifacts WHERE image_hash = ?1 AND kind = ?2",
                params![hash.as_str(), kind.as_str()],
                |r| {
                    Ok(ArtifactRecord {
                        kind,
                        source: match r.get::<_, String>(0)?.as_str() {
                            "embedded" => PreviewSource::Embedded,
                            "full-decode" => PreviewSource::FullDecode,
                            _ => PreviewSource::Original,
                        },
                        width: r.get(1)?,
                        height: r.get(2)?,
                        bytes: r.get(3)?,
                        needs_full_decode: r.get::<_, i64>(4)? != 0,
                        generator_version: r.get(5)?,
                        file: PathBuf::new(),
                    })
                },
            )
            .optional()?;
        Ok(rec.map(|mut r| {
            r.file = preview::artifact_path(&self.cache_dir, hash, kind);
            r
        }))
    }

    /// Test/maintenance: drop artifact rows + files so the preview pass can
    /// be exercised again ("clear preview cache" command, §9.8).
    pub fn clear_preview_cache(&self, hash: &ContentHash) -> Result<(), LibraryError> {
        let now = self.now();
        {
            let conn = self.db.lock().expect("poisoned");
            conn.execute(
                "DELETE FROM preview_artifacts WHERE image_hash = ?1",
                params![hash.as_str()],
            )?;
            conn.execute(
                "UPDATE ingest_passes SET state = 'pending', not_before = NULL
                 WHERE image_hash = ?1 AND pass_name = 'preview' AND state IN ('done','error')",
                params![hash.as_str()],
            )?;
            let _ = now;
        }
        for kind in [ArtifactKind::Thumb, ArtifactKind::Display] {
            let p = preview::artifact_path(&self.cache_dir, hash, kind);
            if p.exists() {
                std::fs::remove_file(&p)?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Embedded-native full resolution (the /embedded protocol route —
    // founder backlog, dogfood round 2)
    // -----------------------------------------------------------------------

    /// The RAW's embedded full-resolution JPEG at NATIVE size, display-
    /// oriented per the same §9.3.1 policy the preview pass applied —
    /// strokes are recorded in display-oriented image space (§9.7), so the
    /// native image MUST agree with the cached preview's orientation and
    /// aspect exactly, or every mark rotates/misplaces at deep zoom.
    ///
    /// On-demand extraction, no new cache tier: the file is local (rawler's
    /// metadata-only parse), and the protocol's immutable cache headers let
    /// the webview's HTTP cache hold the encoded result. Every refusal is a
    /// uniform `Ok(None)` (the route answers 404; Look keeps the 2560
    /// preview silently): non-RAW formats (the /original route owns
    /// webview-decodable originals; TIFF/HEIC stay on the preview until the
    /// M1.5 backfill), offline/missing paths, placeholder files (§5.2: a
    /// dataless file is never read — extraction would force hydration), no
    /// usable embedded JPEG, no pixel gain over the cached display artifact
    /// (small-preview RAWs), or geometry disagreement with that artifact.
    pub fn embedded_native(
        &self,
        hash: &ContentHash,
    ) -> Result<Option<EmbeddedNative>, LibraryError> {
        use image::GenericImageView;
        let Some(record) = self.image(hash)? else {
            return Ok(None);
        };
        if record.format != ImageFormat::Raw {
            return Ok(None);
        }
        // The cached display artifact is the stroke substrate the native
        // image must agree with; without one there is nothing on screen to
        // zoom past either.
        let Some(display) = self.preview_artifact(hash, ArtifactKind::Display)? else {
            return Ok(None);
        };
        let Some(best) = self.best_path(hash)? else {
            return Ok(None);
        };
        if !best.online {
            return Ok(None);
        }
        let Some(mount) = best.mount_point.as_deref() else {
            return Ok(None);
        };
        let abs = join_rel(Path::new(mount), &best.row.rel_path);
        let Ok(meta) = std::fs::metadata(&abs) else {
            return Ok(None);
        };
        if self.placeholders.is_placeholder(&abs, &meta) {
            return Ok(None);
        }
        let extracted = match self.extractor.extract(&abs) {
            Ok(Some(x)) => x,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.log(format!("embedded-native extraction failed on {hash}: {e}"));
                return Ok(None);
            }
        };
        let (oriented, _applied, _reason) = preview::orient_embedded_preview(extracted);
        let (w, h) = oriented.dimensions();
        let (dw, dh) = (
            u32::try_from(display.width).unwrap_or(0),
            u32::try_from(display.height).unwrap_or(0),
        );
        if !preview::embedded_native_acceptable(w, h, dw, dh) {
            if w.max(h) > dw.max(dh) {
                // Pixel gain but geometry disagreement — the load-bearing
                // refusal, worth a debug-panel line (silent refusals for
                // no-gain previews are the expected small-preview case).
                self.log(format!(
                    "embedded-native geometry disagreement on {hash}: native {w}x{h} vs \
                     display artifact {dw}x{dh}; refused (stroke-substrate safety)"
                ));
            }
            return Ok(None);
        }
        let jpeg = match preview::encode_jpeg_native(&oriented) {
            Ok(b) => b,
            Err(e) => {
                self.log(format!("embedded-native encode failed on {hash}: {e}"));
                return Ok(None);
            }
        };
        Ok(Some(EmbeddedNative {
            jpeg,
            width: w,
            height: h,
        }))
    }

    // -----------------------------------------------------------------------
    // Batched grid listing (P4.1 A2 — the shell's one folder read)
    // -----------------------------------------------------------------------

    /// Direct-children images of one folder under one root, with badge data
    /// (has-journal dot, folded rating, offline) in ONE query — the grid's
    /// batched read (UI §3.5; per-image reads over a 20k folder are the N+1
    /// this replaces). `folder` is root-relative ("" = the root itself);
    /// order: ascending rel_path. Joins the events-side derived folds
    /// (`image_journal_stats`, `image_ratings` — P5/B4/B34), which live in
    /// the same database.
    pub fn list_folder(
        &self,
        root_id: &str,
        folder: &str,
    ) -> Result<Vec<FolderImage>, LibraryError> {
        let root = self
            .root(root_id)?
            .ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
        // Volume-relative prefix images in the folder must carry.
        let mut prefix = String::new();
        if !root.rel_path.is_empty() {
            prefix.push_str(&root.rel_path);
            prefix.push('/');
        }
        if !folder.is_empty() {
            prefix.push_str(folder);
            prefix.push('/');
        }
        let root_prefix_len = if root.rel_path.is_empty() {
            0
        } else {
            root.rel_path.len() + 1
        };
        let conn = self.db.lock().expect("poisoned");
        // has_journal: the dulled-red dot is evidence of *annotations* —
        // words OR marks (B34: `has_text OR has_strokes`). Rating keys must
        // produce "no visual change to any thumbnail" (UI §3.7), so a
        // rating-only journal does not light the dot.
        let mut stmt = conn.prepare_cached(
            "SELECT p.rel_path, p.image_hash, i.capture_ts, i.first_ingested_at,
                    COALESCE(s.has_text, 0) OR COALESCE(s.has_strokes, 0) AS has_journal,
                    r.rating,
                    NOT EXISTS (
                      SELECT 1 FROM paths p2
                      JOIN volumes v2 ON v2.volume_id = p2.volume_id
                      WHERE p2.image_hash = p.image_hash
                        AND p2.state = 'active' AND v2.state = 'online'
                    ) AS offline,
                    EXISTS (
                      SELECT 1 FROM preview_artifacts pa
                      WHERE pa.image_hash = p.image_hash AND pa.kind = 'thumb'
                    ) AS preview_ready
             FROM paths p
             JOIN images i ON i.image_hash = p.image_hash
             LEFT JOIN image_journal_stats s ON s.image_hash = p.image_hash
             LEFT JOIN image_ratings r ON r.image_hash = p.image_hash
             WHERE p.root_id = ?1 AND p.state = 'active'
               AND substr(p.rel_path, 1, length(?2)) = ?2
               AND instr(substr(p.rel_path, length(?2) + 1), '/') = 0
             ORDER BY p.rel_path",
        )?;
        let rows = stmt.query_map(params![root_id, prefix], |r| {
            let rel: String = r.get(0)?;
            let hash: String = r.get(1)?;
            let root_relative = rel.get(root_prefix_len..).unwrap_or("").to_owned();
            let file_name = root_relative
                .rsplit('/')
                .next()
                .unwrap_or(&root_relative)
                .to_owned();
            Ok(FolderImage {
                hash: ContentHash::from_hex(&hash).map_err(|_| rusqlite::Error::InvalidQuery)?,
                file_name,
                rel_path: root_relative,
                capture_ts: r.get(2)?,
                first_ingested_at: r.get(3)?,
                has_journal: r.get::<_, i64>(4)? != 0,
                rating: r.get::<_, Option<i64>>(5)?.map(|v| v as u8),
                offline: r.get::<_, i64>(6)? != 0,
                preview_ready: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Folder tree of one root (the rail), derived from active path rows.
    /// `rel_path`s are root-relative; children sorted by name.
    pub fn folder_tree(&self, root_id: &str) -> Result<Vec<FolderTreeNode>, LibraryError> {
        let root = self
            .root(root_id)?
            .ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
        let root_prefix_len = if root.rel_path.is_empty() {
            0
        } else {
            root.rel_path.len() + 1
        };
        let rels: Vec<String> = {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare_cached(
                "SELECT DISTINCT rel_path FROM paths WHERE root_id = ?1 AND state = 'active'",
            )?;
            let rows = stmt.query_map(params![root_id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // Every ancestor directory (root-relative), parents before children.
        let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for rel in &rels {
            let Some(rr) = rel.get(root_prefix_len..) else {
                continue;
            };
            let mut acc = String::new();
            let segs: Vec<&str> = rr.split('/').collect();
            for seg in &segs[..segs.len().saturating_sub(1)] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(seg);
                dirs.insert(acc.clone());
            }
        }
        Ok(build_folder_tree(&dirs))
    }

    /// Effective mtime tolerance for a volume (§7.3: 2 s on FAT/exFAT).
    pub(crate) fn mtime_tolerance_ns(&self, fs_type: Option<&str>) -> i64 {
        const COARSE: &[&str] = &["vfat", "msdos", "exfat", "fat", "fat32"];
        match fs_type {
            Some(t) if COARSE.contains(&t.to_ascii_lowercase().as_str()) => 2_000_000_000,
            _ => 0,
        }
    }
}

/// One grid row of [`Library::list_folder`] (UI §3.5 badge data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderImage {
    pub hash: ContentHash,
    pub file_name: String,
    /// Root-relative path of the file.
    pub rel_path: String,
    /// EXIF capture timestamp (RFC 3339) when known.
    pub capture_ts: Option<String>,
    /// First-ingested timestamp (RFC 3339) — the "date added" sort key.
    pub first_ingested_at: String,
    /// Has-journal dot (UI §3.5/§3.7, B34): remark-or-stroke evidence
    /// (`has_text OR has_strokes`); a rating-only journal lights no dot.
    pub has_journal: bool,
    /// Folded current rating (0..=5); `None` = unrated (E4: 0 is explicit).
    pub rating: Option<u8>,
    /// Every active path for this image sits on an offline volume.
    pub offline: bool,
    /// A thumb artifact exists in the cache. While false the grid shows
    /// the placeholder WITHOUT requesting the protocol URL — during a
    /// large (network-volume) scan, thumbs otherwise fire thousands of
    /// doomed 404 round-trips (founder dogfood, SMB, June 2026).
    pub preview_ready: bool,
}

/// One node of [`Library::folder_tree`] (the rail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTreeNode {
    pub name: String,
    /// Root-relative folder path.
    pub rel_path: String,
    pub children: Vec<FolderTreeNode>,
}

fn build_folder_tree(dirs: &std::collections::BTreeSet<String>) -> Vec<FolderTreeNode> {
    let mut index: std::collections::BTreeMap<String, FolderTreeNode> =
        std::collections::BTreeMap::new();
    for d in dirs {
        let name = d.rsplit('/').next().unwrap_or(d).to_owned();
        index.insert(
            d.clone(),
            FolderTreeNode {
                name,
                rel_path: d.clone(),
                children: Vec::new(),
            },
        );
    }
    // Attach children to parents, deepest first so parents collect built
    // subtrees (BTreeMap iterates parents before children, so reverse).
    let mut roots: Vec<FolderTreeNode> = Vec::new();
    let keys: Vec<String> = index.keys().rev().cloned().collect();
    for k in keys {
        let node = index.remove(&k).expect("present");
        match k.rsplit_once('/') {
            Some((parent, _)) if index.contains_key(parent) => {
                index
                    .get_mut(parent)
                    .expect("parent present")
                    .children
                    .insert(0, node);
            }
            _ => roots.insert(0, node),
        }
    }
    roots
}

/// B29's stated home: the library layer implements the sidecar engine's
/// `ImageLocator` seam directly — hash → current adjacent sidecar location,
/// from `best_path` + volume writability.
impl crate::sidecar::ImageLocator for Library {
    fn locate(&self, image: &ContentHash) -> crate::sidecar::AdjacentLocation {
        use crate::sidecar::AdjacentLocation;
        let Ok(Some(best)) = self.best_path(image) else {
            return AdjacentLocation::Offline { volume_id: None };
        };
        if !best.online {
            return AdjacentLocation::Offline {
                volume_id: Some(best.row.volume_id),
            };
        }
        let Some(mount) = best.mount_point.as_deref() else {
            return AdjacentLocation::Offline {
                volume_id: Some(best.row.volume_id),
            };
        };
        let image_path = join_rel(Path::new(mount), &best.row.rel_path);
        let read_only = self
            .volume(&best.row.volume_id)
            .ok()
            .flatten()
            .map(|v| v.read_only)
            .unwrap_or(true);
        if read_only {
            AdjacentLocation::Unwritable {
                image_path: Some(image_path),
                volume_id: Some(best.row.volume_id),
            }
        } else {
            AdjacentLocation::Writable { image_path }
        }
    }
}

/// Outcome of the §7.2 algorithm for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    FastPath,
    Updated,
    Relinked(ContentHash),
    NewImage(ContentHash),
    Superseded { old: ContentHash, new: ContentHash },
}

#[derive(Debug, Default)]
pub struct QueueOptions {
    pub cancel: Option<CancelFlag>,
    pub max_items: Option<usize>,
}

/// What `Library::doctor` found and did (BACKLOG "Library doctor"): the
/// debug panel renders it, the 6-hour tick `info!`s it when nonzero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DoctorReport {
    /// `done` preview passes re-pended because an artifact file vanished.
    pub repended: usize,
    /// Stale path rows whose image has no surviving active path — COUNTED,
    /// never deleted (doctor v1 is conservative by charter).
    pub stale_orphans: usize,
    /// Stranded `.pp-tmp-*` preview temp files removed.
    pub temps_swept: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueueReport {
    pub processed: usize,
    pub done: usize,
    pub errors: usize,
    pub skipped: usize,
    pub transient_retries: usize,
    pub cancelled: bool,
    /// Images whose preview artifacts landed this drain — the
    /// `previews-changed` payload (thumbs that exhausted their 404 retry
    /// budget heal off it; the journal-changed seam, applied to previews).
    pub completed_previews: Vec<ContentHash>,
}

impl QueueReport {
    /// Fold a wave worker's per-item tally into the drain total.
    /// `processed` is counted at claim time and `cancelled` is drain-level
    /// state — neither merges from workers.
    fn absorb(&mut self, other: &QueueReport) {
        self.done += other.done;
        self.errors += other.errors;
        self.skipped += other.skipped;
        self.transient_retries += other.transient_retries;
        self.completed_previews
            .extend(other.completed_previews.iter().cloned());
    }
}

/// The §10 queue worker pool: decode + resize + encode are the CPU cost
/// of ingest, so size like the hashing pool (`min(cores, 8)` — §1.2's
/// reasoning applies unchanged) but keep it SEPARATE: sharing one pool
/// would let a scan's hash burst starve previews and vice versa. The cap
/// also bounds transient decode memory (a wave holds up to 8 full-size
/// decoded frames).
fn worker_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(hashing::hash_pool_size())
            .thread_name(|i| format!("pp-ingest-{i}"))
            .build()
            .expect("ingest worker pool")
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRecord {
    pub image_hash: ContentHash,
    pub byte_size: i64,
    pub format: ImageFormat,
    pub raw_subtype: Option<String>,
    pub pixel_width: Option<i64>,
    pub pixel_height: Option<i64>,
    pub exif_orientation: u16,
    pub capture_ts: Option<String>,
    pub capture_tz_offset: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub iso: Option<i64>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub first_ingested_at: String,
}

fn image_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
    Ok(ImageRecord {
        image_hash: ContentHash::from_hex(&r.get::<_, String>(0)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        byte_size: r.get(1)?,
        format: ImageFormat::parse(&r.get::<_, String>(2)?).ok_or(rusqlite::Error::InvalidQuery)?,
        raw_subtype: r.get(3)?,
        pixel_width: r.get(4)?,
        pixel_height: r.get(5)?,
        exif_orientation: r.get::<_, i64>(6)? as u16,
        capture_ts: r.get(7)?,
        capture_tz_offset: r.get(8)?,
        camera_make: r.get(9)?,
        camera_model: r.get(10)?,
        lens_model: r.get(11)?,
        focal_length_mm: r.get(12)?,
        iso: r.get(13)?,
        f_number: r.get(14)?,
        exposure_time: r.get(15)?,
        gps_lat: r.get(16)?,
        gps_lon: r.get(17)?,
        first_ingested_at: r.get(18)?,
    })
}

/// One native-size, display-oriented JPEG for Look's progressive
/// full-resolution route ([`Library::embedded_native`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedNative {
    pub jpeg: Vec<u8>,
    /// Display-oriented dimensions (agree with the cached preview's aspect).
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub kind: ArtifactKind,
    pub source: PreviewSource,
    pub width: i64,
    pub height: i64,
    pub bytes: i64,
    pub needs_full_decode: bool,
    pub generator_version: i64,
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRecord {
    pub volume_id: VolumeId,
    pub marker_ulid: Option<String>,
    pub platform_id: Option<String>,
    pub platform_kind: Option<String>,
    pub label: Option<String>,
    pub fs_type: Option<String>,
    pub capacity_bytes: Option<i64>,
    pub read_only: bool,
    pub online: bool,
    pub mount_point: Option<String>,
}

fn volume_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<VolumeRecord> {
    Ok(VolumeRecord {
        volume_id: r.get(0)?,
        marker_ulid: r.get(1)?,
        platform_id: r.get(2)?,
        platform_kind: r.get(3)?,
        label: r.get(4)?,
        fs_type: r.get(5)?,
        capacity_bytes: r.get(6)?,
        read_only: r.get::<_, i64>(7)? != 0,
        online: r.get::<_, String>(8)? == "online",
        mount_point: r.get(9)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRecord {
    pub root_id: RootId,
    pub volume_id: VolumeId,
    pub rel_path: String,
    pub display_name: Option<String>,
    pub state: String,
}

fn root_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<RootRecord> {
    Ok(RootRecord {
        root_id: r.get(0)?,
        volume_id: r.get(1)?,
        rel_path: r.get(2)?,
        display_name: r.get(3)?,
        state: r.get(4)?,
    })
}

// ---------------------------------------------------------------------------
// Connection + path helpers
// ---------------------------------------------------------------------------

/// The §5.1 connection pragmas (EVENTS.md; operational values per DECISIONS
/// P18), applied to the library's writer connection. Duplicated from the
/// store-private `schema::apply_pragmas` — flagged in the packet report.
fn open_library_connection(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    for pragma in [
        "PRAGMA journal_mode = WAL",
        "PRAGMA synchronous = NORMAL",
        "PRAGMA secure_delete = ON",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA cache_size = -65536",
        "PRAGMA mmap_size = 268435456",
        "PRAGMA temp_store = MEMORY",
        "PRAGMA busy_timeout = 5000",
    ] {
        let mut stmt = conn.prepare(pragma)?;
        let mut rows = stmt.query([])?;
        let _ = rows.next()?;
    }
    Ok(conn)
}

pub(crate) fn image_exists_on(conn: &Connection, hash: &ContentHash) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM images WHERE image_hash = ?1",
        params![hash.as_str()],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// File mtime as nanoseconds since the epoch, at filesystem precision (§6).
pub(crate) fn mtime_ns_of(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
        },
        Err(_) => 0,
    }
}

/// Join a volume mount point with a volume-relative `/`-separated path.
pub(crate) fn join_rel(mount_point: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        mount_point.to_path_buf()
    } else {
        let mut p = mount_point.to_path_buf();
        for comp in rel.split('/') {
            p.push(comp);
        }
        p
    }
}

/// Volume-relative path string: UTF-8, `/`-separated on all platforms (§3).
/// `None` = non-UTF-8 (skipped and logged by callers — lossy storage would
/// break relinking).
pub(crate) fn rel_path_str(mount_point: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(mount_point).ok()?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(c) => parts.push(c.to_str()?.to_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Component-wise: is `outer` equal to or an ancestor of `inner`?
fn rel_contains(outer: &str, inner: &str) -> bool {
    if outer == inner {
        return true;
    }
    if outer.is_empty() {
        return true; // volume root contains everything
    }
    inner.starts_with(outer) && inner.as_bytes().get(outer.len()) == Some(&b'/')
}

/// §5.2 detection of well-known sync-service paths (advisory only).
fn sync_service_hint(dir: &Path) -> Option<&'static str> {
    let s = dir.to_string_lossy().to_ascii_lowercase();
    if s.contains("dropbox") {
        Some("Dropbox")
    } else if s.contains("onedrive") {
        Some("OneDrive")
    } else if s.contains("google drive") || s.contains("googledrive") {
        Some("Google Drive")
    } else if s.contains("mobile documents") || s.contains("icloud") {
        Some("iCloud Drive")
    } else if dir.join(".dropbox").exists() {
        Some("Dropbox")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_contains_is_component_wise() {
        assert!(rel_contains("a/b", "a/b/c"));
        assert!(rel_contains("a/b", "a/b"));
        assert!(!rel_contains("a/b", "a/bc"));
        assert!(rel_contains("", "anything"));
        assert!(!rel_contains("a/b/c", "a/b"));
    }

    #[test]
    fn rel_path_round_trip() {
        let mp = Path::new("/mnt/vol");
        let abs = Path::new("/mnt/vol/photos/2026/IMG_1.jpg");
        let rel = rel_path_str(mp, abs).unwrap();
        assert_eq!(rel, "photos/2026/IMG_1.jpg");
        assert_eq!(join_rel(mp, &rel), abs);
        assert_eq!(rel_path_str(mp, mp).unwrap(), "");
    }

    #[test]
    fn monotonic_clock_strictly_increases() {
        let c = MonotonicMillis::new();
        let a = c.now();
        let b = c.now();
        let d = c.now();
        assert!(b > a);
        assert!(d > b);
    }
}
