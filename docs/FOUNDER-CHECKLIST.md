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

## 2. Founder-machine verification (feeds DOGFOOD-M1.md at P4.1)

- [ ] **Real 50k RAW library ingest** (M1 step 9) — resumability, perf
  budgets (≤90 min NVMe target, first 1k thumbs ≤60 s), preview quality.
- [ ] **Real-RAW orientation fixtures** — portrait-orientation Nikon/Fuji
  (and any other makers you shoot) RAWs through the rawler embedded-preview
  extractor; verify no double-rotation (P9: makers pre-rotate
  inconsistently). Synthetic fixtures pass; real files pending.
- [ ] **macOS / Windows volume identity** — DiskArbitration + volume-serial
  recipes and real OS cloud-placeholder flags are implemented behind seams
  but only Linux-probed here. Needs a real Mac/Win machine (or defer until
  you target those platforms).
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
- [ ] **Process-level kill -9 ingest harness** — in-process cancellation is
  covered; a true out-of-process kill harness is a P4.1 candidate.
- [ ] **Full RAW decode backfill pass** (M1.5) — queue knows the pass kind;
  worker deliberately not built (K12).
- [ ] **HEIC support** — deferred to the decode backfill (L5), keeps libheif
  off the M1 spine.

## Update log

- 2026-06-10: created after Phase 2 completion (P1.1, P1.2, P2.1, P2.2,
- 2026-06-10: Phase 3 complete (P3.1 search core, P3.2 desktop shell, search
  wired; 254 workspace tests + 95 vitest). Added B31 erratum, app visual
  checklist, HEIC count check.
  audit fix packet all green; 193 workspace tests).
