# LANDED — shipped from the backlog

The archive half of BACKLOG.md: every `[x]` item moves here verbatim once it
ships, commit hashes, root causes, and founder context intact — this is the
de facto changelog of backlog-sourced work. Open work stays in BACKLOG.md;
this file only grows. Organized by era, newest first; older entries keep
their original wording.

## July 27 2026 - desktop foundation observability and gates

- [x] **Retain crash diagnostics and add startup/lifecycle telemetry**
  - Rotate/preserve previous session logs instead of truncating the only crash
    record on the next launch; add a previous-launch crash marker, panic hook,
    build/runtime/capability metadata, and per-phase startup timings.
    `apps/desktop/src-tauri/src/lib.rs:53-90`.
  - Surface Reveal logs / Copy health report. Log-sink creation failure itself
    must be discoverable.
  - Landed evidence: `diagnostics.rs` rotates and bounds retained launch logs,
    durably marks unclean launches, and records panics; coordinated shutdown
    alone clears the marker. `lifecycle.rs` records monotone phase timings.
    Application Health joins build/runtime/capability and diagnostics metadata,
    promotes log-sink failure to a health issue, and Settings exposes Reveal
    logs plus Copy health report. Relaunch/rotation, phase, health projection,
    and Settings-window tests pin the contract.

- [x] **Add multi-platform CI and dependency/release workflow contracts**
  - Run format, clippy, Rust tests, frontend typecheck/tests/build, no-em-dash
    UI check, migration fixtures, bundle construction, and installed smoke tests
    on Linux/macOS/Windows. Compile platform-specific code paths even when live
    hardware tests remain founder-machine gates.
  - The APFS `s02_2` case-only rename is now a required macOS receipt, not a
    tolerated red; no red is tolerated.
  - Landed source: `.github/workflows/desktop-foundation.yml` defines
    format/clippy/workspace tests and the full frontend check/test/build matrix
    on Linux, macOS, and Windows, with APFS proof mandatory on macOS. Its native
    matrix builds and smokes deb+AppImage/app+dmg/msi+nsis packages. RustSec and
    Bun audits are required, while Dependabot tracks Cargo, npm, and Actions
    weekly. Local YAML and contract validation are green; A24 remains open in
    BACKLOG until the workflows have remote receipts.

## July 7 2026 - Full audit fix wave (loops, sync, model downloads, UX, perf)

Full-codebase audit (`docs/AUDIT-2026-07-07.md`, findings keyed G/S/D/U/F/T)
of the recently-landed features and the standing background loops, then a
wave-1 fix pass. Five parallel deep-read audits over `b9ff46f`; every fix
below carries a named test.

- [x] **G1 - gate was red on clean main beyond `s02_2`** - `config_toml.rs`'s
  `spec_block_equals_defaults` compared the RUNTIME sec 4.4 spec literal against
  `Config::default()` verbatim, but the CLIP default had become
  platform-conditional (fp16 single-file on CUDA builds, int8 base on
  macOS/CPU because the fp16 graph fragments into ~36 CoreML partitions). The
  test now pins the embedder model per-platform and compares the rest exactly;
  the spec block comment states the conditional compiled default. `c2617cb`,
  `0d84280`.

- [x] **S1/S2 - the grid showed stale/ghost assets for up to 10 min after
  launch and after sleep** - nothing reconciled the filesystem at startup (the
  first scan was the +10 min maintenance tick, and the notify watcher does not
  replay pre-launch events), and `on_system_resume` existed but had zero
  callers so sleep/wake missed events silently. The startup doctor now runs a
  background `reconcile_all` as its final ordered step (after the vector/pass/
  preview heals, using the add-root discovered-counter seam so the walk is
  visible in the header); the pump tracks wall-clock gaps between iterations
  (`SystemTime`, since `Instant` pauses during suspend) and a >120 s gap spawns
  a latched background resume reconcile. `wall_gap_signals_resume` extracted as
  a pure fn with boundary + backwards-clock tests. `dc65cc8`.

- [x] **S3/S4 - in-place edits, live moves, and live deletes never notified the
  grid** - `images_version` bumped only in `new_image_tx`, so supersede,
  relink, `observe_removed`, the rename-relink watcher branch, and the scan
  went-stale sweep all changed grid membership without advancing the seam-1
  handshake. All routes now go through one post-commit `bump_images_version`
  chokepoint; idle reconciles deliberately do not bump (pinned by
  `reconcile_scan_bumps_images_version_only_on_change`, no 10-min re-list
  churn). `dc65cc8`.

- [x] **U1 - Diversify silently hid newly-ingested photos** - mid-ingest
  re-lists called `grid.setItems` without re-running the diversify pass, and
  the filter dropped any hash the stale pass had never seen, so import-and-
  review with Diversify on hid every new photo with no hint. Re-lists now
  funnel through a shared `refreshLensesIfScopeChanged` (heat + diversify +
  dupes), and the fold was inverted to key on the report's `hidden` set so
  unknown-hash -> visible is the structural default; `diversifyHidden` is now a
  live getter. Also fixed `selectRedundantDuplicates` mis-focus + missing
  reportScope (U2), the redundant second lens pass on toggle-on (U4), a missing
  debounce cancel (U5), a "diversifying..." pending affordance (U6), and
  kind-blind lens cache keys (U7). `753e865`.

- [x] **F1 - the protocol handler could spawn hundreds of transient OS
  threads on a fling-scroll** - `photoproof://` spawned a fresh thread per
  request doing a blocking `fs::read`, all contending for the FS and the db
  lock. Replaced with a fixed pool sized to core count (clamped 2..8): the
  burst queues FIFO on a constant number of workers, queue as backpressure.
  Locked in by `preview_serve_latency` (T1): 200 concurrent serves 2.78 ms
  wall, warm p99 0.089 ms on M1. `0a2d957`.

- [x] **F2/F3 - grid scroll jank and bitmap bloat at scale** - `isSelected` did
  `order.includes` per mounted cell per scroll frame (Select-All on 10k then
  scrolling measured 1363 ms of string compares per sweep); now a
  WeakMap-memoized Set keyed on the order array identity (0.72 ms same
  workload). The grid also always decoded the 512 px thumb tier even into 96 px
  cells (~150 MB of decoded bitmaps zoomed out); it now picks the 96 px micro
  tier for cells <=200 px with a 404-fallback heal. Perf-budget vitests added
  (T3). `d1fc1a6`.

- [x] **D1/D2/D3/D5 - model-download operational resilience and visibility** -
  a disk-space preflight before the ~13.4 GB tier-1 bundle (statvfs, 2 GiB
  margin, distinct `InsufficientSpace` row); transient 429/5xx now retry
  through the existing backoff honoring Retry-After (only socket-cuts did
  before); a per-model cancel flag + Cancel button (parts kept for resume, no
  error row); and a live "1.2 / 3.9 GB - 8.0 MB/s" throughput line (frontend
  EWMA over the existing byte samples). `4779983`.

S5 later landed with a 30-day authoritative-orphan policy, all-vector cleanup,
retained-summary rebuild, and interruption/relink coverage. Remaining items
from this snapshot continue in BACKLOG: S6/`s02_2`, D4, D6, D7, D8, U3, U8,
F4-F7, T2, and T4 (see their current entries for live status).

## June 17 2026 - Seam 2: model-swap re-embed coverage (revive transient skips)

- [x] **`repend_passes_for_model` covers transiently-skipped + error rows, not
  just `done`** - the Seam 2 re-embed-contract gap (`STATE-MACHINE.md §5/§6c`,
  `ARCHITECTURE-CONTRACTS.md` Seam 2). A model swap used to re-pend only `done`
  rows whose `model_id` differed, silently leaving **`preview-deferred`** HEIC/RAW
  skips in the OLD vector space forever (partial signal that never heals). Now a
  second cohort revives `skipped` rows in a `TRANSIENT_SKIP_CODES` allow-list
  (currently `preview-deferred`) plus attempt-capped `error` rows - but ONLY when
  cohort 1 found a genuine swap (a `done` row whose model changed), so these
  `model_id IS NULL` rows can't churn skipped<->pending on every drain. PERMANENT
  skips (`root-removed`: no active path, nothing to embed) are correctly left
  alone; `running` + `priority` untouched. (Annotation-less text rows are `done`,
  not skipped, so already covered - the audit's framing was imprecise; documented
  in code.) Test: `model_swap_repends_transient_skip_not_permanent_skip` proves
  done+transient-skip revive, root-removed/running don't, and a second same-model
  call is a no-op (idempotent). Gate green (clippy + `cargo test -p photoproof-core`,
  modulo the known-failing `s02_2` sidecar test). REMAINING (still BACKLOG): the
  unchanged-`model_id` swap (weights replaced under the same id) re-embeds nothing.

## June 17 2026 - Seam 1 (data-version) visualizer proof + the sim-state pass

The principled packet that ended the visualizer whack-a-mole (scoped in
`docs/PLAN-SEAM1-SIMSTATE.md`; map `docs/STATE-MACHINE.md §6b`; target
`docs/ARCHITECTURE-CONTRACTS.md` Seams 1 + 3). Replaces the leaky poll mechanism
with a data-version contract and makes the sim invariants explicit + tested.
Gate green: `cargo fmt`/`clippy` clean, `svelte-check` 0/0, 1111 vitest pass
(9 new), em-dash clean. Two commits (backend, then frontend).

- [x] **Seam 1 backend: `vectorsVersion` counter on the vector store** - `32251af`.
  `PpvecStore` gained an in-memory monotonic `AtomicU64` bumped once per committed
  write in `upsert_with_meta` (the single chokepoint, all VecKinds), exposed as
  `vectors_version()`. It rides the existing `IngestStatus` on `ingest-progress`
  (camelCase -> `vectorsVersion`); the pump's `prev != status` emit-gate already
  fires on advance, so a committed write notifies views with NO new channel.
  In-memory by design (monotonic per process is all a view needs; it re-fetches on
  mount) -> no schema column, no migration.

- [x] **Seam 1 frontend: visualizer refreshes on the version, self-heal poll
  DELETED** - `b883dd3`. The visualizer holds the `vectorsVersion` it last rendered
  against and re-fetches affinities only when the live version advances while a half
  it needs is still missing (throttled to coalesce an embed burst). This RETIRES
  `retryWhenEmbeddersReady` + `READINESS_*` entirely - the 45/sec -> 1.5s-beat
  failure class (`STATE-MACHINE.md §6b`): no data advance, no work; a ready-but-
  empty scope stays calm. `embeddersLoading` is now a `$derived` off the reactive
  embedder slot state (no poll behind the banner). Rides the existing reactive
  `ui.shell.ingest` snapshot, so no new wiring. (Interim narrowings `260eeb0` /
  `9f6de6c` from June 16 are superseded by this.)

- [x] **Sim-interaction state-machine pass: re-seed ALWAYS reheats + pure,
  tested invariants** - `b883dd3`. Every node-set re-seed funnels through
  `reseedAndRestart()`, which always `reheat()`s before `restartLoop` - closing the
  open `expandSuper` click-jitter bug AND the same gap in both
  `applyLodZoomTransition` branches (re-seed at cooled heat pins motion to
  `ANNEAL_FLOOR` = jitter). The rest predicate + heat-tied clamp were extracted PURE
  into `forcegraph.ts` (`isAtRest`, `annealedMaxStep`) with named thresholds
  (`REST_ENERGY_PER_BODY` / `SETTLE_FRAMES` / `SETTLED_HEAT`, killing the inline
  `1.0`/`30`/`1.0001` magic numbers - a Seam 3 down payment), so `isSettled`
  delegates and the invariants are unit-tested (`forcegraph-restseed.test.ts`, 9
  cases): drag holds awake (`c8087d9`), scale-invariant rest, and
  cooled-pins-to-floor / reheat-frees (the WHY behind re-seed-must-reheat).

## June 16 2026 - Visualizer interaction/refresh state-machine fixes (3 of a class)

Not backlog-sourced - these surfaced live during dogfooding, all the same class:
the visualizer's interaction/refresh state machine (heat <-> settle <-> poll <->
recompute) tripping over implicit invariants. Recorded here because they are
shipped + pushed; the map is `docs/STATE-MACHINE.md §6b`, the principled fix that
ends the class is `docs/ARCHITECTURE-CONTRACTS.md` Seam 1 (data-version) + a
sim-interaction state-machine pass (now open in BACKLOG).

- [x] **Affinity recompute tight-loop (~45/sec) stopped** - `260eeb0`. A loaded
  CLIP embedder over a mid-embedding space returns `visual_ready=false` (no
  vectors yet), and the self-heal recomputed IMMEDIATELY whenever `clipReady`
  was true -> recompute -> still false -> recompute, ~45 `topic_affinities`/sec
  (founder saw it after the int8 CLIP switch re-embeds the library). Now self-heal
  distinguishes a signal still COMING from genuinely ABSENT via the per-role slot
  state and POLLS on a bounded 1.5s cadence; `recompute(selfHeal)` does not reset
  the poll budget, so a never-finishing space stops after `READINESS_MAX_TRIES`.

- [x] **Self-heal poll thrash on ready-but-empty scope stopped** - `9f6de6c`. The
  interim fix above still treated a READY embedder as "vectors coming", so a scope
  with an empty vector join (`STATE-MACHINE.md §6a` - a HEIC folder, or images not
  in the active space) beat `recompute()` on a 1.5s timer for its full 60s budget
  every visit (founder: "freaks out on a beat ... again and again"). Now polls ONLY
  while `clip/text state == "building"`; a ready-but-empty scope stops immediately.
  Interim until the library->view data-version seam retires the poll (Seam 1).

- [x] **Drag holds the sim awake + warm (no mid-drag freeze)** - `c8087d9`.
  `isSettled()` had no drag awareness: mid-drag the heat cooled to <=1.0001, the
  loop declared rest and STOPPED, so the canvas stopped repainting while the pointer
  kept writing x/y -> "click and drag -> stops updating -> freezes" (founder). Now a
  drag in progress is never "at rest" (loop keeps ticking/drawing) and `pointermove`
  reheats so neighbours follow the moved node instead of being clamped to sub-pixel.
  Standard force-graph drag behavior (d3-force holds alphaTarget>0 while dragging);
  settling resumes on release. This fix confirmed the "too-cold/clamp-frozen" reading
  of the still-open `expandSuper` jitter bug (`STATE-MACHINE.md §6b` contradiction
  callout, now resolved).

## June 16 2026 - Digest visibility (Library-status header indicator) + journal chips

- [x] **Digest visibility: "what is my library doing?"** (founder, June 12 2026) -
  `c65e467` (backend) + `1ea6ea2` (frontend). The only signal used to be the word
  "digesting" in the header. Now a **Library-status indicator** lives in the
  TITLEBAR CENTER (replacing that text + absorbing the offline-drive warning):
  COLLAPSED it reads settled (calm dot + "Library settled") / working (activity
  glyph + current stage + done/total + a thin sliver + overall "~6m") / blocked
  (amber + the top waiting-on reason); on hover/focus it drops a panel with the full
  STAGE list (discover -> hash -> meta -> preview -> embed, each label / "240 /
  5,000" / a bar / "~6m . 12/s"), a "Waiting on" section (offline volumes,
  downloading models, embedder loading), and an errors row. The bottom-right Station
  is now CAPTURE-ONLY (mic/search/pencil/scope/thumbnails kept; all digest rows,
  transients, the ingest hairline, and the missing-model prompt moved out). Backend:
  `PassRemaining` gained `done`/`total`/`ratePerSec`; the pump keeps a per-pass EMA
  (alpha 0.3) FROZEN during capture/offline (no post-pause spike), `ratePerSec`
  excluded from emit-equality with a 0.5/s quantum gate so float drift cannot spam
  the channel; ETA = remaining / ratePerSec (frontend). Old `jobs.ts`/`jobs.test.ts`
  deleted (fully replaced). Stage mapping puts any unknown pass in a trailing
  "Finishing up" stage, never dropped. Tests: rate-EMA fold + pause-freeze +
  no-spike (Rust), done/total carry (Rust), librarystatus model/stages/waitingOn/
  ETA/formatters + rewritten station + titlebar (vitest). REVIEW FLAGS (safe choices
  shipped): discover stage driven off `scanning`/`discovered` (no pass name);
  embedder-loading derived from `clipReady`/`textEmbedderReady` false while installed;
  `acceptModelLicense`/`downloadMissingModel` now unused (left for a later cleanup).

- [x] **Journal entry type/source chips** (founder, June 14 2026) - `90568d6`. Each
  journal entry shows a compact source chip in its byline so a drawing, voice note,
  typed note, and system Summary read distinctly. Pure `sourceChip(entry)` helper +
  chip in `JournalEntry.svelte`, reusing the grid filter-chip pill idiom. Research
  finding: the frontend `source` DTO is only `voice|typed|system` (no `pencil`); a
  drawing is `kind === "stroke"`, so the chip derives "Pencil" from kind (precedence
  over source). `system` -> the "Summary" tag (tinted `--station-working`), lights up
  when summary generation lands. Tests: one per variant + an em-dash guard.

## June 16 2026 - Per-model capture pause + WKWebView capability spike

- [x] **Relax the capture-pause for GPU embedders** (founder, June 14 2026: "Once we
  do have the ML model set up on GPU then we won't want to pause them while we're
  doing ASR.") - `9dbf420`. The scheduler paused ALL background model work while the
  mic was armed for ASR - correct when the embedders ran on CPU and shared silicon
  with the CPU ASR, but wrong now that the CLIP image embed runs on a GPU/ANE EP
  (CoreML on macOS, CUDA/TensorRT on the NVIDIA build) where it no longer contends.
  Now a PER-MODEL policy keyed on each embedder's EXECUTION PROVIDER, not its kind:
  `should_pause_during_capture(runs_on_accelerator) = !runs_on_accelerator` (pump.rs).
  While armed, `drain_embeddings` builds a CLIP-only rig (text=None) and runs it ONLY
  when the CLIP EP is an accelerator - and deliberately does NOT wire `capture_live`
  as the cancel (it is true for the whole armed window; the bounded `EMBED_BATCH`
  keeps the turn short). The text embed (CPU per SPIKE-COREML-TEXT.md) and the GPU
  LLM (request-driven, no background drain) stay paused. `OrtEmbedder` now records
  its effective EP via a factored `resolve_effective_accel(base)` shared with
  `build_session` (so `runs_on_accelerator()` reports exactly what loaded, env
  overrides included) and exposes it. Behavior is IDENTICAL on a pure-CPU machine
  (no GPU EP -> every embedder reports CPU -> everything still pauses). Tests:
  policy gpu-keeps-running/cpu-pauses, select_clip_accel fp16-suffix gate,
  resolve_effective_accel passthrough.

- [x] **WKWebView capability check** (spike, gates the off-main-thread perf work) -
  `9cee205`. Runtime feature-probe (`logic/webviewcaps.ts`) for OffscreenCanvas
  (2d/webgl), Web Workers, createImageBitmap, WebGL/WebGL2, and WebGPU presence +
  async adapter check, logged once at startup (`main.ts` console.info) so a live
  `tauri dev` run pins the verdicts for the host WebKit. Findings in
  `docs/SPIKE-WKWEBVIEW.md`. GO/NO-GO: Workers/WebGL2/createImageBitmap universal;
  OffscreenCanvas on Sonoma 14+ (Safari 16.4+); WebGPU flagged-off through Safari 18
  (HOLD, off critical path). => visualizer-sim-into-a-Worker GO, WebGL render GO,
  off-main-thread thumbnail decode GO-but-optional. Definitive confirmation = the
  startup `webviewcaps` console line on the founder's target macOS. 13 vitest cases.

## June 16 2026 - Ingest pass pipelining

- [x] **Ingest pass pipelining** (backend perf, no webview dep) - `d3e1f36`. The
  ingest drain was a WAVE loop: claim `pool_width` rows, run them all in parallel,
  then a BARRIER - the next wave could not be claimed until the SLOWEST item of the
  current one finished, so one big-RAW preview drained the pool to a single busy
  worker while the rest idled, and decode/encode sat idle between waves while later
  items were still being claimed/hashed. Replaced with a bounded-channel pipeline
  (`run_pipeline` in `library/mod.rs`): a CLAIMER feeds a `std::sync::mpsc::
  sync_channel` (capacity = pool width, so peak in-flight decoded frames stay at
  ~2*width = the old wave's worst case = no memory regression) that `pool_width`
  workers pull from CONTINUOUSLY, so a slow item only occupies its own worker. DB
  stays the source of truth (`claim_next` flips pending->running durably; the
  channel is in-memory scheduling only); cancel/max_items honored per item; a full
  channel blocks the claimer = backpressure. No new crate (`sync_channel` is the
  codebase's existing bounded-channel idiom). Tests: workers-run-concurrently
  (barrier-sized-to-pool deadlocks a serialized drain), every-item-exactly-once
  under backpressure (500 items), cancel-winds-down-without-stuck-running,
  claim-error-aborts-after-inflight-finish; the interrupted-ingest acceptance
  suite (cancel/no-loss/recovery, offline-skip) now exercises the pipelined path
  unchanged.

## June 16 2026 - Removed-folder reconciliation + self-heal + soft-topic v2

- [x] **Removed-folder reconciliation** (founder, June 15 2026 - confirmed bug) -
  `acc74c8`. Removing a root orphaned its images (non-destructive, for relink) but
  they (1) still showed in the LIBRARY scope and (2) kept consuming ingest work
  (founder saw ~414 ghost images re-embedding mid-session). FIX: `image_hashes()`
  (`library/mod.rs`) now filters to images with an active path; `remove_root()`
  skips pending/error ingest passes for images left with NO active path (safe
  multi-root variant); new `Library::heal_orphaned_passes()` + a startup-doctor
  step clean up images orphaned before the fix; frontend `invalidateScopedGraphs`
  is wired into `onRootsChanged` so a removed root drops its stale cached graph
  (no more view-swap workaround). Images themselves stay (relink). Tests: orphaned
  passes skipped + image dropped from `image_hashes()` (with multi-root survival),
  `invalidateScopedGraphs` prefix-collision guard. ORIGINAL: "i removed some
  folders and added some.... but images are still there from the old folders... it
  still shows indexing thousands."

- [x] **Self-heal refinements (verify-before-retire + skip-already-correct)**
  (founder, June 15 2026) - `acc74c8`. (a) VERIFY BEFORE RETIRE -
  `active_vector_models` (`runtime.rs`) now lists a model only once its embedder is
  actually LOADED (`clip_ready`/`text_ready`), so `reconcile_spaces` no longer
  retires a live space because config named an unloadable model (it once dropped
  the live dfn5b space because config said fp16 while fp16 was not loadable). (b)
  SKIP ALREADY-CORRECT - image staleness hash is now
  `image_inputs_hash(image_hash, model_id, GENERATOR_VERSION)` instead of preview
  bytes, so regenerated previews no longer trigger a full library re-embed (founder
  hit a 414-image re-embed after "rebuild all previews"); a generator-version bump
  still re-embeds. Tests: reconcile keeps superseded when active model not ready,
  preview-regen-does-not-reembed-same-image+model.

- [x] **Soft-topic v2: ghost anchors + promote-to-topic** (founder, June 15 2026) -
  `acc74c8`. Overlooked mode now renders a faint GHOST ANCHOR at each unnamed-
  cluster centroid; a click PROMOTES it to a real topic via `ui.addTopic`.
  Labeling (founder decision): NOTES FIRST via `matchClusterLabel` against
  `cluster_topics` (size-distance match with a separation guard, defaults to
  unlabeled rather than mislabeling), unlabeled dot otherwise; an unlabeled promote
  prompts for a name. Tests: `matchClusterLabel` match/too-far/near-tie/blank.
  NOTE STILL OPEN: dogfood the force balance (slider + `tuning.toml`) and tune the
  defaults - tracked in BACKLOG.

## June 14 2026 - Best-per-platform defaults + NVIDIA/MTP app staging

- [x] **Best validated ML as the committed default per platform** - `fe7c8e4`. CLIP
  default -> `...-fp16` (auto-picks CoreML on macOS 8.77x / CUDA-TensorRT on a cuda build
  62-117x / CPU); ASR default -> Nemotron 3.5 parakeet (WER 1.25% + punctuation +
  multilingual). pp-asr-server now builds `--features engine-parakeet` (tauri.conf.json),
  compiling BOTH engines and RUNTIME-DISPATCHING by model layout (`encoder.int8.onnx` =>
  sherpa, else parakeet) - the int8 English engine stays a one-line config fallback, no
  second binary. engine_parakeet descends the nested HF export to `config.json`. Manifest
  parakeet -> tier [1,2]; tier-1 sum + spec RUNTIME §4.4 + tests synced. Verified both
  engines READY via the supervisor arg shape; gate green (connectors, core 255, x2 asr).
- [x] **#53 NVIDIA app launch - staged + verified on margo**. The desktop app compiles
  `--features cuda-dynamic` (50s, clean); `ort_runtime::resolve()` stages `ORT_DYLIB_PATH`
  from `{app-data}/runtime/onnxruntime-cuda/lib` (+ `tensorrt/lib`), both symlinked on
  margo. Proven via that exact staged path: TensorRT ladder loads -> **117.43x** CLIP
  (82.3 vs 0.70 img/s, cosine 0.9999). Live GUI run on margo awaits a display (headless ssh).
- [x] **#55 MTP vendoring - staged + verified on margo**. llama-server (v9636, RUNPATH to
  its libs) symlinked onto PATH; the MTP model symlinked at `{models}/gemma-4-e2b-it-qat-q4_k_xl-mtp/`
  with the pinned basenames; `config.toml` selects it. Supervisor MTP wiring tests pass on
  Linux (`mtp_draft_for` resolves the drafter, `llama_spec` gates `--spec-type draft-mtp`);
  the binary+flags measured 1.32x earlier. Live spawn awaits the GUI run.

## June 14 2026 - Nemotron 3.5 ASR via the parakeet-rs engine

- [x] **Nemotron 3.5 ASR engine** (`engine-parakeet`) - merge `27d7a7f`, dead-code
  gate fix `498e5a1`. The 3.5 streaming model serves our exact `pp-asr-server` WS
  protocol via the published `parakeet-rs` crate (pure Rust over `ort`, CPU EP),
  bypassing the lagging k2-fsa sherpa-onnx crate (still 1.13.2, pre-3.5). `main.rs`
  split into a generic WS loop + an `Engine` trait; `engine_sherpa.rs` is the
  byte-for-byte DEFAULT, `engine_parakeet.rs` the opt-in. Needs its OWN model
  layout (a directory of `config.json` + `encoder.onnx`(+`.data`, FP32 ~2.45 GB) +
  `decoder_joint.onnx` + `tokenizer.model`), so a second staged manifest entry
  (`nemotron-3.5-asr-streaming-0.6b-parakeet`, real SHAs, `tiers: vec![]`). The
  engine owns chunking (560 ms), endpointing (ported CAPTURE §6.3 trailing-silence,
  since parakeet-rs has no rule1/2/3), and B67 (accumulates incremental text). All
  reversible: feature flag + tier flip, default build pulls no parakeet-rs.
  `docs/PLAN-NEMOTRON-35-SIDECAR.md` §10.
- [x] **§7.4 latency/RSS A/B gate - PASSED** - harness `scripts/asr-ab.sh`
  (`2762193`+`63f742e`+`ae6da08`), portable across M1 (macOS `ru_maxrss`) and margo
  (Linux `/proc` `VmHWM`); peak RSS measured via the kernel high-water mark because
  `ps rss` under-reports mmap'd FP32 weights >10x. One clean streamed Alice ch1 pass
  per engine, both machines. RESULT: 3.5-via-parakeet is REAL-TIME everywhere (M1
  4.50x / 9900X 7.75x RTF; sherpa int8 4.11x / 15.57x - so parakeet is SLOWER than
  int8 on the strong x86 CPU but never near the real-time floor), at +1.3 GB peak
  RAM (FP32 ~2.2 GB vs int8 ~0.9-1.1 GB, mic-armed only), buying WER 1.25%
  LibriSpeech with native punctuation + caps + multilingual. Default stays int8
  sherpa until the founder flips the tier. `docs/PLAN-NEMOTRON-35-SIDECAR.md` §11.
- [x] **Cross-machine CLIP GPU re-measure (5080)** - both rungs re-validated on the
  sm_120 onnxruntime (`cuda-dynamic`): CUDA EP **62.69x** (45.6 img/s; prior 54.47x)
  and TensorRT EP **112.35x** (81.7 img/s, 4900 img/min, cosine 0.999936, 0/60 below
  0.999; prior 85.79x - higher now with a warm GPU + cached TRT engine, 4.3 s load).
  TRT libs from `~/trt-venv` (`tensorrt-cu12<11`, 10.16.1, sm120 builder resource).
  `docs/RUNTIME-MATRIX.md`, `docs/PLAN-TENSORRT.md`.
- [x] **Gemma 4 MTP re-confirm (5080)** - llama.cpp v9636 (`8edaca903`), CUDA build,
  `--spec-type draft-mtp --model-draft mtp-gemma-4-E2B-it.gguf --spec-draft-n-max 4`.
  vs a no-speculation baseline on the same prompt/seed: **1.32x** (299.9 -> 395.0
  tok/s) at **35.6% draft acceptance** (149/419) on a hard, low-predictability prompt
  - acceptance (and thus speedup) is prompt-dependent, consistent with the cited
  1.3-2.98x range (the prior 51.5% was an easier prompt). MTP path works end-to-end:
  base + drafter + CUDA. `docs/PLAN-GEMMA-MTP.md`.

## June 14 2026 - Performance plan (PLAN-PERF.md execution)

- [x] **P1 preview-tier fast_image_resize** - `b1422a1`. Swapped the image-crate
  CatmullRom resize for `fast_image_resize` (NEON SIMD) in the preview tiers, same
  CatmullRom kernel (identical geometry; pixels a valid alternate resampling),
  aspect math + pixel type preserved, scalar fallback on error. MEASURED 3.66x on
  the resize step on M-series (pp-bench, 81.59 -> 22.27 ms/resize) - the real
  Apple-Silicon figure (the plan's ~7x was a Neoverse-N1 number). NOTE (corrected
  by P5): the two CatmullRom libs diverge up to ~17/255 on edges, not +-1 LSB -
  fine for display previews, but it is why the same swap was REJECTED for the
  correctness-locked CLIP preprocess (P5). RE-VERIFIED June 16 on M1 (throwaway
  6000x4000 -> 2560 micro-bench, 10 runs): image-crate CatmullRom 212.81 ms vs
  fast_image_resize CatmullRom 59.80 ms = 3.56x, matching the original 3.66x.
- [~] **P5 CLIP-preprocess resize - REJECTED (parity)** - not pursued. The
  parity-gated swap of the CLIP 378x378 resize failed: on 80 real images the
  embeddings shifted (mean cosine 0.99935, min 0.99689, 10/80 below 0.999) because
  the convolutions diverge up to 17/255 on edges. The 378x378 square is
  OpenCLIP-validated, and the win is a sliver of the inference, so it was reverted
  (no commit). clip_preprocess.rs stays on image-rs CatmullRom. See PLAN-PERF P5.
- [x] **P3 graph sim in a Web Worker** - `f2e1409`. The Visualizer force loop
  (O(N^2), `step()` is pure) runs off the UI main thread in a Worker, with a
  transferable SoA buffer (positions ping-pong, static inputs sent on-change),
  pipelined draw(N-1) while computing N, and a full inline fallback for older
  webviews. A bit-identical parity test pins the worker against the inline path;
  all graph features preserved (drag, idle-when-stable, persist/restore, LOD,
  selection, bake). WHY a Worker not a Rust IPC: the render target is in the
  webview and transferable buffers are near-zero-copy, vs per-frame Tauri IPC.
- [x] **P2 CoreML EP spike (verdict: DON'T-SHIP / needs-FP16)** - `a88e91f`; see
  docs/SPIKE-COREML.md. Wired ONNX Runtime's CoreML EP via the `ort` `coreml`
  feature (`ep::CoreML::default().with_compute_units(CPUAndNeuralEngine)
  .with_model_format(MLProgram)`), OFF by default behind the `PHOTOPROOF_ORT_COREML`
  env knob (CPU path byte-identical). FINDINGS: the CoreML EP IS available (no
  custom onnxruntime needed), but our INT8 models do not work on it - the DFN5B
  visual tower's ~397 external-data files mis-load under CoreML, and the int8
  MLProgram compile is pathological (10+ min, killed). CPU baseline confirmed ~18
  img/min. A real win needs an FP16 single-file re-export (path documented; needs
  the original fp32 source not in our int8 snapshot). Kept the wired-off code + the
  reproducible #[ignore] spike harness for when FP16 models exist.
- [x] **P4 RAW demosaic -> PPG** - `761bc14`. Replaced the hand-rolled bilinear
  Bayer demosaic with rawler 0.7.2's built-in PPG (Patterned Pixel Grouping) for
  RGGB crops - fills green along the minimum-gradient direction then reconstructs
  R/B from the completed green plane, removing zipper/maze artifacts on edges +
  fine texture. Only the interpolation changes; our WB + camera->sRGB matrix fix +
  gamma + EXIF orientation stay. Sub-8px crops fall back to bilinear (PPG needs a
  3px border) so the develop property tests stay panic-free. Bumped
  RAW_DEVELOP_VERSION 2 -> 3 so cached `-full-v2` artifacts re-develop at the
  higher quality.

## June 13 2026 - Real image benchmark for search (COCO)

- [x] **`pp-eval-ingest` + COCO benchmark; first benchmark sweep** - merges
  `6263ff8` + fix `316ad2f`; `scripts/fetch-image-benchmark.sh`. Fixes "the golden
  set is too small" the RIGHT way (a real benchmark, NOT fake notes - a planted
  note just tests search finding its own injected text and skips the visual
  signal). `pp-eval-ingest` headlessly creates a THROWAWAY eval library at any
  path, ingests an image folder, CLIP-embeds it (reusing `Library::open` ->
  `scan_root` -> `process_queue` -> `build_clip_embedder` + `process_embedding_queue`),
  and emits a golden set from a normalized captions file (each caption a query, its
  photo the answer) - the real library is never touched. The fetch script stages
  the standard MS-COCO 5K image-text retrieval benchmark (real photos + 5
  human captions each) from a HuggingFace mirror. `EvalRig` made the annotation/
  text space OPTIONAL so an image-only library (no notes) runs CLIP-visual (S4)
  retrieval. REAL RUN: 100 COCO photos embedded -> 500 caption queries -> nDCG@10
  0.954, Recall@10 0.996 (CLIP puts the right photo in the top 10 ~99.6% of the
  time - near-ceiling, as expected from DFN5B). Scales to the full 1000/5000 with
  the same command. NOTE: COCO has no notes, so it measures PURE visual retrieval
  (the fusion s4/beta knobs do not move a single-signal ranking); the journal-
  anchored set is what exercises the notes-vs-visual fusion. (Founder, June 13 2026.)

## June 13 2026 - Tuning loop runs on REAL data

- [x] **Close the no-models gap; first real search sweep** - merge `f64db81` (work
  `18eb55c`) + fix `c7ddb79`. The eval rig (`pp-retrieval-eval`/`pp-sweep search`)
  ran KEYWORD-ONLY - `retrieval_eval::evaluate` built a rig with the embedder slots
  `None`, so the vector signals (S1 annotation, S4 CLIP) were dark and the sweep
  tuned a different search than the app ships. Lifted the pinned model-id->layout +
  `OrtEmbedder` construction into a shared `photoproof-connectors/src/model_specs.rs`
  (app delegates, behavior unchanged); added `EvalRig` (text+clip embedders + the
  `.ppvec` store, model ids resolved read-only off the live `vectors` table) and a
  `--models-dir` flag. `evaluate` now embeds the query and runs the SAME `HybridRig`
  the app builds when models are present; graceful keyword-only fallback keeps the
  headless CI green. Rig built once, re-fused per config. REAL RUN against the live
  library (414 images, 414 image_clip + 31 annotation vectors, 7 journal-anchored
  golden queries): keyword-only nDCG@10 0.286 -> model-backed 0.815 (vectors nearly
  TRIPLE ranking quality). Sweep moved the metric (s4/beta genuinely matter), winner
  on this small set s4=0.5/beta=0.3 nDCG 0.894 - indicative only (tiny journal-
  anchored set; the s4<1.0 preference is this text-heavy query mix, NOT a default
  change). K14 propose-only honored.
- [x] **Real audiobook WER corpus (LibriSpeech) + multi-reader voice sweep** -
  merge `4d1995e` (work `04131c0`), `scripts/fetch-voice-corpus.sh`. Staged the
  standard ASR benchmark (public-domain audiobooks) as the WER corpus: dev-clean (97
  chapters, tune on) + test-clean (87 chapters, report on), 40+ readers, per-chapter
  continuous wav + reference transcript + manifest. CRITICAL: utterances are joined
  with a 0.5s silence gap so the VAD gate closes and the endpoint rules (rule2 etc.)
  actually fire - without it a [voice] sweep over rule2/hysteresis would be a no-op
  (caught + fixed during the merge; an earlier gap-less staging was wrong). Extended
  `pp-sweep voice` with `--corpus-manifest`: scores token-weighted CORPUS WER (total
  edits / total reference tokens across all chapters) so one short chapter cannot
  swing the rank; ascending gating-cost; single-file form preserved. Gitignored
  corpus; only the staging script + manifest format committed. (Founder, June 13 2026.)

## June 13 2026 - Automated testing/tuning loop

See `docs/DESIGN-TUNING-LOOP.md` for the architecture (`sweep -> score -> rank ->
propose -> commit -> guard`; contracts stay fixed, dials are tuned, K14 proposes).

- [x] **pp-sweep search** - merge `2f8a7cf`. The research/offense half for search:
  `pp-sweep search --grid "s4=0.5,0.75,1,1.25;beta=0.3,0.5" --propose <file>` runs the
  existing retrieval eval per config (refactored a shared `evaluate(db, queryset,
  weights, k)` reused by `pp-retrieval-eval`), ranks by nDCG@10, writes a PROPOSED
  `tuning.toml` `[search]` block + delta (founder commits). Threaded `rrf_k` through
  `HybridOptions` so it varies per-config in one process.
- [x] **Regression guard (make tune-check)** - merge (worktree) + fix `981e011`. The
  testing/defense half: `pp-tune-check` runs the cheap synthetic benches
  (retrieval_eval_sample IR metrics + pp-bench synthetic ingest), compares to a
  committed `tuning-baselines.json` within tolerance, `--update-baseline` regen, JSON
  out. KEY POLICY (fixed post-merge): only the DETERMINISTIC search-quality metrics
  GATE; ingest throughput is WARN-only (machine/load-variable - the parallel build
  fan-out proved it false-fails otherwise). `make tune-check` / `make tune-baseline`.
- [x] **pp-sweep voice + `[voice]` config lift** - merge `f1d2062`. Closed the voice
  loop: lifted the endpoint rules (`rule1/2/3`), VAD hysteresis (`vad_enter/exit/hang`),
  and `pre_roll_ms` from code consts/pp-asr-server flags into a `[voice]` `tuning.toml`
  section (DIALS; the onset-budget/skew/drain CONTRACTS stay fixed consts), now READ by
  the launcher (`asr_wrapper_args`), the VAD build (`state.rs`), and the engine pre-roll.
  Defaults byte-identical to before. New `voice_bench.rs` shared module (VAD+ASR+engine
  wiring moved out of the bin); `pp-sweep voice --corpus --expect --grid ...` ranks
  ASCENDING by gating-cost (gated WER - raw WER), proposes a `[voice]` block.
- [x] **Property + fuzz tests** - merge `059a842`. proptest (256/64 cases, fixed seed):
  the search firewall (non-vocab token never a chip; parse never panics on arbitrary
  unicode), sidecar write->read identity + graceful reject of garbage, the RAW develop
  matrix invariant (arbitrary/zero/NaN/Inf matrices never panic, never NaN/black pixels -
  the black-fix proven), and tuning bounds (every dial accepts iff in-range else
  rejects-to-default). No real bugs surfaced.
- [x] **Frontend cross-flow coverage** - merge `895629e`. 29 vitest tests over the
  cross-cutting flows: viewMode transition chains preserving scope+photo, scope-subject
  voice routing precedence, clear-previews global cache-bust + grid-wide rebuild, graph
  node selection scoping. 1041 tests total, no bugs surfaced.

## June 13 2026 — Dogfood round 4: Visualizer polish + RAW cache versioning

Founder dogfood asks against the live Visualizer (the semantic topic-graph lens)
and the RAW develop pipeline. Tracked in the session task list rather than as
discrete BACKLOG checkboxes; recorded here for the changelog.

- [x] **Visualizer performance: butter-smooth** — merge `2503ed8` (work `8fe9d08`).
  Founder: "if I leave the view and come back to graph it all rerenders??? we
  need a focused agent... this has to be BUTTERY smooth." Root cause of the
  reopen re-render: `App.svelte` gated the lens with `{#if ui.graphOpen}`, so
  close DESTROYED the component and every expensive thing (nodes/anchors/affinity,
  zoom/pan, AffinityCache, GraphThumbCache, fields, heat) was instance-local and
  re-derived on reopen (guaranteed affinity miss, golden-spiral reseed discarding
  settled positions, reheat, empty thumb cache). Fixes: new `logic/graphstore.ts`
  single-slot store keyed by (scope, sorted topic-set) snapshots settled
  layout/view/field on unmount and restores on mount (no fetch, no reseed, no
  reheat); AffinityCache + GraphThumbCache moved to `<script module>` so reports
  and decoded `<img>`s survive unmount (repaint callback re-targeted to the live
  instance each mount). Faster settle: `REHEAT_START 6→10`, `HEAT_COOL 0.92→0.88`,
  `subStepsForHeat` (3 sim sub-steps/frame hot, 1 cooled), `seedNearAnchors`
  (snap ~60% toward dominant-topic anchor on recompute). Faster cold open: draw
  the anchor ring immediately before awaiting affinities. Idle-when-stable
  preserved. (Founder, June 12 2026.)
- [x] **Visualizer: zoom grows the thumbnails** — merge `2503ed8` (work `8fe9d08`).
  Founder: "when I zoom on the graph the previews don't change size? they should
  grow to some max size... not huge, but bigger than currently." `nodeBaseSizePx`
  had no zoom term (fixed screen size at every zoom). New `zoomedSide(base, zoom,
  min=20, max=132)` scales draw size with zoom, clamped; both `draw()` and the
  hit-test (`nodeHitExtent`) use it so picks match paint. (Founder, June 12 2026.)
- [x] **Visualizer: `g` returns to grid** — merge `2503ed8` (work `8fe9d08`).
  Founder: "when on graph pressing 'g' doesn't return to grid?" `goHome()` never
  touched `graphOpen`; now closes the lens first. (Founder, June 12 2026.)
- [x] **Rename the "Graph" lens to "Visualizer"** — merge `3cf660b` (work `a658c99`).
  Founder: "can we call the Graph 'Visualizer' everywhere?" User-visible copy only
  (action def verb/label, GridHeader entry button aria/title); code identifiers
  (`toggle-graph`, `TopicGraph.svelte`, `forcegraph.ts`, `[graph]` tuning, prefs)
  left untouched. (Founder, June 12 2026.)
- [x] **Version full-res RAW artifacts so develop fixes auto-invalidate** — merge
  `929f0da` (work `31ec891`). The black-RAW develop fix (color-matrix sourcing)
  left STALE cached 1:1 artifacts that stayed black with no manual clear. New
  `RAW_DEVELOP_VERSION: i64 = 2` const + filename scheme `<hash>-full-v<N>.{webp,jpg}`
  (set to v2 because pre-fix black files were written UNVERSIONED, so they are now
  cache misses and re-develop in color). `existing_full_artifact` resolves only the
  current version (forces re-develop), the glob `is_full_artifact` matches current +
  old `-v<N>` + legacy unversioned (so stats/evict/clear reap every version), and a
  new `remove_stale_full_artifacts` sweeps a hash's stale full files after the atomic
  write. Serve route `/full-decode/<hash>` unchanged (version is internal to the
  filename). +5 preview.rs tests. (Founder, June 12 2026.)
- [x] **Settings: surround color in Appearance + Follow-theme toggle** — merge
  `706e3ea`. The per-image backdrop surround moved into the Appearance section with
  a Follow-theme (default) vs Manual override toggle (`surround-store.svelte.ts`).
  (Founder, June 12 2026.)
- [x] **Consistent hover effects + explanatory button tooltips** — merge `b6bd874`
  (work `34cf626`). Founder: "do a pass over the UI for more consistent hover
  effects explaining buttons." Extended the existing `primitives/tooltip.ts`
  `{@attach tooltip()}` helper to surface the action registry's explanatory `label`
  (single source of truth, key-chord chip appended) and to accept a plain `text` for
  non-action buttons, replacing ad-hoc native `title=`. Applied across GridHeader,
  Station, TopicList, StrokePreview, JournalTab rate buttons. No layout/behavior
  changes. (Founder, June 12 2026.)

## June 13 2026 — Dogfood round 5: cache recovery, dictation, view unification

- [x] **Clear-previews: live cache-bust + auto-rebuild** — merge `8677907` (work
  `02f1af1`). "Clear all previews" left the grid stale until restart (webview
  immutable cache) then stuck at "?" forever (clear didn't re-pend generation).
  Fix: clear emits a global `previews-changed` (empty `hashes` = bump every thumb's
  `?p=` cache-bust so the webview re-requests past the immutable header); and
  `ClearKind::All` re-pends the preview pass for all active roots so thumbnails
  regenerate. Button relabeled "Clear all previews" -> "Rebuild all previews".
  (Founder, June 13 2026.)
- [x] **Viewport-first preview generation** — merge `c2bbf13` (work `edb395e`). As
  the grid viewport changes, the visible (preview-missing) hashes' ingest preview
  pass is bumped above backfill priority (debounced `prioritize_previews` command),
  so after a rebuild the rows you are looking at generate first. Cooperates with the
  client-side thumbqueue load order. (Founder backlog, June 13 2026.)
- [x] **Dictation spanning an image swap targets all viewed images** — merge
  `1acf6d2` (work `75e7600`). A dictation that crosses an arrow-nav (A->B mid
  sentence) used to mint only on A (target frozen at speech onset). Now
  `engine.set_scope` unions the new image into every open in-flight utterance's
  held snapshot, so the note lands on every image viewed during the utterance
  (founder chose "attach to both/all viewed"; no per-word timestamps for a precise
  split). Order-preserving, de-duped. (Founder, June 13 2026.)
- [x] **Visualizer node selection + capture scope** — merge `111dcd9` (work
  `7bc7963`). Single-click a graph node selects it (glow + sets capture scope so
  dictation/rating land on it), double-click/Enter opens Look, Esc deselects then
  closes. Opening the lens neutralizes scope so dictation never hits a stale image.
  Fixed a latent bug: the Visualizer never set scope before. (Founder, June 13 2026.)
- [x] **Topic note log** — merge `1d76407` (work `844da57`). Topics get an
  append-only note log mirroring the existing `collection_notes`: new `topic_notes`
  table (v15 migration), store methods, commands, IPC, a `TopicNotesSlice`, and a
  `TopicNotes.svelte` rail surface shown when a topic detail is open (selecting a
  saved topic both scopes the grid and surfaces its notes, like collections). Typed
  text only. (Founder, June 13 2026.)
- [x] **Unify views: scope x viewMode** — merge `11ea509` (work `137efe5`); design
  `docs/DESIGN-VIEW-MODES.md`. Replaced `surface` (grid|look) + the bolted-on
  `graphOpen` overlay with one `viewMode` (grid|visualizer|look) orthogonal to
  `gridScope`, plus a shared `activeHash` seeded on each view switch so the same
  photo carries across grid<->visualizer<->look. `graphSelection` -> `viewSelection`.
  A future `compare` mode now slots in additively (the litmus in the design doc).
  Reconciled the earlier neutralize-on-open: the visualizer seeds from the active
  photo (not stale), neutral only when nothing is focused. (Founder, June 13 2026.)
- [x] **Voice dictation to a collection/topic note log** — merge `d2ed7be` (work
  `b42cabb`); design `docs/DESIGN-VOICE-SUBJECTS.md`. The capture scope gained an
  optional `ScopeSubject` (Collection|Topic id). Routing rule: a focused image
  always wins (image note); dictation appends to the subject's note log only when a
  collection/topic detail is open AND no image is focused; nothing focused + no
  subject = session note. `on_final` routes via a `SubjectNoteSink` trait (engine
  stays decoupled; existing `CaptureEngine::new` untouched), appending verbatim text
  to `collection_notes`/`topic_notes`. The scope pill names the subject ("noting:
  <name>"). Subject is onset-bound and frozen for the utterance; the spanning-swap
  union stays image-only. (Founder, June 13 2026.)

## June 13 2026 — Roots and folder tree (design round, mostly)

- [x] **Folder-tree improvements** — `770fc5f` (merge `7c26126`). The bulk of the
  "roots and subfolders" design round. Deep-tree ergonomics: lazy expansion past
  `AUTO_EXPAND_DEPTH` (a deep root never renders its whole tree eagerly; explicit
  expands survive the cap) and a filter / jump-to-folder input (type to narrow to
  matching names + ancestors, Enter opens the first match). Overlapping roots: the
  "refuse, merge, or alias?" question decided as REFUSE nesting -
  `register_root` returns a structured `OverlappingRoot { existing_root_id }`, and
  the rail NAVIGATES to the existing root instead of double-ingesting. Archive
  lifecycle: a non-destructive `archived` root state (v14 migration rebuilds the
  `roots.state` CHECK, copies rows verbatim so journals + memberships keyed by
  image hash are untouched); `archive_root` flips state + stops the watcher,
  `unarchive_root` restores + rewatches + rescans drift; archived roots live in a
  collapsed "Archived" rail affordance. STILL OPEN (see BACKLOG): group-by-volume
  in the Folders tab (deferred) + the collections-first folder-UI framing.
  (Founder, June 2026.)

## June 13 2026 — Visualization lenses

- [x] **Attention / engagement heatmap** — see `docs/DESIGN-ATTENTION-HEATMAP.md`.
  Engagement-intensity per image, NOT gaze surveillance: dwell is capped, local,
  and lives OUTSIDE the journal (K14). Backend (photoproof-core): a `[heatmap]`
  tuning section (`w_dwell`/`w_events`/`w_strokes`, `dwell_look_rate`/
  `dwell_grid_rate`, `dwell_cap_ms`, `recency_half_life_days` — rates/cap/weights
  are config, not literals) + matching `tuning.default.toml` block; a v12 schema
  migration adding `image_dwell ( image_hash PK, dwell_ms, focus_count, last_ts )`
  (local telemetry, preserved by `rebuild_derived`, never in sidecars) and an
  `image_journal_stats.stroke_count` column maintained in the SAME recompute
  transaction as event insert/retract/redact (live strokes only). Store methods:
  `record_dwell(hash, source, elapsed_ms)` (tier rate + 60s cap applied in the
  backend, accumulated per image), `image_intensity(hashes, all_time)` (composite
  `w_dwell·dwell + w_events·events + w_strokes·strokes`, normalized 0..1 across the
  scope; recency-weighted by `0.5^(age_days/half_life)` unless all_time), and
  `clear_dwell()`. Three Tauri commands registered in both handler lists. Frontend
  (apps/desktop): a `logic/dwell.ts` focus-episode tracker + a localized
  `app.svelte.ts` hook (refocus from `reportScope`'s ONE funnel, flush on leaving
  Look / deselect / switch / window blur + visibilitychange / short idle); a grid
  heat-tint toggle (Flame, off by default, persisted) rendering a warm glow +
  corner heat-bar in Thumb.svelte; an "All-time" recency switch (founder decision,
  persisted); a "Sort by attention" mode (logic/sort.ts); and a "Clear attention
  data" button in SettingsApp. Tests: backend (record_dwell tier+cap,
  image_intensity composite+normalization, recency vs all-time, stroke_count
  across insert/retract, HeatmapTuning defaults + toml merge + out-of-range
  reject); frontend (dwell episode flush + blur-pause + fan-out, heat + all-time
  toggle state/persist/fetch, sort-by-attention). Gate green (the pre-existing
  `s02_2_case_only_rename_relinks_sidecar` failure aside).

- [x] **Semantic topic-graph (v2)** — see `docs/DESIGN-SEMANTIC-GRAPH.md`. The
  v2 wave on top of the v1 lens: cluster auto-labels + full-library LOD + the v3
  seam scaffold. Backend (photoproof-core::topic): `cluster_topics(scope, k?,
  space?)` runs a self-contained, DETERMINISTIC k-means (farthest-first seeding
  by index + fixed iteration order, no RNG) over the in-scope image vectors — the
  ANNOTATION space (`image_summary`) by default since the labels are
  note-grounded, CLIP optional. `k = clamp(round(sqrt(n/2)), cluster_k_min,
  cluster_k_max)` unless passed. Each cluster is LABELED by the most
  representative salient n-gram in its members' notes (reusing v1's `mine_ngrams`,
  refactored out of `suggest_topics`; most frequent then longer phrase then
  alphabetical), with a generic `Group N` fallback. Returns `[{ label, size,
  centroid_affinity }]`. Reads STORED vectors via a new bulk
  `PpvecStore::read_image_vectors` (one lock/mmap pair, mirroring `score_images`)
  + `any_model_id` so it clusters an embedded library even with models unloaded;
  empty/un-embedded scope returns empty, never errors. New per-image
  `scope_note_texts_by_hash` for per-cluster labeling. v3 SEAM (scaffold only,
  not faked): a `TopicLlm` trait + `suggest_topics_llm` returning an explicit
  `Unavailable` state (Gemma connector mocked in M1) with `// TODO(v3)`. Frontend
  (apps/desktop): `forcegraph.ts` gains LOD — `aggregateToSuperNodes` (bin by
  dominant topic, mass = member count, position = affinity-weighted centroid),
  `expandSuperNode`, `shouldUseLod`; the pure `step` integrator now weights
  repulsion by the mass product and divides acceleration by mass (a single image
  at mass 1 is byte-identical to v1). `TopicGraph.svelte` shows a note-grounded
  "auto topics" rail above the cheap rail + a hidden LLM "themes" rail (appears
  only when the seam is real), aggregates past `graph.lod_threshold` (default
  1500) with the banner now reading "LOD active (showing N clusters of M
  images)", and expands a super-node on click or zoom. New `[graph]` knobs:
  `cluster_k_min`/`cluster_k_max` (k bounds) + `lod_threshold`, with
  `tuning.default.toml` in lockstep. Tests: backend (k-means deterministic on
  planted clusters, k>=n, `pick_k` heuristic, label picks the right note phrase,
  no-notes generic fallback, empty/un-embedded empty, the LLM seam Unavailable,
  the new tuning defaults + merge + range-reject, the command graceful path);
  frontend (super-node creation past threshold, mean-affinity centroid,
  determinism, expand/collapse, the sim handling mass). Gate green (the
  pre-existing `s02_2_case_only_rename_relinks_sidecar` failure aside).
  FOUNDER-REVIEW: the `lod_threshold` 1500 default is a placeholder just above
  v1's ~1200-node strain banner — reconcile with the real scale-spike profile.

## June 12-13 2026 — RAW decode + UI polish wave (three parallel-agent builds)

- [x] **Semantic topic-graph (v1)** — see `docs/DESIGN-SEMANTIC-GRAPH.md`. The
  force-directed lens generalizing "more like this" from an image anchor to a
  TOPIC PHRASE anchor. Backend `photoproof-core::topic`: `topic_affinities`
  embeds each topic in BOTH spaces (CLIP-text tower for the VISUAL half, the
  text embedder + §3 instruct template for the ANNOTATION half), scores every
  in-scope image via a new `PpvecStore::score_images` (the same brute-force
  cosine kernel `search()` uses, but over a KNOWN scope set, not a global
  top-k), then blends `α·visual + (1−α)·annotation`. `suggest_topics` = cheap
  v1 candidates (frequent note n-grams + overlapping collection names, no LLM).
  Three Tauri commands (`topic_affinities`/`suggest_topics`/`graph_tuning`) over
  folder / collection / WHOLE-library scope (the deliberate scale spike; node
  count + scan time LOGGED, never silently capped). Frontend: a pure
  velocity-Verlet force sim (`logic/forcegraph.ts`, ring anchors + affinity
  attraction + repulsion + centering, unit-tested for deterministic
  convergence) rendered to canvas in `components/graph/TopicGraph.svelte` (an
  add-topic input, a suggestion chip rail, a looks/said α slider that re-blends
  live, a full-library toggle, drag/click). Click a topic anchor → semantic
  query scope of the grid; click an image node → Look. `GraphTuning` added to
  the centralized tuning config (`[graph]` block, file-overridable). Graceful
  by construction: a degraded/un-embedded rig returns a well-formed zeros report
  with honest readiness flags, never an error. Tests: blend at α 0/1/0.5 over
  real planted vectors + degraded-rig zeros; suggest_topics n-gram mining;
  GraphTuning defaults + toml merge; the pure force sim convergence + topic
  add/scope/open flows. REMAINING (still in BACKLOG): v2 cluster auto-labels +
  full-library LOD; v3 LLM topic suggestion.

- [x] **Full RAW decode (1:1 preview)** — landed `6d7c4fb` (merge `0722efe`):
  Phase 1 on-demand neutral develop. New `raw_develop` module in
  `photoproof-core`: black/scale (rawler `apply_scaling`) → white-balance
  as-shot → bilinear Bayer (RGGB-family) demosaic → camera→sRGB matrix → sRGB
  gamma → orient LAST (geometry-exact, strokes-land-where-drawn, §9.4). The
  matrix is composed dcraw-style — `cam2rgb = pseudo_inverse(normalize(
  xyz_to_cam[RGB] · SRGB_TO_XYZ_D65))` — mirroring rawler's OWN neutral path,
  because `cam_to_xyz_normalized()` normalizes to camera-neutral=XYZ(1,1,1)
  (not D65) and tints grays (verified). CFA-vs-linear-DNG guard (a linear DNG
  is cpp=3, NOT demosaiced); X-Trans / RGBE / CYGM / monochrome skip clean
  (`UnsupportedCfa`) so the embedded preview always stands; decode wrapped
  panic-safe. `process_raw_decode_queue` drains on a NEW decode pool
  (`max(2, physical_cores/2)`, separate from the M1 CPU pool), `capture_live`-
  cancellable per item (yields to an armed mic). ON-DEMAND: the eager ingest
  enqueue is REMOVED (the 154 permanently-pending rows dissolve); a view-time
  trigger (`request_full_decode`) enqueues one row at a new `PRIORITY_INTERACTIVE`
  (above the watcher) when Look opens an undeveloped RAW, showing "developing...".
  OD-1: a full-SENSOR-resolution artifact (WebP q90, JPEG fallback past
  libwebp's 16383px cap), served by a new `/full-decode/<hash>` deep-zoom route,
  in addition to the 2560 display+thumb tiers (`source='full-decode'`). 7
  synthetic unit tests (known-color RGGB phase, gray-neutrality, orientation
  aspect, linear-DNG-not-demosaiced, X-Trans/RGBE unsupported, float-data) plus
  an `#[ignore]` founder-machine real-RAW stub. The plan was CORRECTED first:
  rawler 0.7.2 `cropped_cfa()` and `linearize()` are `todo!()` PANICS (the same
  panic that stalled imagepipe's migration) — routed around via `camera.cfa` +
  `CFA::shift` and `apply_scaling`; `pixels_u16()` panics on float DNGs — uses
  `data.as_f32()`. Founder decisions ratified in `docs/PLAN-RAW-DECODE.md`.
  REVIEW NOTES (open follow-ups): the full-res artifact is disk-only (no
  `preview_artifacts` schema bump — existence on disk is the cache signal); the
  CFA-shift-with-nonzero-crop phase is exercised only by the founder-machine
  real-RAW test, not the synthetic ones; stroke-promotion logic removed (stroked
  RAWs now develop on view like any other). Resolves the "Embedded preview —
  full decode pending" / "154 stuck RAWs" / "DNG never loads 1:1" founder
  reports (same root cause). (Founder, June 12 2026.)
- [x] **Grid right-click submenus are janky** — landed `91bfa15` (merge
  `e8faf55`): cascading side-flyout submenu panels replacing the in-place
  one-level + breadcrumb swap. New pure `flyout.ts` (edge-aware flip: prefer
  right, flip left only when right overflows and left fits, clamp on-screen,
  top-align with bottom-clamp) and `hoverintent.ts` (open-delay 110ms /
  close-delay 280ms, the simple delay model not a geometric safe-triangle).
  `Menu.svelte` reworked to render the open chain (= `nav.path`) as fixed,
  measured, stacked panels that stay DOM descendants of the menu root (so
  `Popover`'s outside-click is untouched). The `menu.ts` keyboard controller and
  the `menus.ts` data model are UNCHANGED — every call site is invisible to the
  migration. 11 new pure-module tests; the 16 existing menu tests stayed green.
  (Founder, June 12 2026.)
- [x] **T cell-info grows the cell, not overlays the image; info at the top** —
  landed `d541854` (merge `10796c8`): because cell-info is global (one level
  for all cells), every row reserves the same fixed info strip at the TOP and
  the cell extends downward, so the image stays fully visible and rows stay
  UNIFORM — the virtualizer needed no algorithm change beyond a larger row
  stride (`rowH = cell + info + gap`). New pure `infoStripHeight(level)`
  (none=0, minimal=18, annotated=32 px). `marquee.ts` hit-test offset by the
  strip height so selection still targets the image box, not the strip (the one
  subtle spot). Badges re-anchored to the image box. All retry/recycle/
  placeholder logic untouched. (Founder, June 12 2026.)

### Night tooling (same wave, autonomous)

- [x] **Em-dash creep gate** — landed `a60591a` (merge `c010179`): the
  "NOT done: a CI grep-gate" sub-item of the em-dash rule. `scripts/check-no-
  emdash.sh` scans `apps/desktop/src` for `—`/`–` in user-visible Svelte
  template text + rendered attributes + TS/JS string literals, stripping
  `<script>`/`<style>`/comments (with a `://` URL guard) and allowlisting the
  `menus.ts` separator sentinel by exact form. Green on the current tree; wired
  as `npm run check:emdash` and added to the BUILD-LOOP frontend gate line. No
  GitHub Actions (CI policy left to the founder). (Coordinator, June 13 2026.)
- [x] **Audiobook WER scorer** — landed `a4b9604` (merge `d6cf279`): the
  remaining piece of the "Audiobook WER stress harness" backlog item. New
  `photoproof_core::voice_wer` (normalize → word-level Levenshtein → S/D/I/N +
  WER + hit rate, 10 unit tests) plus a `pp-voice-bench --expect <transcript>`
  upgrade that drives the pipeline TWICE over one recording — GATED (production
  VAD params, the path that can truncate) and RAW (gate forced open, the
  model-accuracy ceiling) — and reports both WERs + the gating cost, with
  `--json`. Back-compat preserved (no `--expect` = the old single-pass sweep
  shape). The real Alice-corpus run is founder-machine (`$PP_VOICE_CORPUS` +
  the gitignored wavs). Still open in the harness item: actually running it on
  the corpus and reading the raw-vs-gated delta. (Coordinator, June 13 2026.)

### Search-as-scope + histogram + eval (same wave, autonomous, cont.)

- [x] **Search-as-scope Phase 1** — landed `c4735bf` (merge `a71021e`): the query
  is now a THIRD grid scope alongside folder and collection. The old
  `collectionId`-null two-mode arbitration became a `gridScope` discriminated
  union (`folder | collection | query`); `collectionId` is a back-compat
  `$derived` getter. A new `runQueryScope()` feeder enriches fused-order result
  hashes into GridItems (new `list_images` IPC, order-preserving) and renders
  them IN the grid via `grid.setItems`, guarded by the `gridLoad` token. The
  whole separate overlay selection system is RETIRED: `SearchOverlay.svelte` +
  `SearchResultRow.svelte` deleted, `searchSel`/`searchFocus`/`resultHashes`
  gone, openLook's `fromSearch` branch gone, one selection system (`grid.sel`).
  An always-visible search bar lives in `GridHeader.svelte` (chips + debounce
  migrated from the overlay); `/` and Cmd+F focus it; Escape splits into
  clear-query-scope then blur. Relevance added to `SortMode` (pass-through of the
  backend's fused order, auto-selected in query mode). Backend: `mode:
  "lexical" | "semantic"` on the `search` command (default Auto = prior
  behavior); lexical forces the M1 keyword rig even on warm-embedder machines to
  hold the <100ms keystroke budget (`search_latency.rs` extended with the lexical
  assertion). The agent self-caught two regressions before commit (first-keystroke
  text-erase; a misleading empty-state message). Phases 2-4 (explicit
  lexical/semantic status, per-signal weight toggles, fuzzy) follow.
  D1-D6 ratified in `docs/DESIGN-SEARCH-AS-SCOPE.md`. (Founder + coordinator, June 13 2026.)
- [x] **Histogram overlay in Look** — landed `4b0fe60` (merge `7a6a9b5`): a
  reviewing-aid RGB+luma histogram (exposure / clipping check), toggled by `H`
  (audited free against every Look binding), top-right, semi-transparent,
  pointer-events-none. Computed from the DISPLAYED image via an offscreen canvas
  downsampled to <=1024px long edge, binned once per image change (off the render
  path), recomputed when the RAW full-decode artifact swaps in. Pure tested
  binning module `logic/histogram.ts` (14 tests: Rec.709 luma, transparent-pixel
  skip, downsample, log/linear normalize). Log-scaled by default (keeps end-range
  clipping legible). Obeys Tab lights-out; off by default, persisted. FOUNDER
  REVIEW: combined R/G/B+luma display (vs a luminance-only toggle); log default;
  no explicit clipping-callout markers yet. (Coordinator, June 13 2026 — was a
  "needs founder appetite" item, built on the new decode pipeline.)
- [x] **Golden-query retrieval eval harness** — landed `abcc31f` (merge
  `bf7cd48`): the M3 retrieval-quality gate instrument (the founder supplies the
  query set). New pure `retrieval_eval` module (precision@k, recall@k, MRR,
  nDCG@k with ideal-DCG normalization; 14 unit tests) + a CI-gated sample eval
  (`tests/retrieval_eval_sample.rs`, builds a synthetic corpus via the
  `retrieval_hybrid` helpers and asserts sane metrics) + a `pp-retrieval-eval`
  runner bin (`--db`/`--queries`, `--k`, `--json`, and `--s1/--s2/--s3/--s4`
  weight-sweep overrides via the existing `FusionWeights`/`HybridOptions` API).
  Query-set is JSON keyed by BLAKE3 content hashes; drop the real set at
  gitignored `test-corpora/retrieval/golden.json` (README committed). The runner
  uses `keyword_only_rig()` (no live models); a full four-signal sweep is a
  desktop-driven run feeding the same scorer. Beta (`SIM_BLEND_BETA`) stays a
  compile-time const (would need promoting into `HybridOptions` to sweep at
  runtime — deferred to avoid touching hybrid.rs mid-search-overhaul). Settles
  the B69 "how much should S4 vote" question once real queries land. (Coordinator,
  June 13 2026 — blocked-item advance: instrument built, query set is the
  founder's to supply.)

- [x] **Search Phase 4: fuzzy quiet-toggle** — landed `8d4e6a5` (merge
  `514a1b1`): a `~` glyph in the search bar (off by default), typo-tolerant
  matching over the metadata columns camera/lens/filename. Length-scaled
  Levenshtein (via `strsim`, already in the tree) over the DISTINCT metadata-
  value space — tiny + low-cardinality, so it stays inside the <100ms keystroke
  budget (a new `fuzzy_armed_lexical_lane_stays_under_budget` test pins it). Key
  insight: camera/lens/filename are filter-only columns, NOT in the FTS corpus,
  so fuzzy is a genuinely new ADDITIVE pass. Structurally exact-first: the fuzzy
  pass runs only after the exact FTS set is assembled, appends with a new honest
  `Provenance::FuzzyMeta { field }` ("approximate <field> match"), and skips any
  hash already exact (no dup, no demotion). `fuzzy: bool` through the search
  command, default false = byte-identical; lexical-lane-only (never the semantic
  commit). 7 backend + frontend tests incl. exact-beats-fuzzy and off-is-
  identical. Completes the search-as-scope line (P1-P4). The continuous weight
  sliders remain eval-gated. (Founder-confirmed, June 13 2026.)
- [x] **"More like this" (visual-similarity search)** — landed `3ea6f2f`
  (merge `33865e1`): right-click an image -> "Find similar images" -> the grid
  fills with its visual neighbours. A new `find_similar(hash, limit)` Tauri
  command reuses the existing `image_clip` PPVEC store (`VectorStore::search`,
  brute-force cosine) — new `fetch`/`image_clip_model_id`/`similar_images`
  accessors on `PpvecStore`; resolves the model_id from the stored vectors
  metadata so similarity works even when the CLIP model isn't loaded in memory.
  Surfaced through the search-as-scope machinery: a new `gridScope` variant
  `{kind:"similar", hash, filename}` rendered exactly like a query scope
  (relevance/similarity order, residue "similar to <filename>", one-key clear,
  Escape) via a `runSimilarScope()` mirroring `runQueryScope`. Self-excluded;
  empty/un-embedded index returns empty gracefully (correct before any embed
  pass). Additive only - hybrid fusion + text-search command untouched. 5
  backend + 5 frontend tests. This also proves the topic-graph primitive:
  "score every image vs a reference vector" generalizes from an image anchor to
  a topic-phrase embedding. (Coordinator, June 13 2026 - nice-to-have.)
- [x] **Foreign-edit sidecar reader (portable subset)** — landed `15d88fb`
  (merge) + `0396581` (gate fix): a READ-ONLY backend reader for Lightroom /
  darktable XMP sidecars extracting only the PORTABLE subset — rating (0..5,
  -1 reject), label/color, orientation, and (Lightroom-only) the normalized
  crop rect + angle. darktable crop lives in opaque base64 IOP params and is
  NOT decoded; a `<darktable:history>` block sets `has_unreadable_edits` so the
  UI can later flag "edited in darktable, we can't reproduce it." `quick-xml`
  (default-features off, pulls only memchr) parses both compact-attribute and
  expanded-element RDF; malformed input returns None, never panics; our own
  `.photoproof.json` sidecar is never mistaken for a foreign one. Public API
  `library::read_foreign_edit(path)` / `read_foreign_edit_from_str(xmp)`, 14
  unit tests. HONEST SCOPE: faithful edit RENDER is out (= reimplementing the
  editors); this is the advisory portable seam. FOLLOW-ON (out of scope here):
  surface rating/label/orientation + draw the LR crop overlay on our neutral
  develop + a "has edits we don't reproduce" badge, behind a Tauri command.
  NOTE: this one's gate could not run at merge time (the build disk filled
  mid-night); the coordinator fixed two real bugs (darktable history detection;
  a clippy collapsible_if + a dead helper) and re-gated green before pushing.
  (Coordinator, June 13 2026 — design-round item, backend foundation built.)

## June 12 2026 — the evening waves (two parallel-agent builds + inline fixes)

- [x] **B summons the overlay** — landed `c13f09b`: the key was dead
  twice over (the `pencil-pen` def gated on `overlayVisible`, and
  `togglePencil()` refused while hidden); now B with hidden paper shows
  the overlay AND arms the pencil in one keystroke (show-and-arm),
  visible-overlay toggling byte-for-byte unchanged. (Founder, June
  2026.)
- [x] **The What's-Happening Station (indicator 2.0)** — landed
  `de9f126` (merge-fixed: the mic seat resolves `mic-press` arg
  "toggle", the def the M→Space move owns): pure StationModel
  (logic/station.ts) over existing state, collapsed icon row with one
  breathe driver, hover-expand via the indicator Popover (read-only
  body; icons are the only click targets), info seat pins via new
  `toggle-station-detail` row, pop-chips generalize the note pop to
  mic arm/disarm/"Captured". Founder manual pass pending: pulse/hover
  feel, chip stacking. Original riff: (founder, June
  12, 2026 — "Do you see what I mean?" riff, captured verbatim in
  spirit): evolve the bottom-right capture indicator into the app-wide
  LIVING STATUS ORGAN. Same corner, bigger presence. Two states:
  COLLAPSED = a quiet icon row (mic, magnifying-glass search,
  background-tasks/info dot, the note pencil), pulsing gently when
  something is happening; HOVER = the capsule expands large with real
  context (ingest/digest progress with counts, background task list,
  current scope, streaming utterance), shrinking back to icons on
  leave — counts move INTO the hover, off the always-on chrome.
  Events POP from the station: note creation already does (founder:
  "which is cool" — that's the signature move, keep it), mic
  arm/disarm and push-to-talk evidence join it, and searches could
  pop from there too (pairs with the search-as-scope direction).
  Each icon is a clickable seat with the expected verb: mic =
  toggle (the M tap twin), magnifier = focus search, info = expand
  the tasks view. Existing rulings carry forward: lights-out
  exemption (DECISIONS U5), scope-segment → inspector bridge, the
  note-input summon. This is likely WHERE the digest-visibility
  surface below lives — design the two together. (Founder, June
  2026.)
- [x] **Voice notes save a leading space** — landed `6ee8554`:
  `on_final` mints `seg.text.trim()` (edges only; interior spacing
  verbatim — §6.5 protects words from paraphrase, not BPE tokenizer
  plumbing); acceptance test pins " Slow  down " → "Slow  down".
  NOT taken: normalizing the handful of existing test-note rows
  (append-only journal; they're tonight's throwaway dictation).
  Original report (founder, June 12, 2026):
  every voice remark in the journal starts with a literal " " —
  CONFIRMED IN THE STORE, not a render artifact (sqlite:
  `[ Slow down]`, `[ We've got time left to be lazy]` …; typed notes
  unaffected). Root cause shape: BPE-style ASR tokens carry the
  word-boundary space, so an utterance's first token decodes as
  " Slow", and the engine mints the final without trimming. Fix at the
  final-minting boundary in the capture engine (trim leading/trailing
  whitespace before the journal event exists — whitespace is not "the
  user's words", K14 is safe); decide whether to also normalize the
  nine-and-counting existing rows (journal events are append-only —
  if normalization is wrong, a display-time trim for legacy rows is
  the honest fallback). Check the sidecar snapshots carry the same
  bytes. (Founder, June 12, 2026.)
- [x] **Desktop platform conventions pass** — landed `a0cac41` (audit
  found NO native menu existed): macOS menu bar App/File/Edit/View/
  Window with standard roles (Edit roles = ⌘C/⌘V in WKWebView fields;
  predefined Quit still exits through the sidecar-flush path), custom
  rows routed through the one action registry via a `menu-action`
  event (the menu is a fourth rendering of the action table); UI-scale
  zoom ⌘=/⌘−/⌘0 on a 0.8–1.5 ladder via webview setZoom, persisted
  (`pp.uiZoom`), distinct from Look's plain-key image zoom; keymap now
  forfeits ctrl+meta chords to the menu layer (⌃⌘F fullscreen no
  longer starved by ⌘F search). Founder manual smoke test pending
  (menus/zoom/Edit-paste/window verbs). Original ask:
  (Founder, June 12, 2026):
  all the things long-lived desktop apps just DO, audited and wired for
  macOS first: (a) UI-scale zoom on Cmd+= / Cmd+− / Cmd+0-to-reset —
  the webview zoom convention every Tauri/Electron app inherits (note:
  distinct from the existing image zoom in Look; UI zoom scales the
  chrome) — persist the chosen scale; (b) the window-management row:
  Cmd+W close window, Cmd+M minimize, Cmd+H hide (these come free with
  a proper native menu bar — audit ours for the standard App/File/Edit/
  View/Window menus and make sure every in-app action with a key also
  appears in a menu, which is also what makes them discoverable and
  remappable in System Settings); (c) Cmd+, opens settings (verify the
  existing open-settings binding uses it); (d) Edit-menu basics working
  in every text field (cut/copy/paste/select-all/undo); (e) sweep for
  the rest: double-click titlebar to zoom, full-screen Cmd+Ctrl+F (a
  toggle-fullscreen action exists — check the binding), text-field
  focus outlines. One pass, one checklist, so the app feels NATIVE,
  not webby. (Founder, June 2026.)
- [x] **Click feedback pass: every action acknowledges the click** —
  landed `d8a8658`: one global `button:active` rule (filter+transform,
  chosen because component-scoped background overrides would swallow a
  background-based press) gives every real button a pressed state; new
  `AckFlash`/`AckButton` primitives (copyflash idiom) give
  fire-and-forget verbs a truthful momentary done-label — Restart
  runtime ("Restarted") and Re-detect hardware ("Re-detected") adopted.
  Non-button clickables audited and deliberately left alone (selection
  surfaces already self-signal). (Founder, fresh-instance dogfood,
  June 12, 2026.)
- [x] **M key = push-to-talk on hold, mic toggle on click** — landed
  `2fbe2c9`: pure hold machine (`logic/michold.ts`, time-as-parameter),
  press arms immediately from disarmed (both gestures want sound from
  the keydown), release <250 ms = tap (arm stands), ≥250 ms = PTT
  (explicit disarm ships through the normal drain); from armed, tap
  disarms and hold is deliberately inert (an absent-minded hold never
  tears down a deliberately armed mic). Intents are explicit
  arm/disarm via a new idempotent `set_mic` command — never blind
  toggle. Auto-repeat absorbed; window blur resolves a gesture-opened
  mic, leaves a pre-armed one alone. (Founder, June 12, 2026.)
  SUPERSEDED same night (founder: "like a Zoom call"): the mic moves
  to SPACE — tap toggles, hold is push-to-talk; M is freed back to
  the reserved pool; Space's old verbs displaced (open-Look keeps
  Enter, Look-close keeps Esc, zoomed hold-Space pan dies — drag-pan
  remains). LANDED `e486023`; the hold machine itself was unchanged,
  and §11 input suppression already covered Space (the rule keys on
  "the chord can type", not "single letter").
- [x] **Model download progress must be model-cumulative** — landed
  `ab1369a`: core's download loop carries a `base` accumulator so every
  DownloadProgress event is model-cumulative (the per-file completion
  event is what advances the row through DFN5B's ~290 sub-coalescing
  shards); enqueue seeds from `downloaded_bytes(model)` (statted before
  the host lock) so a resume opens at its true bytes; the dead `last`
  fold deleted. Original diagnosis (founder, fresh-instance dogfood,
  June 12, 2026 — caused two separate "it didn't resume / stuck at
  zero" impressions in one evening while downloads were in fact
  healthy; founder's actual bar: "look and feel modern"): two
  compounding display defects on the settings model rows. (a) `DownloadProgress` bus events carry the CURRENT FILE's
  bytes (core download.rs publish sites), but the row divides by the
  whole model's total (runtime.rs status ~336) — DFN5B is 400 files,
  ~290 of them tiny shards, so the displayed number sits at ~0% while
  gigabytes land verified on disk. (b) clicking Download seeds
  `state.downloads` with `(0, total_bytes)` (enqueue_downloads ~525), so
  a resume of a 1 GB part file FIRST displays "0 bytes" — reads as
  progress thrown away. Fix shape: publish cumulative model bytes from
  core's per-model loop (it knows the model), or seed/fold in the
  manager's `downloaded_bytes(model)` baseline host-side; the discarded
  `last` fold in run_download (~597–623, `let _ = last;`) is a vestige
  of the same seam. One number, one meaning: bytes of THIS MODEL on
  disk over its manifest total.
- [x] **Auto-retry interrupted model downloads** — landed `ab1369a`:
  `run_download` retries the `Interrupted` class ONLY, 4 more attempts
  at 2/5/15/30 s backoff (sliced sleeps against the stop latch so quit
  mid-backoff returns within a beat), row stays "downloading" with a
  `retry_hint` ("connection interrupted — retrying (attempt 2 of 5)")
  until exhaustion; checksum/license/HTTP errors still fail fast.
  NOT taken (still open if wanted): resume-on-launch for models with a
  part file + recorded acceptance. (Founder, fresh-instance dogfood,
  June 12, 2026 — interruptions hit 3× in one evening.) — from the grid or from Look, click-
  drag an image out of the window and drop it into Finder/another app as
  the ORIGINAL file (a native OS file drag carrying absolute paths — the
  D4 reveal/open-with class of OS integration, not an in-app file verb;
  D3 stands: the library never moves or deletes its own files, the drop
  target copies). Implementation pointers: Tauri needs a native start-
  drag (HTML5 dragstart cannot carry real files out of a webview) —
  tauri-plugin-drag (CrabNebula) or NSDraggingSession/NSFilePromise via
  the window handle on macOS. Sub-questions to decide at build time:
  a multi-select drag carries the whole selection; does a collapsed
  RAW+JPEG pair drag both members or the display member (lean: both —
  the pair is one image to the user, and a half-exported pair is the
  kind of silent data loss the welcome card warns about); offline-volume
  images can't drag (no readable path) — quiet refusal, no toast spam.
  (Founder, dogfood round 3, June 2026.)
- [x] **Layout architecture design round: canvas-centered, everything
  resizable** — landed `c12a90c`: one `Panel` primitive (drag-resize
  with pointer capture, double-click-resets, min/max clamps, sizes
  persisted globally under pp.panel.*), canvas-centered shell (flex
  [rail][center 1fr][inspector], center = [canvas][filmstrip] so the
  filmstrip is canvas-width by construction), Tab snapshot-restores
  exactly what was open (DECISIONS exemptions preserved), F total in
  both surfaces (the "works sometimes" gate was scope:"look"), rail
  resize root cause was an $effect that re-read size every drag frame
  and snapped back. Founder manual pass pending: drag feel, filmstrip
  width tracking, traffic-light lockstep. FIRST DOGFOOD FIX `8e24911`:
  the launch filmstrip rendered a fixed 17-neighbor window ("only loads
  17 images… doesn't fill the width") — now a virtual horizontal list
  over the whole order, selected photo centered with the founder's
  override rule (manual scroll holds until the next selection snaps
  back). (Supersedes the narrower "sidebar design pass" from
  dogfood round 2; founder, fresh-instance dogfood, June 12, 2026):
  rethink the BASE LAYERS of the app layout. The principle: the canvas
  (grid/Look) is the center section ALWAYS, regardless of which
  top/bottom/left/right bars are open; every bar is a peer panel with
  the same contract. Concretely from tonight's annoyances: (a) the left
  rail can't be click-drag resized — all four edges' panels should be
  drag-resizable; (b) the left rail and right inspector have visibly
  different UX (affordances, headers, toggle behavior) — one panel
  system, two instances; (c) the filmstrip doesn't extend the full app
  width (and shouldn't depend on what else is open — see the canvas
  principle); (d) F sometimes opens the filmstrip and sometimes doesn't
  — find the contextual gate (or focus dependence) and either make it
  total or make the WHY visible; (e) interaction contract: each panel
  gets its individual toggle, AND the Tab global hide-everything stays
  (it works today and feels right). Layout state (sizes, open/closed)
  persists. This is an architecture round first (the panel/dock layer),
  then reconcile the existing rail/inspector/filmstrip into it.
  (Founder, June 2026.) FOUNDER CALLS (June 12, 2026 — build in
  flight): filmstrip spans the CANVAS width (bottom of the center
  column, dynamic as side panels toggle); panel sizes persist
  GLOBALLY (one size per panel, not per-surface); Tab lights-out
  restores WHAT WAS OPEN (snapshot at hide); F toggles the filmstrip
  in BOTH grid and Look.

## June 12 2026 — the dogfood waves (rounds 1–3: wave2 polish, batch-1 clusters)

- [x] **Mid-ingest scroll stability** — landed: the scroll anchor pins
  the IMAGE (hash) across re-lists — when a re-sort moves it, the
  viewport follows it to its new offset (B64 applied to scroll); and
  scroll-focus-into-view keys on `focusNav` (bumped only by
  setSelection, the user-driven path), so a refresh's silent focus
  remap never yanks the viewport. (Founder, dogfood round 3, June
  2026.)

- [x] **Pair targets vs "+N others"** — landed `wave2/polish` (B61:
  suppress, the stack badge already says it): `siblingTargetsLabel`
  gains the inspected image's pair-mate and never counts it — the mark
  shows only for genuinely DIFFERENT images; `GridSlice.pairMateOf`
  resolves the mate (collapsed alt or expanded partner cell), JournalTab
  threads it down. (Founder, dogfood round 3, June 2026.)
- [x] **"Rebuild previews…" on the rail folder menu** — landed `8755af1`:
  `Library::rebuild_previews(root_id)` re-pends the preview pass for every
  image with an active path under the root, fresh budget, backfill
  priority (the generator_version machinery's manual trigger; regeneration
  overwrites idempotently, §9.8); rail-folder seat
  row right after Rescan. Becomes more load-bearing with M1.5
  preview-policy knobs. (Founder, dogfood round 3, June 2026.)
- [x] **First-run welcome card: how your data is stored** — landed
  `8755af1`: WelcomeCard modal on launch — sidecars
  (`.photoproof.json`, SIDECARS §2.1) live beside images, ARE the data,
  and are filename-specific (outside-the-app renames lean on the §7
  relink heuristics); the index is rebuildable. "Don't show again"
  toggle (default ON) via prefs.ts; escape layer 1; redaction-modal
  frame/focus pattern. (Founder, dogfood round 3, June 2026.)
- [x] **Header shows background jobs** — landed: `IngestStatus` now
  carries a per-pass-kind `passes` breakdown (pending+running, versions
  summed — pure surfacing of `pass_counters` over the existing
  `ingest-progress` channel), and the titlebar shows one dim word
  ("digesting") while ANY kind has queued work; count + kind live in
  the hover title ("Still digesting — hashing 12 · building previews
  480"), never a progress bar. Ingest, preview rebuilds, doctor
  re-pends, and the M3 embedding/caption backfills all flow through
  `ingest_passes`, so the register covers every background job by
  construction (logic/jobs.ts maps queue names to reviewer words;
  unknown passes surface verbatim). The §7.5 indicator hairline keeps
  the fraction. (Founder, June 2026.)
- [x] **Copy actions confirm themselves** — landed: ONE register, the
  icon-to-check flash (toasts stay spec-capped at three triggers,
  UI §7.5/R5, so the confirmation lives AT the affordance).
  `primitives/copyflash.svelte.ts` is the shared seam: every copy
  affordance writes through `copyToClipboard(key, text)` (the one
  webview-fallback clipboard path now) and renders a brief Lucide check
  while `copyFlash.key` matches — truthfully, only after the write
  landed. Applied everywhere copy exists today: the Metadata tab's
  hash/path glyphs flash to a check; the thumb menu's "Copy file path"
  row (def-level `copyConfirm` flag → row `flashKey`) shows the check
  and holds the menu open ~900 ms so the confirmation has a seat.
  Future copy verbs join by setting `copyConfirm` on their def.
  (Founder, dogfood round 3, June 2026.)

- [x] **Library doctor / self-check pass** — v1 landed `8755af1`:
  `Library::doctor()` re-pends done preview passes whose
  artifacts are missing on disk, COUNTS orphaned stale path rows (no
  deletion — conservative by charter), sweeps stranded preview temp
  files; runs on the maintenance tick and as the debug panel's [dev]
  doctor; `info!`s the report when nonzero. v2 candidates remain:
  half-ingested RAW+JPEG pairs (one member's passes dead) → re-enqueue
  the laggard; marker/identity drift → report; stale-orphan sweep. Born
  from dogfood round 3's mangled-folder session: the offline-defer fix
  (`l13_08`) removes the biggest poison source, but mangled states will
  keep happening and the library should HEAL, not just avoid. (Founder,
  June 2026.)
- [x] **Grid: recycled `<img>` can flash the previous image's pixels** —
  landed `wave2/polish`: both loaded-marking paths in Thumb (the
  complete-check effect and onload) now prove via `currentSrc` that the
  element holds THIS hash's bitmap (`srcHash` in ipc/urls.ts) — a
  recycled img stays at the opacity-0 placeholder until the new hash's
  first load; stale complete/naturalWidth and in-flight load events for
  the previous occupant can no longer re-mark it. (P5.1-polish review
  residual, June 2026.)
- [x] **Zoom centering + pan clamp** — landed `652c839` (clampOffsets in
  carryOver; per-axis centering + edge clamp). (Founder, dogfood round 1.)
- [x] **Search entry as overlay, results as canvas** — landed
  (wave2/search): `/` floats the input over a dimmed, pointer-inert
  scrim (visual only — Esc remains the one return path, Sheet's scrim
  contract stands); results expand to the full canvas as they arrive,
  zero-results stays a quiet line in the panel; the contact-sheet
  contracts (selection/write-scope/Look, return point, Esc layers) are
  unchanged. (Founder, dogfood round 2.)
- [x] **Adopt Lucide icons** (`@lucide/svelte`) — landed (wave2/lucide):
  ad-hoc glyphs (🔍 from the spec mockup, sort ▾, ⏏, ×, chevrons, titlebar
  buttons) replaced with the Lucide stroke set, sized per-site (12–16 px)
  and toned via the existing tokens (icons inherit currentColor). Lucide
  ships no eject, so the offline-volume ⏏ became Unplug. UI.md §5 mockup
  emoji is illustrative, not normative. (Founder, dogfood round 2.)
- [x] **Roots changes propagate live across windows** — landed `6dab0f6`
  (batch-1 rail cluster): `add_root`/`remove_root` emit `roots-changed`
  (the `settings-changed` pattern); App listens → `refreshRoots()`.
  (Founder, dogfood round 2.)
- [x] **Add watched folder from the rail, one button click** — landed `6dab0f6`: "Add folder…" footer button + rail-folder context-menu `add-root` row, both opening the picker directly. (Founder, dogfood rounds 1+2.)

- [x] **Compose entries from the journal panel** — landed `506d81a` (batch-1 journal cluster): inline composer in the Journal tab (quiet textarea + rating binding; its focus joins the Esc text-edit layers). (Founder, dogfood round 2.)
- [x] **Journal entries show sibling targets** — landed `506d81a`: "+N
  others" quiet mark (`siblingTargetsLabel`), targets surfaced on the
  journal DTO. (Founder, dogfood round 1.)
- [x] **Select images from note** — landed `506d81a`: `select-journal-targets`
  row affordance + journal-row seat (jump home + select the entry's full
  target set). Availability: every entry kind except redacted stubs (B59).
- [x] **Backend `journal-changed` event** — landed `506d81a`: carries
  affected hashes; journal panel, grid badges, and the Look overlay
  refresh off it (the indicator pulse is pure feedback again).

- [x] **RAW 1:1 via the embedded full-res JPEG** — landed `1cbf7ad`
  (batch-1 raw cluster): `/embedded` route serves the RAW's embedded JPEG
  at native size with the preview's exact §9.3.1 orientation policy
  (strokes stay put at deep zoom); ladder is /original → /embedded →
  preview stands. True decoded 1:1 stays M1.5.
- [x] **Esc keeps the inspector on Look→Grid** — landed `506d81a`: the
  inspector layer peels AFTER Look→Grid (returning to the grid keeps the
  panel on the still-active image). Multi-select display resolved by B60:
  anchor image + quiet "N selected" (`64b220e`).
- [x] **Filmstrip pushes, doesn't overlay** — landed `ca5c9a7` (batch-1
  look cluster): the filmstrip moves the Look viewport up rather than
  covering it (deliberately opposite the rail's I1 overlay convention —
  Look's canvas is the one surface where covered pixels matter).
  (Founder, June 2026.)

## June 12 2026 — lighting up M3

- [x] **Embedder bake-off (MacBook half)** — DONE June 12 2026 (B73,
  docs/SPIKE-P7-EMBED.md): text = EmbeddingGemma-300m q8 (chosen),
  Qwen3-Embedding-0.6B int8 alternative; image = DFN5B confirmed
  (founder call + feasibility numbers + eye-verified zero-shot). All
  SHAs pinned in the report; integration traps recorded.

- [x] **Rail: Folders vs Collections tabs — first slice** — landed
  `98e3cb5`/`d92bd29` (Phase 7): peer tabs in the rail, collection list
  with create + click-to-view (grid shows current members), add/remove
  membership on the image context menu, welcome copy reframed
  (collections are the point; folders are mechanical). REMAINING for the
  design round: the full encouragement UX and autosuggest (below).
  (Founder, June 2026.)

## M2b voice — the P6.1 → P6.2 wiring obligations

All eight closed by P6.2 runtime (`fd0adc8`); recorded at P6.1 review, retired
as a set:

- [x] P6.2: reconcile the two ASR-readiness ctx flags — asrReady (hardcoded false) vs the live asrUnavailable — when supervision lands. (P6.1 review.)
- [x] P6.2: session rotation must re-point an attached CaptureEngine at the newly opened session (shell attaches NoCapture today; currently an undocumented caller burden). (P6.1 review.)
- [x] P6.2: move AudioFeed out of photoproof-connectors' mock namespace — the production engine imports its audio inlet from mock:: (plumbing, not mock behavior). (P6.1 review.)
- [x] P6.2: the shell's real bounded 5 s drain wait at quit (the engine enforces the deadline on its clock; the pump loop owns the blocking wait). (P6.1, B52.)
- [x] P6.2: drain deadline only bites on Poll::Pending — ready finals past the cap still mint and a never-pending stream defeats it; harden against the real stream. (P6.1 review.)
- [x] P6.2: cfg-gate partial text out of the release debug-note ring (§6.5 makes partials dev-build debug territory; today the bounded in-memory ring holds text in all build configs). (P6.1 review.)
- [x] P6.2: pin §6.4's "ArmedSpeaking holds while any utterance is in flight" with a test — a guard-removal mutant currently survives. (P6.1 review.)
- [x] P6.2: close processors run synchronously inline on the close/quit path — fine while the registry is empty, but §2.5 says step 3 never blocks; move onto the pump before real processors register. (P6.1 review.)

## M2a pencil — P5.1 review polish (P5.1 shipped `1e06f1e`)

- [x] Pencil: jitter-dedupe baseline recomputed on transform change (wheel-zoom mid-stroke) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: button-0 gate evaluated before eraser intent — middle/right-click with E held no longer erases or pre-empts the look-backdrop menu — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: PencilOverlay consumes the shared ui.look spaceHeld slice (eraserHeld precedent); the one tracker lives in LookStage behind stageOwnsRawKeys (+ the Space-at-fit close fix, `ffbd515`) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: "Undo stroke" row on the look-backdrop seat (enabled: pencilUndoable) replaces the keyboard-only exemption — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: terminal pen-up sample (dedupe-exempt) to make ts − t_last exact for held dots — founder-resolved, landed with P6.1 (B41). 
