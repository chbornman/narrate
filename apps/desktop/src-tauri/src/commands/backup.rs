//! Installed-shell full backup and restore handoff.
//!
//! The commands only validate and arm a private helper. They never copy or
//! replace app data in the live process. `backup::spawn_offline_helper` keeps
//! that helper behind an inherited-pipe EOF until this process has actually
//! exited and all SQLite/WAL handles are gone.

use std::path::PathBuf;

use tauri::AppHandle;

use super::{S, run_blocking};
use crate::backup::{OfflineOperation, OperationReceipt};
use crate::command_work::CommandClass;
use crate::error::{CmdError, CmdResult};

#[tauri::command]
pub fn backup_operation_status(app: S<'_>) -> CmdResult<Option<OperationReceipt>> {
    let app = app.inner().clone();
    let _permit = super::admit(&app, "backup.status", CommandClass::Read)?;
    crate::backup::read_operation_receipt(&app.app_data)
        .map_err(|error| CmdError::Invalid(error.to_string()))
}

#[tauri::command]
pub async fn backup_and_quit(app: S<'_>, handle: AppHandle, destination: String) -> CmdResult<()> {
    let app = app.inner().clone();
    let operation = OfflineOperation::Backup {
        app_data: app.app_data.clone(),
        destination: PathBuf::from(destination),
    };
    run_blocking(app, "backup.arm", CommandClass::Mutation, move |_| {
        crate::backup::spawn_offline_helper(&operation)
            .map_err(|error| CmdError::Invalid(error.to_string()))
    })
    .await?;
    // ExitRequested performs the managed-task and command admission barrier.
    // The helper remains blocked until OS process teardown closes its pipe.
    handle.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn restore_and_restart(app: S<'_>, handle: AppHandle, backup: String) -> CmdResult<()> {
    let app = app.inner().clone();
    let operation = OfflineOperation::Restore {
        app_data: app.app_data.clone(),
        backup: PathBuf::from(backup),
    };
    run_blocking(
        app,
        "backup.restore-arm",
        CommandClass::Mutation,
        move |_| {
            crate::backup::spawn_offline_helper(&operation)
                .map_err(|error| CmdError::Invalid(error.to_string()))
        },
    )
    .await?;
    handle.exit(0);
    Ok(())
}
