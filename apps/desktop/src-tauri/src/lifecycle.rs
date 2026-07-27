//! Process lifecycle and subsystem health.
//!
//! The phase answers "how far through launch/quit are we?" while health
//! answers "which independently useful parts are degraded?". Keeping those
//! axes separate lets the shell be usable while a volume or model runtime is
//! unavailable instead of inventing one giant all-or-nothing state machine.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    Cold,
    OpeningData,
    Usable,
    Reconciling,
    Ready,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subsystem {
    Storage,
    Settings,
    Roots,
    Watchers,
    Ingest,
    Maintenance,
    Previews,
    Vectors,
    Runtime,
    Capture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsystemHealth {
    Unknown,
    Healthy,
    Degraded { summary: String },
    Unavailable { summary: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleSnapshot {
    pub phase: LifecyclePhase,
    pub health: BTreeMap<Subsystem, SubsystemHealth>,
    pub phase_history: Vec<PhaseTiming>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseTiming {
    pub phase: LifecyclePhase,
    pub entered_at: SystemTime,
    pub elapsed_ms: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid application lifecycle transition from {from:?} to {to:?}")]
pub struct TransitionError {
    pub from: LifecyclePhase,
    pub to: LifecyclePhase,
}

pub struct AppLifecycle {
    state: Mutex<LifecycleSnapshot>,
    started_at: Instant,
}

impl Default for AppLifecycle {
    fn default() -> Self {
        let health = [
            Subsystem::Storage,
            Subsystem::Settings,
            Subsystem::Roots,
            Subsystem::Watchers,
            Subsystem::Ingest,
            Subsystem::Maintenance,
            Subsystem::Previews,
            Subsystem::Vectors,
            Subsystem::Runtime,
            Subsystem::Capture,
        ]
        .into_iter()
        .map(|subsystem| (subsystem, SubsystemHealth::Unknown))
        .collect();
        let entered_at = SystemTime::now();
        Self {
            state: Mutex::new(LifecycleSnapshot {
                phase: LifecyclePhase::Cold,
                health,
                phase_history: vec![PhaseTiming {
                    phase: LifecyclePhase::Cold,
                    entered_at,
                    elapsed_ms: 0,
                }],
            }),
            started_at: Instant::now(),
        }
    }
}

impl AppLifecycle {
    pub fn transition(&self, to: LifecyclePhase) -> Result<(), TransitionError> {
        let mut state = self.state.lock().expect("lifecycle mutex");
        let from = state.phase;
        let valid = from == to
            || matches!(
                (from, to),
                (LifecyclePhase::Cold, LifecyclePhase::OpeningData)
                    | (LifecyclePhase::OpeningData, LifecyclePhase::Usable)
                    | (LifecyclePhase::Usable, LifecyclePhase::Reconciling)
                    | (LifecyclePhase::Usable, LifecyclePhase::Ready)
                    | (LifecyclePhase::Reconciling, LifecyclePhase::Ready)
                    | (
                        LifecyclePhase::OpeningData
                            | LifecyclePhase::Usable
                            | LifecyclePhase::Reconciling
                            | LifecyclePhase::Ready,
                        LifecyclePhase::Stopping
                    )
            );
        if !valid {
            return Err(TransitionError { from, to });
        }
        if from != to {
            state.phase = to;
            state.phase_history.push(PhaseTiming {
                phase: to,
                entered_at: SystemTime::now(),
                elapsed_ms: self
                    .started_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }

    pub fn set_health(&self, subsystem: Subsystem, health: SubsystemHealth) {
        self.state
            .lock()
            .expect("lifecycle mutex")
            .health
            .insert(subsystem, health);
    }

    pub fn snapshot(&self) -> LifecycleSnapshot {
        self.state.lock().expect("lifecycle mutex").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_and_shutdown_transitions_are_explicit() {
        let lifecycle = AppLifecycle::default();
        for phase in [
            LifecyclePhase::OpeningData,
            LifecyclePhase::Usable,
            LifecyclePhase::Reconciling,
            LifecyclePhase::Ready,
            LifecyclePhase::Stopping,
        ] {
            lifecycle.transition(phase).unwrap();
        }
        assert_eq!(
            lifecycle.snapshot().phase,
            LifecyclePhase::Stopping,
            "the terminal process phase is explicit"
        );
        assert_eq!(
            lifecycle.transition(LifecyclePhase::Ready),
            Err(TransitionError {
                from: LifecyclePhase::Stopping,
                to: LifecyclePhase::Ready,
            }),
            "shutdown cannot be undone by a late background completion"
        );
        let history = lifecycle.snapshot().phase_history;
        assert_eq!(history.len(), 6);
        assert_eq!(history[0].phase, LifecyclePhase::Cold);
        assert_eq!(history[5].phase, LifecyclePhase::Stopping);
        assert!(
            history
                .windows(2)
                .all(|pair| pair[0].elapsed_ms <= pair[1].elapsed_ms),
            "phase timings are monotone from process lifecycle construction"
        );
    }

    #[test]
    fn health_is_independent_from_phase_and_other_subsystems() {
        let lifecycle = AppLifecycle::default();
        lifecycle.transition(LifecyclePhase::OpeningData).unwrap();
        lifecycle.set_health(Subsystem::Storage, SubsystemHealth::Healthy);
        lifecycle.set_health(
            Subsystem::Roots,
            SubsystemHealth::Degraded {
                summary: "archive is offline".into(),
            },
        );

        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.phase, LifecyclePhase::OpeningData);
        assert_eq!(
            snapshot.health[&Subsystem::Storage],
            SubsystemHealth::Healthy
        );
        assert!(matches!(
            snapshot.health[&Subsystem::Roots],
            SubsystemHealth::Degraded { .. }
        ));
        assert_eq!(
            snapshot.health[&Subsystem::Runtime],
            SubsystemHealth::Unknown
        );
    }
}
