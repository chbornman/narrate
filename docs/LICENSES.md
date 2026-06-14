# License inventory

Audit of every license PhotoProof depends on: our own code, the Rust + npm
dependency trees, the ML runtime libraries, the GPU runtimes, and the ML model
weights. Last run **2026-06-14**.

> This is an ENGINEERING inventory to flag what needs attention. It is NOT legal
> advice. The model-weight licenses (especially Apple ASCL on the CLIP model)
> warrant review by counsel before commercial distribution. Open items are
> tracked in `docs/BACKLOG.md` (founder added a review item 2026-06-14).

## How to reproduce

- Rust: `cargo install cargo-license` then `cargo license` (groups crates by
  license). Add `--features cuda-dynamic` / `-p pp-asr-server --features
  engine-parakeet` to include the feature-gated deps (ort/cuda, parakeet-rs).
- npm: `apps/desktop/package.json` (small, hand-auditable).
- Models: the `License { name, url, acceptance_required }` block on each entry in
  `crates/photoproof-core/src/runtime/manifest.rs`.

## 1. PhotoProof itself

**`UNLICENSED`** (workspace `Cargo.toml`, inherited by all four crates:
`photoproof-core`, `photoproof-connectors`, `photoproof-desktop`,
`pp-asr-server`). Proprietary, all-rights-reserved; no `LICENSE` file. This is
deliberate, and it is WHY the dependency licenses below matter: copyleft deps do
not mix cleanly into a closed binary.

## 2. Code dependencies

Overwhelmingly permissive. Rust = 773 crates in the lockfile; the bulk are
`Apache-2.0 OR MIT` (459), `MIT` (158), `Apache-2.0` (12). Frontend (npm) =
Svelte, Tauri, Vite, TypeScript, vitest, lucide, all MIT / Apache / ISC.

Two copyleft flags:

| Crate(s) | License | Concern |
|---|---|---|
| **`rawler`** (RAW image decoder) | **LGPL-2.1** | Weak copyleft statically linked into a proprietary binary. LGPL §6 expects dynamic linking OR relinkable object files. Needs a compliance decision (or a permissive swap) before shipping RAW decode. The one real code-license action item. |
| `cssparser`, `cssparser-macros`, `selectors`, `dtoa-short`, `option-ext` | **MPL-2.0** (×5) | Low risk: file-level copyleft (Servo's CSS parser, pulled via the webview/styling). Fine UNMODIFIED; obligation is only to publish changes to those specific files (we make none) + carry their notices. |

Other permissive licenses present in small numbers: BSD-2/3-Clause, ISC, Zlib,
Unicode-3.0, CC0-1.0, CDLA-Permissive-2.0, BSL-1.0, Apache-2.0 WITH
LLVM-exception. `r-efi` is tri-licensed (Apache OR LGPL OR MIT -> choose
Apache/MIT). The 4 `UNLICENSED` entries are our own crates.

## 3. ML runtime libraries

All permissive.

| Component | License | Role |
|---|---|---|
| `ort` / `ort-sys` + ONNX Runtime binary | Apache-2.0 OR MIT (binding); **MIT** (the runtime, Microsoft) | CLIP, VAD, text-embed |
| `sherpa-onnx` / `-sys` | **Apache-2.0** | int8 ASR engine (fallback) |
| `parakeet-rs` 0.3.6 | **MIT OR Apache-2.0** | Nemotron 3.5 ASR engine (default) |
| llama.cpp (`llama-server`, vendored binary) | **MIT** | LLM serving |
| `tokenizers` (HuggingFace) | **Apache-2.0** | tokenization |
| Silero VAD (`crates/photoproof-connectors/assets/silero_vad.onnx`, bundled) | **MIT** | speech gating |

## 4. GPU runtimes (NVIDIA path only, opt-in)

Only linked on the `cuda-dynamic` build (the NVIDIA desktop); absent from the
macOS / CPU binary.

| Component | License | Note |
|---|---|---|
| NVIDIA CUDA runtime (`libcudart`, ...) | **NVIDIA CUDA EULA** (proprietary) | Redistributable under the EULA's redist terms, or rely on the user's own CUDA install. |
| NVIDIA TensorRT (`libnvinfer`, ...) | **NVIDIA proprietary EULA** | Same: redistribute under terms or user-provided. |

## 5. ML model weights

The manifest gates each with `acceptance_required` (good hygiene; the gate is
tested). These custom/model licenses are the real licensing surface.

| Model(s) | License | Flag |
|---|---|---|
| **DFN5B CLIP** ViT-H-14-378 (int8 + fp16) | **Apple Sample Code License (ASCL)** | **HIGHEST CONCERN.** ASCL is scoped to developing FOR Apple platforms and is not a general OSS license; its use as the shipped CLIP on Windows/Linux and in a commercial product needs verification. This is the core image-search model and the most-accelerated seam. Mitigation if it does not clear: swap to a permissively-licensed CLIP (OpenCLIP / LAION variants are MIT). Served via the Immich ONNX export. |
| **Gemma 4** (E2B, E4B, 26B, MTP variants) + **EmbeddingGemma-300m** | **Gemma Terms of Use** (Google) | Commercial use OK, but must propagate the terms + Google's Prohibited Use Policy. Custom license, not OSI. |
| **Nemotron** speech 0.6b + 3.5 (int8 + parakeet export) | **NVIDIA Open Model License** | Commercial use + derivatives OK with attribution + terms. The parakeet export (altunenes) re-packages the same NVIDIA-licensed weights. |
| **Qwen3-embedding-0.6b** (tier-2 alt text embedder) | **Apache-2.0** (`acceptance_required: false`) | The only fully-open model. |

## Open items (tracked in BACKLOG)

1. **DFN5B CLIP / Apple ASCL** review: confirm non-Apple commercial
   redistribution is permitted, or evaluate an MIT-licensed CLIP replacement.
   Highest impact (core search model).
2. **`rawler` / LGPL-2.1**: dynamic-linking compliance vs a permissive RAW
   decoder.
3. On distribution: ship a third-party-notices file (the permissive licenses
   require attribution), propagate the Gemma + NVIDIA model terms, and carry the
   MPL-2.0 source-availability notices.

Everything not flagged above is either permissive (code + ML runtimes) or
already gated behind terms-acceptance in the manifest.
