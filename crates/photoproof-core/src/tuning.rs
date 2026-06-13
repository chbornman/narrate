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

/// The whole tuning surface, one section per domain. Heatmap and graph
/// sections will be added by those features when they land (their design docs
/// already name the knobs); we add no empty dead sections here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub search: SearchTuning,
    pub preview: PreviewTuning,
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
