//! Structured, low-cardinality desktop journey telemetry.
//!
//! This is local operational evidence, not analytics: samples append to an
//! app-data JSONL file and aggregate into a bounded process-memory window. The
//! schema deliberately admits only fixed journey/phase/cache enums,
//! success/error state, and bounded numeric workload sizes. Paths, model ids,
//! queries, command names, and user content cannot become labels.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u8 = 1;
pub const MAX_BATCH_SAMPLES: usize = 256;
pub const MAX_SAMPLES_PER_SERIES: usize = 2_048;
pub const MAX_SERIES: usize = 1_024;
pub const MAX_DURATION_MS: f64 = 3_600_000.0;
pub const MAX_ITEM_COUNT: u64 = 10_000_000;
pub const MAX_BYTES: u64 = 1 << 50;
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Journey {
    Startup,
    LibraryOpen,
    RootAdd,
    FolderOpen,
    Grid,
    Graph,
    Filter,
    Journal,
    Capture,
    Settings,
    BackupRestore,
    Search,
    Preview,
    Look,
    ModelRuntime,
    AppUpdate,
    Shutdown,
    Ipc,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Total,
    Queue,
    Invoke,
    Read,
    Write,
    Scan,
    Decode,
    Render,
    Download,
    Verify,
    Load,
    Reconcile,
    FirstPaint,
    Layout,
    Settle,
    CacheLookup,
    Resize,
    Encode,
    Serve,
    Filter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheStatus {
    None,
    Hit,
    Miss,
    Stale,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum SampleSource {
    Frontend,
    /// Reserved for the direct `record_backend` hook. Initial integration
    /// wires frontend IPC first; backend journeys adopt this incrementally.
    #[allow(dead_code)]
    Backend,
}

/// IPC intake. `deny_unknown_fields` prevents an accidental high-cardinality
/// property from silently becoming part of a future disk contract.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PerformanceSampleInput {
    pub schema_version: u8,
    pub journey: Journey,
    pub phase: Phase,
    pub duration_ms: f64,
    pub ok: bool,
    pub observed_at_ms: u64,
    #[serde(default)]
    pub item_count: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub cache_status: Option<CacheStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PerformanceRecord {
    schema_version: u8,
    app_version: String,
    os: String,
    arch: String,
    run_id: String,
    source: SampleSource,
    journey: Journey,
    phase: Phase,
    duration_ms: f64,
    ok: bool,
    observed_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_status: Option<CacheStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    source: SampleSource,
    journey: Journey,
    phase: Phase,
}

#[derive(Debug, Default)]
struct Aggregate {
    count: u64,
    errors: u64,
    max_ms: f64,
    total_items: u64,
    total_bytes: u64,
    recent_ms: VecDeque<f64>,
}

impl Aggregate {
    fn push(&mut self, duration_ms: f64, ok: bool, item_count: Option<u64>, bytes: Option<u64>) {
        self.count = self.count.saturating_add(1);
        self.errors = self.errors.saturating_add(u64::from(!ok));
        self.max_ms = self.max_ms.max(duration_ms);
        self.total_items = self
            .total_items
            .saturating_add(item_count.unwrap_or_default());
        self.total_bytes = self.total_bytes.saturating_add(bytes.unwrap_or_default());
        if self.recent_ms.len() == MAX_SAMPLES_PER_SERIES {
            self.recent_ms.pop_front();
        }
        self.recent_ms.push_back(duration_ms);
    }

    fn summary(&self, key: SeriesKey) -> PerformanceSeries {
        let mut sorted = self.recent_ms.iter().copied().collect::<Vec<_>>();
        sorted.sort_by(f64::total_cmp);
        PerformanceSeries {
            source: key.source,
            journey: key.journey,
            phase: key.phase,
            count: self.count,
            errors: self.errors,
            retained: sorted.len(),
            p50_ms: percentile(&sorted, 0.50),
            p95_ms: percentile(&sorted, 0.95),
            p99_ms: percentile(&sorted, 0.99),
            max_ms: self.max_ms,
            total_items: self.total_items,
            total_bytes: self.total_bytes,
        }
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

#[derive(Default)]
struct MonitorState {
    series: BTreeMap<SeriesKey, Aggregate>,
    sink_error: Option<String>,
    rotated_logs: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSeries {
    pub source: SampleSource,
    pub journey: Journey,
    pub phase: Phase,
    pub count: u64,
    pub errors: u64,
    pub retained: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub max_ms: f64,
    pub total_items: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSnapshot {
    pub schema_version: u8,
    pub app_version: String,
    pub os: String,
    pub arch: String,
    pub run_id: String,
    pub series: Vec<PerformanceSeries>,
    pub retained_samples: usize,
    pub sink_error: Option<String>,
    pub rotated_logs: u64,
    pub log_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceIngestReport {
    pub accepted: usize,
    pub persisted: bool,
    pub sink_error: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PerformanceValidationError {
    #[error("performance batch is empty")]
    EmptyBatch,
    #[error("performance batch has {found} samples; maximum is {max}")]
    BatchTooLarge { found: usize, max: usize },
    #[error("sample {index} uses schema {found}; expected {expected}")]
    Schema {
        index: usize,
        found: u8,
        expected: u8,
    },
    #[error("sample {index} duration must be finite and between 0 and {max} ms")]
    Duration { index: usize, max: f64 },
    #[error("sample {index} observedAtMs must be non-zero")]
    Timestamp { index: usize },
    #[error("sample {index} itemCount exceeds {max}")]
    ItemCount { index: usize, max: u64 },
    #[error("sample {index} bytes exceeds {max}")]
    Bytes { index: usize, max: u64 },
}

pub struct PerformanceMonitor {
    log_path: PathBuf,
    app_version: String,
    os: String,
    arch: String,
    run_id: String,
    sink_lock: Mutex<()>,
    state: Mutex<MonitorState>,
}

impl PerformanceMonitor {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            run_id: format!("{}-{}", now_epoch_ms(), std::process::id()),
            sink_lock: Mutex::new(()),
            state: Mutex::new(MonitorState::default()),
        }
    }

    pub fn app_data_default(app_data: &Path) -> Self {
        Self::new(app_data.join("performance").join("journeys.v1.jsonl"))
    }

    pub fn ingest_frontend(
        &self,
        samples: Vec<PerformanceSampleInput>,
    ) -> Result<PerformanceIngestReport, PerformanceValidationError> {
        self.ingest(SampleSource::Frontend, samples)
    }

    #[allow(dead_code)]
    pub fn record_backend(
        &self,
        journey: Journey,
        phase: Phase,
        duration_ms: f64,
        ok: bool,
    ) -> Result<PerformanceIngestReport, PerformanceValidationError> {
        self.record_backend_with_context(journey, phase, duration_ms, ok, None, None, None)
    }

    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub fn record_backend_with_context(
        &self,
        journey: Journey,
        phase: Phase,
        duration_ms: f64,
        ok: bool,
        item_count: Option<u64>,
        bytes: Option<u64>,
        cache_status: Option<CacheStatus>,
    ) -> Result<PerformanceIngestReport, PerformanceValidationError> {
        self.ingest(
            SampleSource::Backend,
            vec![PerformanceSampleInput {
                schema_version: SCHEMA_VERSION,
                journey,
                phase,
                duration_ms,
                ok,
                observed_at_ms: now_epoch_ms(),
                item_count,
                bytes,
                cache_status,
            }],
        )
    }

    fn ingest(
        &self,
        source: SampleSource,
        samples: Vec<PerformanceSampleInput>,
    ) -> Result<PerformanceIngestReport, PerformanceValidationError> {
        validate_batch(&samples)?;
        let records = samples
            .iter()
            .map(|sample| PerformanceRecord {
                schema_version: SCHEMA_VERSION,
                app_version: self.app_version.clone(),
                os: self.os.clone(),
                arch: self.arch.clone(),
                run_id: self.run_id.clone(),
                source,
                journey: sample.journey,
                phase: sample.phase,
                duration_ms: sample.duration_ms,
                ok: sample.ok,
                observed_at_ms: sample.observed_at_ms,
                item_count: sample.item_count,
                bytes: sample.bytes,
                cache_status: sample.cache_status,
            })
            .collect::<Vec<_>>();

        let persisted = {
            // Append/rotation is a single-writer critical section. Separate it
            // from the aggregation mutex so snapshots stay cheap while disk is
            // slow, but never let concurrent IPC batches interleave JSON or
            // race the bounded log rotation.
            let _sink = self.sink_lock.lock().expect("performance sink mutex");
            self.append_records(&records)
        };
        let mut state = self.state.lock().expect("performance monitor mutex");
        match &persisted {
            Ok(rotated) => {
                state.sink_error = None;
                state.rotated_logs = state.rotated_logs.saturating_add(u64::from(*rotated));
            }
            Err(error) => state.sink_error = Some(error.to_string()),
        }
        for sample in samples {
            let key = SeriesKey {
                source,
                journey: sample.journey,
                phase: sample.phase,
            };
            let at_capacity = state.series.len() == MAX_SERIES;
            match state.series.entry(key) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().push(
                        sample.duration_ms,
                        sample.ok,
                        sample.item_count,
                        sample.bytes,
                    );
                }
                std::collections::btree_map::Entry::Vacant(entry) if !at_capacity => {
                    entry.insert(Aggregate::default()).push(
                        sample.duration_ms,
                        sample.ok,
                        sample.item_count,
                        sample.bytes,
                    );
                }
                std::collections::btree_map::Entry::Vacant(_) => {
                    // The closed schema product fits below this bound. A
                    // future expansion remains bounded and discoverable.
                    state.sink_error = Some("performance series capacity exhausted".into());
                }
            }
        }
        Ok(PerformanceIngestReport {
            accepted: records.len(),
            persisted: persisted.is_ok(),
            sink_error: state.sink_error.clone(),
        })
    }

    pub fn snapshot(&self) -> PerformanceSnapshot {
        let state = self.state.lock().expect("performance monitor mutex");
        let series = state
            .series
            .iter()
            .map(|(key, aggregate)| aggregate.summary(*key))
            .collect::<Vec<_>>();
        PerformanceSnapshot {
            schema_version: SCHEMA_VERSION,
            app_version: self.app_version.clone(),
            os: self.os.clone(),
            arch: self.arch.clone(),
            run_id: self.run_id.clone(),
            retained_samples: series.iter().map(|item| item.retained).sum(),
            series,
            sink_error: state.sink_error.clone(),
            rotated_logs: state.rotated_logs,
            log_path: self.log_path.display().to_string(),
        }
    }

    fn append_records(&self, records: &[PerformanceRecord]) -> io::Result<bool> {
        let parent = self
            .log_path
            .parent()
            .ok_or_else(|| io::Error::other("performance log has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let mut rotated = false;
        if fs::metadata(&self.log_path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
            let previous = self.log_path.with_extension("jsonl.previous");
            match fs::remove_file(&previous) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            fs::rename(&self.log_path, previous)?;
            rotated = true;
        }

        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).map_err(io::Error::other)?;
            bytes.push(b'\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_data()?;
        Ok(rotated)
    }
}

fn validate_batch(samples: &[PerformanceSampleInput]) -> Result<(), PerformanceValidationError> {
    if samples.is_empty() {
        return Err(PerformanceValidationError::EmptyBatch);
    }
    if samples.len() > MAX_BATCH_SAMPLES {
        return Err(PerformanceValidationError::BatchTooLarge {
            found: samples.len(),
            max: MAX_BATCH_SAMPLES,
        });
    }
    for (index, sample) in samples.iter().enumerate() {
        if sample.schema_version != SCHEMA_VERSION {
            return Err(PerformanceValidationError::Schema {
                index,
                found: sample.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        if !sample.duration_ms.is_finite() || !(0.0..=MAX_DURATION_MS).contains(&sample.duration_ms)
        {
            return Err(PerformanceValidationError::Duration {
                index,
                max: MAX_DURATION_MS,
            });
        }
        if sample.observed_at_ms == 0 {
            return Err(PerformanceValidationError::Timestamp { index });
        }
        if sample
            .item_count
            .is_some_and(|count| count > MAX_ITEM_COUNT)
        {
            return Err(PerformanceValidationError::ItemCount {
                index,
                max: MAX_ITEM_COUNT,
            });
        }
        if sample.bytes.is_some_and(|bytes| bytes > MAX_BYTES) {
            return Err(PerformanceValidationError::Bytes {
                index,
                max: MAX_BYTES,
            });
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(duration_ms: f64, ok: bool) -> PerformanceSampleInput {
        PerformanceSampleInput {
            schema_version: SCHEMA_VERSION,
            journey: Journey::Search,
            phase: Phase::Total,
            duration_ms,
            ok,
            observed_at_ms: 1,
            item_count: None,
            bytes: None,
            cache_status: None,
        }
    }

    #[test]
    fn validation_rejects_bad_schema_duration_timestamp_and_batch_size() {
        let mut bad = sample(1.0, true);
        bad.schema_version = 2;
        assert!(matches!(
            validate_batch(&[bad]),
            Err(PerformanceValidationError::Schema { .. })
        ));
        assert!(matches!(
            validate_batch(&[sample(f64::INFINITY, true)]),
            Err(PerformanceValidationError::Duration { .. })
        ));
        let mut bad = sample(1.0, true);
        bad.observed_at_ms = 0;
        assert!(matches!(
            validate_batch(&[bad]),
            Err(PerformanceValidationError::Timestamp { .. })
        ));
        assert!(matches!(
            validate_batch(&vec![sample(1.0, true); MAX_BATCH_SAMPLES + 1]),
            Err(PerformanceValidationError::BatchTooLarge { .. })
        ));
        let mut bad = sample(1.0, true);
        bad.item_count = Some(MAX_ITEM_COUNT + 1);
        assert!(matches!(
            validate_batch(&[bad]),
            Err(PerformanceValidationError::ItemCount { .. })
        ));
        let mut bad = sample(1.0, true);
        bad.bytes = Some(MAX_BYTES + 1);
        assert!(matches!(
            validate_batch(&[bad]),
            Err(PerformanceValidationError::Bytes { .. })
        ));
    }

    #[test]
    fn nearest_rank_percentiles_and_errors_are_stable() {
        let temp = tempfile::tempdir().unwrap();
        let monitor = PerformanceMonitor::new(temp.path().join("performance.jsonl"));
        monitor
            .ingest_frontend(vec![
                sample(1.0, true),
                sample(2.0, false),
                sample(3.0, true),
                sample(4.0, true),
                sample(100.0, false),
            ])
            .unwrap();
        let item = monitor.snapshot().series.remove(0);
        assert_eq!(item.count, 5);
        assert_eq!(item.errors, 2);
        assert_eq!(item.p50_ms, Some(3.0));
        assert_eq!(item.p95_ms, Some(100.0));
        assert_eq!(item.p99_ms, Some(100.0));
        assert_eq!(item.max_ms, 100.0);
    }

    #[test]
    fn retention_is_bounded_while_lifetime_counts_continue() {
        let temp = tempfile::tempdir().unwrap();
        let monitor = PerformanceMonitor::new(temp.path().join("performance.jsonl"));
        for offset in (0..MAX_SAMPLES_PER_SERIES + 10).step_by(MAX_BATCH_SAMPLES) {
            let count = (MAX_SAMPLES_PER_SERIES + 10 - offset).min(MAX_BATCH_SAMPLES);
            monitor
                .ingest_frontend(vec![sample(offset as f64, true); count])
                .unwrap();
        }
        let snapshot = monitor.snapshot();
        assert_eq!(
            snapshot.series[0].count,
            (MAX_SAMPLES_PER_SERIES + 10) as u64
        );
        assert_eq!(snapshot.series[0].retained, MAX_SAMPLES_PER_SERIES);
        assert_eq!(snapshot.retained_samples, MAX_SAMPLES_PER_SERIES);
    }

    #[test]
    fn workload_totals_do_not_create_new_series_and_cache_status_is_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("performance.jsonl");
        let monitor = PerformanceMonitor::new(path.clone());
        let mut first = sample(4.0, true);
        first.item_count = Some(20);
        first.bytes = Some(1_024);
        first.cache_status = Some(CacheStatus::Hit);
        let mut second = sample(8.0, true);
        second.item_count = Some(3);
        second.bytes = Some(512);
        second.cache_status = Some(CacheStatus::Miss);
        monitor.ingest_frontend(vec![first, second]).unwrap();

        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.series.len(), 1);
        assert_eq!(snapshot.series[0].total_items, 23);
        assert_eq!(snapshot.series[0].total_bytes, 1_536);
        let jsonl = fs::read_to_string(path).unwrap();
        assert!(jsonl.contains("\"cacheStatus\":\"hit\""));
        assert!(jsonl.contains("\"cacheStatus\":\"miss\""));
    }

    #[test]
    fn backend_enriches_records_and_snapshots_with_comparison_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("performance.jsonl");
        let monitor = PerformanceMonitor::new(path.clone());
        monitor
            .ingest_frontend(vec![sample(4.0, true), sample(8.0, true)])
            .unwrap();

        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(snapshot.os, std::env::consts::OS);
        assert_eq!(snapshot.arch, std::env::consts::ARCH);
        assert!(
            snapshot
                .run_id
                .ends_with(&format!("-{}", std::process::id()))
        );

        let records = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        for record in records {
            assert_eq!(record["appVersion"], snapshot.app_version);
            assert_eq!(record["os"], snapshot.os);
            assert_eq!(record["arch"], snapshot.arch);
            assert_eq!(record["runId"], snapshot.run_id);
        }
    }

    #[test]
    fn backend_context_uses_same_validation_and_workload_aggregation() {
        let temp = tempfile::tempdir().unwrap();
        let monitor = PerformanceMonitor::new(temp.path().join("performance.jsonl"));
        monitor
            .record_backend_with_context(
                Journey::Preview,
                Phase::Serve,
                2.5,
                true,
                Some(1),
                Some(8_192),
                Some(CacheStatus::Hit),
            )
            .unwrap();
        let item = &monitor.snapshot().series[0];
        assert_eq!(item.source, SampleSource::Backend);
        assert_eq!(item.total_items, 1);
        assert_eq!(item.total_bytes, 8_192);
        assert_eq!(item.count, 1);

        assert!(matches!(
            monitor.record_backend_with_context(
                Journey::Preview,
                Phase::Serve,
                2.5,
                true,
                Some(MAX_ITEM_COUNT + 1),
                None,
                None,
            ),
            Err(PerformanceValidationError::ItemCount { .. })
        ));
        assert_eq!(
            monitor.snapshot().series[0].count,
            1,
            "invalid backend context must not enter the aggregate"
        );
    }

    #[test]
    fn sink_failure_is_reported_but_does_not_lose_aggregation() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        let monitor = PerformanceMonitor::new(blocker.join("performance.jsonl"));
        let report = monitor.ingest_frontend(vec![sample(5.0, true)]).unwrap();
        assert!(!report.persisted);
        assert!(report.sink_error.is_some());
        assert_eq!(monitor.snapshot().series[0].count, 1);
    }
}
