//! pp-tune-check — the metric-regression guard (DESIGN-TUNING-LOOP.md, the
//! "Testing (defense)" half: "protect quality as code changes").
//!
//! WHY this exists: every tuning primitive already emits JSON (the synthetic
//! retrieval-eval IR metrics, the pp-bench ingest throughput), but nothing
//! GUARDS them — a refactor can silently regress ranking or ingest and the
//! per-commit gate (fmt/clippy/test) would stay green. This binary is that
//! guard: it compares the cheap synthetic benches' freshly-measured metrics to
//! committed baseline values within a per-metric TOLERANCE (not exact equality
//! — perf is machine-variable, and even the deterministic search metric gets a
//! small absolute slack), and exits non-zero with a per-metric report if any
//! metric regresses past its tolerance.
//!
//! It does NOT run the benches itself — `scripts/tune-check.sh` runs them and
//! hands this binary their JSON. That split keeps the comparator pure and
//! unit-testable (the `compare` function below is a deterministic function of
//! observed-vs-baseline numbers; see the tests) while the shell owns the
//! release build + bench invocation.
//!
//! USAGE
//!   pp-tune-check --baselines tuning-baselines.json \
//!       --retrieval <synthetic-eval.json> --ingest <bench.jsonl> [--json]
//!
//!   pp-tune-check --baselines ... --retrieval ... --ingest ... --update-baseline
//!       (rewrites the baselines file from the observed values — K14: the human
//!        commits the new reference when an intended improvement lands)
//!
//! EXIT CODES: 0 = every metric within tolerance (or baseline updated);
//! 1 = at least one metric regressed; 2 = usage/IO error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

// ---------------------------------------------------------------------------
// The comparator (pure — the unit-tested core)
// ---------------------------------------------------------------------------

/// Which direction is "better" for a metric, and therefore which way a
/// regression goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Higher is better (nDCG, MRR, recall, files_per_s). A regression is a
    /// DROP below the allowed floor.
    HigherIsBetter,
}

/// How the tolerance is applied to the baseline to get the failing threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToleranceKind {
    /// Floor = baseline - tolerance. Used for the machine-INDEPENDENT search
    /// metrics: a fixed absolute slack (0.02) catches a real ranking change,
    /// not float noise.
    Absolute,
    /// Floor = baseline * (1 - tolerance). Used for the machine-VARIABLE ingest
    /// throughput: a relative slack (0.15) absorbs host-speed/load swings while
    /// still catching an algorithmic slowdown. See tuning-baselines.json for
    /// the full WHY.
    Relative,
}

/// One metric's verdict: its name, the observed value, the baseline, the
/// computed failing floor, and whether it passed.
#[derive(Debug, Clone, PartialEq)]
struct Verdict {
    name: String,
    observed: f64,
    baseline: f64,
    tolerance: f64,
    floor: f64,
    passed: bool,
}

/// Compare one observed metric to its baseline under a tolerance. This is the
/// whole comparator nucleus: deterministic in (observed, baseline, tolerance,
/// kind, direction), so the tests below pin its three cases (regressed /
/// within-tolerance / improvement).
fn compare(
    name: &str,
    observed: f64,
    baseline: f64,
    tolerance: f64,
    kind: ToleranceKind,
    dir: Direction,
) -> Verdict {
    // The floor is the lowest observed value that still passes.
    let floor = match (kind, dir) {
        (ToleranceKind::Absolute, Direction::HigherIsBetter) => baseline - tolerance,
        (ToleranceKind::Relative, Direction::HigherIsBetter) => baseline * (1.0 - tolerance),
    };
    // Higher-is-better: pass iff observed has not fallen below the floor. A
    // value at or ABOVE baseline (an improvement) always passes; the floor only
    // bites on a drop.
    let passed = observed >= floor;
    Verdict {
        name: name.to_owned(),
        observed,
        baseline,
        tolerance,
        floor,
        passed,
    }
}

// ---------------------------------------------------------------------------
// JSON extraction (robust — never panics on a missing/garbage field)
// ---------------------------------------------------------------------------

/// Pull a finite f64 from a JSON value at a dotted path (e.g. "mean.ndcg_at_k").
/// Returns a descriptive error rather than panicking, so a malformed bench
/// output is a clean exit-2, not a crash — the guard parses machine output it
/// did not necessarily produce.
fn get_f64(v: &Value, path: &str) -> Result<f64, String> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur
            .get(seg)
            .ok_or_else(|| format!("missing field {path:?}"))?;
    }
    let n = cur
        .as_f64()
        .ok_or_else(|| format!("field {path:?} is not a number"))?;
    if !n.is_finite() {
        return Err(format!("field {path:?} is not finite ({n})"));
    }
    Ok(n)
}

/// Parse the pp-bench output. The bench APPENDS one JSON object per line to a
/// .jsonl file, so a fresh run's metrics are the LAST non-empty line — read
/// that, not the first (an old results file may have stale leading lines).
fn parse_last_jsonl(raw: &str) -> Result<Value, String> {
    let last = raw
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| "ingest results file is empty".to_owned())?;
    serde_json::from_str(last).map_err(|e| format!("parsing ingest JSON line: {e}"))
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

struct Args {
    baselines: PathBuf,
    retrieval: PathBuf,
    ingest: PathBuf,
    json: bool,
    update_baseline: bool,
}

fn usage() -> &'static str {
    "usage: pp-tune-check --baselines <tuning-baselines.json> \
     --retrieval <synthetic-eval.json> --ingest <bench.jsonl> \
     [--json] [--update-baseline]"
}

fn parse_args() -> Result<Args, String> {
    let mut baselines: Option<PathBuf> = None;
    let mut retrieval: Option<PathBuf> = None;
    let mut ingest: Option<PathBuf> = None;
    let mut json = false;
    let mut update_baseline = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("flag {name} needs a value"))
        };
        match flag.as_str() {
            "--baselines" => baselines = Some(PathBuf::from(val("--baselines")?)),
            "--retrieval" => retrieval = Some(PathBuf::from(val("--retrieval")?)),
            "--ingest" => ingest = Some(PathBuf::from(val("--ingest")?)),
            "--json" => json = true,
            "--update-baseline" => update_baseline = true,
            "-h" | "--help" => return Err(usage().to_owned()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }
    Ok(Args {
        baselines: baselines.ok_or_else(|| format!("missing --baselines\n{}", usage()))?,
        retrieval: retrieval.ok_or_else(|| format!("missing --retrieval\n{}", usage()))?,
        ingest: ingest.ok_or_else(|| format!("missing --ingest\n{}", usage()))?,
        json,
        update_baseline,
    })
}

/// Load + parse a JSON file into a `Value`, tagging IO/parse errors with the path.
fn read_json(path: &Path) -> Result<Value, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Build the list of verdicts from the baseline doc + the two observed outputs.
/// Each metric carries its own tolerance + kind (absolute for search, relative
/// for ingest) so the comparator never has to know which is which.
fn build_verdicts(
    baselines: &Value,
    retrieval_obs: &Value,
    ingest_obs: &Value,
) -> Result<Vec<Verdict>, String> {
    let mut out = Vec::new();

    // --- search: the three machine-independent IR means, absolute tolerance ---
    let s_tol = get_f64(baselines, "search.tolerance")?;
    for metric in ["mean_ndcg_at_k", "mean_mrr", "mean_recall_at_k"] {
        let baseline = get_f64(baselines, &format!("search.{metric}"))?;
        // The retrieval-eval JSON nests the means under "mean", keyed without
        // the "mean_" prefix (mean.ndcg_at_k etc.).
        let obs_key = metric.strip_prefix("mean_").unwrap_or(metric);
        let observed = get_f64(retrieval_obs, &format!("mean.{obs_key}"))?;
        out.push(compare(
            &format!("search.{metric}"),
            observed,
            baseline,
            s_tol,
            ToleranceKind::Absolute,
            Direction::HigherIsBetter,
        ));
    }

    // --- ingest: throughput, relative tolerance (machine-variable) ---
    let i_tol = get_f64(baselines, "ingest.tolerance")?;
    let baseline = get_f64(baselines, "ingest.files_per_s")?;
    let observed = get_f64(ingest_obs, "files_per_s")?;
    out.push(compare(
        "ingest.files_per_s",
        observed,
        baseline,
        i_tol,
        ToleranceKind::Relative,
        Direction::HigherIsBetter,
    ));

    Ok(out)
}

/// Rewrite the baselines doc IN PLACE with the observed values, preserving the
/// tolerances + comments. Only the measured numbers move — never the slack.
fn update_baselines(
    baselines: &mut Value,
    retrieval_obs: &Value,
    ingest_obs: &Value,
) -> Result<(), String> {
    let set = |doc: &mut Value, path: &str, n: f64| -> Result<(), String> {
        let mut cur = doc;
        let segs: Vec<&str> = path.split('.').collect();
        for seg in &segs[..segs.len() - 1] {
            cur = cur
                .get_mut(*seg)
                .ok_or_else(|| format!("baseline missing {path:?}"))?;
        }
        let last = segs[segs.len() - 1];
        let slot = cur
            .get_mut(last)
            .ok_or_else(|| format!("baseline missing {path:?}"))?;
        *slot = serde_json::json!(n);
        Ok(())
    };

    set(
        baselines,
        "search.mean_ndcg_at_k",
        get_f64(retrieval_obs, "mean.ndcg_at_k")?,
    )?;
    set(
        baselines,
        "search.mean_mrr",
        get_f64(retrieval_obs, "mean.mrr")?,
    )?;
    set(
        baselines,
        "search.mean_recall_at_k",
        get_f64(retrieval_obs, "mean.recall_at_k")?,
    )?;
    set(
        baselines,
        "ingest.files_per_s",
        get_f64(ingest_obs, "files_per_s")?,
    )?;
    Ok(())
}

fn print_report(verdicts: &[Verdict]) {
    println!("metric-regression guard (tune-check)");
    println!(
        "{:<24} {:>10} {:>10} {:>10}  verdict",
        "metric", "observed", "baseline", "floor"
    );
    for v in verdicts {
        println!(
            "{:<24} {:>10.4} {:>10.4} {:>10.4}  {}",
            v.name,
            v.observed,
            v.baseline,
            v.floor,
            if v.passed { "OK" } else { "REGRESSED" }
        );
    }
    let failed = verdicts.iter().filter(|v| !v.passed).count();
    if failed == 0 {
        println!("\nall {} metrics within tolerance.", verdicts.len());
    } else {
        println!("\n{failed} metric(s) regressed past tolerance.");
    }
}

fn print_report_json(verdicts: &[Verdict]) {
    let rows: Vec<Value> = verdicts
        .iter()
        .map(|v| {
            serde_json::json!({
                "metric": v.name,
                "observed": v.observed,
                "baseline": v.baseline,
                "tolerance": v.tolerance,
                "floor": v.floor,
                "passed": v.passed,
            })
        })
        .collect();
    let failed = verdicts.iter().filter(|v| !v.passed).count();
    let doc = serde_json::json!({
        "schema": 1,
        "passed": failed == 0,
        "regressed_count": failed,
        "metrics": rows,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}

fn run(args: &Args) -> Result<bool, String> {
    let mut baselines = read_json(&args.baselines)?;
    let retrieval_obs = read_json(&args.retrieval)?;
    // pp-bench appends to a .jsonl; take the freshest (last) line.
    let ingest_raw = std::fs::read_to_string(&args.ingest)
        .map_err(|e| format!("reading {}: {e}", args.ingest.display()))?;
    let ingest_obs = parse_last_jsonl(&ingest_raw)?;

    if args.update_baseline {
        update_baselines(&mut baselines, &retrieval_obs, &ingest_obs)?;
        let pretty = serde_json::to_string_pretty(&baselines)
            .map_err(|e| format!("serializing baselines: {e}"))?;
        // Trailing newline so the committed file ends cleanly (the repo's files do).
        std::fs::write(&args.baselines, format!("{pretty}\n"))
            .map_err(|e| format!("writing {}: {e}", args.baselines.display()))?;
        eprintln!(
            "baseline updated from observed runs -> {} (review + commit it deliberately)",
            args.baselines.display()
        );
        return Ok(true);
    }

    let verdicts = build_verdicts(&baselines, &retrieval_obs, &ingest_obs)?;
    if args.json {
        print_report_json(&verdicts);
    } else {
        print_report(&verdicts);
    }
    Ok(verdicts.iter().all(|v| v.passed))
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
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE, // a metric regressed
        Err(msg) => {
            eprintln!("pp-tune-check: {msg}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the comparator: the three cases the guard exists to distinguish ----

    #[test]
    fn absolute_metric_below_tolerance_fails() {
        // Search nDCG baseline 1.0, abs tolerance 0.02 -> floor 0.98. An
        // observed 0.95 is a real regression and must FAIL.
        let v = compare(
            "search.ndcg",
            0.95,
            1.0,
            0.02,
            ToleranceKind::Absolute,
            Direction::HigherIsBetter,
        );
        assert!(!v.passed);
        assert!((v.floor - 0.98).abs() < 1e-12);
    }

    #[test]
    fn absolute_metric_within_tolerance_passes() {
        // 0.99 is below baseline but inside the 0.02 slack (>= 0.98 floor) -> PASS.
        let v = compare(
            "search.ndcg",
            0.99,
            1.0,
            0.02,
            ToleranceKind::Absolute,
            Direction::HigherIsBetter,
        );
        assert!(v.passed);
    }

    #[test]
    fn absolute_metric_exactly_at_floor_passes() {
        // Boundary: observed == floor passes (>=, not >). A metric sitting
        // exactly at the allowed slack is not a regression.
        let v = compare(
            "search.ndcg",
            0.98,
            1.0,
            0.02,
            ToleranceKind::Absolute,
            Direction::HigherIsBetter,
        );
        assert!(v.passed);
    }

    #[test]
    fn improvement_always_passes() {
        // An IMPROVEMENT (observed above baseline) must never be flagged — the
        // floor only bites on a drop.
        let v = compare(
            "search.ndcg",
            1.0,
            0.9,
            0.02,
            ToleranceKind::Absolute,
            Direction::HigherIsBetter,
        );
        assert!(v.passed);
    }

    #[test]
    fn relative_ingest_within_tolerance_passes() {
        // Ingest baseline 100 files/s, 15% slack -> floor 85. A slower machine
        // at 90 still passes (machine variance, not a regression).
        let v = compare(
            "ingest.files_per_s",
            90.0,
            100.0,
            0.15,
            ToleranceKind::Relative,
            Direction::HigherIsBetter,
        );
        assert!(v.passed);
        assert!((v.floor - 85.0).abs() < 1e-12);
    }

    #[test]
    fn relative_ingest_below_tolerance_fails() {
        // 80 < 85 floor -> a genuine slowdown, FAIL.
        let v = compare(
            "ingest.files_per_s",
            80.0,
            100.0,
            0.15,
            ToleranceKind::Relative,
            Direction::HigherIsBetter,
        );
        assert!(!v.passed);
    }

    // --- JSON extraction: robust on the real shapes + on garbage ------------

    #[test]
    fn get_f64_walks_a_dotted_path() {
        let v: Value = serde_json::json!({ "mean": { "ndcg_at_k": 0.87 } });
        assert!((get_f64(&v, "mean.ndcg_at_k").unwrap() - 0.87).abs() < 1e-12);
    }

    #[test]
    fn get_f64_missing_field_is_an_error_not_a_panic() {
        let v: Value = serde_json::json!({ "mean": {} });
        assert!(get_f64(&v, "mean.ndcg_at_k").is_err());
        assert!(get_f64(&v, "nope").is_err());
    }

    #[test]
    fn get_f64_rejects_non_numbers_and_nan() {
        let v: Value = serde_json::json!({ "x": "not a number", "n": null });
        assert!(get_f64(&v, "x").is_err());
        assert!(get_f64(&v, "n").is_err());
    }

    #[test]
    fn parse_last_jsonl_takes_the_freshest_line() {
        // An appended results file: the LAST line is the fresh run. A stale
        // leading line must not be read.
        let raw = "{\"files_per_s\": 100.0}\n{\"files_per_s\": 142.5}\n";
        let v = parse_last_jsonl(raw).unwrap();
        assert!((get_f64(&v, "files_per_s").unwrap() - 142.5).abs() < 1e-12);
    }

    #[test]
    fn parse_last_jsonl_ignores_trailing_blank_lines() {
        let raw = "{\"files_per_s\": 1.0}\n{\"files_per_s\": 2.0}\n\n  \n";
        let v = parse_last_jsonl(raw).unwrap();
        assert!((get_f64(&v, "files_per_s").unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn parse_last_jsonl_empty_is_an_error() {
        assert!(parse_last_jsonl("\n\n").is_err());
    }

    // --- end-to-end of the pure layer: a healthy run passes, a regressed
    //     run fails, against the real JSON shapes the benches emit. ----------

    fn baselines_doc() -> Value {
        serde_json::json!({
            "search": {
                "tolerance": 0.02,
                "mean_ndcg_at_k": 1.0,
                "mean_mrr": 1.0,
                "mean_recall_at_k": 1.0
            },
            "ingest": { "tolerance": 0.15, "files_per_s": 140.0 }
        })
    }

    fn retrieval_doc(ndcg: f64) -> Value {
        serde_json::json!({
            "mean": { "ndcg_at_k": ndcg, "mrr": 1.0, "recall_at_k": 1.0 }
        })
    }

    #[test]
    fn build_verdicts_all_green_when_healthy() {
        let v = build_verdicts(
            &baselines_doc(),
            &retrieval_doc(1.0),
            &serde_json::json!({ "files_per_s": 150.0 }), // faster than baseline
        )
        .unwrap();
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|x| x.passed));
    }

    #[test]
    fn build_verdicts_flags_a_ranking_regression() {
        // nDCG collapsed (a broken fusion refactor); ingest fine.
        let v = build_verdicts(
            &baselines_doc(),
            &retrieval_doc(0.5),
            &serde_json::json!({ "files_per_s": 150.0 }),
        )
        .unwrap();
        let ndcg = v
            .iter()
            .find(|x| x.name == "search.mean_ndcg_at_k")
            .unwrap();
        assert!(!ndcg.passed, "a 0.5 nDCG vs 1.0 baseline must regress");
        // the other metrics stay green
        assert!(v.iter().filter(|x| x.passed).count() == 3);
    }

    #[test]
    fn build_verdicts_flags_an_ingest_slowdown() {
        let v = build_verdicts(
            &baselines_doc(),
            &retrieval_doc(1.0),
            &serde_json::json!({ "files_per_s": 100.0 }), // 100 < 140*0.85 = 119
        )
        .unwrap();
        let ing = v.iter().find(|x| x.name == "ingest.files_per_s").unwrap();
        assert!(!ing.passed);
    }

    #[test]
    fn update_baselines_moves_values_but_not_tolerances() {
        let mut doc = baselines_doc();
        update_baselines(
            &mut doc,
            &retrieval_doc(0.9),
            &serde_json::json!({ "files_per_s": 200.0 }),
        )
        .unwrap();
        assert!((get_f64(&doc, "search.mean_ndcg_at_k").unwrap() - 0.9).abs() < 1e-12);
        assert!((get_f64(&doc, "ingest.files_per_s").unwrap() - 200.0).abs() < 1e-12);
        // tolerances are untouched.
        assert!((get_f64(&doc, "search.tolerance").unwrap() - 0.02).abs() < 1e-12);
        assert!((get_f64(&doc, "ingest.tolerance").unwrap() - 0.15).abs() < 1e-12);
    }
}
