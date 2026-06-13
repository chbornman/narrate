//! pp-retrieval-eval — the M3 golden-query retrieval-quality gate's runner.
//!
//! Loads a golden query set (JSON; see `retrieval_eval::QuerySet`), runs each
//! query through the REAL hybrid search rig against a populated library DB,
//! scores the ranked results with the pure IR metrics
//! (`retrieval_eval::QueryMetrics`), and prints a per-query + aggregate
//! report (human table by default, `--json` for machine diffing). It accepts
//! `FusionWeights` overrides so the founder can SWEEP the weights and read
//! the metric deltas — the literal job of the gate that "settles S4
//! always-on weight (B69) and the reranker go/no-go".
//!
//! This is a FOUNDER-MACHINE tool, not a CI test: it needs a populated
//! `photoproof.db` (real images + journal + vectors) and a real query set,
//! neither of which lives in the repo. The CI proof of the same pipe is the
//! in-process `retrieval_eval_sample` integration test (synthetic corpus, no
//! models). Nothing here is hardcoded to a path: the DB and query set come
//! from args/env.
//!
//! USAGE
//!   pp-retrieval-eval --db <photoproof.db> --queries <queryset.json>
//!                     [--k N] [--json]
//!                     [--s1 W] [--s2 W] [--s3 W] [--s4 W]
//!
//!   Paths may also come from the environment (args win):
//!     PP_RETRIEVAL_DB        the library SQLite database
//!     PP_RETRIEVAL_QUERYSET  the golden query-set JSON
//!
//! WEIGHT SWEEP (the point of the gate). Run the baseline, then re-run with
//! one weight moved, and diff the two --json reports:
//!
//!   # baseline (spec defaults: s1=1 s2=1 s3=0.5 s4=1)
//!   pp-retrieval-eval --db ~/Library/Application\ Support/com.photoproof.desktop/photoproof.db \
//!       --queries test-corpora/retrieval/golden.json --json > /tmp/base.json
//!
//!   # does dialing S4 (image_clip, B69) DOWN hurt? sweep it and compare
//!   pp-retrieval-eval --db ... --queries test-corpora/retrieval/golden.json \
//!       --s4 0.5 --json > /tmp/s4-half.json
//!   # then: jq '.mean' /tmp/base.json /tmp/s4-half.json   (or diff the tables)
//!
//! The relevant ids in the query set are image content hashes
//! (`ContentHash::as_str()`), which is exactly what the search results carry
//! — so a query set authored against a real library matches by string
//! equality. Drop the real, library-specific set at the gitignored path
//! `test-corpora/retrieval/` (private hashes never enter git).
//!
//! NOTE on the similarity-blend beta. The §5.3 dense-signal tilt
//! `SIM_BLEND_BETA` is a compile-time constant in `search/hybrid.rs`, NOT a
//! field of the `FusionWeights`/`HybridOptions` API this runner is restricted
//! to. Sweeping beta therefore means editing that constant and rebuilding —
//! this runner sweeps the four fusion WEIGHTS, which ARE in the API. The weight
//! sweep already answers the B69 "how much should S4 vote" question the gate
//! exists for; a beta sweep is a source edit the same report measures.
//!
//! NOTE on signals. This runner uses the keyword-only rig (no embedders/LLM):
//! the heavy real-model wiring (loading CLIP/text embedders + the parse LLM,
//! pointing at the on-disk vector store) is the desktop app's job and well
//! beyond a metrics runner's scope. With keyword-only the FusionWeights still
//! shape any query that reaches fusion through S2 + the SQL-only S3
//! summaries_fts sub-list; the full four-signal sweep is a desktop-driven run
//! that feeds the same query set to the same scorer. The metrics + query-set
//! format this binary ships are the instrument either path uses.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use photoproof_core::retrieval_eval::{
    EvalReport, GoldenQuery, NamedQueryMetrics, QueryMetrics, QuerySet,
};
use photoproof_core::search::{FusionWeights, HybridOptions, Searcher, keyword_only_rig};

/// Built-in @k default when neither the CLI nor the query set pins one.
const DEFAULT_K: usize = 10;

struct Args {
    db: PathBuf,
    queries: PathBuf,
    k: Option<usize>,
    json: bool,
    weights: WeightOverrides,
}

/// Per-signal fusion-weight overrides; `None` keeps the spec default for that
/// signal (so a sweep moves ONE knob and leaves the rest at baseline).
#[derive(Default)]
struct WeightOverrides {
    s1: Option<f64>,
    s2: Option<f64>,
    s3_each: Option<f64>,
    s4: Option<f64>,
}

impl WeightOverrides {
    /// Fold the overrides onto the spec defaults — the `FusionWeights` the
    /// run actually used (echoed into the report so a sweep is reproducible).
    fn apply(&self) -> FusionWeights {
        let d = FusionWeights::default();
        FusionWeights {
            s1: self.s1.unwrap_or(d.s1),
            s2: self.s2.unwrap_or(d.s2),
            s3_each: self.s3_each.unwrap_or(d.s3_each),
            s4: self.s4.unwrap_or(d.s4),
        }
    }
}

fn usage() -> &'static str {
    "usage: pp-retrieval-eval --db <photoproof.db> --queries <queryset.json> \
     [--k N] [--json] [--s1 W] [--s2 W] [--s3 W] [--s4 W]\n\
     (paths also from env PP_RETRIEVAL_DB / PP_RETRIEVAL_QUERYSET; args win)"
}

fn parse_args() -> Result<Args, String> {
    // env supplies defaults; explicit flags override (args win — the usual
    // precedence so a one-off run can point elsewhere without unsetting env).
    let mut db = std::env::var_os("PP_RETRIEVAL_DB").map(PathBuf::from);
    let mut queries = std::env::var_os("PP_RETRIEVAL_QUERYSET").map(PathBuf::from);
    let mut k: Option<usize> = None;
    let mut json = false;
    let mut weights = WeightOverrides::default();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    // Pull the value following a flag, erroring (not panicking) if absent.
    while let Some(flag) = it.next() {
        let mut val = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("flag {name} needs a value"))
        };
        let mut wval = |name: &str| -> Result<f64, String> {
            val(name)?
                .parse::<f64>()
                .map_err(|e| format!("flag {name}: {e}"))
        };
        match flag.as_str() {
            "--db" => db = Some(PathBuf::from(val("--db")?)),
            "--queries" => queries = Some(PathBuf::from(val("--queries")?)),
            "--k" => {
                k = Some(
                    val("--k")?
                        .parse::<usize>()
                        .map_err(|e| format!("flag --k: {e}"))?,
                )
            }
            "--json" => json = true,
            "--s1" => weights.s1 = Some(wval("--s1")?),
            "--s2" => weights.s2 = Some(wval("--s2")?),
            "--s3" => weights.s3_each = Some(wval("--s3")?),
            "--s4" => weights.s4 = Some(wval("--s4")?),
            "-h" | "--help" => return Err(usage().to_owned()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }

    let db = db.ok_or_else(|| format!("missing --db (or PP_RETRIEVAL_DB)\n{}", usage()))?;
    let queries = queries
        .ok_or_else(|| format!("missing --queries (or PP_RETRIEVAL_QUERYSET)\n{}", usage()))?;
    Ok(Args {
        db,
        queries,
        k,
        json,
        weights,
    })
}

/// Run one query through the keyword-only hybrid rig and return the ranked
/// image-hash list — the scorer's input. The `weights` shape any fused query;
/// `now` defaults to wall-clock (relative date filters in a parsed query, if
/// any, resolve against it). Keyword-only means no model is loaded here.
fn ranked_hashes(
    searcher: &Searcher,
    query: &str,
    weights: FusionWeights,
) -> Result<Vec<String>, String> {
    let opts = HybridOptions {
        weights,
        ..HybridOptions::default()
    };
    let out = searcher
        .hybrid_search(query, &[], &keyword_only_rig(), &opts)
        .map_err(|e| format!("search failed for {query:?}: {e}"))?;
    Ok(out
        .images
        .iter()
        .map(|r| r.image_hash.as_str().to_owned())
        .collect())
}

fn run(args: &Args) -> Result<(), String> {
    let raw = std::fs::read_to_string(&args.queries)
        .map_err(|e| format!("reading query set {}: {e}", args.queries.display()))?;
    let query_set: QuerySet = serde_json::from_str(&raw)
        .map_err(|e| format!("parsing query set {}: {e}", args.queries.display()))?;

    // k precedence: --k > query-set default_k > built-in DEFAULT_K.
    let k = args.k.or(query_set.default_k).unwrap_or(DEFAULT_K);
    let weights = args.weights.apply();

    let searcher = Searcher::open(&args.db)
        .map_err(|e| format!("opening library DB {}: {e}", args.db.display()))?;

    let mut scored: Vec<NamedQueryMetrics> = Vec::with_capacity(query_set.queries.len());
    for GoldenQuery {
        query, relevant, ..
    } in &query_set.queries
    {
        let ranked = ranked_hashes(&searcher, query, weights)?;
        let relevant_set: HashSet<String> = relevant.iter().cloned().collect();
        scored.push(NamedQueryMetrics {
            query: query.clone(),
            metrics: QueryMetrics::score(&ranked, &relevant_set, k),
        });
    }

    let report = EvalReport::from_scored(k, scored);
    if args.json {
        print_json(&report, &weights, &args.db, &args.queries);
    } else {
        print_table(&report, &weights);
    }
    Ok(())
}

/// Human-readable report. No em-dashes (the repo's UI-copy gate) and ASCII
/// only so it reads in any terminal.
fn print_table(report: &EvalReport, w: &FusionWeights) {
    println!(
        "retrieval eval: {} queries @k={}  weights[s1={} s2={} s3_each={} s4={}]",
        report.query_count, report.k, w.s1, w.s2, w.s3_each, w.s4
    );
    println!(
        "{:<40} {:>7} {:>7} {:>7} {:>7}",
        "query", "P@k", "R@k", "MRR", "nDCG"
    );
    for nq in &report.per_query {
        let m = &nq.metrics;
        // Truncate long queries to keep the table aligned; the full text is
        // in the --json form for anything that needs it.
        let q = truncate(&nq.query, 40);
        println!(
            "{:<40} {:>7.4} {:>7.4} {:>7.4} {:>7.4}",
            q, m.precision_at_k, m.recall_at_k, m.mrr, m.ndcg_at_k
        );
    }
    println!(
        "{:<40} {:>7.4} {:>7.4} {:>7.4} {:>7.4}",
        "MEAN",
        report.mean_precision_at_k,
        report.mean_recall_at_k,
        report.mean_mrr,
        report.mean_ndcg_at_k
    );
}

/// Machine-diffable JSON: the aggregate, the per-query rows, and the exact
/// weights/inputs the run used (so two sweep outputs are self-describing).
fn print_json(
    report: &EvalReport,
    w: &FusionWeights,
    db: &std::path::Path,
    queries: &std::path::Path,
) {
    // Hand-built so the schema is explicit and stable for diffing; the report
    // structs are not Serialize (core stays serde-free at that boundary, the
    // `pass_counters` precedent), and the shape we want here is the report's,
    // not its in-memory layout.
    let per_query: Vec<serde_json::Value> = report
        .per_query
        .iter()
        .map(|nq| {
            let m = &nq.metrics;
            serde_json::json!({
                "query": nq.query,
                "precision_at_k": m.precision_at_k,
                "recall_at_k": m.recall_at_k,
                "mrr": m.mrr,
                "ndcg_at_k": m.ndcg_at_k,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": 1,
        "k": report.k,
        "query_count": report.query_count,
        "db": db.display().to_string(),
        "queries_file": queries.display().to_string(),
        "weights": { "s1": w.s1, "s2": w.s2, "s3_each": w.s3_each, "s4": w.s4 },
        "mean": {
            "precision_at_k": report.mean_precision_at_k,
            "recall_at_k": report.mean_recall_at_k,
            "mrr": report.mean_mrr,
            "ndcg_at_k": report.mean_ndcg_at_k,
        },
        "per_query": per_query,
    });
    // Pretty so a human can read it AND jq can diff it.
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}

/// Byte-safe truncation to a column width (keeps multi-byte chars whole).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    // Reserve one char for the ellipsis marker; ASCII "..." would overflow
    // the budget, so use a single-dot run that fits the column.
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('~');
    out
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
            eprintln!("pp-retrieval-eval: {msg}");
            ExitCode::FAILURE
        }
    }
}
