//! Centralized tuning configuration — ONE home for every arbitrary ranking /
//! recommendation weight that used to live as scattered `const`s across the
//! implementation files (hybrid.rs's fusion weights / RRF k / β, preview.rs's
//! display + embedded-accept edges).
//!
//! Design: `docs/DESIGN-TUNING-CONFIG.md`; rendered human view: `docs/tuning.html`.
//!
//! WHY this exists: the founder tunes by feel and re-feels without a rebuild.
//! The code holds the defaults (the values that ship); a `<app-data>/tuning.toml`
//! overrides any subset of them at startup. Implementation files READ this
//! config — they no longer OWN the numbers.
//!
//! Contract this module upholds:
//! - **No behavior change on default**: [`Tuning::default`] equals the exact
//!   values the old scattered consts held (proven by the tuning gate test and
//!   every unchanged search/preview test). This module is purely "give the
//!   numbers one home".
//! - **Partial-file merge**: `#[serde(default)]` on every field, so a
//!   `tuning.toml` that sets only `[search].rrf_k` keeps every other default.
//! - **Never a silent bad number**: [`Tuning::load`] range-validates each
//!   field; an out-of-range value is rejected with a `tracing::warn!` and the
//!   DEFAULT is kept. A missing file is pure defaults (not an error).
//!
//! Genuine budgets/contracts (the <100 ms search budget, the embedded-preview
//! aspect tolerance) deliberately stay FIXED consts in their own files: a
//! `tuning.toml` edit must never be able to silently break an invariant. Those
//! are marked "fixed" in `tuning.html`.

use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Code defaults — the SINGLE source of the values the app ships with.
//
// These private consts are the one place the numbers are written. The `Default`
// impls below build the typed config from them (purely, with NO access to the
// process-global), and the loaded/merged runtime config in `TUNING` is layered
// on top of them. Keeping them here — not in hybrid.rs / preview.rs — is the
// whole point of the move: hybrid.rs and preview.rs now READ these (via the
// global) instead of declaring them.
// ---------------------------------------------------------------------------

/// S1 `annotation_chunk` vectors — your own words. Primary.
const FUSION_S1: f64 = 1.0;
/// S2 `event_fts` (FTS5 over the photographer's notes).
const FUSION_S2: f64 = 1.0;
/// Each S3 sub-list (`summaries_fts`, `image_summary` vectors). Held at 0.5
/// deliberately: summaries are DERIVED prose (the model's words, not the
/// photographer's) and must never outvote actual words (RETRIEVAL §5.3).
const FUSION_S3_EACH: f64 = 0.5;
/// S4 `image_clip`. Raised 0.5 → 1.0 (founder dogfood, June 12 2026): visual
/// evidence is not half a note. See `search/hybrid.rs` for the full rationale.
const FUSION_S4: f64 = 1.0;

/// RRF rank constant `k` (RETRIEVAL §5.3): `contribution = w/(k + rank)`.
///
/// `pub` and re-exported as `search::RRF_K` for API stability: it is the CODE
/// DEFAULT for `[search].rrf_k`. The LIVE value the fusion loop reads is
/// `tuning().search.rrf_k` (file-overridable); this const is what that resolves
/// to when no `tuning.toml` overrides it.
pub const RRF_K: f64 = 60.0;

/// Dense-signal similarity-tilt strength `β` (RETRIEVAL §5.3 amendment): the
/// multiplier on a dense signal's centered cosine spans [1−β, 1+β] around its
/// RRF baseline. See `search/hybrid.rs` for the full WHY.
///
/// `pub` and re-exported as `search::SIM_BLEND_BETA` for API stability: it is
/// the CODE DEFAULT for `[search].beta`. The live default `HybridOptions::beta`
/// is `tuning().search.beta` (file-overridable); this const is what that
/// resolves to absent an override.
pub const SIM_BLEND_BETA: f64 = 0.5;

/// Display-tier preview longest edge in px (LIBRARY §9).
const DISPLAY_EDGE: u32 = 2560;
/// §9.3 acceptability threshold: an embedded RAW preview is "good enough" to
/// skip a full decode when its longest edge is ≥ this many px.
const EMBEDDED_ACCEPT_EDGE: u32 = 2048;

// --- Heatmap (DESIGN-ATTENTION-HEATMAP.md) ---------------------------------
//
// The attention/engagement heatmap's knobs. Dwell leads; stroke-count is a
// small factor (founder dropped stroke "effort"/duration). The tier rates and
// the per-episode cap live HERE (not as literals in the record path) so the
// founder can re-feel "how much grid-select counts vs a Look-open" against the
// real library without a rebuild. Intensity weights are explicitly DEFAULTS,
// tuned later (DESIGN §3 "tuning pass on the weights").

/// `w_dwell`: weight on accumulated dwell-ms in the composite. Dwell leads
/// (DESIGN §"What attention is"), but a millisecond is a tiny unit next to a
/// whole event, so this is small — 60 s of capped dwell contributes ~6 to the
/// pre-normalization score, comparable to a handful of events.
const HEATMAP_W_DWELL: f64 = 0.0001;
/// `w_events`: weight on live event_count (remarks + ratings + strokes).
const HEATMAP_W_EVENTS: f64 = 1.0;
/// `w_strokes`: weight on live stroke_count — a SMALL extra nudge over the
/// event_count a stroke already contributes (founder: a bare count is enough).
const HEATMAP_W_STROKES: f64 = 0.5;
/// Look-open dwell tier: full weight (1.0x) — the strongest "I am focusing on
/// THIS" signal (DESIGN §"dwell capture").
const HEATMAP_DWELL_LOOK_RATE: f64 = 1.0;
/// Grid-select dwell tier: a small fraction of the Look rate — a grid click /
/// multi-select counts, but far less (DESIGN: ~0.1-0.2x).
const HEATMAP_DWELL_GRID_RATE: f64 = 0.15;
/// Per-episode-per-image dwell cap, ms (60 s — DESIGN: keeps lunch-break
/// walk-aways from skewing it; window-blur pause is the other half).
const HEATMAP_DWELL_CAP_MS: i64 = 60_000;
/// Recency half-life, days: when recency-weighting is on (the default), an
/// image's intensity is multiplied by `0.5^(age_days / half_life)`, so dwell /
/// annotation from `half_life` days ago counts half as much as today's.
const HEATMAP_RECENCY_HALF_LIFE_DAYS: f64 = 14.0;

// --- Validation bounds (sane ranges; an out-of-range loaded value is rejected
//     back to the default with a logged warning — never a silent bad number) ---

/// Fusion weights are non-negative multipliers; a negative weight would invert
/// a signal's vote, which is never a tuning intent.
const WEIGHT_MIN: f64 = 0.0;
/// Generous upper bound: a weight this large already swamps every other signal;
/// anything past it is a typo, not a tuning choice.
const WEIGHT_MAX: f64 = 1000.0;
/// `k` must be positive (it sits in the denominator `k + rank`); pin a sane
/// span so a fat-fingered 0 or a wild value can't distort every rank discount.
const RRF_K_MIN: f64 = 1.0;
const RRF_K_MAX: f64 = 10_000.0;
/// β is a centered-cosine multiplier; outside [0, 1] the blend can flip a
/// signal's sign or over-dominate the RRF skeleton.
const BETA_MIN: f64 = 0.0;
const BETA_MAX: f64 = 1.0;
/// Preview edges are pixel sizes: large enough to be a usable preview, small
/// enough not to be a typo'd multi-gigapixel target.
const EDGE_MIN: u32 = 256;
const EDGE_MAX: u32 = 16_384;
/// A dwell-tier rate is a fraction-or-multiple of raw elapsed time; outside
/// (0, 1] it would either erase dwell (0) or invent more than really elapsed.
const DWELL_RATE_MIN: f64 = 0.0; // exclusive lower bound enforced below
const DWELL_RATE_MAX: f64 = 1.0;
/// The dwell cap is a per-episode millisecond budget; 0 disables dwell and a
/// wild value defeats the walk-away guard. One day is a generous ceiling.
const DWELL_CAP_MIN_MS: i64 = 0;
const DWELL_CAP_MAX_MS: i64 = 86_400_000;
/// Recency half-life in days: must be positive (it sits in a divisor); a year
/// is effectively "flat" already, so cap there to catch typos.
const HALF_LIFE_MIN_DAYS: f64 = 0.0; // exclusive lower bound enforced below
const HALF_LIFE_MAX_DAYS: f64 = 365.0;

// ---------------------------------------------------------------------------
// The typed config, nested by domain.
// ---------------------------------------------------------------------------

/// §5.3 fusion weights. Defaults are the spec's — and explicitly defaults, not
/// findings: the §12 golden-set eval is the named gate for tuning them (which
/// is why they are data, not constants). The type is re-exported via
/// `search::hybrid`; the DEFAULT VALUES now live here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FusionWeights {
    /// S1 `annotation_chunk` vectors — primary.
    pub s1: f64,
    /// S2 `event_fts` (FTS5).
    pub s2: f64,
    /// Each S3 sub-list (`summaries_fts`, `image_summary` vectors).
    pub s3_each: f64,
    /// S4 `image_clip` (B69: always votes on semantic queries).
    pub s4: f64,
}

impl FusionWeights {
    /// The code default VALUES, built without touching the process-global —
    /// this is what `Default` (and `SearchTuning::default`, and the global's
    /// lazy fallback) resolve to. Kept separate from the `Default` impl so the
    /// global can be defined IN TERMS OF the defaults without a recursive
    /// `Default` → `tuning()` → `Default` cycle.
    const fn code_default() -> Self {
        Self {
            s1: FUSION_S1,
            s2: FUSION_S2,
            s3_each: FUSION_S3_EACH,
            s4: FUSION_S4,
        }
    }
}

impl Default for FusionWeights {
    /// Pulls from the active tuning config (file-overridable), falling back to
    /// the code defaults when nothing has been loaded — so tests and the eval,
    /// which never call [`init_from`], still get the exact shipped values.
    fn default() -> Self {
        tuning().search.fusion
    }
}

/// Search / hybrid-fusion tuning (RETRIEVAL §5.3). LIVE consumers: the fusion
/// loop and `HybridOptions::default()` in `search/hybrid.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchTuning {
    pub fusion: FusionWeights,
    /// RRF rank constant `k`.
    pub rrf_k: f64,
    /// Dense-signal similarity-tilt strength `β`.
    pub beta: f64,
}

impl Default for SearchTuning {
    fn default() -> Self {
        Self {
            // Build the weights from the code defaults directly (NOT via
            // `FusionWeights::default()`, which reads the global): this impl is
            // what the global's own fallback uses, so it must not recurse.
            fusion: FusionWeights::code_default(),
            rrf_k: RRF_K,
            beta: SIM_BLEND_BETA,
        }
    }
}

impl SearchTuning {
    /// Clamp-or-default each field. An out-of-range value is dropped back to
    /// the code default with a warning (never a silent bad number).
    fn validated(self) -> Self {
        let d = Self::default();
        SearchTuning {
            fusion: FusionWeights {
                s1: range_or_default(
                    "search.fusion.s1",
                    self.fusion.s1,
                    WEIGHT_MIN,
                    WEIGHT_MAX,
                    d.fusion.s1,
                ),
                s2: range_or_default(
                    "search.fusion.s2",
                    self.fusion.s2,
                    WEIGHT_MIN,
                    WEIGHT_MAX,
                    d.fusion.s2,
                ),
                s3_each: range_or_default(
                    "search.fusion.s3_each",
                    self.fusion.s3_each,
                    WEIGHT_MIN,
                    WEIGHT_MAX,
                    d.fusion.s3_each,
                ),
                s4: range_or_default(
                    "search.fusion.s4",
                    self.fusion.s4,
                    WEIGHT_MIN,
                    WEIGHT_MAX,
                    d.fusion.s4,
                ),
            },
            rrf_k: range_or_default("search.rrf_k", self.rrf_k, RRF_K_MIN, RRF_K_MAX, d.rrf_k),
            beta: range_or_default("search.beta", self.beta, BETA_MIN, BETA_MAX, d.beta),
        }
    }
}

/// Preview-pipeline tuning (LIBRARY §9). LIVE consumers: `write_artifacts`
/// (display edge) and the embedded-preview accept decision in `library/mod.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewTuning {
    /// Display-tier preview longest edge, px.
    pub display_edge: u32,
    /// Embedded-preview acceptability threshold (longest edge ≥ this), px.
    pub embedded_accept_edge: u32,
}

impl Default for PreviewTuning {
    fn default() -> Self {
        Self {
            display_edge: DISPLAY_EDGE,
            embedded_accept_edge: EMBEDDED_ACCEPT_EDGE,
        }
    }
}

impl PreviewTuning {
    fn validated(self) -> Self {
        let d = Self::default();
        PreviewTuning {
            display_edge: edge_or_default(
                "preview.display_edge",
                self.display_edge,
                d.display_edge,
            ),
            embedded_accept_edge: edge_or_default(
                "preview.embedded_accept_edge",
                self.embedded_accept_edge,
                d.embedded_accept_edge,
            ),
        }
    }
}

/// Attention/engagement heatmap tuning (DESIGN-ATTENTION-HEATMAP.md). LIVE
/// consumers: `EventStore::record_dwell` (tier rate + cap) and
/// `EventStore::image_intensity` (composite weights + recency half-life). The
/// weights/rates/cap are config, not literals, so the founder re-feels them
/// against the real library without a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeatmapTuning {
    /// Weight on accumulated (capped) dwell-ms in the composite. Dwell leads.
    pub w_dwell: f64,
    /// Weight on live event_count.
    pub w_events: f64,
    /// Weight on live stroke_count (a small extra factor).
    pub w_strokes: f64,
    /// Look-open dwell tier (full weight, 1.0).
    pub dwell_look_rate: f64,
    /// Grid-select dwell tier (a small fraction of the Look rate).
    pub dwell_grid_rate: f64,
    /// Per-episode-per-image dwell cap, ms.
    pub dwell_cap_ms: i64,
    /// Recency decay half-life, days (recency-weighted mode only).
    pub recency_half_life_days: f64,
}

impl Default for HeatmapTuning {
    fn default() -> Self {
        Self {
            w_dwell: HEATMAP_W_DWELL,
            w_events: HEATMAP_W_EVENTS,
            w_strokes: HEATMAP_W_STROKES,
            dwell_look_rate: HEATMAP_DWELL_LOOK_RATE,
            dwell_grid_rate: HEATMAP_DWELL_GRID_RATE,
            dwell_cap_ms: HEATMAP_DWELL_CAP_MS,
            recency_half_life_days: HEATMAP_RECENCY_HALF_LIFE_DAYS,
        }
    }
}

impl HeatmapTuning {
    fn validated(self) -> Self {
        let d = Self::default();
        HeatmapTuning {
            w_dwell: range_or_default(
                "heatmap.w_dwell",
                self.w_dwell,
                WEIGHT_MIN,
                WEIGHT_MAX,
                d.w_dwell,
            ),
            w_events: range_or_default(
                "heatmap.w_events",
                self.w_events,
                WEIGHT_MIN,
                WEIGHT_MAX,
                d.w_events,
            ),
            w_strokes: range_or_default(
                "heatmap.w_strokes",
                self.w_strokes,
                WEIGHT_MIN,
                WEIGHT_MAX,
                d.w_strokes,
            ),
            // Rates: (0, 1]. A 0 rate (or NaN/inf) snaps back — a tier that
            // erases dwell is never the intent; a rate > 1 invents time.
            dwell_look_rate: rate_or_default(
                "heatmap.dwell_look_rate",
                self.dwell_look_rate,
                d.dwell_look_rate,
            ),
            dwell_grid_rate: rate_or_default(
                "heatmap.dwell_grid_rate",
                self.dwell_grid_rate,
                d.dwell_grid_rate,
            ),
            dwell_cap_ms: if (DWELL_CAP_MIN_MS..=DWELL_CAP_MAX_MS).contains(&self.dwell_cap_ms) {
                self.dwell_cap_ms
            } else {
                tracing::warn!(
                    field = "heatmap.dwell_cap_ms",
                    value = self.dwell_cap_ms,
                    "tuning value out of range; keeping default"
                );
                d.dwell_cap_ms
            },
            // Half-life: positive and finite (it divides). A 0 or absurd value
            // snaps back so the decay can never go NaN/inf or never decay.
            recency_half_life_days: half_life_or_default(
                "heatmap.recency_half_life_days",
                self.recency_half_life_days,
                d.recency_half_life_days,
            ),
        }
    }
}

/// The whole tuning surface, one section per domain. The graph section will be
/// added by that feature when it lands (its design doc already names the
/// knobs); we add no empty dead sections here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub search: SearchTuning,
    pub preview: PreviewTuning,
    pub heatmap: HeatmapTuning,
}

impl Tuning {
    /// Read `<app_data>/tuning.toml` if present and merge it over the code
    /// defaults; a missing file yields pure defaults. Every field is
    /// range-validated — an out-of-range value logs a warning and keeps the
    /// default, so a hand-edited file can never inject a silent bad number.
    pub fn load(app_data: &Path) -> Tuning {
        let path = app_data.join("tuning.toml");
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            // Missing file is the common case: ship-defaults, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Tuning::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "tuning.toml unreadable; using defaults");
                return Tuning::default();
            }
        };
        // `#[serde(default)]` makes this a partial merge: any field the file
        // omits falls back to its code default.
        match toml::from_str::<Tuning>(&raw) {
            Ok(parsed) => parsed.validated(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "tuning.toml malformed; using defaults");
                Tuning::default()
            }
        }
    }

    /// Range-validate every section (clamp-or-default with a logged warning).
    fn validated(self) -> Tuning {
        Tuning {
            search: self.search.validated(),
            preview: self.preview.validated(),
            heatmap: self.heatmap.validated(),
        }
    }
}

// ---------------------------------------------------------------------------
// Process-global access.
// ---------------------------------------------------------------------------

/// Set once at startup by the desktop shell ([`init_from`]); unset under tests
/// and the eval, where the lazy fallback returns the code defaults.
static TUNING: OnceLock<Tuning> = OnceLock::new();

/// The active tuning config. Initialized once at startup from `tuning.toml`;
/// before that (tests, the eval, library code constructed directly) it returns
/// the code defaults — which are byte-identical to the values these knobs held
/// as scattered consts, so nothing changes by construction.
pub fn tuning() -> &'static Tuning {
    // `get_or_init` gives a stable default if `init_from` was never called,
    // without forcing every consumer to thread an app handle.
    TUNING.get_or_init(Tuning::default)
}

/// Load `<app_data>/tuning.toml` and install it as the process tuning. Call
/// ONCE at startup, BEFORE any search/preview runs. A second call is a no-op
/// (the first install wins) and logs, so a stray re-init can't silently swap
/// the live config mid-run.
pub fn init_from(app_data: &Path) {
    let loaded = Tuning::load(app_data);
    if TUNING.set(loaded).is_err() {
        tracing::warn!("tuning already initialized; ignoring re-init");
    }
}

// ---------------------------------------------------------------------------
// Validation helpers — clamp-to-default with a logged warning.
// ---------------------------------------------------------------------------

/// Keep `v` if it's a finite value within [min, max]; otherwise warn and return
/// `default`. NaN/inf are rejected too (a `tuning.toml` typo like `1.0e999`).
fn range_or_default(field: &str, v: f64, min: f64, max: f64, default: f64) -> f64 {
    if v.is_finite() && (min..=max).contains(&v) {
        v
    } else {
        tracing::warn!(
            field,
            value = v,
            min,
            max,
            "tuning value out of range; keeping default"
        );
        default
    }
}

fn edge_or_default(field: &str, v: u32, default: u32) -> u32 {
    if (EDGE_MIN..=EDGE_MAX).contains(&v) {
        v
    } else {
        tracing::warn!(
            field,
            value = v,
            min = EDGE_MIN,
            max = EDGE_MAX,
            "tuning edge out of range; keeping default"
        );
        default
    }
}

/// A dwell-tier rate: finite, in the OPEN-LOW interval (0, 1]. A 0 erases the
/// tier's dwell entirely (never a tuning intent) and > 1 invents elapsed time.
fn rate_or_default(field: &str, v: f64, default: f64) -> f64 {
    if v.is_finite() && v > DWELL_RATE_MIN && v <= DWELL_RATE_MAX {
        v
    } else {
        tracing::warn!(
            field,
            value = v,
            "tuning dwell rate out of range (0, 1]; keeping default"
        );
        default
    }
}

/// Recency half-life in days: finite and strictly positive (it divides), with
/// a sane upper cap. A non-positive value would make the decay NaN/inf.
fn half_life_or_default(field: &str, v: f64, default: f64) -> f64 {
    if v.is_finite() && v > HALF_LIFE_MIN_DAYS && v <= HALF_LIFE_MAX_DAYS {
        v
    } else {
        tracing::warn!(
            field,
            value = v,
            "tuning half-life out of range; keeping default"
        );
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract that makes this refactor a no-op: the code defaults equal
    /// the exact values the old scattered consts held. If anyone edits a
    /// default, this test (and the documented `tuning.default.toml`) must move
    /// in lockstep — that is the whole guardrail.
    #[test]
    fn default_equals_documented_values() {
        let t = Tuning::default();
        assert_eq!(t.search.fusion.s1, 1.0);
        assert_eq!(t.search.fusion.s2, 1.0);
        assert_eq!(t.search.fusion.s3_each, 0.5);
        assert_eq!(t.search.fusion.s4, 1.0);
        assert_eq!(t.search.rrf_k, 60.0);
        assert_eq!(t.search.beta, 0.5);
        assert_eq!(t.preview.display_edge, 2560);
        assert_eq!(t.preview.embedded_accept_edge, 2048);
        // Heatmap defaults (DESIGN-ATTENTION-HEATMAP.md): dwell leads, the
        // grid tier is a small fraction of Look, 60 s cap, 14-day half-life.
        assert_eq!(t.heatmap.w_dwell, 0.0001);
        assert_eq!(t.heatmap.w_events, 1.0);
        assert_eq!(t.heatmap.w_strokes, 0.5);
        assert_eq!(t.heatmap.dwell_look_rate, 1.0);
        assert_eq!(t.heatmap.dwell_grid_rate, 0.15);
        assert_eq!(t.heatmap.dwell_cap_ms, 60_000);
        assert_eq!(t.heatmap.recency_half_life_days, 14.0);
    }

    /// A partial `[heatmap]` file sets one knob and leaves the rest at their
    /// code defaults (the `#[serde(default)]` merge), and the other sections
    /// are untouched.
    #[test]
    fn partial_heatmap_toml_merges_over_defaults() {
        let toml = r#"
            [heatmap]
            dwell_grid_rate = 0.25
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        assert_eq!(merged.heatmap.dwell_grid_rate, 0.25);
        // Everything else in the section stayed default:
        assert_eq!(merged.heatmap.dwell_look_rate, 1.0);
        assert_eq!(merged.heatmap.dwell_cap_ms, 60_000);
        assert_eq!(merged.heatmap.w_dwell, 0.0001);
        // And the unrelated sections are pure defaults:
        assert_eq!(merged.search, SearchTuning::default());
        assert_eq!(merged.preview, PreviewTuning::default());
    }

    /// Out-of-range heatmap values reject back to defaults (never a silent bad
    /// number): a 0 rate, a > 1 rate, a non-positive half-life, an absurd cap.
    #[test]
    fn out_of_range_heatmap_values_reject_to_default() {
        let toml = r#"
            [heatmap]
            dwell_look_rate = 0.0
            dwell_grid_rate = 5.0
            recency_half_life_days = -1.0
            dwell_cap_ms = -10
            w_dwell = -1.0
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        assert_eq!(merged.heatmap.dwell_look_rate, 1.0); // 0 rejected
        assert_eq!(merged.heatmap.dwell_grid_rate, 0.15); // > 1 rejected
        assert_eq!(merged.heatmap.recency_half_life_days, 14.0); // <= 0 rejected
        assert_eq!(merged.heatmap.dwell_cap_ms, 60_000); // negative rejected
        assert_eq!(merged.heatmap.w_dwell, 0.0001); // negative weight rejected
    }

    /// A partial file merges over defaults: it sets one field and leaves the
    /// rest at their code defaults (the `#[serde(default)]` contract).
    #[test]
    fn partial_toml_merges_over_defaults() {
        let toml = r#"
            [search]
            rrf_k = 42.0
        "#;
        let parsed: Tuning = toml::from_str(toml).unwrap();
        let merged = parsed.validated();
        // The one set field took:
        assert_eq!(merged.search.rrf_k, 42.0);
        // Everything else stayed at the code default:
        assert_eq!(merged.search.beta, 0.5);
        assert_eq!(merged.search.fusion, FusionWeights::code_default());
        assert_eq!(merged.preview, PreviewTuning::default());
    }

    /// A partial weight override inside `[search]` keeps the other weights.
    #[test]
    fn partial_fusion_weight_merges() {
        let toml = r#"
            [search.fusion]
            s4 = 2.0
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        assert_eq!(merged.search.fusion.s4, 2.0);
        assert_eq!(merged.search.fusion.s1, 1.0);
        assert_eq!(merged.search.fusion.s3_each, 0.5);
    }

    /// An out-of-range value is REJECTED back to the default (never a silent
    /// bad number) — a negative weight and an absurd edge both snap back.
    #[test]
    fn out_of_range_values_reject_to_default() {
        let toml = r#"
            [search]
            beta = 5.0

            [search.fusion]
            s1 = -1.0

            [preview]
            display_edge = 1
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        // β > 1 rejected:
        assert_eq!(merged.search.beta, 0.5);
        // negative weight rejected:
        assert_eq!(merged.search.fusion.s1, 1.0);
        // tiny edge rejected:
        assert_eq!(merged.preview.display_edge, 2560);
    }

    /// A NaN/inf from a malformed float is rejected like any out-of-range value.
    #[test]
    fn non_finite_rejects_to_default() {
        assert_eq!(range_or_default("x", f64::NAN, 0.0, 1.0, 0.5), 0.5);
        assert_eq!(range_or_default("x", f64::INFINITY, 0.0, 1.0, 0.5), 0.5);
    }
}
