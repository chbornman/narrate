//! Application state: thin composition of photoproof-core engines. The shell
//! owns wiring and lifetimes; all business logic stays in the core.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use photoproof_connectors::SherpaOnlineTranscriber;
use photoproof_connectors::silero::SileroVad;
use photoproof_core::capture::{CaptureDrain, CaptureEngine, SystemClock};
use photoproof_core::collections::Collections;
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

/// Cadence of the `pp-plan-converge` loop: every consent/config/download
/// mutation is re-applied to the supervisors within this interval (the
/// "within a couple of seconds" self-heal latency that
/// `RuntimeHost::apply_supervisor_plan` documents — this const is the one
/// authoritative home for that number).
const PLAN_CONVERGE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Pump-side wait between `drain_capture_at_quit` iterations while
/// trailing voice finals land at shutdown. The loop's BOUND is the
/// engine's own `DRAIN_WINDOW_MS` (5 s) deadline, so this only sets the
/// iteration granularity (~50 iterations worst case): short enough that
/// quit feels immediate when the final lands early.
const QUIT_DRAIN_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// SQLite busy timeout for the debug panel's read-only sibling connection.
/// The connection shares a WAL database with the ingest pump and sidecar
/// engine, so the timeout must outlast their longest write transaction or
/// debug reads error spuriously.
#[cfg(any(feature = "debug-panel", debug_assertions))]
const READQ_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5000);

pub struct App {
    /// Shared with the sidecar engine (`SidecarEngine::new_shared`); lives
    /// for the process. Shutdown flushing is explicit, not Drop-driven.
    pub store: Arc<EventStore>,
    pub library: Arc<Library>,
    /// The library implements `ImageLocator` directly (DECISIONS B29).
    pub engine: SidecarEngine<'static, Arc<Library>>,
    /// Collections (RETRIEVAL §10, P7.3): user truth mirrored to
    /// `collections.photoproof.json` by the sidecar pump's tick.
    pub collections: Arc<Collections>,
    pub app_data: PathBuf,
    pub scope: Mutex<ScopeTracker>,
    pub session: Mutex<SessionManager>,
    /// Read-only sibling connection for the debug panel's raw-row reads
    /// (dev builds only; every product-facing read goes through core APIs).
    #[cfg(any(feature = "debug-panel", debug_assertions))]
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
    /// P6.4: the LIVE capture engine over the supervised sherpa client —
    /// one for the process, shared between commands (toggle/scope/
    /// indicator) and the `pp-mic` audio thread. `None` only when the
    /// in-process VAD failed to build: the app runs, the mic stays away.
    /// Lock order is session → capture everywhere; the mic thread takes
    /// capture only.
    pub capture: Arc<Mutex<Option<CaptureEngine<'static, SystemClock>>>>,
    /// The running mic thread, present exactly while armed; dropping the
    /// handle stops and joins it (and the cpal stream with it).
    pub mic: Mutex<Option<crate::mic::MicHandle>>,
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
        // Open AFTER the sidecar engine so the schema exists; the open-time
        // reconcile union-merges an existing app-data
        // collections.photoproof.json into the database. The export-restore
        // case (fresh machine, only the one-click export) is covered inside
        // rebuild_from_sidecars, which imports the collections file it
        // finds beside the manifest (RETRIEVAL 10.2).
        let collections = Arc::new(Collections::open(&db_path, &app_data)?);
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
        let mut session = SessionManager::open(&store, ctx)?;
        let app_settings = settings::load(&app_data);
        // RUNTIME init AFTER the journal spine: nothing about journaling
        // ever blocks on the runtime (§7/§10.1). Acquires the §8.5
        // instance lock, sweeps the §8.4 crash net, resolves config +
        // tier, writes the manifest.
        let runtime = Arc::new(RuntimeHost::init(app_data.clone()));
        // P6.4: drive the real supervisors — apply the plan now, then a
        // slow converge loop (every mutation path self-heals within ~2 s)
        // and the §8.1 tick thread.
        runtime.apply_supervisor_plan();
        runtime.supervisors.spawn_tick_thread();
        {
            let runtime = Arc::clone(&runtime);
            std::thread::Builder::new()
                .name("pp-plan-converge".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(PLAN_CONVERGE_INTERVAL);
                        runtime.apply_supervisor_plan();
                    }
                })
                .expect("spawn plan-converge thread");
        }

        // P6.4 (6b): the live capture engine. The transcriber is
        // process-lifetime on purpose (Box::leak): the engine borrows it
        // for 'static and the WS client reads the supervisor's
        // EndpointCell — restarts re-point the port in memory, never here.
        // The VAD is compiled in (silero, §3.3 carve-out); if ONNX Runtime
        // cannot build the session the app still launches, voice disabled.
        let transcriber: &'static SherpaOnlineTranscriber =
            Box::leak(Box::new(SherpaOnlineTranscriber::new(
                runtime.asr_model_id(),
                runtime.supervisors.asr_endpoint.clone(),
            )));
        let capture = Arc::new(Mutex::new(match SileroVad::new() {
            Ok(vad) => Some(CaptureEngine::new(
                SystemClock::new(),
                transcriber,
                Box::new(vad),
                session.id().clone(),
            )),
            Err(e) => {
                eprintln!(
                    "photoproof: in-process VAD failed to build; voice capture disabled: {e}"
                );
                None
            }
        }));
        // The §2.5/§2.2 seam: rotations and closes drain + re-point the
        // engine through the session manager, no caller burden.
        session.attach_capture(Box::new(SharedDrain(Arc::clone(&capture))));

        Ok(Self {
            store,
            library,
            engine,
            collections,
            app_data,
            scope: Mutex::new(ScopeTracker::new()),
            session: Mutex::new(session),
            #[cfg(any(feature = "debug-panel", debug_assertions))]
            readq: Mutex::new(open_read_only(&db_path)?),
            watchers: Mutex::new(HashMap::new()),
            settings: Mutex::new(app_settings),
            last_search: Mutex::new(None),
            searcher,
            runtime,
            capture,
            mic: Mutex::new(None),
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
        // B52: stop the audio thread, then the bounded trailing-final
        // drain — BEFORE the supervisors stop (the ASR child must outlive
        // the drain to deliver finals whose onsets predate the quit).
        drop(self.mic.lock().expect("mic mutex").take());
        {
            let mut capture = self.capture.lock().expect("capture mutex");
            if let Some(engine) = capture.as_mut() {
                crate::pump::drain_capture_at_quit(engine, &self.store, &mut || {
                    std::thread::sleep(QUIT_DRAIN_WAIT);
                });
            }
        }
        self.runtime
            .capture_live
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // P6.4: children walk the §8.4 normal order before we flush.
        self.runtime.supervisors.shutdown();
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
        // App shutdown is an immediate-flush trigger (SIDECARS §9.1); the
        // collections file drains with the sidecars.
        if let Err(e) = self.collections.flush(UtcMillis::now()) {
            eprintln!("photoproof: collections flush failed at shutdown: {e}");
        }
    }
}

/// The session engine's capture seam over the SHARED engine slot: every
/// rotation/close drains and re-points whatever is armed. The session
/// mutex is always taken before this lock (commands go session → capture;
/// the mic thread takes capture only), so the nesting cannot deadlock.
struct SharedDrain(Arc<Mutex<Option<CaptureEngine<'static, SystemClock>>>>);

impl CaptureDrain for SharedDrain {
    fn drain_for_close(&mut self, store: &EventStore, closing: &SessionId) {
        if let Some(engine) = self.0.lock().expect("capture mutex").as_mut() {
            engine.drain_for_close(store, closing);
        }
    }

    fn last_capture_activity(&self) -> Option<(u64, UtcMillis)> {
        self.0
            .lock()
            .expect("capture mutex")
            .as_ref()
            .and_then(CaptureDrain::last_capture_activity)
    }

    fn session_rotated(&mut self, opened: &SessionId) {
        if let Some(engine) = self.0.lock().expect("capture mutex").as_mut() {
            engine.session_rotated(opened);
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
#[cfg(any(feature = "debug-panel", debug_assertions))]
fn open_read_only(db_path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(READQ_BUSY_TIMEOUT)?;
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
