# MODELS — the connector-options matrix

One row per seam in the modular toolchain: what runs today, what was
evaluated and why it lost, candidates not yet tested, and the trigger that
reopens the question. Maintained alongside docs/STATUS.md; the visual
companion is docs/architecture.html section 3. Pins (SHAs, revisions) live
in the compiled manifest; decisions in spec/DECISIONS.md.

The standing rule (founder, June 2026): the seams make swaps cheap, so the
landscape gets a deliberate look quarterly or whenever a release moves the
frontier. The Nemotron 3.5 day proved a full swap evaluation costs an
afternoon: batch eval in Python, streamed eval through sherpa, corpus +
Alice WER as the fixed yardstick.

## ASR — streaming speech-to-text (`Transcriber`)

- **Now:** Nemotron speech 0.6B (English) int8, via the owned
  `pp-asr-server` wrapper over sherpa-onnx. Founder-verified live.
  Interim move in flight: the 560 ms export of the same model (B74 root
  cause: the 160 ms export's baked-in lookahead truncates word tails; no
  runtime knob can compensate).
- **Decided target:** Nemotron 3.5 ASR Streaming 0.6B @ 560 ms int8 (B74)
  — native punctuation + capitalization, 40 locales, same architecture.
  Trigger: sherpa-onnx Rust crate release with 3.5 support (runtime in
  their master June 12 2026; official exports published).
- **Evaluated:** 3.5 batch (flawless on corpus) and streamed at
  160/560 ms (docs/SPIKE-ASR35.md).
- **Candidates, untested:** MLX-community ports of 3.5 (Apple-native
  backend option behind the same trait); NVIDIA NIM gRPC (server-class,
  off-thesis for local-first).
- **Yardstick:** founder voice corpus (test-corpora/voice/) + Alice WER
  (test-corpora/voice-long/), streamed.

## VAD — speech gate (`VoiceActivityDetector`)

- **Now:** silero-vad v5, in-process ort, ~2 MB compiled into the binary.
  Founder-verified live. Hysteresis knobs exposed for the tuning harness;
  400 ms pre-roll feeds cold-start first words.
- **Evaluated:** spike P6.3 (0.08 ms/chunk, +48 ms onset error — far
  inside budget). The v5 context-prepend integration trap is documented
  in the connector.
- **Trigger:** only if a future ASR absorbs VAD duties natively (3.5 has
  internal endpointing; the gate also serves binding + privacy, so it
  stays regardless).

## LLM — retrieval fuel + query parse (`LanguageModel`)

- **Now:** Gemma 4 E2B QAT q4_0 + vision projector, via supervised
  llama-server, `--reasoning-budget 0` (spike-mandated). Child runs live;
  its consumers (captions, summaries, NL query parse) light up with M3
  dogfood.
- **Evaluated (B68 bake-off):** E4B (better prose, 2x cost — the tier-2
  config alternative); Qwen3.5-2B (no official GGUF; community export
  crashed pinned llama.cpp — out for v1).
- **Trigger:** M3 quality eval; any small-model release with materially
  better constrained-JSON parsing at <= 4 GB.

## Text embedder — the primary retrieval signal (`Embedder`)

- **Now:** EmbeddingGemma-300m q8, 768-dim, in-process ort (B73 bake-off
  winner: best paraphrase separation, half the vector bytes, 316 MB).
  Wired by P7.4; e2e-proven on real queries.
- **Evaluated:** Qwen3-Embedding-0.6B int8 (8/8 retrieval but weaker
  margins; stays the pinned alternative).
- **Candidates:** llama.cpp `/v1/embeddings` as a backend alternate
  behind the same seam (spec-sanctioned).
- **Yardstick:** paraphrase-margin + retrieval sanity (docs/
  SPIKE-P7-EMBED.md methodology); the real golden-query eval is
  post-dogfood.

## Image embedder — visual search (`Embedder`)

- **Now:** DFN5B CLIP ViT-H-14-378 (Immich's ONNX export), 1024-dim,
  in-process ort. Wired by P7.4; zero-shot structure eye-verified on
  founder images. Laptop CPU backfill ~3 s/image (idle-hours pacing);
  GPU/CoreML execution providers await spike session 2.
- **Evaluated:** feasibility + quality sanity only (the model CHOICE
  is a kernel decision: Immich's larger supported preset).
- **Trigger:** spike session 2 throughput numbers; any Immich preset
  change worth tracking.

## Reranker (`Reranker`)

- **Now:** none — RRF fusion only. Seam and mock exist.
- **Trigger:** go/no-go at the M3 golden-query eval, post-dogfood.

## Cloud partner — M5 paid tier (`LanguageModel`)

- **Now:** none; the free tier never touches the network.
- **Planned:** Claude connector, per-conversation consent. Spec'd, not
  scheduled.
