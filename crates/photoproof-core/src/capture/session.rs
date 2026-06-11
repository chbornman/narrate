//! Session lifecycle (spec/CAPTURE.md §2): automatic sessions, the
//! 30-minute idle boundary on the CAPTURE clock, lazy closure, crash
//! recovery, and the ordered close-processing framework (empty in P6.1 —
//! M3 registers the real processors).

use crate::id::{SessionId, UtcMillis};
use crate::store::{EventStore, SessionContext, StoreError};

use super::clock::Clock;

/// CAPTURE §2.2: the idle boundary (≥, monotonic).
pub const IDLE_BOUNDARY_MS: u64 = 30 * 60 * 1000;

/// The capture seam sessions talk to: §2.5 step 1 (drain), the §2.1
/// capture-side activity feed, and §2.2 rotation follow-through.
/// Implemented by the capture engine; sessions only know the seam.
pub trait CaptureDrain {
    /// Drain into the CLOSING session: trailing finals mint normally into
    /// it; after the 5 s cap, in-flight utterances are abandoned (§6.5).
    fn drain_for_close(&mut self, store: &EventStore, closing: &SessionId);

    /// §2.1: mic arm/disarm and VAD speech activity (any SpeechStart /
    /// Partial / Final while armed) are ACTIVITY. The most recent
    /// capture-side activity as `(capture clock ms, wall clock)`; `None`
    /// when no capture pipeline is attached or nothing has happened yet.
    /// [`SessionEngine`] consults this before every idle decision, so a
    /// speech-filled UI-idle window never reads as idle and an idle
    /// boundary never bisects an in-flight utterance (§2.2).
    fn last_capture_activity(&self) -> Option<(u64, UtcMillis)> {
        None
    }

    /// §2.2: the idle boundary rotated sessions; subsequent capture
    /// commits belong to `opened`. The session engine calls this after
    /// every rotation, so the capture side follows the new session through
    /// the seam — no separate wiring obligation on the shell.
    fn session_rotated(&mut self, _opened: &SessionId) {}
}

/// No capture pipeline attached (typed-only shells, tests).
pub struct NoCapture;

impl CaptureDrain for NoCapture {
    fn drain_for_close(&mut self, _store: &EventStore, _closing: &SessionId) {}
}

/// §2.5 step 2: sidecar flush. SIDECARS owns mechanics; sessions own the
/// ordering.
pub trait SidecarFlush {
    fn flush_for_close(&mut self, closing: &SessionId);
}

/// No sidecar writer attached (tests).
pub struct NoFlush;

impl SidecarFlush for NoFlush {
    fn flush_for_close(&mut self, _closing: &SessionId) {}
}

/// §2.5 step 3: one registered close processor. Idempotent and resumable
/// by contract; M3 registers session summary / rolling summaries /
/// sentiment / embeddings — retrieval fuel only, never user-facing prose.
pub trait CloseProcessor: Send {
    fn id(&self) -> &str;
    fn run(&mut self, store: &EventStore, session: &SessionId) -> Result<(), StoreError>;
}

/// The ordered close-processor registry — EMPTY in P6.1 by design. The
/// `close_processing_done` bookkeeping makes a run idempotent and
/// resumable: a quit-before-done re-enqueues on next launch (§2.5).
#[derive(Default)]
pub struct CloseProcessing {
    processors: Vec<Box<dyn CloseProcessor>>,
}

impl CloseProcessing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registration order = execution order (§2.5: an ordered list).
    pub fn register(&mut self, p: Box<dyn CloseProcessor>) {
        self.processors.push(p);
    }

    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    /// Run the registered processors for one closed session, in order, then
    /// mark `close_processing_done`. No-op when already done (idempotent);
    /// an error leaves the pending mark so the next launch resumes.
    pub fn run_for(&mut self, store: &EventStore, session: &SessionId) -> Result<(), StoreError> {
        if let Some((_, done)) = store.session_close_state(session)?
            && done
        {
            return Ok(());
        }
        for p in &mut self.processors {
            p.run(store, session)?;
        }
        store.mark_close_processing_done(session)?;
        Ok(())
    }

    /// Re-enqueued work from previous launches: every closed session whose
    /// processing did not complete (§2.5, crash-recovered sessions use the
    /// same processors). Returns the sessions processed.
    pub fn run_pending(&mut self, store: &EventStore) -> Result<Vec<SessionId>, StoreError> {
        let pending = store.close_processing_pending()?;
        for s in &pending {
            self.run_for(store, s)?;
        }
        Ok(pending)
    }
}

/// Crash recovery (§2.4), run on launch BEFORE opening a new session: any
/// `sessions` row with `ended_at IS NULL` means the previous process died
/// with the session open. Sets `ended_at` = ts of the session's last event
/// (else `started_at`), records `closed_clean = false`, and enqueues close
/// processing ONCE (idempotent — the bookkeeping insert keeps the first
/// verdict). Recovery mints no events.
pub fn recover_crashed_sessions(store: &EventStore) -> Result<Vec<SessionId>, StoreError> {
    let open = store.open_sessions()?;
    let mut recovered = Vec::with_capacity(open.len());
    for rec in open {
        let ended = store.last_event_ts(&rec.id)?.unwrap_or(rec.started_ts);
        store.close_session(&rec.id, ended)?;
        store.record_session_close(&rec.id, false)?;
        recovered.push(rec.id);
    }
    Ok(recovered)
}

/// Result of an activity touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Same,
    /// The idle boundary fired: `closed` ended BEFORE the triggering
    /// activity was processed (ended_at = the LAST activity's wall time,
    /// never `now`); the triggering activity belongs to `opened`.
    Rotated {
        closed: SessionId,
        opened: SessionId,
    },
}

/// The §2 engine. Exactly one session is Open while the app runs; closure
/// is lazy — no timer fires during idle.
pub struct SessionEngine<C: Clock> {
    clock: C,
    id: SessionId,
    ctx: SessionContext,
    last_activity_mono: u64,
    last_activity_wall: UtcMillis,
}

impl<C: Clock> SessionEngine<C> {
    /// Open the launch session. Run [`recover_crashed_sessions`] first.
    pub fn open(store: &EventStore, ctx: SessionContext, clock: C) -> Result<Self, StoreError> {
        let id = store.open_session(ctx.clone())?;
        let last_activity_mono = clock.mono_ms();
        let last_activity_wall = clock.wall();
        Ok(Self {
            clock,
            id,
            ctx,
            last_activity_mono,
            last_activity_wall,
        })
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn last_activity_wall(&self) -> UtcMillis {
        self.last_activity_wall
    }

    /// Pull §2.1 capture-side activity (mic arm/disarm, VAD speech, partial
    /// and final arrivals while armed) forward into the idle bookkeeping —
    /// run before EVERY idle decision and before ended_at is recorded, so
    /// speech counts as activity and a boundary never bisects an in-flight
    /// utterance (§2.2).
    fn sync_capture_activity(&mut self, drain: &dyn CaptureDrain) {
        if let Some((mono, wall)) = drain.last_capture_activity()
            && mono > self.last_activity_mono
        {
            self.last_activity_mono = mono;
            self.last_activity_wall = wall;
        }
    }

    /// Record activity (§2.1). When `now_mono − last_activity_mono ≥ 30
    /// min` — with `last_activity` covering capture-side activity through
    /// the [`CaptureDrain`] seam, not just UI touches — the open session
    /// closes BEFORE the triggering activity is processed — §2.5 order:
    /// capture drain (mints into the closing session), sidecar flush,
    /// bookkeeping — and a new session opens that the triggering activity
    /// belongs to; the capture side is told via
    /// [`CaptureDrain::session_rotated`].
    ///
    /// §2.5 step 3 (close processors) is NOT run here: "step 3 never
    /// blocks" — closure only ENQUEUES (`close_processing_done = false`);
    /// the pump thread drains via [`CloseProcessing::run_pending`], and a
    /// quit-before-done re-enqueues on next launch (P6.2 obligation:
    /// processors off the inline close/quit path before real processors
    /// register).
    pub fn on_activity(
        &mut self,
        store: &EventStore,
        drain: &mut dyn CaptureDrain,
        flush: &mut dyn SidecarFlush,
    ) -> Result<Activity, StoreError> {
        self.sync_capture_activity(drain);
        let now_mono = self.clock.mono_ms();
        let now_wall = self.clock.wall();
        let idle = now_mono.saturating_sub(self.last_activity_mono);
        if idle < IDLE_BOUNDARY_MS {
            self.last_activity_mono = now_mono;
            self.last_activity_wall = now_wall;
            return Ok(Activity::Same);
        }
        let closed = self.id.clone();
        self.close_steps(store, drain, flush, true)?;
        let opened = store.open_session(self.ctx.clone())?;
        self.id = opened.clone();
        self.last_activity_mono = now_mono;
        self.last_activity_wall = now_wall;
        drain.session_rotated(&opened);
        Ok(Activity::Rotated { closed, opened })
    }

    /// Clean shutdown (§2.5): steps 1–2 block quit (capped by the drain's
    /// 5 s + the sidecar budget); step 3 never blocks — closure enqueues,
    /// the pump (or the next launch) runs the processors.
    pub fn close_current(
        &mut self,
        store: &EventStore,
        drain: &mut dyn CaptureDrain,
        flush: &mut dyn SidecarFlush,
    ) -> Result<(), StoreError> {
        // Speech after the last UI touch counts toward ended_at (§2.1).
        self.sync_capture_activity(drain);
        self.close_steps(store, drain, flush, true)
    }

    fn close_steps(
        &mut self,
        store: &EventStore,
        drain: &mut dyn CaptureDrain,
        flush: &mut dyn SidecarFlush,
        clean: bool,
    ) -> Result<(), StoreError> {
        let closing = self.id.clone();
        // 1. Capture drain — trailing finals mint into the CLOSING session.
        drain.drain_for_close(store, &closing);
        // 2. Sidecar flush.
        flush.flush_for_close(&closing);
        // ended_at = wall time of the LAST activity, never `now` (§2.2 —
        // a session's span never includes dead air at its tail).
        store.close_session(&closing, self.last_activity_wall)?;
        // 3. ENQUEUE close processing (`close_processing_done = false`);
        //    the pump runs it — never this path.
        store.record_session_close(&closing, clean)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::clock::FakeClock;
    use crate::store::RemarkSource;

    fn ctx() -> SessionContext {
        SessionContext {
            app_version: "0.1.0-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        }
    }

    fn store() -> (tempfile::TempDir, EventStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("journal.db")).unwrap();
        (dir, store)
    }

    fn touch(engine: &mut SessionEngine<FakeClock>, store: &EventStore) -> Activity {
        engine
            .on_activity(store, &mut NoCapture, &mut NoFlush)
            .unwrap()
    }

    #[test]
    fn idle_boundary_is_inclusive_at_exactly_30_minutes_on_the_mono_clock() {
        let (_d, store) = store();
        let clock = FakeClock::new(1_000_000);
        let mut engine = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
        clock.advance(IDLE_BOUNDARY_MS - 1);
        assert_eq!(touch(&mut engine, &store), Activity::Same);
        clock.advance(IDLE_BOUNDARY_MS - 1);
        assert_eq!(touch(&mut engine, &store), Activity::Same);
        clock.advance(IDLE_BOUNDARY_MS);
        assert!(matches!(
            touch(&mut engine, &store),
            Activity::Rotated { .. }
        ));
    }

    #[test]
    fn wall_clock_steps_never_trigger_or_suppress_the_boundary() {
        let (_d, store) = store();
        let clock = FakeClock::new(1_000_000);
        let mut engine = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
        // A 2-hour wall jump with 1 ms of real (monotonic) time: same session.
        clock.advance(1);
        clock.skew_wall(2 * 60 * 60 * 1000);
        assert_eq!(touch(&mut engine, &store), Activity::Same);
    }

    #[test]
    fn rotation_ends_the_old_session_at_the_last_activity_never_now() {
        let (_d, store) = store();
        let clock = FakeClock::new(1_000_000);
        let mut engine = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
        clock.advance(5_000);
        assert_eq!(touch(&mut engine, &store), Activity::Same);
        let last_wall = engine.last_activity_wall();
        clock.advance(31 * 60 * 1000);
        let Activity::Rotated { closed, opened } = touch(&mut engine, &store) else {
            panic!("expected rotation");
        };
        assert_eq!(engine.id(), &opened);
        let rec = store.session(&closed).unwrap().unwrap();
        assert_eq!(rec.ended_ts, Some(last_wall), "ended_at = LAST activity");
        // §2.5 step 3 never blocks: closure ENQUEUES; the pump completes.
        assert_eq!(
            store.session_close_state(&closed).unwrap(),
            Some((true, false)),
            "clean close; close processing enqueued, not run inline"
        );
        let mut processing = CloseProcessing::new();
        assert_eq!(
            processing.run_pending(&store).unwrap(),
            vec![closed.clone()],
            "the pump drains the enqueued closure"
        );
        assert_eq!(
            store.session_close_state(&closed).unwrap(),
            Some((true, true))
        );
        assert!(store.session(&opened).unwrap().unwrap().ended_ts.is_none());
    }

    #[test]
    fn crash_recovery_closes_at_last_event_ts_and_enqueues_once() {
        let (_d, store) = store();
        let dead = store.open_session(ctx()).unwrap();
        let e = store
            .append(
                &dead,
                crate::store::EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "last words".into(),
                    targets: vec![],
                },
                None,
            )
            .unwrap();
        let recovered = recover_crashed_sessions(&store).unwrap();
        assert_eq!(recovered, vec![dead.clone()]);
        let rec = store.session(&dead).unwrap().unwrap();
        assert_eq!(rec.ended_ts, Some(e.ts), "ended_at = last event ts");
        assert_eq!(
            store.session_close_state(&dead).unwrap(),
            Some((false, false)),
            "unclean close, processing pending"
        );
        // Enqueued ONCE: pending exactly once, and a second recovery pass
        // finds nothing to recover and flips no bookkeeping.
        assert_eq!(
            store.close_processing_pending().unwrap(),
            vec![dead.clone()]
        );
        assert!(recover_crashed_sessions(&store).unwrap().is_empty());
        assert_eq!(
            store.close_processing_pending().unwrap(),
            vec![dead.clone()]
        );
        // The empty framework drains the queue idempotently.
        let mut processing = CloseProcessing::new();
        assert_eq!(processing.run_pending(&store).unwrap(), vec![dead.clone()]);
        assert!(store.close_processing_pending().unwrap().is_empty());
        assert!(processing.run_pending(&store).unwrap().is_empty());
    }

    #[test]
    fn recovery_with_no_events_falls_back_to_started_at_and_mints_nothing() {
        let (_d, store) = store();
        let dead = store.open_session(ctx()).unwrap();
        recover_crashed_sessions(&store).unwrap();
        let rec = store.session(&dead).unwrap().unwrap();
        assert_eq!(rec.ended_ts, Some(rec.started_ts));
        assert!(store.sessionlevel_events(&dead).unwrap().is_empty());
    }

    #[test]
    fn close_processors_run_in_registration_order_and_resume() {
        use std::sync::{Arc, Mutex};
        struct P(&'static str, Arc<Mutex<Vec<&'static str>>>, bool);
        impl CloseProcessor for P {
            fn id(&self) -> &str {
                self.0
            }
            fn run(&mut self, _s: &EventStore, _id: &SessionId) -> Result<(), StoreError> {
                self.1.lock().unwrap().push(self.0);
                if self.2 {
                    return Err(StoreError::Invalid("processor failed".into()));
                }
                Ok(())
            }
        }
        let (_d, store) = store();
        let s = store.open_session(ctx()).unwrap();
        store
            .close_session(&s, UtcMillis::from_epoch_ms(1))
            .unwrap();
        store.record_session_close(&s, true).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        // First run fails on the second processor: done stays false.
        let mut failing = CloseProcessing::new();
        failing.register(Box::new(P("summary", log.clone(), false)));
        failing.register(Box::new(P("sentiment", log.clone(), true)));
        assert!(failing.run_for(&store, &s).is_err());
        assert_eq!(store.session_close_state(&s).unwrap(), Some((true, false)));
        // Resumed run (idempotent processors re-run) completes and marks done.
        let mut ok = CloseProcessing::new();
        ok.register(Box::new(P("summary", log.clone(), false)));
        ok.register(Box::new(P("sentiment", log.clone(), false)));
        ok.run_for(&store, &s).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec!["summary", "sentiment", "summary", "sentiment"],
            "ordered, resumable"
        );
        assert_eq!(store.session_close_state(&s).unwrap(), Some((true, true)));
        // Done sessions are skipped entirely.
        ok.run_for(&store, &s).unwrap();
        assert_eq!(log.lock().unwrap().len(), 4);
    }
}
