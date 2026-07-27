//! Full-stack M1 environment for the P4.1 end-to-end suites: EventStore +
//! Library + SidecarEngine + Searcher composed over one temp filesystem,
//! wired exactly like the desktop shell (Arc store via
//! `SidecarEngine::new_shared`, the library itself as the `ImageLocator` —
//! A3/A4), with a `FakeVolumeProbe` so the temp mount is a hermetic volume.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use photoproof_core::library::{
    FakeVolumeProbe, Library, LibraryOptions, PlatformIdKind, PlatformPlaceholderDetector,
    ProbedVolume, QueueOptions, RawlerExtractor, ScanOptions, ScanReport,
};
use photoproof_core::search::Searcher;
use photoproof_core::sidecar::SidecarEngine;
use photoproof_core::{EventStore, SessionContext, SessionId, UtcMillis};

pub fn probed(mount: &Path) -> ProbedVolume {
    ProbedVolume {
        mount_point: mount.to_path_buf(),
        platform_id: Some("uuid-m1-e2e-0001".into()),
        platform_kind: PlatformIdKind::LinuxFsUuid,
        label: Some("M1Vol".into()),
        fs_type: Some("ext4".into()),
        capacity_bytes: Some(1 << 32),
        read_only_flag: false,
        is_system_root: false,
        coarse_mtime: false,
    }
}

pub struct M1Env {
    /// Owns the temp tree for the env's lifetime (None when reopened over a
    /// caller-owned directory).
    pub tmp: Option<tempfile::TempDir>,
    pub base: PathBuf,
    pub mount: PathBuf,
    pub db: PathBuf,
    pub cache: PathBuf,
    pub app_data: PathBuf,
    pub probe: FakeVolumeProbe,
    pub store: Arc<EventStore>,
    pub lib: Arc<Library>,
    pub engine: SidecarEngine<'static, Arc<Library>>,
    pub searcher: Searcher,
    pub session: SessionId,
}

impl M1Env {
    pub fn new() -> M1Env {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().to_path_buf();
        std::fs::create_dir_all(base.join("mount")).unwrap();
        std::fs::create_dir_all(base.join("app-data")).unwrap();
        let mut env = Self::open(base);
        env.tmp = Some(tmp);
        env
    }

    /// Open (or re-open) the full stack over an existing base directory —
    /// the "app restart" primitive.
    pub fn open(base: PathBuf) -> M1Env {
        let mount = base.join("mount");
        let db = base.join("app-data/photoproof.db");
        let cache = base.join("app-data/previews");
        let app_data = base.join("app-data");
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);

        let store = Arc::new(EventStore::open(&db).expect("open store"));
        let lib = Arc::new(
            Library::open_with(
                &db,
                &cache,
                LibraryOptions {
                    probe: Arc::new(probe.clone()),
                    placeholders: Arc::new(PlatformPlaceholderDetector),
                    extractor: Arc::new(RawlerExtractor),
                    ..Default::default()
                },
            )
            .expect("open library"),
        );
        let engine = SidecarEngine::new_shared(store.clone(), &db, &app_data, lib.clone())
            .expect("open engine");
        let searcher = Searcher::open(&db).expect("open searcher");
        let session = store
            .open_session(SessionContext {
                app_version: "0.1.0-p41".into(),
                device_id: super::DEVICE_ID.into(),
                root_context: None,
            })
            .expect("open session");
        M1Env {
            tmp: None,
            base,
            mount,
            db,
            cache,
            app_data,
            probe,
            store,
            lib,
            engine,
            searcher,
            session,
        }
    }

    /// Simulate an app restart over the same state.
    pub fn reopen(mut self) -> M1Env {
        let tmp = self.tmp.take();
        let base = self.base.clone();
        drop(self);
        let mut env = Self::open(base);
        env.tmp = tmp;
        env
    }

    /// The restore drill: delete the SQLite index (db + WAL + SHM) and the
    /// preview cache, then reopen fresh. Sidecars on the mount survive —
    /// they are the truth (K11).
    pub fn wipe_index_and_reopen(mut self) -> M1Env {
        let tmp = self.tmp.take();
        let base = self.base.clone();
        let db = self.db.clone();
        let cache = self.cache.clone();
        drop(self);
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{}", db.display(), suffix));
            if p.exists() {
                std::fs::remove_file(&p).unwrap();
            }
        }
        if cache.exists() {
            std::fs::remove_dir_all(&cache).unwrap();
        }
        let mut env = Self::open(base);
        env.tmp = tmp;
        env
    }

    pub fn register(&self, sub: &str) -> String {
        let dir = self.mount.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        self.lib.register_root(&dir, Some(sub)).unwrap()
    }

    pub fn probe_volumes(&self) {
        self.lib.probe_volumes().unwrap();
    }

    pub fn scan(&self, root_id: &str) -> ScanReport {
        self.lib
            .scan_root(root_id, &ScanOptions::default())
            .unwrap()
    }

    pub fn drain(&self) {
        self.lib.process_queue(&QueueOptions::default()).unwrap();
    }

    pub fn flush_sidecars(&self) {
        self.engine.flush_all(UtcMillis::now()).unwrap();
    }

    pub fn conn(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .unwrap();
        conn
    }

    /// `PRAGMA integrity_check` must answer exactly "ok".
    pub fn assert_db_integrity(&self) {
        let conn = self.conn();
        let answer: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(answer, "ok", "SQLite integrity_check failed");
    }
}
