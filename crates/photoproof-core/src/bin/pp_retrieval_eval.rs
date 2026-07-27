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
//!   pp-retrieval-eval --synthetic [--k N] [--json] [--s1 W] [--s2 W] ...
//!
//!   Paths may also come from the environment (args win):
//!     PP_RETRIEVAL_DB        the library SQLite database
//!     PP_RETRIEVAL_QUERYSET  the golden query-set JSON
//!
//! SYNTHETIC MODE (`--synthetic`): builds a tiny deterministic in-process
//! corpus (three seeded JPEGs, three distinctive remarks, three single-answer
//! golden queries) in a tempdir, runs the SAME `evaluate` loop, and emits the
//! SAME report shape — no real DB, no models, no setup. This is the CI-able,
//! zero-setup search signal the regression guard (`scripts/tune-check.sh`)
//! consumes: it is the runnable analogue of the `retrieval_eval_sample`
//! integration test, deterministic on any machine. `--db`/`--queries` are
//! ignored in this mode (the corpus and golden set are built in code).
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
//! NOTE on the similarity-blend beta. The §5.3 dense-signal tilt β now lives in
//! the centralized tuning config (`crate::tuning`, file-overridable). This
//! runner `init_from`s the same `tuning.toml` beside the DB, so β (and the
//! fusion-weight baseline) come from the file the app reads — set `[search].beta`
//! there to sweep it without a rebuild. The `--s1..--s4` flags then sweep the
//! four fusion WEIGHTS on top of that baseline; the weight sweep already answers
//! the B69 "how much should S4 vote" question the gate exists for.
//!
//! NOTE on signals. Pass `--models-dir <app-data/models>` to run the FULL
//! model-backed rig: the runner loads the SAME text + CLIP embedders the
//! desktop app loads (via `photoproof_connectors::model_specs`) and opens the
//! on-disk `.ppvec` store, so the QUERY is embedded into the annotation space
//! (S1) and the CLIP space (S4, B69 always-votes) and ranked against the stored
//! vectors — the four-signal fusion the app ships. WITHOUT `--models-dir` (no
//! models present) the runner falls back to the keyword-only rig: every vector
//! signal is dark and the FusionWeights shape only S2 + the SQL-only S3
//! summaries_fts sub-list. The graceful fallback keeps the no-models CI path
//! (the synthetic fixture, `retrieval_eval_sample`) green headlessly. The parse
//! LLM seam stays `None` either way (NL parse is out of this runner's scope).

use std::path::PathBuf;
use std::process::ExitCode;

use photoproof_core::retrieval_eval::{EvalConfig, EvalReport, EvalRig, QuerySet, evaluate};
use photoproof_core::search::{FusionWeights, Searcher};

/// Built-in @k default when neither the CLI nor the query set pins one.
const DEFAULT_K: usize = 10;

struct Args {
    /// In `--synthetic` mode these two paths are unused (the corpus + golden
    /// set are built in code); `Option` lets the parser leave them empty.
    db: Option<PathBuf>,
    queries: Option<PathBuf>,
    /// Build a deterministic in-process corpus instead of opening a real DB —
    /// the zero-setup, CI-able signal the regression guard runs.
    synthetic: bool,
    k: Option<usize>,
    json: bool,
    weights: WeightOverrides,
    /// `--models-dir <path>`: the app-data `models/` directory. When present
    /// AND the library has stored vectors, the run uses the FULL model-backed
    /// rig (the query is embedded into the annotation + CLIP spaces, S1/S4
    /// vote). Absent/unset, the run is the keyword-only rig (no models) — the
    /// graceful fallback the CI/no-models path needs. Defaults to the DB's
    /// sibling `models/` if that directory exists, else stays keyword-only.
    models_dir: Option<PathBuf>,
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
     [--models-dir <dir>] [--k N] [--json] [--s1 W] [--s2 W] [--s3 W] [--s4 W]\n\
     or:    pp-retrieval-eval --synthetic [--k N] [--json] [--s1 W] ...\n\
     (paths also from env PP_RETRIEVAL_DB / PP_RETRIEVAL_QUERYSET / PP_RETRIEVAL_MODELS_DIR; args win)\n\
     --models-dir lights up the FULL model-backed rig (S1/S4 vote); absent it is keyword-only"
}

fn parse_args() -> Result<Args, String> {
    // env supplies defaults; explicit flags override (args win — the usual
    // precedence so a one-off run can point elsewhere without unsetting env).
    let mut db = std::env::var_os("PP_RETRIEVAL_DB").map(PathBuf::from);
    let mut queries = std::env::var_os("PP_RETRIEVAL_QUERYSET").map(PathBuf::from);
    let mut synthetic = false;
    let mut k: Option<usize> = None;
    let mut json = false;
    let mut weights = WeightOverrides::default();
    // Env default for the models dir (args win), mirroring --db/--queries.
    let mut models_dir = std::env::var_os("PP_RETRIEVAL_MODELS_DIR").map(PathBuf::from);

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
            "--synthetic" => synthetic = true,
            "--db" => db = Some(PathBuf::from(val("--db")?)),
            "--queries" => queries = Some(PathBuf::from(val("--queries")?)),
            "--models-dir" => models_dir = Some(PathBuf::from(val("--models-dir")?)),
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

    // Real mode needs a DB + query set; synthetic mode builds both in code, so
    // the paths stay optional and unvalidated there (any supplied are ignored).
    if !synthetic {
        if db.is_none() {
            return Err(format!("missing --db (or PP_RETRIEVAL_DB)\n{}", usage()));
        }
        if queries.is_none() {
            return Err(format!(
                "missing --queries (or PP_RETRIEVAL_QUERYSET)\n{}",
                usage()
            ));
        }
    }
    Ok(Args {
        db,
        queries,
        synthetic,
        k,
        json,
        weights,
        models_dir,
    })
}

fn run(args: &Args) -> Result<(), String> {
    if args.synthetic {
        return run_synthetic(args);
    }

    // SAFETY: parse_args guarantees both are Some in non-synthetic mode.
    let db = args.db.as_ref().expect("db required in real mode");
    let queries = args
        .queries
        .as_ref()
        .expect("queries required in real mode");

    let raw = std::fs::read_to_string(queries)
        .map_err(|e| format!("reading query set {}: {e}", queries.display()))?;
    let query_set: QuerySet = serde_json::from_str(&raw)
        .map_err(|e| format!("parsing query set {}: {e}", queries.display()))?;

    // Load the SAME centralized tuning config the app uses, so a sweep tunes
    // the REAL config and a winning `tuning.toml` is what ships. The live file
    // sits beside the journal DB in app-data; `--s1..--s4` flags then compose
    // ON TOP of the file's `[search].fusion` as the baseline (WeightOverrides::
    // apply folds onto `FusionWeights::default()`, which reads this config).
    if let Some(app_data) = db.parent() {
        photoproof_core::tuning::init_from(app_data);
    }

    // k precedence: --k > query-set default_k > built-in DEFAULT_K.
    let k = args.k.or(query_set.default_k).unwrap_or(DEFAULT_K);
    let weights = args.weights.apply();
    // The `--s1..--s4` flags sweep the four fusion WEIGHTS on top of the file
    // baseline (`apply` folds onto `FusionWeights::default()`, which reads the
    // tuning config `init_from` just installed); `rrf_k`/β come from that same
    // baseline via `EvalConfig::default()`.
    let config = EvalConfig {
        weights,
        ..EvalConfig::default()
    };

    let searcher =
        Searcher::open(db).map_err(|e| format!("opening library DB {}: {e}", db.display()))?;

    // Resolve the model-backed rig: explicit --models-dir, else the DB's
    // sibling `models/` if it exists, else keyword-only. When a dir is present
    // the rig embeds the query and exercises S1/S4 (the full app fusion); when
    // absent we fall back to keyword-only, so a no-models machine still runs.
    let rig = build_eval_rig(args.models_dir.as_deref(), db)?;
    if rig.is_some() {
        eprintln!("pp-retrieval-eval: model-backed rig active (S1/S4 vote)");
    } else {
        eprintln!("pp-retrieval-eval: keyword-only rig (no models dir; S1/S4 dark)");
    }

    // Shared run->score loop (`retrieval_eval::evaluate`): the SAME entry point
    // pp-sweep drives, so the gate and the sweep can never diverge.
    let report = evaluate(&searcher, &query_set, config, k, rig.as_ref())?;
    if args.json {
        print_json(&report, &weights, db, queries);
    } else {
        print_table(&report, &weights);
    }
    Ok(())
}

/// Resolve the optional model-backed eval rig from the CLI/env models dir.
///
/// Precedence: an explicit `--models-dir` is REQUIRED to exist (a typo must
/// error, not silently degrade to keyword-only and report misleading numbers);
/// absent the flag, the DB's sibling `models/` is used IF it exists (the
/// app-data layout), else the run stays keyword-only. A present dir with no
/// stored vectors in the DB also errors (the rig has nothing to rank against),
/// surfacing the misconfiguration instead of a silent empty-signal run.
fn build_eval_rig(
    models_dir: Option<&std::path::Path>,
    db: &std::path::Path,
) -> Result<Option<EvalRig>, String> {
    let dir = match models_dir {
        Some(explicit) => {
            if !explicit.is_dir() {
                return Err(format!(
                    "--models-dir {} is not a directory",
                    explicit.display()
                ));
            }
            explicit.to_path_buf()
        }
        None => match db.parent().map(|p| p.join("models")) {
            Some(sibling) if sibling.is_dir() => sibling,
            // No models dir anywhere -> the graceful keyword-only fallback.
            _ => return Ok(None),
        },
    };
    // `from_db` resolves the text/clip model ids off the live `vectors` table
    // (read-only) and defaults the `vectors/` flat-file dir to the DB's sibling.
    Ok(Some(EvalRig::from_db(&dir, db)?))
}

/// `--synthetic`: build a deterministic in-process corpus + golden set, run the
/// SAME `evaluate` loop, emit the SAME report. Zero setup, no models, no real
/// DB — the CI-able search signal the regression guard consumes. This is the
/// runnable analogue of the `retrieval_eval_sample` integration test: a tiny
/// SQLite corpus (three seeded JPEGs), three distinctive remarks, three
/// single-answer golden queries. On a healthy keyword rig every answer ranks
/// first, so the mean metrics are 1.0 deterministically — a refactor that
/// silently breaks fusion/ranking drops them, which is exactly what the guard
/// catches.
fn run_synthetic(args: &Args) -> Result<(), String> {
    let fixture = synthetic::Fixture::build()?;
    let query_set = fixture.golden_set();
    let searcher = Searcher::open(&fixture.db).map_err(|e| format!("opening synthetic DB: {e}"))?;

    // Synthetic mode never reads a tuning.toml (zero-setup, machine-independent):
    // the `--s1..--s4` overrides fold onto the code-default FusionWeights, and
    // rrf_k/beta come from the same code defaults. So the run is fully
    // reproducible from the binary alone, which is what the guard's baseline
    // pins against.
    let k = args.k.unwrap_or(DEFAULT_K);
    let weights = args.weights.apply();
    let config = EvalConfig {
        weights,
        ..EvalConfig::default()
    };

    // Synthetic mode is keyword-only by definition (no models, no vectors): the
    // rig is always `None` here, the headless fallback path.
    let report = evaluate(&searcher, &query_set, config, k, None)?;
    let synth_path = std::path::Path::new("<synthetic>");
    if args.json {
        print_json(&report, &weights, synth_path, synth_path);
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

/// The deterministic in-process fixture for `--synthetic`. Mirrors the corpus
/// the `retrieval_eval_sample` integration test builds, but standalone in the
/// binary so the regression guard can run it with zero setup on any machine.
mod synthetic {
    use std::path::PathBuf;
    use std::sync::Arc;

    use photoproof_core::library::{
        FakeVolumeProbe, Library, LibraryOptions, PlatformIdKind, PlatformPlaceholderDetector,
        ProbedVolume, QueueOptions, RawlerExtractor, ScanOptions,
    };
    use photoproof_core::retrieval_eval::{GoldenQuery, QuerySet};
    use photoproof_core::{ContentHash, EventDraft, EventStore, RemarkSource, SessionContext};

    /// The three distinctive remarks paired with the query whose obvious top
    /// hit they should be. Each remark's keywords are non-overlapping so the
    /// keyword (S2 bm25) path retrieves exactly one image per query — a healthy
    /// rig scores MRR/nDCG 1.0, a broken one does not. WHY hard-coded here (not
    /// loaded): the guard must run with no external file; this IS the golden set.
    const SAMPLES: [(&str, &str); 3] = [
        (
            "the lighthouse at the harbor in heavy fog",
            "lighthouse harbor fog",
        ),
        (
            "a sunlit meadow full of wildflowers in spring",
            "meadow wildflowers spring",
        ),
        (
            "the snowy mountain summit above the clouds",
            "snowy mountain summit",
        ),
    ];

    pub struct Fixture {
        /// Kept alive so the tempdir is not reaped while the Searcher reads it.
        _tmp: TempDirGuard,
        pub db: PathBuf,
        /// Hash-sorted image hashes (the stable id space, matching the real
        /// library and the sample test).
        hashes: Vec<ContentHash>,
    }

    impl Fixture {
        /// Build the corpus: three seeded JPEGs scanned + drained into a fresh
        /// library, then three remarks appended targeting them. Deterministic
        /// (seeded pixels, sorted hashes) so the metrics are identical run over
        /// run on any machine.
        pub fn build() -> Result<Self, String> {
            let tmp = TempDirGuard::new()?;
            let base = tmp.path.clone();
            let corpus = base.join("photos");
            std::fs::create_dir_all(&corpus).map_err(|e| format!("mkdir corpus: {e}"))?;
            for seed in 0..3u32 {
                std::fs::write(corpus.join(format!("img{seed}.jpg")), unique_jpeg(seed))
                    .map_err(|e| format!("write fixture jpeg: {e}"))?;
            }

            // A FakeVolumeProbe hosting exactly the corpus dir — hermetic,
            // identical identity every run (the pp_bench pattern).
            let probe = FakeVolumeProbe::new();
            probe.set_mounts(vec![ProbedVolume {
                mount_point: corpus.clone(),
                platform_id: Some("pp-synthetic-eval".into()),
                platform_kind: PlatformIdKind::Heuristic,
                label: Some("pp-synthetic".into()),
                fs_type: None,
                capacity_bytes: None,
                read_only_flag: false,
                is_system_root: true,
                coarse_mtime: false,
            }]);
            let db = base.join("photoproof.db");
            let lib = Arc::new(
                Library::open_with(
                    &db,
                    base.join("previews"),
                    LibraryOptions {
                        probe: Arc::new(probe),
                        placeholders: Arc::new(PlatformPlaceholderDetector),
                        extractor: Arc::new(RawlerExtractor),
                        ..Default::default()
                    },
                )
                .map_err(|e| format!("open library: {e}"))?,
            );
            let root = lib
                .register_root(&corpus, Some("synthetic"))
                .map_err(|e| format!("register root: {e}"))?;
            lib.scan_root(&root, &ScanOptions::default())
                .map_err(|e| format!("scan: {e}"))?;
            lib.process_queue(&QueueOptions::default())
                .map_err(|e| format!("drain: {e}"))?;

            let mut hashes = lib.image_hashes().map_err(|e| format!("hashes: {e}"))?;
            hashes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            if hashes.len() != 3 {
                return Err(format!("fixture expected 3 images, got {}", hashes.len()));
            }

            // Append one remark per image over the SAME DB (the searcher reads
            // the folded journal). Plain typed remarks: the keyword text is all
            // the S2 path needs.
            let store = EventStore::open(&db).map_err(|e| format!("open store: {e}"))?;
            let session = store
                .open_session(SessionContext {
                    app_version: "pp-retrieval-eval-synthetic".into(),
                    device_id: "00000000000000000000000000000000".into(),
                    root_context: None,
                })
                .map_err(|e| format!("open session: {e}"))?;
            for (i, (remark, _query)) in SAMPLES.iter().enumerate() {
                store
                    .append(
                        &session,
                        EventDraft::Remark {
                            source: RemarkSource::Typed,
                            text: (*remark).to_owned(),
                            targets: vec![hashes[i].clone()],
                        },
                        None,
                    )
                    .map_err(|e| format!("append remark: {e}"))?;
            }

            Ok(Self {
                _tmp: tmp,
                db,
                hashes,
            })
        }

        /// The golden set: each query's single relevant id is the image its
        /// remark targets — exactly the contract a real query-set file uses.
        pub fn golden_set(&self) -> QuerySet {
            let queries = SAMPLES
                .iter()
                .enumerate()
                .map(|(i, (_remark, query))| GoldenQuery {
                    query: (*query).to_owned(),
                    relevant: vec![self.hashes[i].as_str().to_owned()],
                    notes: None,
                })
                .collect();
            QuerySet {
                default_k: Some(5),
                queries,
            }
        }
    }

    /// A small decodable JPEG whose bytes are unique per seed — identical to
    /// the sample test's generator so the corpus shape matches.
    fn unique_jpeg(seed: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(16, 16);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = seed
                .wrapping_mul(2_654_435_761)
                .wrapping_add(x * 31 + y * 7);
            *p = image::Rgb([
                (v & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                ((v >> 16) & 0xff) as u8,
            ]);
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut std::io::Cursor::new(&mut out), 95)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .expect("encode fixture jpeg");
        out
    }

    /// A pid-unique tempdir, removed on drop (no tempfile dep in the bin path,
    /// matching pp_bench). A crash leaves only litter the OS tempdir reaps.
    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new() -> Result<Self, String> {
            let path = std::env::temp_dir().join(format!(
                "pp-retrieval-eval-synth-{}-{}",
                std::process::id(),
                // A nanosecond salt so repeated invocations in one process (the
                // unit test) never collide on the same path.
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).map_err(|e| format!("create tempdir: {e}"))?;
            Ok(Self { path })
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
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
