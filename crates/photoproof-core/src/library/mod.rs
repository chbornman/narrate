//! Library: content-addressed identity, volumes, watched roots, watcher +
//! reconciliation, ingest passes, embedded-preview extraction, EXIF subset,
//! thumbnail cache.
//!
//! Contract: spec/LIBRARY.md. Owned by work packet P2.2.
//!
//! Everything here is the **index side** of the system: every table is
//! rebuildable from the filesystem plus the sidecar set. Identity is the
//! BLAKE3-256 of file bytes — images are known by hash, never by path.

mod clip_preprocess;
mod embedding;
mod exclusions;
mod foreign_sidecar;
mod fs_semantics;
mod hashing;
mod ingest;
mod metadata;
mod paths;
mod phash;
mod placeholder;
mod preview;
mod raw_develop;
mod scan;
mod volumes;
mod watcher;

pub use clip_preprocess::{CLIP_IMAGE_EDGE, preprocess_clip_image};
pub use embedding::EmbeddingRig;
pub use exclusions::{
    ImageFormat, MAX_FILE_BYTES, classify_extension, is_excluded_dir_name, is_excluded_file_name,
};
pub use foreign_sidecar::{
    CropRect, ForeignEdit, ForeignSidecarSource, read_foreign_edit, read_foreign_edit_from_str,
};
pub use fs_semantics::{FileSystemSemantics, PlatformFileSystemSemantics};
pub use hashing::{hash_file, hash_invocation_count, hashed_byte_count};
pub use ingest::{
    PASS_VERSION, PRIORITY_BACKFILL, PRIORITY_GPU, PRIORITY_INTERACTIVE, PRIORITY_SCAN,
    PRIORITY_WATCHER, PassCounters, PassName, PassState, placeholder_sentinel,
};
pub use paths::{Availability, BestPath, PathRow, StaleReason};
// Tier-1 near-duplicate detection (DESIGN-DEDUP-AND-SIMILARITY.md §"Tier 1").
// The dHash + Hamming-grouping primitives; the DuplicateGroup wire shape is the
// `find_near_duplicates` command's return element.
pub use phash::{DuplicateGroup, dhash, group_near_duplicates, hamming};
pub use placeholder::{
    PlaceholderDetector, PlatformPlaceholderDetector, SharedSetPlaceholderDetector,
};
// DISPLAY_EDGE / EMBEDDED_ACCEPT_EDGE no longer live here: they moved to the
// centralized tuning config (`crate::tuning`, file-overridable). Their consume
// sites read `tuning().preview.*` directly.
pub use preview::{
    ArtifactKind, ClearKind, EmbeddedOrientationReason, EmbeddedPreviewExtractor, ExtractedPreview,
    FullDecodeFormat, GENERATOR_VERSION, PreviewCacheStats, PreviewError, PreviewSource,
    RawlerExtractor, THUMB_EDGE, apply_exif_orientation, artifact_path, clear_preview_cache,
    embedded_orientation_decision, evict_preview_cache, existing_full_artifact, full_artifact_path,
    oriented_dims, preview_cache_stats, touch_full_artifact,
};
pub use raw_develop::DevelopError;
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use photoproof_connectors::vector_store::{VecKind, VecSpace, VectorStore};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use thiserror::Error;

#[derive(Debug)]
pub(crate) enum ActivePathMatch {
    Exact(PathRow),
    /// The DB spelling and observed spelling were independently resolved by
    /// the filesystem to the same canonical path.
    CaseAlias(PathRow),
}

use crate::id::{ContentHash, UtcMillis};
use crate::metrics::{CatalogMetrics, PipelineMetrics, StageSnapshot};
use crate::store::StoreError;

pub type VolumeId = String;
pub type RootId = String;

/// Derived preview/vector data is retained for this long after the LAST path
/// for an image became stale. This is deliberately far beyond the watcher's
/// seconds-long move-correlation window: a delayed rename, sleeping removable
/// volume, or folder re-registration gets a full month to relink without
/// rebuilding anything.
pub const DEFAULT_ORPHAN_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// One active root's outcome from a multi-root reconciliation.
///
/// A bad root is data, not control flow for the whole library: callers need
/// the successful reports and the identity of every degraded/offline root so
/// they can surface precise recovery actions without starving later roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootReconcileResult {
    pub root_id: RootId,
    pub outcome: RootReconcileOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootReconcileOutcome {
    Scanned(ScanReport),
    Offline { volume_id: VolumeId },
    Failed { error: String },
}

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
    /// A folder add was refused because it sits inside (or is a parent of) an
    /// existing active root — the founder's "refuse + alias" rule: do not
    /// double-ingest, point the user at the root they already have. Carries
    /// that root's id so the UI can offer "go to the existing folder" instead
    /// of a dead-end error (folder-tree improvements; replaced the older bare
    /// `NestedRoot` message, which carried no id for the rail to navigate to).
    #[error("overlaps existing root {existing_root_id}: {detail}")]
    OverlappingRoot {
        existing_root_id: RootId,
        detail: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("volume offline: {0}")]
    VolumeOffline(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// PPVEC flat-file vector storage failure (P7.1 embedding passes).
    #[error("vector store error: {0}")]
    Vectors(#[from] photoproof_connectors::vector_store::VectorStoreError),
}

/// Cancellation for long operations (interrupt-safety tests drive this; a
/// real `kill -9` is strictly weaker than cancelling between work units plus
/// SQLite transactionality, since transactions are atomic either way).
pub type CancelFlag = Arc<AtomicBool>;

/// A clonable, waitable manual-pause signal shared by shell-owned long
/// operations. Unlike a cancellation flag, pausing never abandons the current
/// idempotent operation: workers sleep at cooperative boundaries and resume
/// the same scan/download when the signal clears.
#[derive(Debug, Clone)]
pub struct PauseToken {
    inner: Arc<PauseState>,
}

#[derive(Debug)]
struct PauseState {
    paused: Mutex<bool>,
    changed: std::sync::Condvar,
}

impl PauseToken {
    pub fn new(paused: bool) -> Self {
        Self {
            inner: Arc::new(PauseState {
                paused: Mutex::new(paused),
                changed: std::sync::Condvar::new(),
            }),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        *self.inner.paused.lock().expect("pause token mutex") = paused;
        self.inner.changed.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        *self.inner.paused.lock().expect("pause token mutex")
    }

    /// Wait until resumed. Returns false when cancellation wins while paused.
    pub fn wait_until_resumed(&self, cancel: Option<&AtomicBool>) -> bool {
        const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);
        let mut paused = self.inner.paused.lock().expect("pause token mutex");
        while *paused {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return false;
            }
            let (next, _) = self
                .inner
                .changed
                .wait_timeout(paused, CANCEL_POLL)
                .expect("pause token condvar");
            paused = next;
        }
        !cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}

/// Injectable collaborators (defaults are the platform implementations).
pub struct LibraryOptions {
    pub probe: Arc<dyn VolumeProbe>,
    pub placeholders: Arc<dyn PlaceholderDetector>,
    pub extractor: Arc<dyn EmbeddedPreviewExtractor>,
    pub fs_semantics: Arc<dyn FileSystemSemantics>,
}

impl Default for LibraryOptions {
    fn default() -> Self {
        Self {
            probe: Arc::new(PlatformVolumeProbe),
            placeholders: Arc::new(PlatformPlaceholderDetector),
            extractor: Arc::new(RawlerExtractor),
            fs_semantics: Arc::new(PlatformFileSystemSemantics),
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
    db_path: PathBuf,
    cache_dir: PathBuf,
    probe: Arc<dyn VolumeProbe>,
    placeholders: Arc<dyn PlaceholderDetector>,
    extractor: Arc<dyn EmbeddedPreviewExtractor>,
    fs_semantics: Arc<dyn FileSystemSemantics>,
    clock: MonotonicMillis,
    debug_log: Mutex<Vec<String>>,
    /// Ingest-stage timings (BACKLOG "measured, not vibes" — first slice).
    metrics: PipelineMetrics,
    /// Shared SQLite catalog-lane wait/operation timings for fixed hot paths.
    catalog_metrics: CatalogMetrics,
    /// Monotonic, in-memory image-set version (Seam 1, sibling of
    /// `PpvecStore::vectors_version`). Bumped on every committed change to
    /// the ACTIVE image↔path set — new image, supersede, relink/reactivate,
    /// and live remove (AUDIT-2026-07-07 S3/S4: bumping only on new images
    /// left the grid blind to in-place edits, moves, and deletions) — so the
    /// library->view data-change contract lets the grid re-list when its
    /// slice advances instead of polling on a wall-clock throttle. In-memory
    /// only: no schema column, no migration — views re-list on mount, so a
    /// counter that resets to 0 at process start is correct. Root-removal
    /// still rides its own `roots-changed` event, so it deliberately does
    /// not bump here (see ARCHITECTURE-CONTRACTS.md Seam 1).
    images_version: AtomicU64,
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
        // §9.8 regeneration: a `generator_version` change (compile-time constant
        // covering encoder, sizes, color pipeline) re-enqueues the preview pass
        // at backfill priority for every artifact NOT at the current version.
        // The `<>` (not `<`) also catches a DOWNGRADE: an older binary opening a
        // DB whose artifacts a newer binary wrote at a higher version would
        // otherwise serve those future-format artifacts undetected
        // (STATE-INTEGRITY-AUDIT.md). Regenerating them at the running version
        // keeps disk and code in agreement in both directions.
        let regen = conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', not_before = NULL, priority = ?2
             WHERE pass_name = 'preview' AND state = 'done'
               AND image_hash IN (SELECT image_hash FROM preview_artifacts
                                  WHERE generator_version <> ?1)",
            params![preview::GENERATOR_VERSION, ingest::PRIORITY_BACKFILL],
        )?;
        // Distinguish a downgrade for the operator: the highest version ever
        // written exceeding what this build produces means a newer app touched
        // this library. Recovery is automatic (the re-pend above regenerates),
        // but it is worth a log line rather than a silent reshape.
        let max_gen: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(generator_version), 0) FROM preview_artifacts",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let cache_dir = cache_dir.into();
        // §9.8 crash hygiene: stranded temp files from a mid-write crash.
        let swept = preview::sweep_temp_files(&cache_dir)?;
        let lib = Self {
            db: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
            cache_dir,
            probe: options.probe,
            placeholders: options.placeholders,
            extractor: options.extractor,
            fs_semantics: options.fs_semantics,
            clock: MonotonicMillis::new(),
            debug_log: Mutex::new(Vec::new()),
            metrics: PipelineMetrics::default(),
            catalog_metrics: CatalogMetrics::default(),
            images_version: AtomicU64::new(0),
        };
        if recovered > 0 {
            lib.log(format!(
                "crash recovery: {recovered} running passes re-pended"
            ));
        }
        if regen > 0 {
            let direction = if max_gen > preview::GENERATOR_VERSION {
                "DOWNGRADE (newer app wrote these)"
            } else {
                "bump"
            };
            lib.log(format!(
                "generator_version {direction}: running v{}, cache had up to v{max_gen}; \
                 {regen} preview passes re-enqueued at backfill priority",
                preview::GENERATOR_VERSION
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

    /// Settings → Previews: on-disk size + count of the preview cache, split
    /// into the full-res 1:1 tier (the budgeted one) and the total footprint.
    /// Cheap (one stat pass, no byte reads) so the Settings readout can call it
    /// on every open. Distinct from `preview_cache_stats` above, which counts
    /// the `preview_artifacts` DB rows (thumb/display): the 1:1 tier is
    /// disk-only and has no rows, so its size is measured on the filesystem.
    pub fn full_cache_stats(&self) -> preview::PreviewCacheStats {
        preview::preview_cache_stats(&self.cache_dir)
    }

    /// DESIGN-PREVIEW-POLICY.md: trim the full-res 1:1 cache to `budget_bytes`,
    /// evicting LEAST-RECENTLY-VIEWED first. No-op under budget. Returns the
    /// number of files evicted. Runs after each on-demand develop and is SAFE
    /// (every evicted 1:1 re-derives on next view; strokes live in vector
    /// coords, never in the artifact).
    pub fn evict_preview_cache(&self, budget_bytes: u64) -> u64 {
        preview::evict_preview_cache(&self.cache_dir, budget_bytes)
    }

    /// Settings → Previews "Clear 1:1 cache" / "Rebuild all previews": remove the
    /// full-res 1:1 tier ([`preview::ClearKind::Full`]) or every preview
    /// artifact ([`preview::ClearKind::All`]). Returns the number of files
    /// removed. SAFE — every removed artifact re-derives on next view.
    ///
    /// WHY the `All` re-pend: the grid's `-thumb`/`-disp` artifacts are only
    /// ever (re)produced by the preview pass; deleting them with no DB touch
    /// would strand the grid on permanent "?" placeholders with nothing to heal
    /// them (founder dogfood, June 2026). So after an `All` sweep we re-pend the
    /// preview pass for every active root — the ingest pump drains them and each
    /// landed artifact heals its thumb off `previews-changed`. The `Full` sweep
    /// does NOT re-pend: the 1:1 full-res tier is on-demand by design (it
    /// redevelops on next view/zoom), and re-pending it would defeat the
    /// disk-reclaim purpose of the button.
    pub fn clear_preview_cache_kind(&self, kind: preview::ClearKind) -> std::io::Result<u64> {
        let removed = preview::clear_preview_cache(&self.cache_dir, kind)?;
        if kind == preview::ClearKind::All {
            // The disk sweep just removed every `-thumb`/`-disp`; without a
            // re-pend the grid is stranded. Map a DB error onto io::Error so the
            // single-Result signature holds (the removal already happened).
            self.repend_all_previews()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(removed)
    }

    /// Re-pend the preview pass for EVERY active root — the all-roots analog of
    /// [`Library::rebuild_previews`] (which scopes to one root). Same recovery
    /// shape (pending, fresh attempt budget, no backoff, no stale error,
    /// backfill priority; `running` rows left in flight so a second worker
    /// cannot claim an image mid-regenerate). A SINGLE UPDATE: it drops the
    /// per-root filter and matches any image with at least one active path.
    /// Returns the number of rows re-pended. Used by the "Rebuild all previews"
    /// clear so the grid heals after a full sweep.
    pub fn repend_all_previews(&self) -> Result<usize, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', attempts = 0, not_before = NULL,
                 error = NULL,
                 priority = CASE WHEN state = 'pending'
                                 THEN MIN(priority, ?1) ELSE ?1 END
             WHERE pass_name = 'preview' AND state != 'running'
               AND image_hash IN (SELECT image_hash FROM paths
                                  WHERE state = 'active')",
            params![ingest::PRIORITY_BACKFILL],
        )?;
        if n > 0 {
            self.log(format!("rebuild all previews: {n} passes re-pended"));
        }
        Ok(n)
    }

    /// Re-pend every `done` pass of one stage so the next drain re-runs it
    /// (fresh attempt budget, no backoff, backfill priority; `running` rows left
    /// in flight). Unlike [`ingest::repend_passes_for_model`] this is
    /// UNCONDITIONAL on the model id: the startup doctor uses it after deleting a
    /// vector space whose file vanished, where the pass row still records the
    /// CURRENT model (so the model-aware re-pend would no-op) yet the vectors
    /// must be rebuilt from scratch. Returns rows re-pended.
    pub fn repend_pass(&self, pass: ingest::PassName) -> Result<usize, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', started_at = NULL, completed_at = NULL,
                 attempts = 0, not_before = NULL, error = NULL,
                 priority = CASE WHEN state = 'pending'
                                 THEN MIN(priority, ?2) ELSE ?2 END
             WHERE pass_name = ?1 AND state = 'done'",
            params![pass.as_str(), ingest::PRIORITY_BACKFILL],
        )?;
        if n > 0 {
            self.log(format!("doctor: re-pended {n} {} passes", pass.as_str()));
        }
        Ok(n)
    }

    /// FORCE a full re-embed of an embed pass into the active space — the Seam 2
    /// tail (`docs/ARCHITECTURE-CONTRACTS.md` rollout step 4). Thin wrapper over
    /// [`ingest::force_repend_passes`]: re-pends EVERY embeddable row (`done` +
    /// transient-skip + attempt-capped `error`) UNCONDITIONALLY, for the
    /// "I swapped weights under the same `model_id`" case the model-aware re-pend
    /// cannot see. Unlike [`Self::repend_pass`] (doctor: `done` only) this covers
    /// the same broader cohorts as [`ingest::repend_passes_for_model`], and
    /// leaves `priority` untouched (a watcher P0 keeps its lane). Returns rows
    /// re-pended. The explicit `force_reembed` command is the only caller — it is
    /// kept off the automatic staleness path so it can never fire by surprise.
    pub fn force_repend_pass(&self, pass: ingest::PassName) -> Result<usize, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let n = ingest::force_repend_passes(&conn, pass)?;
        if n > 0 {
            self.log(format!(
                "force re-embed: re-pended {n} {} passes",
                pass.as_str()
            ));
        }
        Ok(n)
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

    /// Cumulative fixed-label catalog-lane wait and operation timings.
    pub fn catalog_metrics_snapshot(&self) -> Vec<StageSnapshot> {
        self.catalog_metrics.snapshot()
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

    /// `(display name, unavailable image count)` for each OFFLINE volume that
    /// still holds at least one path under an ACTIVE root. Drives the "drive
    /// disconnected" warning (founder: warn + pause): the app should TELL the
    /// user a live source is gone and how many photos that hides, not churn
    /// silently. Archived roots retain their paths and searchable authored
    /// truth, but are resting lifecycle state and therefore create no offline
    /// burden.
    pub fn offline_volume_burden(&self) -> Result<Vec<(String, u64)>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT COALESCE(NULLIF(v.label, ''), v.volume_id) AS name,
                    COUNT(DISTINCT p.image_hash) AS images
             FROM volumes v
             JOIN paths p ON p.volume_id = v.volume_id AND p.state = 'active'
             LEFT JOIN roots r ON r.root_id = p.root_id
             WHERE v.state = 'offline'
               AND (p.root_id IS NULL OR r.state = 'active')
             GROUP BY v.volume_id
             HAVING images > 0
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
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
        // active root, on the same volume. The refusal now carries the
        // offending root's id (folder-tree improvements: refuse + alias) so
        // the caller can navigate to the root the user already has instead of
        // showing a dead-end error.
        {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare(
                "SELECT root_id, rel_path FROM roots WHERE volume_id = ?1 AND state = 'active'",
            )?;
            let existing: Vec<(String, String)> = stmt
                .query_map(params![volume_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (other_id, other) in existing {
                if rel_contains(&other, &rel) || rel_contains(&rel, &other) {
                    return Err(LibraryError::OverlappingRoot {
                        existing_root_id: other_id,
                        detail: format!("'{rel}' overlaps existing active root '{other}'"),
                    });
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
        // Skip the still-pending/error ingest passes of images this root just
        // orphaned. WHY: a pending pass with no live file on disk can never
        // complete — it would defer forever, churning the drain. We use the
        // SAFE filter: an image may be linked from several roots, so only skip
        // when NO active path remains for it after the stale-marking above (an
        // image still reachable from another root keeps its passes). Mirrors
        // `ingest::mark_skipped`: state→'skipped', error code recorded, and
        // not_before/attempts cleared so it never re-enters the queue.
        conn.execute(
            "UPDATE ingest_passes
             SET state = 'skipped', error = 'root-removed', completed_at = ?2,
                 not_before = NULL, attempts = 0
             WHERE image_hash IN (
                   SELECT DISTINCT p.image_hash FROM paths p
                   WHERE p.root_id = ?1
                     AND NOT EXISTS (
                       SELECT 1 FROM paths a
                       WHERE a.image_hash = p.image_hash AND a.state = 'active'))
               AND state IN ('pending', 'error')",
            params![root_id, now],
        )?;
        Ok(())
    }

    /// Startup-doctor heal: skip every pending/error ingest pass whose image has
    /// NO remaining active path. WHY: images orphaned BEFORE the `remove_root`
    /// skip-on-removal fix (or by a file-deletion path that never went through
    /// `remove_root`) still carry live passes that can never complete — they
    /// defer forever, churning the drain. This catch-all sweep, run once at
    /// startup, retires them. Same shape as `ingest::mark_skipped`
    /// (state→'skipped', error code, not_before/attempts cleared). Idempotent:
    /// a second run finds nothing pending for an orphan and returns 0. Returns
    /// the number of passes skipped.
    pub fn heal_orphaned_passes(&self) -> Result<usize, LibraryError> {
        let now = self.now().to_rfc3339();
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE ingest_passes
             SET state = 'skipped', error = 'root-removed', completed_at = ?1,
                 not_before = NULL, attempts = 0
             WHERE state IN ('pending', 'error')
               AND image_hash NOT IN (
                   SELECT DISTINCT image_hash FROM paths WHERE state = 'active')",
            params![now],
        )?;
        Ok(n)
    }

    /// Archive a root (folder-tree improvements): a NON-DESTRUCTIVE lifecycle
    /// resting state. The row flips `active` → `archived` and NOTHING else —
    /// unlike `remove_root`, `paths` stay `active`, so every image journal and
    /// collection membership (all keyed by image hash, never by root) is
    /// preserved exactly. An archived root drops out of the active rail and out
    /// of `active_roots`/`reconcile_all` (those filter on `state = 'active'`),
    /// so it no longer scans or watches; `unarchive_root` brings it whole back.
    pub fn archive_root(&self, root_id: &str) -> Result<(), LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE roots SET state = 'archived'
             WHERE root_id = ?1 AND state = 'active'",
            params![root_id],
        )?;
        if n == 0 {
            return Err(LibraryError::NotFound(format!("active root {root_id}")));
        }
        Ok(())
    }

    /// Restore an archived root to active (folder-tree improvements). The
    /// reverse of [`Self::archive_root`]; the caller rescans afterwards so the
    /// index reconciles any on-disk drift that happened while it rested.
    pub fn unarchive_root(&self, root_id: &str) -> Result<(), LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let n = conn.execute(
            "UPDATE roots SET state = 'active'
             WHERE root_id = ?1 AND state = 'archived'",
            params![root_id],
        )?;
        if n == 0 {
            return Err(LibraryError::NotFound(format!("archived root {root_id}")));
        }
        Ok(())
    }

    /// Archived roots, for the rail's collapsed "Archived" affordance.
    pub fn archived_roots(&self) -> Result<Vec<RootRecord>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT root_id, volume_id, rel_path, display_name, state
             FROM roots WHERE state = 'archived' ORDER BY root_id",
        )?;
        let rows = stmt.query_map([], root_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
        if root.state != "active" {
            return Err(LibraryError::NotFound(format!("active root {root_id}")));
        }
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
    ) -> Result<Vec<RootReconcileResult>, LibraryError> {
        let roots = self.roots()?;
        Ok(reconcile_active_roots(roots, |root_id| {
            self.scan_root(root_id, opts)
        }))
    }

    /// Wake trigger (§7.3): watcher gaps across sleep are a canonical
    /// missed-event source. Called by the shell's pump when its wall-clock
    /// gap detector fires (AUDIT-2026-07-07 S2); takes `opts` so the caller
    /// can wire the live discovered-counter/cancel seams the same way every
    /// other scan trigger does.
    pub fn on_system_resume(
        &self,
        opts: &ScanOptions,
    ) -> Result<Vec<RootReconcileResult>, LibraryError> {
        self.probe_volumes()?;
        self.reconcile_all(opts)
    }

    /// The 6-hour tick (§7.3, §10.5): error-row retry + the doctor's
    /// validate-and-heal sweep + full reconciliation. The doctor joined the
    /// tick per BACKLOG "Library doctor / self-check pass" (founder, dogfood
    /// round 3): mangled states keep happening; the library should HEAL on
    /// its own schedule, not just avoid poisoning.
    pub fn maintenance_tick(&self) -> Result<Vec<RootReconcileResult>, LibraryError> {
        self.maintenance_tick_inner(true, &ScanOptions::default())
    }

    /// The same six-hour maintenance pass when the shell has an independent,
    /// recent volume-probe cadence. Keeping that lightweight 30-second probe
    /// out of the heavy reconciliation avoids performing the same mount walk
    /// twice whenever the six-hour timer lands.
    pub fn maintenance_tick_without_volume_probe(
        &self,
        opts: &ScanOptions,
    ) -> Result<Vec<RootReconcileResult>, LibraryError> {
        self.maintenance_tick_inner(false, opts)
    }

    fn maintenance_tick_inner(
        &self,
        probe_volumes: bool,
        opts: &ScanOptions,
    ) -> Result<Vec<RootReconcileResult>, LibraryError> {
        {
            let conn = self.db.lock().expect("poisoned");
            ingest::retry_errors(&conn)?;
        }
        self.doctor_with_retention_and_cancel(
            UtcMillis::now(),
            OrphanRetentionMode::Reclaim,
            opts.cancel.as_ref(),
        )?;
        if probe_volumes {
            self.probe_volumes()?;
        }
        self.reconcile_all(opts)
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

    /// Library doctor (BACKLOG "Library doctor / self-check pass"): validate
    /// the index against reality and repair what it can conservatively.
    ///
    /// Before checking preview integrity, the doctor reclaims derived data for
    /// images whose EVERY path has been stale for at least
    /// [`DEFAULT_ORPHAN_RETENTION`]. A missing or malformed `stale_since`
    /// timestamp is never guessed and therefore never eligible. Reclamation is
    /// ordered so every interruption is retryable: final preview files are
    /// removed before their rebuildable metadata; vector rows are first marked
    /// dead transactionally, then PPVEC's existing zero/drop + crash-atomic
    /// compaction machinery reclaims their bytes. `image_clip` is always
    /// image-local. `annotation_chunk` rows are retired only when every image
    /// targeted by their source event is in the authoritative eligible cohort;
    /// any active/recent/unknown/busy sibling target protects the shared row.
    /// A relink re-pends `text-embedding`, rebuilding chunks from authored
    /// journal/FTS truth and rebuilding `image_summary` vectors from retained
    /// `derived_summaries(scope='image')` text. Summary text is never reclaimed:
    /// no LLM regeneration is required to restore its disposable vector. The
    /// transaction re-checks active paths and running passes immediately before
    /// acting.
    ///
    /// - `done` preview passes whose thumb OR display artifact file is
    ///   missing on disk (a cache directory mangled outside the app) →
    ///   re-pend at backfill priority with a fresh budget; the next drain
    ///   regenerates (idempotent, §9.8).
    /// - stale path rows whose image has NO surviving active path are grouped
    ///   into recent, timestamp-unknown, busy, and retention-eligible cohorts.
    ///   Stale path tombstones and image/user-truth rows remain intact.
    /// - stranded preview temp files → swept (`sweep_temp_files`, §9.8
    ///   crash hygiene — the same sweep `open_with` runs at startup). A
    ///   sweep racing a mid-write pass at worst fails that one rename;
    ///   the pass retries as transient and finals are never torn.
    pub fn doctor(&self) -> Result<DoctorReport, LibraryError> {
        self.doctor_with_retention_and_cancel(UtcMillis::now(), OrphanRetentionMode::Reclaim, None)
    }

    /// Startup/shutdown-owned doctor variant. Cancellation is observed between
    /// durable repair units; a SQLite transaction or atomic file replacement is
    /// always allowed to finish, so cancellation never creates a torn repair.
    pub fn doctor_with_cancel(&self, cancel: &CancelFlag) -> Result<DoctorReport, LibraryError> {
        self.doctor_with_retention_and_cancel(
            UtcMillis::now(),
            OrphanRetentionMode::Reclaim,
            Some(cancel),
        )
    }

    /// Run the same doctor against an explicit wall clock and retention mode.
    /// The explicit clock makes retention boundaries testable; `ReportOnly`
    /// powers a settings/debug-panel dry run without mutating derived data.
    pub fn doctor_with_retention(
        &self,
        now: UtcMillis,
        retention_mode: OrphanRetentionMode,
    ) -> Result<DoctorReport, LibraryError> {
        self.doctor_with_retention_and_cancel(now, retention_mode, None)
    }

    fn doctor_with_retention_and_cancel(
        &self,
        now: UtcMillis,
        retention_mode: OrphanRetentionMode,
        cancel: Option<&CancelFlag>,
    ) -> Result<DoctorReport, LibraryError> {
        let is_cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        if is_cancelled() {
            return Ok(DoctorReport {
                cancelled: true,
                ..DoctorReport::default()
            });
        }
        let cutoff = UtcMillis::from_epoch_ms(
            now.epoch_ms()
                .saturating_sub(DEFAULT_ORPHAN_RETENTION.as_millis() as i64),
        );
        let mut cohorts = {
            let conn = self.db.lock().expect("poisoned");
            classify_orphans(&conn, cutoff)?
        };
        let mut reclaimed = ReclaimedOrphans::default();
        if retention_mode == OrphanRetentionMode::Reclaim && !cohorts.eligible.is_empty() {
            let mut conn = self.db.lock().expect("poisoned");
            // Take the writer reservation before the authoritative re-check.
            // A deferred read transaction can deadlock upgrading while a
            // second connection waits to commit; IMMEDIATE serializes relinks,
            // pass claims, and vector writes before any file is removed.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Do not act on the earlier snapshot: a watcher may have relinked
            // an image or a worker may have claimed it while filesystem stats
            // were in flight.
            cohorts = classify_orphans(&tx, cutoff)?;
            if !cohorts.eligible.is_empty() {
                let eligible: std::collections::HashSet<String> =
                    cohorts.eligible.iter().cloned().collect();
                let mut candidate_annotation_events = std::collections::BTreeSet::new();
                for image_hash in &cohorts.eligible {
                    let Ok(hash) = ContentHash::from_hex(image_hash) else {
                        // A malformed hash cannot map to a cache path safely.
                        // It remains visible in the eligible count for repair.
                        continue;
                    };
                    let (artifact_rows, artifact_bytes): (i64, i64) = tx.query_row(
                        "SELECT COUNT(*), COALESCE(SUM(bytes), 0)
                         FROM preview_artifacts WHERE image_hash = ?1",
                        [image_hash],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    let preview_files_before = reclaimed.preview_files;
                    let vector_rows_before = reclaimed.vector_rows;
                    reclaimed.preview_files +=
                        preview::remove_cached_artifacts(&self.cache_dir, &hash)?;
                    reclaimed.preview_rows += artifact_rows as usize;
                    reclaimed.preview_bytes += artifact_bytes as u64;

                    let spaces: Vec<(String, String)> = {
                        let mut stmt = tx.prepare(
                            "SELECT DISTINCT vec_kind, model_id FROM vectors
                             WHERE image_hash = ?1
                               AND vec_kind IN ('image_clip', 'image_summary')",
                        )?;
                        let rows = stmt.query_map([image_hash], |r| Ok((r.get(0)?, r.get(1)?)))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    for (kind, model_id) in spaces {
                        if let Some(vec_kind) = retention_vec_kind(&kind) {
                            reclaimed.spaces.insert(VecSpace { vec_kind, model_id });
                        }
                    }
                    {
                        let mut stmt = tx.prepare(
                            "SELECT DISTINCT v.event_id
                             FROM vectors v
                             JOIN event_targets t ON t.event_id = v.event_id
                             WHERE v.vec_kind = 'annotation_chunk'
                               AND t.image_hash = ?1
                               AND v.event_id IS NOT NULL",
                        )?;
                        let rows = stmt.query_map([image_hash], |r| r.get::<_, String>(0))?;
                        candidate_annotation_events
                            .extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
                    }
                    reclaimed.vector_rows += tx.execute(
                        "UPDATE vectors SET deleted = 1
                         WHERE image_hash = ?1
                           AND vec_kind IN ('image_clip', 'image_summary')
                           AND deleted = 0",
                        [image_hash],
                    )?;
                    tx.execute(
                        "DELETE FROM preview_artifacts WHERE image_hash = ?1",
                        [image_hash],
                    )?;
                    // A future relink revives exactly these retention-cleaned
                    // passes. Keeping a terminal state while the image has no
                    // path prevents the background queues from churning.
                    let passes_retired = tx.execute(
                        "UPDATE ingest_passes
                         SET state = 'skipped', error = 'orphan-retention',
                             not_before = NULL, attempts = 0, completed_at = ?2
                         WHERE image_hash = ?1
                           AND pass_name IN
                               ('preview', 'image-embedding', 'text-embedding')
                           AND state != 'running'
                           AND NOT (state = 'skipped' AND error = 'orphan-retention')",
                        params![image_hash, now.to_rfc3339()],
                    )?;
                    if artifact_rows > 0
                        || reclaimed.preview_files > preview_files_before
                        || reclaimed.vector_rows > vector_rows_before
                        || passes_retired > 0
                    {
                        reclaimed.images += 1;
                    }
                }
                for event_id in candidate_annotation_events {
                    let targets: Vec<String> = {
                        let mut stmt = tx.prepare(
                            "SELECT image_hash FROM event_targets
                             WHERE event_id = ?1 ORDER BY image_hash",
                        )?;
                        let rows = stmt.query_map([&event_id], |r| r.get(0))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    // Empty means session-level text and is never an orphan
                    // image's property. A target outside the eligible set may
                    // be active, recent, timestamp-unknown, or busy; all four
                    // conservatively protect this shared authored-text index.
                    if targets.is_empty() || !targets.iter().all(|hash| eligible.contains(hash)) {
                        continue;
                    }
                    let spaces: Vec<(String, String)> = {
                        let mut stmt = tx.prepare(
                            "SELECT DISTINCT vec_kind, model_id FROM vectors
                             WHERE event_id = ?1
                               AND vec_kind = 'annotation_chunk'",
                        )?;
                        let rows = stmt.query_map([&event_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    for (kind, model_id) in spaces {
                        if let Some(vec_kind) = retention_vec_kind(&kind) {
                            reclaimed.spaces.insert(VecSpace { vec_kind, model_id });
                        }
                    }
                    reclaimed.vector_rows += tx.execute(
                        "UPDATE vectors SET deleted = 1
                         WHERE event_id = ?1 AND vec_kind = 'annotation_chunk'
                           AND deleted = 0",
                        [&event_id],
                    )?;
                }
            }
            tx.commit()?;
        }

        // The metadata commit above makes vector rows invisible first. PPVEC's
        // sweep then zeroes bytes before deleting each row, and its two-phase
        // compaction journal makes file shrinkage crash-recoverable.
        if retention_mode == OrphanRetentionMode::Reclaim
            && (!cohorts.eligible.is_empty() || !reclaimed.spaces.is_empty())
            && !is_cancelled()
        {
            let vectors_dir = crate::retrieval::default_vectors_dir(&self.db_path)
                .expect("a database path always has a parent");
            let vectors = crate::retrieval::PpvecStore::open(&self.db_path, vectors_dir)?;
            vectors.sweep_dead()?;
            for space in &reclaimed.spaces {
                if is_cancelled() {
                    break;
                }
                vectors.compact(space.clone())?;
                reclaimed.vector_spaces_compacted += 1;
            }
            // Compaction of a now-empty space leaves a header-only file. Run
            // the orphan-space pass even when this invocation found only an
            // already-compacted interrupted state: a crash after compaction
            // but before file removal otherwise leaves bytes forever.
            vectors.reconcile_spaces(&std::collections::HashMap::new())?;
        }
        let journal_vector_rows_retained = {
            let conn = self.db.lock().expect("poisoned");
            let eligible: std::collections::HashSet<&str> =
                cohorts.eligible.iter().map(String::as_str).collect();
            let mut stmt = conn.prepare(
                "SELECT image_hash FROM vectors
                 WHERE vec_kind = 'image_summary' AND deleted = 0",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|hash| eligible.contains(hash.as_str()))
                .count()
        };

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
            if is_cancelled() {
                break;
            }
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
                if is_cancelled() {
                    break;
                }
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
        let temps_swept = if is_cancelled() {
            0
        } else {
            preview::sweep_temp_files(&self.cache_dir)?
        };
        let report = DoctorReport {
            cancelled: is_cancelled(),
            repended,
            stale_orphans: cohorts.stale_path_rows,
            orphan_images: cohorts.images,
            retention_eligible: cohorts.eligible.len(),
            retention_deferred_recent: cohorts.recent,
            retention_deferred_unknown_timestamp: cohorts.unknown_timestamp,
            retention_deferred_busy: cohorts.busy,
            retention_dry_run: retention_mode == OrphanRetentionMode::ReportOnly,
            reclaimed_images: reclaimed.images,
            preview_rows_reclaimed: reclaimed.preview_rows,
            preview_files_reclaimed: reclaimed.preview_files,
            preview_bytes_reclaimed: reclaimed.preview_bytes,
            vector_rows_reclaimed: reclaimed.vector_rows,
            vector_spaces_compacted: reclaimed.vector_spaces_compacted,
            journal_vector_rows_retained,
            temps_swept,
        };
        if report.repended > 0
            || report.stale_orphans > 0
            || report.temps_swept > 0
            || report.reclaimed_images > 0
        {
            tracing::info!(
                repended = report.repended,
                stale_orphans = report.stale_orphans,
                retention_eligible = report.retention_eligible,
                reclaimed_images = report.reclaimed_images,
                preview_rows_reclaimed = report.preview_rows_reclaimed,
                vector_rows_reclaimed = report.vector_rows_reclaimed,
                journal_vector_rows_retained = report.journal_vector_rows_retained,
                temps_swept = report.temps_swept,
                "library doctor"
            );
            self.log(format!(
                "doctor: {} previews re-pended, {} orphaned images ({} stale paths; {} eligible), \
                 {} images / {} preview rows / {} vector rows reclaimed, {} temp files swept",
                report.repended,
                report.orphan_images,
                report.stale_orphans,
                report.retention_eligible,
                report.reclaimed_images,
                report.preview_rows_reclaimed,
                report.vector_rows_reclaimed,
                report.temps_swept
            ));
        }
        Ok(report)
    }

    /// §7.2 for one observed (stable) file. `move_window_start` bounds the
    /// remove→create move-correlation flip. Watcher hashing observes the
    /// shell's cooperative pause/cancel controls.
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
        cancel: Option<&AtomicBool>,
        pause: Option<&PauseToken>,
    ) -> Result<Option<Observed>, LibraryError> {
        let existing = self.active_path_match(volume_id, rel_path, abs_path)?;
        match existing {
            Some(ActivePathMatch::Exact(row)) => {
                if row.size == size && (row.mtime_ns - mtime_ns).abs() <= tolerance_ns {
                    let conn = self.db.lock().expect("poisoned");
                    paths::touch_verified(&conn, &row.path_id, self.now())?;
                    return Ok(Some(Observed::FastPath));
                }
                let Some((hash, hashed_size)) =
                    hashing::hash_file_controlled(abs_path, cancel, pause)?
                else {
                    return Ok(None);
                };
                if hash == row.image_hash {
                    let conn = self.db.lock().expect("poisoned");
                    paths::update_size_mtime(
                        &conn,
                        &row.path_id,
                        hashed_size as i64,
                        mtime_ns,
                        self.now(),
                    )?;
                    Ok(Some(Observed::Updated))
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
                    Ok(Some(Observed::Superseded {
                        old: row.image_hash,
                        new: hash,
                    }))
                }
            }
            Some(ActivePathMatch::CaseAlias(row)) => {
                // The filesystem, not a guessed fs-type table or SQLite
                // collation, proved these spellings alias one entry. Preserve
                // the existing path_id/hash and update its display spelling.
                if row.size == size && (row.mtime_ns - mtime_ns).abs() <= tolerance_ns {
                    let conn = self.db.lock().expect("poisoned");
                    paths::recase_active(
                        &conn,
                        &row.path_id,
                        root_id,
                        rel_path,
                        size,
                        mtime_ns,
                        self.now(),
                    )?;
                    drop(conn);
                    self.bump_images_version();
                    return Ok(Some(Observed::Relinked(row.image_hash)));
                }
                let Some((hash, hashed_size)) =
                    hashing::hash_file_controlled(abs_path, cancel, pause)?
                else {
                    return Ok(None);
                };
                if hash == row.image_hash {
                    let conn = self.db.lock().expect("poisoned");
                    paths::recase_active(
                        &conn,
                        &row.path_id,
                        root_id,
                        rel_path,
                        hashed_size as i64,
                        mtime_ns,
                        self.now(),
                    )?;
                    drop(conn);
                    self.bump_images_version();
                    Ok(Some(Observed::Relinked(hash)))
                } else {
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
                    Ok(Some(Observed::Superseded {
                        old: row.image_hash,
                        new: hash,
                    }))
                }
            }
            None => {
                // §5 re-registration fast path: a `root-removed` stale row at
                // this location whose size+mtime still match relinks with
                // zero hashing (the hash is the one the row carries).
                if let Some(stale) =
                    self.reactivatable_row(volume_id, rel_path, size, mtime_ns, tolerance_ns)?
                {
                    {
                        let conn = self.db.lock().expect("poisoned");
                        let now = self.now();
                        paths::reactivate(&conn, &stale.path_id, root_id.unwrap_or(""), now)?;
                        ingest::revive_retention_cleaned(&conn, &stale.image_hash, now)?;
                    }
                    // Seam 1 (AUDIT-2026-07-07 S3): reactivation IS a relink
                    // (the image re-enters the grid's slice), just via the
                    // zero-hash fast path — same bump as `relink_tx`, after
                    // the write and outside the lock.
                    self.bump_images_version();
                    return Ok(Some(Observed::Relinked(stale.image_hash)));
                }
                let Some((hash, hashed_size)) =
                    hashing::hash_file_controlled(abs_path, cancel, pause)?
                else {
                    return Ok(None);
                };
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
                    Ok(Some(Observed::Relinked(hash)))
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
                    Ok(Some(Observed::NewImage(hash)))
                }
            }
        }
    }

    /// Resolve an active path claim for an observed file without importing a
    /// case-sensitivity guess into path identity. Exact DB spelling always
    /// wins. A case-folded candidate is accepted only when both path
    /// spellings canonicalize to the same path on the live filesystem.
    ///
    /// Consequently default APFS/FAT aliases recase one row, while two
    /// distinct Linux files such as `a.jpg` and `A.jpg` remain two claims.
    pub(crate) fn active_path_match(
        &self,
        volume_id: &str,
        rel_path: &str,
        observed_abs: &Path,
    ) -> Result<Option<ActivePathMatch>, LibraryError> {
        {
            let conn = self.db.lock().expect("poisoned");
            let exact = paths::active_row_at(&conn, volume_id, rel_path)?;
            if exact.is_some() {
                return Ok(exact.map(ActivePathMatch::Exact));
            }
        }
        self.active_case_alias_match(volume_id, rel_path, observed_abs)
    }

    /// Resolve only a differently-cased active candidate. The watcher uses
    /// this before its exact `from` lookup for a case-only rename pair: on a
    /// case-insensitive volume the post-rename `to` spelling aliases the old
    /// row and path identity can be preserved; on a case-sensitive volume the
    /// proof fails and ordinary move correlation remains authoritative.
    pub(crate) fn active_case_alias_match(
        &self,
        volume_id: &str,
        rel_path: &str,
        observed_abs: &Path,
    ) -> Result<Option<ActivePathMatch>, LibraryError> {
        let (candidates, mount_point) = {
            let conn = self.db.lock().expect("poisoned");
            let candidates = paths::active_case_alias_candidates(&conn, volume_id, rel_path)?;
            let mount_point: Option<String> = conn
                .query_row(
                    "SELECT mount_point FROM volumes WHERE volume_id = ?1",
                    params![volume_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            (candidates, mount_point)
        };
        let Some(mount_point) = mount_point else {
            return Ok(None);
        };
        let mount = Path::new(&mount_point);
        let mut matches = candidates.into_iter().filter(|row| {
            self.fs_semantics
                .same_entry(&mount.join(&row.rel_path), observed_abs)
        });
        let Some(row) = matches.next() else {
            return Ok(None);
        };
        // Ambiguity is never resolved by guessing. It should be impossible
        // for multiple directory entries to canonicalize to one path, but
        // hard links and unusual network filesystems make conservatism cheap.
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(Some(ActivePathMatch::CaseAlias(row)))
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
                // Seam 1 (AUDIT-2026-07-07 S4): a live delete/move-away just
                // removed an image from the grid's slice, but staling a row
                // enqueues no pass — without a bump the pump's `prev != status`
                // gate never fires and the ghost thumbnail lingers until an
                // unrelated event. The no-row branch below stays silent: nothing
                // the grid shows changed, so no re-list is owed.
                self.bump_images_version();
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
    ) -> Result<usize, LibraryError> {
        let mut conn = self.db.lock().expect("poisoned");
        let tx = conn.transaction()?;
        let retention_repairs_revived = self.relink_in_tx(
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
        // Seam 1 (AUDIT-2026-07-07 S3): a relink adds an active path for an
        // existing image — the grid's slice changed (a moved/copied file is
        // visible again), so the version must advance exactly like a new
        // image, or the grid shows the pre-move state until an unrelated
        // event fires. After the commit, mirroring `new_image_tx`.
        self.bump_images_version();
        Ok(retention_repairs_revived)
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
    ) -> Result<usize, LibraryError> {
        let path_id = self.mint_ulid();
        let now = self.now();
        paths::insert_active(
            tx, &path_id, hash, volume_id, root_id, rel_path, size, mtime_ns, now,
        )?;
        paths::correlate_move(tx, hash, move_window_start)?;
        ingest::clear_placeholder(tx, volume_id, rel_path)?;
        Ok(ingest::revive_retention_cleaned(tx, hash, now)?)
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
        // Library->view data-version (Seam 1): a NEW image row has committed, so
        // the grid's slice changed. Bump AFTER the commit (the `?` early returns
        // above are errors and must not advance the version) so the pump's
        // `prev != status` emit-gate refreshes the grid over the existing
        // `ingest-progress` channel instead of the old 2s wall-clock relist.
        self.bump_images_version();
        Ok(())
    }

    /// Monotonic per-process image-set version (Seam 1, sibling of
    /// [`crate::retrieval::PpvecStore::vectors_version`]). Views compare it to
    /// the value they last rendered against and re-list only when it advances.
    /// Advances on any committed active-set change (add, supersede, relink,
    /// remove); root removal rides `roots-changed` separately.
    pub fn images_version(&self) -> u64 {
        self.images_version.load(Ordering::Relaxed)
    }

    /// Advance the Seam-1 image-set version. Call AFTER a successful commit
    /// (never before — an aborted transaction must not trigger a re-list of
    /// unchanged data), from every path that changes which images the grid
    /// would show: new image, supersede, relink/reactivate, live remove,
    /// rename-relink, and the scan's went-stale sweep. One chokepoint so a
    /// future mutation path can't quietly miss the bump again
    /// (AUDIT-2026-07-07 S3/S4).
    pub(crate) fn bump_images_version(&self) {
        self.images_version.fetch_add(1, Ordering::Relaxed);
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
                // NO full-raw-decode row at ingest (June 2026, on-demand): the
                // develop pass is view-time only now, never eager-enqueued.
                ingest::enqueue(
                    tx,
                    hash,
                    PassName::Preview,
                    PassState::Skipped,
                    priority,
                    Some("deferred-heic"),
                    now,
                )?;
            }
            ImageFormat::Raw => {
                // The preview pass produces the embedded preview only; the
                // full RAW develop is ON-DEMAND (view-time trigger), never
                // enqueued here. No full-raw-decode row at ingest.
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
                // Non-RAW originals: the preview pass owns them; full-raw-
                // decode is structurally inapplicable AND on-demand, so NO row
                // is created here (June 2026 — the old `skipped`/`inapplicable`
                // row is gone with the eager-enqueue removal).
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
        // Seam 1 (AUDIT-2026-07-07 S3): an in-place edit swaps which image
        // lives at this path — the grid must re-list to show the new content
        // hash. Bump after the commit (the in-tx helpers above deliberately
        // do NOT bump, so this is the single bump for the whole supersede).
        self.bump_images_version();
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
    /// PIPELINED (BACKLOG "Ingest pass pipelining", June 2026). The drain was a
    /// WAVE loop: claim `pool_width` rows, run them all in parallel, then a
    /// barrier — the next wave could not be claimed until the SLOWEST item of
    /// the current one finished. A single big-RAW preview drained the pool to
    /// one busy worker while the rest sat idle waiting for the wave to end and
    /// the next claim to run under the lock. The pipeline removes that barrier:
    /// a dedicated CLAIMER feeds a BOUNDED channel that `pool_width` worker
    /// tasks pull from CONTINUOUSLY, so a slow item only occupies its own
    /// worker — every other worker keeps pulling fresh rows. This overlaps
    /// claiming (DB-bound) with preview decode/resize/encode (CPU-bound) and,
    /// because the scan thread enqueues rows per hash-batch, lets preview run on
    /// already-hashed items WHILE later items are still being scanned/hashed.
    ///
    /// BOUNDED for backpressure (the §10.3 memory bound): a decoded full-res
    /// frame is tens of MB, so the work channel is a `sync_channel` whose
    /// capacity is the pool width. Peak in-flight decoded frames are then the
    /// items being worked (one per worker, `pool_width`) plus the items queued
    /// (`pool_width`), i.e. ~`2 * pool_width` — exactly the OLD wave's worst
    /// case (a wave held `pool_width` decoded frames in flight), so memory does
    /// not regress. A full channel BLOCKS the claimer, which is the
    /// backpressure: a slow embed/preview stage cannot let an unbounded queue of
    /// decoded images blow up memory. (`std::sync::mpsc::sync_channel` is the
    /// codebase's existing bounded-channel idiom — see `watcher.rs`,
    /// `runtime/bus.rs` — so no new crate is pulled in just to get a bounded
    /// queue.)
    ///
    /// The DB stays the source of truth: `claim_next` still flips pending→running
    /// durably; the channel is purely an in-memory SCHEDULING layer on top.
    ///
    /// Cancel/max_items are honored PER ITEM in the claimer (finer than the old
    /// per-wave check). On cancel the claimer stops claiming and drops the
    /// sender; workers finish the items already handed to them (so no row is
    /// left stuck `running`) and exit as the channel closes. Un-claimed rows
    /// stay `pending` for the next turn. First hard plumbing error (DB/IO —
    /// per-item pass failures are RECORDED, not returned) propagates after the
    /// run, mirroring the old loop's abort semantics.
    pub fn process_queue(&self, opts: &QueueOptions) -> Result<QueueReport, LibraryError> {
        let drain_started = std::time::Instant::now();
        let report = self.run_pipeline(
            opts,
            worker_pool(),
            ingest::claim_next,
            |lib, item, local| lib.run_pass(item, local),
        )?;
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

    /// Drain only the small metadata pass, leaving preview/model/full-decode
    /// rows pending. The desktop uses this under a critical app-data
    /// free-space condition: EXIF indexing can continue, but every large,
    /// reproducible artifact writer is safely paused until capacity recovers.
    pub fn process_essential_queue(
        &self,
        opts: &QueueOptions,
    ) -> Result<QueueReport, LibraryError> {
        self.run_pipeline(
            opts,
            worker_pool(),
            |conn, now| ingest::claim_next_of(conn, now, &[PassName::Exif], true),
            |lib, item, local| lib.run_pass(item, local),
        )
    }

    /// Drain only preview generation. The desktop schedules this behind the
    /// metadata/live-ingest lane so a large decode backlog cannot delay fresh
    /// photo discovery and EXIF visibility.
    pub fn process_preview_queue(&self, opts: &QueueOptions) -> Result<QueueReport, LibraryError> {
        self.run_pipeline(
            opts,
            worker_pool(),
            |conn, now| ingest::claim_next_of(conn, now, &[PassName::Preview], true),
            |lib, item, local| lib.run_pass(item, local),
        )
    }

    /// The shared bounded-channel pipeline (BACKLOG "Ingest pass pipelining").
    /// A CLAIMER (the calling thread) flips pending→running under the DB lock
    /// and feeds a BOUNDED `sync_channel`; `pool_width` worker tasks on `pool`
    /// pull from it continuously and run `work` (the CPU pass + its own DB
    /// writes). See [`process_queue`] for the full topology + memory-bound
    /// rationale. Generic over `claim`/`work` so both the M1 exif/preview drain
    /// and the full-raw-decode drain reuse one correctness-critical loop.
    ///
    /// `claim`: flips the next runnable row to `running` under the lock (the
    /// caller holds the connection mutex when this runs). `work`: runs one pass
    /// to completion (marks the row done/failed/skipped, tallies into the
    /// per-worker [`QueueReport`]). Returns the merged report; `processed` is
    /// counted at claim time, `cancelled` reflects the cancel flag, and the
    /// first hard plumbing error from any worker propagates.
    fn run_pipeline<C, W>(
        &self,
        opts: &QueueOptions,
        pool: &rayon::ThreadPool,
        claim: C,
        work: W,
    ) -> Result<QueueReport, LibraryError>
    where
        C: Fn(&Connection, UtcMillis) -> rusqlite::Result<Option<ingest::QueueItem>> + Sync,
        W: Fn(&Library, &ingest::QueueItem, &mut QueueReport) -> Result<(), LibraryError> + Sync,
    {
        let width = opts
            .max_concurrency
            .unwrap_or_else(|| pool.current_num_threads())
            .clamp(1, pool.current_num_threads().max(1));
        let mut total = QueueReport::default();
        // A one-thread Rayon pool cannot host both `scope`'s producer closure
        // and its spawned consumer: the producer fills the bounded channel and
        // waits forever while the only pool thread is the producer itself.
        // Run the identical claim -> work contract serially at width 1. This is
        // also cheaper than constructing channels for Eco/single-core work.
        if width == 1 {
            loop {
                if opts.is_cancelled() {
                    total.cancelled = true;
                    break;
                }
                if let Some(max) = opts.max_items
                    && total.processed >= max
                {
                    break;
                }
                let claimed = self
                    .metrics
                    .queue_claim
                    .time(|| -> Result<_, LibraryError> {
                        let waiting = std::time::Instant::now();
                        let conn = self.db.lock().expect("poisoned");
                        self.catalog_metrics
                            .queue_claim_wait
                            .record(waiting.elapsed());
                        self.catalog_metrics
                            .queue_claim_operation
                            .time(|| Ok(claim(&conn, self.now())?))
                    })?;
                let Some(item) = claimed else { break };
                total.processed += 1;
                let mut local = QueueReport::default();
                work(self, &item, &mut local)?;
                total.absorb(&local);
            }
            return Ok(total);
        }
        // BOUNDED for backpressure: capacity == pool width keeps peak in-flight
        // decoded frames at ~2*width (width queued + width being worked), the
        // old wave's worst case, so a slow stage cannot grow an unbounded queue
        // of large decoded images. A full channel blocks the claimer.
        let (work_tx, work_rx) = std::sync::mpsc::sync_channel::<ingest::QueueItem>(width);
        // Per-worker reports flow back unbounded — they are tiny tallies, never
        // a memory concern, and the collector drains them after the claimer.
        let (report_tx, report_rx) =
            std::sync::mpsc::channel::<Result<QueueReport, LibraryError>>();
        let work_rx = std::sync::Mutex::new(work_rx);
        // A claim-time DB error aborts the drain (drain-level plumbing failure,
        // unlike a per-item pass failure which is RECORDED). Captured here and
        // returned after the in-flight items finish.
        let mut claim_err: Option<LibraryError> = None;

        // The claimer runs on THIS thread inside the pool scope; the workers are
        // spawned tasks. `scope` joins all workers before returning, so the
        // borrows (`self`, `claim`, `work`, the channels) outlive every task.
        pool.scope(|scope| {
            for _ in 0..width {
                let work_rx = &work_rx;
                let report_tx = report_tx.clone();
                let work = &work;
                scope.spawn(move |_| {
                    // Pull-loop: a worker only ever holds ONE item past the
                    // channel (the one it is decoding), so the channel capacity
                    // is the true memory bound. `recv` returns Err when the
                    // claimer drops `work_tx` AND the queue is drained — clean
                    // wind-down, including on cancel.
                    loop {
                        let item = {
                            let rx = work_rx.lock().expect("poisoned");
                            rx.recv()
                        };
                        let Ok(item) = item else { break };
                        let mut local = QueueReport::default();
                        let outcome = work(self, &item, &mut local).map(|()| local);
                        if report_tx.send(outcome).is_err() {
                            break;
                        }
                    }
                });
            }
            // Drop our own report sender so the collector's recv loop ends once
            // every worker has dropped its clone.
            drop(report_tx);

            // The claimer: claim → send (blocks when the channel is full =
            // backpressure) until cancel/max_items/empty queue. Dropping
            // `work_tx` afterward signals the workers to wind down.
            loop {
                if opts.is_cancelled() {
                    total.cancelled = true;
                    break;
                }
                if let Some(max) = opts.max_items
                    && total.processed >= max
                {
                    break;
                }
                let claimed = self
                    .metrics
                    .queue_claim
                    .time(|| -> Result<_, LibraryError> {
                        let waiting = std::time::Instant::now();
                        let conn = self.db.lock().expect("poisoned");
                        self.catalog_metrics
                            .queue_claim_wait
                            .record(waiting.elapsed());
                        self.catalog_metrics
                            .queue_claim_operation
                            .time(|| Ok(claim(&conn, self.now())?))
                    });
                let item = match claimed {
                    Ok(Some(item)) => item,
                    Ok(None) => break, // queue empty: wind down
                    Err(e) => {
                        // A claim-time DB error is the drain-level abort: stop
                        // feeding and let the in-flight items finish.
                        claim_err = Some(e);
                        break;
                    }
                };
                total.processed += 1;
                if work_tx.send(item).is_err() {
                    // All workers gone (cannot happen before the scope joins,
                    // but keep the loop honest): stop claiming.
                    break;
                }
            }
            drop(work_tx); // workers' recv now returns Err once drained
        });

        // Collect every per-worker report. The scope has joined, so all senders
        // are dropped and this recv loop terminates.
        let mut first_err = claim_err;
        for outcome in report_rx.iter() {
            match outcome {
                Ok(local) => total.absorb(&local),
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(total)
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
                // Tier-1 near-dup dHash off the SAME decoded, display-oriented,
                // sRGB image — near-free here (a 72 px² downscale + 64 compares)
                // and exactly the "compute it in the preview pass" hook the
                // design doc calls for. Stored below in the mark_done txn.
                let phash = phash::dhash(&img);
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
                    self.record_perceptual_hash_locked(&conn, &item.image_hash, phash)?;
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
        let extracted = match self
            .metrics
            .raw_extract
            .time(|| self.extractor.extract(abs))
        {
            Ok(x) => x,
            Err(PreviewError::UnsupportedRaw(reason)) => {
                tracing::warn!(
                    hash = %item.image_hash,
                    error = %reason,
                    "RAW container unsupported or invalid; preview skipped"
                );
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_skipped(&conn, item, "unsupported-or-invalid-raw", self.now())?;
                report.skipped += 1;
                return Ok(());
            }
            Err(e) => return self.fail_preview(item, e, report),
        };
        let now = self.now();
        match extracted {
            None => {
                // No embedded preview at all (§9.3): UI placeholder, and the
                // RAW stays on-demand like any other — the develop runs only
                // when the image is viewed (view-time trigger), never eager.
                //
                // TODO(review): no embedded preview means no substrate until
                // first view. This is the one removal site the founder flagged
                // for further review — an eager develop could be argued here
                // (there is nothing to show meanwhile), but Phase 1 stays
                // fully on-demand. Revisit if dogfood shows a no-embedded-
                // preview RAW feels broken before its first view.
                let conn = self.db.lock().expect("poisoned");
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
                // Embedded-preview accept threshold from the centralized tuning
                // config (code default 2048; file-overridable via tuning.toml).
                let accept_edge = crate::tuning::tuning().preview.embedded_accept_edge;
                let meets_threshold = pw.max(ph) >= accept_edge;
                // Tier-1 near-dup dHash off the oriented embedded preview (the
                // RAW's own JPEG thumbnail) — the same near-free hook as the
                // original path. A RAW and its exported JPEG share this embedded
                // preview's look, so they near-dup-match as intended.
                let phash = phash::dhash(&oriented);
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
                // ON-DEMAND (June 2026): the full RAW develop is NO LONGER
                // enqueued here. `needs_full_decode = !meets_threshold` still
                // rides on the artifact rows above — it drives the UI's "full
                // decode pending" label and tells the view-time trigger a
                // develop is worth requesting — but the develop pass row is
                // created only when the image is actually viewed (the new
                // `request_full_decode` command at PRIORITY_INTERACTIVE), not
                // at ingest. This is what dissolves the 154-stuck-rows bug:
                // with no eager enqueue there is no pending count to misread,
                // and a stroked RAW develops when viewed like any other.
                let _ = meets_threshold;
                self.record_perceptual_hash_locked(&conn, &item.image_hash, phash)?;
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

    // -----------------------------------------------------------------------
    // Full-raw-decode pass (§9.4 / OD-1): on-demand neutral develop to a
    // native-resolution sRGB artifact, plus the 2560 display+thumb tiers.
    // -----------------------------------------------------------------------

    /// Drain the full-raw-decode queue on the SEPARATE decode pool (§10.3).
    /// Modeled on `process_embedding_queue`: a per-item loop claiming only
    /// `FullRawDecode` rows, honoring `opts.cancel` PER ITEM (so an armed mic
    /// preempts between develops — one develop is seconds, the §10.3
    /// politeness bound), reporting via `QueueReport`. The decode pool is net-
    /// new (the M1 wave pool is memory-light; a full develop is not), so the
    /// claim+develop for each item is `install`ed onto it.
    pub fn process_raw_decode_queue(
        &self,
        opts: &QueueOptions,
    ) -> Result<QueueReport, LibraryError> {
        let mut report = QueueReport::default();
        let allowed = [ingest::PassName::FullRawDecode];
        loop {
            if opts.is_cancelled() {
                report.cancelled = true;
                break;
            }
            if let Some(max) = opts.max_items
                && report.processed >= max
            {
                break;
            }
            let item = {
                let waiting = std::time::Instant::now();
                let conn = self.db.lock().expect("poisoned");
                self.catalog_metrics
                    .queue_claim_wait
                    .record(waiting.elapsed());
                // Full-RAW decode reads the original file: require an online path
                // so an offline volume does not churn the queue.
                self.catalog_metrics
                    .queue_claim_operation
                    .time(|| ingest::claim_next_of(&conn, self.now(), &allowed, true))?
            };
            let Some(item) = item else { break };
            report.processed += 1;
            // The develop itself runs on the decode pool (one item at a time
            // from this loop; the pool bounds peak memory across any parallel
            // drains). DB touches stay on the connection mutex as everywhere.
            decode_pool().install(|| self.run_full_raw_decode_pass(&item, &mut report))?;
        }
        Ok(report)
    }

    /// Develop ONE RAW to its full-resolution + display artifacts (§9.4).
    /// Mirrors `run_preview_pass_raw`'s path resolution (best_path +
    /// offline-defer, no attempt burned) and `run_preview_pass_original`'s
    /// artifact-write + record shape, with the new develop in the middle.
    ///
    /// Best-effort discipline (PLAN): the embedded preview always stands, so
    /// no surprise must crash the pool thread. The rawler decode + develop is
    /// wrapped panic-safe (`catch_unwind`) because several rawler 0.7.2
    /// methods are `todo!()` panics on unexpected formats; a panic marks the
    /// row failed (non-transient), never unwinds the pool.
    fn run_full_raw_decode_pass(
        &self,
        item: &ingest::QueueItem,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let Some(image) = self.image(&item.image_hash)? else {
            let conn = self.db.lock().expect("poisoned");
            ingest::mark_failed(&conn, item, "missing-image-row", false, self.now())?;
            report.errors += 1;
            return Ok(());
        };
        // Resolve a readable ONLINE original; offline = ordinary transient
        // defer (no attempt burned), exactly like run_pass.
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
            let conn = self.db.lock().expect("poisoned");
            ingest::defer_offline(&conn, item, self.now())?;
            report.transient_retries += 1;
            return Ok(());
        };

        // Decode + develop, panic-safe. The closure returns the develop
        // RESULT; a panic inside rawler (an unimplemented sibling on a
        // surprise format) is caught and turned into a permanent failure.
        let exif_orientation = image.exif_orientation;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_and_develop(&abs, exif_orientation)
        }));
        let developed = match outcome {
            Ok(Ok(img)) => img,
            Ok(Err(DecodeDevelopError::Unsupported(reason))) => {
                // A CFA we do not demosaic (X-Trans/RGBE/CYGM/mono): SKIP
                // clean — the embedded preview is correct, just not 1:1.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_skipped(
                    &conn,
                    item,
                    &format!("unsupported-cfa: {reason}"),
                    self.now(),
                )?;
                report.skipped += 1;
                return Ok(());
            }
            Ok(Err(DecodeDevelopError::Io(e))) => {
                // Transient: a mid-read volume hiccup. Retry with backoff.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &format!("io: {e}"), true, self.now())?;
                report.transient_retries += 1;
                return Ok(());
            }
            Ok(Err(DecodeDevelopError::Permanent(msg))) => {
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &msg, false, self.now())?;
                report.errors += 1;
                return Ok(());
            }
            Err(_panic) => {
                // rawler `todo!()`/panic on a surprising format: permanent,
                // never crash the pool.
                tracing::warn!(hash = %item.image_hash, "full-raw-decode panicked; marking failed");
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, "decode-panic", false, self.now())?;
                report.errors += 1;
                return Ok(());
            }
        };

        use image::GenericImageView;
        let (full_w, full_h) = developed.dimensions();

        // OD-1: write the NATIVE-resolution artifact (the 100%-zoom surface),
        // then the 2560 display+thumb tiers (grid/fit) from the same develop.
        preview::write_full_decode_artifact(&self.cache_dir, &item.image_hash, &developed)
            .map_err(|e| match e {
                PreviewError::Io(io) => LibraryError::Io(io),
                PreviewError::UnsupportedRaw(reason) => {
                    LibraryError::Watch(format!("full-decode unsupported raw: {reason}"))
                }
                PreviewError::Decode(d) => LibraryError::Watch(format!("full-decode encode: {d}")),
            })?;
        let artifacts =
            preview::write_artifacts(&self.cache_dir, &item.image_hash, &developed, &self.metrics)?;

        // Geometry-safety (§9.4, OD-1): the native artifact's oriented aspect
        // must agree with the display artifact's, or strokes drawn over the
        // 2560 substrate would misplace at deep zoom. They come from the SAME
        // develop so this is belt-and-braces, but the invariant is load-
        // bearing enough to assert before the artifact can ever be served.
        if let Some(display) = artifacts.iter().find(|a| a.kind == ArtifactKind::Display) {
            let native_aspect = f64::from(full_w) / f64::from(full_h.max(1));
            let disp_aspect = f64::from(display.width) / f64::from(display.height.max(1));
            if ((native_aspect - disp_aspect) / disp_aspect).abs()
                >= preview::EMBEDDED_NATIVE_ASPECT_TOLERANCE
            {
                // Should be impossible (one develop, one orientation); if it
                // ever fires, drop the native artifact and fail rather than
                // serve a geometry-mismatched 1:1.
                let _ = std::fs::remove_file(preview::full_artifact_path(
                    &self.cache_dir,
                    &item.image_hash,
                    preview::FullDecodeFormat::for_dimensions(full_w, full_h),
                ));
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(
                    &conn,
                    item,
                    "full-decode geometry disagreement",
                    false,
                    self.now(),
                )?;
                report.errors += 1;
                return Ok(());
            }
        }

        let conn = self.db.lock().expect("poisoned");
        // source='full-decode', needs_full_decode cleared: the metadata label
        // flips from "full decode pending" to just the name (UI resolves it).
        self.record_artifacts_locked(
            &conn,
            &item.image_hash,
            &artifacts,
            PreviewSource::FullDecode,
            false,
        )?;
        ingest::mark_done(&conn, item, self.now())?;
        report.done += 1;
        report.completed_previews.push(item.image_hash.clone());
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

    /// Store an image's Tier-1 perceptual hash (DESIGN-DEDUP-AND-SIMILARITY.md
    /// §"Tier 1"). Called from inside the preview pass's already-held DB lock,
    /// in the same transaction as `mark_done`, so the hash and the preview land
    /// atomically. The `u64` dHash is bit-reinterpreted to `i64` for SQLite's
    /// signed INTEGER; `find_near_duplicates` reverses it before XOR+popcount,
    /// so every bit round-trips and the sign is never observed.
    fn record_perceptual_hash_locked(
        &self,
        conn: &Connection,
        hash: &ContentHash,
        phash: u64,
    ) -> rusqlite::Result<()> {
        conn.execute(
            "UPDATE images SET perceptual_hash = ?2 WHERE image_hash = ?1",
            params![hash.as_str(), phash as i64],
        )?;
        Ok(())
    }

    pub fn pass_counters(
        &self,
    ) -> Result<std::collections::BTreeMap<(String, i64), PassCounters>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        Ok(ingest::pass_counters(&conn)?)
    }

    /// Processable queue counters for the desktop activity projection.
    /// Archived roots keep their durable rows but do not appear as live work.
    pub fn active_pass_counters(
        &self,
    ) -> Result<std::collections::BTreeMap<(String, i64), PassCounters>, LibraryError> {
        let waiting = std::time::Instant::now();
        let conn = self.db.lock().expect("poisoned");
        self.catalog_metrics.activity_wait.record(waiting.elapsed());
        self.catalog_metrics
            .activity_operation
            .time(|| Ok(ingest::active_pass_counters(&conn)?))
    }

    /// Fixed-cardinality ingest failure groups for benchmarks and health
    /// receipts. Raw error strings can contain decoder details and must not
    /// become metric labels; this deliberately emits only a coarse category
    /// plus normalized format/subtype tokens.
    pub fn ingest_error_summary(&self) -> Result<Vec<IngestErrorSummary>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT p.pass_name, p.error, i.format, i.raw_subtype
             FROM ingest_passes p
             JOIN images i ON i.image_hash = p.image_hash
             WHERE p.state = 'error'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut grouped =
            std::collections::BTreeMap::<(String, String, String, Option<String>), u64>::new();
        for row in rows {
            let (pass, error, format, raw_subtype) = row?;
            let key = (
                metric_token(&pass),
                ingest_error_category(error.as_deref()).to_owned(),
                metric_token(&format),
                raw_subtype.as_deref().map(metric_token),
            );
            *grouped.entry(key).or_default() += 1;
        }
        Ok(grouped
            .into_iter()
            .map(
                |((pass, category, format, raw_subtype), count)| IngestErrorSummary {
                    pass,
                    category,
                    format,
                    raw_subtype,
                    count,
                },
            )
            .collect())
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

    /// Start a watcher whose event hashing and recovery scans are admitted by
    /// a shell-supplied dynamic policy. The returned guard is held for the
    /// complete event/reconcile unit; returning `None` cancels that turn.
    pub fn start_watcher_with_options<F, G>(
        self: &Arc<Self>,
        root_id: &str,
        policy: F,
    ) -> Result<watcher::RootWatcherHandle, LibraryError>
    where
        F: Fn(&CancelFlag) -> Option<(ScanOptions, G)> + Send + Sync + 'static,
        G: Send + 'static,
    {
        watcher::start_root_watcher_with_options(self, root_id, policy)
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
        // WHY: only surface images that still have at least one ACTIVE path.
        // An image whose every path went stale (file deleted, root removed) is
        // orphaned: it must drop out of this enumeration so callers (embedding
        // re-pend sweeps, the lens) stop treating it as live work. Mirrors the
        // `state = 'active'` filter `list_folder()` already applies.
        let mut stmt = conn.prepare(
            "SELECT image_hash FROM images
             WHERE image_hash IN (SELECT DISTINCT image_hash FROM paths WHERE state = 'active')
             ORDER BY image_hash",
        )?;
        let rows = stmt.query_map([], |r| {
            let h: String = r.get(0)?;
            ContentHash::from_hex(&h).map_err(|_| rusqlite::Error::InvalidQuery)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tier-1 near-duplicate detection over a scope (DESIGN-DEDUP-AND-SIMILARITY
    /// .md §"Tier 1"). Given the image hashes that define a scope (folder /
    /// collection / library — the shell resolves the scope to hashes) and a
    /// Hamming `threshold`, returns GROUPS of images whose perceptual hashes are
    /// transitively within `threshold` of one another (union-find over pairs).
    ///
    /// Only images that actually carry a perceptual hash participate; images
    /// still awaiting the preview pass (NULL `perceptual_hash`) are silently
    /// skipped rather than mis-grouped as all-zero. The scan is linear O(n²)
    /// over the scope by design (the doc: linear is adequate at our scale; the
    /// BK-tree is an optional optimization).
    ///
    /// Exact byte-identical files already collapse upstream (one BLAKE3
    /// `image_hash` == one row, K13), so this tier surfaces the NEAR-dups: the
    /// re-encodes, resizes, and light edits that share a look but not bytes.
    pub fn find_near_duplicates(
        &self,
        scope: &[ContentHash],
        threshold: u32,
    ) -> Result<Vec<DuplicateGroup>, LibraryError> {
        if scope.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.lock().expect("poisoned");
        // Pull (hash, phash) for the in-scope images that have a phash. We query
        // per-hash rather than build a giant IN-list: the scope can be tens of
        // thousands, past SQLite's bound-parameter / expression-depth limits,
        // and a prepared single-row lookup reused across the scope is simple and
        // index-hits the PRIMARY KEY. The grouping cost (O(n²)) dominates this
        // O(n) read anyway.
        let mut stmt = conn.prepare("SELECT perceptual_hash FROM images WHERE image_hash = ?1")?;
        let mut items: Vec<(String, u64)> = Vec::with_capacity(scope.len());
        for h in scope {
            // Two layers of "absent": the IMAGE row may not exist for this hash
            // (`.optional()` → outer Option), and the `perceptual_hash` COLUMN
            // may be NULL for an image not yet through the preview pass (the
            // closure's `Option<i64>` → inner Option). Both mean "no hash to
            // group", so we skip; only a present, non-NULL value participates.
            let phash: Option<Option<i64>> = stmt
                .query_row(params![h.as_str()], |r| r.get::<_, Option<i64>>(0))
                .optional()?;
            if let Some(Some(bits)) = phash {
                // Reverse the i64<->u64 bit-reinterpret used at store time.
                items.push((h.as_str().to_owned(), bits as u64));
            }
        }
        drop(stmt);
        drop(conn);
        Ok(phash::group_near_duplicates(&items, threshold))
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
        // Also drop the on-disk full-decode artifact (whichever format), so a
        // re-develop is not short-circuited by a stale cache hit.
        if let Some((p, _fmt)) = preview::existing_full_artifact(&self.cache_dir, hash) {
            let _ = std::fs::remove_file(&p);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // On-demand full-raw-decode trigger (view-time, §9.4 / OD-1)
    // -----------------------------------------------------------------------

    /// Whether a cached full-decode artifact already exists for `hash` — the
    /// view-time trigger's cache-hit signal. The artifact lives on disk only
    /// (no DB row); its existence IS "developed". A cache hit means
    /// `request_full_decode` can no-op and Look serves `/full-decode` straight
    /// away.
    pub fn full_decode_cached(&self, hash: &ContentHash) -> bool {
        preview::existing_full_artifact(&self.cache_dir, hash).is_some()
    }

    /// Enqueue ONE full-raw-decode pass row at the top interactive priority
    /// (a user is staring at a "developing..." spinner — §10.3). Idempotent:
    /// a re-request is a no-op once the row exists (`enqueue` keeps the
    /// existing row); a cached artifact means there is nothing to do. RAW
    /// originals only — other formats have no develop to run. Returns whether
    /// a develop is now pending (true) or the cache already had it (false).
    ///
    /// This is the SOLE creator of full-raw-decode rows now (the eager ingest
    /// enqueues were removed): no row exists until the image is viewed.
    pub fn request_full_decode(&self, hash: &ContentHash) -> Result<bool, LibraryError> {
        if self.full_decode_cached(hash) {
            return Ok(false);
        }
        let Some(record) = self.image(hash)? else {
            return Ok(false);
        };
        if record.format != ImageFormat::Raw {
            return Ok(false);
        }
        let now = self.now();
        let conn = self.db.lock().expect("poisoned");
        // If a prior attempt errored/skipped, re-pend it at the interactive
        // priority — the user is asking again, explicitly. A `done` row whose
        // artifact is missing also re-pends (the doctor pattern); a `done` row
        // WITH the artifact was caught by the cache check above.
        ingest::enqueue(
            &conn,
            hash,
            PassName::FullRawDecode,
            PassState::Pending,
            ingest::PRIORITY_INTERACTIVE,
            None,
            now,
        )?;
        conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', not_before = NULL, priority = ?2, error = NULL
             WHERE image_hash = ?1 AND pass_name = 'full-raw-decode'
               AND state IN ('error','skipped','done')",
            params![hash.as_str(), ingest::PRIORITY_INTERACTIVE],
        )?;
        ingest::promote(
            &conn,
            hash,
            PassName::FullRawDecode,
            ingest::PRIORITY_INTERACTIVE,
        )?;
        Ok(true)
    }

    /// Viewport-first preview generation (OD-2, June 2026): the grid sends the
    /// hashes the user is currently LOOKING at (visible + a small look-ahead
    /// margin) so their thumbnails jump the queue. Right after a fresh scan or
    /// "Rebuild all previews", hundreds of preview passes sit pending at
    /// backfill priority in roughly scan order; without this, scrolling to row
    /// 200 means waiting while the pump grinds through rows 1-199. This bumps
    /// the PENDING preview rows for the given hashes up to the top interactive
    /// priority (the same rank `request_full_decode` uses — a user staring at a
    /// blank cell is as urgent as one staring at a develop spinner), so the
    /// pump's `(priority, enqueued_at)` claim picks them next.
    ///
    /// Promotion only (§10.3 never demotes), and ONLY pending preview rows are
    /// touched: a `running` row is regenerating right now, and `done` rows have
    /// nothing left to do. Hashes without a pending preview row are silently a
    /// no-op (already generated, or not a preview-bearing image). This reorders
    /// SERVER generation; the frontend's `thumbqueue` independently orders the
    /// client-side LOAD of already-generated thumbs, so the two cooperate
    /// rather than fight: this gets the bytes made first, that gets them drawn
    /// first. Returns the number of rows actually promoted.
    ///
    /// No pump nudge is needed: the pump polls at a fixed idle interval and the
    /// next claim honors the bumped priority, exactly as `request_full_decode`
    /// relies on (there is no wake channel; promotion is enough).
    pub fn prioritize_previews(&self, hashes: &[ContentHash]) -> Result<usize, LibraryError> {
        if hashes.is_empty() {
            return Ok(0);
        }
        let conn = self.db.lock().expect("poisoned");
        // One promote-shaped UPDATE per hash inside a single lock — matches the
        // codebase's per-hash query style (list_images) rather than building an
        // IN-list, and reuses the exact §10.3 promotion predicate: pending +
        // strictly-worse priority only, so we never demote a P0 watcher row and
        // never disturb running/done.
        let mut promoted = 0usize;
        for hash in hashes {
            promoted += conn.execute(
                "UPDATE ingest_passes SET priority = ?2
                 WHERE image_hash = ?1 AND pass_name = 'preview'
                   AND state = 'pending' AND priority > ?2",
                params![hash.as_str(), ingest::PRIORITY_INTERACTIVE],
            )?;
        }
        Ok(promoted)
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
        let waiting = std::time::Instant::now();
        let conn = self.db.lock().expect("poisoned");
        self.catalog_metrics
            .folder_list_wait
            .record(waiting.elapsed());
        self.catalog_metrics.folder_list_operation.time(|| {
            let root_rel_path: Option<String> = conn
                .query_row(
                    "SELECT rel_path FROM roots WHERE root_id = ?1",
                    params![root_id],
                    |row| row.get(0),
                )
                .optional()?;
            let root_rel_path =
                root_rel_path.ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
            list_folder_on(&conn, root_id, folder, &root_rel_path)
        })
    }

    /// Revisioned, folder-scoped grid catch-up.
    ///
    /// `since_revision = 0` requests an initial full snapshot. A full snapshot
    /// is also returned when the caller's cursor is ahead of this database
    /// (restart/restore) or older than the bounded durable history. Otherwise
    /// only hashes touched in this root's direct-child folder are resolved
    /// against the same SQLite snapshot, so missed desktop events are safe:
    /// each response advances to an exact `to_revision`.
    pub fn list_folder_delta(
        &self,
        root_id: &str,
        folder: &str,
        since_revision: u64,
    ) -> Result<FolderDelta, LibraryError> {
        let waiting = std::time::Instant::now();
        let mut conn = self.db.lock().expect("poisoned");
        self.catalog_metrics
            .folder_delta_wait
            .record(waiting.elapsed());
        self.catalog_metrics.folder_delta_operation.time(|| {
            let root_rel_path: Option<String> = conn
                .query_row(
                    "SELECT rel_path FROM roots WHERE root_id = ?1",
                    params![root_id],
                    |row| row.get(0),
                )
                .optional()?;
            let root_rel_path =
                root_rel_path.ok_or_else(|| LibraryError::NotFound(format!("root {root_id}")))?;
            let tx = conn.transaction()?;
            let head: i64 = tx.query_row(
                "SELECT revision FROM folder_change_clock WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            let oldest: Option<i64> =
                tx.query_row("SELECT MIN(revision) FROM folder_change_log", [], |row| {
                    row.get(0)
                })?;
            let since = i64::try_from(since_revision).ok();
            let reset = match since {
                None => true,
                Some(0) => true,
                Some(value) if value > head => true,
                Some(value) if value < head => oldest.is_none_or(|floor| value < floor - 1),
                Some(_) => false,
            };

            let from_revision = since_revision;
            let to_revision = u64::try_from(head).unwrap_or(0);
            if reset {
                let upserts = list_folder_on(&tx, root_id, folder, &root_rel_path)?;
                tx.commit()?;
                return Ok(FolderDelta {
                    from_revision,
                    to_revision,
                    reset: true,
                    upserts,
                    removed_hashes: Vec::new(),
                });
            }

            let since = since.expect("validated non-reset revision");
            let prefix = folder_volume_prefix(&root_rel_path, folder);
            let mut changed_stmt = tx.prepare_cached(
                "SELECT DISTINCT image_hash
                 FROM folder_change_log
                 WHERE revision > ?1 AND revision <= ?2 AND root_id = ?3
                   AND substr(rel_path, 1, length(?4)) = ?4
                   AND instr(substr(rel_path, length(?4) + 1), '/') = 0
                 ORDER BY image_hash",
            )?;
            let changed = changed_stmt
                .query_map(params![since, head, root_id, prefix], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(changed_stmt);

            let upserts =
                list_changed_folder_images_on(&tx, root_id, folder, &root_rel_path, &changed)?;
            let present: std::collections::BTreeSet<&str> =
                upserts.iter().map(|item| item.hash.as_str()).collect();
            let removed_hashes = changed
                .into_iter()
                .filter(|hash| !present.contains(hash.as_str()))
                .map(|hash| ContentHash::from_hex(&hash))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| LibraryError::Invalid("invalid hash in folder change log".into()))?;
            tx.commit()?;
            Ok(FolderDelta {
                from_revision,
                to_revision,
                reset: false,
                upserts,
                removed_hashes,
            })
        })
    }

    /// Badge rows for an explicit hash list — the collection-members grid
    /// read (RETRIEVAL §10, the rail's Collections tab). Same badge shape
    /// as [`Library::list_folder`]; one prepared statement executed per
    /// hash because membership lists are working-set sized (B71: tens to
    /// low hundreds), unlike the 20k folders that forced the batched
    /// folder query. Input order is preserved.
    ///
    /// Only hashes the index has NEVER ingested are skipped (no `images`
    /// row — e.g. members union-merged from another replica's export,
    /// RETRIEVAL §10.2): there is nothing to put in a grid cell for them.
    /// An indexed member whose every path went STALE (file deleted, root
    /// removed) still renders — membership outlives files (§10.1, B71),
    /// its journal and preview persist, and the collection-side
    /// member_count keeps counting it; dropping it here would make the
    /// rail badge and the grid disagree forever. Such a member reads
    /// `offline: true` (no active online path), so it shows like an
    /// offline-volume member rather than vanishing.
    pub fn list_images(&self, hashes: &[ContentHash]) -> Result<Vec<FolderImage>, LibraryError> {
        let conn = self.db.lock().expect("poisoned");
        // One representative path per image: active-and-online first (an
        // online copy should drive the file name shown), then any active
        // path, then a stale one (a member with only stale paths must
        // still render — see the doc comment), then rel_path for
        // determinism. Root rel_path rides along so the returned rel_path
        // stays root-relative like list_folder's; root_id rides along so
        // the frontend's stack pairing can tell roots apart.
        let mut stmt = conn.prepare_cached(
            "SELECT p.rel_path, COALESCE(rt.rel_path, ''), p.root_id,
                    i.capture_ts, i.first_ingested_at,
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
             FROM images i
             JOIN paths p ON p.image_hash = i.image_hash
             JOIN volumes v ON v.volume_id = p.volume_id
             LEFT JOIN roots rt ON rt.root_id = p.root_id
             LEFT JOIN image_journal_stats s ON s.image_hash = i.image_hash
             LEFT JOIN image_ratings r ON r.image_hash = i.image_hash
             WHERE i.image_hash = ?1
             ORDER BY (p.state = 'active' AND v.state = 'online') DESC,
                      (p.state = 'active') DESC,
                      p.rel_path
             LIMIT 1",
        )?;
        let mut out = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let row = stmt
                .query_row(params![hash.as_str()], |r| {
                    let rel: String = r.get(0)?;
                    let root_rel: String = r.get(1)?;
                    // Strip the root prefix exactly like list_folder does.
                    let prefix_len = if root_rel.is_empty() {
                        0
                    } else {
                        root_rel.len() + 1
                    };
                    let root_relative = rel.get(prefix_len..).unwrap_or("").to_owned();
                    let file_name = root_relative
                        .rsplit('/')
                        .next()
                        .unwrap_or(&root_relative)
                        .to_owned();
                    Ok(FolderImage {
                        hash: hash.clone(),
                        file_name,
                        rel_path: root_relative,
                        root_id: r.get(2)?,
                        capture_ts: r.get(3)?,
                        first_ingested_at: r.get(4)?,
                        has_journal: r.get::<_, i64>(5)? != 0,
                        rating: r.get::<_, Option<i64>>(6)?.map(|v| v as u8),
                        offline: r.get::<_, i64>(7)? != 0,
                        preview_ready: r.get::<_, i64>(8)? != 0,
                    })
                })
                .optional()?;
            if let Some(item) = row {
                out.push(item);
            }
        }
        Ok(out)
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
        match fs_type {
            Some(t) if volumes::COARSE_MTIME_FS.contains(&t.to_ascii_lowercase().as_str()) => {
                COARSE_MTIME_TOLERANCE_NS
            }
            _ => 0,
        }
    }
}

/// §7.3: FAT-family mtime stamps are 2 s granular, so equality checks on
/// those volumes get a 2 s tolerance. Deliberately distinct from scan.rs's
/// `CLOCK_SHIFT_TOLERANCE_NS` — same magnitude, different rule; the two
/// must be free to diverge.
const COARSE_MTIME_TOLERANCE_NS: i64 = 2 * 1_000_000_000;

fn folder_volume_prefix(root_rel_path: &str, folder: &str) -> String {
    let mut prefix = String::new();
    if !root_rel_path.is_empty() {
        prefix.push_str(root_rel_path);
        prefix.push('/');
    }
    if !folder.is_empty() {
        prefix.push_str(folder);
        prefix.push('/');
    }
    prefix
}

fn folder_image_row(
    row: &rusqlite::Row<'_>,
    root_id: &str,
    root_prefix_len: usize,
) -> rusqlite::Result<FolderImage> {
    let rel: String = row.get(0)?;
    let hash: String = row.get(1)?;
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
        root_id: Some(root_id.to_owned()),
        capture_ts: row.get(2)?,
        first_ingested_at: row.get(3)?,
        has_journal: row.get::<_, i64>(4)? != 0,
        rating: row.get::<_, Option<i64>>(5)?.map(|value| value as u8),
        offline: row.get::<_, i64>(6)? != 0,
        preview_ready: row.get::<_, i64>(7)? != 0,
    })
}

const FOLDER_IMAGE_SELECT: &str = "
    SELECT p.rel_path, p.image_hash, i.capture_ts, i.first_ingested_at,
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
    LEFT JOIN image_ratings r ON r.image_hash = p.image_hash";

fn list_folder_on(
    conn: &Connection,
    root_id: &str,
    folder: &str,
    root_rel_path: &str,
) -> Result<Vec<FolderImage>, LibraryError> {
    let prefix = folder_volume_prefix(root_rel_path, folder);
    let root_prefix_len = if root_rel_path.is_empty() {
        0
    } else {
        root_rel_path.len() + 1
    };
    // has_journal: the dulled-red dot is evidence of annotations (words OR
    // marks). A rating-only journal deliberately does not light the dot.
    let sql = format!(
        "{FOLDER_IMAGE_SELECT}
         WHERE p.root_id = ?1 AND p.state = 'active'
           AND substr(p.rel_path, 1, length(?2)) = ?2
           AND instr(substr(p.rel_path, length(?2) + 1), '/') = 0
         ORDER BY p.rel_path"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![root_id, prefix], |row| {
        folder_image_row(row, root_id, root_prefix_len)
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn list_changed_folder_images_on(
    conn: &Connection,
    root_id: &str,
    folder: &str,
    root_rel_path: &str,
    changed_hashes: &[String],
) -> Result<Vec<FolderImage>, LibraryError> {
    if changed_hashes.is_empty() {
        return Ok(Vec::new());
    }
    const HASHES_PER_QUERY: usize = 400;
    let prefix = folder_volume_prefix(root_rel_path, folder);
    let root_prefix_len = if root_rel_path.is_empty() {
        0
    } else {
        root_rel_path.len() + 1
    };
    let mut images = Vec::new();
    for chunk in changed_hashes.chunks(HASHES_PER_QUERY) {
        let placeholders = (3..chunk.len() + 3)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "{FOLDER_IMAGE_SELECT}
             WHERE p.root_id = ?1 AND p.state = 'active'
               AND substr(p.rel_path, 1, length(?2)) = ?2
               AND instr(substr(p.rel_path, length(?2) + 1), '/') = 0
               AND p.image_hash IN ({placeholders})
             ORDER BY p.rel_path"
        );
        let mut values = Vec::with_capacity(chunk.len() + 2);
        values.push(rusqlite::types::Value::Text(root_id.to_owned()));
        values.push(rusqlite::types::Value::Text(prefix.clone()));
        values.extend(chunk.iter().cloned().map(rusqlite::types::Value::Text));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
            folder_image_row(row, root_id, root_prefix_len)
        })?;
        images.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    images.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    Ok(images)
}

/// A folder snapshot or a revisioned set of changes since a prior snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDelta {
    pub from_revision: u64,
    pub to_revision: u64,
    /// Full-snapshot fallback. When true, replace the folder with `upserts`;
    /// `removed_hashes` is empty.
    pub reset: bool,
    pub upserts: Vec<FolderImage>,
    pub removed_hashes: Vec<ContentHash>,
}

/// One grid row of [`Library::list_folder`] (UI §3.5 badge data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderImage {
    pub hash: ContentHash,
    pub file_name: String,
    /// Root-relative path of the file.
    pub rel_path: String,
    /// Root the representative path sits under (`None` only for a path
    /// row without a root). Carried because rel_path alone is ambiguous
    /// across roots: a COLLECTION grid mixes roots (B71), and two roots
    /// can both hold DCIM/100CANON/IMG_0001.* — unrelated photographs the
    /// frontend's stack pairing must never collapse into one cell.
    pub root_id: Option<String>,
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

/// Whether the orphan-retention portion of [`Library::doctor_with_retention`]
/// only reports candidates or also reclaims their derived data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanRetentionMode {
    ReportOnly,
    Reclaim,
}

#[derive(Debug, Default)]
struct OrphanCohorts {
    stale_path_rows: usize,
    images: usize,
    eligible: Vec<String>,
    recent: usize,
    unknown_timestamp: usize,
    busy: usize,
}

#[derive(Debug, Default)]
struct ReclaimedOrphans {
    images: usize,
    preview_rows: usize,
    preview_files: usize,
    preview_bytes: u64,
    vector_rows: usize,
    vector_spaces_compacted: usize,
    spaces: std::collections::HashSet<VecSpace>,
}

/// Classify by IMAGE, using the latest stale timestamp across all of its path
/// tombstones. Every timestamp must be canonical and at/before the cutoff:
/// partial knowledge is not authority to delete.
fn classify_orphans(conn: &Connection, cutoff: UtcMillis) -> Result<OrphanCohorts, LibraryError> {
    let mut by_image: std::collections::BTreeMap<String, (Vec<Option<String>>, bool)> =
        std::collections::BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT s.image_hash, s.stale_since,
                EXISTS(SELECT 1 FROM ingest_passes ip
                       WHERE ip.image_hash = s.image_hash AND ip.state = 'running')
         FROM paths s
         WHERE s.state = 'stale'
           AND NOT EXISTS (
             SELECT 1 FROM paths a
             WHERE a.image_hash = s.image_hash AND a.state = 'active')
         ORDER BY s.image_hash, s.path_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)? != 0,
        ))
    })?;
    for row in rows {
        let (image_hash, stale_since, busy) = row?;
        let entry = by_image.entry(image_hash).or_default();
        entry.0.push(stale_since);
        entry.1 |= busy;
    }

    let mut result = OrphanCohorts {
        stale_path_rows: by_image
            .values()
            .map(|(timestamps, _)| timestamps.len())
            .sum(),
        images: by_image.len(),
        ..OrphanCohorts::default()
    };
    for (image_hash, (timestamps, busy)) in by_image {
        let parsed: Option<Vec<UtcMillis>> = timestamps
            .iter()
            .map(|ts| ts.as_deref().and_then(|s| UtcMillis::parse(s).ok()))
            .collect();
        let Some(parsed) = parsed else {
            result.unknown_timestamp += 1;
            continue;
        };
        if busy {
            result.busy += 1;
            continue;
        }
        if parsed.iter().copied().max().is_some_and(|ts| ts <= cutoff) {
            result.eligible.push(image_hash);
        } else {
            result.recent += 1;
        }
    }
    Ok(result)
}

fn retention_vec_kind(kind: &str) -> Option<VecKind> {
    match kind {
        "annotation_chunk" => Some(VecKind::AnnotationChunk),
        "image_summary" => Some(VecKind::ImageSummary),
        "image_clip" => Some(VecKind::ImageClip),
        _ => None,
    }
}

#[derive(Debug, Default)]
pub struct QueueOptions {
    pub cancel: Option<CancelFlag>,
    /// Optional second cancellation authority. Desktop derived lanes use this
    /// to combine a workload-specific preemption signal (for example capture
    /// becoming live) with process shutdown; either flag stops at the same
    /// per-item boundary.
    pub additional_cancel: Option<CancelFlag>,
    pub max_items: Option<usize>,
    /// Optional per-drain worker ceiling. The desktop resource governor uses
    /// this to turn Eco/Balanced/Max into a decoded-frame/RAM bound without
    /// rebuilding the process-global rayon pool.
    pub max_concurrency: Option<usize>,
    /// Root ids whose images should retain pending embedding rows. Used by the
    /// desktop's durable preview-only/process-later source policy.
    pub excluded_embedding_root_ids: Vec<String>,
}

impl QueueOptions {
    pub(crate) fn is_cancelled(&self) -> bool {
        [&self.cancel, &self.additional_cancel]
            .into_iter()
            .flatten()
            .any(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// What `Library::doctor` found and did (BACKLOG "Library doctor"): the
/// debug panel renders it, the 6-hour tick `info!`s it when nonzero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DoctorReport {
    /// The caller requested shutdown between durable repair units. Work
    /// already committed remains valid; unvisited derived state is checked by
    /// the next startup/maintenance pass.
    pub cancelled: bool,
    /// `done` preview passes re-pended because an artifact file vanished.
    pub repended: usize,
    /// Stale path rows whose image has no surviving active path. Path
    /// tombstones are counted and retained even when derived data is reclaimed.
    pub stale_orphans: usize,
    /// Distinct images represented by `stale_orphans`.
    pub orphan_images: usize,
    /// Images beyond the 30-day boundary with complete authoritative
    /// timestamps and no running ingest pass.
    pub retention_eligible: usize,
    /// Orphan images still inside the retention window.
    pub retention_deferred_recent: usize,
    /// Orphan images with at least one absent or malformed `stale_since`.
    pub retention_deferred_unknown_timestamp: usize,
    /// Old orphan images protected because derived work is currently running.
    pub retention_deferred_busy: usize,
    /// True when eligible data was reported but not reclaimed.
    pub retention_dry_run: bool,
    /// Eligible orphan images whose derived state was retired this run.
    pub reclaimed_images: usize,
    pub preview_rows_reclaimed: usize,
    pub preview_files_reclaimed: usize,
    /// Sum of authoritative `preview_artifacts.bytes` for reclaimed rows.
    pub preview_bytes_reclaimed: u64,
    pub vector_rows_reclaimed: usize,
    pub vector_spaces_compacted: usize,
    /// Legacy observability counter for eligible live image-summary vectors
    /// left after reclamation. This should be zero now that retained summary
    /// text lets the relink-repended text pass rebuild those vectors.
    pub journal_vector_rows_retained: usize,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IngestErrorSummary {
    pub pass: String,
    pub category: String,
    pub format: String,
    pub raw_subtype: Option<String>,
    pub count: u64,
}

fn ingest_error_category(error: Option<&str>) -> &'static str {
    let error = error.unwrap_or_default().to_ascii_lowercase();
    if error.starts_with("io:") || error.starts_with("volume-offline") {
        "io"
    } else if error.starts_with("decode:") {
        "decode"
    } else if error.starts_with("embedder:") {
        "embedder"
    } else if error.starts_with("vector-store:") {
        "vector-store"
    } else if error == "missing-image-row" {
        "missing-image"
    } else if error == "no-worker" {
        "no-worker"
    } else if error == "decode-panic" {
        "decode-panic"
    } else if error.contains("geometry disagreement") {
        "geometry"
    } else {
        "other"
    }
}

fn metric_token(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if !normalized.is_empty()
        && normalized.len() <= 32
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        normalized
    } else {
        "other".to_owned()
    }
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
/// also bounds transient decode memory (a wave holds up to N full-size
/// decoded frames). Width is env-overridable (`ingest_pool_size`).
fn worker_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(ingest_pool_size())
            .thread_name(|i| format!("pp-ingest-{i}"))
            .build()
            .expect("ingest worker pool")
    })
}

/// The ingest decode/resize/encode pool width, with an env override.
///
/// WHY a knob now: the default `min(cores, 8)` cap was benched on the M1
/// (`preview.rs` "more workers thrash the cache"), back when CLIP image
/// embedding ran on CPU and decode was NOT the ceiling. The GPU embed (54x
/// on the 5080, 8.77x on the M1 CoreML) moved the bottleneck to decode, so on
/// a wide desktop (12c/24t) feeding a 54x GPU the `8` cap likely STARVES the
/// GPU. `PHOTOPROOF_INGEST_WORKERS` lets that machine widen the pool and
/// re-bench WITHOUT a recompile (see the `#[ignore]` `bench_ingest_pool_width`
/// harness in the tests below). It is an env knob, not a config field, for the
/// same reason the CoreML / force-CPU spike knobs are (`ort_embedder.rs`): a
/// measurement toggle the operator and the bench can flip with zero API surface
/// and zero default change; if a desktop value wins, it graduates to a real
/// config field alongside the CoreML one (BACKLOG: "graduate the env knob to a
/// config FIELD").
///
/// Default (UNSET, empty, or unparseable): the current `hash_pool_size()` —
/// byte-for-byte the same behavior as before, so NOTHING changes by default. A
/// set value is clamped to `>= 1` (0 threads would deadlock rayon's pool).
fn ingest_pool_size() -> usize {
    parse_pool_override("PHOTOPROOF_INGEST_WORKERS").unwrap_or_else(hashing::hash_pool_size)
}

/// Parse a worker-count env override: a positive integer wins; anything else
/// (unset, empty, non-numeric, or `0`) yields `None` so the caller keeps its
/// default. A free function so the default-vs-override decision is unit-tested
/// (`pool_override_parsing`) without mutating process env across the suite.
fn parse_pool_override(var: &str) -> Option<usize> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
}

/// The full-raw-decode pool (LIBRARY §10.3): a SEPARATE, NARROWER pool than
/// the M1 ingest `worker_pool()`. A full sensor develop holds the whole
/// float image in flight (a 60 MP RAW is ~720 MB as f32 RGB mid-pipe), so
/// memory — not CPU — is the cap; `max(2, cores/2)` keeps a couple of
/// decodes parallel without letting N parallel develops multiply that
/// buffer N times and thrash. `available_parallelism` reports LOGICAL cores
/// (SMT-doubled), so halving it lands near physical-core count on the common
/// SMT machines, which is the §10.3 intent.
fn decode_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let width = (logical / 2).max(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads(width)
            .thread_name(|i| format!("pp-decode-{i}"))
            .build()
            .expect("raw decode pool")
    })
}

/// Outcome taxonomy for one full-raw-decode attempt, mapping onto the queue's
/// retry policy: `Io` is transient (volume hiccup → backoff retry),
/// `Unsupported` is a clean skip (the embedded preview stands), `Permanent`
/// is a hard failure (corrupt/undevelopable file → `error` at once).
enum DecodeDevelopError {
    Io(std::io::Error),
    Unsupported(String),
    Permanent(String),
}

/// Decode a RAW with rawler and develop it to a display-oriented sRGB image.
/// A FREE function (not a method) so it is trivially `UnwindSafe` for the
/// `catch_unwind` the worker wraps it in — rawler 0.7.2 has `todo!()` panic
/// paths on unexpected formats, and a panic here must mark the row failed,
/// never unwind the decode pool thread.
fn decode_and_develop(
    abs: &Path,
    exif_orientation: u16,
) -> Result<image::DynamicImage, DecodeDevelopError> {
    let source = rawler::rawsource::RawSource::new(abs).map_err(DecodeDevelopError::Io)?;
    let decoder = rawler::get_decoder(&source)
        .map_err(|e| DecodeDevelopError::Permanent(format!("rawler: {e}")))?;
    let params = rawler::decoders::RawDecodeParams::default();
    // Full decode (dummy = false): the actual sensor mosaic, not metadata.
    let raw = decoder
        .raw_image(&source, &params, false)
        .map_err(|e| DecodeDevelopError::Permanent(format!("raw_image: {e}")))?;
    match raw_develop::develop_to_display_oriented(raw, exif_orientation) {
        Ok(img) => Ok(img),
        Err(raw_develop::DevelopError::UnsupportedCfa(r)) => {
            Err(DecodeDevelopError::Unsupported(r))
        }
        Err(raw_develop::DevelopError::Decode(m)) => Err(DecodeDevelopError::Permanent(m)),
    }
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

fn reconcile_active_roots(
    roots: Vec<RootRecord>,
    mut scan: impl FnMut(&str) -> Result<ScanReport, LibraryError>,
) -> Vec<RootReconcileResult> {
    roots
        .into_iter()
        .filter(|root| root.state == "active")
        .map(|root| {
            let outcome = match scan(&root.root_id) {
                Ok(report) => RootReconcileOutcome::Scanned(report),
                Err(LibraryError::VolumeOffline(volume_id)) => {
                    RootReconcileOutcome::Offline { volume_id }
                }
                Err(error) => RootReconcileOutcome::Failed {
                    error: error.to_string(),
                },
            };
            RootReconcileResult {
                root_id: root.root_id,
                outcome,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Connection + path helpers
// ---------------------------------------------------------------------------

/// The §5.1 connection pragmas (EVENTS.md; operational values per DECISIONS
/// P18), applied to the library's writer connection. Duplicated from the
/// store-private `schema::apply_pragmas` — flagged in the packet report.
// pub(crate): the PPVEC store (crate::retrieval) opens its metadata
// connection over the same database with the same pragmas.
pub(crate) fn open_library_connection(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    // busy_timeout shares the store's §5.1 constant: the value is
    // spec-pinned, and naming it keeps this duplicate list from silently
    // diverging on the one pragma that is normative rather than P18-tuned.
    let busy_timeout = format!(
        "PRAGMA busy_timeout = {}",
        crate::store::schema::BUSY_TIMEOUT_MS
    );
    for pragma in [
        "PRAGMA journal_mode = WAL",
        "PRAGMA synchronous = NORMAL",
        "PRAGMA secure_delete = ON",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA cache_size = -65536",
        "PRAGMA mmap_size = 268435456",
        "PRAGMA temp_store = MEMORY",
        busy_timeout.as_str(),
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
    fn ingest_error_metrics_are_coarse_and_tokens_are_bounded() {
        assert_eq!(
            ingest_error_category(Some("decode: decoder detail /private/path.raw")),
            "decode"
        );
        assert_eq!(
            ingest_error_category(Some("io: permission denied /private/path.raw")),
            "io"
        );
        assert_eq!(ingest_error_category(Some("unexpected detail")), "other");
        assert_eq!(metric_token("ARW"), "arw");
        assert_eq!(metric_token("/private/path.raw"), "other");
        assert_eq!(metric_token(&"x".repeat(33)), "other");
    }

    fn test_root(root_id: &str, state: &str) -> RootRecord {
        RootRecord {
            root_id: root_id.into(),
            volume_id: format!("volume-{root_id}"),
            rel_path: root_id.into(),
            display_name: None,
            state: state.into(),
        }
    }

    #[test]
    fn reconcile_active_roots_reports_each_failure_and_keeps_scanning() {
        let roots = vec![
            test_root("bad", "active"),
            test_root("archived", "archived"),
            test_root("offline", "active"),
            test_root("healthy", "active"),
        ];
        let mut visited = Vec::new();
        let outcomes = reconcile_active_roots(roots, |root_id| {
            visited.push(root_id.to_owned());
            match root_id {
                "bad" => Err(LibraryError::Invalid("permission denied".into())),
                "offline" => Err(LibraryError::VolumeOffline("volume-offline".into())),
                _ => Ok(ScanReport {
                    files_seen: 2,
                    ..ScanReport::default()
                }),
            }
        });

        assert_eq!(visited, ["bad", "offline", "healthy"]);
        assert_eq!(outcomes.len(), 3);
        assert!(matches!(
            &outcomes[0].outcome,
            RootReconcileOutcome::Failed { error } if error.contains("permission denied")
        ));
        assert_eq!(
            outcomes[1].outcome,
            RootReconcileOutcome::Offline {
                volume_id: "volume-offline".into()
            }
        );
        assert!(matches!(
            &outcomes[2].outcome,
            RootReconcileOutcome::Scanned(report) if report.files_seen == 2
        ));
    }

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

    #[test]
    fn pool_override_parsing_falls_back_unless_a_positive_int() {
        // A uniquely-named var keeps this test from racing any real knob; we
        // exercise every branch of the override parser (the lever-2 contract:
        // only a positive integer overrides, everything else keeps the default).
        // SAFETY: set_var/remove_var are unsafe in edition 2024 (env mutation is
        // process-global); the unique name + restore-to-unset bounds the effect
        // to this test.
        const VAR: &str = "PHOTOPROOF_INGEST_WORKERS_TEST_ONLY";
        unsafe {
            std::env::remove_var(VAR);
        }
        // Unset -> None (caller keeps hash_pool_size()).
        assert_eq!(parse_pool_override(VAR), None);
        // A positive integer (with surrounding whitespace) wins.
        unsafe {
            std::env::set_var(VAR, " 24 ");
        }
        assert_eq!(parse_pool_override(VAR), Some(24));
        // Zero is rejected (0 rayon threads would deadlock) -> default.
        unsafe {
            std::env::set_var(VAR, "0");
        }
        assert_eq!(parse_pool_override(VAR), None);
        // Non-numeric -> default.
        unsafe {
            std::env::set_var(VAR, "lots");
        }
        assert_eq!(parse_pool_override(VAR), None);
        // Empty -> default.
        unsafe {
            std::env::set_var(VAR, "");
        }
        assert_eq!(parse_pool_override(VAR), None);
        unsafe {
            std::env::remove_var(VAR);
        }
    }

    #[test]
    fn ingest_pool_size_defaults_to_hash_pool_size() {
        // With the real knob UNSET, the ingest pool width is byte-for-byte the
        // pre-knob behavior: `hash_pool_size()` (`min(cores, 8)`). This guards
        // the "zero default change" promise of lever 2. (We do not set the real
        // var here — that would perturb other tests sharing the process env.)
        if std::env::var("PHOTOPROOF_INGEST_WORKERS").is_err() {
            assert_eq!(ingest_pool_size(), hashing::hash_pool_size());
        }
    }

    /// Bench harness for re-tuning the ingest pool cap on a NEW machine (the
    /// lever-2 deliverable: the `min(cores, 8)` cap was an M1 number; a wide
    /// desktop feeding a 54x GPU may want more). `#[ignore]` so it never runs in
    /// the normal gate; run it with:
    ///
    /// ```text
    /// PP_BENCH_DECODE_DIR=/path/to/jpegs \
    ///   cargo test -p photoproof-core -- --ignored --nocapture bench_ingest_pool_width
    /// ```
    ///
    /// It sweeps a few candidate widths over the JPEGs in `PP_BENCH_DECODE_DIR`,
    /// running the SAME decode+resize a preview wave does (longest-edge 2560),
    /// and prints img/s per width so the operator can pick the knob value. It
    /// SKIPS CLEANLY (returns, no failure) when the env dir is unset/empty, so a
    /// `--ignored` run on a bare machine stays green.
    #[test]
    #[ignore = "perf bench; needs PP_BENCH_DECODE_DIR pointing at a folder of JPEGs"]
    fn bench_ingest_pool_width() {
        use std::time::Instant;

        let Ok(dir) = std::env::var("PP_BENCH_DECODE_DIR") else {
            eprintln!("skipping: set PP_BENCH_DECODE_DIR to a folder of JPEGs to bench");
            return;
        };
        let paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("read PP_BENCH_DECODE_DIR")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase),
                    Some(ref x) if x == "jpg" || x == "jpeg"
                )
            })
            .collect();
        if paths.is_empty() {
            eprintln!("skipping: no .jpg/.jpeg files in {dir}");
            return;
        }
        eprintln!("benching {} images from {dir}", paths.len());

        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        // Candidate widths bracket the current cap: the M1 default, half the
        // machine, the full machine, and 1.5x (oversubscription is sometimes a
        // win when each worker stalls on IO). Dedup so a small machine does not
        // run the same width thrice.
        let mut widths = vec![
            hashing::hash_pool_size(),
            logical / 2,
            logical,
            logical * 3 / 2,
        ];
        widths.retain(|&w| w >= 1);
        widths.sort_unstable();
        widths.dedup();

        for width in widths {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(width)
                .build()
                .expect("bench pool");
            let t0 = Instant::now();
            pool.install(|| {
                use rayon::prelude::*;
                paths.par_iter().for_each(|p| {
                    // The preview hot path: decode then longest-edge resize to
                    // the display target. Errors are ignored here (a bad file is
                    // not what we are timing); the throughput is what matters.
                    if let Ok(img) = image::open(p) {
                        let _ = preview::resize_to_edge_for_bench(&img, 2560);
                    }
                });
            });
            let secs = t0.elapsed().as_secs_f64();
            eprintln!(
                "width {width:>3}: {:.1} img/s ({:.2}s for {} imgs)",
                paths.len() as f64 / secs,
                secs,
                paths.len()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Ingest pass pipelining (BACKLOG, June 2026)
    //
    // These exercise `run_pipeline` — the bounded-channel claim→work pipeline
    // — directly with SYNTHETIC claim/work closures, so the proofs are
    // deterministic (no real decode timing, no filesystem). The closures
    // ignore the DB; `run_pipeline`'s machinery (channel bound, worker pull
    // loop, cancel wind-down, report merge) is what is under test. The
    // end-to-end DB path (real claim_next, offline/skip, no-loss across a
    // real scan) is covered by the M1Env integration suite.
    // -----------------------------------------------------------------------

    /// A throwaway Library over a temp DB — just enough `&self` for the
    /// pipeline; the synthetic claim/work closures never touch its tables.
    fn pipeline_test_library() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("photoproof.db");
        let cache = tmp.path().join("previews");
        let lib = Library::open(&db, &cache).unwrap();
        (tmp, lib)
    }

    /// A synthetic queue of N identical placeholder items the claim closure
    /// hands out one at a time (mirrors `claim_next` returning rows until the
    /// queue drains, without the DB requirements of real claiming).
    fn synthetic_items(n: usize) -> Vec<ingest::QueueItem> {
        (0..n)
            .map(|i| ingest::QueueItem {
                // Distinct hashes so a no-double-processing check can use them
                // as identity. Hex of i, left-padded to the 64-char content hash.
                image_hash: ContentHash::from_hex(&format!("{i:064x}")).unwrap(),
                pass: ingest::PassName::Preview,
                pass_version: ingest::PASS_VERSION,
                attempts: 1,
            })
            .collect()
    }

    /// Hands out items from a shared queue under a mutex (the claim closure),
    /// returning None once drained — the synthetic stand-in for `claim_next`.
    fn draining_claim(
        queue: &std::sync::Mutex<std::collections::VecDeque<ingest::QueueItem>>,
    ) -> impl Fn(&Connection, UtcMillis) -> rusqlite::Result<Option<ingest::QueueItem>> + Sync + '_
    {
        move |_conn, _now| Ok(queue.lock().expect("poisoned").pop_front())
    }

    /// (a) OVERLAP: the workers run CONCURRENTLY. With a barrier sized to the
    /// pool width, the work closure can only get past the barrier if `width`
    /// workers are inside it AT THE SAME TIME — a sequential or wave-barriered
    /// drain (next item not started until the previous finished) would
    /// deadlock here and the test would hang. A bounded timeout converts that
    /// hang into a clean failure so the proof is deterministic, not flaky.
    #[test]
    fn pipeline_runs_workers_concurrently() {
        let (_tmp, lib) = pipeline_test_library();
        let width = worker_pool().current_num_threads().max(1);
        // Need at least 2 workers to prove concurrency; if the host reports 1
        // logical core the claim is vacuous — assert the machinery still drains.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(width));
        let queue = std::sync::Mutex::new(synthetic_items(width).into());
        let max_inside = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inside = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let report = lib
            .run_pipeline(
                &QueueOptions::default(),
                worker_pool(),
                draining_claim(&queue),
                |_lib, _item, local| {
                    let n = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_inside.fetch_max(n, std::sync::atomic::Ordering::SeqCst);
                    // Rendezvous: every worker must be here at once to proceed.
                    // If the pipeline serialized, only one worker would ever be
                    // inside and this would block forever (caught by the harness
                    // timeout / the assertion below).
                    barrier.wait();
                    inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(report.processed, width);
        assert_eq!(report.done, width);
        assert_eq!(
            max_inside.load(std::sync::atomic::Ordering::SeqCst),
            width,
            "all {width} workers were inside the pass simultaneously: stages overlap"
        );
    }

    #[test]
    fn pipeline_honors_per_drain_concurrency_ceiling() {
        let (_tmp, lib) = pipeline_test_library();
        let available = worker_pool().current_num_threads().max(1);
        let ceiling = available.min(2);
        let queue = std::sync::Mutex::new(synthetic_items(16).into());
        let inside = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_inside = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opts = QueueOptions {
            max_concurrency: Some(ceiling),
            ..QueueOptions::default()
        };
        let report = lib
            .run_pipeline(
                &opts,
                worker_pool(),
                draining_claim(&queue),
                |_lib, _item, local| {
                    let count = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_inside.fetch_max(count, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(report.processed, 16);
        assert!(
            max_inside.load(std::sync::atomic::Ordering::SeqCst) <= ceiling,
            "configured ceiling must bound simultaneous decoded work"
        );
    }

    #[test]
    fn pipeline_single_worker_ceiling_drains_without_deadlock() {
        let (_tmp, lib) = pipeline_test_library();
        let queue = std::sync::Mutex::new(synthetic_items(16).into());
        let opts = QueueOptions {
            max_concurrency: Some(1),
            ..QueueOptions::default()
        };
        let report = lib
            .run_pipeline(
                &opts,
                worker_pool(),
                draining_claim(&queue),
                |_lib, _item, local| {
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(report.processed, 16);
        assert_eq!(report.done, 16);
    }

    /// (b) NO LOSS / NO DOUBLE-PROCESSING: every claimed item is handled
    /// exactly once. We feed many more items than workers (so the bounded
    /// channel backpressures repeatedly) and assert the multiset of processed
    /// hashes equals the input exactly — none dropped, none seen twice.
    #[test]
    fn pipeline_processes_every_item_exactly_once() {
        let (_tmp, lib) = pipeline_test_library();
        const N: usize = 500; // >> pool width: forces the channel to fill/drain many times
        let queue = std::sync::Mutex::new(synthetic_items(N).into());
        let seen = std::sync::Mutex::new(std::collections::HashMap::<String, usize>::new());

        let report = lib
            .run_pipeline(
                &QueueOptions::default(),
                worker_pool(),
                draining_claim(&queue),
                |_lib, item, local| {
                    *seen
                        .lock()
                        .expect("poisoned")
                        .entry(item.image_hash.as_str().to_owned())
                        .or_default() += 1;
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(report.processed, N);
        assert_eq!(report.done, N);
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), N, "every distinct item processed");
        assert!(
            seen.values().all(|&c| c == 1),
            "no item processed more than once"
        );
    }

    /// (c) CANCEL MID-PIPELINE winds down cleanly: once the cancel flag trips
    /// the claimer stops handing out NEW items, but items already claimed are
    /// finished (so no row would be left `running`). We assert fewer than the
    /// full queue were processed and the report is flagged cancelled — the
    /// claimer's per-item cancel check fired and the in-flight work drained.
    #[test]
    fn pipeline_cancel_winds_down_without_stuck_work() {
        let (_tmp, lib) = pipeline_test_library();
        const N: usize = 1_000;
        let queue = std::sync::Mutex::new(synthetic_items(N).into());
        let cancel: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let primary: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let processed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel_at = 10usize;

        let opts = QueueOptions {
            cancel: Some(primary),
            additional_cancel: Some(cancel.clone()),
            max_items: None,
            max_concurrency: None,
            excluded_embedding_root_ids: Vec::new(),
        };
        let report = lib
            .run_pipeline(
                &opts,
                worker_pool(),
                draining_claim(&queue),
                |_lib, _item, local| {
                    let n = processed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    // Trip the second authority mid-drain; the claimer must
                    // treat it exactly like the primary and wind down.
                    if n >= cancel_at {
                        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap();

        assert!(report.cancelled, "cancel flag must surface in the report");
        assert!(
            report.processed < N,
            "cancel stopped the drain early ({} of {N})",
            report.processed
        );
        // Every item the workers TOOK was finished (done == processed): nothing
        // is left half-handled that would be a row stuck `running` in the real
        // DB path.
        assert_eq!(
            report.done, report.processed,
            "every in-flight item finished on cancel wind-down"
        );
    }

    /// (d) A claim-time DB error is the drain-level abort: it propagates after
    /// the in-flight items finish, exactly like the old wave loop. (The
    /// per-item pass failures stay RECORDED, not returned — covered by the
    /// integration suite; this guards the plumbing-error path of the new loop.)
    #[test]
    fn pipeline_claim_error_aborts_after_inflight_finish() {
        let (_tmp, lib) = pipeline_test_library();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let err = lib
            .run_pipeline(
                &QueueOptions::default(),
                worker_pool(),
                |_conn, _now| {
                    // First claim yields one item; second claim errors.
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n == 0 {
                        Ok(Some(ingest::QueueItem {
                            image_hash: ContentHash::from_hex(&"aa".repeat(32)).unwrap(),
                            pass: ingest::PassName::Exif,
                            pass_version: ingest::PASS_VERSION,
                            attempts: 1,
                        }))
                    } else {
                        Err(rusqlite::Error::InvalidQuery)
                    }
                },
                |_lib, _item, local| {
                    finished.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    local.done += 1;
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(
            matches!(err, LibraryError::Sqlite(_)),
            "claim DB error propagates as the drain result"
        );
        assert_eq!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the one item claimed before the error still finished (no stuck work)"
        );
    }

    /// Seam 1 (ARCHITECTURE-CONTRACTS.md): a committed NEW-image insert advances
    /// `images_version` so the grid re-lists on the data-version handshake. A
    /// duplicate insert (same hash, ON CONFLICT DO NOTHING) still rides the same
    /// `new_image_tx` chokepoint, so the version is monotone over calls — the
    /// view only ever sees it go up; the coarse counter is intentionally
    /// per-call, not per-row.
    #[test]
    fn images_version_advances_on_new_image() {
        let (_tmp, lib) = pipeline_test_library();
        assert_eq!(lib.images_version(), 0, "fresh library starts at 0");

        let h1 = ContentHash::from_hex(&format!("{:064x}", 1)).unwrap();
        lib.new_image_tx(&h1, 1234, "vol1", None, "a/IMG_1.jpg", 1_000, 0)
            .unwrap();
        let after_first = lib.images_version();
        assert!(after_first > 0, "a new image advanced the version");

        let h2 = ContentHash::from_hex(&format!("{:064x}", 2)).unwrap();
        lib.new_image_tx(&h2, 5678, "vol1", None, "a/IMG_2.jpg", 2_000, 0)
            .unwrap();
        assert!(
            lib.images_version() > after_first,
            "a second new image advanced the version again (monotone)"
        );
    }

    /// Seed one committed image and return (lib, hash, tempdir) — the shared
    /// rig for the Seam-1 version-bump pins below (AUDIT-2026-07-07 S3/S4).
    fn seeded_library() -> (tempfile::TempDir, Library, ContentHash) {
        let (tmp, lib) = pipeline_test_library();
        let h1 = ContentHash::from_hex(&format!("{:064x}", 1)).unwrap();
        lib.new_image_tx(&h1, 1234, "vol1", None, "a/IMG_1.jpg", 1_000, 0)
            .unwrap();
        (tmp, lib, h1)
    }

    /// AUDIT-2026-07-07 S3: an in-place edit (supersede) changes WHICH image
    /// lives at a path, so it must advance `images_version` — the bump lived
    /// only in `new_image_tx`, leaving the grid stale on in-place edits until
    /// an unrelated event. This pins the committed supersede path.
    #[test]
    fn images_version_advances_on_supersede() {
        let (_tmp, lib, _h1) = seeded_library();
        let before = lib.images_version();
        let path_id: String = {
            let conn = lib.db.lock().expect("poisoned");
            conn.query_row(
                "SELECT path_id FROM paths
                 WHERE volume_id = 'vol1' AND rel_path = 'a/IMG_1.jpg' AND state = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let h2 = ContentHash::from_hex(&format!("{:064x}", 2)).unwrap();
        lib.supersede_tx(&path_id, &h2, 4321, 2_000, "vol1", None, "a/IMG_1.jpg", 0)
            .unwrap();
        assert!(
            lib.images_version() > before,
            "a committed supersede must advance images_version (S3)"
        );
    }

    /// AUDIT-2026-07-07 S3: a relink (live move / copy of a known hash) adds
    /// an active path for an existing image — the grid's slice changed, so
    /// the version must advance exactly like a new image.
    #[test]
    fn images_version_advances_on_relink() {
        let (_tmp, lib, h1) = seeded_library();
        let before = lib.images_version();
        lib.relink_tx(
            &h1,
            "vol1",
            None,
            "b/COPY_OF_IMG_1.jpg",
            1234,
            3_000,
            UtcMillis::from_epoch_ms(0),
        )
        .unwrap();
        assert!(
            lib.images_version() > before,
            "a committed relink must advance images_version (S3)"
        );
    }

    /// AUDIT-2026-07-07 S4: a live delete stales the row but enqueues no pass,
    /// so `images_version` is the ONLY signal the pump's emit-gate has — no
    /// bump meant ghost thumbnails until the next folder revisit. Also pins
    /// the negative: removing a path the index never knew changes nothing the
    /// grid shows, so it must NOT bump (no spurious re-lists).
    #[test]
    fn images_version_advances_on_observe_removed() {
        let (_tmp, lib, _h1) = seeded_library();
        let before = lib.images_version();

        let window = UtcMillis::from_epoch_ms(0);
        assert!(
            !lib.observe_removed("vol1", "b/NEVER_SEEN.jpg", window)
                .unwrap(),
            "unknown path: nothing removed"
        );
        assert_eq!(
            lib.images_version(),
            before,
            "a no-op remove must not force a grid re-list"
        );

        assert!(
            lib.observe_removed("vol1", "a/IMG_1.jpg", window).unwrap(),
            "the active row was removed"
        );
        assert!(
            lib.images_version() > before,
            "a committed live remove must advance images_version (S4)"
        );
    }
}
