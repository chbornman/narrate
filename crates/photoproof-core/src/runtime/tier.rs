//! Hardware tiers (spec/RUNTIME.md §6): the detection recipe behind a
//! seam, the §6.2 decision table **as data**, override rules, and the
//! cache. The live probe (wgpu adapter enumeration + per-platform VRAM
//! query) lives in the desktop shell, feeding [`HardwareReport`] through
//! the [`HardwareProbe`] seam; this container has no GPU, so the live
//! path resolves Tier 0 — real numbers are P6.3 founder-machine work.

use std::path::Path;

use serde::{Deserialize, Serialize};

use photoproof_connectors::config::Tier;

/// One detected adapter. `vram_bytes` is the byte-accurate native query
/// (§6.1: Vulkan largest DEVICE_LOCAL heap / DXGI DedicatedVideoMemory /
/// Metal recommendedMaxWorkingSetSize); `None` when only enumeration
/// succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuAdapter {
    pub name: String,
    pub backend: String,
    pub vram_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HardwareReport {
    pub adapters: Vec<GpuAdapter>,
    /// Apple Silicon unified memory (`sysctl hw.memsize`); `None` off
    /// macOS / on Intel Macs.
    pub apple_unified_bytes: Option<u64>,
    /// RFC 3339 detection time (cache bookkeeping).
    pub detected_at: String,
}

/// The detection seam: the shell's wgpu/native probe implements this;
/// tests script reports.
pub trait HardwareProbe: Send {
    fn probe(&mut self) -> HardwareReport;
}

/// One row of the §6.2 decision table, as data.
#[derive(Debug, Clone, Copy)]
pub struct TierRule {
    pub tier: u8,
    /// Minimum dedicated VRAM (bytes) on any adapter…
    pub min_vram_bytes: u64,
    /// …or minimum Apple unified memory (bytes; usable model budget =
    /// 60% heuristic is folded into these gates per §6.1.2).
    pub min_apple_unified_bytes: u64,
}

/// §6.2, top row first. Tier 0 is the fall-through: < 7.5 GB dedicated
/// and < 16 GB Apple unified, or no GPU — full journal, no downloads.
///
/// VRAM gates carry a 0.5 GiB headroom tolerance (P6.3 spike +
/// founder-checklist calibration): drivers report the LARGEST HEAP, not
/// the marketing number — the founder's 16 GB RTX 5080 reports
/// 15.92 GiB and sat 85 MiB under a literal 16 GiB gate. The spike's
/// measured budgets (E2B 4.3 GB + ASR 1.1 GB + embedders ≈ 6–8 GB
/// resident) say a 15.5 GiB card carries Tier 2 with room to spare.
/// Apple-unified gates stay at marketing numbers: hw.memsize reports
/// the full physical size exactly.
const GIB: u64 = 1024 * 1024 * 1024;
pub const DECISION_TABLE: &[TierRule] = &[
    TierRule {
        tier: 2,
        min_vram_bytes: 15 * GIB + GIB / 2,
        min_apple_unified_bytes: 32 * GIB,
    },
    TierRule {
        tier: 1,
        min_vram_bytes: 7 * GIB + GIB / 2,
        min_apple_unified_bytes: 16 * GIB,
    },
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierDecision {
    pub detected_tier: u8,
    /// The §6.2 effective tier after the user override (always wins,
    /// both directions).
    pub effective_tier: u8,
    /// Overriding ABOVE detected hardware shows a one-time plain warning
    /// (§6.2).
    pub overridden_above: bool,
    pub reason: String,
}

/// The §6.2 table over a report (pure; the table is data).
pub fn detect_tier(report: &HardwareReport) -> (u8, String) {
    let best_vram = report
        .adapters
        .iter()
        .filter_map(|a| a.vram_bytes)
        .max()
        .unwrap_or(0);
    let unified = report.apple_unified_bytes.unwrap_or(0);
    for rule in DECISION_TABLE {
        if best_vram >= rule.min_vram_bytes {
            return (rule.tier, format!("{} bytes dedicated VRAM", best_vram));
        }
        if unified >= rule.min_apple_unified_bytes {
            return (rule.tier, format!("{unified} bytes Apple unified memory"));
        }
    }
    (
        0,
        if report.adapters.is_empty() {
            "no GPU adapters".to_owned()
        } else {
            format!("below the floor ({best_vram} bytes best VRAM)")
        },
    )
}

/// Detection + the `[runtime] tier` override (§6.2: user override always
/// wins, in both directions; above-detected gets the one-time warning).
pub fn decide_tier(report: &HardwareReport, config_tier: Tier) -> TierDecision {
    let (detected, reason) = detect_tier(report);
    match config_tier {
        Tier::Auto => TierDecision {
            detected_tier: detected,
            effective_tier: detected,
            overridden_above: false,
            reason,
        },
        Tier::Fixed(t) => TierDecision {
            detected_tier: detected,
            effective_tier: t,
            overridden_above: t > detected,
            reason: format!("user override ({reason})"),
        },
    }
}

/// Cached report + decision (`{app-data}/runtime/tier.json`): cached, and
/// re-detected on demand or when hardware changes (§6.1.4 — the shell
/// re-probes on the settings action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierCache {
    pub report: HardwareReport,
    pub decision: TierDecision,
}

impl TierCache {
    pub fn load(path: &Path) -> Option<Self> {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("tier json"))?;
        std::fs::rename(tmp, path)
    }
}

/// Resolve the tier: cache hit unless `redetect`; probe + decide + cache
/// otherwise.
pub fn resolve_tier(
    probe: &mut dyn HardwareProbe,
    config_tier: Tier,
    cache_path: &Path,
    redetect: bool,
) -> TierDecision {
    if !redetect && let Some(cached) = TierCache::load(cache_path) {
        // The override is config, not cache: re-apply over the cached
        // report so a config edit wins without a re-probe.
        return decide_tier(&cached.report, config_tier);
    }
    let report = probe.probe();
    let decision = decide_tier(&report, config_tier);
    let _ = TierCache {
        report,
        decision: decision.clone(),
    }
    .save(cache_path);
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(vram_gb: Option<u64>, unified_gb: Option<u64>) -> HardwareReport {
        HardwareReport {
            adapters: vram_gb
                .map(|gb| {
                    vec![GpuAdapter {
                        name: "scripted".into(),
                        backend: "vulkan".into(),
                        vram_bytes: Some(gb * 1024 * 1024 * 1024),
                    }]
                })
                .unwrap_or_default(),
            apple_unified_bytes: unified_gb.map(|gb| gb * 1024 * 1024 * 1024),
            detected_at: "2026-06-11T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_6_2_decision_table_resolves_each_gate() {
        // No GPU → 0; 6 GB → 0; 8 GB → 1; 12 GB → 1; 16 GB → 2.
        assert_eq!(detect_tier(&report(None, None)).0, 0);
        assert_eq!(detect_tier(&report(Some(6), None)).0, 0);
        assert_eq!(detect_tier(&report(Some(8), None)).0, 1);
        assert_eq!(detect_tier(&report(Some(12), None)).0, 1);
        assert_eq!(detect_tier(&report(Some(16), None)).0, 2);
        // Apple unified: 8 GB → 0; 16 GB → 1; 32 GB → 2.
        assert_eq!(detect_tier(&report(None, Some(8))).0, 0);
        assert_eq!(detect_tier(&report(None, Some(16))).0, 1);
        assert_eq!(detect_tier(&report(None, Some(32))).0, 2);
    }

    #[test]
    fn user_override_wins_both_directions_and_flags_above_detected() {
        let r = report(Some(12), None); // detected 1
        let down = decide_tier(&r, Tier::Fixed(0));
        assert_eq!(down.effective_tier, 0);
        assert!(!down.overridden_above, "downgrade: no warning");
        let up = decide_tier(&r, Tier::Fixed(2));
        assert_eq!(up.effective_tier, 2);
        assert!(up.overridden_above, "above detected: one-time warning");
        assert_eq!(up.detected_tier, 1);
    }

    #[test]
    fn cache_round_trips_and_redetect_refreshes() {
        struct Scripted(Vec<HardwareReport>);
        impl HardwareProbe for Scripted {
            fn probe(&mut self) -> HardwareReport {
                self.0.remove(0)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tier.json");
        let mut probe = Scripted(vec![report(Some(8), None), report(Some(16), None)]);
        let first = resolve_tier(&mut probe, Tier::Auto, &path, false);
        assert_eq!(first.effective_tier, 1);
        // Cache hit: no probe call.
        let cached = resolve_tier(&mut probe, Tier::Auto, &path, false);
        assert_eq!(cached.effective_tier, 1);
        // Config override re-applies OVER the cache without a probe.
        let overridden = resolve_tier(&mut probe, Tier::Fixed(0), &path, false);
        assert_eq!(overridden.effective_tier, 0);
        // Re-detect on demand consumes the second scripted report.
        let redetected = resolve_tier(&mut probe, Tier::Auto, &path, true);
        assert_eq!(redetected.effective_tier, 2);
        assert!(probe.0.is_empty(), "exactly two probe calls");
    }
}
