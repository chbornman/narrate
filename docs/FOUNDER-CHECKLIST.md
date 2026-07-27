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
- [ ] **ASR 560 ms swap (B74 interim) - re-dogfood dictation**: settings
  offers the new model id (nemotron...560ms-int8, ~660 MB; the old 160 ms
  row becomes removable). Expect: word tails complete ("incredible",
  "Keeper"), finals arriving about half a second later, first words
  intact after long silences. The voice corpus validates it headlessly;
  your live feel is the real gate. Then re-record a card or two so the
  corpus carries the new baseline.
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
- [x] **APFS case-insensitivity ruling** — s02_2 (case-only rename
  relinks sidecar) is implemented with live mounted-volume semantics:
  same-entry aliases recase one path row, while case-distinct entries remain
  independent. DONE 2026-07-27 on the founder Mac's default case-insensitive
  APFS Data volume at `4495b5b`: sidecar relink, zero-hash reconciliation
  recase, and all 11 watcher tests passed with `result=PASS`.
- [ ] **Desktop-foundation native receipts** — retain the installed Mac
  package startup/shutdown and crash/relaunch record, two-webview convergence,
  real suspend/resume, CPAL device-removal behavior, and control-file plus
  backup/restore drills. The deterministic A01-A26 foundation is green; these
  are the facts simulation cannot establish.
- [ ] **Accelerator and performance receipts** — run CoreML on this Mac and
  CUDA/TensorRT on the 5080, confirm the UI-reported selected provider matches
  the profile, and retain idle/peak RSS plus primary journey timings. Windows,
  removable-drive, and hard-NAS receipts remain separate platform gates.
- [ ] **Release credentials and rollout** — provision Developer ID
  Application/notarization, Windows Authenticode, updater signing, and the
  cohort-aware HTTPS endpoint before any 0.1.0 candidate is published.
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
  search is ONE code path and with no embedder READY it must feel exactly
  like M1 (asserted byte-equal in tests; your fingers are the second
  check). Collection chips work fully degraded — fuzzy name match, active
  collections win ties; an unresolvable chip is now a HARD error, not a
  silent whole-library browse. As of P7.4 semantic ranking wakes up once
  the embedder models are downloaded (item below); the debug panel shows
  per-signal ranks and dropped-clause reasons.
- [ ] **Semantic search live (P7.4) — the dogfood script line**: consent
  -> download -> watch the jobs indicator -> search semantically.
  Concretely: (1) accept the model consent (sizes below), and note the
  EmbeddingGemma row brings a SECOND Gemma license acceptance in settings
  — a separate model id keeps its own acceptance record, so seeing the
  terms gate again for the embedder is correct, not a bug; (2) let the
  downloads finish (settings model rows; embedder rows then show
  running/idle state); (3) watch the titlebar background-jobs indicator —
  the embedding backfill drains only while ingest is idle and the mic is
  off, lowest priority by design; (4) search with words you never typed
  in a note ("the foggy ones", "dog in the leaves") and check the right
  images surface with quoted provenance. CONSENT SIZE CHANGE: tier 1 now
  asks for ~4.3 GB more than the voice-era card — EmbeddingGemma 0.33 GB
  + DFN5B CLIP 3.95 GB (400 pinned files; the card's byte sum is computed
  live from the manifest). Tier 2 additionally offers the Qwen3
  alternative (+0.62 GB, Apache-2.0, no gate). BACKFILL EXPECTATIONS:
  image embedding measured 2.96 s/image on laptop CPU (spike; ~4.5 s in
  the debug-build e2e) — a 50k-image library is ~41 h of background CPU,
  so plan on idle hours on the MacBook or run the backfill on the
  desktop; text notes are cheap (85 ms/note). The e2e proof of the whole
  path ran 2026-06-12 (`retrieval_e2e_real_models.rs`, BUILD-LOOP P7.4
  row); what remains here is YOUR library and YOUR queries — this
  checklist item is the live flip for the STATUS retrieval rows.
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
- [ ] **P6.3 model spike, session 2** (RTX 5080 half): Tier-2
  VRAM/throughput calibration, CUDA posture, the RUNTIME 12.4 concurrency
  matrix, and the GPU/CoreML EP numbers for the DFN5B backfill. The
  embedder bake-off itself is DONE (docs/SPIKE-P7-EMBED.md, MacBook half)
  and P7.4 pinned + wired the winners — what session 2 buys now is the
  fast image-backfill path. Session 1 (Apple Silicon) is DONE —
  docs/SPIKE-P6.3.md — and voice is verified live (P6.4).
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
- 2026-06-12 (later): **P7.4 embedder wiring done — semantic search is
  built-tested and waiting on your machine.** B73 winners pinned
  (EmbeddingGemma default + Gemma terms gate, Qwen3 tier-2 alternative,
  DFN5B fully enumerated), in-process ort embedders, pump-scheduled
  backfill, hybrid rig live in the shell; real-model e2e ran on this
  machine (BUILD-LOOP P7.4 row has the observed numbers). Added the
  "Semantic search live" dogfood item (consent size +~4.3 GB at tier 1,
  second Gemma acceptance, backfill pacing); reworded the session-2 spike
  item (bake-off done, GPU EP remains). Gate: cargo 613 passed (s02_2
  known), clippy 0, svelte-check 0/0 (359 files), vitest 624/624.
