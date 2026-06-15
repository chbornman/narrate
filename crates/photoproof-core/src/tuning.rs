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

// --- Semantic topic-graph defaults (DESIGN-SEMANTIC-GRAPH.md) ---
//
// The force-directed lens' knobs. ALL are physics/layout tunables the founder
// tunes by feel — so they live here, file-overridable, never scattered consts
// in the frontend or a new const block. The frontend reads them through the
// `graph_tuning` command (which returns `tuning().graph`); the affinity BLEND
// default α also gates the backend's `topic_affinities` when a caller passes
// none, so search and the graph share one blend model.

/// α — the looks-vs-said blend default (0 = pure annotation, 1 = pure visual).
/// 0.5 starts neutral (DESIGN open decision: "50/50 start?").
const GRAPH_ALPHA_DEFAULT: f64 = 0.5;
/// Attraction stiffness: an image's pull toward a topic anchor scales with this
/// times its blended affinity to that topic. Higher = tighter clusters.
/// Raised 0.02 -> 0.08 (founder, June 2026: "attraction to topics needs to be
/// stronger... images should mostly congregate around topics"): topic affinity
/// (a CLIP/note cosine) is modest, so a small stiffness left images loose in the
/// middle; this pulls a well-matched image decisively onto its topic while a
/// weakly-matched one (small affinity) still drifts free to cluster by semantics.
const GRAPH_ATTRACTION: f64 = 0.08;
/// Semantic-neighbor spring stiffness (CLIP/note similarity): how hard alike
/// photos draw together. Lowered relative to the topic `attraction` (founder:
/// "attraction to other images needs to be lessened") so topics WIN for matched
/// images and the neighbor pull only clumps the un-topic'd remainder. The
/// frontend force sim reads this as `neighborAttraction`.
const GRAPH_NEIGHBOR_ATTRACTION: f64 = 0.03;
/// Rest length (px) of the semantic-neighbor spring: neighbors pull together only
/// while farther apart than this, so a clump packs to a legible disc instead of
/// collapsing. The frontend reads this as `neighborRestLength`.
const GRAPH_NEIGHBOR_REST_LENGTH: f64 = 40.0;
/// Mutual image-image repulsion strength (an inverse-square-ish spread force),
/// so dense clusters don't collapse to a point.
const GRAPH_REPULSION: f64 = 800.0;
/// Per-step velocity damping (velocity-Verlet friction): the sim cools toward a
/// stable layout instead of oscillating forever.
const GRAPH_DAMPING: f64 = 0.85;
/// Centering pull toward the origin, so an image related to no topic drifts to
/// the middle rather than flying off the canvas.
const GRAPH_CENTERING: f64 = 0.01;
/// Topic-anchor ring radius (px) in sim space. The ring is now only the INITIAL
/// layout: anchors are FORCE-PLACED (founder dogfood, June 2026) and relax from
/// here toward their images' centroids.
const GRAPH_RING_RADIUS: f64 = 320.0;
/// Anchor attraction stiffness (founder dogfood): pull on a topic anchor toward
/// the affinity-weighted centroid of the images that hold to it. Higher = two
/// topics sharing many images snap together faster. Larger than the image
/// `attraction` so anchors visibly move within the reheat's ~1 s.
const GRAPH_ANCHOR_ATTRACTION: f64 = 0.08;
/// Mutual anchor-anchor repulsion (founder dogfood): keeps force-placed anchors
/// from collapsing onto each other even when they share most images. Much larger
/// than the image `repulsion` so a handful of topics stay legibly spread.
const GRAPH_ANCHOR_REPULSION: f64 = 60_000.0;
/// Per-step velocity damping for the anchors (their own cooling). A touch
/// heavier than the image `damping` so the heavy, centroid-seeking anchors
/// settle without oscillating.
const GRAPH_ANCHOR_DAMPING: f64 = 0.8;
/// v2 cluster auto-labels — k bounds for `cluster_topics`' k-means. The cluster
/// count is `clamp(round(sqrt(n/2)), k_min, k_max)` unless a `k` is passed: a
/// handful of images yields `k_min` clusters; a large scope is capped at `k_max`
/// so the suggestion rail stays short and the k-means cheap.
const GRAPH_CLUSTER_K_MIN: u32 = 2;
const GRAPH_CLUSTER_K_MAX: u32 = 12;
/// v2 full-library LOD — the node count past which the lens AGGREGATES images
/// into super-nodes instead of rendering every one (DESIGN scale section).
/// Default 700 (visualizer audit, June 2026): the live force sim is only
/// numerically comfortable in the low hundreds, so scopes past ~700 aggregate
/// into super-nodes that settle quickly, keeping the un-aggregated O(N^2) sim
/// inside its stable, cheap band. (Was 1500 — chosen vs the v1 banner strain
/// point, but that left the 700-1500 "hundreds of images" band running the full
/// sim, squarely where it diverged and repainted forever.) Pairs with the
/// per-step displacement clamp + per-body settle test that bound the sim itself.
const GRAPH_LOD_THRESHOLD: u32 = 700;

// --- Voice endpoint/VAD feel dials (DESIGN-TUNING-LOOP.md "voice" arm) -------
//
// The voice DIALS lifted out of `pp-asr-server`'s consts and the silero/engine
// consts into config, so a swept winner (pp-sweep voice) is a FILE the app
// reads — exactly like the search weights. These are FEEL knobs: how snappy a
// final lands after you stop talking, how eager the gate opens, how much
// cold-start audio is replayed. They are NOT the timing CONTRACTS: the onset
// error budget (`ONSET_ERROR_BUDGET_MS`), the association skew bound
// (`ASSOCIATION_MAX_SKEW_MS`), the drain window (`DRAIN_WINDOW_MS`), and the
// trailing-ship window (`TRAILING_SHIP_MS`) stay FIXED consts in
// `capture/engine.rs` — they are correctness invariants (a wrong target / a
// lost trailing final), and a tuning.toml edit must never be able to break one.
//
// Defaults are BYTE-IDENTICAL to today's shipped const/flag values so lifting
// them changes nothing until a `[voice]` section overrides one (proven by the
// behavior-unchanged gate test below).

/// Endpoint rule 1 (CAPTURE §6.3): minimum trailing silence, in SECONDS, when
/// NOTHING has been decoded yet — the dead-air endpoint. sherpa's canonical
/// default; today `pp-asr-server`'s `DEFAULT_RULE1_TRAILING_SILENCE_S`.
const VOICE_RULE1_S: f64 = 2.4;
/// Endpoint rule 2 (CAPTURE §6.3): minimum trailing silence, in SECONDS, AFTER
/// decoded speech — the post-utterance latency a user feels after they stop.
/// THE primary snappiness dial; today `DEFAULT_RULE2_TRAILING_SILENCE_S`.
const VOICE_RULE2_S: f64 = 1.2;
/// Endpoint rule 3 (CAPTURE §6.3): minimum utterance length, in SECONDS, that
/// forces an endpoint regardless of silence; today `DEFAULT_RULE3_MIN_UTTERANCE_S`.
const VOICE_RULE3_S: f64 = 20.0;
/// silero ENTER threshold: speech-probability a window must reach to OPEN the
/// gate (a fraction). Today `silero.rs`'s `ENTER`.
const VOICE_VAD_ENTER: f64 = 0.5;
/// silero EXIT threshold: speech-probability a window must DROP BELOW (for the
/// hang count) to close the gate. Today `silero.rs`'s `EXIT`.
const VOICE_VAD_EXIT: f64 = 0.35;
/// silero HANG: consecutive below-EXIT windows (each 32 ms) before SpeechEnd —
/// 15 × 32 ms = 480 ms hang. Today `silero.rs`'s `HANG_WINDOWS`. A u32 count.
const VOICE_VAD_HANG: u32 = 15;
/// §6.2 pre-roll, ms: silence-gated frames retained and flushed when shipping
/// resumes, so a cold-start first word is not chopped. Today the capture
/// engine's `PRE_ROLL_MS`.
const VOICE_PRE_ROLL_MS: u64 = 1_000;

// Validation bounds for the voice dials (clamp-or-default like every section).
/// Endpoint rules are trailing-silence / utterance-length SECONDS. A negative
/// or zero rule would disable endpointing (never finalize) or fire instantly
/// (truncate every word); a huge one would never finalize. Span a sane window:
/// from a tenth of a second to a minute. rule3 (utterance cap) lives in the
/// same span — 60 s is already "effectively never forced".
const VOICE_RULE_MIN_S: f64 = 0.1;
const VOICE_RULE_MAX_S: f64 = 60.0;
/// VAD thresholds are speech probabilities in [0, 1]. The RAW-feed sweep uses
/// 0.0 (gate forced open), so 0 is a legal in-band value here.
const VOICE_VAD_PROB_MIN: f64 = 0.0;
const VOICE_VAD_PROB_MAX: f64 = 1.0;
/// HANG is a window count: at least 1 (zero would close the gate the instant
/// speech dips, shredding utterances), capped so a typo cannot hang the gate
/// open for minutes. 300 windows = ~9.6 s, already absurdly long.
const VOICE_VAD_HANG_MIN: u32 = 1;
const VOICE_VAD_HANG_MAX: u32 = 300;
/// Pre-roll ms: 0 disables it (legal — the pre-grace behavior); cap at 5 s so a
/// typo cannot replay a huge stretch of stale audio into the recognizer.
const VOICE_PRE_ROLL_MIN_MS: u64 = 0;
const VOICE_PRE_ROLL_MAX_MS: u64 = 5_000;

// Validation bounds for the graph knobs (clamp-or-default like every other
// section: a hand-edited tuning.toml can never inject a silent bad number).
/// α is a blend fraction; outside [0, 1] it would flip or over-weight a space.
const GRAPH_ALPHA_MIN: f64 = 0.0;
const GRAPH_ALPHA_MAX: f64 = 1.0;
/// Force/length knobs are positive magnitudes; a generous span catches typos
/// (a negative force would invert the layout; an absurd one would explode it).
const GRAPH_FORCE_MIN: f64 = 0.0;
const GRAPH_FORCE_MAX: f64 = 100_000.0;
/// Damping is a per-step retain fraction in (0, 1]; 0 freezes instantly, >1
/// injects energy and diverges.
const GRAPH_DAMPING_MIN: f64 = 0.0;
const GRAPH_DAMPING_MAX: f64 = 1.0;
/// Cluster k bounds: at least 1 cluster, capped at a readable rail width. A k of
/// 0 would yield no clusters; an absurd k would over-fragment the rail.
const GRAPH_CLUSTER_K_BOUND_MIN: u32 = 1;
const GRAPH_CLUSTER_K_BOUND_MAX: u32 = 64;
/// LOD threshold bounds: at least a handful (below ~50 there is nothing to
/// aggregate); a generous ceiling catches a typo without freezing aggregation
/// off on a real library.
const GRAPH_LOD_MIN: u32 = 50;
const GRAPH_LOD_MAX: u32 = 1_000_000;

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

/// Semantic topic-graph tuning (DESIGN-SEMANTIC-GRAPH.md). LIVE consumers: the
/// `topic_affinities` blend default (`alpha`) and the frontend force sim, which
/// reads every field through the `graph_tuning` command. Defaults are starting
/// points the founder tunes by feel — that is exactly why they live here,
/// file-overridable, rather than as frontend consts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphTuning {
    /// Looks-vs-said blend default (0 = annotation only, 1 = visual only).
    pub alpha_default: f64,
    /// Attraction stiffness toward a topic anchor (× blended affinity).
    pub attraction: f64,
    /// Mutual image-image repulsion strength.
    pub repulsion: f64,
    /// Semantic-neighbor spring stiffness (CLIP/note similarity) — how hard alike
    /// photos draw together (the frontend's `neighborAttraction`).
    pub neighbor_attraction: f64,
    /// Rest length (px) of the semantic-neighbor spring (the frontend's
    /// `neighborRestLength`).
    pub neighbor_rest_length: f64,
    /// Per-step velocity damping (the sim's cooling).
    pub damping: f64,
    /// Centering pull toward the origin.
    pub centering: f64,
    /// Topic-anchor ring radius in sim-space px (the INITIAL anchor layout).
    pub ring_radius: f64,
    /// Anchor attraction toward its images' affinity-weighted centroid (anchors
    /// are force-placed; related topics drift together).
    pub anchor_attraction: f64,
    /// Mutual anchor-anchor repulsion (so force-placed anchors don't collapse).
    pub anchor_repulsion: f64,
    /// Per-step velocity damping for the anchors (their own cooling).
    pub anchor_damping: f64,
    /// v2 cluster auto-labels — minimum k for `cluster_topics`' k-means.
    pub cluster_k_min: u32,
    /// v2 cluster auto-labels — maximum k (caps the suggestion rail width).
    pub cluster_k_max: u32,
    /// v2 full-library LOD — the node count past which the lens aggregates
    /// images into super-nodes instead of rendering every one.
    pub lod_threshold: u32,
}

impl Default for GraphTuning {
    fn default() -> Self {
        Self {
            alpha_default: GRAPH_ALPHA_DEFAULT,
            attraction: GRAPH_ATTRACTION,
            repulsion: GRAPH_REPULSION,
            neighbor_attraction: GRAPH_NEIGHBOR_ATTRACTION,
            neighbor_rest_length: GRAPH_NEIGHBOR_REST_LENGTH,
            damping: GRAPH_DAMPING,
            centering: GRAPH_CENTERING,
            ring_radius: GRAPH_RING_RADIUS,
            anchor_attraction: GRAPH_ANCHOR_ATTRACTION,
            anchor_repulsion: GRAPH_ANCHOR_REPULSION,
            anchor_damping: GRAPH_ANCHOR_DAMPING,
            cluster_k_min: GRAPH_CLUSTER_K_MIN,
            cluster_k_max: GRAPH_CLUSTER_K_MAX,
            lod_threshold: GRAPH_LOD_THRESHOLD,
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

/// Voice endpoint/VAD feel tuning (DESIGN-TUNING-LOOP.md "voice" arm). LIVE
/// consumers: the ASR-wrapper launcher (`runtime::launch::asr_wrapper_args`
/// passes `rule1/2/3` to `pp-asr-server`), the shell's silero VAD construction
/// (`enter`/`exit`/`hang`), and the capture engine's pre-roll cap (`pre_roll_ms`).
/// These are DIALS the founder sweeps by feel (pp-sweep voice); the timing
/// CONTRACTS (onset budget, association skew, drain + trailing-ship windows)
/// stay fixed consts in `capture/engine.rs` — they are correctness invariants,
/// not feel dials.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceTuning {
    /// Endpoint rule 1: trailing-silence seconds with nothing decoded yet.
    pub rule1: f64,
    /// Endpoint rule 2: trailing-silence seconds after decoded speech (the
    /// primary snappiness dial).
    pub rule2: f64,
    /// Endpoint rule 3: utterance-length seconds that force an endpoint.
    pub rule3: f64,
    /// silero ENTER threshold (open the gate at this speech-probability).
    pub vad_enter: f64,
    /// silero EXIT threshold (close the gate below this, for `vad_hang` windows).
    pub vad_exit: f64,
    /// silero HANG: below-EXIT windows (32 ms each) before SpeechEnd.
    pub vad_hang: u32,
    /// §6.2 pre-roll, ms: withheld silence-gated audio replayed on resume.
    pub pre_roll_ms: u64,
}

impl Default for VoiceTuning {
    fn default() -> Self {
        Self {
            rule1: VOICE_RULE1_S,
            rule2: VOICE_RULE2_S,
            rule3: VOICE_RULE3_S,
            vad_enter: VOICE_VAD_ENTER,
            vad_exit: VOICE_VAD_EXIT,
            vad_hang: VOICE_VAD_HANG,
            pre_roll_ms: VOICE_PRE_ROLL_MS,
        }
    }
}

impl VoiceTuning {
    fn validated(self) -> Self {
        let d = Self::default();
        VoiceTuning {
            rule1: range_or_default(
                "voice.rule1",
                self.rule1,
                VOICE_RULE_MIN_S,
                VOICE_RULE_MAX_S,
                d.rule1,
            ),
            rule2: range_or_default(
                "voice.rule2",
                self.rule2,
                VOICE_RULE_MIN_S,
                VOICE_RULE_MAX_S,
                d.rule2,
            ),
            rule3: range_or_default(
                "voice.rule3",
                self.rule3,
                VOICE_RULE_MIN_S,
                VOICE_RULE_MAX_S,
                d.rule3,
            ),
            vad_enter: range_or_default(
                "voice.vad_enter",
                self.vad_enter,
                VOICE_VAD_PROB_MIN,
                VOICE_VAD_PROB_MAX,
                d.vad_enter,
            ),
            vad_exit: range_or_default(
                "voice.vad_exit",
                self.vad_exit,
                VOICE_VAD_PROB_MIN,
                VOICE_VAD_PROB_MAX,
                d.vad_exit,
            ),
            vad_hang: count_or_default(
                "voice.vad_hang",
                self.vad_hang,
                VOICE_VAD_HANG_MIN,
                VOICE_VAD_HANG_MAX,
                d.vad_hang,
            ),
            pre_roll_ms: if (VOICE_PRE_ROLL_MIN_MS..=VOICE_PRE_ROLL_MAX_MS)
                .contains(&self.pre_roll_ms)
            {
                self.pre_roll_ms
            } else {
                tracing::warn!(
                    field = "voice.pre_roll_ms",
                    value = self.pre_roll_ms,
                    min = VOICE_PRE_ROLL_MIN_MS,
                    max = VOICE_PRE_ROLL_MAX_MS,
                    "tuning value out of range; keeping default"
                );
                d.pre_roll_ms
            },
        }
    }
}

impl GraphTuning {
    fn validated(self) -> Self {
        let d = Self::default();
        GraphTuning {
            alpha_default: range_or_default(
                "graph.alpha_default",
                self.alpha_default,
                GRAPH_ALPHA_MIN,
                GRAPH_ALPHA_MAX,
                d.alpha_default,
            ),
            attraction: range_or_default(
                "graph.attraction",
                self.attraction,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.attraction,
            ),
            repulsion: range_or_default(
                "graph.repulsion",
                self.repulsion,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.repulsion,
            ),
            neighbor_attraction: range_or_default(
                "graph.neighbor_attraction",
                self.neighbor_attraction,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.neighbor_attraction,
            ),
            neighbor_rest_length: range_or_default(
                "graph.neighbor_rest_length",
                self.neighbor_rest_length,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.neighbor_rest_length,
            ),
            damping: range_or_default(
                "graph.damping",
                self.damping,
                GRAPH_DAMPING_MIN,
                GRAPH_DAMPING_MAX,
                d.damping,
            ),
            centering: range_or_default(
                "graph.centering",
                self.centering,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.centering,
            ),
            ring_radius: range_or_default(
                "graph.ring_radius",
                self.ring_radius,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.ring_radius,
            ),
            anchor_attraction: range_or_default(
                "graph.anchor_attraction",
                self.anchor_attraction,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.anchor_attraction,
            ),
            anchor_repulsion: range_or_default(
                "graph.anchor_repulsion",
                self.anchor_repulsion,
                GRAPH_FORCE_MIN,
                GRAPH_FORCE_MAX,
                d.anchor_repulsion,
            ),
            anchor_damping: range_or_default(
                "graph.anchor_damping",
                self.anchor_damping,
                GRAPH_DAMPING_MIN,
                GRAPH_DAMPING_MAX,
                d.anchor_damping,
            ),
            cluster_k_min: count_or_default(
                "graph.cluster_k_min",
                self.cluster_k_min,
                GRAPH_CLUSTER_K_BOUND_MIN,
                GRAPH_CLUSTER_K_BOUND_MAX,
                d.cluster_k_min,
            ),
            cluster_k_max: count_or_default(
                "graph.cluster_k_max",
                self.cluster_k_max,
                GRAPH_CLUSTER_K_BOUND_MIN,
                GRAPH_CLUSTER_K_BOUND_MAX,
                d.cluster_k_max,
            ),
            lod_threshold: count_or_default(
                "graph.lod_threshold",
                self.lod_threshold,
                GRAPH_LOD_MIN,
                GRAPH_LOD_MAX,
                d.lod_threshold,
            ),
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

/// The whole tuning surface, one section per domain. Graph and heatmap sections
/// are both live; a new feature adds its section here (its design doc names the
/// knobs); we add no empty dead sections here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub search: SearchTuning,
    pub preview: PreviewTuning,
    pub graph: GraphTuning,
    pub heatmap: HeatmapTuning,
    pub voice: VoiceTuning,
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
            graph: self.graph.validated(),
            heatmap: self.heatmap.validated(),
            voice: self.voice.validated(),
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

/// A u32 count knob clamped to `[min, max]` (the graph cluster-k / LOD-threshold
/// bounds). Out of range snaps back to the default with a warning, like every
/// other knob.
fn count_or_default(field: &str, v: u32, min: u32, max: u32, default: u32) -> u32 {
    if (min..=max).contains(&v) {
        v
    } else {
        tracing::warn!(
            field,
            value = v,
            min,
            max,
            "tuning count out of range; keeping default"
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
        // Graph (DESIGN-SEMANTIC-GRAPH.md) — must match tuning.default.toml.
        assert_eq!(t.graph.alpha_default, 0.5);
        assert_eq!(t.graph.attraction, 0.08);
        assert_eq!(t.graph.repulsion, 800.0);
        assert_eq!(t.graph.neighbor_attraction, 0.03);
        assert_eq!(t.graph.neighbor_rest_length, 40.0);
        assert_eq!(t.graph.damping, 0.85);
        assert_eq!(t.graph.centering, 0.01);
        assert_eq!(t.graph.ring_radius, 320.0);
        // Force-placed anchor knobs (founder dogfood, June 2026).
        assert_eq!(t.graph.anchor_attraction, 0.08);
        assert_eq!(t.graph.anchor_repulsion, 60_000.0);
        assert_eq!(t.graph.anchor_damping, 0.8);
        // Graph v2 knobs (cluster k bounds + LOD threshold).
        assert_eq!(t.graph.cluster_k_min, 2);
        assert_eq!(t.graph.cluster_k_max, 12);
        assert_eq!(t.graph.lod_threshold, 700);
        // Heatmap defaults (DESIGN-ATTENTION-HEATMAP.md): dwell leads, the
        // grid tier is a small fraction of Look, 60 s cap, 14-day half-life.
        assert_eq!(t.heatmap.w_dwell, 0.0001);
        assert_eq!(t.heatmap.w_events, 1.0);
        assert_eq!(t.heatmap.w_strokes, 0.5);
        assert_eq!(t.heatmap.dwell_look_rate, 1.0);
        assert_eq!(t.heatmap.dwell_grid_rate, 0.15);
        assert_eq!(t.heatmap.dwell_cap_ms, 60_000);
        assert_eq!(t.heatmap.recency_half_life_days, 14.0);
        // Voice dials (DESIGN-TUNING-LOOP.md "voice" arm) — these MUST equal the
        // old scattered const/flag values byte-for-byte, so lifting them is a
        // no-op until a `[voice]` section overrides one: rule1/2/3 are
        // pp-asr-server's DEFAULT_RULE*_S, enter/exit/hang are silero's
        // ENTER/EXIT/HANG_WINDOWS, pre_roll_ms is the engine's PRE_ROLL_MS.
        assert_eq!(t.voice.rule1, 2.4);
        assert_eq!(t.voice.rule2, 1.2);
        assert_eq!(t.voice.rule3, 20.0);
        assert_eq!(t.voice.vad_enter, 0.5);
        assert_eq!(t.voice.vad_exit, 0.35);
        assert_eq!(t.voice.vad_hang, 15);
        assert_eq!(t.voice.pre_roll_ms, 1_000);
    }

    /// A partial `[voice]` file sets one dial and leaves the rest (and every
    /// other section) at their code defaults — the `#[serde(default)]` merge.
    #[test]
    fn partial_voice_toml_merges_over_defaults() {
        let toml = r#"
            [voice]
            rule2 = 0.8
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        // The one set dial took (a snappier post-utterance endpoint):
        assert_eq!(merged.voice.rule2, 0.8);
        // The rest of the voice section stayed at the shipped defaults:
        assert_eq!(merged.voice.rule1, 2.4);
        assert_eq!(merged.voice.rule3, 20.0);
        assert_eq!(merged.voice.vad_enter, 0.5);
        assert_eq!(merged.voice.vad_hang, 15);
        assert_eq!(merged.voice.pre_roll_ms, 1_000);
        // Unrelated sections are pure defaults:
        assert_eq!(merged.search, SearchTuning::default());
        assert_eq!(merged.graph, GraphTuning::default());
    }

    /// Out-of-range voice dials reject back to defaults (never a silent bad
    /// number): a sub-floor rule (would truncate every word), a > 1 VAD
    /// probability, a 0 hang (would shred utterances), and an absurd pre-roll.
    #[test]
    fn out_of_range_voice_values_reject_to_default() {
        let toml = r#"
            [voice]
            rule1 = 0.0
            rule2 = 120.0
            vad_enter = 1.5
            vad_exit = -0.1
            vad_hang = 0
            pre_roll_ms = 999999
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        assert_eq!(merged.voice.rule1, 2.4); // below 0.1 s floor rejected
        assert_eq!(merged.voice.rule2, 1.2); // above 60 s cap rejected
        assert_eq!(merged.voice.vad_enter, 0.5); // > 1 probability rejected
        assert_eq!(merged.voice.vad_exit, 0.35); // < 0 probability rejected
        assert_eq!(merged.voice.vad_hang, 15); // 0 windows rejected
        assert_eq!(merged.voice.pre_roll_ms, 1_000); // > 5 s rejected
    }

    /// The RAW-feed sweep forces the gate open with enter/exit = 0.0; that is a
    /// LEGAL in-band value (the bounds are inclusive of 0), so it must survive
    /// validation rather than snap back — otherwise the raw WER ceiling pass
    /// could never be expressed as a config.
    #[test]
    fn voice_zero_vad_thresholds_are_in_band() {
        let toml = r#"
            [voice]
            vad_enter = 0.0
            vad_exit = 0.0
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        assert_eq!(merged.voice.vad_enter, 0.0);
        assert_eq!(merged.voice.vad_exit, 0.0);
    }

    /// A partial `[graph]` override merges over defaults like every other
    /// section, and an out-of-range graph knob snaps back to its default.
    #[test]
    fn graph_partial_merge_and_range_reject() {
        let toml = r#"
            [graph]
            alpha_default = 0.8
            damping = 9.0
            lod_threshold = 800
            cluster_k_max = 200
        "#;
        let merged = toml::from_str::<Tuning>(toml).unwrap().validated();
        // The in-range override took:
        assert_eq!(merged.graph.alpha_default, 0.8);
        // The out-of-range damping (>1) snapped back to the default:
        assert_eq!(merged.graph.damping, 0.85);
        // The in-range v2 LOD threshold override took:
        assert_eq!(merged.graph.lod_threshold, 800);
        // The out-of-range cluster k (>64) snapped back to its default:
        assert_eq!(merged.graph.cluster_k_max, 12);
        // Untouched graph fields kept their defaults:
        assert_eq!(merged.graph.attraction, 0.08);
        assert_eq!(merged.graph.ring_radius, 320.0);
        assert_eq!(merged.graph.anchor_attraction, 0.08);
        assert_eq!(merged.graph.anchor_repulsion, 60_000.0);
        assert_eq!(merged.graph.anchor_damping, 0.8);
        assert_eq!(merged.graph.cluster_k_min, 2);
        // Other sections are entirely undisturbed:
        assert_eq!(merged.search, SearchTuning::default());
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
