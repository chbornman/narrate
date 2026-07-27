//! One monotone application-state clock for cross-window convergence.
//!
//! Domain snapshot events remain useful for immediate rendering, but they are
//! not a catch-up protocol: a webview can miss an event while opening or while
//! a listener is being reinstalled. Every committed mutation advances this
//! process clock and emits the closed set of affected domains. A window then
//! reads `application_state_snapshot` and applies it only when its revision is
//! newer than the last snapshot it committed.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateDomain {
    Settings,
    Roots,
    Collections,
    Topics,
    Runtime,
    PreviewCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStateChanged {
    pub revision: u64,
    pub domains: Vec<StateDomain>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStateRevisions {
    pub settings: u64,
    pub roots: u64,
    pub collections: u64,
    pub topics: u64,
    pub runtime: u64,
    pub preview_cache: u64,
}

#[derive(Debug, Default)]
struct Clock {
    revision: u64,
    revisions: ApplicationStateRevisions,
}

#[derive(Debug, Default)]
pub struct StateConvergence {
    clock: Mutex<Clock>,
}

impl StateConvergence {
    pub fn snapshot(&self) -> (u64, ApplicationStateRevisions) {
        let clock = self.clock.lock().expect("state convergence mutex");
        (clock.revision, clock.revisions)
    }

    pub fn publish<R: Runtime>(
        &self,
        handle: &AppHandle<R>,
        domains: impl IntoIterator<Item = StateDomain>,
    ) -> u64 {
        let domains = domains.into_iter().collect::<Vec<_>>();
        let revision = {
            let mut clock = self.clock.lock().expect("state convergence mutex");
            clock.revision += 1;
            let revision = clock.revision;
            for domain in &domains {
                let target = match domain {
                    StateDomain::Settings => &mut clock.revisions.settings,
                    StateDomain::Roots => &mut clock.revisions.roots,
                    StateDomain::Collections => &mut clock.revisions.collections,
                    StateDomain::Topics => &mut clock.revisions.topics,
                    StateDomain::Runtime => &mut clock.revisions.runtime,
                    StateDomain::PreviewCache => &mut clock.revisions.preview_cache,
                };
                *target = revision;
            }
            revision
        };
        let _ = handle.emit(
            "application-state-changed",
            ApplicationStateChanged { revision, domains },
        );
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tauri::Listener;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[test]
    fn revisions_are_process_monotone() {
        let clock = StateConvergence::default();
        assert_eq!(clock.snapshot().0, 0);
        {
            let mut state = clock.clock.lock().unwrap();
            state.revision += 1;
        }
        assert_eq!(clock.snapshot().0, 1);
        {
            let mut state = clock.clock.lock().unwrap();
            state.revision += 1;
        }
        assert_eq!(clock.snapshot().0, 2);
    }

    #[test]
    fn publication_updates_only_named_domains_and_emits_the_same_revision() {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let payload = Arc::new(Mutex::new(None::<String>));
        let sink = Arc::clone(&payload);
        app.listen_any("application-state-changed", move |event| {
            *sink.lock().expect("payload mutex") = Some(event.payload().to_owned());
        });
        let clock = StateConvergence::default();

        let revision = clock.publish(
            app.handle(),
            [StateDomain::Settings, StateDomain::PreviewCache],
        );

        assert_eq!(revision, 1);
        assert_eq!(
            clock.snapshot(),
            (
                1,
                ApplicationStateRevisions {
                    settings: 1,
                    preview_cache: 1,
                    ..ApplicationStateRevisions::default()
                },
            )
        );
        let event: ApplicationStateChanged = serde_json::from_str(
            payload
                .lock()
                .expect("payload mutex")
                .as_deref()
                .expect("state event"),
        )
        .expect("event json");
        assert_eq!(event.revision, 1);
        assert_eq!(
            event.domains,
            vec![StateDomain::Settings, StateDomain::PreviewCache]
        );
    }
}
