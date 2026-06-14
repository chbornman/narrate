//! pp-sweep — the search-tuning sweep orchestrator (DESIGN-TUNING-LOOP.md).
//!
//! The "research (offense)" half of the tuning loop: find a better `[search]`
//! config. `pp-sweep search` runs the EXISTING retrieval eval
//! (`retrieval_eval::evaluate` — the same run->score loop `pp-retrieval-eval`
//! drives) once per config over a knob grid, ranks the configs by a quality
//! metric (default nDCG@10) subject to the latency contract, and writes a
//! PROPOSED `tuning.proposed.toml` plus a human delta.
//!
//! K14 (binding): pp-sweep PROPOSES; the founder commits by copying the
//! proposal into `tuning.toml`. It NEVER writes `tuning.toml` itself.
//!
//! USAGE (scripts/sweep.sh wraps the release build):
//!   pp-sweep search --db <photoproof.db> --queries <golden.json>
//!            --grid "s4=0.5,0.75,1.0,1.25;beta=0.3,0.5"
//!            [--metric ndcg_at_k] [--k N] [--max-latency-ms N]
//!            [--json] [--propose <file>]
//!
//!   # paths also from env (args win): PP_RETRIEVAL_DB / PP_RETRIEVAL_QUERYSET
//!
//! The grid is `knob=v,v,...;knob=v,...`; the run is the CARTESIAN PRODUCT of
//! the per-knob value lists. Supported knobs: s1, s2, s3 (-> s3_each), s4,
//! rrf_k, beta. A knob absent from the grid keeps the baseline value. The
//! current committed config (from `tuning.toml` beside the DB, or the spec
//! defaults if no file) is ALWAYS included as a labeled `baseline` row, so the
//! winner's improvement over baseline is explicit.
//!
//! The leaderboard is deterministically sorted (primary metric desc, then a
//! stable tiebreak) so two runs diff cleanly; `--json` emits the full results
//! array for machine diffing.
//!
//! The first positional is the SUBSYSTEM. Only `search` is implemented today;
//! the structure leaves room for `voice`/`ingest` (DESIGN build sequence), and
//! an unknown subsystem errors cleanly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use photoproof_core::retrieval_eval::{EvalConfig, EvalReport, QuerySet, evaluate};
use photoproof_core::search::Searcher;
use photoproof_core::tuning::VoiceTuning;
use photoproof_core::voice_bench::{self, ScoreResult, VoiceKnobs};

/// Built-in @k default when neither the CLI nor the query set pins one (matches
/// `pp-retrieval-eval`).
const DEFAULT_K: usize = 10;

/// The default search ranking metric: nDCG@k (DESIGN: search maximizes it).
const DEFAULT_METRIC: &str = "ndcg_at_k";

/// The default VOICE ranking metric: gating-cost = gated WER - raw WER (DESIGN:
/// voice MINIMIZES it — the WER the pipeline's gating adds over the model's own
/// error, the founder's headline). Lower is better, so the voice arm ranks
/// ASCENDING (unlike search's descending).
const DEFAULT_VOICE_METRIC: &str = "gating_cost";

// Tuning validation bounds — the DOCUMENTED public ranges from
// `crate::tuning` (see `tuning.default.toml` / tuning.html). A grid value
// outside these is rejected with a clear error rather than silently clamped:
// the sweep author meant to try a real value, and a quiet clamp would rank a
// config that isn't the one they asked for.
const WEIGHT_MIN: f64 = 0.0;
const WEIGHT_MAX: f64 = 1000.0;
const RRF_K_MIN: f64 = 1.0;
const RRF_K_MAX: f64 = 10_000.0;
const BETA_MIN: f64 = 0.0;
const BETA_MAX: f64 = 1.0;

// Voice tuning bounds — mirror the documented `[voice]` ranges in
// `crate::tuning` (tuning.default.toml). A grid value outside these errors,
// exactly like the search bounds, so the sweep never ranks a config the
// validator would have snapped back at runtime.
const VOICE_RULE_MIN_S: f64 = 0.1;
const VOICE_RULE_MAX_S: f64 = 60.0;
const VOICE_VAD_PROB_MIN: f64 = 0.0;
const VOICE_VAD_PROB_MAX: f64 = 1.0;
const VOICE_VAD_HANG_MIN: f64 = 1.0;
const VOICE_VAD_HANG_MAX: f64 = 300.0;
const VOICE_PRE_ROLL_MIN_MS: f64 = 0.0;
const VOICE_PRE_ROLL_MAX_MS: f64 = 5_000.0;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    /// The subsystem to sweep (`search` or `voice`).
    subsystem: String,
    // --- search arm paths (required for `search`) ---
    db: Option<PathBuf>,
    queries: Option<PathBuf>,
    // --- voice arm paths (required for `voice`) ---
    /// `--corpus <wav>`: the recording to score every config over.
    corpus: Option<PathBuf>,
    /// `--expect <ref>`: the reference transcript WER is scored against.
    expect: Option<PathBuf>,
    /// `--model-dir`/`--server`: ASR model dir + `pp-asr-server` binary
    /// (default: founder-machine layout / sibling bin).
    model_dir: Option<PathBuf>,
    server: Option<PathBuf>,
    // --- shared ---
    grid: String,
    metric: String,
    k: Option<usize>,
    max_latency_ms: Option<u64>,
    json: bool,
    /// `--propose <file>` target; `None` skips proposal emission.
    propose: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage:\n\
     pp-sweep search --db <photoproof.db> --queries <queryset.json> \
     --grid \"s4=0.5,1.0;beta=0.3,0.5\" \
     [--metric ndcg_at_k] [--k N] [--max-latency-ms N] [--json] [--propose <file>]\n\
     (search paths also from env PP_RETRIEVAL_DB / PP_RETRIEVAL_QUERYSET; args win)\n\
     search knobs: s1 s2 s3 s4 rrf_k beta ; metrics: ndcg_at_k precision_at_k recall_at_k mrr\n\
     \n\
     pp-sweep voice --corpus <recording.wav> --expect <reference.txt> \
     --grid \"rule2=0.8,1.0,1.2;vad_hang=10,15,20\" \
     [--metric gating_cost] [--model-dir DIR] [--server PATH] [--json] [--propose <file>]\n\
     (voice paths also from env PP_VOICE_CORPUS / PP_VOICE_EXPECT; args win)\n\
     voice knobs: rule1 rule2 rule3 vad_enter vad_exit vad_hang pre_roll_ms ; \
     metrics: gating_cost gated_wer raw_wer"
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // The first positional is the subsystem; a missing/flag-leading first arg
    // is a usage error (we never default the subsystem silently).
    let mut it = argv.iter();
    let subsystem = match it.next() {
        Some(s) if !s.starts_with('-') => s.clone(),
        Some(s) if s == "-h" || s == "--help" => return Err(usage().to_owned()),
        _ => {
            return Err(format!(
                "missing subsystem (expected `search` or `voice`)\n{}",
                usage()
            ));
        }
    };

    // env supplies path defaults; explicit flags override (args win).
    let mut db = std::env::var_os("PP_RETRIEVAL_DB").map(PathBuf::from);
    let mut queries = std::env::var_os("PP_RETRIEVAL_QUERYSET").map(PathBuf::from);
    let mut corpus = std::env::var_os("PP_VOICE_CORPUS").map(PathBuf::from);
    let mut expect = std::env::var_os("PP_VOICE_EXPECT").map(PathBuf::from);
    let mut model_dir: Option<PathBuf> = None;
    let mut server: Option<PathBuf> = None;
    let mut grid = String::new();
    // The default metric is per-subsystem: search ranks by nDCG, voice by
    // gating-cost (the founder's headline). A `--metric` flag overrides.
    let mut metric: Option<String> = None;
    let mut k: Option<usize> = None;
    let mut max_latency_ms: Option<u64> = None;
    let mut json = false;
    let mut propose: Option<PathBuf> = None;

    while let Some(flag) = it.next() {
        let mut val = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("flag {name} needs a value"))
        };
        match flag.as_str() {
            "--db" => db = Some(PathBuf::from(val("--db")?)),
            "--queries" => queries = Some(PathBuf::from(val("--queries")?)),
            "--corpus" => corpus = Some(PathBuf::from(val("--corpus")?)),
            "--expect" => expect = Some(PathBuf::from(val("--expect")?)),
            "--model-dir" => model_dir = Some(PathBuf::from(val("--model-dir")?)),
            "--server" => server = Some(PathBuf::from(val("--server")?)),
            "--grid" => grid = val("--grid")?,
            "--metric" => metric = Some(val("--metric")?),
            "--k" => {
                k = Some(
                    val("--k")?
                        .parse::<usize>()
                        .map_err(|e| format!("flag --k: {e}"))?,
                )
            }
            "--max-latency-ms" => {
                max_latency_ms = Some(
                    val("--max-latency-ms")?
                        .parse::<u64>()
                        .map_err(|e| format!("flag --max-latency-ms: {e}"))?,
                )
            }
            "--json" => json = true,
            "--propose" => propose = Some(PathBuf::from(val("--propose")?)),
            "-h" | "--help" => return Err(usage().to_owned()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }

    if grid.trim().is_empty() {
        return Err(format!("missing --grid\n{}", usage()));
    }
    // Per-subsystem default metric, then validate the (resolved) metric against
    // the subsystem's allowed set.
    let metric = metric.unwrap_or_else(|| match subsystem.as_str() {
        "voice" => DEFAULT_VOICE_METRIC.to_owned(),
        _ => DEFAULT_METRIC.to_owned(),
    });
    validate_metric(&subsystem, &metric)?;
    Ok(Args {
        subsystem,
        db,
        queries,
        corpus,
        expect,
        model_dir,
        server,
        grid,
        metric,
        k,
        max_latency_ms,
        json,
        propose,
    })
}

/// The primary-metric selectors a leaderboard can rank by, per subsystem.
/// Reject an unknown metric at parse time so a typo never silently ranks by the
/// default. Search maximizes its metric; voice minimizes gating-cost/WER (the
/// rank direction is the arm's, not the metric name's).
fn validate_metric(subsystem: &str, metric: &str) -> Result<(), String> {
    match (subsystem, metric) {
        ("voice", "gating_cost" | "gated_wer" | "raw_wer") => Ok(()),
        ("voice", other) => Err(format!(
            "unknown voice --metric {other:?} (want gating_cost|gated_wer|raw_wer)"
        )),
        (_, "ndcg_at_k" | "precision_at_k" | "recall_at_k" | "mrr") => Ok(()),
        (_, other) => Err(format!(
            "unknown search --metric {other:?} (want ndcg_at_k|precision_at_k|recall_at_k|mrr)"
        )),
    }
}

/// Pull the named mean metric off a report (the ranking key).
fn metric_value(report: &EvalReport, metric: &str) -> f64 {
    match metric {
        "ndcg_at_k" => report.mean_ndcg_at_k,
        "precision_at_k" => report.mean_precision_at_k,
        "recall_at_k" => report.mean_recall_at_k,
        "mrr" => report.mean_mrr,
        // parse-time validation guarantees one of the above; default keeps the
        // match total without an unreachable panic.
        _ => report.mean_ndcg_at_k,
    }
}

// ---------------------------------------------------------------------------
// Grid parsing
// ---------------------------------------------------------------------------

/// The knobs a grid may sweep, in a stable column order (so the config-diff
/// label and the proposal read the same way every run).
const KNOBS: [&str; 6] = ["s1", "s2", "s3", "s4", "rrf_k", "beta"];

/// One config's knob values, folded onto the baseline. Only the knobs the grid
/// names are present; the rest stay at baseline.
type ConfigOverride = BTreeMap<String, f64>;

/// The search arm's grid parse — `parse_grid_with` over the search `KNOBS` and
/// the search-knob validator.
fn parse_grid(grid: &str) -> Result<Vec<ConfigOverride>, String> {
    parse_grid_with(grid, &KNOBS, validate_knob)
}

/// Parse `--grid "knob=v,v,...;knob=v,..."` into the per-knob value lists, then
/// expand to the CARTESIAN PRODUCT of configs over the given `knobs` column
/// order. Each value is an `f64`, range-validated by `validate` (out-of-range is
/// an error, not a clamp). A repeated knob, an unknown knob, or an unparsable
/// value all error with a clear message. Shared by the search and voice arms so
/// they parse and ENUMERATE identically (deterministic, last-knob-fastest), the
/// only difference being which knob set + bounds apply.
fn parse_grid_with(
    grid: &str,
    knobs: &[&str],
    validate: fn(&str, f64) -> Result<(), String>,
) -> Result<Vec<ConfigOverride>, String> {
    // knob -> its value list, kept in column order via a BTreeMap keyed by the
    // column index so the cartesian product is deterministic.
    let mut by_col: BTreeMap<usize, (String, Vec<f64>)> = BTreeMap::new();
    for clause in grid.split(';') {
        let clause = clause.trim();
        if clause.is_empty() {
            continue; // tolerate a trailing/`;;` separator
        }
        let (knob, values) = clause
            .split_once('=')
            .ok_or_else(|| format!("grid clause {clause:?} is not `knob=v,v,...`"))?;
        let knob = knob.trim();
        let col = knobs
            .iter()
            .position(|k| *k == knob)
            .ok_or_else(|| format!("unknown grid knob {knob:?} (want one of {knobs:?})"))?;
        if by_col.contains_key(&col) {
            return Err(format!("grid knob {knob:?} appears more than once"));
        }
        let mut parsed = Vec::new();
        for raw in values.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(format!("grid knob {knob:?} has an empty value"));
            }
            let v: f64 = raw
                .parse()
                .map_err(|e| format!("grid knob {knob:?} value {raw:?}: {e}"))?;
            validate(knob, v)?;
            parsed.push(v);
        }
        if parsed.is_empty() {
            return Err(format!("grid knob {knob:?} has no values"));
        }
        by_col.insert(col, (knob.to_owned(), parsed));
    }
    if by_col.is_empty() {
        return Err("grid named no knobs".to_owned());
    }

    // Cartesian product: start with one empty config, fold each knob's value
    // list in (knobs in column order -> last knob varies fastest).
    let mut configs: Vec<ConfigOverride> = vec![BTreeMap::new()];
    for (knob, values) in by_col.values() {
        let mut next = Vec::with_capacity(configs.len() * values.len());
        for base in &configs {
            for v in values {
                let mut c = base.clone();
                c.insert(knob.clone(), *v);
                next.push(c);
            }
        }
        configs = next;
    }
    Ok(configs)
}

/// Range-validate one search knob value against its documented tuning bound.
fn validate_knob(knob: &str, v: f64) -> Result<(), String> {
    let (min, max) = match knob {
        "s1" | "s2" | "s3" | "s4" => (WEIGHT_MIN, WEIGHT_MAX),
        "rrf_k" => (RRF_K_MIN, RRF_K_MAX),
        "beta" => (BETA_MIN, BETA_MAX),
        other => return Err(format!("unknown grid knob {other:?}")),
    };
    if !v.is_finite() || v < min || v > max {
        return Err(format!(
            "grid knob {knob:?} value {v} out of range [{min}, {max}]"
        ));
    }
    Ok(())
}

/// Fold a `ConfigOverride` onto the baseline `EvalConfig` -> the concrete
/// config a run uses. A knob absent from the override keeps the baseline value.
fn apply_override(baseline: EvalConfig, ov: &ConfigOverride) -> EvalConfig {
    let mut weights = baseline.weights;
    let mut rrf_k = baseline.rrf_k;
    let mut beta = baseline.beta;
    for (knob, v) in ov {
        match knob.as_str() {
            "s1" => weights.s1 = *v,
            "s2" => weights.s2 = *v,
            "s3" => weights.s3_each = *v,
            "s4" => weights.s4 = *v,
            "rrf_k" => rrf_k = *v,
            "beta" => beta = *v,
            _ => {} // parse_grid already rejected unknown knobs
        }
    }
    EvalConfig {
        weights,
        rrf_k,
        beta,
    }
}

// ---------------------------------------------------------------------------
// Run + leaderboard
// ---------------------------------------------------------------------------

/// One scored config: its label, the knobs that DIFFER from baseline, the
/// concrete config it ran, the full metric report, and the wall-time the run
/// took. `is_baseline` marks the always-present baseline row.
struct Row {
    label: String,
    /// Knobs whose value differs from baseline, in KNOBS column order — the
    /// human "config" cell ("s4=1.25 beta=0.30"); empty for baseline.
    diff: Vec<(String, f64)>,
    config: EvalConfig,
    report: EvalReport,
    wall_ms: u128,
    is_baseline: bool,
}

/// Run the baseline plus every grid config through the shared eval, returning
/// the scored rows (UNSORTED — the caller ranks). Sequential by design (see the
/// note in `run`): the rows must be deterministic and the eval holds a single
/// `Searcher` connection, so we trade a little wall-clock for clean diffs.
fn score_all(
    searcher: &Searcher,
    query_set: &QuerySet,
    baseline: EvalConfig,
    overrides: &[ConfigOverride],
    k: usize,
) -> Result<Vec<Row>, String> {
    let mut rows = Vec::with_capacity(overrides.len() + 1);

    // The baseline row is always first to compute; it anchors the deltas.
    rows.push(score_one(
        searcher,
        query_set,
        baseline,
        &BTreeMap::new(),
        baseline,
        k,
        "baseline",
        true,
    )?);

    for (i, ov) in overrides.iter().enumerate() {
        let config = apply_override(baseline, ov);
        // Skip a grid config that is byte-identical to baseline (e.g. the grid
        // happens to list the current value) — it would be a duplicate row.
        if config == baseline {
            continue;
        }
        let label = format!("cfg{i}");
        rows.push(score_one(
            searcher, query_set, baseline, ov, config, k, &label, false,
        )?);
    }
    Ok(rows)
}

/// Score one config and time its run.
#[allow(clippy::too_many_arguments)]
fn score_one(
    searcher: &Searcher,
    query_set: &QuerySet,
    baseline: EvalConfig,
    ov: &ConfigOverride,
    config: EvalConfig,
    k: usize,
    label: &str,
    is_baseline: bool,
) -> Result<Row, String> {
    let started = Instant::now();
    let report = evaluate(searcher, query_set, config, k)?;
    let wall_ms = started.elapsed().as_millis();
    Ok(Row {
        label: label.to_owned(),
        diff: diff_from_baseline(baseline, ov),
        config,
        report,
        wall_ms,
        is_baseline,
    })
}

/// The knobs in `ov` whose value differs from baseline, in KNOBS column order.
/// (An override knob that happens to equal baseline is not shown as a change.)
fn diff_from_baseline(baseline: EvalConfig, ov: &ConfigOverride) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for knob in KNOBS {
        if let Some(v) = ov.get(knob) {
            let base = baseline_knob(baseline, knob);
            if (*v - base).abs() > f64::EPSILON {
                out.push((knob.to_owned(), *v));
            }
        }
    }
    out
}

/// The baseline value of one knob (for the diff label).
fn baseline_knob(baseline: EvalConfig, knob: &str) -> f64 {
    match knob {
        "s1" => baseline.weights.s1,
        "s2" => baseline.weights.s2,
        "s3" => baseline.weights.s3_each,
        "s4" => baseline.weights.s4,
        "rrf_k" => baseline.rrf_k,
        "beta" => baseline.beta,
        _ => f64::NAN,
    }
}

/// Apply `--max-latency-ms` (drop configs slower than the budget) and then sort
/// the rows into the final leaderboard: primary metric DESCENDING, then a
/// STABLE tiebreak (wall-time asc, then label) so two runs over the same grid
/// produce byte-identical order. The baseline row is ranked alongside the rest
/// (it is a real config), never specially floated.
///
/// WHY filter before ranking: a config that violates the latency budget is not
/// a candidate at all, so it must not be able to win on metric. For a pure
/// weight sweep this is a near-no-op (weights do not move latency); it bites
/// once the grid gains a latency-affecting dimension (candidate-pool size, a
/// reranker). The filter only runs when `--max-latency-ms` is passed.
fn rank(mut rows: Vec<Row>, metric: &str, max_latency_ms: Option<u64>) -> Vec<Row> {
    if let Some(budget) = max_latency_ms {
        rows.retain(|r| r.wall_ms <= budget as u128);
    }
    rows.sort_by(|a, b| {
        metric_value(&b.report, metric)
            .total_cmp(&metric_value(&a.report, metric))
            .then_with(|| a.wall_ms.cmp(&b.wall_ms))
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Human leaderboard table. No em-dashes (the repo's UI-copy gate); ASCII only.
fn print_table(rows: &[Row], metric: &str, k: usize) {
    println!(
        "search sweep: {} configs @k={} ranked by {metric}",
        rows.len(),
        k
    );
    println!(
        "{:>4} {:<28} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "rank", "config", "nDCG", "P@k", "R@k", "MRR", "ms"
    );
    for (i, r) in rows.iter().enumerate() {
        let cfg = config_label(r);
        let m = &r.report;
        println!(
            "{:>4} {:<28} {:>8.4} {:>8.4} {:>8.4} {:>8.4} {:>8}",
            i + 1,
            truncate(&cfg, 28),
            m.mean_ndcg_at_k,
            m.mean_precision_at_k,
            m.mean_recall_at_k,
            m.mean_mrr,
            r.wall_ms,
        );
    }
}

/// The "config" cell: "baseline" for the baseline row, else the differing
/// knobs ("s4=1.25 beta=0.30"). A non-baseline row whose override happened to
/// equal baseline (filtered out earlier) cannot reach here.
fn config_label(r: &Row) -> String {
    if r.is_baseline {
        return "baseline".to_owned();
    }
    if r.diff.is_empty() {
        // Defensive: should not happen (such configs are skipped), but never
        // print a blank cell.
        return r.label.clone();
    }
    r.diff
        .iter()
        .map(|(knob, v)| format!("{knob}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Machine-diffable JSON: schema, run inputs, host block, and the ranked
/// results array (stable order). Mirrors `pp-retrieval-eval`'s `schema: 1`
/// convention and `pp-bench`'s host block.
fn print_json(rows: &[Row], args: &Args, k: usize) {
    let results: Vec<serde_json::Value> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let m = &r.report;
            let diff: serde_json::Map<String, serde_json::Value> = r
                .diff
                .iter()
                .map(|(knob, v)| (knob.clone(), serde_json::json!(v)))
                .collect();
            serde_json::json!({
                "rank": i + 1,
                "label": r.label,
                "baseline": r.is_baseline,
                "config_diff": diff,
                "weights": {
                    "s1": r.config.weights.s1,
                    "s2": r.config.weights.s2,
                    "s3_each": r.config.weights.s3_each,
                    "s4": r.config.weights.s4,
                },
                "rrf_k": r.config.rrf_k,
                "beta": r.config.beta,
                "ndcg_at_k": m.mean_ndcg_at_k,
                "precision_at_k": m.mean_precision_at_k,
                "recall_at_k": m.mean_recall_at_k,
                "mrr": m.mean_mrr,
                "wall_ms": r.wall_ms,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": 1,
        "k": k,
        "metric": args.metric,
        "db": args.db.as_ref().map(|p| p.display().to_string()),
        "queries": args.queries.as_ref().map(|p| p.display().to_string()),
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "results": results,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}

/// Byte-safe truncation to a column width (keeps multi-byte chars whole).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('~');
    out
}

// ---------------------------------------------------------------------------
// Proposal (K14: propose only; never write tuning.toml)
// ---------------------------------------------------------------------------

/// Render the proposed `tuning.proposed.toml` text: the WINNING config's
/// `[search]` block, a K14 header, and the baseline->winner delta as comments.
/// PURE (no IO) so it is unit-testable; the caller writes the file.
///
/// `winner` is rows[0] (the ranked leader); `baseline` is the baseline row.
/// When the winner IS the baseline, the body is the baseline config plus a
/// "baseline already optimal in this grid" note (nothing changed).
fn render_proposal(winner: &Row, baseline: &Row, queries: &Path, metric: &str) -> String {
    let w = &winner.config;
    let mut out = String::new();
    out.push_str(&format!(
        "# PROPOSED by pp-sweep on {}; review and copy into tuning.toml to apply. \
         The machine proposes; you commit (K14).\n",
        queries.display()
    ));

    let baseline_wins = winner.is_baseline;
    if baseline_wins {
        out.push_str("# baseline already optimal in this grid: no knob change is proposed.\n");
    } else {
        // The knob changes (baseline -> winner), one comment line each.
        out.push_str("# knob changes (baseline -> winner):\n");
        for (knob, v) in &winner.diff {
            let base = baseline_knob(baseline.config, knob);
            out.push_str(&format!("#   {knob}: {base} -> {v}\n"));
        }
    }
    // The metric delta (baseline -> winner) for the ranking metric, plus the
    // full metric line so the proposal is self-describing.
    let bm = &baseline.report;
    let wm = &winner.report;
    out.push_str(&format!(
        "# {metric}: {:.4} -> {:.4} (delta {:+.4})\n",
        metric_value(bm, metric),
        metric_value(wm, metric),
        metric_value(wm, metric) - metric_value(bm, metric),
    ));
    out.push_str(&format!(
        "# winner metrics: nDCG={:.4} P@k={:.4} R@k={:.4} MRR={:.4}\n",
        wm.mean_ndcg_at_k, wm.mean_precision_at_k, wm.mean_recall_at_k, wm.mean_mrr,
    ));
    out.push('\n');

    // The `[search]` block, in the same shape as tuning.default.toml so a copy
    // into tuning.toml is a drop-in. Only the search dials (never a contract).
    out.push_str("[search]\n");
    out.push_str(&format!("rrf_k = {}\n", w.rrf_k));
    out.push_str(&format!("beta = {}\n", w.beta));
    out.push('\n');
    out.push_str("[search.fusion]\n");
    out.push_str(&format!("s1 = {}\n", w.weights.s1));
    out.push_str(&format!("s2 = {}\n", w.weights.s2));
    out.push_str(&format!("s3_each = {}\n", w.weights.s3_each));
    out.push_str(&format!("s4 = {}\n", w.weights.s4));
    out
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

fn run(args: &Args) -> Result<(), String> {
    // Dispatch by subsystem; the DESIGN build sequence adds `ingest` later, and
    // an unknown subsystem errors cleanly.
    match args.subsystem.as_str() {
        "search" => run_search(args),
        "voice" => run_voice(args),
        other => Err(format!(
            "subsystem {other:?} not implemented (want `search` or `voice`)"
        )),
    }
}

fn run_search(args: &Args) -> Result<(), String> {
    let db = args
        .db
        .as_ref()
        .ok_or_else(|| format!("search: missing --db (or PP_RETRIEVAL_DB)\n{}", usage()))?;
    let queries = args.queries.as_ref().ok_or_else(|| {
        format!(
            "search: missing --queries (or PP_RETRIEVAL_QUERYSET)\n{}",
            usage()
        )
    })?;

    let raw = std::fs::read_to_string(queries)
        .map_err(|e| format!("reading query set {}: {e}", queries.display()))?;
    let query_set: QuerySet = serde_json::from_str(&raw)
        .map_err(|e| format!("parsing query set {}: {e}", queries.display()))?;

    // Source the baseline from the SAME `tuning.toml` the app reads (beside the
    // DB), so the baseline row IS the live committed config. Absent a file this
    // is the spec defaults. `EvalConfig::default()` then reads that global.
    if let Some(app_data) = db.parent() {
        photoproof_core::tuning::init_from(app_data);
    }
    let baseline = EvalConfig::default();

    // k precedence: --k > query-set default_k > built-in DEFAULT_K (matches
    // pp-retrieval-eval).
    let k = args.k.or(query_set.default_k).unwrap_or(DEFAULT_K);

    let overrides = parse_grid(&args.grid)?;

    let searcher =
        Searcher::open(db).map_err(|e| format!("opening library DB {}: {e}", db.display()))?;

    // SEQUENTIAL by choice: the eval holds a single `Searcher` connection and
    // the rows must be deterministic for clean diffs. Parallelism would need
    // per-config read-only connections (SQLite concurrent readers) and buys
    // little here (the eval is keyword-only, no models), so correctness wins.
    let rows = score_all(&searcher, &query_set, baseline, &overrides, k)?;
    let ranked = rank(rows, &args.metric, args.max_latency_ms);
    if ranked.is_empty() {
        return Err(
            "no configs survived the latency filter (try a larger --max-latency-ms)".into(),
        );
    }

    if args.json {
        print_json(&ranked, args, k);
    } else {
        print_table(&ranked, &args.metric, k);
    }

    if let Some(path) = &args.propose {
        // The winner is the ranked leader; the baseline row anchors the delta.
        let winner = &ranked[0];
        let baseline_row = ranked
            .iter()
            .find(|r| r.is_baseline)
            .ok_or("internal: baseline row missing from ranked set")?;
        let text = render_proposal(winner, baseline_row, queries, &args.metric);
        guard_propose_target(path)?;
        std::fs::write(path, text)
            .map_err(|e| format!("writing proposal {}: {e}", path.display()))?;
        eprintln!(
            "wrote proposal to {} (review and copy into tuning.toml to apply)",
            path.display()
        );
    }
    Ok(())
}

/// K14 write guard, shared by both arms: a `--propose` target may NEVER be
/// `tuning.toml`. pp-sweep PROPOSES; the founder commits.
fn guard_propose_target(path: &Path) -> Result<(), String> {
    if path.file_name().and_then(|n| n.to_str()) == Some("tuning.toml") {
        return Err(
            "refusing to write tuning.toml: pp-sweep PROPOSES only (K14). \
             Point --propose at e.g. tuning.proposed.toml."
                .into(),
        );
    }
    Ok(())
}

// ===========================================================================
// Voice arm (DESIGN-TUNING-LOOP.md "voice"): sweep the [voice] dials over the
// voice corpus via the shared pp-voice-bench pipeline, ranked by gating-cost.
//
// Reuses the search arm's scaffolding where it fits (the grid parser via
// `parse_grid_with`, the deterministic enumeration, the K14 propose guard); the
// scoring (one server spawn per config + a gated/raw WER pass) and the ASCENDING
// gating-cost rank are voice-specific, so they live here beside the search rank.
// ===========================================================================

/// The voice knobs a grid may sweep, in a stable column order (the proposal /
/// diff cell read the same way every run). Mirrors the `[voice]` tuning fields.
const VOICE_KNOBS: [&str; 7] = [
    "rule1",
    "rule2",
    "rule3",
    "vad_enter",
    "vad_exit",
    "vad_hang",
    "pre_roll_ms",
];

/// Range-validate one voice knob against its documented `[voice]` tuning bound
/// (out-of-range is an error, not a clamp — same posture as the search arm).
fn validate_voice_knob(knob: &str, v: f64) -> Result<(), String> {
    let (min, max) = match knob {
        "rule1" | "rule2" | "rule3" => (VOICE_RULE_MIN_S, VOICE_RULE_MAX_S),
        "vad_enter" | "vad_exit" => (VOICE_VAD_PROB_MIN, VOICE_VAD_PROB_MAX),
        "vad_hang" => (VOICE_VAD_HANG_MIN, VOICE_VAD_HANG_MAX),
        "pre_roll_ms" => (VOICE_PRE_ROLL_MIN_MS, VOICE_PRE_ROLL_MAX_MS),
        other => return Err(format!("unknown grid knob {other:?}")),
    };
    if !v.is_finite() || v < min || v > max {
        return Err(format!(
            "grid knob {knob:?} value {v} out of range [{min}, {max}]"
        ));
    }
    // hang / pre_roll_ms are integer dials: a fractional grid value is a typo.
    if matches!(knob, "vad_hang" | "pre_roll_ms") && v.fract() != 0.0 {
        return Err(format!(
            "grid knob {knob:?} value {v} must be a whole number"
        ));
    }
    Ok(())
}

/// The voice arm's grid parse — `parse_grid_with` over `VOICE_KNOBS`.
fn parse_voice_grid(grid: &str) -> Result<Vec<ConfigOverride>, String> {
    parse_grid_with(grid, &VOICE_KNOBS, validate_voice_knob)
}

/// Fold a `ConfigOverride` onto a baseline `VoiceTuning` -> the concrete config a
/// run uses (a knob absent from the override keeps the baseline value).
fn apply_voice_override(baseline: VoiceTuning, ov: &ConfigOverride) -> VoiceTuning {
    let mut c = baseline;
    for (knob, v) in ov {
        match knob.as_str() {
            "rule1" => c.rule1 = *v,
            "rule2" => c.rule2 = *v,
            "rule3" => c.rule3 = *v,
            "vad_enter" => c.vad_enter = *v,
            "vad_exit" => c.vad_exit = *v,
            "vad_hang" => c.vad_hang = *v as u32,
            "pre_roll_ms" => c.pre_roll_ms = *v as u64,
            _ => {} // parse_voice_grid already rejected unknown knobs
        }
    }
    c
}

/// The `VoiceKnobs` (the bench's knob bundle) for a concrete `VoiceTuning`. The
/// sweep ALWAYS sets rule1/2/3 and pre_roll explicitly (so the config is exactly
/// what runs, not a server/global fallback).
fn knobs_of(c: VoiceTuning) -> VoiceKnobs {
    VoiceKnobs {
        rule1: Some(c.rule1 as f32),
        rule2: Some(c.rule2 as f32),
        rule3: Some(c.rule3 as f32),
        endpoint_grace_ms: None,
        enter: c.vad_enter as f32,
        exit: c.vad_exit as f32,
        hang: c.vad_hang,
        pre_roll_ms: Some(c.pre_roll_ms),
    }
}

/// One scored voice config: its label, the knobs differing from baseline, the
/// concrete config, the gated/raw score, and the wall-time. `is_baseline` marks
/// the always-present baseline row.
struct VoiceRow {
    label: String,
    diff: Vec<(String, f64)>,
    config: VoiceTuning,
    score: ScoreResult,
    wall_ms: u128,
    is_baseline: bool,
}

/// The named voice metric off a score (the ranking key). gating_cost is the
/// default (lower better); gated_wer/raw_wer let a caller rank by either WER.
fn voice_metric_value(s: &ScoreResult, metric: &str) -> f64 {
    match metric {
        "gating_cost" => s.gating_cost(),
        "gated_wer" => s.gated.wer,
        "raw_wer" => s.raw.wer,
        _ => s.gating_cost(),
    }
}

/// The voice knobs in `ov` that differ from baseline, in `VOICE_KNOBS` order.
fn voice_diff_from_baseline(baseline: VoiceTuning, ov: &ConfigOverride) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for knob in VOICE_KNOBS {
        if let Some(v) = ov.get(knob) {
            let base = voice_baseline_knob(baseline, knob);
            if (*v - base).abs() > f64::EPSILON {
                out.push((knob.to_owned(), *v));
            }
        }
    }
    out
}

/// The baseline value of one voice knob (for the diff label).
fn voice_baseline_knob(b: VoiceTuning, knob: &str) -> f64 {
    match knob {
        "rule1" => b.rule1,
        "rule2" => b.rule2,
        "rule3" => b.rule3,
        "vad_enter" => b.vad_enter,
        "vad_exit" => b.vad_exit,
        "vad_hang" => b.vad_hang as f64,
        "pre_roll_ms" => b.pre_roll_ms as f64,
        _ => f64::NAN,
    }
}

/// Score the baseline plus every grid config over the corpus (UNSORTED — the
/// caller ranks). Each config spawns its OWN `pp-asr-server` (rule1/2/3 are
/// server-side flags) and runs the gated + raw WER passes. SEQUENTIAL by design:
/// one server at a time keeps it deterministic and avoids N concurrent model
/// loads. A config byte-identical to baseline is skipped (it would duplicate the
/// baseline row).
fn score_all_voice(
    samples: &[f32],
    reference: &str,
    server: &Path,
    model_dir: &Path,
    baseline: VoiceTuning,
    overrides: &[ConfigOverride],
) -> Result<Vec<VoiceRow>, String> {
    let mut rows = Vec::with_capacity(overrides.len() + 1);
    rows.push(score_one_voice(
        samples,
        reference,
        server,
        model_dir,
        baseline,
        &BTreeMap::new(),
        baseline,
        "baseline",
        true,
    )?);
    for (i, ov) in overrides.iter().enumerate() {
        let config = apply_voice_override(baseline, ov);
        if config == baseline {
            continue; // a grid value equal to the current config: duplicate row
        }
        let label = format!("cfg{i}");
        rows.push(score_one_voice(
            samples, reference, server, model_dir, baseline, ov, config, &label, false,
        )?);
    }
    Ok(rows)
}

/// Score one voice config over the corpus and time it.
#[allow(clippy::too_many_arguments)]
fn score_one_voice(
    samples: &[f32],
    reference: &str,
    server: &Path,
    model_dir: &Path,
    baseline: VoiceTuning,
    ov: &ConfigOverride,
    config: VoiceTuning,
    label: &str,
    is_baseline: bool,
) -> Result<VoiceRow, String> {
    let knobs = knobs_of(config);
    let started = Instant::now();
    // One server per config (the endpoint rules are the server's). Kill-on-drop
    // so a scoring error mid-grid never leaks a child.
    let (child, addr) = voice_bench::spawn_server(server, model_dir, &knobs)?;
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _reap = Reap(child);
    let score = voice_bench::score_run(samples, addr, &knobs, reference);
    let wall_ms = started.elapsed().as_millis();
    Ok(VoiceRow {
        label: label.to_owned(),
        diff: voice_diff_from_baseline(baseline, ov),
        config,
        score,
        wall_ms,
        is_baseline,
    })
}

/// Rank voice rows ASCENDING by the primary metric (lower gating-cost / WER is
/// better — the OPPOSITE of search's descending sort), with a STABLE tiebreak
/// (wall-time asc, then label) so two runs over the same grid produce
/// byte-identical order. The baseline row is ranked alongside the rest.
fn rank_voice(mut rows: Vec<VoiceRow>, metric: &str) -> Vec<VoiceRow> {
    rows.sort_by(|a, b| {
        voice_metric_value(&a.score, metric)
            .total_cmp(&voice_metric_value(&b.score, metric))
            .then_with(|| a.wall_ms.cmp(&b.wall_ms))
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

/// The "config" cell for a voice row: "baseline", else the differing knobs.
fn voice_config_label(r: &VoiceRow) -> String {
    if r.is_baseline {
        return "baseline".to_owned();
    }
    if r.diff.is_empty() {
        return r.label.clone();
    }
    r.diff
        .iter()
        .map(|(knob, v)| format!("{knob}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Human voice leaderboard. ASCII only (no em-dashes). gating-cost is the
/// headline; gated/raw WER and the minted-segment count are shown alongside.
fn print_voice_table(rows: &[VoiceRow], metric: &str) {
    println!(
        "voice sweep: {} configs ranked by {metric} (ascending; lower is better)",
        rows.len()
    );
    println!(
        "{:>4} {:<28} {:>9} {:>9} {:>9} {:>5} {:>4} {:>8}",
        "rank", "config", "gate_cost", "gatedWER", "rawWER", "segs", "abnd", "ms"
    );
    for (i, r) in rows.iter().enumerate() {
        let cfg = voice_config_label(r);
        let s = &r.score;
        println!(
            "{:>4} {:<28} {:>+9.4} {:>9.4} {:>9.4} {:>5} {:>4} {:>8}",
            i + 1,
            truncate(&cfg, 28),
            s.gating_cost(),
            s.gated.wer,
            s.raw.wer,
            s.gated_segs.len(),
            s.abandoned,
            r.wall_ms,
        );
    }
}

/// Machine-diffable voice JSON (schema 1, stable order), mirroring the search
/// arm's shape with the voice metric block.
fn print_voice_json(rows: &[VoiceRow], args: &Args) {
    let results: Vec<serde_json::Value> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let s = &r.score;
            let diff: serde_json::Map<String, serde_json::Value> = r
                .diff
                .iter()
                .map(|(knob, v)| (knob.clone(), serde_json::json!(v)))
                .collect();
            serde_json::json!({
                "rank": i + 1,
                "label": r.label,
                "baseline": r.is_baseline,
                "config_diff": diff,
                "config": voice_config_json(r.config),
                "gating_cost": s.gating_cost(),
                "gated_wer": s.gated.wer,
                "raw_wer": s.raw.wer,
                "segments": s.gated_segs.len(),
                "abandoned": s.abandoned,
                "wall_ms": r.wall_ms,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": 1,
        "metric": args.metric,
        "corpus": args.corpus.as_ref().map(|p| p.display().to_string()),
        "expect": args.expect.as_ref().map(|p| p.display().to_string()),
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "results": results,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}

/// A `VoiceTuning` as a JSON object (the full concrete config a row ran).
fn voice_config_json(c: VoiceTuning) -> serde_json::Value {
    serde_json::json!({
        "rule1": c.rule1, "rule2": c.rule2, "rule3": c.rule3,
        "vad_enter": c.vad_enter, "vad_exit": c.vad_exit, "vad_hang": c.vad_hang,
        "pre_roll_ms": c.pre_roll_ms,
    })
}

/// Render the proposed `[voice]` block: the WINNING config, a K14 header, and the
/// baseline->winner delta (gating-cost + both WERs) as comments. PURE (no IO) so
/// it is unit-testable; the caller writes the file. Mirrors the search arm's
/// `render_proposal`.
fn render_voice_proposal(
    winner: &VoiceRow,
    baseline: &VoiceRow,
    corpus: &Path,
    metric: &str,
) -> String {
    let w = winner.config;
    let mut out = String::new();
    out.push_str(&format!(
        "# PROPOSED by pp-sweep voice on {}; review and copy into tuning.toml to \
         apply. The machine proposes; you commit (K14).\n",
        corpus.display()
    ));
    if winner.is_baseline {
        out.push_str("# baseline already optimal in this grid: no knob change is proposed.\n");
    } else {
        out.push_str("# knob changes (baseline -> winner):\n");
        for (knob, v) in &winner.diff {
            let base = voice_baseline_knob(baseline.config, knob);
            out.push_str(&format!("#   {knob}: {base} -> {v}\n"));
        }
    }
    let bs = &baseline.score;
    let ws = &winner.score;
    out.push_str(&format!(
        "# {metric}: {:+.4} -> {:+.4} (delta {:+.4}; lower is better)\n",
        voice_metric_value(bs, metric),
        voice_metric_value(ws, metric),
        voice_metric_value(ws, metric) - voice_metric_value(bs, metric),
    ));
    out.push_str(&format!(
        "# winner: gating_cost={:+.4} gated_wer={:.4} raw_wer={:.4}\n",
        ws.gating_cost(),
        ws.gated.wer,
        ws.raw.wer,
    ));
    out.push('\n');
    // The `[voice]` block, in the same shape as tuning.default.toml so a copy
    // into tuning.toml is a drop-in. Only the voice DIALS (never a contract).
    out.push_str("[voice]\n");
    out.push_str(&format!("rule1 = {}\n", w.rule1));
    out.push_str(&format!("rule2 = {}\n", w.rule2));
    out.push_str(&format!("rule3 = {}\n", w.rule3));
    out.push_str(&format!("vad_enter = {}\n", w.vad_enter));
    out.push_str(&format!("vad_exit = {}\n", w.vad_exit));
    out.push_str(&format!("vad_hang = {}\n", w.vad_hang));
    out.push_str(&format!("pre_roll_ms = {}\n", w.pre_roll_ms));
    out
}

fn run_voice(args: &Args) -> Result<(), String> {
    let corpus = args
        .corpus
        .as_ref()
        .ok_or_else(|| format!("voice: missing --corpus (or PP_VOICE_CORPUS)\n{}", usage()))?;
    let expect = args
        .expect
        .as_ref()
        .ok_or_else(|| format!("voice: missing --expect (or PP_VOICE_EXPECT)\n{}", usage()))?;

    // The baseline IS the committed `[voice]` config. Source it from the same
    // `tuning.toml` the app reads when one sits beside the corpus; absent a file
    // this is the shipped voice defaults. `tuning().voice` reads that global.
    if let Some(app_data) = corpus.parent() {
        photoproof_core::tuning::init_from(app_data);
    }
    let baseline = photoproof_core::tuning::tuning().voice;

    let overrides = parse_voice_grid(&args.grid)?;

    let samples = voice_bench::read_wav(corpus)
        .map_err(|e| format!("reading corpus {}: {e}", corpus.display()))?;
    let reference = std::fs::read_to_string(expect)
        .map_err(|e| format!("reading --expect {}: {e}", expect.display()))?;

    let server = args
        .server
        .clone()
        .unwrap_or_else(voice_bench::default_server);
    let model_dir = args
        .model_dir
        .clone()
        .unwrap_or_else(voice_bench::default_model_dir);

    let rows = score_all_voice(
        &samples, &reference, &server, &model_dir, baseline, &overrides,
    )?;
    let ranked = rank_voice(rows, &args.metric);
    if ranked.is_empty() {
        return Err("voice sweep produced no rows (empty grid?)".into());
    }

    if args.json {
        print_voice_json(&ranked, args);
    } else {
        print_voice_table(&ranked, &args.metric);
    }

    if let Some(path) = &args.propose {
        let winner = &ranked[0];
        let baseline_row = ranked
            .iter()
            .find(|r| r.is_baseline)
            .ok_or("internal: baseline row missing from ranked set")?;
        let text = render_voice_proposal(winner, baseline_row, corpus, &args.metric);
        guard_propose_target(path)?;
        std::fs::write(path, text)
            .map_err(|e| format!("writing proposal {}: {e}", path.display()))?;
        eprintln!(
            "wrote proposal to {} (review and copy into tuning.toml to apply)",
            path.display()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("pp-sweep: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_core::search::FusionWeights;

    fn cfg(s1: f64, s2: f64, s3_each: f64, s4: f64, rrf_k: f64, beta: f64) -> EvalConfig {
        EvalConfig {
            weights: FusionWeights {
                s1,
                s2,
                s3_each,
                s4,
            },
            rrf_k,
            beta,
        }
    }

    /// A spec-default baseline that does NOT touch the process tuning global
    /// (so the test is independent of any `init_from`).
    fn baseline() -> EvalConfig {
        cfg(1.0, 1.0, 0.5, 1.0, 60.0, 0.5)
    }

    // --- grid parser: cartesian product, ordering ---------------------------
    #[test]
    fn grid_single_knob_lists_each_value() {
        let configs = parse_grid("s4=0.5,1.0,1.25").unwrap();
        assert_eq!(configs.len(), 3);
        assert_eq!(configs[0].get("s4"), Some(&0.5));
        assert_eq!(configs[1].get("s4"), Some(&1.0));
        assert_eq!(configs[2].get("s4"), Some(&1.25));
        // A single-knob grid touches only that knob.
        assert_eq!(configs[0].len(), 1);
    }

    #[test]
    fn grid_cartesian_product_is_deterministic() {
        // 2 x 2 = 4 configs; last knob (beta) varies fastest within each s4.
        let configs = parse_grid("s4=0.5,1.0;beta=0.3,0.5").unwrap();
        assert_eq!(configs.len(), 4);
        let pairs: Vec<(f64, f64)> = configs
            .iter()
            .map(|c| (*c.get("s4").unwrap(), *c.get("beta").unwrap()))
            .collect();
        assert_eq!(pairs, vec![(0.5, 0.3), (0.5, 0.5), (1.0, 0.3), (1.0, 0.5)]);
    }

    #[test]
    fn grid_tolerates_trailing_separator_and_whitespace() {
        let configs = parse_grid(" s4 = 0.5 , 1.0 ; ").unwrap();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].get("s4"), Some(&0.5));
    }

    #[test]
    fn grid_s3_maps_to_s3_each_on_apply() {
        let configs = parse_grid("s3=0.25").unwrap();
        let applied = apply_override(baseline(), &configs[0]);
        assert_eq!(applied.weights.s3_each, 0.25);
        // The other weights stay at baseline.
        assert_eq!(applied.weights.s1, 1.0);
    }

    // --- grid parser: bad input --------------------------------------------
    #[test]
    fn grid_unknown_knob_errors() {
        let err = parse_grid("s5=1.0").unwrap_err();
        assert!(err.contains("unknown grid knob"), "{err}");
    }

    #[test]
    fn grid_bad_value_errors() {
        let err = parse_grid("s4=notanumber").unwrap_err();
        assert!(err.contains("value"), "{err}");
    }

    #[test]
    fn grid_out_of_range_value_errors() {
        // beta must be in [0, 1].
        let err = parse_grid("beta=2.0").unwrap_err();
        assert!(err.contains("out of range"), "{err}");
        // a negative weight is rejected too.
        let err = parse_grid("s1=-1.0").unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn grid_duplicate_knob_errors() {
        let err = parse_grid("s4=0.5;s4=1.0").unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn grid_empty_value_errors() {
        let err = parse_grid("s4=0.5,,1.0").unwrap_err();
        assert!(err.contains("empty value"), "{err}");
    }

    // --- apply_override: absent knobs keep baseline ------------------------
    #[test]
    fn apply_override_keeps_absent_knobs_at_baseline() {
        let mut ov = ConfigOverride::new();
        ov.insert("s4".into(), 1.25);
        ov.insert("rrf_k".into(), 80.0);
        let applied = apply_override(baseline(), &ov);
        assert_eq!(applied.weights.s4, 1.25);
        assert_eq!(applied.rrf_k, 80.0);
        // Untouched knobs are baseline.
        assert_eq!(applied.weights.s1, 1.0);
        assert_eq!(applied.weights.s2, 1.0);
        assert_eq!(applied.weights.s3_each, 0.5);
        assert_eq!(applied.beta, 0.5);
    }

    // --- ranking: stable sort by metric desc -------------------------------
    /// Build a Row with a chosen mean nDCG and wall-time (other metrics 0).
    fn row(label: &str, is_baseline: bool, ndcg: f64, wall_ms: u128) -> Row {
        let report = EvalReport {
            k: 10,
            query_count: 1,
            mean_precision_at_k: 0.0,
            mean_recall_at_k: 0.0,
            mean_mrr: 0.0,
            mean_ndcg_at_k: ndcg,
            per_query: Vec::new(),
        };
        Row {
            label: label.to_owned(),
            diff: Vec::new(),
            config: baseline(),
            report,
            wall_ms,
            is_baseline,
        }
    }

    #[test]
    fn rank_sorts_by_metric_descending() {
        let rows = vec![
            row("baseline", true, 0.50, 5),
            row("cfg0", false, 0.80, 5),
            row("cfg1", false, 0.65, 5),
        ];
        let ranked = rank(rows, "ndcg_at_k", None);
        let labels: Vec<&str> = ranked.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["cfg0", "cfg1", "baseline"]);
    }

    #[test]
    fn rank_tiebreaks_stably_on_walltime_then_label() {
        // Equal metric: faster wall-time first, then label.
        let rows = vec![
            row("cfg1", false, 0.80, 20),
            row("cfg0", false, 0.80, 10),
            row("cfg2", false, 0.80, 10),
        ];
        let ranked = rank(rows, "ndcg_at_k", None);
        let labels: Vec<&str> = ranked.iter().map(|r| r.label.as_str()).collect();
        // cfg0 and cfg2 are both 10ms -> label order; cfg1 (20ms) last.
        assert_eq!(labels, vec!["cfg0", "cfg2", "cfg1"]);
    }

    #[test]
    fn rank_filters_out_over_budget_configs() {
        let rows = vec![
            row("baseline", true, 0.50, 5),
            row("cfg0", false, 0.90, 200), // best metric but too slow
            row("cfg1", false, 0.60, 5),
        ];
        let ranked = rank(rows, "ndcg_at_k", Some(50));
        let labels: Vec<&str> = ranked.iter().map(|r| r.label.as_str()).collect();
        // cfg0 dropped by the latency filter; cfg1 wins the survivors.
        assert_eq!(labels, vec!["cfg1", "baseline"]);
    }

    // --- proposal emission --------------------------------------------------
    #[test]
    fn proposal_contains_winner_weights_and_delta() {
        let baseline_row = row("baseline", true, 0.50, 5);
        let mut winner = row("cfg0", false, 0.80, 5);
        winner.config = cfg(1.0, 1.0, 0.5, 1.25, 60.0, 0.3);
        winner.diff = vec![("s4".into(), 1.25), ("beta".into(), 0.3)];
        let text = render_proposal(
            &winner,
            &baseline_row,
            Path::new("golden.json"),
            "ndcg_at_k",
        );
        // K14 header present.
        assert!(text.contains("The machine proposes; you commit (K14)."));
        // The winning weights are in the [search] block.
        assert!(text.contains("[search.fusion]"));
        assert!(text.contains("s4 = 1.25"));
        assert!(text.contains("beta = 0.3"));
        // The knob delta and the metric delta are present.
        assert!(text.contains("s4: 1 -> 1.25"), "{text}");
        assert!(text.contains("ndcg_at_k: 0.5000 -> 0.8000"), "{text}");
        // The proposal mentions tuning.toml ONLY as the human instruction to
        // copy the proposal in (K14) - never as a write target. The refusal to
        // write tuning.toml itself is enforced in `run` (the file-name guard),
        // not in this pure text; here we just confirm the K14 instruction is
        // the sole mention.
        assert!(text.contains("copy into tuning.toml to apply"), "{text}");
    }

    #[test]
    fn proposal_when_baseline_wins_says_nothing_changed() {
        let baseline_row = row("baseline", true, 0.70, 5);
        // The winner IS the baseline row.
        let text = render_proposal(
            &baseline_row,
            &baseline_row,
            Path::new("golden.json"),
            "ndcg_at_k",
        );
        assert!(
            text.contains("baseline already optimal in this grid"),
            "{text}"
        );
        // Still emits a valid [search] block (the baseline config) so a copy is
        // a no-op rather than a blank file.
        assert!(text.contains("[search]"));
        assert!(text.contains("ndcg_at_k: 0.7000 -> 0.7000"), "{text}");
    }

    // =======================================================================
    // Voice arm (deterministic logic only: grid parse, apply, rank, propose —
    // the model-dependent scoring is covered by the env-guarded integration
    // test in tests/, not here).
    // =======================================================================
    use photoproof_core::voice_bench::Seg;
    use photoproof_core::voice_wer::WerScore;

    fn voice_baseline() -> VoiceTuning {
        VoiceTuning::default()
    }

    /// A `ScoreResult` with chosen gated/raw WER (the rest filled trivially).
    fn vscore(gated: f64, raw: f64) -> ScoreResult {
        let mk = |wer: f64| WerScore {
            wer,
            sub: 0,
            del: 0,
            ins: 0,
            ref_words: 10,
            hyp_words: 10,
            hits: 10,
        };
        ScoreResult {
            raw: mk(raw),
            gated: mk(gated),
            gated_segs: vec![Seg {
                onset_ms: 0,
                dur_ms: 1,
                text: "x".into(),
            }],
            abandoned: 0,
        }
    }

    fn vrow(label: &str, is_baseline: bool, gated: f64, raw: f64, wall_ms: u128) -> VoiceRow {
        VoiceRow {
            label: label.to_owned(),
            diff: Vec::new(),
            config: voice_baseline(),
            score: vscore(gated, raw),
            wall_ms,
            is_baseline,
        }
    }

    // --- voice grid parser --------------------------------------------------
    #[test]
    fn voice_grid_cartesian_product_is_deterministic() {
        // 4 x 3 = 12; last knob (vad_hang) varies fastest within each rule2.
        let configs = parse_voice_grid("rule2=0.8,1.0,1.2,1.5;vad_hang=10,15,20").unwrap();
        assert_eq!(configs.len(), 12);
        let first: Vec<(f64, f64)> = configs
            .iter()
            .take(3)
            .map(|c| (*c.get("rule2").unwrap(), *c.get("vad_hang").unwrap()))
            .collect();
        assert_eq!(first, vec![(0.8, 10.0), (0.8, 15.0), (0.8, 20.0)]);
    }

    #[test]
    fn voice_grid_unknown_and_out_of_range_error() {
        // A search knob is unknown to the voice arm.
        assert!(
            parse_voice_grid("s4=1.0")
                .unwrap_err()
                .contains("unknown grid knob")
        );
        // rule below the 0.1 s floor.
        assert!(
            parse_voice_grid("rule2=0.0")
                .unwrap_err()
                .contains("out of range")
        );
        // VAD probability above 1.
        assert!(
            parse_voice_grid("vad_enter=1.5")
                .unwrap_err()
                .contains("out of range")
        );
        // A fractional integer dial is a typo.
        assert!(
            parse_voice_grid("vad_hang=12.5")
                .unwrap_err()
                .contains("whole number")
        );
    }

    #[test]
    fn voice_zero_vad_threshold_is_in_band() {
        // The RAW-feed ceiling uses 0.0 thresholds; the grid must accept them.
        let configs = parse_voice_grid("vad_exit=0.0,0.35").unwrap();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].get("vad_exit"), Some(&0.0));
    }

    // --- apply: absent knobs keep baseline ----------------------------------
    #[test]
    fn apply_voice_override_keeps_absent_knobs_at_baseline() {
        let mut ov = ConfigOverride::new();
        ov.insert("rule2".into(), 0.8);
        ov.insert("vad_hang".into(), 20.0);
        let c = apply_voice_override(voice_baseline(), &ov);
        assert_eq!(c.rule2, 0.8);
        assert_eq!(c.vad_hang, 20);
        // Untouched knobs are baseline (the shipped defaults).
        assert_eq!(c.rule1, 2.4);
        assert_eq!(c.rule3, 20.0);
        assert_eq!(c.vad_enter, 0.5);
        assert_eq!(c.pre_roll_ms, 1_000);
    }

    // --- rank ASCENDING (lower gating-cost wins) ----------------------------
    #[test]
    fn voice_rank_sorts_ascending_by_gating_cost() {
        // gating_cost = gated - raw: baseline +0.10, cfg0 +0.02 (best), cfg1 +0.07.
        let rows = vec![
            vrow("baseline", true, 0.20, 0.10, 5),
            vrow("cfg0", false, 0.12, 0.10, 5),
            vrow("cfg1", false, 0.17, 0.10, 5),
        ];
        let ranked = rank_voice(rows, "gating_cost");
        let labels: Vec<&str> = ranked.iter().map(|r| r.label.as_str()).collect();
        // Lower gating-cost first: cfg0 (+0.02) < cfg1 (+0.07) < baseline (+0.10).
        assert_eq!(labels, vec!["cfg0", "cfg1", "baseline"]);
    }

    #[test]
    fn voice_rank_tiebreaks_stably_on_walltime_then_label() {
        // Equal gating-cost (all +0.0): faster wall first, then label.
        let rows = vec![
            vrow("cfg1", false, 0.10, 0.10, 20),
            vrow("cfg0", false, 0.10, 0.10, 10),
            vrow("cfg2", false, 0.10, 0.10, 10),
        ];
        let ranked = rank_voice(rows, "gating_cost");
        let labels: Vec<&str> = ranked.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["cfg0", "cfg2", "cfg1"]);
    }

    // --- proposal emission --------------------------------------------------
    #[test]
    fn voice_proposal_contains_winner_dials_and_delta() {
        let baseline_row = vrow("baseline", true, 0.20, 0.10, 5); // gating_cost +0.10
        let mut winner = vrow("cfg0", false, 0.12, 0.10, 5); // gating_cost +0.02
        winner.config = VoiceTuning {
            rule2: 0.8,
            vad_hang: 20,
            ..VoiceTuning::default()
        };
        winner.diff = vec![("rule2".into(), 0.8), ("vad_hang".into(), 20.0)];
        let text = render_voice_proposal(
            &winner,
            &baseline_row,
            Path::new("corpus.wav"),
            "gating_cost",
        );
        // K14 header present.
        assert!(text.contains("The machine proposes; you commit (K14)."));
        // The winning dials are in the [voice] block.
        assert!(text.contains("[voice]"));
        assert!(text.contains("rule2 = 0.8"), "{text}");
        assert!(text.contains("vad_hang = 20"), "{text}");
        // The knob delta and the gating-cost delta are present (baseline rule2
        // is the shipped 1.2; baseline vad_hang is 15).
        assert!(text.contains("rule2: 1.2 -> 0.8"), "{text}");
        assert!(text.contains("vad_hang: 15 -> 20"), "{text}");
        assert!(text.contains("gating_cost: +0.1000 -> +0.0200"), "{text}");
        // The human instruction to copy into tuning.toml (K14) — never a write
        // target (that refusal is enforced by guard_propose_target).
        assert!(text.contains("copy into tuning.toml to apply"), "{text}");
    }

    #[test]
    fn voice_proposal_when_baseline_wins_says_nothing_changed() {
        let baseline_row = vrow("baseline", true, 0.15, 0.10, 5);
        let text = render_voice_proposal(
            &baseline_row,
            &baseline_row,
            Path::new("corpus.wav"),
            "gating_cost",
        );
        assert!(
            text.contains("baseline already optimal in this grid"),
            "{text}"
        );
        // Still emits a valid [voice] block (the baseline config).
        assert!(text.contains("[voice]"));
        assert!(text.contains("rule2 = 1.2"), "{text}");
    }

    // --- K14: the propose guard refuses tuning.toml -------------------------
    #[test]
    fn propose_guard_refuses_tuning_toml() {
        assert!(guard_propose_target(Path::new("/x/tuning.toml")).is_err());
        assert!(guard_propose_target(Path::new("/x/tuning.proposed.toml")).is_ok());
    }

    // --- metric validation is per-subsystem ---------------------------------
    #[test]
    fn voice_metric_validation_rejects_search_metrics() {
        assert!(validate_metric("voice", "gating_cost").is_ok());
        assert!(validate_metric("voice", "gated_wer").is_ok());
        assert!(validate_metric("voice", "ndcg_at_k").is_err());
        assert!(validate_metric("search", "gating_cost").is_err());
        assert!(validate_metric("search", "ndcg_at_k").is_ok());
    }
}
