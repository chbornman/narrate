//! Session lifecycle wiring (CAPTURE §2) — a thin adapter over the core
//! session engine (photoproof_core::capture::session, P6.1): automatic
//! sessions, the 30-minute idle boundary on the MONOTONIC clock, lazy
//! closure with `ended_at` = last activity, and the §2.5 close order
//! (capture drain · sidecar flush · bookkeeping + close processors).
//!
//! The shell attaches no capture pipeline yet (`NoCapture`): the voice
//! engine is mock-verified in core and wires in with P6.2's supervised
//! runtime. Sidecar flushing is the one live §2.5 hook here.

use photoproof_core::capture::{
    Activity, Clock, CloseProcessing, NoCapture, SessionEngine, SidecarFlush, SystemClock,
};
use photoproof_core::{EventStore, SessionContext, SessionId, StoreError};

pub struct SessionManager<C: Clock = SystemClock> {
    engine: SessionEngine<C>,
    /// Ordered close processors — EMPTY until M3 registers the real ones;
    /// the bookkeeping (`close_processing_done`) is live now (§2.5).
    processing: CloseProcessing,
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
        })
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
        match self
            .engine
            .on_activity(store, &mut NoCapture, flush, &mut self.processing)?
        {
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
        self.engine
            .close_current(store, &mut NoCapture, flush, &mut self.processing)
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
            Some((true, true))
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
