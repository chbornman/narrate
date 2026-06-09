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
packets add: `npm run check` (svelte-check), `npm run build`, `vitest`.

**Parallelism rule:** packets run in parallel only when their file ownership
is disjoint (the pattern that built the specs). Shared files (workspace
Cargo.toml, lib.rs module lists) are touched only by the coordinator between
packets.

**Spec discipline:** agents implement the spec as written. Ambiguities are
resolved by the reading most consistent with the integrity invariants,
implemented, and *flagged* — never silently "improved." Spec changes go
through DECISIONS.md.

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
LanguageModel · **P7.3 Projects store** (+ portability file, union merge).
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

| Packet | Status | Gate result | Commit |
|---|---|---|---|
| P0 Scaffold | ✅ done | fmt/clippy/test green (2 tests) | `d2b1ec2` |
| P1.1 Events engine | not started | — | — |
| P1.2 Connector traits | not started | — | — |
| P2.1 Sidecars | not started | — | — |
| P2.2 Library | not started | — | — |
| P3.1 Search core | not started | — | — |
| P3.2 Desktop shell | not started | — | — |
| P4.1 M1 hardening | not started | — | — |
| P5.1 Pencil + journal panel | not started | — | — |
| P6.1 Capture engine (mocked) | not started | — | — |
| P6.2 Runtime supervision (mocked) | not started | — | — |
| P6.3 Model spike | founder-machine | — | — |
| P7.1–7.3 Retrieval | not started | — | — |
