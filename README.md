# Photoproof

**A second pair of eyes on a lifetime of work.**

Photoproof is a local-first desktop app for photographers with a serious, career-long practice: the kind of archive that grows to tens or hundreds of thousands of frames over decades. It does the work a great picture editor does at your side. It helps you reflect on a body of work, surface the through-lines for the next book or show, and resurface the strong frames you forgot you ever shot.

You browse your library, select frames, and work the sheet the way photographers always have: talking through the work and marking it up with a red grease pencil. Every utterance and every stroke is captured into an append-only, timestamped journal bound to the image itself. Over the years your library stops being a write-only pile of files and becomes a longitudinal, searchable record of your creative thinking, in your own words.

The intelligence stays deliberately quiet. It does not generate opinions about your photographs, automate the edit, or push "AI" in your face. It surfaces candidates, connections, and forgotten gems; you stay the author. Every competitor's AI tells you what it thinks of your photos. Photoproof preserves and resurfaces what *you* think.

Not an editor, not a DAM, not another AI critic. A curatorial aide for a career archive, running entirely on your own machine.

> "Photoproof" is the working title. The positioning (a curatorial aide for a career archive) has sharpened faster than the name, which is under active reconsideration. The repo is `narrate` for historical reasons (DECISIONS B70).

## Status (June 2026)

The journal spine (M1) and the sheet (M2: grease-pencil markup + live on-device voice capture) are built and verified live on real libraries: ingest of 50k-image SMB folders, typed notes, ratings, pencil strokes, and streaming voice transcription minting journal entries.

The full local ML stack is now validated and wired as the committed default, auto-selecting the best path for whatever machine it runs on:

- **Image search (CLIP)** is the FP16 single-file model, auto-accelerated: CoreML on Apple Silicon (8.77x over CPU), CUDA / TensorRT on NVIDIA (62-117x), CPU everywhere else.
- **Voice (ASR)** is Nemotron 3.5 via `parakeet-rs` (native punctuation, capitalization, multilingual), on CPU, real-time on every tested machine. The lighter int8 English engine stays a runtime-dispatched fallback in the same binary.
- **Text embeddings + VAD** are EmbeddingGemma and Silero, on CPU by design.
- **Language model** is Gemma 4 (Metal on Mac, CUDA on NVIDIA, with multi-token-prediction speculative decode on capable NVIDIA cards).

Hybrid keyword + semantic search (FTS5 + vector fusion) is wired. The intelligent detect -> tier -> select -> fallback machinery picks the execution provider per model, with a CPU floor under every accelerator and a complete zero-model Tier-0 product underneath (typed notes, ratings, grease pencil, FTS5). Summary and caption GENERATION (the background LLM passes that feed richer search) is spec'd but not yet wired.

- [docs/STATUS.md](docs/STATUS.md) - the capability ledger: every spec obligation, its state, the evidence
- [docs/RUNTIME-MATRIX.md](docs/RUNTIME-MATRIX.md) - where each model runs per machine, the acceleration numbers, the fallback chains
- [docs/LANDED.md](docs/LANDED.md) - shipped work with commit hashes; [docs/BACKLOG.md](docs/BACKLOG.md) - open work
- [docs/LICENSES.md](docs/LICENSES.md) - full license inventory (code, runtimes, model weights)

## Running it

```sh
# dev: tauri builds the ASR sidecar (with the parakeet 3.5 engine) automatically
cd apps/desktop && cargo tauri dev      # F12 toggles the debug panel in dev builds

# voice + search then need only in-app consent, which downloads the pinned models.
# the LLM child wants llama-server on PATH in dev: brew install llama.cpp
```

NVIDIA GPU build: `cargo tauri build --features cuda-dynamic`, with a hardware-matched onnxruntime staged at `{app-data}/runtime/onnxruntime-cuda/lib` (see RUNTIME-MATRIX for the Blackwell sm_120 recipe).

## Tests and benches

```sh
cargo test --workspace          # one known red on macOS: s02_2 (APFS case-only rename, ruling pending)
cd apps/desktop && npm run check && npx vitest run

scripts/bench.sh                # ingest/preview perf against frozen corpora (test-corpora/, gitignored)
cargo run --bin pp_voice_bench  # voice e2e: wav in, minted journal entries out; all chunking knobs as flags
scripts/asr-ab.sh               # ASR engine A/B (sherpa int8 vs parakeet 3.5): RTF + peak RSS, both machines
```

The standing gate for every change: `cargo fmt` + `clippy` (zero warnings) + the full test suites above.

## Repo map

| Path | What |
|---|---|
| `spec/` | The normative implementation contract (EVENTS, SIDECARS, LIBRARY, CAPTURE, RETRIEVAL, RUNTIME, UI; DECISIONS is the why-log). Where anything disagrees with a spec, the spec wins. |
| `docs/` | Vision (SCOPE), feature inventory (FEATURES), ledgers (STATUS, BUILD-LOOP, LANDED), the acceleration matrix (RUNTIME-MATRIX) + its plans/spikes, license inventory (LICENSES), queue (BACKLOG), dogfood scripts |
| `crates/photoproof-core` | Domain logic: events, sidecars, library/ingest, capture engine, search, vector store, collections, model runtime |
| `crates/photoproof-connectors` | Model/IO seams (Transcriber, Embedder, LanguageModel, VectorStore) + deterministic mocks; ONNX Runtime embedders (CoreML/CUDA/TensorRT EPs); silero VAD; sherpa WS client |
| `crates/pp-asr-server` | The owned streaming-ASR wrapper child: two engines (sherpa int8 + parakeet 3.5) compiled in, dispatched at runtime by model layout (B67: finals never know less than their partials) |
| `apps/desktop` | Tauri 2 + Svelte 5 shell: thin commands over core, three quiet surfaces (Grid, Look, Search) |

## The core loop

**browse -> select -> speak & mark -> it remembers -> it resurfaces**

## Principles (the short version)

- **Local-first, cloud-optional.** The free tier never touches the network.
- **Deferential intelligence.** The AI surfaces candidates and connections; it never overwrites your judgment or automates the edit. You stay the author.
- **The journal is the product.** Append-only event log; entries are never overwritten.
- **Sidecars are the truth.** SQLite is a rebuildable index; canonical data lives in open-format `.photoproof.json` sidecars beside your images.
- **Content-addressed identity.** Images are known by BLAKE3 hash, never by path. Reorganize freely; annotations follow the pixels.
- **Collections over folders.** Folders are mechanical; collections are intent (tags with time, never moved files).
- **You can always walk away** with everything, in open formats.

## Stack

Tauri 2 · Rust workspace · SQLite (WAL) + FTS5 · BLAKE3 · rawler (RAW) · PPVEC vector store · ONNX Runtime with per-platform execution providers (CoreML / CUDA / TensorRT / CPU) · local ASR (parakeet-rs + sherpa-onnx) and LLM (llama.cpp) as supervised child processes behind swappable connector traits · optional Claude cloud connector planned for the paid conversational tier (M5).

## Roadmap

| Milestone | Theme | State |
|---|---|---|
| M1 - Spine | Ingest, content-addressed library, browser, typed notes -> event log -> sidecars, FTS5 search | shipped, dogfooding |
| M2 - The Sheet | Streaming voice capture, grease-pencil markup, stroke<->utterance linking | shipped, dogfooding |
| M3 - Retrieval | Embeddings, hybrid search, collections (intent memory), natural-language query | built; ML stack validated + wired as per-platform defaults; summary/caption generation passes pending |
| M4 - Time | Sentiment trajectories, "changed my mind" queries, per-image timeline, stroke scrubbing | spec'd |
| M5 - Partner | Cloud connector (Claude), two-way conversation, premium tier | spec'd |
