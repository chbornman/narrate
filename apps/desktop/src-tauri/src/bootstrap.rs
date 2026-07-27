//! Minimal shell bootstrap truth that exists even when full `App` creation
//! fails.
//!
//! Most commands require `State<Arc<App>>`, which intentionally does not exist
//! after a fatal database/open/migration error. This separate tiny state is
//! managed before `App::init`; its command lets the already-created webview
//! render the real blocking error and a relaunch/recovery surface instead of
//! Tauri setup aborting and closing the window.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStatus {
    pub state: &'static str,
    pub error: Option<String>,
    pub recovery_action: Option<&'static str>,
}

pub struct BootstrapState {
    status: Mutex<BootstrapStatus>,
}

impl Default for BootstrapState {
    fn default() -> Self {
        Self {
            status: Mutex::new(BootstrapStatus {
                state: "opening",
                error: None,
                recovery_action: None,
            }),
        }
    }
}

impl BootstrapState {
    pub fn ready(&self) {
        *self.status.lock().expect("bootstrap mutex") = BootstrapStatus {
            state: "ready",
            error: None,
            recovery_action: None,
        };
    }

    pub fn fatal(&self, error: impl Into<String>, recovery_action: Option<&'static str>) {
        *self.status.lock().expect("bootstrap mutex") = BootstrapStatus {
            state: "fatal",
            error: Some(error.into()),
            recovery_action,
        };
    }

    pub fn snapshot(&self) -> BootstrapStatus {
        self.status.lock().expect("bootstrap mutex").clone()
    }
}

#[tauri::command]
pub fn bootstrap_status(state: State<'_, Arc<BootstrapState>>) -> BootstrapStatus {
    state.snapshot()
}

/// Fatal App construction cannot be retried through App-owned commands because
/// that state was intentionally never published. Request a clean process
/// restart instead of presenting a button that can only poll the same fatal
/// snapshot forever.
#[tauri::command]
pub fn bootstrap_relaunch(state: State<'_, Arc<BootstrapState>>, app: AppHandle) -> bool {
    if state.snapshot().state != "fatal" {
        return false;
    }
    app.request_restart();
    true
}

/// Recover a fail-closed device identity before full `App` state exists.
/// Other fatal startup classes deliberately cannot call this command.
#[tauri::command]
pub fn bootstrap_reset_device_identity(
    state: State<'_, Arc<BootstrapState>>,
    app: AppHandle,
) -> Result<bool, String> {
    let status = state.snapshot();
    if status.state != "fatal" || status.recovery_action != Some("reset-device-identity") {
        return Ok(false);
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    crate::settings::reset_device_identity(&app_data).map_err(|error| error.to_string())?;
    app.request_restart();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_open_error_remains_queryable_without_an_app() {
        let state = BootstrapState::default();
        assert_eq!(state.snapshot().state, "opening");
        state.fatal("database is newer than this application", None);
        assert_eq!(
            state.snapshot(),
            BootstrapStatus {
                state: "fatal",
                error: Some("database is newer than this application".into()),
                recovery_action: None,
            }
        );
    }

    #[test]
    fn fatal_identity_error_exposes_only_the_explicit_reset_action() {
        let state = BootstrapState::default();
        state.fatal(
            "device identity unavailable: both copies are corrupt",
            Some("reset-device-identity"),
        );
        assert_eq!(
            state.snapshot().recovery_action,
            Some("reset-device-identity")
        );
    }
}
