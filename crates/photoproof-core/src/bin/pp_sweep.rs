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

/// Built-in @k default when neither the CLI nor the query set pins one (matches
/// `pp-retrieval-eval`).
const DEFAULT_K: usize = 10;

/// The default ranking metric: nDCG@k (DESIGN: search maximizes `ndcg_at_k`).
const DEFAULT_METRIC: &str = "ndcg_at_k";

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

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    /// The subsystem to sweep (only `search` is implemented).
    subsystem: String,
    db: PathBuf,
    queries: PathBuf,
    grid: String,
    metric: String,
    k: Option<usize>,
    max_latency_ms: Option<u64>,
    json: bool,
    /// `--propose <file>` target; `None` skips proposal emission.
    propose: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: pp-sweep search --db <photoproof.db> --queries <queryset.json> \
     --grid \"s4=0.5,1.0;beta=0.3,0.5\" \
     [--metric ndcg_at_k] [--k N] [--max-latency-ms N] [--json] [--propose <file>]\n\
     (paths also from env PP_RETRIEVAL_DB / PP_RETRIEVAL_QUERYSET; args win)\n\
     knobs: s1 s2 s3 s4 rrf_k beta ; metrics: ndcg_at_k precision_at_k recall_at_k mrr"
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
                "missing subsystem (expected `search`)\n{}",
                usage()
            ));
        }
    };

    // env supplies path defaults; explicit flags override (args win).
    let mut db = std::env::var_os("PP_RETRIEVAL_DB").map(PathBuf::from);
    let mut queries = std::env::var_os("PP_RETRIEVAL_QUERYSET").map(PathBuf::from);
    let mut grid = String::new();
    let mut metric = DEFAULT_METRIC.to_owned();
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
            "--grid" => grid = val("--grid")?,
            "--metric" => metric = val("--metric")?,
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

    let db = db.ok_or_else(|| format!("missing --db (or PP_RETRIEVAL_DB)\n{}", usage()))?;
    let queries = queries
        .ok_or_else(|| format!("missing --queries (or PP_RETRIEVAL_QUERYSET)\n{}", usage()))?;
    if grid.trim().is_empty() {
        return Err(format!("missing --grid\n{}", usage()));
    }
    validate_metric(&metric)?;
    Ok(Args {
        subsystem,
        db,
        queries,
        grid,
        metric,
        k,
        max_latency_ms,
        json,
        propose,
    })
}

/// The four primary-metric selectors the leaderboard can rank by. Reject an
/// unknown metric at parse time so a typo never silently ranks by nDCG.
fn validate_metric(metric: &str) -> Result<(), String> {
    match metric {
        "ndcg_at_k" | "precision_at_k" | "recall_at_k" | "mrr" => Ok(()),
        other => Err(format!(
            "unknown --metric {other:?} (want ndcg_at_k|precision_at_k|recall_at_k|mrr)"
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

/// Parse `--grid "knob=v,v,...;knob=v,..."` into the per-knob value lists, then
/// expand to the CARTESIAN PRODUCT of configs. Each value is an `f64`,
/// range-validated against the knob's tuning bounds (out-of-range is an error,
/// not a clamp). A repeated knob, an unknown knob, or an unparsable value all
/// error with a clear message.
///
/// Ordering is deterministic: knobs iterate in `KNOBS` order, values in the
/// order written, and the product enumerates last-knob-fastest. That makes the
/// resulting leaderboard reproducible (the tiebreak below leans on it).
fn parse_grid(grid: &str) -> Result<Vec<ConfigOverride>, String> {
    // knob -> its value list, kept in KNOBS column order via a BTreeMap keyed
    // by the column index so the cartesian product is deterministic.
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
        let col = KNOBS
            .iter()
            .position(|k| *k == knob)
            .ok_or_else(|| format!("unknown grid knob {knob:?} (want one of {KNOBS:?})"))?;
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
            validate_knob(knob, v)?;
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

/// Range-validate one knob value against its documented tuning bound.
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
        "db": args.db.display().to_string(),
        "queries": args.queries.display().to_string(),
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
    // Only `search` is implemented; an unknown subsystem errors cleanly (the
    // DESIGN build sequence adds voice/ingest later).
    if args.subsystem != "search" {
        return Err(format!(
            "subsystem {:?} not implemented (only `search` for now)",
            args.subsystem
        ));
    }

    let raw = std::fs::read_to_string(&args.queries)
        .map_err(|e| format!("reading query set {}: {e}", args.queries.display()))?;
    let query_set: QuerySet = serde_json::from_str(&raw)
        .map_err(|e| format!("parsing query set {}: {e}", args.queries.display()))?;

    // Source the baseline from the SAME `tuning.toml` the app reads (beside the
    // DB), so the baseline row IS the live committed config. Absent a file this
    // is the spec defaults. `EvalConfig::default()` then reads that global.
    if let Some(app_data) = args.db.parent() {
        photoproof_core::tuning::init_from(app_data);
    }
    let baseline = EvalConfig::default();

    // k precedence: --k > query-set default_k > built-in DEFAULT_K (matches
    // pp-retrieval-eval).
    let k = args.k.or(query_set.default_k).unwrap_or(DEFAULT_K);

    let overrides = parse_grid(&args.grid)?;

    let searcher = Searcher::open(&args.db)
        .map_err(|e| format!("opening library DB {}: {e}", args.db.display()))?;

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
        let text = render_proposal(winner, baseline_row, &args.queries, &args.metric);
        // K14: we write ONLY the proposal path the founder named, NEVER
        // tuning.toml. Guard against a caller pointing --propose at it.
        if path.file_name().and_then(|n| n.to_str()) == Some("tuning.toml") {
            return Err(
                "refusing to write tuning.toml: pp-sweep PROPOSES only (K14). \
                 Point --propose at e.g. tuning.proposed.toml."
                    .into(),
            );
        }
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
}
