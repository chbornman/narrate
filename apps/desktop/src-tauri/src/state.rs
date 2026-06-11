//! Application state: thin composition of photoproof-core engines. The shell
//! owns wiring and lifetimes; all business logic stays in the core.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use photoproof_core::library::{Library, RootWatcherHandle};
use photoproof_core::search::Searcher;
use photoproof_core::sidecar::SidecarEngine;
use photoproof_core::{EventStore, SessionContext, SessionId, UtcMillis};

use crate::error::CmdError;
use crate::runtime::RuntimeHost;
use crate::scope::ScopeTracker;
use crate::search_types;
use crate::session::SessionManager;
use crate::settings::{self, AppSettings};

pub struct App {
    /// Shared with the sidecar engine (`SidecarEngine::new_shared`); lives
    /// for the process. Shutdown flushing is explicit, not Drop-driven.
    pub store: Arc<EventStore>,
    pub library: Arc<Library>,
    /// The library implements `ImageLocator` directly (DECISIONS B29).
    pub engine: SidecarEngine<'static, Arc<Library>>,
    pub app_data: PathBuf,
    pub scope: Mutex<ScopeTracker>,
    pub session: Mutex<SessionManager>,
    /// Read-only sibling connection for the debug panel's raw-row reads
    /// (dev builds only; every product-facing read goes through core APIs).
    #[cfg(feature = "debug-panel")]
    pub readq: Mutex<rusqlite::Connection>,
    pub watchers: Mutex<HashMap<String, RootWatcherHandle>>,
    pub settings: Mutex<AppSettings>,
    /// Last query echo for the debug panel's Search tab.
    pub last_search: Mutex<Option<search_types::QueryEcho>>,
    /// The M1 search engine (RETRIEVAL §4, packet P3.1) on its own
    /// connection; `interrupt()` cancels in-flight queries on new keystrokes.
    pub searcher: Searcher,
    /// The model runtime (RUNTIME, P6.2): instance lock, orphan sweep,
    /// tier, manifest, consent, downloads. No supervised child exists
    /// until P6.3 vendors real binaries; readiness stays false and the
    /// app IS the degraded mode that is the whole M1 product (§7).
    pub runtime: Arc<RuntimeHost>,
    pub shutdown: Arc<AtomicBool>,
}

impl App {
    pub fn init(app_data: PathBuf) -> Result<Self, CmdError> {
        std::fs::create_dir_all(&app_data)?;
        let db_path = app_data.join("photoproof.db");
        let cache_dir = app_data.join("previews");

        let store = Arc::new(EventStore::open(&db_path)?);
        let library = Arc::new(Library::open(&db_path, &cache_dir)?);
        let engine =
            SidecarEngine::new_shared(store.clone(), &db_path, &app_data, library.clone())?;
        let searcher = Searcher::open(&db_path).map_err(|e| CmdError::Invalid(e.to_string()))?;

        // CAPTURE §2.4 crash recovery, before opening the launch session:
        // any session left open by a dead process closes at its last event's
        // ts (else its start) with `closed_clean = false`, and close
        // processing is enqueued ONCE. Recovery mints no events. The empty
        // P6.1 processor registry then drains the pending queue (idempotent;
        // M3 registers the real processors).
        photoproof_core::capture::recover_crashed_sessions(&store)?;
        photoproof_core::capture::CloseProcessing::new().run_pending(&store)?;

        let ctx = SessionContext {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: settings::device_id(&app_data)?,
            root_context: None,
        };
        let session = SessionManager::open(&store, ctx)?;
        let app_settings = settings::load(&app_data);
        // RUNTIME init AFTER the journal spine: nothing about journaling
        // ever blocks on the runtime (§7/§10.1). Acquires the §8.5
        // instance lock, sweeps the §8.4 crash net, resolves config +
        // tier, writes the manifest.
        let runtime = Arc::new(RuntimeHost::init(app_data.clone()));

        Ok(Self {
            store,
            library,
            engine,
            app_data,
            scope: Mutex::new(ScopeTracker::new()),
            session: Mutex::new(session),
            #[cfg(feature = "debug-panel")]
            readq: Mutex::new(open_read_only(&db_path)?),
            watchers: Mutex::new(HashMap::new()),
            settings: Mutex::new(app_settings),
            last_search: Mutex::new(None),
            searcher,
            runtime,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// §2.5 step 3, pump-owned: drain enqueued close processing. Called
    /// from the sidecar pump tick — never from the close/quit path.
    pub fn run_close_processing(&self) -> Result<(), CmdError> {
        let mut session = self.session.lock().expect("session mutex");
        session.run_pending_close_processing(&self.store)?;
        Ok(())
    }

    /// Activity touch (CAPTURE §2.1/§2.2): refreshes the idle timer, rotating
    /// the session across a 30-minute boundary (idle measured on the
    /// monotonic capture clock; `ended_at` = the last activity's wall time).
    pub fn touch(&self) -> Result<(), CmdError> {
        let mut session = self.session.lock().expect("session mutex");
        session.touch(&self.store, &mut EngineFlush { app: self })?;
        Ok(())
    }

    pub fn session_id(&self) -> SessionId {
        self.session.lock().expect("session mutex").id().clone()
    }

    /// Shutdown (CAPTURE §2.5): close the session through the core engine
    /// (capture drain — `NoCapture` until P6.3 attaches the live engine;
    /// the pump-owned bounded drain wait is `pump::drain_capture_at_quit`
    /// — then sidecar flush, then bookkeeping; step 3 is enqueued for the
    /// next launch's pump), and re-flush the session journal afterwards so
    /// the sidecar carries `ended_ts` (SIDECARS S3).
    pub fn shutdown(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Stop watchers first so nothing new lands mid-flush.
        self.watchers.lock().expect("watchers mutex").clear();
        let session_id = self.session_id();
        if let Err(e) = self
            .session
            .lock()
            .expect("session mutex")
            .close(&self.store, &mut EngineFlush { app: self })
        {
            eprintln!("photoproof: session close failed at shutdown: {e}");
        }
        if let Err(e) = self.engine.flush_session(&session_id) {
            eprintln!("photoproof: session journal flush failed at shutdown: {e}");
        }
    }
}

/// The §2.5 step-2 hook: flush pending sidecars (and the closing session's
/// journal) when a session closes — rotation and shutdown alike.
struct EngineFlush<'a> {
    app: &'a App,
}

impl photoproof_core::capture::SidecarFlush for EngineFlush<'_> {
    fn flush_for_close(&mut self, closing: &SessionId) {
        let now = UtcMillis::now();
        if let Err(e) = self.app.engine.flush_all(now) {
            eprintln!("photoproof: sidecar flush at session close failed: {e}");
        }
        if let Err(e) = self.app.engine.flush_session(closing) {
            eprintln!("photoproof: session journal flush at session close failed: {e}");
        }
    }
}

/// Read-only sibling connection over the shared WAL database (the debug
/// panel's raw tail reads bypass core on purpose: they render raw rows).
#[cfg(feature = "debug-panel")]
fn open_read_only(db_path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_recovers_open_sessions_then_opens_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a previous run that died with a session open.
        let db = dir.path().join("photoproof.db");
        let dead_sid = {
            let store = EventStore::open(&db).unwrap();
            store
                .open_session(SessionContext {
                    app_version: "0.0.1".into(),
                    device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
                    root_context: None,
                })
                .unwrap()
        };
        let app = App::init(dir.path().to_path_buf()).unwrap();
        // The dead session is closed…
        let rec = app.store.session(&dead_sid).unwrap().unwrap();
        assert!(rec.ended_ts.is_some(), "recovered session must be closed");
        // …and the launch session is open and distinct.
        let live = app.session_id();
        assert_ne!(live, dead_sid);
        assert!(
            app.store
                .session(&live)
                .unwrap()
                .unwrap()
                .ended_ts
                .is_none()
        );
        app.shutdown();
        let closed = app.store.session(&live).unwrap().unwrap();
        assert!(closed.ended_ts.is_some(), "shutdown closes the session");
    }
}
