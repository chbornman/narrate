# Research: Streaming ASR, llama.cpp Supervision, Tauri UI

Validation of spec/RUNTIME.md, spec/CAPTURE.md §5–8, spec/UI.md §3–4/§10
against shipped desktop practice.

## Verdicts — ASR

- **sherpa-onnx — VALIDATED** as the production desktop engine: active,
  official Rust bindings ([crates.io](https://crates.io/crates/sherpa-onnx)),
  the Nemotron conversion exists
  ([csukuangfj conversion](https://huggingface.co/csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-2026-01-14)),
  NeMo-transducer support active upstream ([PR #3077](https://github.com/k2-fsa/sherpa-onnx/pull/3077)).
  whisper.cpp is batch-retrofitted-to-streaming ([AssemblyAI survey](https://www.assemblyai.com/blog/top-open-source-stt-options-for-voice-applications));
  Vosk a generation behind; [Moonshine v2](https://arxiv.org/abs/2602.12241) /
  [Kyutai STT](https://kyutai.org/stt) on the watch list behind the trait.
- **Vendored websocket server — RISKY**: (i) wire format is **float32
  samples + "Done" text message**, not 16-bit PCM as the spec claimed
  ([online WebSocket docs](https://k2-fsa.github.io/sherpa/onnx/websocket/online-websocket.html));
  (ii) it's closer to a reference binary than a hardened server; result JSON
  carries `text/tokens/timestamps/segment/is_final` ([C API](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/c-api/c-api.h))
  but whether the Nemotron export emits usable token timestamps is
  unverified ([discussion #985](https://github.com/k2-fsa/sherpa-onnx/discussions/985)).
  Spike must test timestamps explicitly; option recorded: wrap the
  sherpa-onnx Rust crate in a tiny purpose-built child process we own.
- **"ASR endpointing authoritative, no VAD" — PARTIALLY CONTRADICTED.** The
  closest shipped analog ([Handy](https://github.com/cjpais/Handy), Tauri+Rust)
  fronts STT with silero-vad; sherpa-onnx guidance says segment boundaries
  come from a VAD ([#985](https://github.com/k2-fsa/sherpa-onnx/discussions/985)).
  Decisive: CAPTURE requires a ≤300 ms `SpeechStart` onset signal **which the
  online recognizer does not emit**, and transducer token times are
  systematically late (RNN-T emission delay — [FastEmit](https://arxiv.org/abs/2010.11148)).
  But VAD-only *endpointing* hurts accuracy ([NVIDIA Riva notes](https://docs.nvidia.com/nim/riva/asr/1.8.0/release-notes.html)).
  Resolution: **silero-vad for onset (+ silence gating + "speaking"
  affordance), ASR endpointing for segmentation** (~1 MB, ~2 ms/chunk —
  [latency](https://rajatpandit.com/agentic-ai/real-time-audio-vad/)).
- **Per-final confidence — RISKY**: `ys_probs` only recently landed
  ([PR #2897](https://github.com/k2-fsa/sherpa-onnx/pull/2897)), raw
  log-probs, uncalibrated, hotword quirks ([#2937](https://github.com/k2-fsa/sherpa-onnx/issues/2937)).
  Define as exp(mean token log-prob), optional, never compared across model
  versions.
- **cpal — VALIDATED layer, three failure modes specced**: streams die on
  device change/unplug with no auto-recovery ([recovery pattern](https://bhanueso.dev/broadcasts/cpal-stream-recovery/));
  never request 16 kHz from the device — open default config and resample
  ([cpal #593](https://github.com/RustAudio/cpal/issues/593));
  Bluetooth/AirPods mic engagement forces HFP/SCO narrow-band and degrades
  WER ([Apple Communities](https://discussions.apple.com/thread/251360777)).

## Verdicts — llama.cpp supervision

- **Supervision architecture — VALIDATED** (same shape as Ollama's
  supervisor — [#4442](https://github.com/ollama/ollama/issues/4442); health
  gating matches [llama-server README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)).
- **Lanes — VALIDATED, one flag CONTRADICTED**: `--ctx-size` is **divided
  across parallel slots** ([#11681](https://github.com/ggml-org/llama.cpp/issues/11681))
  — `8192 --parallel 2` = 4096/lane. Fixed: launch corrected, VRAM
  re-budgeted; slot semantics churn ([#17989](https://github.com/ggml-org/llama.cpp/issues/17989))
  re-verified per pinned build.
- **Disconnect preemption — RISKY**: only works for **streaming** requests
  ([#9273](https://github.com/ggml-org/llama.cpp/issues/9273)); prompt-
  processing phase is non-preemptible ([ik_llama #1020](https://github.com/ikawrakow/ik_llama.cpp/issues/1020)).
  Fixed: background lane MUST stream; background prompts bounded.
- **`/health` as liveness — RISKY**: health requests queue behind inference
  ([#20684](https://github.com/ggml-org/llama.cpp/issues/20684)); `/slots`
  can hang while `/health` is green ([#20921](https://github.com/ggml-org/llama.cpp/issues/20921)).
  Fixed: restart only on process exit / connection refused / health failure
  while idle; health timeout under load = Busy; waitpid is ground truth.
- **VRAM fragmentation across restarts — NON-ISSUE** (problems are
  within-process multi-model accumulation — [#19425](https://github.com/ggml-org/llama.cpp/discussions/19425));
  spike kill/respawn loop as cheap insurance.
- **Q4_K_M — VALIDATED** ("the reliable default" — [2026 GGUF guide](https://bmdpat.com/blog/gguf-quantization-q4-q5-q8-explained-2026),
  [Kaitchup](https://kaitchup.substack.com/p/choosing-a-gguf-model-k-quants-i));
  prefer imatrix uploads when the pinned repo offers them.

## Verdicts — Tauri UI

- **Virtualized grid at 20k — VALIDATED contingent on delivery** (shipped
  reality — [Tauview](https://www.blog.brightcoding.dev/2026/05/04/tauview-the-revolutionary-image-viewer-every-developer-needs)).
- **Thumbnail delivery: IPC CONTRADICTED, custom protocol VALIDATED.**
  Maintainers: asset protocol "much faster" than fs/IPC; base64-through-
  invoke "inefficient" ([#7145](https://github.com/orgs/tauri-apps/discussions/7145),
  [#11498](https://github.com/tauri-apps/tauri/discussions/11498),
  [#5690](https://github.com/orgs/tauri-apps/discussions/5690)). Carry-over
  caveat: historical WebView2 asset memory-release bug
  ([#2952](https://github.com/tauri-apps/tauri/issues/2952)) + event-emit
  leak ([#852](https://github.com/tauri-apps/tauri/issues/852)) ⇒ webview
  memory is an explicit acceptance criterion now.
- **getCoalescedEvents — VALIDATED with floor**: WKWebView only Safari
  18.2+ / macOS 15.2+ ([caniuse](https://caniuse.com/mdn-api_pointerevent_getcoalescedevents));
  `pointerrawupdate` absent in WebKit. Feature-detect; strokes must be
  acceptable from plain pointermove cadence.
- **Pen pressure — RISKY on macOS**: macOS tablets post mouse events with
  subtype data ([Wacom dev docs](https://developer-docs.wacom.com/docs/icbt/macos/ns-events/ns-events-basics/));
  no positive evidence Wacom pressure reaches WKWebView; Windows works via
  Windows Ink (driver toggle caveat — [Wacom support](https://support.wacom.com/hc/en-us/articles/1500006343962-Why-is-my-pen-pressure-not-working);
  WebView2 pen bugs — [#2450](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2450)).
  Fixed: pressure = progressive enhancement; constant base_w on macOS;
  recorded future: native NSEvent pressure passthrough plugin.

## Verdicts — audio privacy UX

- **In-memory-only audio — VALIDATED, exceeds field norm** (leading local
  dictation tools still persist recordings — [Superwhisper](https://superwhisper.com/vs/wispr-flow)).
- **Mic state — VALIDATED with two gaps fixed**: the macOS orange dot burns
  the entire armed session ([Apple](https://support.apple.com/en-us/118449))
  — the cpal stream is now **closed (not paused) on disarm** so app state and
  OS dot always agree, and the armed-state hover carries the one-line privacy
  claim ("Listening — transcribed on this device, never written to disk").

## Amendments (all applied)

U1 silero-vad onset front; bind on VAD onset; spike measures Nemotron token
timestamps vs the 250 ms budget · U2 ctx-size/parallel corrected · U3
background lane streams; prompt-phase preemption limits stated; worst-case
acceptance test · U4 /health busy-vs-lost; waitpid ground truth · U5 P2 wire
= float32 + "Done"; Rust-crate wrapper option for the spike · U6 confidence
= exp(mean logprob), optional, uncalibrated · U7 cpal default-config +
resample, watchdog re-arm, Bluetooth advisory · U8 thumbnails via custom
URI scheme, never IPC/base64; webview memory bound criterion; recycled img
elements · U9 pressure progressive enhancement; macOS gap recorded; coalesced
feature-detect · U10 stream closed on disarm; OS-dot privacy line.

## Non-issues

VRAM fragmentation across restarts · Q4_K_M choice · lanes-vs-queue (false
dichotomy; ours is Ollama's shape) · ports/orphans/locks/pinned downloads
(more careful than most shipped apps) · 60 s ring buffer (~3.8 MB) · stroke
storage model (matches drawing-app practice — [Nutrient on coalesced events](https://www.nutrient.io/blog/using-getcoalescedevents/)) ·
Look-swap budget · CPU-only ASR (a feature: Handy/Parakeet-class apps ship
exactly this).
