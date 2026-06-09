# Best-Practices Research Review — June 2026

Four research agents validated the spec set against 2024–2026 production
practice before implementation. Full cited reports live in `docs/research/`;
this is the executive summary and the amendment disposition.

**Overall verdict: no architectural rework required.** All four domains came
back "validated" at the architecture level — the storage report's phrasing:
*"matches 2024–2026 best practice almost point for point."* The retrieval
report: *"a properly planned — unusually disciplined — local RAG system."*
The library report: *"in several places ahead of the surveyed tools."* What
the pass caught is calibration and contract errors — about 30 concrete
amendments, every one a paragraph-level spec change, now applied (see
disposition table).

## The five findings that mattered most

1. **The vector-store math was optimistic** (RAG report). Brute-force scan is
   memory-bandwidth-bound; at f32/1024-dim the 2M-event ceiling means an 8 GB
   scan (~270 ms), and the old ">500k vectors" swap trigger already breached
   the 50 ms budget. **Fix applied: vectors are stored int8-quantized at
   MRL-512 dims** (~1–2% quality cost, 8× reduction) — the flat file now
   covers the lifetime ceiling (~35 ms at 2M chunks) with no ANN index ever.
2. **The utterance-binding rule needed a real onset signal** (runtime report).
   sherpa-onnx emits no `SpeechStart`, and transducer token timestamps are
   systematically late (the RNN-T emission-delay problem). **Fix applied:
   silero-vad fronts the ASR in-process for onset detection (and silence
   gating); ASR endpointing remains authoritative for segmentation.** This
   protects the product's signature behavior with ~1 MB of model.
3. **EVENTS and RETRIEVAL specified two different FTS5 constructions**
   (storage report) — plain content-ful vs external-content, different prefix
   settings. **Fix applied: EVENTS' plain content-ful table wins** (folded
   text exists nowhere as a real column; external-content desync is a
   documented corruption source); RETRIEVAL's search SQL restructured to
   materialize MATCH hits before joining (the documented 650× planner trap)
   and to evaluate snippet() only post-LIMIT.
4. **Embedded-preview orientation is a correctness bug waiting to happen**
   (library report). RAW previews are inconsistently pre-rotated across
   makers, and strokes are drawn over previews — silent double-rotation
   corrupts the stroke contract. **Fix applied: orientation verification
   against RAW dimensions, per-format test fixtures, and an explicit
   "never regenerate a stroke substrate except via generator_version"
   invariant.**
5. **Free retrieval-quality wins were on the table** (RAG report).
   **Applied:** embedding instruction prefixes normative (skipping costs
   1–5%), deterministic context prefixes for tiny chunks, an optional local
   reranker stage behind a trait (M3+, +5–15 nDCG@10, sub-second CPU), and a
   golden-query eval harness as the gate for all deferred tuning decisions.

## Amendment disposition

| # | Amendment | Spec(s) | Status |
|---|---|---|---|
| R1 | Embedding instruction prefixes normative | RETRIEVAL, RUNTIME | applied |
| R2 | PPVEC v2: int8 + MRL-512 stored default | RETRIEVAL | applied |
| R3 | Latency math fixed; swap trigger in bytes-scanned | RETRIEVAL | applied |
| R4 | Optional `Reranker` trait stage, M3+, eval-gated | RETRIEVAL | applied |
| R5 | Deterministic context prefix for tiny chunks | RETRIEVAL | applied |
| R6 | Golden-query retrieval eval harness | RETRIEVAL | applied |
| R7 | Lost-in-the-middle ordering guidance (§8) | RETRIEVAL | applied |
| R8 | Benchmark EmbeddingGemma-308M in spike | RUNTIME | applied |
| S1 | One FTS construction: plain content-ful | RETRIEVAL (EVENTS wins) | applied |
| S2 | Journal fold = ≤3 batched queries, N+1 forbidden | EVENTS | applied |
| S3 | M1 search SQL: materialize MATCH → join → snippet post-LIMIT | RETRIEVAL | applied |
| S4 | `image_journal_stats` derived table (badges/chips/tie-break) | EVENTS | applied |
| S5 | Pragma block (cache_size, mmap, busy_timeout) + WAL/statement hygiene | EVENTS | applied |
| S6 | prefix='2 3' (drop 4); scheduled FTS optimize | EVENTS, RETRIEVAL | applied |
| S7 | Index tidy: (target_event, kind); drop idx_events_kind; JSON-column rule | EVENTS | applied |
| S8 | Rebuild discipline: id-sorted, 10k batches, derived-after, ANALYZE | EVENTS, SIDECARS | applied |
| L1 | Ingest budget re-scoped to internal NVMe + slow-volume UX | LIBRARY | applied |
| L2 | FAT/exFAT uniform clock-shift detection (DST re-hash storms) | LIBRARY | applied |
| L3 | Reconciliation triggers: system wake + watcher error | LIBRARY | applied |
| L4 | Embedded-preview orientation verification + fixtures | LIBRARY | applied |
| L5 | Preview→decode color shift; stroke-substrate invariant explicit | LIBRARY | applied |
| L6 | Cloud-sync detection; placeholder/dataless files; exclusions; mtime-stable sidecar writes | LIBRARY, SIDECARS | applied |
| L7 | Priority bump: threshold-miss-with-strokes; CR3 HEIF previews | LIBRARY | applied |
| U1 | silero-vad onset front; binding on VAD onset; spike measures timestamps | CAPTURE, RUNTIME | applied |
| U2 | --ctx-size divided across slots: launch corrected | RUNTIME | applied |
| U3 | Background lane: stream:true mandatory; prompt-phase preemption limits | RUNTIME | applied |
| U4 | /health busy-vs-lost rule; process liveness is ground truth | RUNTIME | applied |
| U5 | P2 wire = float32 + "Done"; Rust-crate wrapper option for spike | RUNTIME | applied |
| U6 | confidence = exp(mean logprob), uncalibrated, optional | RUNTIME, CAPTURE | applied |
| U7 | cpal device-failure paths (default config, watchdog, BT note) | CAPTURE | applied |
| U8 | Thumbnails via custom protocol, never IPC; webview memory criterion | UI | applied |
| U9 | Pressure = progressive enhancement; macOS WKWebView gap; coalesced-events floor | UI, CAPTURE | applied |
| U10 | Mic stream closed (not paused) on disarm; OS-dot interplay + privacy line | CAPTURE, UI | applied |

## Explicit non-issues (researched; do not build)

ANN indexes / vector DBs at our scale · HyDE & query expansion · learned
fusion / LTR · GraphRAG, ColBERT, SPLADE · trigram FTS tokenizer · a general
materialized current-state table (fold-on-read holds to the 5k-events/image
worst case) · switching BLAKE3 → xxh3 (disk-bound either way; merge wants the
cryptographic property) · VRAM fragmentation across llama.cpp restarts ·
inotify/FSEvents capacity at 50k files · WebP decode speed · sidecar-clutter
tolerance · Lightroom-style history bloat (ours is human words, not
per-slider snapshots) · digiKam's "switch to MySQL above 100k" folklore.

Full reports: `docs/research/STORAGE.md`, `docs/research/RAG.md`,
`docs/research/LIBRARY.md`, `docs/research/RUNTIME-UX.md`.
