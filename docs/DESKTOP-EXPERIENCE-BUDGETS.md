# Desktop experience budgets

Status: active executable gate set, 2026-07-27. This file is the A26/T2/T4
contract. A number is a release claim only when the named receipt exists for
that platform/fixture; source constants alone are not measurements.

## Hard gates

| Experience | Budget | Executable evidence |
|---|---:|---|
| Installed fresh-data initialization to `Usable` | <= 5,000 ms | `smoke-installed-bundle.mjs`; `installed-smoke.json.initToUsableMs` |
| Installed clean shutdown | <= 5,000 ms | `smoke-installed-bundle.mjs`; `installed-smoke.json.shutdownMs` |
| Managed task acknowledgement during quit | <= 3,000 ms | `state.rs`; managed-task shutdown/phase tests |
| Hardware capability probe | <= 20,000 ms | helper-process timeout/kill/reap tests |
| Grid folder-list p99, 24-image deterministic corpus | <= 25 ms | `pp_bench grid-list`; `scripts/tune-check.sh` |
| Grid folder-list p99, 20k catalog rows | <= 75 ms | `scripts/scale-check.sh` CI tier |
| Grid folder-list p99, 100k catalog rows | <= 350 ms | `scripts/scale-check.sh --tier founder` |
| Activity publication racing folder list, 20k images / 100k pass rows | max(counter p99, list p99) <= 250 ms | `pp_bench activity-contention`; CI tier |
| Activity publication racing folder list, 100k images / 500k pass rows | max(counter p99, list p99) <= 900 ms | `pp_bench activity-contention`; founder tier |
| Preview generation p99, 24-image deterministic corpus | <= 100 ms/image | `pp_bench preview-generate`; cumulative stage histogram |
| Warm thumbnail-file serve p99, 24-image deterministic corpus | <= 5 ms | `pp_bench preview-serve`; `scripts/tune-check.sh` |
| Warm installed protocol thumbnail p99 | <= 2 ms | release-only ignored `preview_serve_latency`; installed receipt pending |
| Ingest progress publication while changing | <= 400 ms | fake-clock `progress_cadence_is_immediate_then_coalesced_at_400ms` |
| Grid fling request growth, 50k items / 60 frames | <= frames x pool; < 25k total | `fling-load-budget.test.ts` |
| Full-snapshot transforms, 20k / 100k items | p99 <= 100 / 500 ms | `catalog-snapshot-scale.test.ts`; ordinary Vitest |
| Full-snapshot transforms, 250k items | p99 <= 1,500 ms | founder-tier Vitest |
| Diversify fold p99, 20k items | <= 25 ms | `journey-performance.test.ts`; committed baseline |
| Closed-form graph layout p99, 20k nodes / 8 topics | <= 250 ms | `journey-performance.test.ts`; committed baseline |
| Lexical search-as-you-type | < 100 ms | core search latency/plan suites; installed journey monitor |
| Full-decode cache | <= configured budget after maintenance | preview eviction tests; default 20 GiB |

The 5-second installed budgets are intentionally broad portability ceilings,
not targets. The settled Linux x86_64 DEB and AppImage receipts each measured
7 ms to `Usable` and 0 ms for clean shutdown. Checked-in release workflows are
configured to run the same extracted-package receipt on Linux, macOS, and
Windows, but those remote runs are not evidence until they execute. A workspace
binary or archive-presence check is not accepted as installed evidence.

The core ingest monitor is always on and uses fixed lock-free logarithmic
histograms for queue claim, EXIF, preview total, decode, RAW extraction,
resize, encode, artifact write, and database record phases. Its p50/p95/p99
values are conservative bucket upper bounds; this gives preview generation a
release-safe live baseline instead of retaining unbounded per-photo samples.

## Catalog scale tiers

The scale harness separates catalog mechanics from media throughput:

- `--catalog-fixture` inserts canonical image, path, thumbnail-artifact, and
  ingest-pass rows into a disposable database. It exercises the real schema,
  triggers, `list_folder`, activity projection, and shared database lane
  without writing 100k fake JPEG payloads.
- `activity-contention` races one activity-counter publication against one
  folder listing per turn. A barrier prevents either thread from manufacturing
  lock starvation by immediately starting the next turn.
- The frontend receipt pays the current full-snapshot capture-date sort, two
  hash projections, RAW+JPEG stack construction, and unit projection. It does
  not include IPC JSON parsing, Svelte invalidation, DOM, image decode, GPU
  upload, or paint.

The ceilings are regression guardrails derived from the July 27 local audit,
not aspirational frame budgets. Before the incremental-catalog work, direct
local measurements were approximately 34.5 ms p99 for a 20k catalog listing
and 302 ms p99 for a 100k generated-catalog listing; the exact 500k-pass
activity query took about 418 ms. Pure frontend transforms measured
approximately 12 ms at 20k, 83 ms at 100k, and 240 ms at 250k. The committed
ceilings leave portability/JIT/load slack while still rejecting a new
quadratic path. Tighten them only from repeated clean receipts on every
supported runner.

Final combined-tree receipts on the July 27 Linux x86_64 founder machine:

| Tier | Grid-list p99 | Counter/list contention p99 | Projection rows verified |
|---|---:|---:|---:|
| CI, 20k images / 100k pass rows | 32.45 ms | 34.39 ms | 100,000 / 100,000 |
| Founder, 100k images / 500k pass rows | 222.27 ms | 318.77 ms | 500,000 / 500,000 |

The contention number includes time waiting for the same catalog lane, which is
why it is the relevant progress-publication guard rather than the counter's
isolated lookup time. The harness asserts the projection's summed counters
equal every seeded pass row before accepting a timing receipt.

These fixtures do **not** prove RAW extraction, high-resolution decode,
filesystem traversal, network-volume behavior, webview paint, memory pressure,
or GPU-provider throughput. Synthetic catalog numbers must never be reported
as photographs-per-second.

The first corrected 2,000-RAW NVMe receipt settled 3,990 jobs with zero queue
errors in 160.8 seconds (12.4 files/s), with 2.46 GiB peak RSS and an unchanged
source fingerprint. It proves invalid Fujifilm containers settle honestly; it
does not prove a speedup because earlier diagnostic receipts completed in
117-125 seconds. Preview work remains embedded-preview extraction plus CPU
JPEG decode, SIMD resize, and WebP encode. Device-total NVIDIA memory is not
preview-provider evidence, and no GPU preview claim is made.

## Resource and memory bounds

The process governor supplies deterministic concurrency/batch bounds, which
are stronger regression gates than noisy CI RSS:

| Mode | expensive lanes | ingest workers | ingest batch | embedding batch | RAW batch | decoded-frame peak proxy |
|---|---:|---:|---:|---:|---:|---:|
| Eco | 1 | 1 | 8 | 1 | 1 | 2 frames |
| Balanced | 2 | 2 | 32 | 4 | 1 | 4 frames |
| Max | 4 | 8 | 64 | 8 | 2 | 16 frames |

Interactive RAW owns a reserved foreground seat. Priority/fairness tests prove
that lower-priority model/download/maintenance work cannot consume it and that
the highest-priority waiter receives the next background seat. Pause is
observed per filesystem entry, per 64 KiB hash/download chunk, and between
queue batches.

RSS remains a machine receipt, not inferred from these bounds. Founder-machine
runs must record idle and peak RSS for each mode beside the platform receipts.

## Fixture ledger

| Fixture | Required claims | Current evidence |
|---|---|---|
| Linux local SSD, CPU-only | package startup/shutdown, grid/serve, ingest, idle | local installed receipt plus deterministic benches |
| Removable/offline drive | prompt `Usable`, retained truth, fair reconcile | synthetic volume/watcher acceptance; native receipt still required |
| Sleeping or kernel-blocked NAS | window maps while probe remains blocked | killable probe and fake blocked-lane tests; native hard-mount receipt required |
| Apple Silicon | CoreML availability/fallback, APFS semantics, package timings | `bornmanmac.local` run required |
| NVIDIA | CUDA/TensorRT availability, selected-provider proof, package timings | capability fixtures and compile gate; native profile receipt required |
| Windows | CPU/DXGI capability, `.exe` sidecar, package timings | workflow and extracted-package harness; native workflow receipt required |

## Model recovery and load behavior

- Cached capability data is display-only; execution remains Tier 0 until a
  fresh bounded probe commits.
- A missing/corrupt/stale installed-model index never hashes model payloads
  before `Usable`. Candidate models remain dark while a managed,
  cancellable post-Usable task verifies and durably adopts them.
- Native ORT construction runs in one killable helper process per embedder
  role. The 180-second watchdog, plan changes, retry, and shutdown can kill and
  reap a wedged helper; independent role lanes prevent one constructor from
  blocking the other. Model removal and approved GC cancel the role and
  synchronously reap its helper before deleting model files.

## Commands

```text
scripts/tune-check.sh
scripts/scale-check.sh
scripts/scale-check.sh --tier founder
node scripts/real-library-soak.mjs --source /homenas/iris_images/RAW --receipts /tmp/photoproof-soak-dry-receipts --tier dry
cargo test -p photoproof-desktop --release --test preview_serve_latency -- --ignored --nocapture
cd apps/desktop && bunx vitest run tests/fling-load-budget.test.ts tests/catalog-snapshot-scale.test.ts
cd apps/desktop && npm run bundle:smoke
```

The remaining real-library soak is an installed-app receipt, not a headless
substitute: ingest at least 50k mixed RAW+JPEG files from the founder's actual
storage while repeatedly fling-scrolling, opening Look, zooming to 1:1,
annotating, changing folders, pausing/resuming, and sleeping/waking the host.
Record first-card time, final settle time, counter/list/preview journey
percentiles, stale/dropped preview requests, idle and peak RSS/VRAM, database
busy time, errors, and provider/fallback truth. Repeat on local NVMe and the
intended NAS/removable-volume class. A passing catalog scale check is necessary
but cannot replace this soak.

The repeatable inventory/copy/headless portion and spreadsheet receipt format
are implemented by `scripts/real-library-soak.mjs`; its immutable-source
contract, tier commands, installed-runner adapter, and manual remainder are in
`docs/REAL-LIBRARY-SOAK.md`.

Machine receipts belong in release artifacts or `docs/releases/`; do not
replace them with hand-edited pass labels.

## Structured journey monitoring

Installed sessions append schema-versioned local records to
`<app-data>/performance/journeys.v1.jsonl`. The file rotates at 8 MiB, retains
one previous generation, and is also summarized in Application Health as
bounded p50/p95/p99/max/error series. Sink failure never breaks the measured
journey and remains visible in health.

The backend enriches every persisted record and health snapshot with the
application version, OS, architecture, and one per-process run id. These fixed,
non-user-data fields make comparisons across releases, platforms, and launches
possible without allowing frontend callers to choose high-cardinality labels.

The taxonomy is deliberately closed and low-cardinality: startup, library and
folder open, root changes, grid, previews, Look, search, graph, filters,
journal, capture, settings, backup/restore, model runtime, updates, shutdown,
and generic IPC. Phases include queue/read/write/scan, decode/resize/encode/
serve, cache lookup, invoke, layout/render/first paint/settle, download/verify/
load/reconcile, and total. Optional item and byte counts plus
`none/hit/miss/stale` cache state provide workload context without becoming
series labels.

The monitor never records paths, hashes, model ids, queries, note text,
command arguments, error messages, or other user-authored content. Frontend
samples are batched through a bounded queue; malformed labels, unknown fields,
oversized batches, unsafe numeric values, and unsupported schema versions are
rejected at both sides of IPC.

Every frontend command receives an end-to-end invoke sample. The graph adds
explicit affinity-cache, neighbor-read, closed-form layout, render,
first-paint, and settle samples; the Diversify journey adds an end-to-end
filter sample. Preview serve is recorded in the backend protocol path, while
preview generation uses the core stage histogram described above. These
installed observations complement, rather than replace, deterministic CI
ceilings in `tuning-baselines.json`.
