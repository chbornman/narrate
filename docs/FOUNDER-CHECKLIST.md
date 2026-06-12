# Founder Checklist — running list

Living document, updated by the coordinator every packet. Three buckets:
decisions awaiting Caleb, verification only Caleb's machine can do, and
items deferred to CI/later milestones. The *rationale* for resolved
decisions lives in spec/DECISIONS.md; this file is the action list.

## 1. Decisions awaiting you

- [ ] **Q1 — final product name.** "Photoproof" is the working placeholder.
  The `.photoproof.json` sidecar suffix and `.photoproof-volume` markers
  harden into real user data the moment you dogfood M1 on your library.
  Decide before running the M1 dogfood; rename is a clean find/replace
  until then.
- [ ] **B22 — append of a redaction-condemned id is rejected** (`CondemnedId`
  error) rather than silently inserted in scrubbed form. Integrity-
  conservative reading; confirm or veto.
- [ ] **B23 — blocked WAL-hygiene checkpoint surfaces as an error**
  (`CheckpointBlocked`; the write is already durable, `maintain()` heals).
  The alternative was silent background retry. Confirm or veto.
- [ ] **Thumbnail/preview cache sizes** (M1-BUILD-PLAN decision 2): 512 px
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

- [ ] **First live voice run (P6.4, MacBook)** — the whole pipeline is
  wired; nothing has heard a real microphone yet. Steps: `brew install
  llama.cpp` (P1 dev binary; pp-asr-server builds with the workspace),
  restart `cargo tauri dev`, accept the consent offer in settings (≈5.3 GB
  at Tier 1: E2B QAT + Nemotron ASR; Gemma needs its license acceptance),
  wait for the downloads, watch the mic glyph appear in the indicator when
  the ASR child reports Ready, press **M** (macOS will ask for mic
  permission on first arm), speak a note over a selected image, watch the
  pulse + journal entry land. Disarm with M; check the OS mic dot turns
  off with it. If arming quietly fails: the debug panel (F12) shows
  supervisor states and capture notes.
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
