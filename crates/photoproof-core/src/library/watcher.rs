//! Live filesystem watcher (§7.1): per-path 500 ms debounce, 2 s stability
//! re-checks for mid-copy files, paired renames as direct relinks, move
//! correlation, and overflow/error degradation to polled mode.
//!
//! Architecture: `DebounceCore` is a pure state machine over injected
//! timestamps (the watcher tests drive it deterministically — no sleeps, no
//! flakiness); `WatchPipeline` binds it to a `Library` root and performs the
//! §7.2 single algorithm per stable path; `start_root_watcher` feeds the same
//! pipeline from a real `notify` watcher with the wall clock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use notify::Watcher as _;

use crate::id::UtcMillis;

use super::exclusions::{MAX_FILE_BYTES, classify_extension, is_excluded_file_name};
use super::ingest::PRIORITY_WATCHER;
use super::scan::ScanOptions;
use super::{
    ActivePathMatch, CancelFlag, Library, LibraryError, Observed, mtime_ns_of, paths, rel_path_str,
};

/// §7.1 timing knobs (DECISIONS L3). Tests inject compressed clocks; the
/// defaults are the normative values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebounceConfig {
    /// Per-path quiescence before evaluation.
    pub debounce_ms: i64,
    /// Re-check interval until size+mtime are stable across two checks.
    pub stability_ms: i64,
    /// Remove→create correlation window (§7.2 step R).
    pub move_window_ms: i64,
}

impl Default for DebounceConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            stability_ms: 2_000,
            move_window_ms: 10_000,
        }
    }
}

/// Raw watcher input, already normalized from the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawWatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
    /// A paired rename (both ends known): handled directly as a relink.
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    /// Backend overflow: events were missed; the scan is the recovery.
    Overflow,
    Error(String),
}

/// Outcome of a stability check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stability {
    /// First sighting or still changing: re-check after `stability_ms`.
    Recheck,
    /// Size+mtime stable across two checks: process now.
    Stable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathState {
    /// Burst coalescing: waiting for quiescence.
    Debouncing,
    /// A remove was the last event for this path.
    RemovePending,
    /// First stat recorded; awaiting the confirming check.
    Stability { size: i64, mtime_ns: i64 },
}

#[derive(Debug)]
struct Entry {
    state: PathState,
    due_ms: i64,
}

/// The pure per-path debounce/stability state machine. All timestamps are
/// injected epoch-milliseconds.
#[derive(Debug)]
pub struct DebounceCore {
    cfg: DebounceConfig,
    entries: HashMap<PathBuf, Entry>,
}

impl DebounceCore {
    pub fn new(cfg: DebounceConfig) -> Self {
        Self {
            cfg,
            entries: HashMap::new(),
        }
    }

    /// A create/modify event: (re)start the quiescence window. A path in the
    /// stability phase drops back to debouncing — it is still changing.
    pub fn push_changed(&mut self, path: PathBuf, now_ms: i64) {
        self.entries.insert(
            path,
            Entry {
                state: PathState::Debouncing,
                due_ms: now_ms + self.cfg.debounce_ms,
            },
        );
    }

    /// A remove event: latest event wins.
    pub fn push_removed(&mut self, path: PathBuf, now_ms: i64) {
        self.entries.insert(
            path,
            Entry {
                state: PathState::RemovePending,
                due_ms: now_ms + self.cfg.debounce_ms,
            },
        );
    }

    /// Paths whose deadline has passed, in deterministic order.
    pub fn due(&self, now_ms: i64) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(_, e)| e.due_ms <= now_ms)
            .map(|(p, _)| p.clone())
            .collect();
        v.sort();
        v
    }

    pub fn is_remove(&self, path: &Path) -> bool {
        self.entries
            .get(path)
            .is_some_and(|e| e.state == PathState::RemovePending)
    }

    /// Record a stat observation for a due path. `Stable` requires the same
    /// size+mtime across two checks ≥ `stability_ms` apart (§7.1: a file
    /// whose size is still changing is re-checked every 2 s).
    pub fn record_stat(&mut self, path: &Path, size: i64, mtime_ns: i64, now_ms: i64) -> Stability {
        match self.entries.get_mut(path) {
            Some(e) => match e.state {
                PathState::Stability {
                    size: s0,
                    mtime_ns: m0,
                } if s0 == size && m0 == mtime_ns => {
                    self.entries.remove(path);
                    Stability::Stable
                }
                _ => {
                    e.state = PathState::Stability { size, mtime_ns };
                    e.due_ms = now_ms + self.cfg.stability_ms;
                    Stability::Recheck
                }
            },
            None => {
                self.entries.insert(
                    path.to_path_buf(),
                    Entry {
                        state: PathState::Stability { size, mtime_ns },
                        due_ms: now_ms + self.cfg.stability_ms,
                    },
                );
                Stability::Recheck
            }
        }
    }

    /// Drop a path (processed, vanished, or excluded).
    pub fn complete(&mut self, path: &Path) {
        self.entries.remove(path);
    }

    /// The earliest pending deadline (drives the wait in the real loop).
    pub fn next_due_ms(&self) -> Option<i64> {
        self.entries.values().map(|e| e.due_ms).min()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What a pipeline step did (tests assert on these; the watcher thread acts
/// on `ScanNeeded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineEffect {
    Observed {
        rel_path: String,
        outcome: Observed,
    },
    Removed {
        rel_path: String,
    },
    /// A paired rename relinked directly — zero hashing (§7.1).
    RenameRelinked {
        from: String,
        to: String,
    },
    /// Overflow/error: run a §7.3 scan of this root now.
    ScanNeeded,
    Ignored {
        rel_path: String,
    },
}

/// The §7.1 state machine bound to one root. Deterministic: every entry
/// point takes the clock as an argument.
pub struct WatchPipeline {
    lib: Arc<Library>,
    root_id: String,
    volume_id: String,
    mount_point: PathBuf,
    root_dir: PathBuf,
    /// Symlink-resolved twins of `mount_point`/`root_dir`. FSEvents
    /// (macOS) reports RESOLVED event paths — a `/var/…` root's events
    /// arrive as `/private/var/…` — while inotify reports the registered
    /// form, so containment and rel-path math accept EITHER prefix
    /// (founder-machine find, June 2026: every event on a symlinked root
    /// was silently dropped).
    mount_point_canonical: PathBuf,
    root_dir_canonical: PathBuf,
    tolerance_ns: i64,
    cfg: DebounceConfig,
    core: DebounceCore,
    polled_mode: bool,
}

impl WatchPipeline {
    pub(crate) fn new(
        lib: Arc<Library>,
        root_id: &str,
        cfg: DebounceConfig,
    ) -> Result<Self, LibraryError> {
        let (_root, volume, dir) = lib.root_location(root_id)?;
        let mount_point = PathBuf::from(volume.mount_point.clone().expect("online volume"));
        let tolerance_ns = lib.mtime_tolerance_ns(volume.fs_type.as_deref());
        // Resolution failure (e.g. the volume going offline mid-setup)
        // falls back to the unresolved form: the twin checks degrade to
        // the old single-prefix behavior instead of erroring the watcher.
        let mount_point_canonical = mount_point
            .canonicalize()
            .unwrap_or_else(|_| mount_point.clone());
        let root_dir_canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        Ok(Self {
            volume_id: volume.volume_id.clone(),
            root_id: root_id.to_owned(),
            mount_point,
            root_dir: dir,
            mount_point_canonical,
            root_dir_canonical,
            tolerance_ns,
            cfg,
            core: DebounceCore::new(cfg),
            polled_mode: false,
            lib,
        })
    }

    /// Did overflow / an unsupported backend degrade this root to polled
    /// mode (a §7.3 scan every 10 minutes)?
    pub fn polled_mode(&self) -> bool {
        self.polled_mode
    }

    pub fn pending_paths(&self) -> usize {
        self.core.len()
    }

    /// Containment in BOTH spellings (see the `*_canonical` field note).
    fn in_root(&self, p: &Path) -> bool {
        p.starts_with(&self.root_dir) || p.starts_with(&self.root_dir_canonical)
    }

    fn rel_of(&self, path: &Path) -> Option<String> {
        rel_path_str(&self.mount_point, path)
            .or_else(|| rel_path_str(&self.mount_point_canonical, path))
    }

    pub fn push(
        &mut self,
        event: RawWatchEvent,
        now: UtcMillis,
    ) -> Result<Vec<PipelineEffect>, LibraryError> {
        let now_ms = now.epoch_ms();
        match event {
            RawWatchEvent::Created(p) | RawWatchEvent::Modified(p) => {
                if !self.in_root(&p) {
                    return Ok(vec![]);
                }
                // A directory moved/created inside the root may arrive as a
                // single event: enqueue its files (backends do not replay
                // children).
                if p.is_dir() {
                    self.enqueue_dir(&p, now_ms);
                } else {
                    self.core.push_changed(p, now_ms);
                }
                Ok(vec![])
            }
            RawWatchEvent::Removed(p) => {
                if !self.in_root(&p) {
                    return Ok(vec![]);
                }
                self.core.push_removed(p, now_ms);
                Ok(vec![])
            }
            RawWatchEvent::Renamed { from, to } => self.handle_rename(from, to, now),
            RawWatchEvent::Overflow | RawWatchEvent::Error(_) => {
                // Events were missed; the scan is the recovery (§7.1), and
                // the root additionally degrades to polled mode.
                if let RawWatchEvent::Error(e) = &event {
                    self.lib
                        .log(format!("watcher error on root {}: {e}", self.root_id));
                } else {
                    self.lib
                        .log(format!("watcher overflow on root {}", self.root_id));
                }
                self.polled_mode = true;
                Ok(vec![PipelineEffect::ScanNeeded])
            }
        }
    }

    fn enqueue_dir(&mut self, dir: &Path, now_ms: i64) {
        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_file() {
                self.core.push_changed(entry.into_path(), now_ms);
            }
        }
    }

    /// Paired rename → direct relink (§7.1): zero hashing when the target
    /// stats clean against the old row; anything off falls through to the
    /// §7.2 remove/create path, which handles moves identically.
    fn handle_rename(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        now: UtcMillis,
    ) -> Result<Vec<PipelineEffect>, LibraryError> {
        let now_ms = now.epoch_ms();
        let fallback = |this: &mut Self| {
            if this.in_root(&from) {
                this.core.push_removed(from.clone(), now_ms);
            }
            if this.in_root(&to) {
                this.core.push_changed(to.clone(), now_ms);
            }
        };
        if to.is_dir() {
            // Directory rename: every child path row needs a relink; the
            // remove side surfaces through the per-file correlation, so a
            // scan handles it wholesale.
            if self.in_root(&from) || self.in_root(&to) {
                return Ok(vec![PipelineEffect::ScanNeeded]);
            }
            return Ok(vec![]);
        }
        let (Some(from_rel), Some(to_rel)) = (
            self.in_root(&from).then(|| self.rel_of(&from)).flatten(),
            self.in_root(&to).then(|| self.rel_of(&to)).flatten(),
        ) else {
            fallback(self);
            return Ok(vec![]);
        };
        // Renamed into an excluded name (e.g. a dotfile): it is a remove.
        let to_excluded = to
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| is_excluded_file_name(n) || classify_extension(n).is_none());
        if to_excluded {
            self.core.push_removed(from, now_ms);
            return Ok(vec![]);
        }
        let Ok(meta) = std::fs::metadata(&to) else {
            fallback(self);
            return Ok(vec![]);
        };
        // For a case-only pair, ask whether the old DB spelling aliases the
        // post-rename path BEFORE treating `from` as an ordinary exact move.
        // That preserves path_id on APFS/FAT. The same proof fails when
        // `a.jpg` and `A.jpg` are distinct entries, so case-sensitive volumes
        // continue through the normal stale+insert move-correlation path.
        let case_alias = if from_rel != to_rel && from_rel.eq_ignore_ascii_case(&to_rel) {
            self.lib
                .active_case_alias_match(&self.volume_id, &to_rel, &to)?
        } else {
            None
        };
        let old_row = match case_alias {
            some @ Some(_) => some,
            None => match self
                .lib
                .active_path_match(&self.volume_id, &from_rel, &to)?
            {
                some @ Some(_) => some,
                None => self.lib.active_path_match(&self.volume_id, &to_rel, &to)?,
            },
        };
        match old_row {
            Some(ActivePathMatch::CaseAlias(row)) if row.size == meta.len() as i64 => {
                let now_db = self.lib.now();
                let conn = self.lib.db.lock().expect("poisoned");
                paths::recase_active(
                    &conn,
                    &row.path_id,
                    Some(&self.root_id),
                    &to_rel,
                    row.size,
                    mtime_ns_of(&meta),
                    now_db,
                )?;
                drop(conn);
                self.lib.bump_images_version();
                Ok(vec![PipelineEffect::RenameRelinked {
                    from: from_rel,
                    to: to_rel,
                }])
            }
            Some(ActivePathMatch::Exact(row)) if row.size == meta.len() as i64 => {
                let mtime_ns = mtime_ns_of(&meta);
                let path_id = self.lib.mint_ulid();
                let now_db = self.lib.now();
                let mut conn = self.lib.db.lock().expect("poisoned");
                let tx = conn.transaction()?;
                paths::mark_stale(&tx, &row.path_id, paths::StaleReason::Moved, now_db)?;
                paths::insert_active(
                    &tx,
                    &path_id,
                    &row.image_hash,
                    &self.volume_id,
                    Some(&self.root_id),
                    &to_rel,
                    row.size,
                    mtime_ns,
                    now_db,
                )?;
                tx.commit()?;
                drop(conn);
                // Seam 1 (AUDIT-2026-07-07 S3/S4): this branch commits its own
                // relink transaction (it bypasses `relink_tx`), so it owes the
                // same post-commit version bump — a live rename changes the
                // path the grid sorts/labels by, and without the bump the grid
                // shows the old name until an unrelated event re-lists it.
                self.lib.bump_images_version();
                Ok(vec![PipelineEffect::RenameRelinked {
                    from: from_rel,
                    to: to_rel,
                }])
            }
            _ => {
                fallback(self);
                Ok(vec![])
            }
        }
    }

    /// Evaluate every quiescent path (call on a timer, or after `push` with
    /// an advanced clock in tests).
    pub fn tick(&mut self, now: UtcMillis) -> Result<Vec<PipelineEffect>, LibraryError> {
        self.tick_with_options(now, &ScanOptions::default())
    }

    fn tick_with_options(
        &mut self,
        now: UtcMillis,
        opts: &ScanOptions,
    ) -> Result<Vec<PipelineEffect>, LibraryError> {
        let now_ms = now.epoch_ms();
        let mut effects = Vec::new();
        for path in self.core.due(now_ms) {
            let Some(rel) = self.rel_of(&path) else {
                self.lib
                    .log(format!("skipped non-UTF-8 path {}", path.display()));
                self.core.complete(&path);
                continue;
            };
            if self.core.is_remove(&path) {
                self.core.complete(&path);
                if self.observe_removed(&rel, now)? {
                    effects.push(PipelineEffect::Removed { rel_path: rel });
                }
                continue;
            }
            // §7.2 step 1: stat. Missing → remove handling; excluded → ignore.
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    self.core.complete(&path);
                    if self.observe_removed(&rel, now)? {
                        effects.push(PipelineEffect::Removed { rel_path: rel });
                    }
                    continue;
                }
            };
            if meta.is_dir() || meta.file_type().is_symlink() {
                self.core.complete(&path);
                continue;
            }
            let excluded = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|n| is_excluded_file_name(n) || classify_extension(n).is_none())
                || meta.len() > MAX_FILE_BYTES
                || rel
                    .split('/')
                    .rev()
                    .skip(1)
                    .any(super::exclusions::is_excluded_dir_name);
            if excluded {
                self.core.complete(&path);
                effects.push(PipelineEffect::Ignored { rel_path: rel });
                continue;
            }
            if self.lib.placeholders.is_placeholder(&path, &meta) {
                // §5.2: never read, never hashed.
                self.lib.record_placeholder(&self.volume_id, &rel)?;
                self.core.complete(&path);
                effects.push(PipelineEffect::Ignored { rel_path: rel });
                continue;
            }
            let size = meta.len() as i64;
            let mtime_ns = mtime_ns_of(&meta);
            match self.core.record_stat(&path, size, mtime_ns, now_ms) {
                Stability::Recheck => {} // mid-copy: re-checked every 2 s
                Stability::Stable => {
                    let window = UtcMillis::from_epoch_ms(now.epoch_ms() - self.cfg.move_window_ms);
                    let Some(outcome) = self.lib.observe_file(
                        &self.volume_id,
                        Some(&self.root_id),
                        &rel,
                        &path,
                        size,
                        mtime_ns,
                        PRIORITY_WATCHER,
                        self.tolerance_ns,
                        window,
                        opts.cancel.as_deref(),
                        opts.pause.as_ref(),
                    )?
                    else {
                        continue;
                    };
                    effects.push(PipelineEffect::Observed {
                        rel_path: rel,
                        outcome,
                    });
                }
            }
        }
        Ok(effects)
    }

    fn observe_removed(&self, rel: &str, now: UtcMillis) -> Result<bool, LibraryError> {
        let window = UtcMillis::from_epoch_ms(now.epoch_ms() - self.cfg.move_window_ms);
        self.lib.observe_removed(&self.volume_id, rel, window)
    }
}

// ---------------------------------------------------------------------------
// notify-backed live watcher
// ---------------------------------------------------------------------------

/// Polled-mode rescan interval (§7.1).
const POLLED_SCAN_INTERVAL: Duration = Duration::from_secs(600);

/// recv_timeout tick of the pp-watcher thread loop. One value bounds
/// three latencies: stop-flag responsiveness (drop/stop joins within a
/// tick), debounce due-time firing, and idle wakeup cost. Must stay well
/// under the 500 ms `debounce_ms` so due times fire near-on-time, while
/// staying long enough that an idle watcher barely wakes.
const WATCHER_TICK: Duration = Duration::from_millis(100);

/// Handle for a running root watcher. Dropping it stops the watcher.
pub struct RootWatcherHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl RootWatcherHandle {
    pub fn is_active(&self) -> bool {
        self.join.as_ref().is_some_and(|join| !join.is_finished())
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self._watcher.take(); // drop the backend; its channel sender closes
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RootWatcherHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn map_notify(res: notify::Result<notify::Event>) -> Vec<RawWatchEvent> {
    use notify::EventKind as K;
    use notify::event::{ModifyKind, RenameMode};
    match res {
        Err(e) => vec![RawWatchEvent::Error(e.to_string())],
        Ok(ev) => {
            if ev.need_rescan() {
                return vec![RawWatchEvent::Overflow];
            }
            match ev.kind {
                K::Create(_) => ev.paths.into_iter().map(RawWatchEvent::Created).collect(),
                K::Remove(_) => ev.paths.into_iter().map(RawWatchEvent::Removed).collect(),
                K::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                    let mut it = ev.paths.into_iter();
                    vec![RawWatchEvent::Renamed {
                        from: it.next().expect("two paths"),
                        to: it.next().expect("two paths"),
                    }]
                }
                K::Modify(ModifyKind::Name(RenameMode::From)) => {
                    ev.paths.into_iter().map(RawWatchEvent::Removed).collect()
                }
                K::Modify(ModifyKind::Name(RenameMode::To)) => {
                    ev.paths.into_iter().map(RawWatchEvent::Created).collect()
                }
                K::Access(_) => vec![],
                _ => ev.paths.into_iter().map(RawWatchEvent::Modified).collect(),
            }
        }
    }
}

pub(crate) fn start_root_watcher(
    lib: &Arc<Library>,
    root_id: &str,
) -> Result<RootWatcherHandle, LibraryError> {
    start_root_watcher_with_options(lib, root_id, |stop| {
        Some((
            ScanOptions {
                cancel: Some(Arc::clone(stop)),
                ..ScanOptions::default()
            },
            (),
        ))
    })
}

pub(crate) fn start_root_watcher_with_options<F, G>(
    lib: &Arc<Library>,
    root_id: &str,
    policy: F,
) -> Result<RootWatcherHandle, LibraryError>
where
    F: Fn(&CancelFlag) -> Option<(ScanOptions, G)> + Send + Sync + 'static,
    G: Send + 'static,
{
    let mut pipeline = WatchPipeline::new(Arc::clone(lib), root_id, DebounceConfig::default())?;
    let root_dir = pipeline.root_dir.clone();
    let (tx, rx) = mpsc::channel::<RawWatchEvent>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        for ev in map_notify(res) {
            let _ = tx.send(ev);
        }
    })
    .map_err(|e| LibraryError::Watch(e.to_string()))?;
    watcher
        .watch(&root_dir, notify::RecursiveMode::Recursive)
        .map_err(|e| LibraryError::Watch(e.to_string()))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let lib_thread = Arc::clone(lib);
    let root = root_id.to_owned();
    let join = std::thread::Builder::new()
        .name("pp-watcher".into())
        .spawn(move || {
            let mut last_polled_scan = std::time::Instant::now();
            let run_scan = |reason: &str, opts: &ScanOptions| {
                if let Err(e) = lib_thread.scan_root(&root, opts) {
                    lib_thread.log(format!("watcher-triggered scan ({reason}) failed: {e}"));
                }
            };
            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    break;
                }
                let mut scan_needed = false;
                match rx.recv_timeout(WATCHER_TICK) {
                    Ok(ev) => {
                        let Some((opts, _permit)) = policy(&stop_thread) else {
                            if stop_thread.load(Ordering::Relaxed) {
                                break;
                            }
                            continue;
                        };
                        match pipeline.push(ev, UtcMillis::now()) {
                            Ok(effects) => {
                                scan_needed |= effects.contains(&PipelineEffect::ScanNeeded);
                            }
                            Err(e) => lib_thread.log(format!("watcher push failed: {e}")),
                        }
                        // Drain the burst before ticking.
                        while let Ok(ev) = rx.try_recv() {
                            match pipeline.push(ev, UtcMillis::now()) {
                                Ok(effects) => {
                                    scan_needed |= effects.contains(&PipelineEffect::ScanNeeded);
                                }
                                Err(e) => lib_thread.log(format!("watcher push failed: {e}")),
                            }
                        }
                        match pipeline.tick_with_options(UtcMillis::now(), &opts) {
                            Ok(effects) => {
                                scan_needed |= effects.contains(&PipelineEffect::ScanNeeded);
                            }
                            Err(e) => lib_thread.log(format!("watcher tick failed: {e}")),
                        }
                        if scan_needed {
                            run_scan("overflow/error", &opts);
                            last_polled_scan = std::time::Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if pipeline.pending_paths() > 0 {
                            let Some((opts, _permit)) = policy(&stop_thread) else {
                                if stop_thread.load(Ordering::Relaxed) {
                                    break;
                                }
                                continue;
                            };
                            match pipeline.tick_with_options(UtcMillis::now(), &opts) {
                                Ok(effects) => {
                                    scan_needed |= effects.contains(&PipelineEffect::ScanNeeded);
                                }
                                Err(e) => lib_thread.log(format!("watcher tick failed: {e}")),
                            }
                            if scan_needed {
                                run_scan("overflow/error", &opts);
                                last_polled_scan = std::time::Instant::now();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if pipeline.polled_mode() && last_polled_scan.elapsed() >= POLLED_SCAN_INTERVAL {
                    let Some((opts, _permit)) = policy(&stop_thread) else {
                        if stop_thread.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    };
                    run_scan("polled mode", &opts);
                    last_polled_scan = std::time::Instant::now();
                }
            }
        })
        .expect("spawn watcher thread");
    Ok(RootWatcherHandle {
        stop,
        join: Some(join),
        _watcher: Some(watcher),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: DebounceConfig = DebounceConfig {
        debounce_ms: 500,
        stability_ms: 2_000,
        move_window_ms: 10_000,
    };

    #[test]
    fn debounce_coalesces_bursts() {
        let mut core = DebounceCore::new(CFG);
        let p = PathBuf::from("/r/a.jpg");
        core.push_changed(p.clone(), 0);
        core.push_changed(p.clone(), 100);
        core.push_changed(p.clone(), 400);
        assert!(core.due(899).is_empty(), "quiescence restarts per event");
        assert_eq!(core.due(900), vec![p.clone()]);
        assert_eq!(core.len(), 1, "a burst is one pending evaluation");
    }

    #[test]
    fn stability_needs_two_matching_checks() {
        let mut core = DebounceCore::new(CFG);
        let p = PathBuf::from("/r/a.jpg");
        core.push_changed(p.clone(), 0);
        assert_eq!(core.due(500), vec![p.clone()]);
        // First check records; not yet stable.
        assert_eq!(core.record_stat(&p, 100, 1, 500), Stability::Recheck);
        assert!(core.due(2_499).is_empty());
        assert_eq!(core.due(2_500), vec![p.clone()]);
        // Still growing: stays in the loop.
        assert_eq!(core.record_stat(&p, 200, 2, 2_500), Stability::Recheck);
        // Stable across two checks.
        assert_eq!(core.record_stat(&p, 200, 2, 4_500), Stability::Stable);
        assert!(core.is_empty());
    }

    #[test]
    fn remove_then_change_latest_wins() {
        let mut core = DebounceCore::new(CFG);
        let p = PathBuf::from("/r/a.jpg");
        core.push_removed(p.clone(), 0);
        assert!(core.is_remove(&p));
        core.push_changed(p.clone(), 100);
        assert!(!core.is_remove(&p));
    }

    #[test]
    fn next_due_tracks_earliest_deadline() {
        let mut core = DebounceCore::new(CFG);
        assert_eq!(core.next_due_ms(), None);
        core.push_changed(PathBuf::from("/r/a.jpg"), 0);
        core.push_changed(PathBuf::from("/r/b.jpg"), 300);
        assert_eq!(core.next_due_ms(), Some(500));
    }
}
