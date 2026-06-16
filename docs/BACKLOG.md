# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis). Shipped items move to LANDED.md verbatim — only open
work lives here.

## Dogfood round 4 (founder, June 12 2026 evening — second live session)

- [x] **Search ranking is rank-flat: any note outranks a perfect CLIP
  match** — landed `0907fe7` (B75): similarity-aware RRF — dense
  (cosine) signals tilt their contribution by `w·(1/(k+rank))·(1+β·cos)`
  (β=0.5, so a perfect match earns up to +50% over its rank baseline
  and can BEAT, not just tie, a same-rank keyword hit); sparse bm25
  signals stay pure RRF; S4 raised 0.5→1.0 (visual = a note's full
  weight), S3 held at 0.5 (derived prose never outvotes own words).
  Spec deviation in DECISIONS B75 + RETRIEVAL §5.3; regression test
  pins the founder scenario. NOTE: weights/β are data the §12 eval
  still owns; the search-UI overhaul will make them user-visible.
  ORIGINAL: (founder, THE headline bug): "ANY saved note in the image
  journal is outranking even perfect semantic visual clip search."
  ROOT CAUSE FOUND (`search/hybrid.rs` FusionWeights): weighted RRF
  with k=60. S2 (note keyword FTS) and S1 (note own-words vectors) are
  weight 1.0; S4 (image_clip visual) is weight 0.5. Because RRF scores
  by RANK not similarity — score = weight / (60 + rank) — an image
  ranked #1 by a weak note keyword hit scores 1.0/61 = 0.0164 and a
  PERFECT CLIP visual match ranked #1 scores 0.5/61 = 0.0082, so the
  note ALWAYS wins regardless of how strong the visual match is or how
  weak the note hit. The 0.5 CLIP weight (B69: "protected by WEIGHT not
  exclusion") was a spec default explicitly flagged as "data not
  findings, the §12 golden-set eval is the named gate." This is that
  gate arriving via dogfood. Two moves, likely both: (a) re-weight —
  CLIP visual should not sit at half a note's vote when the query is
  visual; consider raising S4 or making weights query-shaped (a
  visually-descriptive query leans S4, a "what did I say about…" query
  leans S1/S2); (b) RRF's rank-flatness is itself the deeper culprit —
  a near-miss and a perfect match at the same rank score identically;
  consider a similarity-aware fusion or a score-floor so a high-cosine
  CLIP hit can't be buried under a tangential keyword brush. PAIRS WITH
  the search-as-scope UI overhaul the founder asked to start now (see
  "Lighting up M3" + the search-scope riff) — the relevance-sort and
  per-signal toggles from that design make the weighting VISIBLE and
  tunable by the user, not just an invisible constant. (Founder, June
  12 2026.)
- [x] **Backend logs to a file** — landed `6c1f44b`: fresh
  file per `tauri dev` launch (founder preferred over rotating) at
  `<app_data>/logs/photoproof.log`, installed in `lib.rs::install_logging`
  (console + truncate-on-start file sharing one env filter). Recorded
  in CLAUDE.md as the first-class debug surface. NOT done: folding the
  stray `eprintln!`s into tracing; surfacing the path in settings.
  ORIGINAL ASK:
  (founder asked; also: the
  assistant can't see runtime behavior without it): `lib.rs` installs
  a `tracing_subscriber::fmt()` to STDERR only (`info` default,
  `photoproof_core/desktop=debug`), plus scattered `eprintln!`s
  (mic.rs, pump.rs, state.rs, embedders.rs). Nothing persists, so a
  crash/jank is unreviewable after the fact. Add a file layer
  (`tracing-appender` non-blocking rolling appender) writing to the
  app-data dir (e.g. `<app>/logs/photoproof.log`, daily roll, keep N);
  keep the stderr layer for `tauri dev`. Fold the stray `eprintln!`s
  into `tracing` while there so one sink captures everything. Surface
  the log path in the debug panel / settings for "reveal in Finder."
  (Founder, June 12 2026.)
- [x] (LANDED `6d7c4fb`, merge `0722efe`; details in LANDED.md) **Full RAW decode (1:1 preview) — PLAN WRITTEN `docs/PLAN-RAW-DECODE.md`**
  (`ffd118a`): the founder asked to build it (not just hide the count).
  Key finding — NO new dependency: rawler 0.7.2 already exposes WB
  coeffs, cam→XYZ matrix, CFA, levels; we write the develop arithmetic
  (black/scale→WB→demosaic→matrix→sRGB→gamma) as a cancellable
  `full-raw-decode` pass draining like the embedding queue. FOUNDER
  DECISIONS RESOLVED (June 12, in the plan): (1) "1:1" = FULL SENSOR
  resolution, deep-zoom like LR/darktable 100% (not just 2560px); (2)
  quality = typical neutral decode, "just need real resolution"; (3)
  memory = Lightroom's model (develop once → cache full-res artifact to
  disk → serve zoom from cache; one develop in flight, tiled-demosaic
  fallback on low RAM). (4) ON-DEMAND not eager — do NOT develop every
  RAW on ingest; develop lazily when the user opens/zooms an image in
  Look (the "ask"), cache to disk, serve from cache after. Removes the
  eager enqueue that created the 154 stuck rows. READY TO BUILD.
  ORIGINAL DIAGNOSIS:
  "154 RAWs left to decode" reads as stuck — it's an UNBUILT pass,
  not a stall: (founder: "154 raws left to decode that seem stuck").
  DIAGNOSED: `ingest_passes` has 154 `full-raw-decode` rows in state
  `pending`, `attempts=0`, no error — they were enqueued and NEVER
  claimed, because `ingest::claim_next` drains only `Exif` + `Preview`;
  `full-raw-decode` is M1.5 and has NO worker yet ("stay pending in the
  queue by design"). So nothing is broken — but the UI advertises a
  count of work that will never move until M1.5 ships, which reads as a
  hang. Fix is honesty, not a decoder (unless M1.5 graduates now): stop
  surfacing pending counts for passes that have no worker, or label
  them "available in a future version," not "left to decode." (Same
  root cause as the DNG item below.) (Founder, June 12 2026.)
- [x] (LANDED `6d7c4fb`; same root cause, resolved by the RAW decode pass above) **DNG (and other RAW) never loads a 1:1 preview** (founder:
  "Embedded preview — full decode pending… a dng file never loads
  1-to-1 preview"). SAME ROOT CAUSE as the stuck-RAW item: the 1:1
  view needs a full demosaic, which IS the `full-raw-decode` M1.5 pass
  — unbuilt, never claimed, so "full decode pending" is permanent. The
  embedded preview (the in-RAW JPEG) loads; the true 1:1 cannot until
  the decode pass exists (`preview.rs` already enqueues it at backfill
  priority and notes the CR3 HDR-PQ / chained-JPEG ladder it would
  feed). DECISION NEEDED: graduate the M1.5 full-RAW-decode pass now
  (rawler demosaic → 1:1 artifact), or make the UI stop promising a 1:1
  that won't arrive. For DNG specifically, verify rawler's DNG path and
  whether a larger embedded preview exists to show meanwhile. (Founder,
  June 12 2026.)
- [x] **Add-to-collection from the grid offers "New collection…"** — landed `589a0fd`: new `new-collection-add` thumb seat (available even at zero collections), captures targets synchronously, reuses the rail's inline name input (one create UX), runs create-then-add in order; blank name leaves nothing empty.
  ORIGINAL ASK:
  (founder: "if I right click on image(s) in grid, I want to add to a
  collection even if none exists / add to new collection"). Today the
  thumb context menu's add-to-collection only lists EXISTING collections
  (`collectionRows` over the current set); with zero collections there's
  no path, and you can't mint one from the selection. Add a "New
  collection…" item to the add-to-collection submenu that creates the
  collection AND adds the current selection in one evented step (the
  rail already has an inline "New collection…" creator —
  `SourceRail.svelte` — reuse its create path, then chain
  add-to-collection). This is also the natural feeder for the
  autosuggest/encourage-collecting thesis. (Founder, June 12 2026.)
- [ ] **Review "done work": exports-folder path + foreign edit sidecars**
  (founder, June 12 2026: "the main point of the app should be to review
  DONE work… we may want to support reading in sidecar edit files from
  Lightroom/darktable"). In TENSION with a neutral RAW develop: an edited
  RAW shown via our neutral develop looks WRONG vs the editor. Honest
  scoping (see PLAN-RAW-DECODE.md "foreign edit sidecars"): (a) FIRST-CLASS
  the export-folder review path — done work is usually exported JPEG/TIFF
  with the edit baked in, which the app already handles; cheapest, highest
  fidelity. (b) Faithful XMP/`.xmp` render = reimplementing Adobe/darktable
  = NOT feasible. (c) Pragmatic middle: read the PORTABLE subset from the
  sidecar — crop, orientation/flip, rating/label/color (and maybe basic
  exposure/WB) — approximated on the neutral develop, labeled "approximate";
  crop+orientation+rating is the high-value low-risk slice (matches the
  photographer's keep/reject intent even if tone differs). (d) Prefer an
  editor-written embedded full-res preview when present. SEPARATE from the
  develop pass — must not block it. Needs a design round. (Founder, June
  12 2026.)
- [x] (LANDED `91bfa15`, merge `e8faf55`) **Grid right-click submenus are janky** (founder: "submenus don't
  stick out the side, don't always open/close smoothly"). The whole
  context menu is `ContextMenuHost.svelte` (a 1 KB stub) — submenus
  (add-to-collection, surround, etc.) don't flyout to the side and
  open/close unreliably. Needs a real submenu implementation: side
  flyout with edge-aware flipping (open left when the right edge is
  near the viewport), hover-intent open/close with a small close delay
  so diagonal travel into the submenu doesn't dismiss it, keyboard
  arrows. Likely wants a small reusable Menu primitive rather than
  more ad-hoc positioning. (Founder, June 12 2026.)
- [x] (LANDED `d541854`, merge `10796c8`) **T cell-info should grow the cell, not overlay the image; info at
  the TOP** (founder). Today the cell-info row (`cellinfo.ts` cycled by
  T) is `position: absolute` over the bottom of the thumbnail
  (`Thumb.svelte` ~234), covering the image. Founder wants: when info
  is shown, the cell EXTENDS DOWNWARD to make room (image stays fully
  visible, info sits in its own strip below — or per the founder, info
  at the TOP of the cell). Touches the grid layout math (cell height
  becomes image + info-strip when active) and the gridlayout row-height
  calc, not just Thumb CSS. (Founder, June 12 2026.)
- [x] **No em-dashes in UI copy** — landed `ddb0e86`: 41 visible
  strings across 22 files de-dashed (spaced hyphen or clean range);
  residual dashes all non-visible (comments + the menus.ts separator
  sentinel rendering as <hr/>). Recorded as a rule in CLAUDE.md. NOT
  done: a CI grep-gate to stop creep. ORIGINAL ASK: (founder, emphatic:
  "no emdashes in the
  UI!!!"). Sweep user-VISIBLE strings (EmptyState lines, button labels,
  settings copy, station/indicator text, welcome/consent cards,
  tooltips) and replace `—` with " - " or a rephrase. ~408 `—` occur in
  the frontend but MOST are code comments — target only rendered text;
  do not touch comments or this backlog. Consider a tiny lint (grep gate
  in CI over .svelte template regions / string literals) so they don't
  creep back. (Founder, June 12 2026.)

## Founder thread, June 14 2026 - model-usage walkthrough (decisions)

Came out of a walkthrough of every ML model and where they overlap (see
`docs/RUNTIME-MATRIX.md` "Model concurrency", `docs/ROADMAP.md`). Four items, two
of them posture changes the founder made deliberately.

- [ ] **Derived summaries become VISIBLE journal entries, marked "system" and
  DELETABLE** (founder, June 14 2026: "I do want to have derived summaries be shown
  to the user and marked as 'system', which allows users to delete them if the
  system made an error instead of keeping that hidden wrong forever. All this can be
  in the journal tab."). A DELIBERATE BEND of K14 (machine prose was "retrieval fuel
  only, never user-visible"): instead of hiding machine summaries, SURFACE them in
  the journal tab with a `system` source chip and let the user DELETE a wrong one (a
  retraction on a system-authored entry) - transparency beats hidden-wrong-forever.
  Spec impact: RETRIEVAL §9 (summaries no longer invisible fuel-only), EVENTS (a
  `system` source + a user-deletable system entry; reuse retraction). Pairs with the
  summary-GENERATION work (still unbuilt, M3) and the type-chip item below. The
  derived-summary text must STILL be embedded by EmbeddingGemma into the
  `image_summary` vector (the S3 search lane already consumes it) so a visible system
  summary stays searchable - confirmed that is the existing design, just not yet
  generating. (Founder, June 14 2026.)


- [ ] **Generate tags from our data and EXPORT them INTO the image files (writes
  real file metadata)** (founder, June 14 2026: "a feature which is to generate tags
  based off our data and export them directly to the images, with clear warnings
  that this actually does change the meta-data of the actual files"). Generate
  keyword tags from what the app already knows (collections, topic-graph clusters,
  derived summaries, CLIP/embedding neighborhoods) and write them as IPTC/XMP
  keywords INTO the originals (or an adjacent `.xmp`), for use in Lightroom / Bridge
  / Finder. POSTURE CHANGE: this is the FIRST feature that writes to the user's
  originals - it knowingly breaks the strict non-destructive posture, so it MUST be
  opt-in, explicitly warned ("this modifies your actual files"), and ideally
  backup/undo-aware. RELATION: goes beyond SIDECARS §14 (a one-way XMP *sidecar*
  export that never touches files); and it REOPENS the "Won't build: metadata
  editing" line at the bottom of this file - founder-directed, but scoped to warned,
  opt-in tag EXPORT, not general in-app metadata management. Needs a design round
  (what writes, where, how warned, reversibility). (Founder, June 14 2026.)


- [~] **CUDA execution provider for the `ort` embedders (Ryzen 9900X + RTX 5080
  desktop)** - VALIDATED June 14 2026: the FP16 CLIP runs on the 5080 at **54.47x**
  over CPU (2259 vs 41 img/min, near-lossless cosine 0.9998). The per-model NVIDIA
  gating (`OrtEmbedder::clip` -> `Accel::Nvidia` -> the TensorRT/CUDA ladder, behind
  the `cuda`/`tensorrt`/`cuda-dynamic` features) + the `cuda_spike` harness are
  committed. The Blackwell (sm_120) blocker (no prebuilt onnxruntime kernels) was
  solved with the official **cuda13 onnxruntime tarball** (real sm_120 SASS) loaded
  via `ort/load-dynamic` + `ORT_DYLIB_PATH` (recipe + result in
  `docs/PLAN-ORT-BLACKWELL.md`). The TensorRT EP rung ALSO validated: **85.79x**
  (3635 img/min, +1.58x over CUDA, cosine 0.99994) with TensorRT 10.16.1
  (`pip 'tensorrt-cu12<11'`, ships the sm120 builder resource). REMAINING: (a) wire
  `ORT_DYLIB_PATH` + `LD_LIBRARY_PATH` + the `cuda-dynamic` build into the desktop app
  launch on NVIDIA (the analog of the macOS CoreML flip); (b) re-measure EmbeddingGemma
  on CUDA/TensorRT (whole-graph, unlike CoreML); (c) the decode-pool + batching levers
  (the bottleneck moved to decode). The 5080 can also run a higher tier (bigger LLM +
  Gemma 4 MTP, see PLAN-GEMMA-MTP). (Founder, June 14 2026.)

- [ ] **Cross-platform GPU path for the `ort` embedders (non-Apple, non-NVIDIA)**
  (founder, June 14 2026: "and Vulkan too") - CORRECTED framing, see `docs/PLAN-VULKAN.md`:
  ONNX Runtime has NO Vulkan EP and never shipped one, so "raw Vulkan" is the wrong
  target. The real answers: (a) **DirectML EP** (Windows, any DX12 GPU - AMD/Intel/NVIDIA)
  - cheapest near-term win, exists in `ort` behind the `directml` feature, runs FP16 (not
  int8 - matches our split exactly), consumes the SAME single-file FP16 model, mirrors the
  CoreML/CUDA gating; Windows-only. (b) **WebGPU EP** (Dawn -> Vulkan on Linux / DX12 on
  Win / Metal on Mac) - the strategic ONE-EP-for-all-non-Apple/NVIDIA bet, exists in `ort`
  (`webgpu` feature), op coverage looks right, but younger/needs a real-hardware spike
  before shipping. (c) ncnn / Burn-wgpu / vendor EPs (OpenVINO-Intel, MIGraphX-AMD) only
  as a last resort for Linux-AMD - each costs a conversion away from ONNX. Sequencing:
  DirectML when the Windows bucket opens -> WebGPU-EP spike in parallel. Lower priority
  than the M1 (done) + 5080/CUDA (in progress) targets. (Founder, June 14 2026.)

- [~] **The GPU embed moved the bottleneck to DECODE - two levers** (founder, June 14
  2026, after the 5080 hit 54x): now that CLIP image embedding is GPU-fast (54x on the
  5080, 8.77x on the M1 CoreML), decode/resize is the new ingest ceiling. Decode IS
  parallel today (rayon pool `min(cores,8)`, `library/mod.rs`; BLAKE3 `min(cores,8)`)
  but two wins are now worth it: (a) **re-bench the `min(cores,8)` decode-pool cap** - it
  was tuned on the M1 (`preview.rs` "more workers thrash the cache"); on the 9900X
  (12c/24t) feeding a 54x GPU it likely STARVES the GPU, so make it per-machine/tunable
  and re-bench on the desktop; (b) **BATCH the GPU embed** - `OrtEmbedder::embed_image`
  does ONE image per forward pass; GPUs love batches (the MLX text spike saw ~24x batched
  vs single-item), so batching the CLIP visual tower (16-32/forward) could beat even the
  TensorRT +1.5x. These are the next perf frontier on capable GPUs. (Founder, June 14 2026.)
  - LEVER (a) LANDED June 14: the ingest pool cap is now env-overridable via
    `PHOTOPROOF_INGEST_WORKERS` (`ingest_pool_size()` in `library/mod.rs`; default UNSET =
    the prior `min(cores,8)`, byte-for-byte). Re-tune on the desktop with the `#[ignore]`
    `bench_ingest_pool_width` harness (`PP_BENCH_DECODE_DIR=/jpegs cargo test -p
    photoproof-core -- --ignored --nocapture bench_ingest_pool_width`), which sweeps
    candidate widths over a real JPEG folder and prints img/s. Graduate the winning value
    to a config FIELD (pairs with the CoreML env->field graduation).
  - LEVER (b) BLOCKED on a model re-export, NOT a code change: the shipped DFN5B visual
    ONNX (BOTH the int8 external-data tower AND the FP16 single-file tower on CoreML/CUDA)
    has its batch dim FIXED at 1 - the `image` input is `[1,3,378,378]` (concrete `1`, no
    dim_param) and the PyTorch trace baked that `1` into ~350 internal Reshape constants,
    so ORT rejects any other batch size (verified June 14 against the on-disk graph
    metadata + asserted by the `#[ignore]` real-model test
    `visual_tower_is_batch_one_so_batching_needs_a_reexport`). FOLLOW-UP: re-export the
    visual tower with `dynamic_axes={"image":{0:"batch"}}`, re-run the COCO eval to confirm
    retrieval-safety, THEN add `OrtEmbedder::embed_images(&[DecodedImage]) -> Vec<Embedding>`
    (one `[N,...]` forward pass) + batch the image-embedding pass (16-32/wave). The note on
    `run_clip_image` (`ort_embedder.rs`) records this; the test flips loud when a dynamic
    re-export lands. Single query path stays one-at-a-time regardless.

- [ ] **First-run onboarding flow: "optimizing for your hardware" + guided setup**
  (founder, June 14 2026: "on our welcome screen we should explain / walk through initial
  setup... we probably want a whole flow"). Today there is a welcome card + a consent gate
  for model download, but not a guided FIRST-RUN FLOW. Build one that: (1) welcomes +
  explains the local-first thesis (your photos never leave; models run on-device); (2)
  **DETECTS the hardware and SHOWS it** - "optimizing for your Apple M1 Pro: CoreML / your
  RTX 5080: CUDA / no GPU detected: CPU" - turning the intelligent-detection matrix
  (`docs/RUNTIME-MATRIX.md`) into a visible, reassuring moment; (3) walks the model-download
  consent (sizes, the license gates, what each model enables, what works WITHOUT models =
  the Tier-0 floor); (4) the first-folder / watched-root add; (5) progress while the library
  digests (ties into the "Digest visibility" item). Should gracefully convey "you'll get
  the best your hardware allows" without jargon. A real design round - it is the user's
  first impression + the place the hardware intelligence becomes legible. Pairs with the
  Station / progressive-import / digest-visibility items. (Founder, June 14 2026.)

## Visualizer + state-integrity follow-ups (founder, June 15 2026)

Context: this session reworked the semantic visualizer end to end and landed a
batch of state-integrity / self-heal fixes. Full session narrative + file
pointers in `docs/HANDOFF.md`; audit in `docs/STATE-INTEGRITY-AUDIT.md`. LANDED
this session (see git, `Visualizer:` + state-integrity commits): bounded/annealed
force sim that always settles; semantic CLIP+note k-NN spring attraction (alike
photos draw together); rebalance + live-tunable knobs (`graph.attraction`,
`graph.neighbor_attraction`, `graph.neighbor_rest_length`); a live topic-strength
slider; "soft topics" unified INTO the Overlooked lens (detect unnamed coherent
clusters via `synthesis.ts unnamedClusters`, list + glow them). Open items below.

- [ ] **Soft-topic v2: dogfood the force balance + tune defaults** (founder,
  June 15 2026): the ghost-anchor + promote-to-topic feature LANDED (`acc74c8`,
  see LANDED.md). Remaining is the numbers pass: restart `tauri dev`, work the
  topic-strength slider + `tuning.toml` (`[graph] attraction /
  neighbor_attraction / neighbor_rest_length / repulsion`), and tune the
  defaults from the feel. Architecture is settled; this is dials.

- [ ] **Host the fp16 CLIP + re-pin the manifest** (founder, June 15 2026 — ops):
  the fp16 single-file CLIP is a NOMINAL/unhostable manifest entry (the immich-app
  `local-fp16-convert` revision 404s). It was regenerated on margo, staged
  locally, and registered in `installed.json`; the embedder-bypass (fp16 ->
  installed compatible model) covers fresh machines for now. TO MAKE IT
  DOWNLOADABLE: host the 3 files (on margo at `~/fp16-convert/dfn5b-fp16/`) then
  re-pin the fp16 entry in `crates/photoproof-core/src/runtime/manifest.rs` with
  the real repo + revision + SHAs. SHAs already computed: visual `06554df3…`,
  textual `8617a89a…`, tokenizer `6d9109cc…`. margo scratch (~6 GB at
  `~/fp16-convert/`) can be cleaned once hosted.

## Performance / SOTA (audit June 13 2026)

Cited gap analysis (our stack vs 2025-2026 SOTA, adversarially verified). The
findings live in **docs/PERF-AUDIT.md**; the dependency-ordered build plan (where
each lands, exact API, effort/risk/win/validation) is **docs/PLAN-PERF.md**.
Ordered by the plan below. Validate the spikes; do not act on unverified
magnitudes.

- [~] **CoreML EP spike (the embedding bottleneck)** - SPIKE DONE June 14, verdict
  **SHIP-WITH-FP16** (`docs/SPIKE-COREML.md` + `crates/photoproof-connectors/tests/
  coreml_spike.rs`, merged `17255f5`). The int8 tower could not load under CoreML (the
  397-file external-data split), but an INLINED FP16 visual tower (converted from the
  Immich FP32, same lineage as our int8) LOADS and runs **8.77x** faster than CPU
  (18 -> 162 img/min), near-lossless (cosine vs CPU min 0.9956; vs FP32 min 0.99998).
  MLProgram + CPUAndNeuralEngine. ONE caveat: a ~16.5 min FIRST-LOAD compile, so
  production must set `.with_model_cache_dir(...)`. CODE WIRING LANDED June 14: the
  CoreML compiled-model cache (`.with_model_cache_dir`, beside each tower) + the
  `...__dfn5b-fp16` model spec (`ort_embedder.rs`/`model_specs.rs`, gate green, CPU
  default byte-identical) - so the env-knob CoreML path now compiles once not per
  launch, and the fp16 id is buildable by the eval rig. EVAL HELD + FLIPPED ON THE
  M1 PRO June 14: COCO-1k nDCG 0.8212 (fp16/CoreML) vs 0.8225 (int8/CPU), R@10 up,
  MRR within 0.3% - retrieval-safe. Per-model CoreML gating
  (`OrtEmbedder::clip`, macOS + `-fp16` only; int8/text stay CPU) + the fp16
  `manifest.rs` entry are committed; this machine's `installed.json` + `config.toml`
  select fp16, so the desktop app runs CLIP on CoreML (re-embeds the library under
  the fp16 space on next launch; revert = delete config.toml). REMAINING for ALL
  users: (a) HOST the fp16 files at a real URL + re-pin (it is locally converted);
  (b) graduate the env knob to a config FIELD; (c) CUDA EP for the Ryzen/5080
  desktop (`docs/RUNTIME-MATRIX.md` target-hardware). NOTE (d) text-embed on
  CoreML was SPIKED + REJECTED June 14 (0.48-0.64x slower; the EmbeddingGemma
  transformer graph does not partition to the ANE) - it STAYS int8/CPU, its best
  path (`docs/SPIKE-COREML-TEXT.md`, `coreml_spike_text.rs`). ORIGINAL: we run
  `ort` CPU-only; enable ONNX Runtime's CoreML EP (MLProgram, NOT legacy
  NeuralNetwork which casts FP16 and can flip predictions). Immich shipped this in
  v2.2.0 (PR #17718).
- [ ] **Visualizer off main thread, then WebGL** (WKWebView check: GO - the probe
  + `docs/SPIKE-WKWEBVIEW.md` landed; Workers/WebGL2 universal, OffscreenCanvas on
  Sonoma 14+; confirm via the startup `webviewcaps` console line on the target Mac).
  The graph sim is all-pairs O(N^2) Canvas-2D on the MAIN THREAD (sustains ~5k
  nodes). Interim: move the existing sim into a Web Worker so it stops blocking.
  Full: WebGL render (Sigma.js) + GPU/Barnes-Hut O(N log N) layout (cosmos.gl scales
  to 1M+). Pairs with the existing graph-perf work.
- [ ] **Off-main-thread thumbnail decode** (small/optional; WKWebView check GO -
  Workers + createImageBitmap universal). CORRECTION from the recon: the grid is
  ALREADY virtualized (`gridlayout.ts` visible-window + DOM pool) and `Thumb.svelte`
  already uses `<img decoding="async">`, so this is a control upgrade, not a fix.
  Optional: `createImageBitmap` in a Worker. Do only if scroll-decode jank is
  actually measured. (See P7 in PLAN-PERF.md.)
- [ ] **USearch HNSW at scale** - DEFERRED, scale-triggered. Brute-force int8
  MRL-512 is CORRECT now (negligible vs HNSW under ~100k per arXiv 2409.06464).
  Trigger: when a library crosses ~tens of thousands of images, benchmark the
  M-series brute-force scan against the <100ms contract and adopt USearch HNSW
  (int8 274k QPS vs 171k f32 @ 98.9% recall@1) if needed. (The "~10x past 1M"
  justification was refuted 1-2.)

## Next polish round (small, founder-requested)

- [ ] **Voice chunking tuning** — first live run (June 2026) works end to
  end ("it is making finals and saving notes"), but utterance
  segmentation needs a deliberate tuning round against real dictation.
  The knobs, all in one place so the round is empirical, not archaeology:
  (a) server-side endpoint rules in `pp-asr-server` — rule2 1.2 s
  trailing silence after decoded speech (the main "when does a sentence
  end" feel), rule1 2.4 s, rule3 20 s max utterance; (b) the engine's
  `TRAILING_SHIP_MS` 3 s ship window (must stay > the rules it feeds);
  (c) silero hang `HANG_WINDOWS` 15 x 32 ms = 480 ms (gate flap vs
  intra-sentence pauses) and ENTER/EXIT 0.5/0.35 thresholds; (d)
  `asr.chunk_ms` config (160 ms default — latency vs throughput).
  Consider whether consecutive finals within a short gap on the SAME
  scope should merge into one journal entry (a capture-policy question,
  not a knob). THE TOOL EXISTS: `pp_voice_bench` (synth + run modes, all
  knobs as flags, --json for sweeps) — first sweeps bracket rule2
  between 0.6 (over-splits intra-sentence pauses) and 1.2 (merges 0.8 s
  thought-pauses); real tuning needs founder dictation clips (drop wavs
  in gitignored test-corpora/voice/). The harness's first catch — the
  engine's FIFO onset-association binding text to the WRONG onset when
  VAD and ASR disagree on segment count — is FIXED (B72: proximity
  association + merged-onset retirement + one stream clock,
  `8c2393b`/`6739de9`); the tuning round itself remains open. (Founder,
  first voice dogfood, June 2026.)
  TUNING ROUND 1 FINDINGS (June 12, founder-corpus-driven): cold-start
  first-word chop FIXED (engine pre-roll PRE_ROLL_MS 400, `cec8604`,
  verified on the corpus). Endpoint-tail truncation ("actually incred",
  "Kee[per]") is INVARIANT to rule2 (1.2/1.5/2.0), feed pacing
  (realtime vs fast), wire chunk size (50/160 ms), and pre-roll length
  - while flush-minted finals (disarm/Done path) always come back
  COMPLETE and raw ungated feeds through the SAME server emit full
  tails. Conclusion: something in the gated stream's content around
  the tail; NEXT FORENSIC: a --dump-shipped tee in pp_voice_bench
  (write exactly what the engine shipped to a wav; raw-feed that wav
  back - splits engine-content from server-behavior in one move).
  Mumble-zone mid-word dropouts ("fogens") are invariant to exit/hang
  knobs - likely model-level on quiet speech; quantify with the
  audiobook WER harness (below). pp-asr-server has an endpoint-grace
  mechanism (--endpoint-grace-ms + energy early-out) defaulted OFF:
  the corpus showed deferred resets clip the next word's start when
  pauses run short.
  RE-PRIORITIZED BY B74 (June 12): the truncation class root-caused to
  the export's baked-in lookahead (docs/SPIKE-ASR35.md) - the 560 ms pin
  swap supersedes further old-model pipeline forensics (dump-shipped tee
  et al now low-priority); chunking FEEL tuning (rule2, merge policy)
  remains live and applies to any model.

- [~] **Roots and subfolders: the long-practice design round** (founder,
  June 2026): MOSTLY LANDED `770fc5f` (merge `7c26126`) - see LANDED. Resolved:
  overlapping roots (decided: REFUSE nesting + navigate to the existing root,
  no double-ingest); deep-tree ergonomics (lazy expansion, filter/jump-to-folder);
  root lifecycle (archive/unarchive non-destructive via v14, moved/removed-root
  relink + `root-removed` stale already existed). STILL OPEN: (a) **group-by-volume**
  in the Folders tab (greyed offline groups) - explicitly deferred in `770fc5f` as
  "more than a small change, would reshape the row provider"; online/volume state
  is already on `RootDto`. (b) the open framing: whether the Folders tab should
  group roots by year-shaped naming, and how the collections-first philosophy
  shapes how much folder UI we even want. Pairs with the sidebar design pass.
  (Founder, June 2026.)
- [ ] **Model-landscape survey** (founder, June 2026 - periodic): the
  toolchain is modular by seam, so every block deserves a recurring
  look at the leading alternatives: ASR, VAD, LLM, image embedder, text
  embedder, reranker. docs/MODELS.md is the living matrix; refresh it
  quarterly or when a release moves the frontier (the Nemotron 3.5 day
  proved the swap evaluation costs an afternoon).
- [ ] **Nemotron 3.5 upgrade watch** (B74): trigger = sherpa-onnx Rust
  crate release with 3.5 support (runtime landed in their master June
  12; official exports live at csukuangfj2/...-2026-06-11). Then: pin
  the 560 ms int8 export, wire the per-stream language option, rerun
  the voice corpus + Alice WER STREAMED, spike-style latency/RSS
  numbers. Brings native punctuation/capitalization + 40 locales.
  PLAN WRITTEN `docs/PLAN-NEMOTRON-35.md` (June 14): go/no-go = NO-GO
  today, STAGED. Trigger UNMET - newest published `sherpa-onnx` Rust
  crate is 1.13.2 (May 14), predating 3.5 (C++ master only, ~June 12,
  PR 3671); pp-asr-server still pins 1.13.2. The 560 ms int8 export entry
  is staged in `manifest.rs` with REAL SHAs at `tiers: vec![]` (offered
  nowhere - live ASR path untouched) + a guard test. GO = a crate release
  carrying 3.5 + the language binding; then flip tiers, bump the crate,
  wire `en`/`auto`, run validation. See the plan for the full delta.
  UPDATE (June 14): 3.5 ALREADY LANDED via a DIFFERENT path - the
  `parakeet-rs` engine behind `engine-parakeet` (`docs/PLAN-NEMOTRON-35-SIDECAR.md`),
  whose §7.4 latency/RSS A/B PASSED on both machines (see LANDED). So 3.5 is
  shipping-ready today without the crate. This B74 crate-watch now narrows to
  a LATER CONSOLIDATION: if/when the k2-fsa sherpa-onnx Rust crate ships 3.5,
  evaluate retiring the younger `parakeet-rs` engine for the mature crate
  (int8, lighter RAM) - a bench-off, not a blocker.
- [ ] **Audiobook WER stress harness** (founder idea, June 2026): run a
  LONG known-transcript recording through the full pipeline - a LibriVox
  public-domain audiobook chapter (librivox.org) with its Project
  Gutenberg text. Gives three things the cards cannot: (a) word-error
  rate at scale, separating MODEL accuracy from PIPELINE truncation
  (score raw feed vs gated feed against the same transcript); (b)
  endurance - memory and drift over an hour of armed decode; (c) a
  fixed public corpus any machine reproduces. Recipe: fetch one chapter
  (solo reader, clean recording), afconvert to 16 kHz mono PCM16 into
  gitignored test-corpora/voice-long/, align the Gutenberg chapter
  text, add a WER scorer (sidecar script or a pp_voice_bench --expect
  upgrade). CORPUS FETCHED June 12: test-corpora/voice-long/ holds Alice
  ch1 (LibriVox v8 solo, 64+128 kbps -> 16 kHz wavs) + the exact
  Gutenberg transcript + caveats README; the scorer is the remaining
  piece. (Founder, June 2026.)
  SCORER LANDED `a4b9604` (June 13): `voice_wer` module + `pp-voice-bench
  --expect <transcript>` scoring RAW vs GATED feeds (gating-cost delta),
  `--json`, 10 unit tests. REMAINING: run it on the Alice corpus on the founder
  machine and read the raw-vs-gated WER delta (needs the model + gitignored wavs).
- [ ] **Import progressively: cards before hashes, previews in tiers** —
  big-folder import should SHOW something immediately: (a) discovery
  pass lists filenames and paints placeholder cards before hashing
  completes (needs a pre-identity card state — today an image exists
  only once hashed, K1; the card would carry the path until its hash
  arrives and the card re-keys), (b) a quiet per-card indicator while
  the preview builds (the previewReady placeholder is the seam — give it
  a subtle building shimmer instead of dead gray), (c) consider a
  low-res-first tier: a tiny embedded thumbnail (EXIF IFD1 ~160px) is
  readable in milliseconds even over SMB — paint it blurred-up, replace
  with the real 512px artifact when the preview pass lands. Performance
  work should be DRIVEN by pp-bench numbers (scripts/bench.sh), not
  vibes. (Founder, dogfood round 3, June 2026.)
  FRESH-INSTANCE DOGFOOD (founder, June 12, 2026) sharpened two more
  edges of the same flow — BOTH LANDED `d066fe8`: (d) instant scanning
  state — `ingestExpecting` optimistic bridge set synchronously on
  add-root/drop/rescan, cleared by the first real ingest event; the
  walk itself now reads as running (root cause was structural:
  scan_root walked the entire tree before any pass row existed, so
  `running` was false for the whole walk); (e) live discovered count —
  a per-file atomic counter on ScanOptions rides the existing
  ingest-progress channel; the empty state reads "Indexing — N
  photographs found so far…". Items (a)–(c) above (pre-identity cards,
  shimmer, low-res tier) remain open. The whole shebang remains the
  goal: add folder → instant "scanning" → live count → cards appear →
  previews fill in.
- [ ] **Stronger storage story beyond the welcome card** — the residue of
  the welcome-card item: hash-keyed sidecar recovery sweep,
  case-insensitive-filesystem rename semantics (APFS: a case-only rename
  isn't a rename; s02_2 fails on macOS today), import-time warnings on
  risky volumes. (Founder, dogfood round 3, June 2026.)

- [ ] **Full metrics suite across every pipeline stage** — when the product is feature-complete, instrument each step (ingest passes, hash/preview throughput, search latency, fold cost, capture/binding latencies, overlay render, IPC round-trips) into one coherent metrics surface (debug panel growing into a perf dashboard); founder wants "blazing fast" to be measured, not vibes. (Founder, June 2026.)

## M1.5 (scheduled concept, not yet a packet)

- [ ] Full RAW decode backfill pass (rawler/libheif worker; queue already
  knows the pass kind) — unlocks HEIC previews + RAW 1:1 zoom.
- [ ] Preview-policy settings (which previews to build/keep; LrC-style
  "build 1:1 on demand, discard after N days" knobs) — founder asked for
  exposure of these as toggles eventually.

## Milestone-attached extras (build with their milestone)

- **M2a (pencil) — P5.1 SHIPPED** (`1e06f1e`): B/E/O keys, overlay, undo/eraser, journal stroke micro-previews. The toolbar idea is ruled out for good — zero-chrome wins (U14); the old P/E/V band is retired. Review-sourced polish landed (LANDED.md) except:
- [ ] Pencil: one-euro live-stroke filter (CAPTURE §8.3 MAY) — add only if real-pen dogfood shows live wobble. (P5.1, DOGFOOD-M2.)
- **M2b (voice) — P6.1 engine (`9a5eece`) + P6.2 runtime (`fd0adc8`) SHIPPED**: sessions/scope ring/VAD-onset binding/voice pipeline/corrections/linking, mock/stub-verified (supervisor, downloads incl. byte-zero license gate, tiers, scheduler, consent card, OpenAI-compatible + sherpa-WS clients); M-key mic row still reserved — un-reserving needs the real arm path (P6.3). All eight P6.1→P6.2 wiring obligations closed by P6.2 (the items live in LANDED.md).
- [ ] M2b: hold-to-talk duality; journal-changed event (above) becomes load-bearing.
- **M3 (retrieval/collections)**: rail source-list grows collections + saved
  searches; drag-selection-to-rail filing; query-residue indicator segment
  with one-key clear; chip-creation UI (parser-driven); select-from-note ↔
  collection filing workflow chain.
- **M3 north star (founder)**: ONE unified retrieval system across all
  surfaces — toggles, filters, and sorting modes power users can configure
  precisely, over an excellent zero-config default where a quick search
  just pops the right image. Power-user depth must never tax the quick
  path (the <100 ms as-you-type budget and quiet defaults are the floor).
- **Stroke-aware retrieval (founder + design, pre-M3)**: strokes are
  already searchable via has_strokes (built), the stroke↔utterance link
  (K9 — words spoken while drawing find the stroke; provenance carries
  linked_stroke), and stroke provenance in results. NEW: (a) gesture
  semantics — classify stroke geometry (circle/X/underline/arrow) into
  searchable intent ("images I X'd out"); raw points are stored, pure
  downstream consumer. (b) region-conditioned visual embeddings — embed
  the CIRCLED CROP, not the frame: visual search conditioned on where the
  photographer's attention went. Both M3+/M4 candidates.
- **M3 additions (founder, dogfood round 2)**: the fuzzy quiet-toggle over
  metadata (camera/lens/filename, typo-tolerant) LANDED — see LANDED.md
  (additive widening below exact FTS, never default-on, lexical-lane only).
  **M3 design decision still to make**: when collections become
  browsable grids ("collection view"), does search turn contextual — e.g.
  a right sidebar scoped to the collection — instead of the full-canvas
  destination? (Tension: the right edge is reserved for journal/partner;
  founder suspects he'll want search-as-sidebar there. Decide at M3 design
  time, not before.) Full-canvas search stands until then.
- **M4 (time)**: Look bottom-edge stroke scrubber (seat reserved); journal
  timeline rendering upgrade; trajectories as an alternate grid lens.
  - **Library-wide event timeline** (founder, June 2026): a view of WHEN
    annotation activity happened across ALL folders — every event is
    db-stored with ts + session, so this is a query + rendering problem,
    no new capture machinery: sessions as spans, events as marks, click
    lands on the image/journal. Natural M4 fit (it IS the time milestone);
    consider it the journal-timeline upgrade's library-level sibling.
- **M5 (partner)**: right-edge dockable panel sharing the inspector slot;
  summon key reserved; obeys Tab lights-out unconditionally.

## Visualization lenses (founder, June 13 2026 — design docs written)

- [x] (LANDED `cbe20c2`, merge `feefde4`; details in LANDED.md) **Attention / engagement heatmap** — see `docs/DESIGN-ATTENTION-HEATMAP.md`.
  Engagement-intensity per image from capped dwell (NEW local telemetry, 60s/
  focus cap, tiered: Look-open full, grid-select far less) + annotation counts
  (stroke COUNT small; effort dropped). Grid heat-tint toggle + sort-by-attention.
  NOT gaze surveillance; dwell lives outside the journal, local-only, resettable.
- [ ] **Semantic topic-graph (v3)** — see `docs/DESIGN-SEMANTIC-GRAPH.md`. v1 +
  v2 LANDED (see LANDED.md): v1 = manual-seed topics + cheap suggestions +
  looks/said blend slider + live force layout + full-library scale spike; v2 =
  `cluster_topics` note-grounded auto-labels (deterministic k-means) + a
  full-library LOD (super-node aggregation / expand-on-click) + the v3 seam
  scaffold. REMAINING: v3 LLM topic suggestion — wire the real Gemma connector
  into the existing `suggest_topics_llm` seam (it returns `Unavailable` until
  then). ALSO OPEN (v2 founder-review): reconcile `graph.lod_threshold`
  (placeholder 1500) with the real full-library scale-spike numbers once the
  founder profiles the spike.
- [ ] **Heatmap x graph synthesis (FUTURE opportunity)** (founder, June 13 2026):
  once both exist, combine them. Two payoffs the founder named: (a) **"hot
  topics"** — overlay engagement intensity onto the topic-graph so the themes
  you actually spend attention on light up (where heat clusters in the semantic
  space); (b) **"missing themes from ignored images"** — the inverse: surface
  topic regions / image clusters with LOW engagement (high semantic coherence
  but little dwell/annotation), i.e. coherent groups of work you've been
  neglecting. The graph gives the semantic structure, the heatmap gives the
  attention field; multiplying them reveals both what's hot and what's been
  overlooked. Design round of its own once the two primitives land.
- [ ] **Compare module (4th view mode)** (founder, June 13 2026 - deferred, not
  high priority yet): a side-by-side compare view. ARCHITECTURE IS READY - it
  drops onto the `viewMode` axis (`grid|visualizer|look|compare`) as ~5 additive
  edits per the `docs/DESIGN-VIEW-MODES.md` litmus (a `ViewMode` member, one
  App.svelte render arm, an `activeHash` + `dwellRefocus` arm, an
  `enterCompare`/`leaveCompare` pair on the `openVisualizer` template + a trigger,
  a `CompareSurface.svelte`, and optionally one `scope.ts` rule). DEFAULTS already
  reasoned: 2-up side-by-side, 3-4 in a small grid, synced zoom/pan on with a
  toggle, click a pane to focus it. THE DICTATION/NOTES QUESTION the founder flagged
  ("tag the other photo's hash, similar to multiselect"): a compare note can reuse
  the existing multi-target `event_targets` (ordered by `position`, no schema
  change) - the focused pane is the subject (position 0) and the comparand(s) ride
  along tagged (positions 1+), so the note shows in both journals. THREE options to
  decide when picked up: (1) focused-primary + tagged comparand (recommended - note
  is A's, framed as "compared from A" in B's journal); (2) equal multi-target like
  multiselect (identical on both, no subject); (3) focused-only (single target, no
  comparison link). OPEN QUESTIONS raised but not resolved: whether the
  "compared from <subject>" back-reference in the comparand's journal is wanted or
  noise; one shared note across both panes vs noting each pane separately (two
  independent notes each tagging the other); how rating works (rate the focused
  pane, or a "pick this one" verb that ranks A over B); strictly 2-up vs genuine
  N-up (changes "the other hash" from singular to plural); and whether compare is a
  persistent view you return to or a transient "hold these two up" gesture. Needs a
  short design round on those before building. (Founder, June 13 2026.)

## Lighting up M3 (the semantic-search chain, in order)

- [ ] **Real embedder connector + backfill packet**: implement the
  Embedder seam against the pinned models (RUNTIME process or in-process
  ort, per spike findings), let the existing P7.1 embedding passes chew
  through the library, flip STATUS.md's mock-only retrieval rows live.
- [ ] **Spike session 2, desktop half** (needs the RTX 5080 machine):
  tier-2 throughput calibration, CUDA posture, the full RUNTIME 12.4
  concurrency matrix.
- [ ] **Golden-query retrieval eval** (post-dogfood, M3 quality gate):
  founder-built query set over his real annotated library; settles S4
  always-on weight (B69) and the reranker go/no-go. HARNESS BUILT (awaiting
  the real query set): pure IR metrics + golden-set format in
  `crates/photoproof-core/src/retrieval_eval.rs` (P@k/R@k/MRR/nDCG, unit
  tested), a CI-gated synthetic sample in
  `crates/photoproof-core/tests/retrieval_eval_sample.rs`, and the runner
  `pp-retrieval-eval` (`src/bin/pp_retrieval_eval.rs`). TO RUN THE GATE: drop
  the real query set (JSON; format documented in `retrieval_eval.rs`) at the
  gitignored `test-corpora/retrieval/`, then sweep weights, e.g.
  `cargo run -p photoproof-core --bin pp-retrieval-eval -- --db <photoproof.db>
  --queries test-corpora/retrieval/golden.json --json` and re-run with `--s4
  0.5` (etc.) to diff the metric deltas. See `test-corpora/retrieval/`.

## Collections (B71 — the M3 curation thread)

- [ ] **Collection-note composer (UI slice)**: the storage, merge rules,
  and commands (add_collection_note / collection_notes) landed with
  P7.3 - collections carry their own append-only notes, a deliberately
  separate kind from image journal events (about the grouping's intent,
  not any image). Missing: the composer - a notes area when viewing a
  collection in the rail tab, possibly a "note the collection" verb
  while its grid is open. (Founder, June 2026.)
- [ ] **Collection-level rollups from member notes (LLM)** - founder
  idea, June 2026; posture split to respect K14 ("machine prose is
  retrieval fuel only; the journal preserves YOURS"): (a) FUEL TIER,
  uncontroversial: LLM-derived collection summaries, invisible,
  search/context only - "find that melancholy series" works without
  visible machine prose; (b) NUDGE TIER: surface quiet observations
  ("seven of twelve notes here mention fog") that invite the USER to
  write the collection note - machine notices, human authors; ties into
  the encourage-collecting principle and autosuggest below. AVOIDED by
  recommendation: machine-drafted notes entering the store as content,
  even behind an accept button - search provenance would quote words the
  photographer never said. FOUNDER CALL pending on whether (b) ever
  graduates toward drafting.
- [ ] **Autosuggest collections** (founder, June 2026): the app should
  NATURALLY encourage collecting — that is the point of gathering all
  this disparate context. Beyond manual creation, propose collections
  quietly from signals the app already has: images co-annotated in one
  session, repeated phrases across voice/typed notes, time+folder
  affinity, search queries the user runs repeatedly. Surface as a quiet
  suggestion (never a modal); accepting one creates the collection with
  evented membership. Needs a design round — record signals first,
  suggest later is a legitimate v1 (the membership tables make late
  suggestions retroactively useful).

## Decided, awaiting founder appetite

- [ ] Full interface themes (light chrome + grays) — token architecture
  ready; surround-luminance shipped in P4.2 (D6).
- [ ] Configurable external editor (D4 revisit).
- [ ] Type-to-jump filename in grid (Search covers it meanwhile).
- [ ] Burst/HDR-bracket stacks beyond RAW+JPEG.
- [ ] GPS map view; histogram in Look (needs decode-pipeline access).
- [ ] Very-large grid cells served by display previews (>512px targets).
- [ ] CI pipeline (GitHub Actions: standing gate + OS-matrix sidecar
  byte-compare + nightly full-scale `#[ignore]` lane).

## Recorded, not designed (K17 — unchanged)

Future fine-tuning of a small LLM for app tasks; voice-command retraction;
audio-retention opt-in; multi-machine sync as a product feature.

## Won't build (UI-FEATURESET §8 + D3 — kept here so they stay decided)

Color labels / pick-reject flags · metadata editing · image editing ·
import/copy/move workflows · in-app deletion (D3) · multi-window/tabs ·
auto-hide chrome · keyword taxonomies (collections are intent groupings with
evented membership — "tags with time" — never hierarchical vocabularies).

NOTE (June 14 2026): "metadata editing" is NARROWLY REOPENED as an opt-in, warned
TAG-EXPORT-to-files feature (see the June 14 founder thread above) - generating
keyword tags from app data and writing them into originals for interop. That is
export, not in-app metadata management; editing existing metadata as a workflow
stays off-thesis.
