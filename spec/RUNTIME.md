# spec/RUNTIME.md — The Local Model Runtime

Status: Draft 1, June 2026. Closes gap **E5** in `docs/SPEC-GAPS.md` ("the biggest unbuilt system in the plan"). Normative for crates `photoproof-core` (supervision, config, scheduling) and `photoproof-connectors` (trait implementations).

Boundaries with sibling specs:

- **CAPTURE.md** consumes the `Transcriber` stream; it owns binding rules, session lifecycle, and audio policy. This spec owns the ASR process and the stream contract.
- **RETRIEVAL.md** consumes `Embedder` and `LanguageModel`; it owns the `VectorStore` trait, index recipes, and reindex rules. This spec owns the processes, models, weights, config, and lifecycle behind those traits.
- UI specs own first-run surfaces; this spec defines the first-run *contract* (§10).

---

## 1. Architecture invariants

1. **The app never links LLM or ASR inference.** All generative and speech inference runs in managed external child processes, reached over localhost HTTP/WebSocket. Local↔cloud is symmetric by construction: swapping a backend is a config change, never a code change.
2. **At most two managed external processes** (the kernel allows three; we need two):
   - **P1 — llama.cpp `llama-server`**: hosts the small LLM (text + multimodal projector for captions), OpenAI-compatible HTTP.
   - **P2 — ASR server**: hosts Nemotron streaming ASR (serving recipe in §3.2; the contract is hard now, the recipe is the M2b-spike deliverable).
   - **Nothing else.** No Python anywhere. No Docker anywhere.
3. **One sanctioned exception to the no-linking rule: the in-process `ort` components** — the embedders and silero-vad — run in-process via ONNX Runtime (`ort` crate). Defense in §3.3.
4. **Below the hardware floor the app is fully functional as a journal.** Tier 0 = typed notes + grease pencil + FTS5 search, no models, no downloads, no degradation of any M1 feature. Stated loudly: **degraded mode is not a crippled app; it is exactly M1, which is a complete product.** Every runtime failure path lands here, quietly.
5. **No model ships in the installer.** Weights are downloaded on demand, with explicit consent, resumable and checksummed (§5). The installer stays small; first run works instantly in Tier 0 mode while downloads proceed.
6. All inference endpoints bind to `127.0.0.1` on random ports. Nothing listens on external interfaces, ever. ("Free tier never touches the network" refers to user data; weight download is the one sanctioned egress, consent-gated, and carries no user data.)

## 2. Research findings (verified June 2026)

These findings anchor the model and serving decisions below.

- **Embedder model.** Immich's community model guide recommends **`ViT-H-14-378-quickgelu__dfn5b`** (Apple DFN5B-CLIP-ViT-H-14 at 378 px) as its top-quality smart-search preset — quality score 0.828, 542 GMACs per image, **1024-dimensional embeddings**; the 224 px sibling `ViT-H-14-quickgelu__dfn5b` is the cheaper variant of the same class. Immich serves these via ONNX Runtime, which is precisely the recipe we adopt. Sources: [immich discussion #11862](https://github.com/immich-app/immich/discussions/11862), [immich discussion #17135](https://github.com/immich-app/immich/discussions/17135), [DFN5B-CLIP-ViT-H-14-378 model card](https://www.aimodels.fyi/models/huggingFace/dfn5b-clip-vit-h-14-378-apple).
- **llama.cpp server.** `llama-server` provides OpenAI-compatible `/v1/chat/completions`, `/v1/completions`, and `/v1/embeddings`, plus `/health`; it supports structured **JSON-schema-constrained output** (`response_format`/grammar) and **multimodal input via `--mmproj` projector files — currently flagged experimental** in the OAI-compatible chat endpoint. Multimodal *embeddings* are non-OAI and unsuitable as our embedder seam. Sources: [llama.cpp server README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md), [llama.cpp repo](https://github.com/ggml-org/llama.cpp). Consequence: llama.cpp's CLIP support is a *VLM projector*, not a contrastive dual-encoder — it cannot serve DFN5B-class image/text embeddings. The embedder must be served elsewhere (§3.3).
- **Nemotron ASR serving.** The Nemotron streaming-ASR family (cache-aware FastConformer-RNNT, 0.6B, chunk sizes 80/160/560/1120 ms) has a working desktop path: ONNX export (encoder/decoder/joiner as three sessions) running CPU-only in real time — int8/int4 quantizations reach ~8.2% WER in ~0.7 GB — and the **English model has a published sherpa-onnx conversion** (`sherpa-onnx-nemotron-speech-streaming-en-0.6b`). ONNX export of the *multilingual* `nemotron-3.5-asr-streaming-0.6b` is not yet confirmed upstream (open HF discussion). Sources: [nemotron-speech-streaming-en-0.6b](https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b), [nemotron-3.5 ONNX discussion](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/discussions/1), [sherpa-onnx conversion](https://huggingface.co/csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-2026-01-14/blob/main/README.md), [CPU streaming pipeline paper](https://arxiv.org/html/2604.14493v1).
- **Unverified, carried as assumptions:** exact VRAM/throughput numbers per tier (§5.4, §6 — the spike measures them); multilingual-Nemotron ONNX export viability (§3.2); DFN5B license-acceptance mechanics (§5.3).

## 3. The processes

### 3.1 P1 — llama.cpp server (`llama-server`)

Hosts the small LLM behind `LanguageModel`. One server, one model resident.

- Binary: pinned `llama-server` builds vendored per platform inside the app bundle (CUDA, Metal, Vulkan, CPU variants; the supervisor picks at spawn time from detected hardware). The *binary* ships with the app; only *weights* are downloaded.
- Launch (illustrative; flags resolved from config + tier):

  ```
  llama-server --host 127.0.0.1 --port {alloc} \
    --model {models}/gemma-4-e4b-it/gemma-4-e4b-it-Q4_K_M.gguf \
    --mmproj {models}/gemma-4-e4b-it/mmproj-f16.gguf \
    --ctx-size 16384 --parallel 2 --gpu-layers {auto|n} \
    --no-webui --log-disable
  ```

- Endpoints used: `/health` (readiness); `/v1/chat/completions` for completion, caption (image content parts via mmproj), and JSON-schema-constrained query parsing (`response_format: {type:"json_schema", ...}`). `/v1/embeddings` is *reserved* (§3.3 text-embedding note) but unused by default.
- `--parallel 2` gives two server slots; the app-side scheduler (§9) maps them to one interactive lane + one background lane. Multimodal (caption) requests are background-lane only.
- **`--ctx-size` is divided across parallel slots** ([llama.cpp #11681](https://github.com/ggml-org/llama.cpp/issues/11681)): 16384 with `--parallel 2` yields **8192 per lane** — the per-lane context the rest of this spec assumes. The doubled total context roughly doubles KV-cache VRAM versus a naïve 8192 budget; the Tier-1 numbers absorb this conservatively (§6.2). Slot semantics have churned upstream ([#17989](https://github.com/ggml-org/llama.cpp/issues/17989)); **verify the divide-vs-share behavior against the pinned build** at vendoring time.
- Quantization: **Q4_K_M** for all GGUF weights. Rationale: the best quality/size point of the K-quant family in community evals, universally published for the target models, and it fits the Tier-1 budget; Q5/Q6 buys little for summarization/parsing at ~25–50% more VRAM; below Q4 measurably degrades instruction following, which schema-constrained query parsing depends on.

### 3.2 P2 — ASR server (Nemotron streaming)

**Primary recipe: `sherpa-onnx` online (streaming) WebSocket server** running the Nemotron 0.6B streaming model as int8 ONNX, **CPU execution provider by default on every tier** (GPU optional via config). Rationale:

- It is the only Python-free, container-free, desktop-grade path that exists today (verified, §2). NVIDIA NIM/Riva containers are rejected: Docker on a consumer desktop violates invariant 1.2's spirit and the installation reality of this audience.
- CPU-resident ASR is a *feature*, not a fallback: a 0.6B cache-aware streaming model is real-time on laptop CPUs at int8, and keeping ASR off the GPU removes the worst VRAM contention (live mic vs. LLM) by construction (§9).
- Binary: pinned `sherpa-onnx-online-websocket-server` (static, per platform) vendored in the app bundle, spawned on a random localhost port. **Wire protocol (corrected): raw float32 sample frames** in over the WebSocket, with a `"Done"` text message signaling end-of-stream — the 16-bit/16 kHz language in sherpa's docs describes wave *files*, not the socket ([online WebSocket docs](https://k2-fsa.github.io/sherpa/onnx/websocket/online-websocket.html)). Result JSON carries `text`, `tokens`, `timestamps`, `segment`, `start_time`, `is_final` ([C API](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/c-api/c-api.h)); the connector maps `segment` → `utterance_id` and adapts the rest to the `Transcriber` contract.
- **Serving shape is a spike decision (§12.2).** The vendored websocket server is closer to a reference binary than a hardened server. The alternative: wrap the official [sherpa-onnx Rust crate](https://crates.io/crates/sherpa-onnx) in a **tiny purpose-built child process we own** — same process boundary, same wire contract, drops the demo-grade server. Either way P2 stays an external child (invariant 1.1). The spike MUST also test whether the Nemotron export emits usable token `timestamps` — unverified ([discussion #985](https://github.com/k2-fsa/sherpa-onnx/discussions/985)); CAPTURE's binding no longer depends on them (VAD onset is authoritative, CAPTURE §5), but the cross-check wants them.
- Model: English `nemotron-speech-streaming-en-0.6b` int8 (published sherpa-onnx export) as the v1 default. The multilingual `nemotron-3.5-asr-streaming-0.6b` is the *target* default per SCOPE; its ONNX/sherpa export is unconfirmed, so: **the Transcriber contract (§4.1) is normative now; the multilingual serving recipe is an explicit M2b-spike deliverable (§12).** Fallback order: multilingual 3.5 → English 0.6b → ASR disabled (voice features dark, journal unaffected).
- Chunk size: 160 ms default (config `chunk_ms`), trading ~200 ms extra latency for roughly half the CPU of 80 ms chunks.

### 3.3 In-process ONNX Runtime — embedders + silero-vad (the defended exception)

`Embedder` is implemented **in-process** via the `ort` crate (ONNX Runtime), loading the visual and text towers of `ViT-H-14-378-quickgelu__dfn5b` as two ONNX sessions (the same artifact layout Immich uses).

Why this is the right exception to "never link inference":

- llama.cpp cannot serve contrastive CLIP embeddings (§2); the alternatives are a Python server (banned), or a third bespoke C++ embedding server (weeks of work to reimplement what `ort` gives us in an afternoon).
- The encoder is small, **deterministic, fixed-shape, stateless** — no KV cache, no sampling, no streaming, no prompt surface. It is the closest thing in the stack to "a B-tree": exactly the component where in-process is proportionate.
- ONNX Runtime running these exact models is production-proven at scale in Immich.

The trade-off, stated honestly: **a native crash inside ONNX Runtime crashes Photoproof** — there is no process boundary to absorb it. Mitigations: pin the `ort`/ONNX Runtime version per release; default to the CPU execution provider with GPU (CUDA/CoreML/DirectML) as a tier-promoted opt-in once the spike validates stability; run all embedding work on a dedicated thread with inputs pre-validated to fixed shapes; the embedder runs only in background passes, never in the capture path, so a crash can never lose an annotation (events are durable before embedding starts).

**silero-vad joins the `ort` exception (capture path — the explicit carve-out).** A silero-vad session (~1 MB, ~2 ms per audio chunk — [measured latency](https://rajatpandit.com/agentic-ai/real-time-audio-vad/)) runs in-process on the cpal capture stream for **speech-onset detection ahead of P2**, plus silence gating and the "speaking" indicator affordance; **P2 keeps endpointing/segmentation authority** (CAPTURE §5–6). The embedder defense above leans on "background passes only, never the capture path" — silero-vad is the deliberate exception to that clause, and it earns it the same way: tiny, deterministic, fixed-shape, stateless across chunks from the caller's view. A native crash in it costs the armed mic (`Disarmed(error)`, CAPTURE §6.6), never durable data — annotations are events before anything model-shaped touches them.

**Two embedders (resolved cross-spec).** The DFN5B text tower's 77-token CLIP context cannot honor RETRIEVAL's ~512-token annotation chunks — and annotation text is the product's *primary* signal. The runtime therefore hosts **two embedder instances behind the same `Embedder` trait**: the **CLIP embedder** (DFN5B preset above — `image_clip` vectors and short S4 query embeddings only) and the **text embedder** — a small dedicated text-embedding model (Qwen3-Embedding-0.6B-class, ONNX int8, ~0.6 GB) run in-process via the same defended `ort` exception (same determinism/fixed-shape argument applies; it is also a background-pass-and-query-time component, never in the capture path). Alternative backend behind the same seam: a GGUF embedding model on llama.cpp's `/v1/embeddings`. RETRIEVAL §3 assigns vec_kinds: `annotation_chunk` + `image_summary` → text embedder; `image_clip` → CLIP embedder.

**Instruction-prefix contract (text embedder).** Instruction-aware text embedders lose measurable quality without their query instruction (the Qwen3-Embedding card reports 1–5% — [card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)). Normative split: **queries are embedded with the model's instruction prefix; documents are embedded bare.** The exact template, its version, and its place in `inputs_hash` are owned by RETRIEVAL.md (amendment R1); the runtime embedder applies the configured template verbatim and never invents one.

## 4. Connector traits (normative Rust)

Crate `photoproof-connectors`. Rust 2024, native `async fn` in traits; streaming uses `futures_core::stream::BoxStream` for object safety. All connectors are `Send + Sync` and selected at startup from config (§4.4).

```rust
use std::time::Duration;
use futures_core::stream::BoxStream;

/// Unified connector error. Every variant maps to a supervision or
/// degradation behavior; none of them ever surfaces as user-facing prose.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// Backing service not Ready (starting, restarting, or Failed).
    /// Callers must treat the feature as unavailable, not retry-loop.
    #[error("backend not ready: {0}")]
    NotReady(&'static str),
    /// Transport-level failure mid-call (process died, socket closed).
    /// The supervisor restarts; callers may retry exactly once (§13).
    #[error("backend connection lost")]
    ConnectionLost(#[source] std::io::Error),
    #[error("backend timeout after {0:?}")]
    Timeout(Duration),
    /// Non-2xx or protocol-level error from the backend.
    #[error("backend error {status}: {message}")]
    Backend { status: u16, message: String },
    /// Response arrived but could not be decoded (bad JSON, schema
    /// violation after constrained decoding — a bug, log loudly).
    #[error("malformed backend response: {0}")]
    Decode(String),
    /// Cloud backends only: key ref unresolvable, 401/403.
    /// Never contains key material.
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("cancelled")]
    Cancelled,
}

pub type ConnectorResult<T> = Result<T, ConnectorError>;

// ---------------------------------------------------------------- ASR ----

/// Milliseconds relative to the stream clock: 0 = the instant `stream()`
/// accepted its first audio frame. CAPTURE.md maps stream time to wall
/// time and to selection snapshots (VAD-onset binding, gap B1).
pub type StreamMs = u64;

#[derive(Debug, Clone)]
pub struct TranscriptSegment {
    /// Stable id per utterance: partials and the final for one utterance
    /// share an `utterance_id`; the final replaces all partials.
    pub utterance_id: u64,
    pub kind: SegmentKind,
    pub text: String,
    /// VAD speech onset for this utterance (selection-snapshot anchor).
    pub onset: StreamMs,
    /// End of speech covered by this segment so far.
    pub end: StreamMs,
    /// exp(mean token log-prob), in [0,1]. OPTIONAL: `None` when the
    /// backend exposes no token log-probs (sherpa-onnx `ys_probs` only
    /// recently landed — k2-fsa/sherpa-onnx#2897 — and has hotword quirks,
    /// #2937). Explicitly UNCALIBRATED: a score, not a probability; never
    /// compare values across model versions. Stored per event per the
    /// kernel when present. Partials may carry a provisional value.
    pub confidence: Option<f32>,
    /// BCP-47 of the recognized language, when the model reports it.
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Mutable hypothesis; UI may use it for the capture pulse, never stored.
    Partial,
    /// Immutable; becomes an annotation event. Audio for this utterance
    /// is discarded after finalization per the kernel.
    Final,
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// f32 samples, mono, at `Transcriber::sample_rate()` — matches both
    /// CAPTURE's resampled feed and the P2 float32 wire format (§3.2);
    /// no i16 round-trip anywhere in the path.
    pub samples: Vec<f32>,
    pub captured_at: StreamMs,
}

pub trait Transcriber: Send + Sync {
    /// Open one streaming session. Frames go in; segments come out.
    /// The output stream ends after the input closes and the last Final
    /// is emitted, or yields Err and ends on connection loss (the caller
    /// re-arms the mic only after the supervisor reports Ready again).
    fn stream<'a>(
        &'a self,
        audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>>;

    /// Required input sample rate (Nemotron: 16_000).
    fn sample_rate(&self) -> u32;
    fn model_id(&self) -> &str;
}

// ----------------------------------------------------------- Embedder ----

#[derive(Debug, Clone)]
pub struct Embedding {
    pub vector: Vec<f32>,        // L2-normalized
    pub model_id: String,        // e.g. "ViT-H-14-378-quickgelu__dfn5b"
}

/// Decoded, display-oriented sRGB pixels (LIBRARY.md owns decode).
pub struct DecodedImage {
    pub rgb8: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Two configured instances exist behind this one trait (§3.3): the TEXT
/// embedder (annotation chunks & summaries; embed_image unsupported) and the
/// CLIP embedder (image vectors; embed_text = short queries only — its CLIP
/// text tower truncates at 77 tokens, ample for queries, never for chunks).
pub trait Embedder: Send + Sync {
    async fn embed_text(&self, text: &str) -> ConnectorResult<Embedding>;
    async fn embed_image(&self, img: &DecodedImage) -> ConnectorResult<Embedding>;
    /// 1024 for the DFN5B ViT-H-14 presets; per the configured text model
    /// for the text instance. Stored with every vector.
    fn dimensions(&self) -> usize;
    fn model_id(&self) -> &str;
}

// ------------------------------------------------------ LanguageModel ----

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,           // system/user/assistant
    pub max_tokens: u32,
    pub temperature: f32,
    /// When set, decoding is constrained to this JSON Schema (llama.cpp
    /// `response_format: json_schema`; cloud adapters map to their native
    /// structured-output mechanism). Query parsing requires it.
    pub json_schema: Option<serde_json::Value>,
    pub priority: Lane,                       // see §9
}

#[derive(Debug, Clone)]
pub struct ChatMessage { pub role: Role, pub content: String }
#[derive(Debug, Clone, Copy)] pub enum Role { System, User, Assistant }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane { Interactive, Background }

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub model_id: String,
}

pub trait LanguageModel: Send + Sync {
    async fn complete(&self, req: ChatRequest) -> ConnectorResult<ChatResponse>;
    /// Caption for retrieval fuel only (kernel: never user-facing prose).
    /// Local impl: multimodal chat call via the mmproj projector;
    /// always Lane::Background.
    async fn caption_image(
        &self,
        img: &DecodedImage,
        prompt: &str,
    ) -> ConnectorResult<String>;
    fn model_id(&self) -> &str;
}

// VectorStore is owned by RETRIEVAL.md; referenced here only so the
// dependency direction is explicit: vectors reference events; embeddings
// never appear in event rows or sidecars (kernel).
```

### 4.4 Config-driven backend selection — literal TOML schema

`{app-data}/config.toml`, runtime-relevant sections, defaults shown. **API keys are never stored in this file**: `api_key_ref` is an OS-keychain reference (`keychain:<service>/<account>`), resolved at call time via the platform keychain (macOS Keychain, Windows Credential Manager, Secret Service on Linux). A literal key found in config is rejected at parse time with a hard error.

```toml
[runtime]
tier = "auto"            # "auto" | 0 | 1 | 2  — user override always wins
models_dir = ""          # default: {app-data}/models
vram_headroom_mb = 2048  # never plan above (detected VRAM - headroom), §9

[llm]
backend = "local-llamacpp"   # "local-llamacpp" | "openai-compatible" | "anthropic"
model = "gemma-4-e4b-it-q4_k_m"   # manifest model id (local) or API model name

[llm.local-llamacpp]
ctx_size = 16384             # TOTAL; divided across slots (#11681) → 8192/lane
parallel_slots = 2           # 1 interactive + 1 background lane
gpu_layers = "auto"          # "auto" | integer | 0 (CPU)
startup_timeout_secs = 120

[llm.openai-compatible]      # any OAI-speaking endpoint: vLLM, Ollama, hosted
base_url = "http://127.0.0.1:11434/v1"
api_key_ref = ""             # optional; "keychain:photoproof/openai-compat"

[llm.anthropic]              # M5: thin adapter behind LanguageModel —
base_url = "https://api.anthropic.com"          # maps ChatRequest→Messages API,
api_key_ref = "keychain:photoproof/anthropic"   # json_schema→tool-forced
model = "claude-sonnet-latest"                  # structured output.

[asr]
backend = "local-sherpa"     # "local-sherpa" | "disabled"
model = "nemotron-speech-streaming-en-0.6b-int8"
chunk_ms = 160               # 80 | 160 | 560 | 1120 (model-supported)
device = "cpu"               # "cpu" (default, all tiers) | "gpu"

[embedder]
backend = "local-ort"        # "local-ort" | "openai-compatible"
model = "ViT-H-14-378-quickgelu__dfn5b"   # or ViT-H-14-quickgelu__dfn5b (224px)
device = "auto"              # "auto" | "cpu" | "gpu"

[embedder.text]              # the text-embedding model (§3.3): annotation
backend = "local-ort"        # chunks & summaries. "local-ort" |
model = "qwen3-embedding-0.6b-int8"  # "local-llamacpp" (/v1/embeddings) |
device = "cpu"               # "openai-compatible"
                             # query/document instruction prefixes: template
                             # owned by RETRIEVAL (R1); queries instructed,
                             # documents bare (§3.3)
```

Unknown keys warn; missing sections take defaults. A config naming an uninstalled model puts that connector in `NotConfigured` (feature dark, journal unaffected) and surfaces the fix in settings + debug panel.

## 5. Weight distribution

### 5.1 Manifest

Each app release embeds a **model manifest** (compiled in; also written to `{models_dir}/manifest.json` for the debug panel). Upgrading models = shipping a manifest bump with an app update — never silent (§10, §11).

```json
{
  "manifest_version": 3,
  "models": [
    {
      "id": "gemma-4-e4b-it-q4_k_m",
      "role": "llm",
      "tiers": [1, 2],
      "license": { "name": "Gemma Terms of Use",
                   "url": "https://ai.google.dev/gemma/terms",
                   "acceptance_required": true },
      "total_bytes": 5460000000,
      "files": [
        { "repo": "hf:ggml-org/gemma-4-e4b-it-GGUF",
          "path": "gemma-4-e4b-it-Q4_K_M.gguf",
          "sha256": "<64 hex>", "bytes": 4870000000 },
        { "repo": "hf:ggml-org/gemma-4-e4b-it-GGUF",
          "path": "mmproj-gemma-4-e4b-it-f16.gguf",
          "sha256": "<64 hex>", "bytes": 590000000 }
      ]
    }
  ]
}
```

Rules: every file pinned to an **exact filename + SHA-256 + byte size** at an immutable Hugging Face revision (`repo` resolves to `https://huggingface.co/{org}/{name}/resolve/{pinned_revision}/{path}`). No wildcards, no "latest", no branch refs.

### 5.2 Download manager

- Resumable: HTTP `Range` into `{file}.part`; on relaunch, resume from the part-file length. Verify SHA-256 over the complete file; mismatch deletes and re-downloads (one automatic retry, then surface in settings/debug panel).
- Atomic: a verified file is renamed into place; a model is "installed" only when **all** its files verify; recorded in `{models_dir}/installed.json` (`model_id → {manifest_version, when}`).
- Concurrency: one file at a time, background priority, throttled while a capture session is live.
- Layout: `{models_dir}/{model_id}/{files…}`. Download activity logs to §8.6.

### 5.3 Licenses (surfaced before download, per model)

| Model | License | UI obligation |
|---|---|---|
| Gemma 4 E4B / 26B | Gemma Terms of Use | **Display + explicit acceptance** before download (Google requires terms agreement). |
| Qwen 3.6 variants | Apache-2.0 | Display notice; no acceptance gate. |
| Nemotron ASR 0.6B | NVIDIA Open Model License | **Display + explicit acceptance** before download. |
| DFN5B-CLIP ViT-H-14 | Apple license per model card | Display notice; acceptance gate if the card requires agreement (spike confirms exact terms). |

Acceptance is recorded (model id, license url, timestamp) in app data. License texts/links remain viewable in settings.

### 5.4 Disk budget, stated to the user before download

| Tier | Bundle | Approx. download |
|---|---|---|
| 1 | LLM E4B Q4_K_M + mmproj (~5.5 GB) + ASR int8 (~0.8 GB) + DFN5B-378 ONNX (~2.6 GB) + text embedder int8 (~0.6 GB) | **~9.5 GB** |
| 2 (optional upgrade) | + quality LLM (Gemma 4 26B MoE Q4_K_M ~16 GB *or* Qwen 3.6-35B-A3B Q4_K_M ~20 GB) | **+16–20 GB** |

Table values are planning estimates; the consent dialog always shows the live manifest sum (`Σ total_bytes`), and the spike (§12) replaces estimates with measured sizes.

## 6. Hardware tiers

### 6.1 Detection recipe (first run, and on demand)

1. **GPU + VRAM**: enumerate adapters via `wgpu`; for byte-accurate VRAM query the native layer: Vulkan `VkPhysicalDeviceMemoryProperties` (largest `DEVICE_LOCAL` heap), Windows DXGI `QueryVideoMemoryInfo`/`DedicatedVideoMemory`, macOS Metal `recommendedMaxWorkingSetSize`.
2. **Apple Silicon**: unified memory = `sysctl hw.memsize`; usable model budget = 60% of total RAM (Metal working-set heuristic), so 16 GB unified ≈ 9.6 GB budget ⇒ Tier 1.
3. **CPU fallback reality check**: ASR-on-CPU is real-time and good (§2). LLM-on-CPU is *not* acceptable for interactive query parsing (tens of seconds per E4B parse on a laptop CPU) and only marginal for background summaries. Therefore CPU-only machines are Tier 0 by default; a config override may enable CPU ASR (voice without semantic search) or even CPU LLM (summaries trickle overnight) — supported, never proposed.
4. The result is cached; re-detected when hardware changes or on user request.

### 6.2 Decision table (normative)

| Tier | Hardware gate | ASR | LLM | Embedder | Experience |
|---|---|---|---|---|---|
| **0** | < 8 GB dedicated VRAM and < 16 GB Apple unified; or no GPU | — | — | — | **Full journal**: typed notes, grease pencil, ratings, sessions, sidecars, FTS5 search. No downloads. This is M1, complete. |
| **1** | 8–12 GB VRAM, or Apple Silicon ≥ 16 GB unified | Nemotron 0.6B int8, **CPU** | Gemma 4 E4B-class Q4_K_M (or small Qwen 3.6), GPU | DFN5B ViT-H-14-378, GPU (background only) | Voice capture, summaries, captions, embeddings, NL query parse. |
| **2** | ≥ 16 GB VRAM (or ≥ 32 GB Apple unified) | same as Tier 1 | Optional quality upgrade: Gemma 4 26B MoE (A4B) Q4_K_M at 16–24 GB; Qwen 3.6-35B-A3B Q4_K_M at ≥ 24 GB (low-active-param MoE tolerates partial CPU offload) | same | Tier 1 plus better summaries/parsing. Offered, never auto-applied. |

- **KV-cache note (post-#11681 correction):** the corrected launch doubles *total* context to 16384 to preserve 8192/lane, roughly doubling KV-cache VRAM versus the draft budget. Tier 1 plans for this conservatively — the E4B-class bundle plus doubled KV must still fit under `detected_VRAM − vram_headroom_mb` at the 8 GB gate; the spike (§12.1) measures the real number and may lower `gpu_layers` defaults rather than the gate.
- 6–8 GB cards: Tier 0 by default; the settings override to Tier 1 is honored with `ctx_size = 8192` (**4096/lane** post-division — the extra KV of a 16384 total does not fit here) and the 224 px embedder variant.
- **User override always wins** (`[runtime] tier`), in both directions. Overriding above detected hardware shows a one-time plain warning.
- Tier selection changes which manifest entries are *offered*; it never deletes installed weights.

## 7. Degraded mode (Tier 0 and every failure path)

Typed notes, grease pencil, ratings, sessions, sidecars, merge, export, FTS5 search, structured filters — all fully functional with **zero** model processes. Voice toggle, semantic search, and NL queries are simply absent or quietly disabled (§8.3 affordance). Any runtime failure — download impossible, process `Failed`, model corrupt — degrades that specific feature to this baseline; **nothing about journaling ever blocks on the runtime.** This is a restatement of M1 as the permanent safety floor.

## 8. Process supervision

One supervisor task per managed process inside `photoproof-core`.

### 8.1 State machine (per process)

```
                      ┌──────────────────────────────────────────────┐
                      ▼                                              │
NotConfigured ─► Downloading ─► Spawning ─► Starting ─► Ready ─► Stopping ─► Stopped
   (no weights /      │            │  ▲        │  ▲        │
    tier 0 /          │ dl error   │  │        │  │ health │ process exit /
    disabled)         ▼            │  │        │  │   ok   │ health lost
                  DownloadFailed   │  └─Backoff◄┘  │        ▼
                  (retry/consent)  │     ▲   │     └──── Restarting(n)
                                   │     │   │ max attempts   │
                                   ▼     │   ▼                │
                                 Failed ◄┴────────────────────┘
                            (feature degraded; debug panel detail)
```

- `Spawning`: allocate port (§8.2), spawn child, attach log capture.
- `Starting`: poll health (P1: `GET /health` until 200 — llama.cpp returns 503 while loading; P2: WebSocket handshake open/close) every 500 ms up to `startup_timeout_secs` (default 120 s — first load from a cold HDD is slow). Timeout ⇒ kill ⇒ `Restarting`.
- `Ready`: liveness check every 5 s; an in-flight `ConnectionLost` triggers an immediate check. **`/health` requests queue behind inference under load** ([#20684](https://github.com/ggml-org/llama.cpp/issues/20684)), so a slow or timed-out health probe while requests are in flight means **Busy, not Lost** — no restart. Restart triggers are exactly three: child-process exit (`waitpid` — the ground truth for liveness), connection refused, or health failure while **no** request is in flight. An optional `/slots` cross-check MAY feed the debug panel but never triggers a restart — it can itself hang while `/health` is green ([#20921](https://github.com/ggml-org/llama.cpp/issues/20921)).
- `Restarting(n)`: exponential backoff 1, 2, 4, 8 … capped at 60 s; **max 5 attempts per rolling 10 minutes**, then `Failed`.
- `Failed`: feature-degrades quietly (no dialog, no toast storm); full detail — state history, last 200 log lines, exit codes — in the dev-build debug panel. A "restart runtime" action in settings re-enters `Spawning` with a fresh attempt budget.

### 8.2 Port allocation

Random localhost port, never fixed: bind a `TcpListener` to `127.0.0.1:0`, read the assigned port, drop the listener, pass the port to the child. The bind race is accepted; if the child fails to bind, that is a spawn failure and `Restarting` picks a new port. Ports are never written to config; connectors get them from the supervisor in memory.

### 8.3 Readiness gating (UI contract)

A UI feature appears/enables only when its backing service is `Ready`:

- Mic toggle disabled until ASR is `Ready` — a **quiet affordance**: the control renders dimmed with a "warming up" tooltip; no spinner takes over any surface, per the quiet-UI principle.
- NL-query parsing falls back to FTS+vector (RETRIEVAL's parse-failure path) while the LLM is not `Ready`; plain keyword search needs nothing.
- Background passes (summaries, captions, embeddings) don't schedule until their dependencies are `Ready`.

Readiness changes are events on the core bus; the UI subscribes; the debug panel shows raw states.

### 8.4 Clean shutdown — children die with the parent

- **Windows**: a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; children are assigned at spawn; the OS kills them even if the app crashes.
- **Linux**: `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec`, plus the process-group kill below.
- **macOS** (no job objects, no pdeathsig): children spawn in their own process group; normal shutdown sends SIGTERM, then SIGKILL after 5 s.
- Crash-orphan safety net (all platforms): each child's PID + process start-time + port is recorded in `{app-data}/runtime/children.json`; on every app start, stale entries are checked (PID alive **and** start-time matches — never kill a recycled PID) and orphans are killed before any spawn.
- Normal order: stop accepting requests → drain in-flight (≤ 5 s) → SIGTERM/CTRL-BREAK → wait 5 s → SIGKILL → reap → clear `children.json`.

### 8.5 Single-instance discipline

Tauri single-instance plugin plus an exclusive OS file lock on `{app-data}/runtime/instance.lock`. A second launch never reaches the supervisor: it forwards focus to the first instance and exits. Supervisors additionally refuse to spawn unless the lock is held (belt and braces); the `children.json` net (§8.4) catches anything that slips through.

### 8.6 Log capture

Child stdout/stderr piped to `{app-data}/logs/runtime/{process}.log`, rotated at 5 MB × 5 files. Supervisor decisions (state transitions, restart reasons, health latencies) log to the same directory. The dev-build debug panel tails these; release builds keep writing them (a support artifact) but expose no UI.

## 9. VRAM arbitration

We can only arbitrate **our own** usage. Whether Capture One or darktable needs VRAM is **not detectable** from our process, and this spec does not pretend otherwise. The honest contract:

- **Static headroom budget**: the runtime plans against `detected_VRAM − vram_headroom_mb` (default 2048 MB, §4.4). The Tier-1 bundle is sized so the floor config fits alongside Capture One per SCOPE; users with heavier GPU editing loads raise the headroom or lower `gpu_layers`, and the first-run tier proposal says so in one sentence.
- **Priority order (our usage)**: `live ASR session > interactive LLM call (query parse) > background passes (summaries, embeddings, captions)`.
- **Mechanism — one LLM server, priority lanes, no second instance.** Separate model instances are too expensive (double weights), and load/unload thrash is worse. So: one `llama-server` with `--parallel 2`; the app-side **pass scheduler** owns every request:
  - `Lane::Interactive` (query parse): queue depth 1, head-of-line; if it would otherwise wait more than 250 ms, the background slot's current request is *cancelled* (HTTP disconnect — llama.cpp frees the slot) and re-queued.
  - `Lane::Background` (summaries, captions): single in-flight request, bounded `max_tokens`, scheduled only when no interactive call is queued. **All background-lane requests MUST set `stream: true`** — llama-server only detects a client disconnect (and frees the slot) for streaming requests; a non-streaming request is not cancellable mid-flight ([#9273](https://github.com/ggml-org/llama.cpp/issues/9273)).
  - **Honesty clause — prompt processing is not preemptible.** Disconnect-cancellation takes effect during token *generation*; a request still in its prompt-processing phase may run that phase to completion regardless ([ik_llama #1020](https://github.com/ikawrakow/ik_llama.cpp/issues/1020)). Therefore background prompts are kept deliberately small — a caption is one image plus a short prompt, never a batch — so the worst-case non-preemptible window stays bounded.
- **Mic armed ⇒ background passes pause.** ASR is CPU-resident (§3.2), so this is mostly CPU/memory-bandwidth hygiene, but the rule is unconditional: while the mic is armed, the scheduler holds background LLM calls and embedding batches; in-flight items finish (bounded) and nothing new starts. Unpause at mic disarm + 5 s.
- **Embedder coexists**: the ort embedder (GPU EP) runs only in background passes, batch size set by the spike; on allocation failure it falls back to CPU EP for the rest of the pass and records the event for the debug panel.

## 10. First-run UX contract (UI owns the surfaces; this is the contract)

1. The app opens **directly into the working app** (Tier 0 behavior): watched roots, grid, typed notes, pencil, FTS. Nothing blocks on the runtime.
2. Hardware detection runs (sub-second); one quiet card proposes a tier: "This machine can run Photoproof's local voice & search models (~9.5 GB download). Download now / Later / Never." — with per-model license display, required acceptances (§5.3), and the disk budget from the live manifest (§5.4).
3. **No download starts without explicit consent.** "Later" re-offers from settings only; "Never" is remembered.
4. Downloads run in the background with progress visible in settings (and debug panel); journaling continues untouched.
5. **Features light up as ready**, individually, per §8.3: ASR weights verified → P2 spawns → mic toggle enables; LLM `Ready` → NL parse activates; embedder ready → RETRIEVAL's backfill passes begin. No fanfare — controls simply become available.
6. The degraded path never blocks journaling, at any step, ever.

## 11. Model updates, versioning, GC

- Model upgrades are **explicit**: a new app release ships a manifest with a bumped `manifest_version` and newly pinned files. No runtime auto-upgrade, no background manifest fetch.
- On a manifest bump: new weights download (same consent rules if licenses changed; otherwise a settings notice); the old model keeps serving until the new one is **verified** (checksums pass, process reaches `Ready` once).
- **Old weights are garbage-collected only after** (a) the new model is verified, **and** (b) for embedders, RETRIEVAL's reindex over the new `model_id` is complete (old vectors stay queryable until then — RETRIEVAL.md owns that rule; the runtime asks "may I GC model X?" via a core-bus query).
- **Never delete during an active pass**: GC takes the same scheduler lock as background passes; a model with any in-flight or queued work is untouchable.
- `installed.json` tracks which manifest version installed each model; orphaned directories (no manifest entry, no reindex hold) are listed in settings ("reclaim 4.9 GB") rather than silently deleted.

## 12. The M1-parallel runtime spike (throwaway code; findings PR updates this spec)

Deliverables, each with a measured number:

1. **Supervision proof**: spawn/supervise `llama-server` with Gemma 4 E4B-class Q4_K_M on the dev box (RTX 5080) and one Tier-1-class machine (Apple Silicon 16 GB). Demonstrate: random-port spawn, health gating, kill-mid-call → restart → single-retry success, parent-kill → zero orphans (exercising every §8.4 mechanism per platform).
2. **ASR recipe selection**: run sherpa-onnx Nemotron English 0.6B int8 streaming on CPU; measure partial latency, final latency at 160 ms chunks, and CPU% on a laptop-class machine. Decide: multilingual `nemotron-3.5-asr-streaming-0.6b` ONNX export — feasible now / wait / English-only v1. **This decision is the spike's headline deliverable** (§3.2). Additionally: (a) decide the P2 serving shape — vendored C++ websocket server vs. a tiny Rust-crate wrapper child we own (§3.2); (b) **test whether the Nemotron export emits usable token timestamps** (§3.2 — gates the binding cross-check, [#985](https://github.com/k2-fsa/sherpa-onnx/discussions/985)); (c) measure silero-vad onset error (detection latency + clock conversion) against CAPTURE §1's **250 ms combined budget**.
3. **Embedder recipe**: DFN5B ViT-H-14-378 via `ort` — images/sec on Tier-1 GPU and on CPU; peak VRAM; text-tower parity vs. reference OpenCLIP (cosine ≥ 0.999 on a probe set); stability under a 10k-image batch (the in-process crash risk, §3.3) → gates the GPU-EP default. **Text embedder bake-off**: benchmark **EmbeddingGemma-308M** ([HF blog](https://huggingface.co/blog/embeddinggemma)) alongside Qwen3-Embedding-0.6B as the half-cost candidate — retrieval quality on the RETRIEVAL golden-query set (R6), throughput, and footprint decide the default.
4. **Concurrency reality**: ASR streaming (CPU) + interactive LLM call + embedding batch simultaneously on the Tier-1 machine; verify §9 priorities hold (interactive parse < 2 s p95 while a batch runs), **including the worst case**: parse issued mid-prompt-processing of a caption (the non-preemptible window, §9 / acceptance 13.8).
5. **JSON-schema decoding**: confirm llama.cpp constrained output yields parseable RETRIEVAL filter-AST JSON at E4B Q4_K_M — 50-query probe set, ≥ 98% schema-valid.
6. Findings PR updates: §5.4 sizes, §6.2 gates if needed, §9 batch sizes; resolves the §3.2 and §3.3 open items.

## 13. Acceptance criteria

1. **No orphans**: kill the app (SIGKILL / Task Manager) on each OS → zero `llama-server`/ASR processes remain after at most one relaunch (the `children.json` net cleans crash leftovers; a normal quit leaves none immediately).
2. **Mid-call crash**: kill `llama-server` during an in-flight completion → the caller gets `ConnectorError::ConnectionLost`, the supervisor restarts with backoff, the request retries **exactly once** after `Ready` and succeeds; no user-visible error in the main flow.
3. **Tier 0 is whole**: on a machine below the floor (or with all backends `Failed`), every journal feature — typed notes, pencil, ratings, sessions, sidecars, merge, export, FTS search, filters — works identically to M1. No dialog ever mentions models unless the user opens settings.
4. **Resumable download**: cut the network mid-download → relaunch → the download resumes from the byte offset; final SHA-256 verifies; a corrupted part-file is detected and re-fetched without user action.
5. **Single instance**: launching a second app instance focuses the first and exits; the process table shows exactly one `llama-server` and one ASR process throughout.
6. **Readiness gating**: from cold start with installed models, the mic toggle is disabled-with-affordance until ASR `Ready`, then enables with no other UI change; NL queries before LLM `Ready` degrade to FTS+vector and still return results.
7. **License gate**: zero bytes of an `acceptance_required` model are downloaded before the recorded acceptance exists.
8. **Arbitration, worst case**: with the mic armed, no background LLM/embedding work starts (observable in the debug panel). An interactive parse issued **mid-prompt-processing of a caption** — the non-preemptible window (§9), not the friendly mid-generation case — still completes within the §12.4 interactive budget; the test MUST time this case explicitly.
9. **GC safety**: trigger a manifest bump with a reindex pending → old embedder weights survive until RETRIEVAL reports the reindex complete; no active pass ever loses its model files.
10. **Busy is not Lost**: a `/health` probe that times out while a long completion is in flight causes **no restart** (the supervisor reports Busy, visible in the debug panel); the same timeout with nothing in flight does restart. Child exit is detected via `waitpid`, not via health polling.

## 14. Open items (tracked, not blocking)

- Multilingual Nemotron 3.5 ONNX export viability — spike deliverable §12.2.
- Text-embedding model final pick (Qwen3-Embedding-0.6B-class is the working default; EmbeddingGemma-308M is the half-cost challenger, §12.3) — spike validates quality/throughput alongside the CLIP embedder.
- DFN5B exact license-acceptance requirement — spike confirms (§5.3).
- GPU execution-provider stability for the in-process embedder — spike §12.3 gates the default (`device = "auto"` may resolve to CPU in v1 if instability is observed).
- Future consideration (recorded only, per kernel): fine-tuning a small LLM for app tasks (summarization, sentiment, query parsing) — would slot in as a new manifest entry behind the same `LanguageModel` trait; no design work now.
