# Build Loop & Verification Plan

How the application gets built from the approved specs: phase-gated work
packets, each driven through the same implement→verify→fix→commit loop until
every spec is implemented and its acceptance criteria are encoded as passing
tests. The specs in `spec/` are the contract; this document is the execution
order and the definition of "verified" at each gate.

## The loop (every work packet runs this)

```
┌──────────────────────────────────────────────────────────────┐
│ 1. IMPLEMENT   agent builds the packet against its spec      │
│                sections, with tests written alongside        │
│ 2. VERIFY      coordinator runs the gate (below) on a clean  │
│                checkout — not the agent's word for it        │
│ 3. FIX LOOP    failures → diagnose → fix (agent or direct)   │
│                → re-verify; two failed cycles = stop, replan │
│ 4. COMMIT      green gate → commit + push (the container is  │
│                ephemeral; remote is the only durable state)  │
│ 5. LEDGER      update the status table at the bottom of this │
│                file; spec ambiguities found → DECISIONS.md   │
└──────────────────────────────────────────────────────────────┘
```

**The standing gate for all Rust packets** (run by the coordinator, never
skipped):

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

plus the packet's own acceptance tests (each spec's acceptance-criteria
section must be traceable to named tests — see Traceability). Frontend
packets add: `npm run check` (svelte-check), `npm run build`, `vitest`, and
`npm run check:emdash` (the no-em-dashes-in-visible-UI-copy creep gate —
`scripts/check-no-emdash.sh`; scopes to rendered text/attributes, exempts
comments and the menus.ts separator sentinel).

**Parallelism rule:** packets run in parallel only when their file ownership
is disjoint (the pattern that built the specs). Shared files (workspace
Cargo.toml, lib.rs module lists) are touched only by the coordinator between
packets.

**Spec discipline:** agents implement the spec as written. Ambiguities are
resolved by the reading most consistent with the integrity invariants,
implemented, and *flagged* — never silently "improved." Spec changes go
through DECISIONS.md.

**Docs discipline (the long-term rule, founder, June 12 2026):** behavior
changes carry their spec delta; closed work moves a ledger (BACKLOG → LANDED
with the commit hash; STATUS at packet close). The HTML pages
(`index.html`, `architecture.html`, `features.html`) are RENDERED VIEWS —
self-contained on purpose (no toolchain to rot), regenerated at packet
close, never the source of truth. Where any page disagrees with a spec or
ledger, the markdown wins.

## What "tested" can mean in this environment (honest scope)

| Verifiable here (cloud container) | Requires the founder's machine |
|---|---|
| Entire headless core: events, folds, redaction, merge, sidecars, rebuild, hashing, ingest passes, FTS search — full unit + integration + invariant suites | Visual/UX verification of the running app (grid feel, pencil feel, indicator) |
| Synthetic-library integration tests (tens of thousands of generated images), interrupt/resume, relink, round-trip | Real 50k RAW library dogfood (M1 step 9) |
| Capture state machines (binding ring buffer, session lifecycle) against a **mock Transcriber** with scripted timing — including the B1 binding acceptance test | Real mic, real ASR latency, VAD-onset error measurement (the runtime spike) |
| Runtime supervision logic against **mock child processes** (kill/restart/readiness/orphan tests with a stub binary) | Real llama.cpp + sherpa-onnx + GPU: model quality, VRAM tiers, token timestamps |
| Vector store (PPVEC v2), chunking, RRF fusion, query-AST validation — with synthetic vectors and a mock LanguageModel | Embedding/retrieval *quality* (golden-query eval needs real annotations + models) |
| Svelte frontend: type-check, unit tests, production build; Tauri Rust side compile | Full Tauri bundle + run (needs webkit2gtk etc. — attempted here, not promised), pen/stylus behavior |
| Perf smoke tests (fold ≤3 queries, search latency on generated corpora) | Perf budgets on target hardware (60 fps grid, real NVMe ingest) |

Every founder-machine item ships with a written verification script (what to
run, what to look at) so dogfooding is a checklist, not archaeology.

## Phases and work packets

Dependencies flow downward; packets on the same row run in parallel.

### Phase 0 — Scaffold ✅ (done)
Workspace + `photoproof-core` identity primitives + `photoproof-connectors`
placeholder. Gate: green. Committed (`d2b1ec2`).

### Phase 1 — Foundation
| Packet | Builds | Spec | Gate highlights |
|---|---|---|---|
| **P1.1 Events engine** | `photoproof-core`: EventId, schema/migrations/pragmas, canonical JSON, EventStore (append/fold/redact/merge), FTS maintenance, derived tables | EVENTS (all) | Invariants **I1–I16** as tests; spec §4.3 byte-exact JSON vectors; order-shuffled merge property test; ≤3-query fold assertion; 5k-event fold timing |
| **P1.2 Connector traits** | `photoproof-connectors`: ConnectorError, Transcriber, Embedder, LanguageModel, VectorStore, Reranker, typed config + mocks | RUNTIME §4, RETRIEVAL §1.2/3b, CAPTURE §6.2 | Compiles clean; config TOML parse tests; mock implementations usable by later packets |

### Phase 2 — Truth on disk (depends on P1.1)
| Packet | Builds | Spec | Gate highlights |
|---|---|---|---|
| **P2.1 Sidecars** | Sidecar serializer (pretty canonical form), debounced mtime-stable writer, overflow store, session journals, redaction propagation queue, export+manifest, rebuild-from-sidecars | SIDECARS (all) | Acceptance §13 incl. round-trip (delete DB → rebuild → identical folded journal), kill-during-write corruption test, mtime-stable test, redaction offline-queue test |
| **P2.2 Library** | Hashing pipeline (mmap+rayon), volumes + marker files, watched roots, watcher + reconciliation (clock-shift, wake triggers), ingest passes as queue, embedded-preview extraction + orientation policy, EXIF subset, thumbnail cache, cloud-placeholder detection | LIBRARY (all) | Acceptance §13 (1–15) incl. relink running/stopped, interrupt/resume idempotence, clock-shift no-storm, placeholder skip, orientation fixtures (synthetic EXIF-oriented images) |

### Phase 3 — M1 surface (depends on Phase 2)
| Packet | Builds | Spec | Gate highlights |
|---|---|---|---|
| **P3.1 Search core** | M1 FTS query layer (materialize-first SQL), structured filters, result grouping + snippets, `image_journal_stats` consumers | RETRIEVAL §4 | Plan-shape test (no FTS-driven join), snippet-post-LIMIT, latency smoke on generated 1M-row corpus |
| **P3.2 Desktop shell** | Tauri 2 + Svelte 5 app: window shell, custom thumbnail URI protocol, Grid (virtualized) / Look / Search surfaces, selection→write-scope, typed notes input, capture indicator, keyboard map, settings (roots), debug panel behind `debug-panel` feature | UI (all M1 parts), CAPTURE §3–4 | svelte-check + vitest (scope/selection/keyboard logic), Rust side compiles, release build excludes debug panel (CI assertion), Tauri bundle attempted here / verified on founder machine |

### Phase 4 — M1 hardening (depends on Phase 3)
**P4.1 Integration & perf pass**: synthetic 50k-image library generator;
end-to-end ingest→annotate→search→rebuild scenarios; perf budget smoke
tests; the founder-machine dogfood checklist (`docs/DOGFOOD-M1.md`).
Gate: full workspace suite green + all M1 acceptance criteria traceable.
**→ M1 ships for dogfooding here.**

### Phase 5 — M2a: The pencil (depends on Phase 3)
**P5.1 Grease pencil + journal panel**: canvas overlay, stroke capture →
events (coordinate mapping through pan/zoom, commit threshold, undo =
retraction, eraser), overlay toggle, journal panel (verbatim history,
revision folding, retract/redact flows with stub rendering).
Gate: stroke round-trip fidelity tests (draw → store → re-render within
tolerance at any zoom, orientation cases), CAPTURE §13 pencil criteria,
UI journal-panel criteria.

### Phase 6 — M2b: Voice, with mocks (depends on P1.2, P5.1)
| Packet | Builds | Gate |
|---|---|---|
| **P6.1 Capture engine** | Session lifecycle, scope ring buffer, VAD-onset binding, voice pipeline, transcript correction — against mock VAD/Transcriber with scripted timings | The B1 binding acceptance test (speak across selection change), session idle/crash-recovery tests, no-audio-on-disk assertion |
| **P6.2 Runtime supervision** | Process supervisor (spawn/health/backoff/orphan/single-instance), download manager (resumable, SHA-pinned), config/tiers — against stub child processes | RUNTIME §13 acceptance with mocks: kill app → no orphans; kill child mid-call → retry once; interrupted download resumes |
| **P6.3 The model spike** | *Founder-machine deliverable*: real llama.cpp + sherpa-onnx + silero-vad recipes, token-timestamp/VAD-onset measurements, tier numbers | Spike report updates RUNTIME; until then voice ships mock-verified only |

### Phase 7 — M3: Retrieval (depends on P3.1, P6.2)
**P7.1 Vector store** (PPVEC v2 int8/MRL, scrub/compact) + chunking +
embedding passes against mock Embedder · **P7.2 Hybrid search** (RRF fusion,
filter AST validation + fallback path, provenance contract) against mock
LanguageModel · **P7.3 Collections store** (+ portability file, union merge).
Gate: RETRIEVAL §13 acceptance incl. PPVEC round-trip + redaction byte-scan,
worked-example walkthroughs reproduced with deterministic mock embeddings.
Real-model retrieval *quality* (golden-query eval, reranker go/no-go) is
founder-machine, post-dogfood.

### Phase 8 — M4/M5 (not scheduled yet)
Trajectories/time-scrub and the cloud partner tier get their own loop pass
after M3 dogfooding; their specs are written but gated on M3 learnings
(sentiment quality, eval results) by design.

## Traceability

Every spec's acceptance-criteria section maps to named tests. The convention:
`crates/photoproof-core/tests/invariants_events.rs::i07_derived_equals_fold`
style names carrying the spec id. A packet is not green until its spec's
acceptance list is either (a) a passing test, or (b) explicitly listed in the
ledger as founder-machine-deferred with a checklist entry.

## Status ledger

The packet-grain ledger below tracks loop progress; the capability-grain view
(exactly what has and hasn't been built, per spec area) lives in
[STATUS.md](STATUS.md) and must be updated at every packet close.

| Packet | Status | Gate result | Commit |
|---|---|---|---|
| P0 Scaffold | ✅ done | fmt/clippy/test green (2 tests) | `d2b1ec2` |
| P1.1 Events engine | ✅ done | fmt/clippy/test green (40 tests: I1–I16, §4.3 byte-exact vectors, shuffled-merge property, ≤3-query fold assert, 5k fold 7.9 ms release) | `ddc77bd` |
| P1.2 Connector traits | ✅ done | fmt/clippy/test green (53 tests: config TOML valid+invalid, scripted-timing mock Transcriber, deterministic mock Embedder/LLM/VectorStore/Reranker, VAD contract) | `f604666` |
| P2.1 Sidecars | ✅ done | fmt/clippy/test green (193 workspace tests debug+release; §13 1–11 traced incl. round-trip, kill-during-write ×1000, mtime-stable, offline redaction queue; cross-OS byte-compare deferred to CI matrix) | `34a0f9c` |
| Audit fix packet (Phase 1) | ✅ done | fmt/clippy/test green, 163 tests (16 new regressions, each verified failing pre-fix); docs/AUDIT-PHASE1.md resolved | `b52bba6` |
| P4.2 Desktop conventions (UI-FEATURESET) | ✅ done | staged workflow build (F→A∥B∥C→I→2 verifiers); traceability 46/46 PASS; conformance findings fixed; cargo 286, svelte-check 0/0, vitest 411 | `d0e9061` |
| P2.2 Library | ✅ done | fmt/clippy/test green (147 workspace tests; §13 1–15 traced; 50k-scan/10k-clock-shift verified in release, shipped `#[ignore]`; real-RAW orientation + macOS/Win volume ids founder-machine) | `80c7ccd` |
| P3.1 Search core | ✅ done | fmt/clippy/test green; plan-shape + snippet-post-LIMIT asserted; §7.2 reproduced (spec erratum B31); 1.18M-row latency coordinator-verified: p50 9.3 ms / p95 36 ms | `ee6e386` |
| P3.2 Desktop shell | ✅ done | svelte-check 0/0, vitest 95, cargo green both feature configs; release-excludes-debug-panel asserted; Tauri .deb bundle built (8.8 MB); search wired in `38b4a61` (254 tests) | `9143b72` |
| P4.1 M1 hardening | ✅ done — **M1 dogfood-ready** | 268 tests debug+release; E2E: round-trip w/ search parity, resume+relink, kill -9 harness (integrity_check per kill), redaction byte-scan; perf (this machine): ingest 1349 files/s, search p95 3.55 ms @50k, rebuild 5.8 s @50k events; DOGFOOD-M1.md + M1 traceability matrix | `46dc10e` |
| P5.1 Pencil + journal panel | ✅ done | workflow build (implement → 3-lens adversarial review → 1 fix round, review-clean); coordinator gate on merged tree: cargo 297 fmt/clippy/test green, svelte-check 0/0, vitest 498, build green; §13.3 traced (named + 40 randomized zoom/pan round-trips ≤1 src px, ×8 orientation, wire-integer pin), §13.4 command/controller slice traced; U14 + B40–B45 recorded; pencil feel/pressure founder-machine (DOGFOOD-M2) | `1e06f1e` |
| P6.1 Capture engine (mocked) | ✅ done | workflow build (implement → 3-lens adversarial review → 1 fix round after a 529-killed first attempt, resumed from cache; review-clean); coordinator gate on merged tree: cargo 342 fmt/clippy/test green, svelte-check 0/0, vitest 505, build green; §13.1/2/5/6/7/8(a–d) traced (B1 script + 50 ms no-grace + detection-delay binding, no-audio byte-scan, muddy→moody FTS, 29/31-min boundaries + enqueue-once recovery, ASR death, linking incl. pinned in-flight suppression), §13.9 trace added; B41 terminal sample landed; B46–B52 recorded; real mic/VAD + supervision deferred to P6.2/P6.3 (DOGFOOD-M2 §6) | `9a5eece` |
| P6.2 Runtime supervision (mocked) | ✅ done | workflow build (implement → 3-lens review → 1 fix round, review-clean; review caught cross-model download fan-out, lockless orphan sweep, unpinned backoff-reset mutant); coordinator gate on merged tree: cargo 449 + 58 debug-panel, fmt/clippy clean both configs, svelte-check 0/0, vitest 511, build green; RUNTIME §13 1–10 traced vs stub child/servers (real pdeathsig SIGKILL proof, byte-zero license gate server-asserted, Busy-not-Lost both ways, §13.8 timed on the fake clock); all 8 P6.1 obligations closed; B53–B58 recorded; real binaries/pins/TLS + Win/mac mechanics + tier-gate calibration → P6.3 (DOGFOOD-M2 §7) | `fd0adc8` |
| Batch-1 polish (journal/look/rail/raw clusters) | ✅ done | 4 parallel worktrees, each adversarially reviewed + bounded fix pass; coordinator merge gate: cargo workspace green (Linux + macOS), svelte-check 0/0, vitest 547; B59/B60 recorded; the look cluster's reviewer found the stale-spaceHeld bug (fixed at merge); RAW 1:1 /embedded route + §9.3.1 orientation agreement traced | `4f5d945` |
| Dogfood round 3 fix packet (founder machine, macOS) | ✅ done | First macOS session, eight fix commits: cfg(macos) compile paths (proc_pidinfo, libc dep), FSEvents symlink-resolved paths (w07 3 s, was 30 s timeout), real volume probe (getmntinfo + getattrlist UUID) + §4.1 level-3 heuristic match (l13_07), offline-defer §10.5 amendment + poisoned-row rescue (l13_08), grid identity-by-hash, previews-changed thumb healing, ingest metrics + tracing + parallel preview waves, journal-row overlay actions; B62–B65 recorded; s02_2 (APFS case-only rename) = known macOS failure awaiting a case-sensitivity ruling | `68f2484`…`f36712b` |
| Wave 2 (chrome/search/lucide/polish/robust/rawzoom) | ✅ done | 6 parallel worktrees (two workflow runs cut off mid-flight; lanes finished + merged by hand); coordinator merge gate: cargo workspace green (s02_2 known), svelte-check 0/0, vitest 587; macOS Overlay chrome + lights-out traffic-light parity, search entry-overlay/results-canvas, Lucide adoption, B61 pair-mate mark, rebuild-previews verb + doctor v1 + welcome card, RAW lying-load gate + Sony ARW chained-IFD sweep; founder eyeball items in FOUNDER-CHECKLIST §2 | `7eccab9` |
| P6.3 Model spike | session 1 done (Apple Silicon half) | docs/SPIKE-P6.3.md: ASR recipe = English 0.6b int8 @160ms, real-time CPU (p95 lag 650 ms, 2.5 cores, 1.1 GB) BUT vendored server drops final text → owned wrapper child (B67); silero-vad 0.08 ms/chunk, onset +48 ms vs 250 ms budget; llama-server+E4B Q4_K_M: Ready 2.3 s, 34.6 tok/s Metal, 6.7 GB @16k ctx, schema probe 50/50 (needs --reasoning-budget 0); ASR+LLM concurrent: no interference, memory is the Tier-1 constraint (recommend 8k ctx on 16 GB); SHAs pinned for the B55 manifest; B66–B67 recorded. Session 2: RTX 5080 half, embedder bake-off, full concurrency matrix | spike |
| P6.4 Voice wiring | ✅ done — VERIFIED LIVE (founder MacBook, June 2026: spoke, finals minted, journal entries saved) | Pieces 1–5: real manifest pins (E2B default per B68; 160 ms ASR export; embedders stay fail-closed UNPINNED); launch recipes with spike flags PINNED BY TESTS (--reasoning-budget 0; ASR thread floor 4); tier gates with 0.5 GiB VRAM headroom (5080 calibration); real silero-vad in-process via ort (context-prepend trap implemented + regression-guarded; onset test on real speech); pp-asr-server — the owned P2 wrapper child, B67 PROVEN on the real model; https downloads via ureq/rustls (B66) with unpinned-fails-closed pre-flight. Piece 6a: RuntimePlan → REAL supervisors in the shell (EndpointCells, persistent ChildRegistry, §8.1 tick thread + 2 s plan-converge, live §8.3 readiness). Piece 6b/6c: cpal mic on its own thread (!Send on macOS; downmix + linear resample to 16 kHz, stream-clock carried across feeds), process-lifetime SherpaOnlineTranscriber + CaptureEngine shared engine/mic-thread via one mutex (lock order session → capture), SharedDrain through the §2.5/§2.2 session seam, B52 bounded quit drain wired before supervisor stop, toggle_mic command (§5.2 capture_live throttle flips with arm), live §11 indicator off the engine, M-key un-reserved + mic segment is its own click target (audit pointer path). Gates: cargo workspace green (s02_2 known), clippy/fmt 0, svelte-check 0/0, vitest 589. FIRST LIVE RUN (founder): consent → model downloads → M → speak | `eb3a2c1`… |
| Live-dictation binding fix (B72, amends B49) | done | Two commits off the defect pp-voice-bench surfaced on first live dictation: `8c2393b` associates finals to in-flight onsets by PROXIMITY instead of FIFO (the VAD splits speech into more utterances than the ASR endpointer merges; observed live, final#1 claimed a stale 6.55 s onset and the real 14.1 s onset was abandoned at disarm) and retires onsets the endpointer merged into a final - settled without minting, not counted abandoned; `6739de9` closes what the first fix left open and records the decision (DECISIONS B72; CAPTURE 1/5.1/6.3/6.5 amended): segment times now arrive on ONE clock - the capture frame clock - via the sherpa adapter's per-shipped-run ShipClock translation (armed-silence withholding had let the server clock fall behind without bound, silently degrading proximity back to FIFO after the first quiet stretch); claims bridge at most 2 s of skew, so the 5.3 independent-binding branch is reachable again; retire_merged runs before the whitespace-only-final early return (no more stuck ArmedSpeaking / phantom streaming indicator / phantom abandon); a merged final's durable span folds in retired onsets' VAD ends so a stroke drawn during the merged tail keeps its promised link. New acceptance test reproduces the live three-onset scenario exactly; gate green on the merged tree | `8c2393b`, `6739de9` |
| P7.1 Vector store + chunking + embedding passes | done (mock-verified) | fmt/clippy/test green (s02_2 known); PPVEC v2 int8/MRL-512 with §13.12 traced (`r13_12_*`: round-trip vs transient-f32 reference, exact header round-trip, scrub byte-scan at the trait AND end-to-end through `EventStore::redact` + drain); §2 chunker (offsets in Unicode scalars, sentence-snap overlap, deterministic tiny-chunk prefix); `text-embedding`/`image-embedding` passes on the L4 queue (inputs_hash staleness, §1.1 re-pend hook on journal change, NotConfigured-idle when no embedder); RETRIEVAL §1.2 `vectors` DDL lands as migration v7; embedders stay unpinned per P6.4 — real-model quality is founder-machine | `cc63978` |
| P7.1 review fix packet | ✅ done | Eight confirmed findings fixed, all regression-tested: (1) §13.5 redaction zeroing is now SYNCHRONOUS — `EventStore::redact`/`merge` zero the PPVEC flat-file bytes before returning (drain sweep stays as idempotent backstop; previously the zeroing waited on a drain nothing schedules yet); (2) mid-run journal changes are never lost — the §1.1 re-pend hook covers `running` rows and `mark_done` is guarded to running-only; (3) compaction is crash-atomic via a two-phase marker (`ppvec_compactions`, migration v8) recovered at open — no more remapped-pointers-over-old-file window; (4) all flat-file IO is serialized against the metadata pointers (connection mutex + process-wide file lock shared with the events engine's zeroing); (5) session-level remarks (zero targets) are embedded via a per-drain sweep — the per-image queue cannot reach them; (6) the tiny-chunk prefix folder is resolved per EVENT (smallest target folder), so multi-target inputs_hash is claim-order independent (§13.8); (7) §1.3 read path: mmap + rayon-parallel multiply-add scan kernel + `prewarm()` (hand-tuned SIMD intrinsics remain available behind the trait if profiling demands; prewarm wiring lands with the P7.2 query path — until then the first cold search sits outside the latency budget, documented in the module doc); (8) torn appends/headers self-heal by truncation/recreate, and per-item store IO failures mark the item failed instead of aborting the whole drain. NAMED INTERIM DEVIATION: quantization scale/offset are still the full-range constants, not a §1.3 calibration sample — the cited 1–2% int8 quality cost does NOT transfer to this scheme (~3.5 effective bits on unit vectors); the §12 eval harness gates the real number and a data-derived calibration drops in behind the same header. Also still open: nothing in the shipped app schedules `process_embedding_queue` yet (P7.2 wiring) | — |
| P7.3 Collections store | ✅ done | fmt/clippy/test green (s02_2 known); RETRIEVAL §10.1 tables (migration v9) + the §10.2 portability file (`collections.photoproof.json`, debounced writer, C2-family union merge incl. removal-beats-stale and earlier-removal-wins) + shell commands; §13.9 round-trip traced in `collections_acceptance.rs`; review fixes followed (rebuild consumes the export, set-semantics reads, loud lockouts) | `8115a66`, `073e0ea` |
| P7.2 Hybrid search | done (mock-verified) | fmt/clippy/test green (s02_2 known), svelte-check 0/0, vitest 590; the §5 pipeline as ONE code path (M1 = degenerate case, asserted byte-equal): §5.1 parse against the LanguageModel seam (grounding lists, few-shots, constrained-decoding schema, temp 0) + the validation firewall (per-clause deny_unknown_fields; §13.4 traced — unknown type / rating range / vocab miss / sub-threshold collection drop with debug-visible reasons); §10.3 collection resolution (normalized Jaro-Winkler ≥ 0.80, active>shelved>done tiebreak) needs no model, so collection chips work fully degraded; §13.3 fallback traced (model error, garbage JSON, over-budget parse, everything-dropped); weighted RRF k=60 over S1/S2/S3×2/S4 with max-per-signal aggregation (S4 always votes on semantic queries per B69 — weight, not exclusion); §5.4 best-quote provenance chain with summaries never quoted and no-evidence candidates dropped, per-signal ranks in DebugScores; schema v10 adds `derived_summaries` + `summaries_fts` (query path only — generation passes are later packets); §13.6 traced: §7.1 reproduced end-to-end (scripted parse JSON, dropped clause, per-signal ranks, fused 0.043997/0.032522/0.027163, exact voice-quote span; the spec's illustrative 0.41 similarity is metric-shape dependent — threshold/drop/reason are the normative artifacts); shell search command routes through the no-models rig, UI unchanged. Still open: nothing schedules `process_embedding_queue`/prewarm in the shipped app (embedders remain unpinned, P6.4) — wiring lands when the runtime serves embedders; real-model quality (golden queries §12, reranker §5.3) founder-machine post-dogfood | `a97c420` |
| P7.2 review fix packet | done | Confirmed findings fixed, regression-tested: (1) an unresolvable collection CHIP is now a hard error (`SearchError::UnresolvedCollection`) — chips are typed user intent, not model output, so the §5.1 drop-with-debug-visibility firewall no longer applies to them (the silent drop turned a chip-only query into a whole-library browse; M1's `UnsupportedFilter` discipline restored at the hybrid layer); (2) the §5.4 S3 provenance re-run scopes the MATCH to the image's own `fts_map` rowids BEFORE the rank cut — an own-words quote sitting below the global bm25 top-500 is no longer lost or mislabeled VisualMatch (§6); (3) §9.5/§1.1 redaction propagation is wired: `redact()` and merge delete dependent `derived_summaries` rows (image/session scope by key; folder rows conservatively — `inputs_hash` is an opaque cache key, scope-based over-deletion is safe by E4) + their `summaries_fts` rows + the chain's `sentiment_scores` rows in the same transaction as the content scrub, and mark + physically zero affected `image_summary` vectors (§13.5); (4) the parse-LLM call runs OUTSIDE the Searcher connection mutex (grounding lists under a short lock of their own) so a hung model cannot wedge later searches or pin the WAL read snapshot — the §5.1 budget stays post-hoc, wall-clock preemption remains the connector seam's transport-timeout obligation for P6.4 wiring; (5) the firewall's camera/lens vocabulary is the library's FULL distinct EXIF sets — the ≤40 most-used cap stays prompt-only, so gear at usage rank 41+ validates; (6) the `sentiment_scores` half of the §9 common-storage DDL — the scope cut the P7.2 commit message did not name — lands as migration v11 | — |
| Rail Folders/Collections tabs (B71 first slice) | done | Founder-directed (BACKLOG "Rail: Folders vs Collections tabs"): the left rail gets two PEER tabs - Collections (name + member count, shelved/done rows status-dimmed but never hidden, inline "New collection..." footer input) beside Folders (SourceList untouched); collection-open drives the grid exactly as folder selection does (rootId null so folder-keyed sort persistence and ingest re-lists stand down, title follows the collection name, live refresh on the collections-changed snapshot); "Add to collection" submenu on the thumb context menu adds the whole stack-expanded selection; new `list_collection_members` command over a batched core `Library::list_images` read. Review fixes `d92bd29`: stale-only members render offline-badged instead of vanishing (the active-path JOIN was stricter than the documented skip - membership outlives files, RETRIEVAL 10.1; only never-ingested hashes are skipped), cross-root false stacking fixed (GridItem carries root_id into the stack pairKey - identical camera paths across two roots no longer collapse into one cell with a hidden write target), membership is reversible (checkmarked Add submenu + Remove-from-collection submenu via the reportScope inspector-follow pattern), stale async collection-grid responses guarded. Gate: svelte-check 0/0, vitest 610, cargo green (s02_2 known). Encouragement/autosuggest UX stays a later design round | `98e3cb5`, `d92bd29` |
| Phase 7 close gate (full re-run, this machine) | green | The whole gate set from scratch on the final tree: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets` 0 warnings; `cargo test --workspace` 479 passed / 9 ignored (full-scale release-verified suites) / 1 failed: s02_2, the one known macOS failure (APFS case-only rename, awaiting the case-sensitivity ruling, FOUNDER-CHECKLIST 1); svelte-check 0 errors / 0 warnings across 354 files; vitest 610/610. Founder-machine remainder for Phase 7: embedder PINS await spike session 2 (RTX 5080 half + the embedder bake-off) - until then the `text-embedding`/`image-embedding` passes idle NotConfigured and nothing in the shipped app schedules `process_embedding_queue`; retrieval QUALITY eval (RETRIEVAL 12 golden queries from real annotations, reranker go/no-go) is post-dogfood by design | — |
| Backlog/cleanup pass (small UX, constants, docs coherence, test deepening) | done | Founder-directed cleanup night after the Phase 7 close, four lanes. (1) Small UX: copy actions confirm inline through ONE shared register (`copyflash.svelte.ts`; the toast cap stays at its three spec triggers) and the titlebar surfaces background jobs from pass_counters (dim "digesting" word, counts on hover; every job kind flows through ingest_passes by construction); review fixes `7ab67ba` (cancelable menu close timer, subject-keyed flash, fallback path unified). (2) Constants: bare policy literals across core/connectors/pp-asr-server/shell/frontend promoted to named constants with WHY comments, behavior-preserving (every value identical); the normative EVENTS 3.3 / CAPTURE 8.2 validation ranges published as core consts shared by the shell and PINNED by `event.rs::stroke_envelope_consts_match_the_spec`; the EVENTS 5.1 busy_timeout single-sourced (BUSY_TIMEOUT_MS) across core's four connections to the file, then exported in the coherence follow-up so the shell's debug-panel raw-tail reader (a fifth connection compiled into every dev binary) names it too instead of a same-valued local; ASR and LLM connect timeouts decoupled so each names its own liveness contract. (3) Docs close-out: spent docs retired into the list below, `docs/README.md` index created, root README rewritten to the truth, SPEC-GAPS closed banner, BACKLOG truth pass + the Lighting-up-M3 chain (`e9c16df`/`80cb349`); this row and the STATUS/checklist coherence pass close the lane. (4) Tests: cross-feature integration deepening that EXPOSED AND FIXED a real bug - `rebuild_derived` wiped event_fts but never summaries_fts (RETRIEVAL 11 says both), so index drift left orphan/lost summary rows forever; fixed and pinned by `retrieval_hybrid.rs::s11_rebuild_derived_rebuilds_summaries_fts_from_summary_rows` (verified failing pre-fix; its live-S3 assertion made discriminating in `53c067d`), plus the one-scenario redaction conjunction `m1_e2e_redaction.rs::r13_5_one_redaction_scrubs_fts_vectors_summaries_sentiment_and_sidecars`. Gate on the final tree, re-run from scratch at close: fmt clean, clippy 0 warnings, cargo test 487 passed / 9 ignored / 1 failed (s02_2 known), svelte-check 0/0 across 359 files, vitest 624/624 | `10e359e`...`d3d61a0` |
| P7.4 Embedder wiring (pins, in-process ort, scheduling, hybrid-for-real) | done (built-tested; live in-app semantic search = founder dogfood) | Five lanes per docs/PLAN-P7.4-EMBEDDER-WIRING.md. L1 path-preserving downloads (the flatten bug: DFN5B ships `visual/model.onnx` AND `textual/model.onnx` — same basename collided — and ort resolves the visual tower's ~100 external-data files relative to model.onnx; dest is now `models_dir/<id>/<file.path>` with escape rejection, resume/verify kept; nested round-trip + nested .part resume tested). L2 pins: embeddinggemma-300m-q8 (B73 text default, Gemma acceptance_required), qwen3-embedding-0.6b-int8 (alternative), DFN5B with the FULL external-data enumeration generated from the local snapshot (`runtime/dfn5b_files.rs` + `gen_dfn5b_manifest.sh`; every SHA recomputed locally, spike report as cross-check); config default flipped; RUNTIME sec 3 sentence. L3 one `OrtEmbedder` connector (recipe enum: Gemma mean-pool + documented task/title prompts, Qwen3 last-token + exactly one EOS — the spike harness's double-EOS and double-SOT/EOT bugs corrected with recorded WHY; DFN5B pair with the batch=1 textual trap; zero-length past_key_values shaped from session metadata; CPU EP 4 threads; everything L2-normalized; core owns the 378 px geometry, connector owns mean/std+CHW — recorded one-line amendment to plan decision 3); deterministic unit tests + `#[ignore]` real-model tests (paraphrase-margin floor passes both text models). L4 EmbedderHost (per-role slots + per-role build generations, loads off-thread and serialized, Failed = dark-not-crash per RUNTIME 3.3), pump embedding drain (lowest priority: only when embedders ready AND ingest idle; bounded batch; capture_live wired as the cancel flag so the mic preempts mid-sweep; jobs indicator free via pass_counters), search rig fed from the host when ready (`search_wire.rs`) — keyword path byte-identical when no embedder; settings model rows show running/idle. L5 gate, full re-run from scratch on the final tree: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets` 0 warnings; `cargo test --workspace` 613 passed / 15 ignored / 1 failed: s02_2 only (known APFS case-only rename); svelte-check 0 errors / 0 warnings across 359 files; vitest 624/624. E2E OBSERVED (`tests/retrieval_e2e_real_models.rs`, `#[ignore]`, run 2026-06-12 on this machine, real models from the local snapshots): ingest+previews of 3 real corpus JPEGs 0.65 s; EmbeddingGemma load 2.37 s; DFN5B load 11.83 s; embedding drain 14.59 s, 6/6 rows done, 0 errors (3 notes + 3 CLIP image vectors — about 4.5 s/image debug-build CPU, consistent with the spike's 2.96 s/image release posture); query "dog among autumn foliage" 521 ms -> bissy-8.jpeg ranked first with Quote provenance of the photographer's own note (fused 0.0246 vs 0.0242 / 0.0238); query "evening portrait by a stairway" 239 ms -> jamie_web1-15.jpeg first with its Quote; both queries are asserted word-disjoint from every note, so S2 keyword FTS contributed nothing — the wins are pure S1/S4 semantics through the exact shipped rig shape. Still open: parse LLM not wired (NL chips later), prewarm uncalled (first cold search outside budget), GPU/CoreML EP + tier-2 calibration await spike session 2, retrieval QUALITY is the post-dogfood sec 12 eval | `567da3d`… |

| ASR 560 ms pin swap (B74 interim) | done | Root cause of the tail-truncation class = attention lookahead baked into the model EXPORT (docs/SPIKE-ASR35.md): the 160 ms preset starves word-final tokens; no runtime knob could compensate (proven by rule2/pacing/chunk/pre-roll invariance sweeps). Swap: manifest pins -> nemotron-speech-streaming-en-0.6b-560ms-int8 (same crate, same wrapper), chunk_ms default 560, PRE_ROLL_MS 400 -> 1000 (the bigger lookahead needs warm left context after shipping gaps - 400 ms lost "Print this one" post-gap), connector flush pad 1.5 s of silence before "Done" (the cache-aware model strands last-utterance tokens otherwise; spike tail-padding finding). Corpus-validated through pp_voice_bench: "actually incredible" / "Keeper" / "the empty bench... still care about in ten years" all complete; finals land ~0.5 s later (imperceptible for journaling). Gates: workspace green (s02_2 known), fmt/clippy 0. Founder action: download the new ASR model in settings (~660 MB; the old 160 ms entry is removable). | (this commit) |

Retired tracking docs (content absorbed; full text in git history):
`docs/AUDIT-PHASE1.md` (findings → audit fix packet `b52bba6`),
`docs/RCA-BTRFS-AND-PREVIEWS.md` + `docs/BTRFS-PREVIEW-BUGS.md`
(duplicate RCAs of the btrfs volume/preview-404 incident, fixed
`613ccf9`),
`docs/M1-BUILD-PLAN.md` (M1 orientation; superseded by spec/ and shipped
- M1 verified live; its open decisions live in FOUNDER-CHECKLIST),
`docs/BEST-PRACTICES-REVIEW.md` (pre-build research executive summary;
its ~30 amendments were dispositioned into the specs at the time; the
full cited reports remain in `docs/research/`).
