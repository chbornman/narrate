//! Stage metrics — the first slice of the BACKLOG "measured, not vibes"
//! suite (founder, June 2026). `StageStat`/`StageSnapshot` are the
//! crate-wide primitives: counts and wall-clock per named stage,
//! aggregated lock-free since workers record from parallel pools. A record
//! updates count/total/max plus one fixed histogram bucket, cheap enough to
//! stay on in release builds.
//!
//! `PipelineMetrics` (ingest) is the first tenant; capture, search, and
//! IPC metrics become sibling structs here when the full suite graduates
//! from the backlog — same primitives, same snapshot shape, one debug
//! surface. The shape is deliberately the Prometheus counter model:
//! snapshots are process-lifetime CUMULATIVE (no reset method), so rates
//! fall out of diffing two snapshots and concurrent readers can never
//! clear each other's window. The fixed logarithmic histogram makes
//! p50/p95/p99 available without retaining samples: bucket N is the inclusive
//! upper bound `2^N` microseconds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// One stage's cumulative tally. Relaxed ordering throughout: each field
/// is independently monotone and a snapshot may tear between fields by a
/// record or two — diagnostics, not accounting.
#[derive(Debug)]
pub struct StageStat {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    buckets: [AtomicU64; 64],
}

impl Default for StageStat {
    fn default() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl StageStat {
    pub fn record(&self, elapsed: Duration) {
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        self.buckets[histogram_bucket(us)].fetch_add(1, Ordering::Relaxed);
    }

    /// Time a closure into this stage; the closure's value passes through
    /// (errors included — a failed decode still costs what it cost).
    pub fn time<T>(&self, f: impl FnOnce() -> T) -> T {
        let started = std::time::Instant::now();
        let out = f();
        self.record(started.elapsed());
        out
    }

    fn snapshot(&self, stage: &'static str) -> StageSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        let total_us = self.total_us.load(Ordering::Relaxed);
        let max_us = self.max_us.load(Ordering::Relaxed);
        let buckets = self
            .buckets
            .each_ref()
            .map(|bucket| bucket.load(Ordering::Relaxed));
        StageSnapshot {
            stage,
            count,
            total_ms: total_us as f64 / 1_000.0,
            mean_ms: if count == 0 {
                0.0
            } else {
                total_us as f64 / count as f64 / 1_000.0
            },
            p50_ms: percentile_upper_bound_ms(&buckets, count, 50),
            p95_ms: percentile_upper_bound_ms(&buckets, count, 95),
            p99_ms: percentile_upper_bound_ms(&buckets, count, 99),
            max_ms: max_us as f64 / 1_000.0,
        }
    }
}

fn histogram_bucket(micros: u64) -> usize {
    if micros <= 1 {
        return 0;
    }
    (u64::BITS - (micros - 1).leading_zeros()) as usize
}

fn percentile_upper_bound_ms(buckets: &[u64; 64], count: u64, percentile: u64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let target = count.saturating_mul(percentile).div_ceil(100);
    let mut observed = 0u64;
    for (index, bucket_count) in buckets.iter().enumerate() {
        observed = observed.saturating_add(*bucket_count);
        if observed >= target {
            return 1u64.checked_shl(index as u32).unwrap_or(u64::MAX) as f64 / 1_000.0;
        }
    }
    u64::MAX as f64 / 1_000.0
}

/// Plain data out — the desktop shell maps this onto its own DTO (the
/// `pass_counters` precedent; core stays serde-free here).
#[derive(Debug, Clone, PartialEq)]
pub struct StageSnapshot {
    pub stage: &'static str,
    pub count: u64,
    pub total_ms: f64,
    pub mean_ms: f64,
    /// Conservative upper-bound estimate from the fixed histogram.
    pub p50_ms: f64,
    /// Conservative upper-bound estimate from the fixed histogram.
    pub p95_ms: f64,
    /// Conservative upper-bound estimate from the fixed histogram.
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// The ingest pipeline's stages, named after what the wall-clock is spent
/// ON (not which function ran): `queue_claim` is DB contention, `decode`
/// is the original-route image decode + ICC/orient, `raw_extract` is the
/// camera-embedded preview pull, `resize`/`encode`/`write` are the
/// artifact fan-out inside `preview::write_artifacts`, `db_record` is the
/// post-artifact bookkeeping under the connection lock, and the two
/// `*_pass` totals bound everything per queue item.
#[derive(Debug, Default)]
pub struct PipelineMetrics {
    pub queue_claim: StageStat,
    pub exif_pass: StageStat,
    pub preview_pass: StageStat,
    pub decode: StageStat,
    pub raw_extract: StageStat,
    pub resize: StageStat,
    pub encode: StageStat,
    pub write: StageStat,
    pub db_record: StageStat,
}

impl PipelineMetrics {
    pub fn snapshot(&self) -> Vec<StageSnapshot> {
        vec![
            self.queue_claim.snapshot("queue_claim"),
            self.exif_pass.snapshot("exif_pass"),
            self.preview_pass.snapshot("preview_pass"),
            self.decode.snapshot("decode"),
            self.raw_extract.snapshot("raw_extract"),
            self.resize.snapshot("resize"),
            self.encode.snapshot("encode"),
            self.write.snapshot("write"),
            self.db_record.snapshot("db_record"),
        ]
    }
}

/// Fixed-cardinality catalog-lane timings for the operations that dominate
/// interactive ingest/browse traffic. Each operation has two independent
/// series: time blocked on the library's shared SQLite connection mutex, and
/// time executing after the lane was acquired. Labels are compile-time
/// constants and never contain root ids, paths, hashes, or other user data.
#[derive(Debug, Default)]
pub struct CatalogMetrics {
    pub activity_wait: StageStat,
    pub activity_operation: StageStat,
    pub folder_list_wait: StageStat,
    pub folder_list_operation: StageStat,
    pub folder_delta_wait: StageStat,
    pub folder_delta_operation: StageStat,
    pub queue_claim_wait: StageStat,
    pub queue_claim_operation: StageStat,
}

impl CatalogMetrics {
    pub fn snapshot(&self) -> Vec<StageSnapshot> {
        vec![
            self.activity_wait.snapshot("activity.wait"),
            self.activity_operation.snapshot("activity.operation"),
            self.folder_list_wait.snapshot("folder_list.wait"),
            self.folder_list_operation.snapshot("folder_list.operation"),
            self.folder_delta_wait.snapshot("folder_delta.wait"),
            self.folder_delta_operation
                .snapshot("folder_delta.operation"),
            self.queue_claim_wait.snapshot("queue_claim.wait"),
            self.queue_claim_operation.snapshot("queue_claim.operation"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_count_total_and_max() {
        let stat = StageStat::default();
        stat.record(Duration::from_millis(2));
        stat.record(Duration::from_millis(6));
        let snap = stat.snapshot("s");
        assert_eq!(snap.count, 2);
        assert_eq!(snap.total_ms, 8.0);
        assert_eq!(snap.mean_ms, 4.0);
        assert_eq!(snap.p50_ms, 2.048);
        assert_eq!(snap.p95_ms, 8.192);
        assert_eq!(snap.p99_ms, 8.192);
        assert_eq!(snap.max_ms, 6.0);
    }

    #[test]
    fn time_passes_the_value_through_and_records() {
        let stat = StageStat::default();
        let v = stat.time(|| 41 + 1);
        assert_eq!(v, 42);
        assert_eq!(stat.snapshot("s").count, 1);
    }

    #[test]
    fn empty_stage_reads_all_zeros_not_nan() {
        let snap = StageStat::default().snapshot("s");
        assert_eq!(snap.count, 0);
        assert_eq!(snap.mean_ms, 0.0);
        assert_eq!(snap.p50_ms, 0.0);
        assert_eq!(snap.p95_ms, 0.0);
        assert_eq!(snap.p99_ms, 0.0);
    }

    #[test]
    fn histogram_uses_the_tight_power_of_two_upper_bound() {
        for micros in [0, 1, 2, 3, 4, 5, 1023, 1024, 1025, u32::MAX as u64] {
            let bucket = histogram_bucket(micros);
            let upper = 1u64 << bucket;
            assert!(upper >= micros.max(1), "{micros} landed above {upper}");
            if bucket > 0 {
                assert!(
                    upper / 2 < micros,
                    "{micros} did not use its tightest bucket"
                );
            }
        }
    }

    #[test]
    fn snapshot_carries_every_stage_once() {
        let m = PipelineMetrics::default();
        let names: Vec<_> = m.snapshot().into_iter().map(|s| s.stage).collect();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names.len(), 9);
        assert_eq!(names, dedup);
    }

    #[test]
    fn catalog_snapshot_is_fixed_cardinality_and_separates_wait_from_operation() {
        let metrics = CatalogMetrics::default();
        metrics.folder_list_wait.record(Duration::from_millis(2));
        metrics
            .folder_list_operation
            .record(Duration::from_millis(7));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.len(), 8);
        let names = snapshot.iter().map(|stage| stage.stage).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "activity.wait",
                "activity.operation",
                "folder_list.wait",
                "folder_list.operation",
                "folder_delta.wait",
                "folder_delta.operation",
                "queue_claim.wait",
                "queue_claim.operation",
            ]
        );
        let wait = snapshot
            .iter()
            .find(|stage| stage.stage == "folder_list.wait")
            .unwrap();
        let operation = snapshot
            .iter()
            .find(|stage| stage.stage == "folder_list.operation")
            .unwrap();
        assert_eq!(wait.count, 1);
        assert_eq!(wait.p50_ms, 2.048);
        assert_eq!(operation.count, 1);
        assert_eq!(operation.p95_ms, 8.192);
    }
}
