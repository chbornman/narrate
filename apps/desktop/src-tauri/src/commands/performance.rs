//! IPC intake/snapshot for local structured performance evidence.

use std::sync::Arc;

use tauri::State;

use crate::performance::{
    PerformanceIngestReport, PerformanceMonitor, PerformanceSampleInput, PerformanceSnapshot,
};

#[tauri::command]
pub async fn performance_ingest(
    monitor: State<'_, Arc<PerformanceMonitor>>,
    samples: Vec<PerformanceSampleInput>,
) -> Result<PerformanceIngestReport, String> {
    let monitor = Arc::clone(monitor.inner());
    tauri::async_runtime::spawn_blocking(move || monitor.ingest_frontend(samples))
        .await
        .map_err(|error| format!("performance intake worker failed: {error}"))?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn performance_snapshot(
    monitor: State<'_, Arc<PerformanceMonitor>>,
) -> Result<PerformanceSnapshot, String> {
    let monitor = Arc::clone(monitor.inner());
    tauri::async_runtime::spawn_blocking(move || monitor.snapshot())
        .await
        .map_err(|error| format!("performance snapshot worker failed: {error}"))
}
