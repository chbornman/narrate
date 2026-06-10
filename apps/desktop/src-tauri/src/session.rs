//! Session lifecycle wiring (CAPTURE §2): sessions are automatic, one open
//! while the app runs, 30-minute idle boundary, lazy closure. The store owns
//! the rows (`open_session`/`close_session`); this module owns the policy.

use photoproof_core::{EventStore, SessionContext, SessionId, StoreError, UtcMillis};

/// CAPTURE §2.2: the idle boundary.
pub const IDLE_BOUNDARY_MS: i64 = 30 * 60 * 1000;

/// Pure idle policy, separated from the store so the boundary rules are
/// testable without a database. Times are monotonic-ish milliseconds (the
/// caller feeds wall-clock millis; CAPTURE's capture-clock precision matters
/// for utterance binding, which is P6.1, not for the 30-minute boundary).
#[derive(Debug, Clone, Copy)]
pub struct IdlePolicy {
    last_activity: UtcMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Same session; idle timer refreshed.
    Same,
    /// CAPTURE §2.2: close the open session *before* the triggering activity,
    /// with `ended_at` = the LAST activity (never `now` — a session's span
    /// never includes dead air at its tail), then open a new session that the
    /// triggering activity belongs to.
    Rotate { ended_at: UtcMillis },
}

impl IdlePolicy {
    pub fn new(now: UtcMillis) -> Self {
        Self { last_activity: now }
    }

    pub fn on_activity(&mut self, now: UtcMillis) -> Boundary {
        let prev = self.last_activity;
        self.last_activity = now;
        if now.epoch_ms() - prev.epoch_ms() >= IDLE_BOUNDARY_MS {
            Boundary::Rotate { ended_at: prev }
        } else {
            Boundary::Same
        }
    }

    pub fn last_activity(&self) -> UtcMillis {
        self.last_activity
    }
}

pub struct SessionManager {
    id: SessionId,
    policy: IdlePolicy,
    ctx: SessionContext,
}

impl SessionManager {
    /// Open the launch session (crash recovery for previous unclosed sessions
    /// happens in `state::init` before this runs).
    pub fn open(store: &EventStore, ctx: SessionContext) -> Result<Self, StoreError> {
        let id = store.open_session(ctx.clone())?;
        Ok(Self {
            id,
            policy: IdlePolicy::new(UtcMillis::now()),
            ctx,
        })
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Record activity; rotates the session across an idle boundary.
    /// Returns the new session id when a rotation happened.
    pub fn touch(
        &mut self,
        store: &EventStore,
        now: UtcMillis,
    ) -> Result<Option<SessionId>, StoreError> {
        match self.policy.on_activity(now) {
            Boundary::Same => Ok(None),
            Boundary::Rotate { ended_at } => {
                store.close_session(&self.id, ended_at)?;
                let new_id = store.open_session(self.ctx.clone())?;
                self.id = new_id.clone();
                Ok(Some(new_id))
            }
        }
    }

    /// Clean shutdown: `ended_at` = last activity (CAPTURE §2.2's no-dead-air
    /// rule applies to quit as well — the close *is* triggered by quitting,
    /// but the session's span ends at the last real activity).
    pub fn close(&self, store: &EventStore) -> Result<(), StoreError> {
        store.close_session(&self.id, self.policy.last_activity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: i64) -> UtcMillis {
        UtcMillis::from_epoch_ms(v)
    }

    #[test]
    fn activity_within_30_minutes_stays_in_the_same_session() {
        let mut p = IdlePolicy::new(ms(0));
        // 29 minutes later: same session (CAPTURE §13.6).
        assert_eq!(p.on_activity(ms(29 * 60 * 1000)), Boundary::Same);
    }

    #[test]
    fn activity_after_30_minutes_rotates_with_ended_at_last_activity() {
        let mut p = IdlePolicy::new(ms(0));
        assert_eq!(p.on_activity(ms(5 * 60 * 1000)), Boundary::Same);
        // 31 minutes after the LAST activity: rotate, ended_at = that last
        // activity, never `now`.
        let b = p.on_activity(ms(5 * 60 * 1000 + 31 * 60 * 1000));
        assert_eq!(
            b,
            Boundary::Rotate {
                ended_at: ms(5 * 60 * 1000)
            }
        );
    }

    #[test]
    fn boundary_is_inclusive_at_exactly_30_minutes() {
        let mut p = IdlePolicy::new(ms(0));
        let b = p.on_activity(ms(IDLE_BOUNDARY_MS));
        assert_eq!(b, Boundary::Rotate { ended_at: ms(0) });
    }

    #[test]
    fn rotation_refreshes_the_idle_timer() {
        let mut p = IdlePolicy::new(ms(0));
        let t1 = 40 * 60 * 1000;
        assert!(matches!(p.on_activity(ms(t1)), Boundary::Rotate { .. }));
        // 10 minutes after the rotating activity: same (new) session.
        assert_eq!(p.on_activity(ms(t1 + 10 * 60 * 1000)), Boundary::Same);
    }

    #[test]
    fn touch_echoes_the_rotated_session_id_across_the_idle_boundary() {
        // The contract report_activity/add_stroke surface to the frontend:
        // session closure is LAZY, so the post-touch echo is how the
        // session-scoped pencil undo stack observes its session closing
        // (CAPTURE §8.5 "cleared at session close").
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let ctx = SessionContext {
            app_version: "0.0.1-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        };
        let mut mgr = SessionManager::open(&store, ctx).expect("open manager");
        let first = mgr.id().clone();

        // Within the boundary: same session, no echo.
        let now = UtcMillis::now();
        assert_eq!(mgr.touch(&store, now).expect("touch"), None);
        assert_eq!(mgr.id(), &first);

        // 30+ minutes after the last activity: the touch rotates and
        // echoes the NEW session id; the manager answers with it from
        // here on (what add_stroke binds the next stroke to).
        let later = UtcMillis::from_epoch_ms(now.epoch_ms() + IDLE_BOUNDARY_MS + 1);
        let rotated = mgr
            .touch(&store, later)
            .expect("touch")
            .expect("rotation echoes the new session id");
        assert_ne!(rotated, first);
        assert_eq!(mgr.id(), &rotated);
    }
}
