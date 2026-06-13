# DESIGN: Automated tuning loop

Status: accepted (founder, June 13 2026). First instance: `pp-sweep` for search.

## Why

Every primitive for tuning already exists: three JSON-emitting benches
(`pp-retrieval-eval` search IR metrics, `pp-voice-bench` WER with gated-vs-raw
split, `pp-bench` ingest), frozen corpora (`test-corpora/{retrieval,voice,
voice-long}`), the validated file-overridable `tuning.toml`, and fixed contracts
held as code consts (the <100ms search budget, the 0.02 geometry tolerance). What
is missing is ORCHESTRATION: nothing sweeps configs, ranks them, proposes a winner,
or guards against regressions. This doc is that layer.

## The loop

`sweep -> score -> rank -> propose -> (founder commits) -> guard`

Two halves over the same primitives:

- **Research (offense)** - find better configs. `pp-sweep <subsystem>` runs the
  subsystem's bench once per config over a knob grid, aggregates the JSON into a
  leaderboard ranked by the subsystem's primary metric subject to its contract,
  and writes a PROPOSED config + a human-readable delta. The founder commits the
  winner (K14: the machine proposes, the human commits - never auto-writes
  `tuning.toml`).
- **Testing (defense)** - protect quality as code changes. A regression guard runs
  the benches at the COMMITTED config against the frozen corpora and flags a metric
  drop past a threshold vs a stored baseline. Split by cost: the cheap
  synthetic/keyword-only forms (the `retrieval_eval_sample` test, voice synth) can
  run in CI on any machine; the full real-model sweeps run periodically on the
  founder's machine (the GPU is needed there).

A later **model-survey** layer swaps a candidate behind a trait seam
(`Transcriber`/`Embedder`/`LanguageModel`/`Reranker`/`VAD`), runs the corpus bench,
and proposes a `docs/MODELS.md` update with the metric delta.

## Contracts vs dials (unchanged invariant)
The loop tunes DIALS (`tuning.toml`: search fusion weights/`rrf_k`/`beta`, graph
forces, heatmap weights, and - once lifted - the `[voice]` endpoint rules). It
NEVER tunes CONTRACTS (the <100ms budget, the 0.02 tolerance) - those stay fixed
consts. A swept winner that violated a contract is rejected, not applied.

## pp-sweep contract (the driver)
- Invocation: `pp-sweep search --db <path> --queries <golden.json>
  --grid "s4=0.5,0.75,1.0,1.25;beta=0.3,0.5" [--metric ndcg_at_k] [--k 10]
  [--max-latency-ms N] [--json] [--propose <file>]`.
- `--grid` parses `knob=v,v,...;knob=v,...` into the cartesian product of configs.
  Each config is a `tuning.toml` `[search]` override (s1/s2/s3/s4/rrf_k/beta),
  validated against the tuning bounds.
- Runs the EXISTING retrieval eval in-process per config (reuse the
  `retrieval_eval` library entry the `pp-retrieval-eval` bin already calls; refactor
  a shared `evaluate(db, queryset, weights, k)` if the bin does it inline). The
  current committed/spec config is always included as the BASELINE row.
- Output: a leaderboard sorted by the primary metric (default `ndcg_at_k`),
  showing every metric (P@k, recall@k, MRR, nDCG@k) + per-config wall-time, with the
  baseline marked. `--json` emits the full results array (stable sort for diffing).
- `--propose <file>` writes a `tuning.proposed.toml` (the winning `[search]` block,
  header-commented "PROPOSED by pp-sweep - review and copy into tuning.toml to
  apply") plus the delta (baseline metrics -> winner metrics, and the knob changes).
  It NEVER writes `tuning.toml` itself.
- `--max-latency-ms` excludes configs whose measured p-latency exceeds it. For a
  pure weight sweep this is a near-no-op (weights do not materially change latency;
  the vector searches dominate and run regardless) - it exists for future sweep
  dimensions (candidate-pool size, a reranker) that DO move latency.

## Per-subsystem metric + constraint (as instances land)
- **search** (first): maximize `ndcg_at_k` (P@k/recall@k/MRR reported) subject to
  the latency contract. Knobs: fusion s1..s4, `rrf_k`, `beta`.
- **voice** (next): minimize gating-cost (gated WER - raw WER) and segmentation
  error vs `expected`, subject to the snappiness feel. Requires lifting the
  endpoint rules (`rule1/2/3`), VAD hysteresis, and pre-roll into a `[voice]`
  tuning section so a swept winner is a file the app reads (today they are
  `pp-asr-server`/code consts).
- **ingest** (guard): throughput baseline from `pp-bench`; regression guard only.

## Build sequence
1. `pp-sweep search` + the shared `evaluate` refactor (this packet).
2. The regression guard (baselines + a `make tune-check` / periodic job; cheap
   synthetic in CI, full local periodically).
3. Lift the voice knobs into `[voice]`, then `pp-sweep voice`.
4. The model-survey layer.
