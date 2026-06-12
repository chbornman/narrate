# DOGFOOD-M1 — the founder-machine checklist

M1 ships for dogfooding here (BUILD-LOOP Phase 4). Everything headless is
already verified by the automated gate (see the traceability matrix at the
bottom); this document is the part only your machine and your eyes can do.
Work through it top to bottom and check boxes. Every step says **what to do**,
**what to look at**, and **what failure looks like**.

> **Before anything: Q1, the name.** The `.photoproof.json` sidecar suffix
> and `.photoproof-volume` markers harden into real user data the moment you
> ingest your library. Renaming is a clean find/replace *today* and a data
> migration *tomorrow*. Decide, or explicitly accept "Photoproof", before
> step 2.

## 0. Install & launch

- [ ] **Install** — either:
  - the bundle: `cargo tauri build` (from `apps/desktop/`), then install
    `target/release/bundle/deb/Photoproof_0.1.0_amd64.deb`, or
  - dev mode: `cd apps/desktop && npm ci && cargo tauri dev`
    (dev mode includes the debug panel — `F12` — which makes several checks
    below easier; the release bundle deliberately excludes it).
- [ ] **First-run flow** (UI §9.1): a fresh launch (no app data) must show the
  one quiet empty state — "Add a folder to begin" — and nothing else. No
  tour, no model talk. *Failure: any onboarding chrome, any mention of
  models/AI in the first-run path.*
- [ ] **Window title** is the app name only; no document-style titles.

## 1. Pre-flight on the real library

- [ ] **HEIC count check** (FOUNDER-CHECKLIST): count HEICs in your library —
  `find /path/to/library -iname '*.heic' -o -iname '*.heif' | wc -l`.
  HEIC previews wait for the M1.5 full-decode backfill (L5); the journal
  works on them day one, but they render as placeholders. Decide whether
  that gap matters for this dogfood before judging the grid.
- [ ] **Back up nothing, but know the blast radius**: M1 writes
  `*.photoproof.json` files beside images and one `.photoproof-volume`
  marker at each volume root. It never modifies image bytes (the watcher
  test suite enforces this, but know what new files to expect).

## 2. Ingest the real ~50k library (M1 step 9; FOUNDER-CHECKLIST bucket 2)

- [ ] **Register the library root** in Settings → Library (or the first-run
  button). Pick the real archive root, spinning disk or NVMe as you actually
  store it.
- [ ] **Watch the first minute**: the grid must start populating
  incrementally within seconds (UI §9.1 live-ingest population) — newest
  folders appearing as the scan walks, thumbnails trailing the hash pass.
  *Failure: a frozen empty grid until the whole scan finishes, or a modal
  progress dialog (there is none by design — ingest is a 2 px hairline on
  the indicator, I3).*
- [ ] **First 1k thumbnails ≤ 60 s** (L7, NVMe): eyeball with a clock from
  "root added" to "a screenful of real thumbnails everywhere you scroll in
  the newest folder". Reference numbers from the synthetic suite are in §7
  below — your real number is the one that counts (RAW embedded-preview
  extraction is heavier than the synthetic JPEGs).
- [ ] **Full ingest ≤ 90 min** (L7, 8-core NVMe; spinning archive disks scale
  with throughput — P12). Leave it running; check the debug panel's Ingest
  tab (F12) occasionally: `pending` should fall monotonically, `error`
  should stay at ~0. *Failure: errors climbing (inspect the recent-errors
  list — transient I/O retries are fine, repeated identical failures are
  not), or the queue stalling while `pending > 0`.*
- [ ] **Interrupt it once, on purpose**: quit the app (or `kill -9` the
  process — the automated harness does exactly this, but do it once on real
  hardware) mid-ingest, relaunch, confirm ingest resumes and the final
  counts settle. *Failure: duplicate grid items, images that never appear,
  or a corrupt-database error on relaunch.*
- [ ] **Real-RAW orientation fixtures** (P9; the one library-suite item that
  needs real files): find a portrait-orientation NEF (Nikon) and RAF (Fuji)
  — and any other maker you shoot — and confirm their thumbnails and Look
  views are upright. *Failure: sideways images or, the classic, EXACTLY
  upside-down/double-rotated previews (pre-rotated embedded preview +
  tag applied again).* Synthetic fixtures for both conventions pass
  (`l13_10_embedded_preview_orientation_fixtures`); real files are this
  step.
- [ ] **Preview quality on your monitor** (M1-BUILD-PLAN decision 2): 512 px
  thumbs / 2560 px display WebP. Open a dozen Look views at full screen —
  sharp enough? This is the moment to veto the sizes, before the cache is
  fully built.

## 3. Grid & Look feel (UI §13 budgets — eyeball half)

- [ ] **Grid scroll 60 fps on a 20k-item folder**: open your largest folder,
  flick-scroll hard. *Failure: blank-tile flashes that persist (recycling
  too slow), stutter, layout shifting as thumbs land.*
- [ ] **Webview memory ≤ 800 MB** while doing it (P16): watch the WebKit
  process in `htop`. *Failure: memory climbing without bound while
  scrolling (img recycling broken).*
- [ ] **Look swap < 150 ms** (`←`/`→` through a folder, cache warm): should
  feel instant, image-to-image. *Failure: visible white/black flash or a
  spinner between adjacent frames.*
- [ ] **Surface transitions < 100 ms perceived** (Grid ⇄ Look ⇄ Search,
  `Escape` strictly back one layer — I1).
- [ ] **Rail toggle feel** (P4.2 amends UI §2: the rail is a PUSH panel on
  `\`, no dwell/pin — DECISIONS U3): summon/hide < 100 ms; the grid
  re-snaps integer columns rather than overlaying.
- [ ] **Has-journal dot** (UI §3.5/§3.7, B34): annotate one image with a typed
  note → dulled-red dot appears on its thumbnail. Rate another image 0–5 →
  **no visual change to that thumbnail at all** (rating-only journals do
  not light the dot; the `has_text` fold enforces this — but confirm the
  pixel truth). Retract the note (M2a UI; for now check via search
  disappearing) — the dot must clear.
- [ ] **Dark-theme + no-saturated-red audit** (I5): scan every surface;
  the saturated pencil red must appear nowhere in M1 (the pencil ships in
  M2a). The has-journal dot is the *dulled* red.
- [ ] **Indicator** (I3): bottom-right capsule shows `● session` with nothing
  selected; selection counts update as you select; typed-note commit pulses
  < 50 ms after Enter; the pulse never obscures image pixels at fit.
- [ ] **Settings window** opens as one modest separate window; exactly four
  sections; "rebuild index" and "export" present.

### 3b. P4.2 UI build — §visual (the eyes-only half of the featureset gate)

- [ ] **Push-panel resize at 60 fps over 20k items**: drag the rail and
  inspector edges with your largest folder open — the grid re-snaps
  integer columns live. *Failure: stutter, blank tiles, or the scroll
  anchor jumping while dragging (DECISIONS U3 perf trade).*
- [ ] **Marquee feel**: drag from empty gutter — rubber-band selects; a
  drag STARTING on a thumb never marquees; Ctrl-drag adds to the
  selection; a plain gutter click clears, Ctrl-click on gutter doesn't.
- [ ] **Zoom-at-cursor on a trackpad**: pinch/scroll in Look — the point
  under the cursor must not drift on EITHER axis, including near
  letterboxed edges (the P3.x anchor bug, fixed in logic/zoom.ts). Zoom
  must persist across `←`/`→` and re-anchor (not drift) on panel resize.
- [ ] **Space triple role in Look**: at fit a tap closes; zoomed, hold+drag
  pans and a clean tap closes. *Failure: a pan that closes Look on
  release, or a wedged hold after Tab/overlay changes mid-hold.*
- [ ] **Stacks `● 2` truth**: select one collapsed RAW+JPEG cell — the
  indicator reads `● 2`; a note lands on both members (flip with `R`,
  check the journal on each). Collapse/expand is live both directions.
- [ ] **Surround legibility on real images** (D6): cycle black → white via
  backdrop right-click — selection borders, focus ring, marquee, and the
  journal dot stay readable at every level (token contrast tuning).
- [ ] **Lights-out instantaneity**: `Tab` hides rail/inspector/titlebar/
  header/filmstrip in one frame; the indicator and an open note input
  stay (DECISIONS U5); `Tab` again restores exactly what was open.
- [ ] **Toast placement**: retract a journal entry — "Retracted • Undo"
  appears above the indicator, auto-dismisses in 5 s, never stacks over
  image pixels at fit; Undo RE-STATES the content (DECISIONS U10).
- [ ] **F11 + window geometry on Linux/Wayland**: F11 round-trips; resize/
  move, quit, relaunch — geometry restores without drift with the custom
  titlebar (tauri-plugin-window-state, DECISIONS U9). *Failure here =
  switch to the manual fallback in commands/app.rs.*
- [ ] **Context-menu completeness by hand, all four seats**: thumb (Open ·
  Rate ▸ · stack toggle · Inspector ▸ · reveal/copy-path/open-default),
  gutter (select all/none · Sort ▸ · Size ▸ · Stacks ▸ · Surround ▸),
  rail folder (Open · Show in file manager · Rescan), Look backdrop
  (zoom rows · Surround ▸). Every verb shows its key hint.
- [ ] **"Copy file path" clipboard smoke** (webkit2gtk secure-context
  quirk): copy from the thumb menu, paste in a terminal — the absolute
  path arrives; on an offline volume the verb quietly does nothing.
- [ ] **Drag a folder onto the window**: the quiet confirm sheet appears;
  Esc dismisses; confirming registers the root and the folder opens with
  ingest streaming in.

## 4. Typed notes & scope discipline (CAPTURE §3–4 M1 slice)

- [ ] Select N images → indicator says N → type a note → it lands on all N
  (check each image's dot / search). Selection change while the note input
  is open cancels the transient (B35) — confirm the input clears rather
  than silently retargeting.
- [ ] Rating keys 0–5 with a multi-selection rate all selected (one event);
  with session scope (nothing selected) they do nothing.
- [ ] **Offline-volume annotation half** (bucket 2): unplug an ingested
  drive → grid still browses cached previews, images carry the ⏏ badge,
  typed notes still attach. Replug → notes flush to sidecars (check the
  file mtimes beside the images). *Failure: annotation blocked while
  offline, or duplicate events after remount.*

## 5. Search (RETRIEVAL §4)

- [ ] **Search-as-you-type p95 < 100 ms** on the real library: type a word
  you know you've noted, watch results update per keystroke (≥ 2 chars,
  50 ms debounce). The debug panel's Search tab shows the last query echo.
  *Failure: results visibly lagging typing, or the grid stuttering while
  querying (queries run off the UI thread and cancel on new keystrokes —
  the synthetic-library p95 numbers are in §7).*
- [ ] **Provenance is always your words**: every result must show the quoted
  snippet (date/session attached). *Failure: a result with no visible
  reason for matching (RETRIEVAL §13.2 — that's a bug, not a degraded
  state).*
- [ ] **Chips**: rating ≥ N, has-strokes (empty in M1 — no strokes exist
  yet), camera, date range. Chips are hard filters; combined chips narrow.
- [ ] A retracted note's text must stop matching the instant you retract it;
  same for redaction (drill below).

## 6. The three drills (do them on the real library, once each)

### 6a. Sidecar spot-checks
- [ ] Annotate an image → within ~5 s, `IMG_xxxx.jpg.photoproof.json` exists
  beside it (S3 debounce: 2 s quiet / 5 s cap). Open it: pretty-printed,
  sorted keys, your text verbatim, the image's hash embedded.
- [ ] Annotate the same image again → file rewrites (mtime bumps once);
  *annotating a different image must not touch this file* — repeated
  reconciliation passes leave converged sidecars' mtimes alone
  (`s13_11_mtime_stable_writer` is the automated half; spot-check one file
  so cloud-sync tools won't re-upload your archive forever).
- [ ] Session-level note (nothing selected) → appears under
  `<app-data>/sessions/<ulid>.photoproof.json`, not beside any image.

### 6b. Redaction drill
- [ ] Note something fake-sensitive on one image ("SECRET-DRILL-XYZZY") →
  confirm search finds it → redact it (journal panel ships in M2a; for the
  M1 drill use the debug panel or accept the automated coverage:
  `c4_redaction_scrubs_db_wal_fts_sidecars_and_search` runs the entire
  drill, byte-scanning db+WAL+sidecars). If you run it by hand:
  `grep -r XYZZY` over the app-data dir AND the image's folder must come
  back empty; search must return nothing; the journal must show a
  "[redacted]" stub (Q2), not a hole.
- [ ] The redaction toast is one of exactly three permitted toasts (I3).

### 6c. Rebuild drill (sidecars are the truth — K11)
- [ ] Quit the app. Move `<app-data>/photoproof.db*` aside (don't delete it
  on the first try). Relaunch → register the same root → let it re-ingest →
  Settings → "Rebuild index from sidecars". Every note must come back,
  word for word; search must find what it found before; ratings hold.
  *Failure: any missing/duplicated note (the automated
  `c1_ingest_annotate_search_rebuild_round_trip` asserts byte-identical
  journals and identical search results — a real-library deviation means a
  case the synthetic tree doesn't cover; capture it).*

### 6d. Relink drill
- [ ] With the app **running**: rename a noted file and move it two folders
  away (Finder/`mv`). Within ~5 s the grid shows it at the new location,
  dot intact; search still finds it. Zero re-hash (debug panel Ingest tab:
  hash counter unchanged for a pure rename).
- [ ] With the app **stopped**: move another noted file, relaunch →
  startup reconciliation relinks it the same way
  (`c2_interrupted_ingest_resumes_then_relinks_after_external_move` is the
  automated twin).

## 7. Perf reference numbers (synthetic suite, recorded at P4.1)

Automated perf smoke tests: `cargo test -p photoproof-core --release --test
m1_perf -- --nocapture` (plus `-- --ignored --test-threads=1` for the
full-scale variants). Numbers below were measured at P4.1 on the build
machine (AMD Ryzen 9 9900X, 24 threads, NVMe, Linux, release profile) —
**machine-relative**: treat them as shape, not law; your machine's numbers
are the dogfood truth. Synthetic files are small (template JPEG/PNG/TIFF);
real RAW hashing + embedded-preview extraction is substantially heavier per
file, so ingest rates here are upper bounds.

| Measurement | P4.1 reference (release) | Budget |
|---|---|---|
| 5k-event fold, post-merge, revision-heavy (D1, best-of-5) | 8.2 ms | ≤ 10 ms (I16; the spec-shape case measures 7.9 ms at P1.1) |
| Ingest 5k-file synthetic tree (D2 full) | scan 0.29 s (~17.5k files/s); hash+exif+preview drain 3.7 s (~1,350 files/s) | L7's 90-min/50k is founder-hardware + real-RAW; this is the pipeline floor |
| First 1k thumbnails, synthetic (D2 full) | 0.79 s | ≤ 60 s on founder NVMe (L7) |
| Search p95, 2k ingested files + 600 notes (D3) | p50 0.47 ms / p95 0.62 ms | < 100 ms (RETRIEVAL §13.1) |
| Search p95, 50k ingested files + 5k notes, real paths/previews joined (D3 full) | p50 2.90 ms / p95 3.55 ms / max 4.74 ms | < 100 ms (RETRIEVAL §13.1) |
| 1.18M-row FTS corpus (P3.1 suite, heavier text load) | p50 9.3 ms / p95 36 ms (coordinator-verified at P3.1) | < 100 ms |
| Rebuild-from-sidecars, 50k events / 2k sidecars (D4 full) | sidecar flush 1.75 s; rebuild+derived 5.80 s; re-ingest 1.52 s | the §6c drill, automated |
| kill -9 harness (C3): 6 randomized kills over a 350-file ingest | converges in ~6 s wall; integrity ok every round | no duplicates / no misses / no corruption |

## 8. Deferred from this checklist (tracked elsewhere)

- macOS / Windows volume identity + real OS cloud-placeholder flags —
  needs a real Mac/Win machine (FOUNDER-CHECKLIST bucket 2; seams tested).
- Cross-OS sidecar byte-compare — CI OS-matrix job (SIDECARS §13.6; bytes
  are platform-independent by construction, Linux-asserted).
- Pencil, voice, retrieval quality — M2a/M2b/M3 gates, not M1.

---

## M1 traceability

Every M1 acceptance criterion maps to (a) a named passing test in the
automated gate, or (b) a checklist step above. Convention:
`<file>::<test>` under `crates/photoproof-core/tests/` unless noted.
`§6x` / `§N` references are steps in this document.

### EVENTS §11 — integrity invariants

| Criterion | Test |
|---|---|
| I1 append-only | `invariants_events.rs::i01_append_only` |
| I2 scrub is minimal | `invariants_events.rs::i02_scrub_is_minimal`; e2e: `m1_e2e_redaction.rs::c4_…` (keeper survives) |
| I3 round-trip | `invariants_events.rs::i03_round_trip`; full-stack: `m1_e2e_roundtrip.rs::c1_ingest_annotate_search_rebuild_round_trip`; drill §6c |
| I4 relink binds by hash | `invariants_events.rs::i04_relink_binds_by_hash`; full-stack: `m1_e2e_resume_relink.rs::c2_…`; drill §6d |
| I5 canonical stability | `invariants_events.rs::i05_canonical_stability` + `canonical_vectors.rs` (§4.3 byte-exact vectors) |
| I6 merge is a set | `invariants_events.rs::i06_merge_is_a_set` (+ `i06_within_batch_*`, `merge_order_shuffle_property`) |
| I7 derived = fold | `invariants_events.rs::i07_derived_equals_fold`; `m1_core_api.rs::a1_has_text_clears_on_redaction_and_i7_rebuild_agrees` (new `has_text` column) |
| I8 redaction supremacy | `invariants_events.rs::i08_redaction_supremacy`, `i08_small_merge_scrub_truncates_live_wal`; e2e byte-scan: `m1_e2e_redaction.rs::c4_…`; drill §6b |
| I9 rating fold | `invariants_events.rs::i09_rating_fold` |
| I10 folds terminate | `invariants_events.rs::i10_folds_terminate`, `i10_cycles_across_batches_self_cycle_and_redaction` |
| I11 dedupe | `invariants_events.rs::i11_dedupe`; e2e: `c1_…` (multi-target rebuilt to one row) |
| I12 retraction folds out | `invariants_events.rs::i12_retraction_folds_out` |
| I13 order discipline | `invariants_events.rs::i13_order_discipline` |
| I14 monotonic mint | `invariants_events.rs::i14_monotonic_mint` |
| I15 scrubbed shape | `invariants_events.rs::i15_scrubbed_shape` |
| I16 fold reads batched + 5k timing | `invariants_events.rs::i16_fold_reads_batched`, `i16_fold_5k_events_timing`; post-merge: `m1_perf.rs::d1_fold_5k_events_post_merge_under_budget` |

### SIDECARS §13

| Criterion | Test |
|---|---|
| 1 round-trip (byte-identical) | `sidecars_acceptance.rs::s13_01_round_trip`; full-stack `c1_…`; drill §6c |
| 2 latency ≤ 5 s under bursts | `sidecars_acceptance.rs::s13_02_latency_under_sustained_bursts`; spot-check §6a |
| 3 crash safety (kill-during-write ×1000) | `sidecars_acceptance.rs::s13_03_kill_during_write`; process-level: `m1_e2e_kill9.rs::c3_kill9_process_harness` |
| 4 redaction propagation (offline + stale restore) | `sidecars_acceptance.rs::s13_04_redaction_propagation_offline_volume`; e2e `c4_…`; drill §6b |
| 5 unknown-field preservation | `sidecars_acceptance.rs::s13_05_unknown_field_preservation` |
| 6 determinism | `sidecars_acceptance.rs::s13_06_byte_determinism` (Linux); cross-OS byte-compare → CI matrix (ledger, §8) |
| 7 multi-target dedupe | `sidecars_acceptance.rs::s13_07_multi_target_dedupe` |
| 8 collision safety | `sidecars_acceptance.rs::s13_08_collision_safety` |
| 9 overflow migration | `sidecars_acceptance.rs::s13_09_overflow_migration` |
| 10 re-match (full §10.3 table) | `sidecars_acceptance.rs::s13_10_rematch_*` (6 tests) |
| 11 mtime-stable writer | `sidecars_acceptance.rs::s13_11_mtime_stable_writer`; kill-harness convergence (`c3_…` SkippedIdentical); spot-check §6a |

### LIBRARY §13

| Criterion | Test |
|---|---|
| 1 relink, running | `library_acceptance.rs::l13_01_relink_running`; drill §6d |
| 2 relink, stopped | `library_acceptance.rs::l13_02_relink_stopped`; e2e `c2_…`; drill §6d |
| 3 interrupt/resume | `library_acceptance.rs::l13_03_interrupt_resume` (in-process); **process-level kill -9: `m1_e2e_kill9.rs::c3_kill9_process_harness`** (closes the FOUNDER-CHECKLIST bucket-3 item); §2 manual interrupt |
| 4 duplicates | `library_acceptance.rs::l13_04_duplicates`; synthetic dup pairs asserted in `c1_…`/`c3_…` via `unique_hashes` |
| 5 in-place overwrite | `library_acceptance.rs::l13_05_in_place_overwrite` |
| 6 volume remount, new mount point | `library_acceptance.rs::l13_06_volume_remount_new_mount_point` |
| 7 marker precedence | `library_acceptance.rs::l13_07_marker_precedence` |
| 8 offline browsing | `library_acceptance.rs::l13_08_offline_browsing` (automated state half); visual half → §4 |
| 9 root removal | `library_acceptance.rs::l13_09_root_removal` |
| 10 orientation correctness | `library_acceptance.rs::l13_10_orientation_correctness`, `l13_10_embedded_preview_orientation_fixtures` (synthetic conventions); real NEF/RAF → §2 |
| 11 threshold routing | `library_acceptance.rs::l13_11_threshold_routing` |
| 12 reconciliation budget | `library_acceptance.rs::l13_12_reconciliation_budget` (+ `_50k_full` `#[ignore]`, release-verified at P2.2) |
| 13 exclusions | `library_acceptance.rs::l13_13_exclusions` |
| 14 clock shift | `library_acceptance.rs::l13_14_clock_shift` (+ `_10k_full`, `_sample_mismatch_aborts`) |
| 15 placeholder files | `library_acceptance.rs::l13_15_placeholder_files` |

### RETRIEVAL §13 — M1 items

| Criterion | Test |
|---|---|
| 1 M1 latency p95 < 100 ms | `m1_perf.rs::d3_search_p95_on_ingested_synthetic_library` (+ `d3_search_p95_50k_full`, real previews/paths joined); `search_latency.rs::latency_smoke_search_as_you_type_on_1m_row_corpus`; real-library eyeball → §5 |
| 2 provenance always present | `search_acceptance.rs::quotes_come_from_folded_text`, `filter_only_browse_orders_by_capture_date_descending`, `has_strokes_browse_carries_stroke_provenance`; visual → §5 |
| 5 retraction/redaction instant | `search_acceptance.rs::retracted_and_redacted_text_absent_immediately`; e2e `c4_…` |
| 6 worked example §7.2 | `search_acceptance.rs::worked_example_7_2_fog_ba_with_rating_chip` (B31 erratum noted) |
| 7 multi-target correctness | `search_filters.rs::image_scoped_filters_apply_per_joined_target` |
| 8 rebuildability (M1 slice: FTS + derived) | `m1_e2e_roundtrip.rs::c1_…` (identical search results after wipe+rebuild); `store_api.rs::fts_tracks_effective_text_per_chain_root` |
| 10 fuel-only invariant (M1 slice) | structural: no M1 command returns derived text (none exists); `apps/desktop/scripts/assert-release-clean.sh` asserts the release surface |
| 11 M1 query plan shape | `search_plan.rs::plan_match_resolved_in_materialized_cte`, `snippet_cost_bounded_by_limited_hit_set` |
| 3, 4, 9, 12, 13 | M3 scope (parse fallback, hallucination firewall, collections, PPVEC, eval harness) — not M1 criteria; tracked in BUILD-LOOP Phase 7 |

### UI §13 budgets + M1 surface

| Criterion | Verification |
|---|---|
| Grid scroll 60 fps / 20k | §3 (eyeball; virtualization logic under vitest) |
| Look swap < 150 ms | §3 |
| Search keystroke→results < 100 ms | §5 + RETRIEVAL 13.1 tests above |
| Typed note → pulse < 50 ms | §3 (commit path is a single local append; `state.rs` tests) |
| Journal panel < 100 ms | M2a (panel ships with the pencil packet) |
| Rail summon < 100 ms, no reflow | §3 |
| Surface transitions < 100 ms | §3 |
| Scope/selection/keyboard logic | `apps/desktop` vitest suite (95 tests: scope→write-binding, selection, keyboard map, search debounce) |
| Debug panel absent from release (I6) | `apps/desktop/scripts/assert-release-clean.sh` (frontend bundle + Rust binary markers) |
| Session lifecycle (CAPTURE §2 M1 slice) | `apps/desktop` `session.rs` + `state.rs` tests (idle boundary, crash recovery via `EventStore::open_sessions`/`last_event_ts`) |
| Has-journal dot semantics (B34) | `m1_core_api.rs::a1_journal_stats_is_batched_and_rating_only_has_no_text`, `a1_has_text_clears_when_all_remarks_retract_but_rating_survives`, `a2_list_folder_returns_direct_children_with_badges`; pixel truth → §3 |
