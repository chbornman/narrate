# M1 — Spine: Build Plan

> **Superseded in detail by `spec/`** — the normative implementation contract is
> spec/EVENTS.md, SIDECARS.md, LIBRARY.md, CAPTURE.md, RETRIEVAL.md, RUNTIME.md,
> UI.md (see also docs/SPEC-GAPS.md for the revised phase order: embedded-preview
> ingest, pencil before voice, M1-parallel runtime spike). This file remains the
> M1 orientation document; where it and a spec disagree, the spec wins.

M1 proves the journal with zero AI risk: ingest a real library, browse it, attach typed notes to images as append-only events, mirror everything to sidecars, and search it with FTS5. No models, no audio, no embeddings. If M1 is good, the product thesis (an append-only journal bound to content-addressed images) is real; everything after is amplification.

**Definition of done:** the founder's ~50k-image library ingests (resumably), browses smoothly, accepts typed notes against any selection, every note appears in a `.photoproof.json` sidecar beside the image within seconds, a moved/renamed file relinks without losing its journal, and FTS5 search over notes returns the right frames.

## Repository Layout

```
narrate/
├── Cargo.toml                 # workspace root
├── crates/
│   ├── photoproof-core/        # the product: domain types, event log, ingest,
│   │                           #   sidecar sync, search. No Tauri, no UI deps.
│   │   ├── src/
│   │   │   ├── id.rs           # ContentHash (BLAKE3), Ulid newtypes
│   │   │   ├── events/         # event log per spec/EVENTS.md (retraction/redaction)
│   │   │   ├── ingest/         # walker, hasher, queue, thumbnailer, RAW decode
│   │   │   ├── library/        # watched roots, volumes, relink, notify watcher
│   │   │   ├── sidecar/        # .photoproof.json schema (versioned), mirror writer
│   │   │   ├── search/         # FTS5 queries, structured filters
│   │   │   └── db/             # SQLite (WAL), migrations, rebuild-from-sidecars
│   │   └── tests/              # the integrity tests live here (see below)
│   └── photoproof-connectors/  # trait definitions ONLY in M1 (Transcriber,
│                               #   Embedder, LanguageModel, VectorStore) + stubs
├── apps/
│   └── desktop/                # Tauri 2 app
│       ├── src-tauri/          # thin command layer over photoproof-core
│       └── src/                # frontend: grid browser, image view, notes panel
└── docs/
```

Rules that keep M1 honest:
- `photoproof-core` never imports Tauri. It must be drivable from tests and a future CLI.
- Connector traits are defined in M1 but implemented only as no-op/echo stubs, so M2/M3 plug in without reshaping the core.
- SQLite is rebuildable from sidecars from day one — `db/rebuild.rs` is an M1 deliverable, not a later promise. It is the proof of the "sidecars are the truth" principle.

## Key Dependencies (initial picks)

| Concern | Crate | Note |
|---|---|---|
| Hashing | `blake3` | mmap + rayon for throughput |
| IDs | `ulid` | sortable event ids |
| DB | `rusqlite` (bundled) | WAL mode; FTS5 ships in the bundled build |
| RAW decode | `rawler` | backfill pass only — M1 ships on embedded JPEG previews (spec/LIBRARY.md §9); libraw FFI fallback deferred until a real format gap appears |
| Non-RAW decode | `image` + `kamadak-exif` | JPEG/TIFF/PNG previews + EXIF subset |
| FS watching | `notify` | plus startup reconciliation scan |
| Serialization | `serde` / `serde_json` | sidecar schema carries an explicit `version` field |
| Async runtime | `tokio` | ingest queue, sidecar writer, watcher all need it |
| Frontend | Tauri 2 + **Svelte 5** | decided (founder); not revisited during M1 |

## Build Order

1. **Workspace + identity.** Cargo workspace, `ContentHash`, ULID newtypes, error types. Hash a directory tree fast (mmap + rayon); this is the perf foundation everything sits on.
2. **Schema + event log.** SQLite migrations per spec/EVENTS.md (`annotation_events`, `event_targets`, `sessions`, FTS5) and spec/LIBRARY.md (`images`, `paths`, `volumes`, `ingest_passes`). Append/read API with retraction folds and the redaction scrub path — *the only update that exists.*
3. **Sidecar mirror.** Versioned `.photoproof.json` schema (embeds content hash + full event history for that image), debounced background writer, overflow store for unwritable volumes, and `rebuild-from-sidecars`.
4. **Ingest pipeline.** Resumable versioned passes: walk → hash → embedded-preview extraction → thumbnail cache, EXIF subset (spec/LIBRARY.md §10). Idempotent by hash; interrupt/resume tested on a large tree. Full RAW decode via rawler is a later backfill pass, not an M1 blocker.
5. **Library sync.** Watched roots, `notify` watcher, startup reconciliation, move/rename = relink, offline-volume state.
6. **Desktop shell.** Tauri app: register roots, virtualized thumbnail grid, single-image view, selection model.
7. **Typed notes + write scope.** Notes panel bound to current selection; the "speaking about: N images" scope indicator ships now, with typing, so the scope discipline is baked in before voice exists.
8. **Search.** FTS5 over note text + structured filters (date, camera, folder, rated). Search-as-you-type in the shell.
9. **Dogfood pass.** Full ingest of the founder's library; fix what hurts.

Steps 1–5 are pure `photoproof-core` and fully testable headless. The UI (6–8) starts once 1–3 are stable.

## Integrity Tests (non-negotiable, written alongside, not after)

The scope doc's prime directive is log integrity and portability. M1 encodes it as tests:

- **Append-only:** no API mutates or deletes an event; retraction tombstones (content preserved), redaction scrubs content while the row and its structure survive (spec/EVENTS.md §7).
- **Round-trip:** ingest → annotate → delete the SQLite db → rebuild from sidecars → byte-identical event history.
- **Relink:** move/rename a file out from under the app (running and stopped); annotations follow the hash.
- **Interrupt:** kill ingest mid-run; resume completes with no duplicates and no missed files.
- **Sidecar match:** a sidecar separated from its image re-matches by embedded hash.

## Explicitly Out of Scope for M1

Voice/ASR, the grease pencil, embeddings and vector search, LLM summaries, sentiment, projects/intent store, XMP export, cloud anything. The connector traits exist; nothing implements them yet.

## Decisions To Make Before Coding

1. ~~**Frontend framework**~~ — decided: **Svelte 5** inside Tauri 2.
2. **Thumbnail/preview cache format and location** — app data dir, content-hash-keyed; sizes (grid thumb + 2560px preview?) worth a quick benchmark against the real library.
3. **Sidecar write debounce window** — "within seconds" UX promise vs. write amplification on spinning archive drives.
4. **Name check** — "Photoproof" is a working placeholder; trademark/domain due diligence before anything public, and the final name is deliberately deferred (tracked in SCOPE.md open questions; doesn't block code).
