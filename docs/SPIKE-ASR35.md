# Nemotron 3.5 ASR dev evaluation (June 12, 2026)

Founder decision following this eval: BUILD WITH THE NEW MODEL — Nemotron
3.5 ASR Streaming 0.6B is the target ASR; the pinned
nemotron-speech-streaming-en-0.6b-160ms-int8 remains the working pipeline
until the 3.5 streaming deployment path ripens (sherpa-onnx export +
Rust crate release; support landed in sherpa master June 12).

Setup: NeMo main (3.1.0+95f92737c, the 26.xx line - pip 2.7.3 LACKS the
prompt model class), python 3.12 venv at ~/spike-asr35/, model
nvidia/nemotron-3.5-asr-streaming-0.6b, BATCH transcription on M-series
CPU, language forced en-US via the langID prompt path (the temp-manifest
default 'unified' mode coin-flips into a None-language crash - monkeypatch
in transcribe.py; report upstream eventually).

## Result: every pipeline defect class resolved on the founder corpus

- mixed-register: "Okay, this contact sheet is actually incredible ...
  the empty bench that is the one I will still care about in ten years."
  (current pipeline: "incred" truncated, "empt" dropout, "still" lost)
- cold-starts: "Keeper print this one for the show in October reject the
  focus mist maybe ask me again next month best of the day" (current:
  "Kee" truncated; all five verdicts complete here)
- somber-mumble: complete - "fog flattens" / "into a gray shape" /
  "single figure at the rail" / "that winter series" (current: "fogens",
  "intoy", "single at", "thatter")
- alice-60s: near-verbatim with native punctuation and capitalization.
- Throughput: 2-4 s per file (batch, CPU). Native punctuation +
  capitalization throughout.

## Caveats

- BATCH mode (full context) vs the product's STREAMING decode - part of
  the quality gap may be batch-vs-streaming, not new-vs-old model. The
  fair comparison is 3.5 STREAMED (att_context configurable 80 ms-1.12 s)
  vs the current streamed pipeline, on the same corpus + the Alice WER
  harness, once the sherpa export exists.
- Output carries <en-US> prompt tags (integration residue to strip).
- Genuine mishearings remain ("crowdblazer" for crowd pleaser, "mist"
  for missed) - reduced, not eliminated.

## Upgrade triggers (watch)

1. Official sherpa-onnx-layout streaming export of 3.5 on HF (k2-fsa /
   csukuangfj usually within days of master support).
2. sherpa-onnx Rust crate release containing the 3.5 support (we pin
   1.13.2 in pp-asr-server).
Then: manifest pin + possible crate bump + rerun voice corpus +
Alice WER (streaming) + spike-style latency/RSS numbers; punctuation
changes journal entry rendering - check downstream (FTS, chunking,
sidecars are content-agnostic; only display benefits).

## STREAMED findings (June 12, later) - ROOT CAUSE IDENTIFIED

sherpa-onnx built from master (3.5 support, PR 3671); official exports
published at csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-
{80,160,560,1120}ms{-int8}-2026-06-11.

- 160 ms int8 STREAMED: the SAME tail-truncation class as the current
  production pipeline ("incre", "Keper", "crowd pla") plus heavy
  syllable drops. NOT a pipeline bug and NOT old-model-specific.
- 560 ms int8 STREAMED: near-batch quality - "incredible", "Keeper",
  "crowd pleaser" (which even batch misheard), mumble dropouts resolved,
  punctuation present. Residual artifacts small and partly at the rule3
  20 s forced endpoint.
- CONCLUSION: the att_context right-lookahead (~80 ms at the 160 ms
  preset vs ~480 ms at 560 ms) decides whether word-final tokens emit
  before an endpoint mints the final. The entire truncation
  investigation (rule2/pacing/chunk/pre-roll invariance) is explained:
  the lookahead is baked into the EXPORT, so no runtime knob could fix
  it. Latency cost of 560 ms is irrelevant for journaling.
- ACTIONS: (a) interim: swap the CURRENT model to its 560 ms export
  (exists: ...nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25;
  same crate, manifest-pin-only change) - immediate dictation quality
  win; (b) B74 target: 3.5 @ 560 ms int8 once the Rust crate ships
  support; language forced via stream set_option (binding exposes it).
