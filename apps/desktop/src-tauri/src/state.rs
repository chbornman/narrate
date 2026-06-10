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
use crate::locator::LibLocator;
use crate::scope::ScopeTracker;
use crate::session::SessionManager;
use crate::settings::{self, AppSettings};
use crate::{gridq, search_types};

pub struct App {
    /// Leaked on purpose: the store lives for the process (the sidecar
    /// engine borrows it for `'static`; the app never tears it down before
    /// exit, and shutdown flushing is explicit, not Drop-driven).
    pub store: &'static EventStore,
    pub library: Arc<Library>,
    pub engine: SidecarEngine<'static, LibLocator>,
    pub app_data: PathBuf,
    pub scope: Mutex<ScopeTracker>,
    pub session: Mutex<SessionManager>,
    /// Read-only sibling connection for batched grid reads (see gridq.rs).
    pub readq: Mutex<rusqlite::Connection>,
    pub watchers: Mutex<HashMap<String, RootWatcherHandle>>,
    pub settings: Mutex<AppSettings>,
    /// Last query echo for the debug panel's Search tab.
    pub last_search: Mutex<Option<search_types::QueryEcho>>,
    /// The M1 search engine (RETRIEVAL §4, packet P3.1) on its own
    /// connection; `interrupt()` cancels in-flight queries on new keystrokes.
    pub searcher: Searcher,
    pub shutdown: Arc<AtomicBool>,
}

impl App {
    pub fn init(app_data: PathBuf) -> Result<Self, CmdError> {
        std::fs::create_dir_all(&app_data)?;
        let db_path = app_data.join("photoproof.db");
        let cache_dir = app_data.join("previews");

        let store: &'static EventStore = Box::leak(Box::new(EventStore::open(&db_path)?));
        let library = Arc::new(Library::open(&db_path, &cache_dir)?);
        let engine = SidecarEngine::new(store, &db_path, &app_data, LibLocator(library.clone()))?;
        let readq = gridq::open_read_only(&db_path)?;
        let searcher = Searcher::open(&db_path).map_err(|e| CmdError::Invalid(e.to_string()))?;

        // CAPTURE §2.4 crash recovery, before opening the launch session:
        // any session left open by a dead process closes at its last event's
        // ts (else its start), `closed_clean = false` semantics are the
        // store's. Recovery mints no events.
        for sid in gridq::open_sessions(&readq)? {
            let ended = gridq::last_event_ts_ms(&readq, &sid)?
                .or(gridq::session_started_ms(&readq, &sid)?)
                .unwrap_or_else(|| UtcMillis::now().epoch_ms());
            let session_id = SessionId::from_str_strict(&sid)
                .map_err(|e| CmdError::Invalid(format!("recovered session id: {e}")))?;
            store.close_session(&session_id, UtcMillis::from_epoch_ms(ended))?;
        }

        let ctx = SessionContext {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: settings::device_id(&app_data)?,
            root_context: None,
        };
        let session = SessionManager::open(store, ctx)?;
        let app_settings = settings::load(&app_data);

        Ok(Self {
            store,
            library,
            engine,
            app_data,
            scope: Mutex::new(ScopeTracker::new()),
            session: Mutex::new(session),
            readq: Mutex::new(readq),
            watchers: Mutex::new(HashMap::new()),
            settings: Mutex::new(app_settings),
            last_search: Mutex::new(None),
            searcher,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Activity touch (CAPTURE §2.1/§2.2): refreshes the idle timer, rotating
    /// the session across a 30-minute boundary.
    pub fn touch(&self) -> Result<(), CmdError> {
        let mut session = self.session.lock().expect("session mutex");
        session.touch(self.store, UtcMillis::now())?;
        Ok(())
    }

    pub fn session_id(&self) -> SessionId {
        self.session.lock().expect("session mutex").id().clone()
    }

    /// Shutdown (CAPTURE §2.5 steps for M1): close the session, then flush
    /// every pending sidecar immediately (SIDECARS S3: immediate flush on
    /// shutdown), including the closing session's journal.
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
            .close(self.store)
        {
            eprintln!("photoproof: session close failed at shutdown: {e}");
        }
        let now = UtcMillis::now();
        if let Err(e) = self.engine.flush_all(now) {
            eprintln!("photoproof: sidecar flush failed at shutdown: {e}");
        }
        if let Err(e) = self.engine.flush_session(&session_id) {
            eprintln!("photoproof: session journal flush failed at shutdown: {e}");
        }
    }
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
