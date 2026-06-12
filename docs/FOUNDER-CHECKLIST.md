# Founder Checklist — running list

Living document, updated by the coordinator every packet. Three buckets:
decisions awaiting Caleb, verification only Caleb's machine can do, and
items deferred to CI/later milestones. The *rationale* for resolved
decisions lives in spec/DECISIONS.md; this file is the action list.

## 1. Decisions awaiting you

- [x] **Q1 — product name.** DECIDED (founder, June 2026): "Photoproof"
  stands for now (B70). The `.photoproof.json` sidecar suffix and
  `.photoproof-volume` markers harden as the founder dogfoods; a future
  rename becomes a sidecar-format migration, not a find/replace.
- [ ] **B22 — append of a redaction-condemned id is rejected** (`CondemnedId`
  error) rather than silently inserted in scrubbed form. Integrity-
  conservative reading; confirm or veto.
- [ ] **B23 — blocked WAL-hygiene checkpoint surfaces as an error**
  (`CheckpointBlocked`; the write is already durable, `maintain()` heals).
  The alternative was silent background retry. Confirm or veto.
- [ ] **Thumbnail/preview cache sizes** (early build-plan decision 2): 512 px
  thumb / 2560 px display WebP implemented per spec; worth a quick look at
  real previews on your monitor during dogfood before declaring final.
- [ ] **Q4 — M3 gates** (later): sentiment quality evaluation; dedicated
  text-embedder benchmark during dogfooding.
- [ ] **B31 — RETRIEVAL §7.2 erratum**: FTS5's real bm25 orders the worked
  example's image Z before X (the spec's expected order is mathematically
  unreachable). Tests assert engine truth; confirm and the spec text gets
  fixed in a future edit.

- [ ] **U1 / U5 sign-off (P4.2)**: U1 — Space now opens/closes Look;
  selection-toggle moved to Ctrl+Space (UI.md §3.4 amendment). U5 — the
  indicator and an open note input stay visible under Tab lights-out
  (coordinator ruling). Veto either and it's a small remap.

- [ ] **U14 sign-off (P5.1)**: pencil toggle moved P→B per spec; Space-tap
  at fit no longer closes Look while pencil is on; no pencil toolbar ever
  appears (zero-chrome reading — Pencil/Overlay live on the look-backdrop
  context menu instead); pencil undo is keyboard-only (Ctrl+Z; the journal
  panel's Retract row is the pointer path). Veto any of it and it's a
  registry remap.

- [ ] **Tier-gate calibration (P6.2 finding; spike session 1 adds data)**:
  your RTX 5080 reports 15.92 GiB largest Vulkan heap — 85 MiB under the
  §6.2 "≥ 16 GB" Tier-2 gate → detects Tier 1. Spike data (M1 Pro 16 GB):
  E4B Q4_K_M = 6.7 GB at 16k ctx + ASR 1.1 GB runs but compresses memory →
  the model set FITS in ~8 GB; a 15.92 GiB GPU clearly carries Tier 2.
  Recommendation: gate at ≥ 15.5 GiB (headroom tolerance). Confirm and
  RUNTIME §6.2 gets the number; 5080-measured VRAM lands in session 2.

## 2. Founder-machine verification — **docs/DOGFOOD-M1.md is the script**

Platform note: the NVIDIA+Wayland WebKit crash is handled in-app now
(`b142477`); no env var needed. Dev builds generate previews at full speed
since `e60cb15` (optimized deps).

- [x] **First live voice run (P6.4, MacBook)** — DONE (June 2026): spoke
  over a selected image, finals minted, journal entries saved. The run
  flushed out and fixed: the 416 complete-part resume bug, the starved
  endpointer (trailing-silence shipping), the stranded disarm drain, and
  dev-binary debug commands. Follow-up lives in BACKLOG "Voice chunking
  tuning".
- [ ] **Pencil feel + pressure (P5.1)** — **docs/DOGFOOD-M2.md is the
  script**: live-stroke latency, marks-zoom-with-image feel, pressure per
  platform (Wacom needs "Use Windows Ink" ON; your Linux stylus is the
  open question), eraser radius + 12-px floor, cursor visibility, pen-up →
  pulse < 50 ms.
- [ ] **Real 50k RAW library ingest** (M1 step 9) — resumability, perf
  budgets (≤90 min NVMe target, first 1k thumbs ≤60 s), preview quality.
- [ ] **Real-RAW orientation fixtures** — portrait-orientation Nikon/Fuji
  (and any other makers you shoot) RAWs through the rawler embedded-preview
  extractor; verify no double-rotation (P9: makers pre-rotate
  inconsistently). Synthetic fixtures pass; real files pending.
- [x] **macOS volume identity** — DONE (dogfood round 3, June 2026): real
  probe via getmntinfo + getattrlist(ATTR_VOL_UUID), firmlink-aware path
  binding, §4.1 level-3 heuristic fallback (B63). Windows volume-serial
  recipe still behind its seam — defer until Windows is a target.
- [ ] **APFS case-insensitivity ruling** — s02_2 (case-only rename
  relinks sidecar) fails on macOS: a case-only rename is NOT a rename on
  default APFS. Decide the semantics (detect fs case-sensitivity and
  branch, or treat case-only renames as same-file on insensitive
  volumes); until then s02_2 is the one known-red test on macOS.
- [ ] **Wave-2 eyeball pass (macOS)** — only you can judge: rounded
  corners + traffic lights (Overlay titlebar; check the lights hide/show
  with Tab lights-out), the welcome card copy + first-run feel, the
  search entry-overlay dim + results-canvas expansion, the Lucide icon
  pass (sizes/tone vs the old glyphs; ⏏ became Unplug), and RAW deep
  zoom on your real ARWs (full-res embedded JPEG should serve now —
  Sony chained-IFD sweep; run "Rebuild previews…" on the folder first to
  pick up full-size artifacts).
- [ ] **Rail Collections tab (B71 first slice) — eyeball pass**: the rail
  now has Folders/Collections peer tabs. Check: tab strip feel + persistence
  across restarts; the inline "New collection..." footer (Enter commits,
  Esc cancels); shelved/done rows dimmed but visible; opening a collection
  drives the grid (title follows the name, members list live when
  membership changes); "Add to collection" / "Remove from collection" on
  the thumb context menu with membership checkmarks, acting on the whole
  multi-selection; a member whose file is gone shows offline-badged rather
  than disappearing (membership outlives files). Known limit: members
  union-merged from another replica but never ingested here are skipped.
- [ ] **Search behavior after P7.2 — confirm nothing regressed**: hybrid
  search is ONE code path and with no models pinned it must feel exactly
  like M1 (asserted byte-equal in tests; your fingers are the second
  check). Collection chips work fully degraded — fuzzy name match, active
  collections win ties; an unresolvable chip is now a HARD error, not a
  silent whole-library browse. Semantic ranking (RRF fusion, voice-quote
  provenance) stays dormant until embedders are pinned (spike session 2);
  the debug panel shows per-signal ranks and dropped-clause reasons when
  it wakes.
- [ ] **Offline-volume browsing, visual half** — unplug an ingested drive,
  confirm grid still browses cached previews and annotations queue.
- [ ] **Visual/UX feel of the running app** (P3.2 shipped; install
  `target/release/bundle/deb/Photoproof_0.1.0_amd64.deb` or `cargo tauri dev`):
  grid at 60 fps on 20k items with webview memory ≤ 800 MB, Look swap
  < 150 ms, indicator pulse < 50 ms, rail dwell/pin feel, indicator never
  obscuring pixels at fit, dark-theme + no-saturated-red audit, first-run
  flow with a real folder, live-ingest incremental grid population, settings
  window, window titles.
- [ ] **HEIC count check**: previews for HEIC/HEIF wait for the M1.5
  full-decode backfill (B-deferred; journal works day one). Count HEICs in
  your library to decide whether that gap matters for your dogfood.
- [ ] **P6.3 model spike** (Phase 6, yours by design): real llama.cpp +
  sherpa-onnx + silero-vad recipes, token-timestamp/VAD-onset measurement,
  VRAM tier numbers. Until then voice ships mock-verified.
- [ ] **Retrieval quality** (post-M3): golden-query eval set (~50–100 pairs
  from your real annotations), reranker go/no-go (P8).

## 3. Deferred / CI

- [ ] **Cross-OS sidecar byte-compare** (SIDECARS §13.6) — bytes are
  platform-independent by construction; assert via CI OS-matrix job when CI
  exists.
- [ ] **CI pipeline itself** — the standing gate (fmt/clippy/test debug+release)
  currently runs only on the coordinator's machine; lift into GitHub Actions.
- [ ] **Full-scale `#[ignore]` tests** (50k-scan, 10k clock-shift, hash
  throughput) — verified once in release here; wire into a nightly/manual CI
  lane rather than every push.
- [x] **Process-level kill -9 ingest harness** — closed by P4.1's C3
  scenario: real SIGKILL at randomized points, `PRAGMA integrity_check`
  after every kill, no dups/misses, sidecars converge.
- [ ] **Full RAW decode backfill pass** (M1.5) — queue knows the pass kind;
  worker deliberately not built (K12).
- [ ] **HEIC support** — deferred to the decode backfill (L5), keeps libheif
  off the M1 spine.

## Update log

- 2026-06-10: created after Phase 2 completion (P1.1, P1.2, P2.1, P2.2,
  audit fix packet all green; 193 workspace tests).
- 2026-06-10: Phase 3 complete (P3.1 search core, P3.2 desktop shell, search
  wired; 254 workspace tests + 95 vitest). Added B31 erratum, app visual
  checklist, HEIC count check.
- 2026-06-10 (later): **P4.1 complete — M1 dogfood-ready.** 268 tests both
  profiles, E2E incl. kill -9 + redaction drills, DOGFOOD-M1.md written.
  Kill-9 harness item closed; NVIDIA/Wayland fix baked in; dev-loop fixes
  (thumb retry, optimized-dep profile) landed after live testing.
- 2026-06-12: **Phase 7 (M3 retrieval) mock-verified** — P7.1 vector store,
  P7.2 hybrid search, P7.3 collections store + the rail Collections tab
  and the B72 live-dictation binding fix. Close gate: cargo 479 passed
  (s02_2 known), clippy 0 warnings, svelte-check 0/0, vitest 610. Added
  the Collections-tab and search-behavior eyeball items; embedder pins
  wait on spike session 2, retrieval quality eval is post-dogfood.
