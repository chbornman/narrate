# Photoproof

**The digital contact sheet with a grease pencil.**

Photoproof is a local-first desktop app for photographers with a serious practice. You browse your library, select frames, and work the sheet the way photographers always have — talking through the work and marking it up with a red grease pencil. Every utterance and every stroke is captured into an append-only, timestamped journal bound to the image itself. Over years, your library stops being a pile of files and becomes a longitudinal record of your creative thinking — searchable in your own words.

It is not an editor, not a DAM, and pointedly not another AI opinion about your photographs. Every competitor's AI generates opinions about your photos; Photoproof preserves *yours*.

> Working title. Previously "Darkroom Notes," briefly "Daido." This repo is `narrate` for historical reasons.

## Status

**Spec-complete, pre-build.** The implementation contract is written; code has not started.

Vision & planning (`docs/`):
- [docs/SCOPE.md](docs/SCOPE.md) — pitch, scope & architecture overview
- [docs/FEATURES.md](docs/FEATURES.md) — milestone-tagged feature inventory
- [docs/SPEC-GAPS.md](docs/SPEC-GAPS.md) — design review that drove the specs; revised phase order
- [docs/M1-BUILD-PLAN.md](docs/M1-BUILD-PLAN.md) — Milestone 1 orientation

Normative specs (`spec/` — where these and `docs/` disagree, `spec/` wins):
- [spec/EVENTS.md](spec/EVENTS.md) — the event model (foundation: log, folds, redaction, merge)
- [spec/SIDECARS.md](spec/SIDECARS.md) — sidecar format, overflow store, export/rebuild
- [spec/LIBRARY.md](spec/LIBRARY.md) — identity, volumes, watcher, ingest passes, previews
- [spec/CAPTURE.md](spec/CAPTURE.md) — sessions, write-scope binding, voice, grease pencil
- [spec/RETRIEVAL.md](spec/RETRIEVAL.md) — indexes, query pipeline, ranking, collections
- [spec/RUNTIME.md](spec/RUNTIME.md) — local model runtime, processes, tiers, downloads
- [spec/UI.md](spec/UI.md) — the three surfaces, indicator, journal panel, debug panel
- [spec/DECISIONS.md](spec/DECISIONS.md) — architecture decision log + open questions

## The core loop

**browse → select → speak & mark → it remembers**

## Principles (the short version)

- **Local-first, cloud-optional.** The free tier never touches the network.
- **The journal is the product.** Append-only event log; entries are never overwritten.
- **Sidecars are the truth.** SQLite is a rebuildable index; canonical data lives in open-format `.photoproof.json` sidecars beside your images.
- **Content-addressed identity.** Images are known by BLAKE3 hash, never by path — reorganize freely, annotations follow the pixels.
- **You can always walk away** with everything, in open formats.

## Planned stack

Tauri 2 · Rust core · SQLite (WAL) + FTS5 · BLAKE3 · rawler · local ASR/LLM behind swappable connector traits (llama.cpp, OpenAI-compatible seam) · optional Claude cloud connector for the paid conversational tier.

## Roadmap

| Milestone | Theme |
|---|---|
| M1 — Spine | Ingest, content-addressed library, browser, typed notes → event log → sidecars, FTS5 search |
| M2 — The Sheet | Streaming voice capture, grease-pencil markup, stroke↔utterance linking, local summaries |
| M3 — Retrieval | Embeddings, hybrid search, collections (intent memory), natural-language query |
| M4 — Time | Sentiment trajectories, "changed my mind" queries, per-image timeline, stroke scrubbing |
| M5 — Partner | Cloud connector (Claude), two-way conversation, premium tier |
