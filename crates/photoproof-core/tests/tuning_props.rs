//! Property tests for the tuning validation bounds (`tuning.rs`).
//!
//! Contract under test (spec/DESIGN-TUNING-CONFIG.md, tuning.rs module docs):
//! "Never a silent bad number." A hand-edited `tuning.toml` value is accepted
//! IFF it is finite and within the documented bound for that dial; otherwise it
//! is REJECTED back to the code default (with a logged warning) - never a
//! silently-applied bad number.
//!
//! WHY a SEPARATE file (not inline in tuning.rs): a parallel agent is editing
//! tuning.rs (adding a `[voice]` section). Keeping these tests here avoids a
//! merge clash, and they only reference the CURRENTLY-EXISTING sections
//! ([search] / [preview] / [graph] / [heatmap]).
//!
//! HOW we drive validation through the PUBLIC API: the section `validated()`
//! methods are private, so we exercise the real path the founder hits - write a
//! `tuning.toml` and call `Tuning::load(app_data)`, which parses + validates +
//! merges. A temp dir per case keeps the cases independent.
//!
//! The documented bounds are duplicated here as test oracles. They are the
//! values tuning.rs spells out in its bounds consts; if a bound changes, this
//! file is the second place that must move (the same guardrail the inline
//! `default_equals_documented_values` test provides for the defaults).

use photoproof_core::tuning::Tuning;
use proptest::prelude::*;

// --- documented bounds (oracle copies of tuning.rs's private consts) --------

// Fusion weights and heatmap weights: non-negative multipliers, generous cap.
const WEIGHT_MIN: f64 = 0.0;
const WEIGHT_MAX: f64 = 1000.0;
// RRF rank constant k: positive denominator term.
const RRF_K_MIN: f64 = 1.0;
const RRF_K_MAX: f64 = 10_000.0;
// beta: centered-cosine multiplier.
const BETA_MIN: f64 = 0.0;
const BETA_MAX: f64 = 1.0;
// Preview pixel edges (u32).
const EDGE_MIN: u32 = 256;
const EDGE_MAX: u32 = 16_384;
// Graph alpha blend fraction.
const GRAPH_ALPHA_MIN: f64 = 0.0;
const GRAPH_ALPHA_MAX: f64 = 1.0;
// Graph force/length magnitudes.
const GRAPH_FORCE_MIN: f64 = 0.0;
const GRAPH_FORCE_MAX: f64 = 100_000.0;
// Graph damping retain-fraction.
const GRAPH_DAMPING_MIN: f64 = 0.0;
const GRAPH_DAMPING_MAX: f64 = 1.0;
// Graph cluster-k bounds (u32).
const GRAPH_CLUSTER_K_BOUND_MIN: u32 = 1;
const GRAPH_CLUSTER_K_BOUND_MAX: u32 = 64;
// Graph LOD threshold bounds (u32).
const GRAPH_LOD_MIN: u32 = 50;
const GRAPH_LOD_MAX: u32 = 1_000_000;
// Dwell rate: OPEN-low interval (0, 1].
const DWELL_RATE_MAX: f64 = 1.0;
// Dwell cap (i64).
const DWELL_CAP_MIN_MS: i64 = 0;
const DWELL_CAP_MAX_MS: i64 = 86_400_000;
// Recency half-life: OPEN-low (0, 365].
const HALF_LIFE_MAX_DAYS: f64 = 365.0;

/// Write one `[section] key = value` tuning.toml into a fresh temp dir, load it
/// through the real validation path, and return the merged config so the caller
/// can assert against it with `prop_assert_eq!` (which needs `?`-able context).
fn load_toml(section_key_value: &str) -> Tuning {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("tuning.toml"), section_key_value).expect("write toml");
    Tuning::load(dir.path())
}

/// `true` when `v` is finite and within the CLOSED interval [min, max] - the
/// `range_or_default` predicate for every closed-bound float dial.
fn in_closed(v: f64, min: f64, max: f64) -> bool {
    v.is_finite() && (min..=max).contains(&v)
}

/// `true` when `v` is finite and within the OPEN-low interval (0, max] - the
/// `rate_or_default` / `half_life_or_default` predicate.
fn in_open_low(v: f64, max: f64) -> bool {
    v.is_finite() && v > 0.0 && v <= max
}

// A finite-f64 strategy spanning well inside, on, and well outside every bound,
// so each property sees both the accept and reject branches. We deliberately
// exclude NaN/Inf here (TOML float literals for those are covered separately);
// `prop_assume`-free so coverage stays high.
fn spanning_f64(lo: f64, hi: f64) -> impl Strategy<Value = f64> {
    let span = hi - lo;
    prop_oneof![
        // Comfortably inside.
        lo..=hi,
        // Around and past the upper bound.
        Just(hi),
        (hi)..=(hi + span.max(1.0) * 2.0),
        // Around and past the lower bound (including negatives).
        Just(lo),
        (lo - span.max(1.0) * 2.0)..=(lo),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    // ---- [search] ---------------------------------------------------------

    /// A fusion weight is accepted iff finite and in [0, 1000]; else default 1.0.
    #[test]
    fn search_fusion_s1_accepts_iff_in_bound(v in spanning_f64(WEIGHT_MIN, WEIGHT_MAX)) {
        let t = load_toml(&format!("[search.fusion]\ns1 = {v:?}\n"));
        let expected = if in_closed(v, WEIGHT_MIN, WEIGHT_MAX) { v } else { 1.0 };
        prop_assert_eq!(t.search.fusion.s1, expected);
    }

    /// `rrf_k` is accepted iff finite and in [1, 10000]; else default 60.0.
    #[test]
    fn search_rrf_k_accepts_iff_in_bound(v in spanning_f64(RRF_K_MIN, RRF_K_MAX)) {
        let t = load_toml(&format!("[search]\nrrf_k = {v:?}\n"));
        let expected = if in_closed(v, RRF_K_MIN, RRF_K_MAX) { v } else { 60.0 };
        prop_assert_eq!(t.search.rrf_k, expected);
    }

    /// `beta` is accepted iff finite and in [0, 1]; else default 0.5.
    #[test]
    fn search_beta_accepts_iff_in_bound(v in spanning_f64(BETA_MIN, BETA_MAX)) {
        let t = load_toml(&format!("[search]\nbeta = {v:?}\n"));
        let expected = if in_closed(v, BETA_MIN, BETA_MAX) { v } else { 0.5 };
        prop_assert_eq!(t.search.beta, expected);
    }

    // ---- [preview] --------------------------------------------------------

    /// `display_edge` (u32 px) is accepted iff in [256, 16384]; else default 2560.
    #[test]
    fn preview_display_edge_accepts_iff_in_bound(v in any::<u32>()) {
        let t = load_toml(&format!("[preview]\ndisplay_edge = {v}\n"));
        let expected = if (EDGE_MIN..=EDGE_MAX).contains(&v) { v } else { 2560 };
        prop_assert_eq!(t.preview.display_edge, expected);
    }

    /// `embedded_accept_edge` (u32 px): accepted iff in [256, 16384]; else 2048.
    #[test]
    fn preview_embedded_accept_edge_accepts_iff_in_bound(v in any::<u32>()) {
        let t = load_toml(&format!("[preview]\nembedded_accept_edge = {v}\n"));
        let expected = if (EDGE_MIN..=EDGE_MAX).contains(&v) { v } else { 2048 };
        prop_assert_eq!(t.preview.embedded_accept_edge, expected);
    }

    // ---- [graph] ----------------------------------------------------------

    /// `alpha_default` is accepted iff finite and in [0, 1]; else default 0.5.
    #[test]
    fn graph_alpha_accepts_iff_in_bound(v in spanning_f64(GRAPH_ALPHA_MIN, GRAPH_ALPHA_MAX)) {
        let t = load_toml(&format!("[graph]\nalpha_default = {v:?}\n"));
        let expected = if in_closed(v, GRAPH_ALPHA_MIN, GRAPH_ALPHA_MAX) { v } else { 0.5 };
        prop_assert_eq!(t.graph.alpha_default, expected);
    }

    /// A graph force magnitude (`repulsion`) is accepted iff finite and in
    /// [0, 100000]; else its default 800.0.
    #[test]
    fn graph_repulsion_accepts_iff_in_bound(v in spanning_f64(GRAPH_FORCE_MIN, GRAPH_FORCE_MAX)) {
        let t = load_toml(&format!("[graph]\nrepulsion = {v:?}\n"));
        let expected = if in_closed(v, GRAPH_FORCE_MIN, GRAPH_FORCE_MAX) { v } else { 800.0 };
        prop_assert_eq!(t.graph.repulsion, expected);
    }

    /// `damping` is accepted iff finite and in [0, 1]; else default 0.85.
    #[test]
    fn graph_damping_accepts_iff_in_bound(v in spanning_f64(GRAPH_DAMPING_MIN, GRAPH_DAMPING_MAX)) {
        let t = load_toml(&format!("[graph]\ndamping = {v:?}\n"));
        let expected = if in_closed(v, GRAPH_DAMPING_MIN, GRAPH_DAMPING_MAX) { v } else { 0.85 };
        prop_assert_eq!(t.graph.damping, expected);
    }

    /// `cluster_k_max` (u32) is accepted iff in [1, 64]; else default 12.
    #[test]
    fn graph_cluster_k_max_accepts_iff_in_bound(v in any::<u32>()) {
        let t = load_toml(&format!("[graph]\ncluster_k_max = {v}\n"));
        let expected = if (GRAPH_CLUSTER_K_BOUND_MIN..=GRAPH_CLUSTER_K_BOUND_MAX).contains(&v) { v } else { 12 };
        prop_assert_eq!(t.graph.cluster_k_max, expected);
    }

    /// `lod_threshold` (u32) is accepted iff in [50, 1_000_000]; else 1500.
    #[test]
    fn graph_lod_threshold_accepts_iff_in_bound(v in any::<u32>()) {
        let t = load_toml(&format!("[graph]\nlod_threshold = {v}\n"));
        let expected = if (GRAPH_LOD_MIN..=GRAPH_LOD_MAX).contains(&v) { v } else { 1500 };
        prop_assert_eq!(t.graph.lod_threshold, expected);
    }

    // ---- [heatmap] --------------------------------------------------------

    /// A heatmap weight (`w_dwell`) is accepted iff finite and in [0, 1000];
    /// else its default 0.0001.
    #[test]
    fn heatmap_w_dwell_accepts_iff_in_bound(v in spanning_f64(WEIGHT_MIN, WEIGHT_MAX)) {
        let t = load_toml(&format!("[heatmap]\nw_dwell = {v:?}\n"));
        let expected = if in_closed(v, WEIGHT_MIN, WEIGHT_MAX) { v } else { 0.0001 };
        prop_assert_eq!(t.heatmap.w_dwell, expected);
    }

    /// A dwell rate is accepted iff finite and in the OPEN-low interval (0, 1]
    /// (a 0 erases the tier; > 1 invents time); else its default. `dwell_grid_rate`
    /// default is 0.15.
    #[test]
    fn heatmap_dwell_grid_rate_accepts_iff_in_open_low(v in spanning_f64(0.0, DWELL_RATE_MAX)) {
        let t = load_toml(&format!("[heatmap]\ndwell_grid_rate = {v:?}\n"));
        let expected = if in_open_low(v, DWELL_RATE_MAX) { v } else { 0.15 };
        prop_assert_eq!(t.heatmap.dwell_grid_rate, expected);
    }

    /// `dwell_cap_ms` (i64) is accepted iff in [0, 86_400_000]; else 60_000.
    #[test]
    fn heatmap_dwell_cap_accepts_iff_in_bound(v in any::<i64>()) {
        let t = load_toml(&format!("[heatmap]\ndwell_cap_ms = {v}\n"));
        let expected = if (DWELL_CAP_MIN_MS..=DWELL_CAP_MAX_MS).contains(&v) { v } else { 60_000 };
        prop_assert_eq!(t.heatmap.dwell_cap_ms, expected);
    }

    /// `recency_half_life_days` is accepted iff finite and in the OPEN-low
    /// interval (0, 365] (it divides; non-positive would go NaN/Inf); else 14.0.
    #[test]
    fn heatmap_half_life_accepts_iff_in_open_low(v in spanning_f64(0.0, HALF_LIFE_MAX_DAYS)) {
        let t = load_toml(&format!("[heatmap]\nrecency_half_life_days = {v:?}\n"));
        let expected = if in_open_low(v, HALF_LIFE_MAX_DAYS) { v } else { 14.0 };
        prop_assert_eq!(t.heatmap.recency_half_life_days, expected);
    }
}

/// A non-finite float literal in the TOML (`nan` / `inf` / `-inf`) is rejected
/// to the default for every float dial - the `is_finite()` half of the guard.
/// (TOML supports these literals, so a hand-edit really can produce them.)
#[test]
fn non_finite_toml_literals_reject_to_default() {
    for lit in ["nan", "inf", "-inf"] {
        let toml = format!(
            "[search]\nrrf_k = {lit}\nbeta = {lit}\n[search.fusion]\ns1 = {lit}\n\
             [graph]\nalpha_default = {lit}\nrepulsion = {lit}\n\
             [heatmap]\nw_dwell = {lit}\ndwell_grid_rate = {lit}\nrecency_half_life_days = {lit}\n"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tuning.toml"), &toml).unwrap();
        let t = Tuning::load(dir.path());
        assert_eq!(t.search.rrf_k, 60.0, "rrf_k {lit}");
        assert_eq!(t.search.beta, 0.5, "beta {lit}");
        assert_eq!(t.search.fusion.s1, 1.0, "s1 {lit}");
        assert_eq!(t.graph.alpha_default, 0.5, "alpha {lit}");
        assert_eq!(t.graph.repulsion, 800.0, "repulsion {lit}");
        assert_eq!(t.heatmap.w_dwell, 0.0001, "w_dwell {lit}");
        assert_eq!(t.heatmap.dwell_grid_rate, 0.15, "grid_rate {lit}");
        assert_eq!(t.heatmap.recency_half_life_days, 14.0, "half_life {lit}");
    }
}

/// A missing tuning.toml is pure defaults (not an error) - the baseline the
/// property oracle compares against.
#[test]
fn missing_file_is_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let t = Tuning::load(dir.path());
    assert_eq!(t, Tuning::default());
}
