# Real-library soak harness

Status: active founder-machine verification harness, 2026-07-27.

`scripts/real-library-soak.mjs` turns an immutable real-photo source into a
repeatable performance receipt without ever pointing writable PhotoProof state
at that source.

## Safety contract

- The source is read and copied only. The harness never creates a marker,
  sidecar, database, cache, receipt, or temporary file there.
- `/homenas` is an immutable namespace. Destination and receipt paths inside it
  are rejected before inventory starts.
- The runner receives only the separate staging directory, never the source.
- Every source gets a stable top-level namespace in the stage (`raw/...`,
  `photos/...`), so equal relative paths from different roots never collide.
- The staging and receipt paths must be explicit, outside the source, outside
  one another, and outside the repository root.
- A new stage must be empty. `--reuse-stage` accepts it only when its versioned
  manifest matches the resolved source and deterministic selection.
- Source `(relative path, size, mtime_ns)` is fingerprinted before and after.
  Up to eight selected source files are also hashed against their staged copies.
  A mismatch fails the run. This is practical tamper detection, not proof
  against a same-size/same-mtime mutation outside the hashed sample.
- Linux receipts resolve the containing mount and record filesystem, source,
  mount options, and read-only truth. The sandboxed dry run reported `/homenas`
  as NFS4 `ro,noatime`, while the approved host-side copy run truthfully
  reported `rw,noatime`. The harness therefore never relies on mount flags for
  safety: it rejects `/homenas` as a destination/receipt namespace, opens
  source files only for reading, fingerprints metadata before and after, and
  hashes sampled source/stage pairs. Unavailable mount introspection stays
  explicit on other hosts.

The allowlist and exclusions mirror `library/exclusions.rs`: JPEG, PNG, TIFF,
WebP, HEIC/HEIF, and the shipped RAW extensions; hidden/tool/cache directories,
PhotoProof sidecars, unsupported media, symlinks, and files over 2 GiB are
skipped.

## Start small

Inventory 100 supported entries and select ten without copying or running
PhotoProof:

```text
node scripts/real-library-soak.mjs \
  --source /homenas/iris_images/RAW \
  --receipts /tmp/photoproof-soak-dry-receipts \
  --tier dry \
  --run-id founder-dry-01
```

Copy and ingest a bounded 25-file sample:

```text
node scripts/real-library-soak.mjs \
  --source smoke=/homenas/photoproof_test_set \
  --destination /tmp/photoproof-soak-small-media \
  --receipts /tmp/photoproof-soak-small-receipts \
  --tier small \
  --run-id founder-small-01
```

`/homenas/photoproof_test_set` contains 101 supported JPEGs (about 1.335 GB;
its PSD is excluded), making it the preferred repeatable smoke source.

The small tier examines at most 2,000 supported files before making its
deterministic selection. Its receipt therefore says
`inventory.complete=false`; it is a safety/compatibility check, not a complete
source census.

First harness acceptance receipt on this machine used the same command shape
with `--limit 2 --inventory-cap 20`: two 127 MB ARW files (254,808,064 selected
bytes), one clean loop in 868 ms, 2.3 files/s, zero queue errors, and 875.9 MB
peak process RSS. Both source metadata and two source↔stage hashes matched.
NVIDIA and provider truth were honestly unavailable in the sandbox. This proves
the harness path, not a two-file performance budget.

The first corrected 2,000-RAW receipt is
`/mnt/storage/photoproof-soak/receipts/nvme-raw-2k-raf-fix-20260727.json`.
It settled 3,990 jobs with zero queue errors in 160.8 seconds (12.4 files/s)
at 2.46 GiB peak process RSS. The source metadata fingerprint and eight
sampled source/stage hashes remained equal. Eight `.RAF` entries in this
selection are not valid Fujifilm RAW containers: their staged bytes match the
source, they lack the `FUJIFILMCCD-RAW` signature, and external format probes
also reject them. PhotoProof now keeps those files indexed and settles preview
work once as unsupported/invalid rather than retrying or reporting a queue
error. The runtime is slower than the earlier 117-125 second diagnostic
receipts, so this is correctness evidence, not evidence of a throughput win.

## Full tiers

`standard` inventories every provided source, selects 5,000 files, and performs
three clean headless ingests. `soak` selects 50,000 and also runs three loops.
The current NAS inventory provides 28,241 RAW files under
`/homenas/iris_images/RAW` (about 1.692 TB) and enough JPEGs under
`/homenas/Photos` to fill the remainder. `--include-all-raw` puts every
inventoried RAW first, then deterministically selects 21,759 rendered files:

The immediately writable local tier is a bounded RAW-only run on the Samsung
9100 PRO. It has about 450 GB free; this command selects 2,000 RAWs, hard-caps
the copied selection at 300 GiB, and refuses to begin copying unless at least
100 GiB remains:

```text
node scripts/real-library-soak.mjs \
  --source raw=/homenas/iris_images/RAW \
  --destination /mnt/storage/photoproof-soak/stages/raw-2k \
  --receipts /mnt/storage/photoproof-soak/receipts \
  --tier standard \
  --limit 2000 \
  --include-all-raw \
  --max-selected-gib 300 \
  --reserve-gib 100 \
  --run-id nvme-raw-2k-build-abc123
```

The full 50k preparation remains targeted at `/mnt/scratch` once its ownership
is available:

```text
node scripts/real-library-soak.mjs \
  --source raw=/homenas/iris_images/RAW \
  --source photos=/homenas/Photos \
  --destination /mnt/scratch/photoproof-soak/stages/nvme-50k-build-abc123 \
  --receipts /mnt/bulk/photoproof-soak/receipts \
  --tier soak \
  --include-all-raw \
  --reserve-gib 1024 \
  --run-id nvme-50k-build-abc123
```

`/mnt/scratch` and `/mnt/bulk` are separate empty 4 TB Samsung 990 PRO ext4
volumes with about 3.4 TB free each. Before copying, the harness reads the
destination filesystem's available blocks and requires
`selected_bytes + reserve_bytes`; the soak-tier default reserve is 1 TiB.
Small and standard defaults are 5 GiB and 100 GiB. The failed preflight writes
no media and remains visible in the JSON/CSV ledger.

The harness also applies a pre-copy selection ceiling: 50 GiB for small,
300 GiB for standard, and 2.2 TiB for soak. Override it explicitly with
`--max-selected-gib`; it is independent of the free-space reserve.

Each loop gives
`pp_bench` a fresh disposable database and cache while reusing the byte-identical
stage. Use `--reuse-stage` on later runs with the same source, seed, limit, and
inventory result. `--limit`, `--inventory-cap`, `--loops`, and `--seed` provide
explicit overrides. Repeated `--source [NAME=]DIR` arguments are supported;
explicit names are recommended for long-lived receipts.

`--prepare-only` inventories, copies, and validates without starting a runner.
`--bench-bin /absolute/path/to/pp_bench` skips the release build.

## Installed-compatible runner

The default runner is headless `pp_bench ingest --source <stage>`. An installed
or instrumented build can use the same stage and receipt ledger through a
shell-free JSON argv template:

```text
node scripts/real-library-soak.mjs \
  --source raw=/homenas/iris_images/RAW \
  --source photos=/homenas/Photos \
  --destination /mnt/scratch/photoproof-soak/stages/installed-standard \
  --receipts /mnt/bulk/photoproof-soak/receipts \
  --tier standard \
  --runner-command-json \
  '["/opt/PhotoProof/photoproof","--library-soak","{stage}","--receipt","{runnerReceipt}"]'
```

Available substitutions are `{stage}`, `{runnerReceipt}`, `{runId}`, and
`{loop}`. The command is spawned directly, never through a shell. A runner may
write its own JSON receipt at `{runnerReceipt}`; the harness nests it in the
top-level run receipt. If it reports a `provider` field, that becomes provider
truth. Absence remains explicitly `unavailable`.

The production app does not yet expose `--library-soak`; this adapter is ready
for an installed/instrumented executable without granting it access to the NAS
source. Until that entry point exists, installed scrolling/Look/annotation
remains a manual action over the prepared stage.

## Receipts and spreadsheet tracking

The receipt directory contains:

| File | Behavior |
|---|---|
| `<run-id>.json` | Atomically updated current/final versioned receipt |
| `soak-progress.v1.jsonl` | Append-only phase events: inventory, copy, loop, validation, completion/failure |
| `soak-runs.v1.jsonl` | One append-only final receipt per invocation |
| `soak-progress.v2.csv` | One spreadsheet row per `run_id`; updated in place as phases advance |
| `<run-id>.loop-N.bench.jsonl` | Native `pp_bench` receipt for each loop |

Reusing a `run_id` updates its CSV row but appends a new final JSONL record.
CSV columns include source/stage, tier, inventory completeness, selected
files/bytes and RAW/rendered mix, loop count, mean ingest time and rate, peak
process RSS, NVIDIA status/device/peak device memory, provider truth, source
validation, queue-error total, result, and retained error. A runner process
exiting successfully is not enough: any nonzero `pp_bench.queue_errors` makes
the overall receipt failed after all requested loops finish. Error groups retain
only fixed categories plus normalized format/subtype tokens—never paths,
hashes, or decoder messages.

Process RSS is sampled from Linux `/proc/<pid>/status` (`VmRSS`/`VmHWM`) or
`ps rss` elsewhere. NVIDIA sampling uses `nvidia-smi` when the driver and
device are accessible. The value is device-total memory and may include other
processes; the receipt labels that scope. Missing `nvidia-smi`, inaccessible
device nodes, and runners that do not initialize an ML provider are recorded
as unavailable, never inferred as CPU success.

Preview construction currently extracts an embedded RAW preview, decodes JPEG,
resizes with SIMD CPU code, and encodes WebP on the CPU. Device-total NVIDIA
memory in a headless receipt is therefore not proof that preview work used the
GPU. A GPU preview path is justified only by an end-to-end corpus receipt that
beats this pipeline after transfer, synchronization, fallback, and codec costs;
the GPU remains directly useful for the application's embedding workloads.

## What still needs human interaction

Headless ingest receipts cover scan/hash/metadata/preview work, repeatability,
RSS, and catalog integrity. They do not exercise webview paint or human
interaction. Complete release evidence still requires opening the prepared
50k stage in an installed build and repeatedly:

- fling-scroll while ingest remains active;
- open Look, navigate rapidly, and zoom to 1:1;
- annotate and change folders without viewport jumps;
- pause/resume, sleep/wake, and relaunch;
- record installed journey percentiles, stale/superseded preview requests,
  final settle, idle/peak RSS and VRAM, provider/fallback truth, and errors.

Run that pass once from local NVMe and once from the intended network/removable
storage class. Do not substitute the copied-source headless number for either
installed receipt.
