//! Headless acceptance probe executed from the installed application binary.
//!
//! The package harness invokes `photoproof --installed-smoke <dir>` from an
//! extracted native bundle. This proves the executable can open a brand-new
//! data directory, migrate its database, see its bundled ASR child beside
//! itself, expose the model-free degraded baseline, and use normal shutdown
//! without relying on the source workspace.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::lifecycle::LifecyclePhase;
use crate::state::App;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledSmokeReceipt {
    version: &'static str,
    phase_before_shutdown: String,
    phase_after_shutdown: String,
    database_user_version: i64,
    init_to_usable_ms: u64,
    shutdown_ms: u64,
    subsystem_health: Vec<String>,
    sidecar_path: String,
    sidecar_bytes: u64,
    model_count: usize,
    asr_ready: bool,
    llm_ready: bool,
    backup_helper_files: usize,
    restore_rollback_retained: bool,
}

pub fn run(app_data: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(app_data).map_err(|error| error.to_string())?;
    let sidecar = crate::supervisors::asr_binary().ok_or_else(|| {
        "the packaged pp-asr-server is not beside the installed app executable".to_owned()
    })?;
    let sidecar_bytes = std::fs::metadata(&sidecar)
        .map_err(|error| format!("could not inspect {}: {error}", sidecar.display()))?
        .len();
    if sidecar_bytes < 1_000_000 {
        return Err(format!(
            "packaged pp-asr-server is implausibly small ({sidecar_bytes} bytes)"
        ));
    }

    let init_started = Instant::now();
    let app = App::init_with_diagnostics(app_data.to_path_buf(), None, None)
        .map_err(|error| error.to_string())?;
    let init_to_usable_ms = init_started.elapsed().as_millis() as u64;
    let lifecycle_before = app.lifecycle.snapshot();
    let phase_before = lifecycle_before.phase;
    if phase_before != LifecyclePhase::Usable {
        return Err(format!(
            "fresh installed app did not reach Usable (phase {phase_before:?})"
        ));
    }
    let runtime = app.runtime.status();
    if runtime
        .models
        .iter()
        .any(|model| model.state == "installed")
    {
        return Err("fresh smoke directory unexpectedly contains installed models".into());
    }

    let shutdown_started = Instant::now();
    app.shutdown();
    let shutdown_ms = shutdown_started.elapsed().as_millis() as u64;
    let phase_after = app.lifecycle.snapshot().phase;
    if phase_after != LifecyclePhase::Stopping {
        return Err(format!(
            "installed app did not enter Stopping (phase {phase_after:?})"
        ));
    }
    // Drop every database/filesystem handle before exercising the packaged
    // offline helper, exactly mirroring the production pipe-EOF boundary.
    drop(app);

    let backup_path = app_data.with_file_name(format!(
        "{}.installed-smoke.ppbackup",
        app_data
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("app-data")
    ));
    crate::backup::run_packaged_helper_smoke(&crate::backup::OfflineOperation::Backup {
        app_data: app_data.to_path_buf(),
        destination: backup_path.clone(),
    })
    .map_err(|error| format!("packaged backup helper: {error}"))?;
    let backup_manifest = crate::backup::verify_offline_backup(&backup_path)
        .map_err(|error| format!("verify packaged helper backup: {error}"))?;
    crate::backup::run_packaged_helper_smoke(&crate::backup::OfflineOperation::Restore {
        app_data: app_data.to_path_buf(),
        backup: backup_path,
    })
    .map_err(|error| format!("packaged restore helper: {error}"))?;
    let restore_receipt = crate::backup::read_operation_receipt(app_data)
        .map_err(|error| format!("read packaged restore receipt: {error}"))?
        .ok_or_else(|| "packaged restore helper did not write a receipt".to_owned())?;
    if !restore_receipt.succeeded || restore_receipt.operation != "restore" {
        return Err(format!(
            "packaged restore helper reported failure: {}",
            restore_receipt.detail
        ));
    }
    let rollback = restore_receipt
        .rollback_path
        .as_ref()
        .ok_or_else(|| "packaged restore did not retain rollback app data".to_owned())?;
    if !rollback.is_dir() {
        return Err(format!(
            "packaged restore rollback directory is absent: {}",
            rollback.display()
        ));
    }

    let database_user_version = rusqlite::Connection::open(app_data.join("photoproof.db"))
        .and_then(|connection| {
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        })
        .map_err(|error| format!("could not verify installed database: {error}"))?;
    let receipt = InstalledSmokeReceipt {
        version: env!("CARGO_PKG_VERSION"),
        phase_before_shutdown: format!("{phase_before:?}"),
        phase_after_shutdown: format!("{phase_after:?}"),
        database_user_version,
        init_to_usable_ms,
        shutdown_ms,
        subsystem_health: lifecycle_before
            .health
            .into_iter()
            .map(|(subsystem, health)| format!("{subsystem:?}:{health:?}"))
            .collect(),
        sidecar_path: sidecar.display().to_string(),
        sidecar_bytes,
        model_count: runtime.models.len(),
        asr_ready: runtime.asr_ready,
        llm_ready: runtime.llm_ready,
        backup_helper_files: backup_manifest.files.len(),
        restore_rollback_retained: true,
    };
    let receipt_path = app_data.join("installed-smoke.json");
    let bytes = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
    std::fs::write(&receipt_path, bytes).map_err(|error| error.to_string())?;
    Ok(receipt_path)
}
