//! One backend-originated application-health snapshot.
//!
//! This is intentionally an aggregation command, not another owner of state:
//! lifecycle, tasks, roots/volumes/watchers, ingest, and runtime remain in
//! their existing authorities. The UI consumes this joined snapshot so it
//! cannot invent a contradictory loading/degraded story.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use super::S;
use crate::command_work::{CommandClass, CommandWorkSnapshot};
use crate::dto::{IngestStatus, RuntimeStatus};
use crate::error::CmdResult;
use crate::lifecycle::{LifecyclePhase, LifecycleSnapshot, Subsystem, SubsystemHealth};
use crate::managed_tasks::{TaskPriority, TaskSnapshot, TaskState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationHealth {
    pub observed_at_ms: u64,
    pub phase: &'static str,
    /// One actionable projection for product UI. The detailed snapshots below
    /// remain diagnostic truth; consumers render this list instead of each
    /// inventing severity and recovery semantics independently.
    pub issues: Vec<HealthIssue>,
    pub phase_timings: Vec<PhaseTimingStatus>,
    pub subsystems: Vec<SubsystemStatus>,
    pub database: DatabaseStatus,
    pub volumes: Vec<VolumeStatus>,
    pub volume_inventory_error: Option<String>,
    pub roots: Vec<RootStatus>,
    pub tasks: Vec<BackgroundTaskStatus>,
    pub command_work: Vec<CommandWorkStatus>,
    pub control_files: Vec<ShellControlFileStatus>,
    pub disk: crate::disk::DiskHealthSnapshot,
    pub resources: crate::resource_governor::ResourceStatus,
    pub diagnostics: DiagnosticsStatus,
    pub repair_integrity: crate::doctor::RepairIntegritySnapshot,
    pub performance: PerformanceHealth,
    pub ingest: IngestStatus,
    pub runtime: RuntimeStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStatus {
    pub state: &'static str,
    pub schema_version: Option<i64>,
    pub expected_schema_version: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeStatus {
    pub volume_id: String,
    pub label: String,
    pub online: bool,
    pub read_only: bool,
    pub fs_type: Option<String>,
    pub mount_point: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthIssue {
    pub id: String,
    pub subsystem: &'static str,
    pub title: String,
    /// Blocking means recovery needs intervention (for example a held WAL or
    /// unavailable startup store). Degraded means the shell remains usable,
    /// but a capability or derived artifact is hurt.
    pub blocking: bool,
    pub summary: String,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<u64>,
    pub action: HealthAction,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthAction {
    /// Closed vocabulary interpreted by Settings. Every emitted kind maps to
    /// a currently registered, idempotent/safe command.
    pub kind: &'static str,
    pub label: &'static str,
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceHealth {
    pub journeys: crate::performance::PerformanceSnapshot,
    pub ingest_stages: Vec<IngestStagePerformance>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestStagePerformance {
    pub stage: &'static str,
    pub count: u64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseTimingStatus {
    pub phase: &'static str,
    pub entered_at_ms: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubsystemStatus {
    pub name: &'static str,
    pub state: &'static str,
    pub blocking: bool,
    pub summary: Option<String>,
    pub action: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootStatus {
    pub root_id: String,
    pub display_name: String,
    pub volume_id: String,
    pub online: bool,
    pub watcher_active: bool,
    pub lifecycle_state: String,
    pub state: &'static str,
    pub summary: Option<String>,
    pub action: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskStatus {
    pub owner: String,
    pub key: String,
    pub priority: &'static str,
    pub state: &'static str,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub progress: Option<f32>,
    pub progress_message: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandWorkStatus {
    pub id: u64,
    pub name: &'static str,
    pub class: &'static str,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsStatus {
    pub build_version: &'static str,
    pub previous_unclean_launch: bool,
    pub logs_dir: Option<String>,
    pub current_log: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellControlFileStatus {
    pub name: &'static str,
    pub state: &'static str,
    pub source: Option<&'static str>,
    pub quarantined: Vec<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub action: Option<&'static str>,
}

#[tauri::command]
pub fn application_health(
    app: S<'_>,
    performance: State<'_, Arc<crate::performance::PerformanceMonitor>>,
) -> CmdResult<ApplicationHealth> {
    let app = app.inner().clone();
    let _permit = super::admit(&app, "health.application", CommandClass::Read)?;
    Ok(build_application_health(&app, performance.snapshot()))
}

/// Re-run the managed integrity sweep after a degraded retained repair result.
/// The task registry supplies single-flight, progress, cancellation, and
/// shutdown ownership; the command only acknowledges successful admission.
#[tauri::command]
pub fn retry_integrity_repair(app: S<'_>) -> CmdResult<()> {
    let app = app.inner().clone();
    let _permit = super::admit(&app, "health.retry-integrity", CommandClass::Mutation)?;
    app.start_startup_doctor()
        .map_err(|error| crate::error::CmdError::Unavailable(error.to_string()))
}

fn build_application_health(
    app: &crate::state::App,
    journeys: crate::performance::PerformanceSnapshot,
) -> ApplicationHealth {
    let lifecycle = app.lifecycle.snapshot();
    let roots: Vec<RootStatus> = match app.library.roots() {
        Ok(roots) => roots
            .into_iter()
            .map(|root| {
                let display_name = root.display_name.clone().unwrap_or_else(|| {
                    root.rel_path
                        .rsplit('/')
                        .next()
                        .filter(|name| !name.is_empty())
                        .unwrap_or("Volume root")
                        .to_owned()
                });
                let volume = app.library.volume(&root.volume_id);
                let (online, summary) = match volume {
                    Ok(Some(volume)) if volume.online => (true, None),
                    Ok(Some(_)) => (false, Some("volume is offline".to_owned())),
                    Ok(None) => (false, Some("volume record is missing".to_owned())),
                    Err(error) => (false, Some(format!("volume lookup failed: {error}"))),
                };
                let watcher_active = app
                    .watchers
                    .lock()
                    .expect("watchers mutex")
                    .get(&root.root_id)
                    .is_some_and(|watcher| watcher.is_active());
                let state = if root.state != "active" {
                    "archived"
                } else if !online {
                    "unavailable"
                } else if !watcher_active {
                    "degraded"
                } else {
                    "healthy"
                };
                let summary = summary.or_else(|| {
                    (!watcher_active).then(|| "filesystem watcher is not active".to_owned())
                });
                RootStatus {
                    root_id: root.root_id,
                    display_name,
                    volume_id: root.volume_id,
                    online,
                    watcher_active,
                    lifecycle_state: root.state,
                    state,
                    summary,
                    action: (!matches!(state, "healthy" | "archived")).then_some("retry-root"),
                }
            })
            .collect(),
        Err(error) => vec![RootStatus {
            root_id: String::new(),
            display_name: "Library roots".into(),
            volume_id: String::new(),
            online: false,
            watcher_active: false,
            lifecycle_state: "unknown".into(),
            state: "unavailable",
            summary: Some(format!("root inventory failed: {error}")),
            action: Some("retry-roots"),
        }],
    };

    let observed_at_ms = system_time_ms(SystemTime::now());
    let database = match app.store.schema_status() {
        Ok((actual, expected)) => DatabaseStatus {
            state: if actual == expected {
                "healthy"
            } else {
                "unavailable"
            },
            schema_version: Some(actual),
            expected_schema_version: expected,
            error: (actual != expected)
                .then(|| format!("database schema is {actual}, expected {expected}")),
        },
        Err(error) => DatabaseStatus {
            state: "unavailable",
            schema_version: None,
            expected_schema_version: photoproof_core::store::EventStore::expected_schema_version(),
            error: Some(error.to_string()),
        },
    };
    let (volumes, volume_inventory_error) = match app.library.volumes() {
        Ok(volumes) => (
            volumes
                .into_iter()
                .map(|volume| VolumeStatus {
                    label: volume
                        .label
                        .clone()
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| volume.volume_id.clone()),
                    volume_id: volume.volume_id,
                    online: volume.online,
                    read_only: volume.read_only,
                    fs_type: volume.fs_type,
                    mount_point: volume.mount_point,
                })
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let subsystems = subsystem_statuses(&lifecycle);
    let runtime = app.runtime.status();
    let mut tasks = app
        .tasks
        .snapshots()
        .into_iter()
        .map(task_status)
        .collect::<Vec<_>>();
    tasks.extend(runtime_model_build_tasks(&runtime, observed_at_ms));
    let control_files = vec![
        shell_control_status("settings", &app.settings_recovery),
        ShellControlFileStatus {
            name: "device-identity",
            state: recovery_state(&app.device_identity_recovery),
            source: Some(recovery_source(&app.device_identity_recovery)),
            quarantined: app
                .device_identity_recovery
                .quarantined
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            warnings: app
                .device_identity_recovery
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect(),
            error: None,
            action: (!app.device_identity_recovery.warnings.is_empty()
                || !app.device_identity_recovery.quarantined.is_empty())
            .then_some("reveal-logs"),
        },
        tuning_control_status(&app.tuning_recovery),
    ];
    let disk = app.disk.snapshot();
    let diagnostics = DiagnosticsStatus {
        build_version: env!("CARGO_PKG_VERSION"),
        previous_unclean_launch: app
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.previous_unclean_launch),
        logs_dir: app
            .diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.logs_dir.display().to_string()),
        current_log: app
            .diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.current_log.display().to_string()),
        error: app.diagnostics_error.clone(),
    };
    let repair_integrity = app
        .repair_integrity
        .lock()
        .expect("repair integrity mutex")
        .clone();
    let issues = application_issues(IssueInputs {
        observed_at_ms,
        database: &database,
        volume_inventory_error: volume_inventory_error.as_deref(),
        subsystems: &subsystems,
        roots: &roots,
        tasks: &tasks,
        control_files: &control_files,
        disk: &disk,
        runtime: &runtime,
        diagnostics: &diagnostics,
        repair_integrity: &repair_integrity,
    });

    ApplicationHealth {
        observed_at_ms,
        phase: phase_name(lifecycle.phase),
        issues,
        phase_timings: lifecycle
            .phase_history
            .iter()
            .map(|timing| PhaseTimingStatus {
                phase: phase_name(timing.phase),
                entered_at_ms: system_time_ms(timing.entered_at),
                elapsed_ms: timing.elapsed_ms,
            })
            .collect(),
        subsystems,
        database,
        volumes,
        volume_inventory_error,
        roots,
        tasks,
        command_work: app
            .command_work
            .snapshots()
            .into_iter()
            .filter(|work| work.name != "health.application")
            .map(command_work_status)
            .collect(),
        control_files,
        diagnostics,
        repair_integrity,
        performance: PerformanceHealth {
            journeys,
            ingest_stages: app
                .library
                .metrics_snapshot()
                .into_iter()
                .map(|stage| IngestStagePerformance {
                    stage: stage.stage,
                    count: stage.count,
                    total_ms: stage.total_ms,
                    mean_ms: stage.mean_ms,
                    p50_ms: stage.p50_ms,
                    p95_ms: stage.p95_ms,
                    p99_ms: stage.p99_ms,
                    max_ms: stage.max_ms,
                })
                .collect(),
        },
        disk,
        resources: app.resources.snapshot(),
        ingest: crate::pump::ingest_status(app),
        runtime,
    }
}

fn runtime_model_build_tasks(
    runtime: &RuntimeStatus,
    observed_at_ms: u64,
) -> Vec<BackgroundTaskStatus> {
    [
        ("clip", &runtime.clip),
        ("text-embedder", &runtime.text_embedder),
    ]
    .into_iter()
    .filter(|(_, slot)| {
        matches!(
            slot.state,
            crate::dto::EmbedderState::Queued | crate::dto::EmbedderState::Building
        )
    })
    .map(|(role, slot)| BackgroundTaskStatus {
        owner: "runtime".into(),
        key: format!("model-build:{role}"),
        priority: "background",
        state: "running",
        // The richer RuntimeStatus retains the RFC 3339 dispatch time. This
        // generic task DTO is milliseconds-only and the desktop crate avoids a
        // second time parser solely for projection, so use observation time.
        started_at_ms: observed_at_ms,
        ended_at_ms: None,
        progress: None,
        progress_message: Some(match &slot.model_id {
            Some(model_id) => format!("Loading {model_id} in the isolated helper"),
            None => format!("Loading the {role} model in the isolated helper"),
        }),
        last_error: None,
    })
    .collect()
}

fn command_work_status(work: CommandWorkSnapshot) -> CommandWorkStatus {
    CommandWorkStatus {
        id: work.id,
        name: work.name,
        class: match work.class {
            CommandClass::Read => "read",
            CommandClass::Mutation => "mutation",
        },
        started_at_ms: system_time_ms(work.started_at),
    }
}

struct IssueInputs<'a> {
    observed_at_ms: u64,
    database: &'a DatabaseStatus,
    volume_inventory_error: Option<&'a str>,
    subsystems: &'a [SubsystemStatus],
    roots: &'a [RootStatus],
    tasks: &'a [BackgroundTaskStatus],
    control_files: &'a [ShellControlFileStatus],
    disk: &'a crate::disk::DiskHealthSnapshot,
    runtime: &'a RuntimeStatus,
    diagnostics: &'a DiagnosticsStatus,
    repair_integrity: &'a crate::doctor::RepairIntegritySnapshot,
}

fn application_issues(input: IssueInputs<'_>) -> Vec<HealthIssue> {
    let IssueInputs {
        observed_at_ms,
        database,
        volume_inventory_error,
        subsystems,
        roots,
        tasks,
        control_files,
        disk,
        runtime,
        diagnostics,
        repair_integrity,
    } = input;
    let mut issues = Vec::new();

    if let Some(issue) = database_issue(observed_at_ms, database) {
        issues.push(issue);
    }

    if let Some(error) = volume_inventory_error {
        issues.push(HealthIssue {
            id: "volumes:inventory".into(),
            subsystem: "volumes",
            title: "Volume inventory is unavailable".into(),
            blocking: false,
            summary: error.to_owned(),
            last_error: Some(error.to_owned()),
            last_error_at_ms: Some(observed_at_ms),
            action: HealthAction {
                kind: "retry-roots",
                label: "Retry folders",
                target_id: None,
            },
        });
    }

    for subsystem in subsystems.iter().filter(|status| {
        status.state != "healthy"
            && status.state != "unknown"
            && !matches!(status.name, "roots" | "watchers" | "runtime")
    }) {
        issues.push(HealthIssue {
            id: format!("subsystem:{}", subsystem.name),
            subsystem: subsystem.name,
            title: format!("{} is {}", subsystem.name, subsystem.state),
            blocking: subsystem.blocking,
            summary: subsystem
                .summary
                .clone()
                .unwrap_or_else(|| format!("{} needs attention", subsystem.name)),
            last_error: subsystem.summary.clone(),
            last_error_at_ms: Some(observed_at_ms),
            action: subsystem_health_action(subsystem.name),
        });
    }

    for root in roots
        .iter()
        .filter(|root| !matches!(root.state, "healthy" | "archived"))
    {
        issues.push(HealthIssue {
            id: format!("root:{}", root.root_id),
            subsystem: "roots",
            title: format!("{} is unavailable", root.display_name),
            blocking: false,
            summary: root
                .summary
                .clone()
                .unwrap_or_else(|| "The watched folder needs to be reconnected.".into()),
            last_error: root.summary.clone(),
            last_error_at_ms: Some(observed_at_ms),
            action: HealthAction {
                kind: if root.root_id.is_empty() {
                    "retry-roots"
                } else {
                    "retry-root"
                },
                label: "Retry folder",
                target_id: (!root.root_id.is_empty()).then(|| root.root_id.clone()),
            },
        });
    }

    for task in tasks
        .iter()
        .filter(|task| task.state == "failed" && task.last_error.is_some())
    {
        issues.push(HealthIssue {
            id: format!("task:{}:{}", task.owner, task.key),
            subsystem: "background-work",
            title: format!("{} failed", task.key),
            blocking: false,
            summary: task
                .last_error
                .clone()
                .unwrap_or_else(|| "Background work failed.".into()),
            last_error: task.last_error.clone(),
            last_error_at_ms: task.ended_at_ms,
            action: reveal_logs_action(),
        });
    }

    for control in control_files
        .iter()
        .filter(|control| control.state != "healthy")
    {
        let summary = control
            .error
            .clone()
            .or_else(|| control.warnings.first().cloned())
            .unwrap_or_else(|| {
                if control.quarantined.is_empty() {
                    "The control file was recovered with safe defaults.".into()
                } else {
                    "A damaged control file was quarantined.".into()
                }
            });
        issues.push(HealthIssue {
            id: format!("control:{}", control.name),
            subsystem: "configuration",
            title: format!("{} configuration was recovered", control.name),
            blocking: false,
            summary: summary.clone(),
            last_error: control.error.clone().or(Some(summary)),
            last_error_at_ms: Some(observed_at_ms),
            action: restore_defaults_action(control.name),
        });
    }

    disk_issues(observed_at_ms, disk, &mut issues);
    runtime_issues(observed_at_ms, runtime, &mut issues);

    if let Some(error) = diagnostics.error.clone() {
        issues.push(HealthIssue {
            id: "diagnostics:logging".into(),
            subsystem: "diagnostics",
            title: "Diagnostic logging is unavailable".into(),
            blocking: false,
            summary: error.clone(),
            last_error: Some(error),
            last_error_at_ms: Some(observed_at_ms),
            action: reveal_logs_action(),
        });
    }

    if matches!(repair_integrity.state, "degraded" | "cancelled")
        || !repair_integrity.errors.is_empty()
    {
        let summary = repair_integrity
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| format!("Integrity repair ended {}.", repair_integrity.state));
        issues.push(HealthIssue {
            id: "repair:integrity".into(),
            subsystem: "repair",
            title: "Library integrity repair needs attention".into(),
            blocking: false,
            summary: summary.clone(),
            last_error: Some(summary),
            last_error_at_ms: repair_integrity.completed_at_ms.or(Some(observed_at_ms)),
            action: HealthAction {
                kind: "retry-repair",
                label: "Retry repair",
                target_id: None,
            },
        });
    }

    issues
}

fn database_issue(observed_at_ms: u64, database: &DatabaseStatus) -> Option<HealthIssue> {
    if database.state == "healthy" {
        return None;
    }
    let summary = database.error.clone().unwrap_or_else(|| {
        format!(
            "Database schema {} does not match the expected schema {}.",
            database
                .schema_version
                .map_or_else(|| "unknown".into(), |version| version.to_string()),
            database.expected_schema_version
        )
    });
    Some(HealthIssue {
        id: "database:schema".into(),
        subsystem: "database",
        title: "Database schema is unavailable".into(),
        blocking: true,
        summary: summary.clone(),
        last_error: Some(summary),
        last_error_at_ms: Some(observed_at_ms),
        action: reveal_logs_action(),
    })
}

fn disk_issues(
    observed_at_ms: u64,
    disk: &crate::disk::DiskHealthSnapshot,
    issues: &mut Vec<HealthIssue>,
) {
    use crate::disk::{CapacityState, WalState};

    for (id, title, state, blocking) in [
        (
            "app-data",
            "Application-data disk space",
            disk.app_data_state,
            disk.app_data_state == CapacityState::Critical,
        ),
        ("models", "Model disk space", disk.models_state, false),
    ] {
        if state != CapacityState::Healthy {
            let available = disk
                .stores
                .iter()
                .find(|store| {
                    if id == "models" {
                        store.name == "models"
                    } else {
                        store.name == "database-and-wal"
                    }
                })
                .and_then(|store| store.available_bytes);
            issues.push(HealthIssue {
                id: format!("disk:{id}"),
                subsystem: "disk",
                title: title.into(),
                blocking,
                summary: match available {
                    Some(bytes) => format!(
                        "{} bytes remain; derived work {}.",
                        bytes,
                        if disk.derived_work_paused {
                            "is paused"
                        } else {
                            "remains admitted"
                        }
                    ),
                    None => "Free space could not be measured.".into(),
                },
                last_error: None,
                last_error_at_ms: Some(observed_at_ms),
                action: reveal_logs_action(),
            });
        }
    }

    let wal = &disk.wal;
    if wal.state != WalState::Healthy {
        let (blocking, title, summary) = match wal.state {
            WalState::Blocked => (
                true,
                "Database maintenance is blocked",
                "A reader is holding the SQLite WAL open. Close other Photoproof windows, then relaunch if it does not clear.".into(),
            ),
            WalState::Critical => (
                false,
                "Database WAL needs attention",
                format!(
                    "The WAL is critically large or old ({} bytes, age {} ms).",
                    wal.size_bytes.unwrap_or(0),
                    wal.age_ms.unwrap_or(0)
                ),
            ),
            WalState::Warning => (
                false,
                "Database WAL maintenance is overdue",
                format!(
                    "The WAL is larger or older than its maintenance threshold ({} bytes, age {} ms).",
                    wal.size_bytes.unwrap_or(0),
                    wal.age_ms.unwrap_or(0)
                ),
            ),
            WalState::Unknown => (
                false,
                "Database WAL could not be inspected",
                wal.inventory_error
                    .clone()
                    .unwrap_or_else(|| "WAL inventory is unavailable.".into()),
            ),
            WalState::Healthy => unreachable!(),
        };
        issues.push(HealthIssue {
            id: "disk:wal".into(),
            subsystem: "database",
            title: title.into(),
            blocking,
            summary,
            last_error: wal
                .last_maintenance_error
                .clone()
                .or_else(|| wal.inventory_error.clone()),
            last_error_at_ms: wal.last_maintenance_failure_at_ms.or(Some(observed_at_ms)),
            action: reveal_logs_action(),
        });
    }

    for store in disk
        .stores
        .iter()
        .filter(|store| store.inventory_errors > 0)
    {
        issues.push(HealthIssue {
            id: format!("disk-inventory:{}", store.name),
            subsystem: "cache",
            title: format!("{} inventory is incomplete", store.name),
            blocking: false,
            summary: format!(
                "{} paths could not be inspected; reported usage is a lower bound.",
                store.inventory_errors
            ),
            last_error: None,
            last_error_at_ms: Some(observed_at_ms),
            action: if store.name == "previews" || store.name == "full-decode-cache" {
                HealthAction {
                    kind: "rebuild-previews",
                    label: "Rebuild previews",
                    target_id: None,
                }
            } else {
                reveal_logs_action()
            },
        });
    }
}

fn runtime_issues(observed_at_ms: u64, runtime: &RuntimeStatus, issues: &mut Vec<HealthIssue>) {
    for (id, title, error) in [
        (
            "asr",
            "Speech model runtime is blocked",
            &runtime.asr_blocked,
        ),
        (
            "llm",
            "Language model runtime is blocked",
            &runtime.llm_blocked,
        ),
    ] {
        if let Some(error) = error {
            issues.push(HealthIssue {
                id: format!("runtime:{id}"),
                subsystem: "runtime",
                title: title.into(),
                blocking: false,
                summary: error.clone(),
                last_error: Some(error.clone()),
                last_error_at_ms: Some(observed_at_ms),
                action: HealthAction {
                    kind: "retry-runtime",
                    label: "Restart runtime",
                    target_id: None,
                },
            });
        }
    }

    for (id, title, slot) in [
        ("clip", "Image search model failed", &runtime.clip),
        (
            "text-embedder",
            "Text search model failed",
            &runtime.text_embedder,
        ),
    ] {
        if slot.state == crate::dto::EmbedderState::Failed {
            issues.push(HealthIssue {
                id: format!("runtime:{id}"),
                subsystem: "runtime",
                title: title.into(),
                blocking: false,
                summary: slot
                    .error
                    .clone()
                    .unwrap_or_else(|| "The isolated model helper failed.".into()),
                last_error: slot.error.clone(),
                last_error_at_ms: Some(observed_at_ms),
                action: HealthAction {
                    kind: "retry-runtime",
                    label: "Restart runtime",
                    target_id: None,
                },
            });
        }
    }

    if runtime.capability_state == "failed" {
        issues.push(HealthIssue {
            id: "runtime:hardware-detection".into(),
            subsystem: "runtime",
            title: "Hardware detection failed".into(),
            blocking: false,
            summary: runtime
                .capability_summary
                .clone()
                .unwrap_or_else(|| "The safe CPU runtime remains available.".into()),
            last_error: runtime.capability_summary.clone(),
            last_error_at_ms: Some(observed_at_ms),
            action: HealthAction {
                kind: "redetect-runtime",
                label: "Re-detect hardware",
                target_id: None,
            },
        });
    }

    for model in runtime
        .models
        .iter()
        .filter(|model| model.error.is_some() || model.registry_error.is_some())
    {
        let error = model
            .registry_error
            .clone()
            .or_else(|| model.error.clone())
            .expect("filtered model error");
        issues.push(HealthIssue {
            id: format!("model:{}", model.id),
            subsystem: "models",
            title: format!("{} needs verification", model.id),
            blocking: false,
            summary: error.clone(),
            last_error: Some(error),
            last_error_at_ms: Some(observed_at_ms),
            action: HealthAction {
                kind: "verify-model",
                label: "Verify model",
                target_id: Some(model.id.clone()),
            },
        });
    }

    for control in runtime.control_files.iter().filter(|control| {
        !control.errors.is_empty()
            || !control.validation_warnings.is_empty()
            || control.recovery.as_ref().is_some_and(|recovery| {
                !recovery.quarantined.is_empty() || !recovery.warnings.is_empty()
            })
    }) {
        let error = control
            .errors
            .first()
            .map(|error| error.detail.clone())
            .or_else(|| control.validation_warnings.first().cloned())
            .unwrap_or_else(|| "Runtime configuration was recovered.".into());
        issues.push(HealthIssue {
            id: format!("runtime-control:{}", control.name),
            subsystem: "configuration",
            title: format!("{} runtime configuration needs attention", control.name),
            blocking: false,
            summary: error.clone(),
            last_error: Some(error),
            last_error_at_ms: Some(observed_at_ms),
            action: restore_defaults_action(&control.name),
        });
    }
}

fn subsystem_health_action(subsystem: &str) -> HealthAction {
    match subsystem {
        "roots" | "watchers" | "ingest" => HealthAction {
            kind: "retry-roots",
            label: "Retry folders",
            target_id: None,
        },
        "previews" => HealthAction {
            kind: "rebuild-previews",
            label: "Rebuild previews",
            target_id: None,
        },
        "runtime" => HealthAction {
            kind: "retry-runtime",
            label: "Restart runtime",
            target_id: None,
        },
        _ => reveal_logs_action(),
    }
}

fn reveal_logs_action() -> HealthAction {
    HealthAction {
        kind: "reveal-logs",
        label: "Reveal logs",
        target_id: None,
    }
}

fn restore_defaults_action(control: &str) -> HealthAction {
    match control {
        "settings" | "config" | "tuning" => HealthAction {
            kind: "restore-controls",
            label: "Restore defaults",
            target_id: Some(control.to_owned()),
        },
        _ => reveal_logs_action(),
    }
}

fn shell_control_status(
    name: &'static str,
    recovery: &Result<crate::settings::ControlFileRecovery, crate::settings::ControlFileIssue>,
) -> ShellControlFileStatus {
    match recovery {
        Ok(recovery) => ShellControlFileStatus {
            name,
            state: recovery_state(recovery),
            source: Some(recovery_source(recovery)),
            quarantined: recovery
                .quarantined
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            warnings: recovery.warnings.iter().map(ToString::to_string).collect(),
            error: None,
            action: (!recovery.warnings.is_empty() || !recovery.quarantined.is_empty())
                .then_some("restore-controls"),
        },
        Err(issue) => ShellControlFileStatus {
            name,
            state: "unavailable",
            source: None,
            quarantined: issue
                .quarantined_path
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            warnings: Vec::new(),
            error: Some(issue.to_string()),
            action: Some("restore-controls"),
        },
    }
}

fn recovery_state(recovery: &crate::settings::ControlFileRecovery) -> &'static str {
    if recovery.warnings.is_empty()
        && recovery.quarantined.is_empty()
        && recovery.source != crate::settings::ControlFileSource::LastKnownGood
    {
        "healthy"
    } else {
        "degraded"
    }
}

fn recovery_source(recovery: &crate::settings::ControlFileRecovery) -> &'static str {
    match recovery.source {
        crate::settings::ControlFileSource::Primary => "primary",
        crate::settings::ControlFileSource::LastKnownGood => "last-known-good",
        crate::settings::ControlFileSource::MissingDefault => "missing-default",
        crate::settings::ControlFileSource::Created => "created",
    }
}

fn tuning_control_status(
    loaded: &Result<
        photoproof_core::tuning::TuningControlLoad,
        photoproof_core::runtime::ControlFileError,
    >,
) -> ShellControlFileStatus {
    match loaded {
        Ok(loaded) => {
            let recovery = &loaded.recovery;
            let degraded = recovery.source
                == photoproof_core::runtime::ControlFileSource::LastKnownGood
                || !recovery.quarantined.is_empty()
                || !recovery.warnings.is_empty()
                || !loaded.validation_warnings.is_empty();
            let mut warnings = recovery
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            warnings.extend(loaded.validation_warnings.iter().cloned());
            ShellControlFileStatus {
                name: "tuning",
                state: if degraded { "degraded" } else { "healthy" },
                source: Some(match recovery.source {
                    photoproof_core::runtime::ControlFileSource::Primary => "primary",
                    photoproof_core::runtime::ControlFileSource::LastKnownGood => "last-known-good",
                    photoproof_core::runtime::ControlFileSource::Missing => "missing-default",
                }),
                quarantined: recovery
                    .quarantined
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                warnings,
                error: None,
                action: degraded.then_some("restore-controls"),
            }
        }
        Err(issue) => ShellControlFileStatus {
            name: "tuning",
            state: "unavailable",
            source: None,
            quarantined: issue
                .quarantined_path
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            warnings: Vec::new(),
            error: Some(issue.to_string()),
            action: Some("restore-controls"),
        },
    }
}

fn subsystem_statuses(snapshot: &LifecycleSnapshot) -> Vec<SubsystemStatus> {
    snapshot
        .health
        .iter()
        .map(|(subsystem, health)| {
            let (state, summary) = match health {
                SubsystemHealth::Unknown => ("unknown", None),
                SubsystemHealth::Healthy => ("healthy", None),
                SubsystemHealth::Degraded { summary } => ("degraded", Some(summary.clone())),
                SubsystemHealth::Unavailable { summary } => ("unavailable", Some(summary.clone())),
            };
            SubsystemStatus {
                name: subsystem_name(*subsystem),
                state,
                blocking: matches!(
                    (snapshot.phase, subsystem, health),
                    (
                        LifecyclePhase::OpeningData,
                        Subsystem::Storage | Subsystem::Settings,
                        SubsystemHealth::Unavailable { .. }
                    )
                ),
                summary,
                action: recovery_action(*subsystem, health),
            }
        })
        .collect()
}

fn task_status(task: TaskSnapshot) -> BackgroundTaskStatus {
    let (progress, progress_message) = task
        .progress
        .map_or((None, None), |p| (Some(p.fraction), Some(p.message)));
    BackgroundTaskStatus {
        owner: task.owner,
        key: task.key,
        priority: match task.priority {
            TaskPriority::Background => "background",
            TaskPriority::Maintenance => "maintenance",
        },
        state: match task.state {
            TaskState::Running => "running",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        },
        started_at_ms: system_time_ms(task.started_at),
        ended_at_ms: task.ended_at.map(system_time_ms),
        progress,
        progress_message,
        last_error: task.last_error,
    }
}

fn phase_name(phase: LifecyclePhase) -> &'static str {
    match phase {
        LifecyclePhase::Cold => "cold",
        LifecyclePhase::OpeningData => "opening-data",
        LifecyclePhase::Usable => "usable",
        LifecyclePhase::Reconciling => "reconciling",
        LifecyclePhase::Ready => "ready",
        LifecyclePhase::Stopping => "stopping",
    }
}

fn subsystem_name(subsystem: Subsystem) -> &'static str {
    match subsystem {
        Subsystem::Storage => "storage",
        Subsystem::Settings => "settings",
        Subsystem::Roots => "roots",
        Subsystem::Watchers => "watchers",
        Subsystem::Ingest => "ingest",
        Subsystem::Maintenance => "maintenance",
        Subsystem::Previews => "previews",
        Subsystem::Vectors => "vectors",
        Subsystem::Runtime => "runtime",
        Subsystem::Capture => "capture",
    }
}

fn recovery_action(subsystem: Subsystem, health: &SubsystemHealth) -> Option<&'static str> {
    if matches!(health, SubsystemHealth::Healthy) {
        return None;
    }
    Some(match subsystem {
        Subsystem::Storage => "reveal-logs",
        Subsystem::Settings => "reveal-logs",
        Subsystem::Roots | Subsystem::Watchers => "retry-roots",
        Subsystem::Ingest => "retry-roots",
        Subsystem::Maintenance => "reveal-logs",
        Subsystem::Previews => "rebuild-previews",
        Subsystem::Vectors => "reveal-logs",
        Subsystem::Runtime => "retry-runtime",
        Subsystem::Capture => "reveal-logs",
    })
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::AppLifecycle;
    use crate::settings::{ControlFileRecovery, ControlFileSource};

    #[test]
    fn health_projection_keeps_degraded_subsystems_independent() {
        let lifecycle = AppLifecycle::default();
        lifecycle.transition(LifecyclePhase::OpeningData).unwrap();
        lifecycle.set_health(Subsystem::Storage, SubsystemHealth::Healthy);
        lifecycle.set_health(
            Subsystem::Roots,
            SubsystemHealth::Degraded {
                summary: "archive offline".into(),
            },
        );

        let statuses = subsystem_statuses(&lifecycle.snapshot());
        let roots = statuses.iter().find(|s| s.name == "roots").unwrap();
        let storage = statuses.iter().find(|s| s.name == "storage").unwrap();
        assert_eq!(roots.state, "degraded");
        assert_eq!(roots.action, Some("retry-roots"));
        assert_eq!(storage.state, "healthy");
        assert_eq!(storage.action, None);
    }

    #[test]
    fn schema_mismatch_is_blocking_and_retains_observation_time() {
        let issue = database_issue(
            73,
            &DatabaseStatus {
                state: "unavailable",
                schema_version: Some(13),
                expected_schema_version: 14,
                error: None,
            },
        )
        .expect("schema mismatch issue");
        assert!(issue.blocking);
        assert_eq!(issue.subsystem, "database");
        assert_eq!(issue.last_error_at_ms, Some(73));
        assert!(issue.summary.contains("13"));
        assert!(issue.summary.contains("14"));
    }

    #[test]
    fn health_inventory_keeps_archived_roots_without_reporting_them_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        let photos = dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        let app = crate::state::App::init(app_data.clone()).unwrap();
        let root_id = app.library.register_root(&photos, None).unwrap();
        app.library.archive_root(&root_id).unwrap();
        let performance =
            crate::performance::PerformanceMonitor::new(app_data.join("performance-test.jsonl"));

        let health = build_application_health(&app, performance.snapshot());
        assert_eq!(health.database.state, "healthy");
        assert_eq!(
            health.database.schema_version,
            Some(health.database.expected_schema_version)
        );
        assert!(health.volume_inventory_error.is_none());
        assert!(!health.volumes.is_empty());
        let root = health
            .roots
            .iter()
            .find(|root| root.root_id == root_id)
            .expect("archived root retained");
        assert_eq!(root.lifecycle_state, "archived");
        assert_eq!(root.state, "archived");
        assert!(
            health
                .issues
                .iter()
                .all(|issue| issue.id != format!("root:{root_id}"))
        );
    }

    #[test]
    fn control_file_recovery_is_product_health_not_a_silent_default() {
        let recovered = shell_control_status(
            "settings",
            &Ok(ControlFileRecovery {
                source: ControlFileSource::LastKnownGood,
                quarantined: vec!["settings.json.corrupt-test".into()],
                warnings: Vec::new(),
            }),
        );
        assert_eq!(recovered.state, "degraded");
        assert_eq!(recovered.source, Some("last-known-good"));
        assert_eq!(recovered.action, Some("restore-controls"));
        assert_eq!(recovered.quarantined.len(), 1);
    }

    #[test]
    fn tuning_recovery_and_validation_warnings_are_visible() {
        let recovered = tuning_control_status(&Ok(photoproof_core::tuning::TuningControlLoad {
            value: photoproof_core::tuning::Tuning::default(),
            recovery: photoproof_core::runtime::ControlFileRecovery {
                source: photoproof_core::runtime::ControlFileSource::LastKnownGood,
                quarantined: vec!["tuning.toml.corrupt-test".into()],
                warnings: Vec::new(),
            },
            validation_warnings: vec!["unsupported tuning key: search.future_knob".into()],
        }));
        assert_eq!(recovered.state, "degraded");
        assert_eq!(recovered.source, Some("last-known-good"));
        assert_eq!(recovered.action, Some("restore-controls"));
        assert_eq!(recovered.quarantined.len(), 1);
        assert_eq!(recovered.warnings.len(), 1);
    }

    #[test]
    fn blocked_wal_is_a_blocking_issue_with_failure_time_and_real_action() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        let models = dir.path().join("models");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(app_data.join("photoproof.db-wal"), b"pending").unwrap();
        let disk = crate::disk::DiskGovernor::new(app_data, models);
        let snapshot = disk.record_wal_maintenance_failure("reader is busy", true);

        let mut issues = Vec::new();
        disk_issues(42, &snapshot, &mut issues);
        let wal = issues.iter().find(|issue| issue.id == "disk:wal").unwrap();
        assert!(wal.blocking);
        assert_eq!(wal.last_error.as_deref(), Some("reader is busy"));
        assert!(wal.last_error_at_ms.is_some());
        assert_eq!(wal.action.kind, "reveal-logs");
    }

    #[test]
    fn critical_wal_is_degraded_not_falsely_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app");
        let models = dir.path().join("models");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        let disk = crate::disk::DiskGovernor::new(app_data, models);
        let mut snapshot = disk.snapshot();
        snapshot.wal.state = crate::disk::WalState::Critical;
        snapshot.wal.size_bytes = Some(crate::disk::WAL_CRITICAL_BYTES);
        snapshot.wal.age_ms = Some(crate::disk::WAL_CRITICAL_AGE_MS);

        let mut issues = Vec::new();
        disk_issues(42, &snapshot, &mut issues);
        let wal = issues.iter().find(|issue| issue.id == "disk:wal").unwrap();
        assert!(!wal.blocking);
        assert!(wal.summary.contains("critically"));
        assert_eq!(wal.action.kind, "reveal-logs");
    }
}
