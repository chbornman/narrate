//! Stage metrics — the first slice of the BACKLOG "measured, not vibes"
//! suite (founder, June 2026). `StageStat`/`StageSnapshot` are the
//! crate-wide primitives: counts and wall-clock per named stage,
//! aggregated lock-free since workers record from parallel pools — a
//! record is two `fetch_add`s and a CAS-loop max, cheap enough to stay
//! on in release builds.
//!
//! `PipelineMetrics` (ingest) is the first tenant; capture, search, and
//! IPC metrics become sibling structs here when the full suite graduates
//! from the backlog — same primitives, same snapshot shape, one debug
//! surface. The shape is deliberately the Prometheus counter model:
//! snapshots are process-lifetime CUMULATIVE (no reset method), so rates
//! fall out of diffing two snapshots and concurrent readers can never
//! clear each other's window. Percentile histograms are a future
//! StageStat-internal upgrade — `record()` is the stable seam; call
//! sites never change.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// One stage's cumulative tally. Relaxed ordering throughout: each field
/// is independently monotone and a snapshot may tear between fields by a
/// record or two — diagnostics, not accounting.
#[derive(Debug, Default)]
pub struct StageStat {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

impl StageStat {
    pub fn record(&self, elapsed: Duration) {
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
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
        StageSnapshot {
            stage,
            count,
            total_ms: total_us as f64 / 1_000.0,
            mean_ms: if count == 0 {
                0.0
            } else {
                total_us as f64 / count as f64 / 1_000.0
            },
            max_ms: max_us as f64 / 1_000.0,
        }
    }
}

/// Plain data out — the desktop shell maps this onto its own DTO (the
/// `pass_counters` precedent; core stays serde-free here).
#[derive(Debug, Clone, PartialEq)]
pub struct StageSnapshot {
    pub stage: &'static str,
    pub count: u64,
    pub total_ms: f64,
    pub mean_ms: f64,
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
}
