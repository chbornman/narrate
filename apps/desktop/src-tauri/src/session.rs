//! Session lifecycle wiring (CAPTURE §2) — a thin adapter over the core
//! session engine (photoproof_core::capture::session, P6.1): automatic
//! sessions, the 30-minute idle boundary on the MONOTONIC clock, lazy
//! closure with `ended_at` = last activity, and the §2.5 close order
//! (capture drain · sidecar flush · bookkeeping + close processors).
//!
//! The shell holds the capture seam (`CaptureDrain`) the session engine
//! drives: `NoCapture` until a real engine attaches (P6.3 wires the cpal
//! device + supervised sherpa client); rotation re-points the ATTACHED
//! drain through `CaptureDrain::session_rotated` (P6.2 obligation — no
//! undocumented caller burden). §2.5 step 3 never runs inline: closures
//! ENQUEUE and the sidecar pump drains them via
//! [`SessionManager::run_pending_close_processing`].

use photoproof_core::capture::{
    Activity, CaptureDrain, Clock, CloseProcessing, NoCapture, SessionEngine, SidecarFlush,
    SystemClock,
};
use photoproof_core::{EventStore, SessionContext, SessionId, StoreError};

pub struct SessionManager<C: Clock = SystemClock> {
    engine: SessionEngine<C>,
    /// Ordered close processors — EMPTY until M3 registers the real ones;
    /// the bookkeeping (`close_processing_done`) is live now (§2.5), and
    /// the PUMP owns the runs (never the close/quit path).
    processing: CloseProcessing,
    /// The attached capture pipeline. `NoCapture` until P6.3's device
    /// wiring; every rotation re-points whatever is attached.
    drain: Box<dyn CaptureDrain + Send>,
}

impl SessionManager<SystemClock> {
    /// Open the launch session (crash recovery for previous unclosed
    /// sessions happens in `state::init`, via the core, before this runs).
    pub fn open(store: &EventStore, ctx: SessionContext) -> Result<Self, StoreError> {
        Self::open_with_clock(store, ctx, SystemClock::new())
    }
}

impl<C: Clock> SessionManager<C> {
    pub fn open_with_clock(
        store: &EventStore,
        ctx: SessionContext,
        clock: C,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            engine: SessionEngine::open(store, ctx, clock)?,
            processing: CloseProcessing::new(),
            drain: Box::new(NoCapture),
        })
    }

    /// Attach the live capture pipeline (P6.3: the engine over the
    /// supervised sherpa client). From here on, idle rotations re-point
    /// it at the newly opened session automatically.
    #[allow(dead_code)] // P6.3 attaches the live engine; rotation re-pointing pinned by test
    pub fn attach_capture(&mut self, drain: Box<dyn CaptureDrain + Send>) {
        self.drain = drain;
    }

    pub fn id(&self) -> &SessionId {
        self.engine.id()
    }

    /// Record activity; rotates the session across an idle boundary (the
    /// §2.5 close steps run on the closing session first). Returns the new
    /// session id when a rotation happened.
    pub fn touch(
        &mut self,
        store: &EventStore,
        flush: &mut dyn SidecarFlush,
    ) -> Result<Option<SessionId>, StoreError> {
        match self.engine.on_activity(store, self.drain.as_mut(), flush)? {
            Activity::Same => Ok(None),
            Activity::Rotated { opened, .. } => Ok(Some(opened)),
        }
    }

    /// Clean shutdown: `ended_at` = last activity (CAPTURE §2.2's
    /// no-dead-air rule applies to quit as well).
    pub fn close(
        &mut self,
        store: &EventStore,
        flush: &mut dyn SidecarFlush,
    ) -> Result<(), StoreError> {
        self.engine.close_current(store, self.drain.as_mut(), flush)
    }

    /// §2.5 step 3, pump-owned: run the registered close processors for
    /// every enqueued closed session. Called from the sidecar pump tick —
    /// never from the close/quit path (P6.2 obligation).
    pub fn run_pending_close_processing(
        &mut self,
        store: &EventStore,
    ) -> Result<Vec<SessionId>, StoreError> {
        self.processing.run_pending(store)
    }
}

#[cfg(test)]
mod tests {
    use photoproof_core::capture::{FakeClock, IDLE_BOUNDARY_MS, NoFlush};

    use super::*;

    fn ctx() -> SessionContext {
        SessionContext {
            app_version: "0.0.1-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        }
    }

    /// The contract report_activity/add_stroke surface to the frontend:
    /// session closure is LAZY, so the post-touch echo is how the
    /// session-scoped pencil undo stack observes its session closing
    /// (CAPTURE §8.5 "cleared at session close").
    #[test]
    fn touch_echoes_the_rotated_session_id_across_the_idle_boundary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let clock = FakeClock::new(1_780_000_000_000);
        let mut mgr =
            SessionManager::open_with_clock(&store, ctx(), clock.clone()).expect("open manager");
        let first = mgr.id().clone();

        // Within the boundary: same session, no echo.
        clock.advance(IDLE_BOUNDARY_MS - 1);
        assert_eq!(mgr.touch(&store, &mut NoFlush).expect("touch"), None);
        assert_eq!(mgr.id(), &first);

        // 30+ minutes after the last activity: the touch rotates and
        // echoes the NEW session id; the closed row carries the §2.3
        // bookkeeping (clean close, processing complete — registry empty).
        clock.advance(IDLE_BOUNDARY_MS);
        let rotated = mgr
            .touch(&store, &mut NoFlush)
            .expect("touch")
            .expect("rotation echoes the new session id");
        assert_ne!(rotated, first);
        assert_eq!(mgr.id(), &rotated);
        assert!(store.session(&first).unwrap().unwrap().ended_ts.is_some());
        assert_eq!(
            store.session_close_state(&first).unwrap(),
            Some((true, false)),
            "step 3 never blocks: enqueued, not run inline"
        );
        // The PUMP owns the runs (P6.2 obligation): draining the queue
        // completes the bookkeeping.
        assert_eq!(
            mgr.run_pending_close_processing(&store).unwrap(),
            vec![first.clone()]
        );
        assert_eq!(
            store.session_close_state(&first).unwrap(),
            Some((true, true))
        );
    }

    /// P6.2 obligation (b): an ATTACHED capture drain is re-pointed at the
    /// newly opened session on rotation — through the seam, no caller
    /// burden.
    #[test]
    fn rotation_repoints_the_attached_capture_drain() {
        use std::sync::{Arc, Mutex};
        #[derive(Default)]
        struct Probe {
            drained_into: Arc<Mutex<Vec<String>>>,
            rotated_to: Arc<Mutex<Vec<String>>>,
        }
        impl CaptureDrain for Probe {
            fn drain_for_close(&mut self, _store: &EventStore, closing: &SessionId) {
                self.drained_into
                    .lock()
                    .unwrap()
                    .push(closing.as_str().to_owned());
            }
            fn session_rotated(&mut self, opened: &SessionId) {
                self.rotated_to
                    .lock()
                    .unwrap()
                    .push(opened.as_str().to_owned());
            }
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let clock = FakeClock::new(1_780_000_000_000);
        let mut mgr =
            SessionManager::open_with_clock(&store, ctx(), clock.clone()).expect("open manager");
        let first = mgr.id().clone();
        let probe = Probe::default();
        let (drained, rotated) = (probe.drained_into.clone(), probe.rotated_to.clone());
        mgr.attach_capture(Box::new(probe));

        clock.advance(IDLE_BOUNDARY_MS);
        let opened = mgr
            .touch(&store, &mut NoFlush)
            .expect("touch")
            .expect("rotated");
        assert_eq!(
            *drained.lock().unwrap(),
            vec![first.as_str().to_owned()],
            "§2.5 step 1 drained into the CLOSING session"
        );
        assert_eq!(
            *rotated.lock().unwrap(),
            vec![opened.as_str().to_owned()],
            "the attached engine now mints into the NEW session"
        );
    }

    #[test]
    fn close_runs_the_sidecar_flush_hook_before_the_row_close() {
        struct Probe(Vec<String>);
        impl SidecarFlush for Probe {
            fn flush_for_close(&mut self, closing: &SessionId) {
                self.0.push(closing.as_str().to_owned());
            }
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let clock = FakeClock::new(1_780_000_000_000);
        let mut mgr = SessionManager::open_with_clock(&store, ctx(), clock).expect("open");
        let id = mgr.id().clone();
        let mut probe = Probe(Vec::new());
        mgr.close(&store, &mut probe).expect("close");
        assert_eq!(probe.0, vec![id.as_str().to_owned()]);
        assert!(store.session(&id).unwrap().unwrap().ended_ts.is_some());
    }
}
