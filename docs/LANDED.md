# LANDED — shipped from the backlog

The archive half of BACKLOG.md: every `[x]` item moves here verbatim once it
ships, commit hashes, root causes, and founder context intact — this is the
de facto changelog of backlog-sourced work. Open work stays in BACKLOG.md;
this file only grows. Organized by era, newest first; older entries keep
their original wording.

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
