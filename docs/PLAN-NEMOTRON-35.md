# PLAN-NEMOTRON-35 — upgrade the ASR to Nemotron 3.5 Streaming 0.6B (B74)

Status as of 2026-06-14. Supersedes the "watch" line in `docs/BACKLOG.md`
(B74) with a concrete go/no-go, real artifact pins, and the swap delta.
Background: `docs/SPIKE-ASR35.md` (the dev eval + the 560 ms lookahead
root-cause), `spec/RUNTIME.md` §3.2 (the `pp-asr-server` seam).

## TL;DR verdict: NO-GO to ship today; STAGED and ready

The model is right, the export exists and is pinned with real SHAs in this
branch, but the **single trigger condition is not met**: the sherpa-onnx
**Rust** crate has not shipped a release that supports 3.5 streaming. The
swap is a small, well-bounded change the day the crate ships — the code in
this branch makes it close to a one-line tier flip plus a crate bump.

## 1. Is it shippable now? (the trigger)

NO. The trigger is "a sherpa-onnx Rust crate release containing 3.5
support" (SPIKE-ASR35.md, BACKLOG B74). Evidence:

- **Rust crate** `sherpa-onnx` newest published version is **1.13.2**,
  released **2026-05-14** (crates.io versions API). `pp-asr-server` pins
  `sherpa-onnx = "1.13.2"`. 1.13.2 predates 3.5 by a month.
  - https://crates.io/api/v1/crates/sherpa-onnx/versions
  - https://crates.io/crates/sherpa-onnx
- **Upstream C++** 3.5 streaming support landed in **master ~2026-06-12**
  (PR 3671 per SPIKE-ASR35.md; the older request thread is issue #3408 →
  PR #3555). The tagged GitHub releases through **v1.13.2 (2026-05-13)**
  do NOT mention 3.5.
  - https://github.com/k2-fsa/sherpa-onnx/releases
  - https://github.com/k2-fsa/sherpa-onnx/issues/3408
- So 3.5 is in C++ master only. No Rust crate (1.13.3 / 1.14.x) carrying
  it has been published. **Building from master is out of scope** — we
  pin published crate versions (manifest discipline; no branch refs).

**Go condition (re-check trigger):** a new `sherpa-onnx` crate version on
crates.io whose underlying C++ is >= the master commit that merged 3.5
(PR 3671) AND that exposes the per-stream language option. Re-run this
plan's §1 check (crates.io versions + the release notes) before flipping.

## 2. The model artifacts (REAL pins, staged in this branch)

Official sherpa-onnx-layout export, 560 ms int8 variant
(the SPIKE's chosen serving point — clears the tail-truncation class the
160 ms lookahead bakes in):

- HF repo: `csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11`
- Pinned revision (main sha, lastModified 2026-06-12): `f4111f5f930348aa484ccf1779c5fb6f71e20dea`
- Four-file transducer layout, SAME basenames as the current entry:

  | file              | bytes       | sha256 |
  |-------------------|-------------|--------|
  | encoder.int8.onnx | 657,395,114 | 4ff9fedb8f2324ad9736cad6c4a89063d8a428fe21364504ec613a3d60f749b4 |
  | decoder.int8.onnx | 14,978,075  | 19f9c98fc6d0a2c33a65a43b36fdb2e914c26c0aa9764be3aebc502a1e982fb0 |
  | joiner.int8.onnx  | 9,504,438   | 4101c7c679a0bc30483794b27a059e34e79232aa2068d78d51231a22c8b0d7ce |
  | tokens.txt        | 131,440     | 729cc103155bafa785f9cd45746cd41cabe97eab7182fc04d594129587958f8a |

  Total: 681,909,067 bytes (~682 MB; vs the current 662 MB). encoder/
  decoder/joiner SHAs are HF LFS oids; tokens.txt is git-stored (no LFS
  oid) so its sha256 was computed locally from the pinned revision.

- **Native punctuation + capitalization + ~40 locales** confirmed: the
  tokens.txt (131 kB vs the old ~9 kB) carries locale tags (`<bg-BG>`,
  `<en-US>`, ...), Cyrillic/Vietnamese glyphs, and punctuation tokens.
  The export README documents a **per-stream language string** (`en`,
  `ja`, `auto`).
- Other available chunk presets in the same series:
  `{80,160,560,1120}ms{-int8}-2026-06-11`. We pin **560 ms int8** only.
- License: NVIDIA Open Model License (acceptance gate — same flow as the
  current ASR entry; a distinct model id keeps its own acceptance record).
- Sources: model card https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b
  ; export tree https://huggingface.co/csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11/tree/main
  ; release note https://www.marktechpost.com/2026/06/06/nvidia-releases-nemotron-3-5-asr-a-600m-parameter-cache-aware-streaming-model-transcribing-40-language-locales-in-real-time/

## 3. Integration delta (what changes on GO)

Smaller than the model jump suggests, because the export keeps the
four-file transducer layout and the same basenames.

1. **sherpa-onnx crate bump** — `crates/pp-asr-server/Cargo.toml`,
   `sherpa-onnx = "1.13.2"` → the first published version with 3.5
   support. This is the ONLY hard blocker. Re-run the build after the bump
   (no other crate consumes sherpa directly; `photoproof-connectors` is
   the WS client, wire-contract-bound, model-agnostic).
2. **Manifest pin** — `crates/photoproof-core/src/runtime/manifest.rs`.
   The 3.5 entry is ALREADY in the branch (id
   `nemotron-3.5-asr-streaming-0.6b-560ms-int8`, real SHAs) but offered at
   `tiers: vec![]`. GO = flip it to `vec![1, 2]` and demote the current
   `nemotron-speech-streaming-en-0.6b-560ms-int8` entry to `tiers: vec![]`
   (keep it — installed weights are never deleted; it becomes the
   fallback). Update the tier-1 sum test (current `10_830_366_615` →
   +19,989,651 bytes for the 682 MB vs 662 MB delta = `10_850_356_266`)
   and flip `nemotron_35_is_pinned_but_offered_at_no_tier` to assert the
   new offered set.
3. **Model-pick** — `asr_wrapper_args` (`runtime/launch.rs`) hardcodes the
   basenames `encoder.int8.onnx` / `decoder.int8.onnx` / `joiner.int8.onnx`
   / `tokens.txt`, which the 3.5 export ALSO uses, so the launcher needs
   **no path change**. The model-dir id is resolved upstream from the
   offered ASR entry; confirm whichever selection layer picks "the asr
   model offered at the tier" lands on the 3.5 id once it is the offered
   one. (No config schema change required for the basic swap.)
4. **Per-stream language option** — NEW. The export expects a language
   string per stream (`en` / `auto`). In `pp-asr-server::recognizer` /
   the stream creation path, set it once to `en` (the product is English
   today; SPIKE forced en-US via prompt). Exact binding name depends on
   the crate version that ships — wire it when the crate is in hand. Until
   then leaving it default risks the SPIKE's `None-language` coin-flip; do
   NOT flip tiers without wiring this.
5. **Chunk-size / lookahead (B74)** — already decided: **560 ms** is the
   pin precisely because the right-lookahead is baked into the export and
   no runtime knob fixes the 160 ms truncation. `--chunk-ms` stays a
   client-side concern; the server ignores it. No endpoint-rule change is
   forced, though the 560 ms tail behavior may let `rule2` relax (a FEEL
   tuning item, not a correctness one).
6. **Streaming API shape** — unchanged from the consumer side: still an
   `OnlineRecognizer` + transducer + greedy_search + endpointing. The
   only additive surface is the language option (item 4). The WS wire
   contract (f32 frames in, result JSON out, "Done") is untouched.
7. **Downstream of punctuation** — finals now carry punctuation +
   capitalization. FTS / chunking / sidecars are content-agnostic (they
   store whatever text arrives), so only journal-event DISPLAY benefits.
   Strip any residual `<en-US>` prompt tags if they appear in output
   (SPIKE noted integration residue in the NeMo path; the sherpa export
   should not emit them, verify on the first run).

## 4. Validation plan (gate: 3.5 >= current, STREAMED)

Run on the founder machine (needs the real model + gitignored corpora),
all STREAMED (not batch — the SPIKE's fair-comparison caveat):

1. **Voice corpus** — `pp-sweep voice` / `pp_voice_bench` over the founder
   corpus (mixed-register, cold-starts, somber-mumble cards). Expect the
   SPIKE's STREAMED 560 ms result: "incredible"/"Keeper"/"crowd pleaser"
   intact, mumble dropouts resolved, punctuation present.
2. **Alice WER** — `pp-voice-bench --expect <gutenberg transcript>` on the
   `test-corpora/voice-long/` Alice ch1 wavs (scorer landed `a4b9604`).
   Read the RAW-vs-GATED gating-cost delta; 3.5 streamed must be <= the
   current model's WER on both feeds.
3. **Latency / RSS** — spike-style: per-final latency and peak RSS vs the
   current 560 ms pipeline (the 560 ms latency is irrelevant for
   journaling per SPIKE, but capture the number).
4. **Acceptance**: 3.5 ships only if (1) and (2) are >= current on the
   same corpora AND latency/RSS stay within the §3.2/§9 budgets.

## 5. Effort / risk

- **Effort**: ~half a day once the crate ships (SPIKE: "the swap
  evaluation costs an afternoon"). Crate bump + tier flip + language wiring
  + the four validation runs.
- **Risk: LOW–MEDIUM.**
  - LOW: model quality (SPIKE already proved the 560 ms export on the
    corpus), artifact integrity (real pins), the wire contract (unchanged),
    downstream (content-agnostic).
  - MEDIUM: the per-stream language binding is new and crate-version
    dependent — the one unknown surface; the `None-language` crash class
    the SPIKE hit upstream means this must be wired and tested, not
    assumed.
  - The swap is fully reversible: the old entry stays installed (just
    un-offered), so a regression is a one-line revert.

## 6. What this branch already does (safe, gated)

- Adds the 3.5 entry to `manifest.rs` with REAL SHAs/sizes/revision,
  `tiers: vec![]` (offered nowhere → no consent sum, no download, the live
  ASR path untouched).
- Adds `nemotron_35_is_pinned_but_offered_at_no_tier` guarding the
  "doesn't ship by accident" property (pinned + resolvable, but the only
  offered ASR model stays the current one).
- Does NOT bump the sherpa-onnx crate (no published version supports 3.5)
  and does NOT touch `pp-asr-server` / `asr_wrapper_args`.
- `cargo fmt` / `clippy` / the manifest tests are green.
