# Photoproof

**The digital contact sheet with a grease pencil.**

Photoproof is a local-first desktop app for photographers with a serious practice. You browse your library, select frames, and work the sheet the way photographers always have — talking through the work and marking it up with a red grease pencil. Every utterance and every stroke is captured into an append-only, timestamped journal bound to the image itself. Over years, your library stops being a pile of files and becomes a longitudinal record of your creative thinking — searchable in your own words.

It is not an editor, not a DAM, and pointedly not another AI opinion about your photographs. Every competitor's AI generates opinions about your photos; Photoproof preserves *yours*.

> The repo is `narrate` for historical reasons; the product name stands for now (DECISIONS B70).

## Status (June 2026)

M1 (the journal spine) and M2 (grease pencil + live voice capture) are built and **verified live on real libraries** — ingest of 50k-image SMB folders, typed notes, ratings, pencil strokes, and on-device streaming voice transcription minting journal entries. M3 retrieval (vector store, hybrid search, collections) is built and mock-verified; semantic search lights up once the embedder models are pinned (spike session 2).

- [docs/STATUS.md](docs/STATUS.md) — **the capability ledger**: every spec obligation, what state it is in, and the evidence
- [docs/BUILD-LOOP.md](docs/BUILD-LOOP.md) — the packet-grain build ledger and verification rules
- [docs/BACKLOG.md](docs/BACKLOG.md) — decided-but-not-scheduled work
- [docs/FOUNDER-CHECKLIST.md](docs/FOUNDER-CHECKLIST.md) — decisions and founder-machine verification pending

## Running it

```sh
# dev (debug panel included automatically in dev binaries; F12 toggles it)
cd apps/desktop && cargo tauri dev

# voice needs: brew install llama.cpp (P1 dev binary), then in-app consent
# downloads the pinned models (Gemma E2B QAT + Nemotron streaming ASR)
```

## Tests and benches

```sh
cargo test --workspace          # one known red on macOS: s02_2 (APFS case-only rename, ruling pending)
cd apps/desktop && npm run check && npx vitest run

scripts/bench.sh                # ingest/preview perf against frozen corpora (test-corpora/, gitignored)
cargo run --bin pp_voice_bench  # voice e2e: wav in, minted journal entries out; all chunking knobs as flags
```

The standing gate for every change: `cargo fmt` + `clippy` (zero warnings) + the full test suites above.

## Repo map

| Path | What |
|---|---|
| `spec/` | The normative implementation contract (EVENTS, SIDECARS, LIBRARY, CAPTURE, RETRIEVAL, RUNTIME, UI; DECISIONS is the why-log). Where anything disagrees with a spec, the spec wins. |
| `docs/` | Vision (SCOPE), feature inventory (FEATURES), ledgers (STATUS, BUILD-LOOP), queue (BACKLOG), dogfood scripts, spike reports |
| `crates/photoproof-core` | Domain logic: events, sidecars, library/ingest, capture engine, search, vector store, collections, model runtime |
| `crates/photoproof-connectors` | Model/IO seams (Transcriber, Embedder, LanguageModel, VectorStore) + deterministic mocks; silero VAD; sherpa WS client |
| `crates/pp-asr-server` | The owned streaming-ASR wrapper child (B67: finals never know less than their partials) |
| `apps/desktop` | Tauri 2 + Svelte 5 shell: thin commands over core, three quiet surfaces (Grid, Look, Search) |

## The core loop

**browse → select → speak & mark → it remembers**

## Principles (the short version)

- **Local-first, cloud-optional.** The free tier never touches the network.
- **The journal is the product.** Append-only event log; entries are never overwritten.
- **Sidecars are the truth.** SQLite is a rebuildable index; canonical data lives in open-format `.photoproof.json` sidecars beside your images.
- **Content-addressed identity.** Images are known by BLAKE3 hash, never by path — reorganize freely, annotations follow the pixels.
- **Collections over folders.** Folders are mechanical; collections are intent — tags with time, never moved files.
- **You can always walk away** with everything, in open formats.

## Stack

Tauri 2 · Rust workspace · SQLite (WAL) + FTS5 · BLAKE3 · rawler · PPVEC vector store · local ASR/LLM as supervised child processes behind swappable connector traits (llama.cpp, sherpa-onnx, OpenAI-compatible seam) · optional Claude cloud connector planned for the paid conversational tier (M5).

## Roadmap

| Milestone | Theme | State |
|---|---|---|
| M1 — Spine | Ingest, content-addressed library, browser, typed notes → event log → sidecars, FTS5 search | shipped, dogfooding |
| M2 — The Sheet | Streaming voice capture, grease-pencil markup, stroke↔utterance linking | shipped, dogfooding |
| M3 — Retrieval | Embeddings, hybrid search, collections (intent memory), natural-language query | built mock-verified; awaits embedder pins |
| M4 — Time | Sentiment trajectories, "changed my mind" queries, per-image timeline, stroke scrubbing | spec'd |
| M5 — Partner | Cloud connector (Claude), two-way conversation, premium tier | spec'd |
