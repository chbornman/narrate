# P6.3 Model Spike — findings (session 1: Apple Silicon Tier-1 floor)

Machine: **Apple M1 Pro, 16 GB unified, 10 cores** — exactly RUNTIME §12.1's
"one Tier-1-class machine (Apple Silicon 16 GB)". The RTX 5080 half (margo)
and the embedder bake-off (§12.3) are session 2. Harness and pinned
artifacts live in `spike-p6.3/` (gitignored, throwaway per §12); every
number below reproduces from those scripts.

## Headline (deliverable 2): the ASR recipe

**English-only v1, and the serving shape is the Rust-crate wrapper child —
not the vendored websocket server.** Measured grounds:

- Model: `sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8`
  (csukuangfj2, 2026-04-25 export — supersedes the January export the spec
  referenced; the April series ships per-chunk-size exports, we take 160 ms
  per the §12.2 measurement point). Multilingual 3.5 ONNX export remains
  unconfirmed upstream → English v1, fallback order per §3.2 stands.
- **Real-time on CPU, but ONLY with `--num-threads=4`** (ONNX intra-op;
  the flag DEFAULTS TO 1 and single-threaded decode falls behind real time
  ~1.1 s per audio second — lag reached 7.5 s on a 6.6 s clip). At 4
  threads: **partial lag mean 44 ms / p95 ~650 ms / max ~925 ms**, final
  ~0.5–0.6 s after end-of-speech, ~2.5 cores busy, **RSS 1.1 GB**.
- **Disqualifying server bug**: the vendored
  `sherpa-onnx-online-websocket-server` (v1.13.2) mints FINALS that drop
  text its own partials already decoded — last partial carried
  "…here and there the squalid quar of the brothels", the final truncated
  at "…here and " (reproduced ×4, `--reset-encoder` makes no difference).
  CAPTURE mints events from finals; losing decoded words is unacceptable →
  **wrap the sherpa-onnx Rust crate in a tiny child process we own**
  (§3.2's named alternative). Same process boundary, same wire contract.
- **Streaming flush needs tail padding**: the cache-aware transducer holds
  right context; the client must send ~0.8 s of silence before "Done" or
  the last second never decodes (sherpa's own clients do this — the
  connector must too).
- **Token timestamps: USABLE** (§3.2 cross-check unblocked): monotone,
  second-scale, aligned with the VAD onset — e.g. tokens
  `[' A','fter','ly',' n','ight',…]` ↔ `[0.48, 0.48, 1.2, 1.44, 1.44, …]`.
- Quality note: int8 export produced "Afterly" for "After early" and
  "quar" for "quarter" on the test clip — consistent with the published
  ~8.2 % WER; acceptable for journal verbatims (corrections exist by
  design).

## Deliverable 2c: silero-vad

- **Per-chunk inference 0.08 ms mean / 0.37 ms max** (CPU, 512-sample
  chunks @ 16 kHz) — 25× under the spec's ~2 ms working assumption.
- **Onset error +48 ms** (32 ms chunk grid + intrinsic detection delay)
  against CAPTURE §1's 250 ms combined budget → ~200 ms left for clock
  conversion and plumbing. Comfortable.
- **Integration trap for the `ort` wiring**: silero v5's ONNX needs each
  512-sample chunk PREPENDED with the previous chunk's last 64 samples
  (input `[1, 576]`); without the context the model silently returns
  probabilities ≈ 0. The official python wrapper hides this; our
  in-process session must not forget it.

## Deliverable 1 (Apple Silicon half): llama-server + Gemma 4 E4B

`llama-server` b9590 (brew, Metal), `gemma-4-E4B-it-Q4_K_M` + Q8_0 mmproj,
spec §3.1 shape (`--ctx-size 16384 --parallel 2 -ngl 99`):

- **Spawn → `/health` 200: 2.3–3.8 s** (warm/cold). RSS **5.3 GB** after
  load, **6.7 GB** after exercising 16 k context.
- **Generation 34.6 tok/s, prompt 127 tok/s** (Metal).
- **`--reasoning-budget 0` is REQUIRED in the §3.1 command line**: Gemma 4
  E4B is a reasoning model — without the flag every token goes to
  `reasoning_content` and constrained output never produces content (the
  JSON probe scored 0/50). Interactive parses can't afford thinking tokens
  against §9's 2 s budget anyway.

## Deliverable 5: JSON-schema-constrained decoding

**50/50 schema-valid (gate: ≥ 98 %)** on a filter-AST-shaped schema over 50
templated photo-search queries, temperature 0. Mean **2.9 s/query**
unoptimized — over §9's 2 s interactive p95 on THIS machine; levers before
calling it a problem: prompt-cache reuse (system prompt re-tokenized every
call here), tighter max_tokens, and this being the Tier-1 floor rather
than the dev box.

## Deliverable 4 (scoped): ASR + LLM concurrency

Streaming ASR while a 400-token LLM generation runs: **partial lag p95
650 ms vs 656 ms solo** — no interference (LLM on Metal, ASR on CPU: the
unified-memory machine separates the compute domains naturally). Memory is
the real constraint: with both resident (6.7 + 1.1 GB + app + OS) the
compressor was active. **Tier-1 16 GB recommendation: `--ctx-size 8192`
single-slot** unless measurement on the wired app shows otherwise.

## Decisions taken (recorded as B66–B67 in DECISIONS.md)

- **TLS client (B55 open item): `ureq` + rustls.** The download manager is
  a single serialized background worker (B58) doing resumable GETs —
  synchronous fits the pump's blocking model, rustls avoids the platform
  OpenSSL matrix, and reqwest would pull a tokio runtime into a crate that
  deliberately has none.
- **P2 serving shape: owned wrapper child** (grounds above).

## Artifact pins (for the §5.1 manifest when P6.4 wires real downloads)

```
90ce98129eb3e8cc57e62433d500c97c624b1e3af1fcc85dd3b55ad7e0313e9f  gemma-4-E4B-it-Q4_K_M.gguf
51d4b7fd825e4569f746b200fccc5332bf914e8ef7cbe447272ce4fec6df3db6  mmproj-gemma-4-E4B-it-Q8_0.gguf
71111f61b18e1e65e01e369434a5c0434868d2f44892742ae54240600c681209  nemotron .../encoder.int8.onnx
0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98  nemotron .../decoder.int8.onnx
a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23  nemotron .../joiner.int8.onnx
1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3  silero_vad.onnx (v5)
50c5c04d93113602432a13454d6bf8e5d2624206b985fbd0dd4698454ae6c509  sherpa-onnx-v1.13.2-osx-arm64-shared.tar.bz2
```

## Session 2 remainder (not yet measured)

RTX 5080 supervision half (margo, every §8.4 mechanism per platform);
embedder bake-off (DFN5B ort parity + EmbeddingGemma-308M vs
Qwen3-Embedding-0.6B on the R6 golden queries); full §12.4 concurrency
matrix incl. the parse-mid-prompt-processing worst case; multilingual
Nemotron watch.
