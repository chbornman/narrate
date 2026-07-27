//! Explicit, signed desktop updates.
//!
//! Updates are deliberately dark in developer and unsigned CI builds. A
//! production build must set `PHOTOPROOF_UPDATES_ENABLED=1` while compiling
//! and merge the release-only Tauri config containing the real HTTPS endpoint
//! and updater public key. Tauri verifies every downloaded artifact with that
//! public key; this module never accepts an unsigned fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::state::App;

const UPDATE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub enabled: bool,
    pub current_version: String,
    pub phase: String,
    pub available: Option<UpdateMetadata>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct MutableUpdateStatus {
    phase: String,
    available: Option<UpdateMetadata>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
}

impl Default for MutableUpdateStatus {
    fn default() -> Self {
        Self {
            phase: if updates_enabled() {
                "idle".into()
            } else {
                "disabled".into()
            },
            available: None,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        }
    }
}

pub struct UpdateCoordinator {
    operation_active: AtomicBool,
    status: Mutex<MutableUpdateStatus>,
}

impl Default for UpdateCoordinator {
    fn default() -> Self {
        Self {
            operation_active: AtomicBool::new(false),
            status: Mutex::new(MutableUpdateStatus::default()),
        }
    }
}

struct OperationGuard<'a>(&'a AtomicBool);

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn updates_enabled() -> bool {
    option_env!("PHOTOPROOF_UPDATES_ENABLED") == Some("1")
}

fn metadata(update: &Update) -> UpdateMetadata {
    UpdateMetadata {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
        published_at: update.date.map(|date| date.to_string()),
    }
}

impl UpdateCoordinator {
    fn begin(&self, phase: &str) -> Result<OperationGuard<'_>, String> {
        if !updates_enabled() {
            return Err(
                "signed updates are not configured in this build; install a production package"
                    .into(),
            );
        }
        self.operation_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "another update operation is already running".to_owned())?;
        let mut status = self.status.lock().expect("update status mutex");
        status.phase = phase.into();
        status.available = None;
        status.downloaded_bytes = 0;
        status.total_bytes = None;
        status.error = None;
        Ok(OperationGuard(&self.operation_active))
    }

    fn fail(&self, error: &str) {
        let mut status = self.status.lock().expect("update status mutex");
        status.phase = "failed".into();
        status.error = Some(error.into());
    }

    fn snapshot(&self, app: &AppHandle) -> UpdateStatus {
        let status = self.status.lock().expect("update status mutex");
        UpdateStatus {
            enabled: updates_enabled(),
            current_version: app.package_info().version.to_string(),
            phase: status.phase.clone(),
            available: status.available.clone(),
            downloaded_bytes: status.downloaded_bytes,
            total_bytes: status.total_bytes,
            error: status.error.clone(),
        }
    }
}

async fn check(app: &AppHandle) -> Result<Option<Update>, String> {
    app.updater_builder()
        .timeout(UPDATE_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn update_status(
    app: AppHandle,
    coordinator: State<'_, Arc<UpdateCoordinator>>,
) -> UpdateStatus {
    coordinator.snapshot(&app)
}

/// Check only after explicit user intent. Startup never contacts the release
/// service, so offline launch stays fast and update telemetry is not invented.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    coordinator: State<'_, Arc<UpdateCoordinator>>,
) -> Result<UpdateStatus, String> {
    let operation = coordinator.begin("checking")?;
    let result = check(&app).await;
    match &result {
        Ok(update) => {
            let mut status = coordinator.status.lock().expect("update status mutex");
            status.available = update.as_ref().map(metadata);
            status.phase = if update.is_some() {
                "available".into()
            } else {
                "current".into()
            };
            status.error = None;
        }
        Err(error) => coordinator.fail(error),
    }
    drop(operation);
    match result {
        Ok(_) => Ok(coordinator.snapshot(&app)),
        Err(error) => Err(error),
    }
}

/// Re-check the signed feed and require the exact version the user approved.
/// The complete update is downloaded and signature-verified before application
/// shutdown starts. Only then do we stop writers, install, and restart.
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    coordinator: State<'_, Arc<UpdateCoordinator>>,
    expected_version: String,
) -> Result<(), String> {
    let operation = coordinator.begin("checking")?;
    let Some(update) = check(&app)
        .await
        .inspect_err(|error| coordinator.fail(error))?
    else {
        let error = "the approved update is no longer offered".to_owned();
        coordinator.fail(&error);
        return Err(error);
    };
    if update.version != expected_version {
        let error = format!(
            "the offered update changed from {expected_version} to {}; check again before installing",
            update.version
        );
        coordinator.fail(&error);
        return Err(error);
    }

    {
        let mut status = coordinator.status.lock().expect("update status mutex");
        status.phase = "downloading".into();
        status.available = Some(metadata(&update));
    }
    let progress = Arc::clone(coordinator.inner());
    let finished = Arc::clone(coordinator.inner());
    let bytes = update
        .download(
            move |chunk, total| {
                let mut status = progress.status.lock().expect("update status mutex");
                status.downloaded_bytes = status.downloaded_bytes.saturating_add(chunk as u64);
                status.total_bytes = total;
            },
            move || {
                finished.status.lock().expect("update status mutex").phase = "verified".into();
            },
        )
        .await
        .map_err(|error| {
            let error = error.to_string();
            coordinator.fail(&error);
            error
        })?;

    // Do not stop the application until the complete artifact has passed
    // Tauri's mandatory updater signature verification.
    coordinator
        .status
        .lock()
        .expect("update status mutex")
        .phase = "stopping".into();
    let Some(state) = app.try_state::<Arc<App>>() else {
        let error = "application data is not open; update was not installed".to_owned();
        coordinator.fail(&error);
        return Err(error);
    };
    state.shutdown();

    if let Err(error) = update.install(&bytes) {
        tracing::error!(%error, "verified update could not be installed; restarting current build");
        drop(operation);
        app.restart();
    }
    coordinator
        .status
        .lock()
        .expect("update status mutex")
        .phase = "restarting".into();
    drop(operation);
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_build_stays_fail_closed() {
        if updates_enabled() {
            return;
        }
        let coordinator = UpdateCoordinator::default();
        assert_eq!(coordinator.status.lock().unwrap().phase, "disabled",);
        assert!(coordinator.begin("checking").is_err());
    }

    #[test]
    fn only_one_update_operation_is_admitted() {
        if !updates_enabled() {
            return;
        }
        let coordinator = UpdateCoordinator::default();
        let operation = coordinator.begin("checking").unwrap();
        assert!(coordinator.begin("checking").is_err());
        drop(operation);
        assert!(coordinator.begin("checking").is_ok());
    }
}
