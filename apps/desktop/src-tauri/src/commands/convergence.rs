//! Versioned, bounded catch-up snapshot for every desktop window.

use serde::Serialize;

use super::{S, run_blocking};
use crate::command_work::CommandClass;
use crate::convergence::ApplicationStateRevisions;
use crate::dto::{CollectionDto, PreviewCacheStatsDto, RootDto, RuntimeStatus, TopicDto};
use crate::error::{CmdError, CmdResult};
use crate::settings::{AppSettings, LiveControlStatus};

const SNAPSHOT_STABILITY_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStateSnapshot {
    pub revision: u64,
    pub revisions: ApplicationStateRevisions,
    pub settings: AppSettings,
    pub live_controls: Vec<LiveControlStatus>,
    pub roots: Vec<RootDto>,
    pub archived_roots: Vec<RootDto>,
    pub collections: Vec<CollectionDto>,
    pub topics: Vec<TopicDto>,
    pub runtime: RuntimeStatus,
    pub preview_cache: PreviewCacheStatsDto,
}

#[tauri::command]
pub async fn application_state_snapshot(app: S<'_>) -> CmdResult<ApplicationStateSnapshot> {
    let app = app.inner().clone();
    run_blocking(
        app,
        "app.application-state-snapshot",
        CommandClass::Read,
        |app| {
            for _ in 0..SNAPSHOT_STABILITY_ATTEMPTS {
                let before = app.convergence.snapshot();
                let settings = app.settings.lock().expect("settings mutex").clone();
                let live_controls = app
                    .live_controls
                    .lock()
                    .expect("live controls mutex")
                    .snapshot();
                let roots = super::library::active_roots(app)?;
                let archived_roots = super::library::archived_roots(app)?;
                let collections = super::collections::snapshot(app)?;
                let topics = app
                    .topics
                    .list()
                    .map_err(|error| CmdError::Invalid(error.to_string()))?
                    .into_iter()
                    .map(super::topics::topic_dto)
                    .collect();
                let runtime = app.runtime.status();
                let preview_cache = super::app::preview_cache_snapshot(app);
                let after = app.convergence.snapshot();
                if before == after {
                    return Ok(ApplicationStateSnapshot {
                        revision: after.0,
                        revisions: after.1,
                        settings,
                        live_controls,
                        roots,
                        archived_roots,
                        collections,
                        topics,
                        runtime,
                        preview_cache,
                    });
                }
            }
            Err(CmdError::Unavailable(
                "application state is changing; retry the snapshot".into(),
            ))
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tauri::Manager;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    use super::*;
    use crate::convergence::StateDomain;
    use crate::state::App;

    #[test]
    fn catch_up_snapshot_carries_the_committed_domain_revision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tauri_app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let state = Arc::new(App::init(tmp.path().join("appdata")).expect("app init"));
        tauri_app.manage(Arc::clone(&state));
        state
            .convergence
            .publish(tauri_app.handle(), [StateDomain::Roots]);

        let snapshot =
            tauri::async_runtime::block_on(application_state_snapshot(tauri_app.state()))
                .expect("application snapshot");

        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.revisions.roots, 1);
        assert_eq!(snapshot.revisions.settings, 0);
        assert!(snapshot.roots.is_empty());
        assert!(snapshot.archived_roots.is_empty());
        assert!(snapshot.collections.is_empty());
        assert!(snapshot.topics.is_empty());
    }
}
