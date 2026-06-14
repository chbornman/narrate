//! The model manifest (spec/RUNTIME.md §5.1) and license records (§5.3).
//!
//! Each app release embeds a manifest — compiled in, also written to
//! `{models_dir}/manifest.json` for the debug panel. Every file is pinned
//! to an exact filename + SHA-256 + byte size at an immutable revision:
//! no wildcards, no "latest", no branch refs. Upgrading models = shipping
//! a manifest bump with an app update (§11).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

// The DFN5B external-data enumeration (400 files: visual/ + textual/) lives in
// a checked-in generated literal — see gen_dfn5b_manifest.sh for WHY a
// generator. `include!` keeps it a pure compiled `&[(path, sha, bytes)]` slice
// with no network and no runtime discovery; the manifest builder below maps it
// into FileEntry rows at the pinned revision.
include!("dfn5b_files.rs");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    /// "llm" | "llm-alt" | "asr" | "embedder" | "text-embedder" |
    /// "text-embedder-alt" (the "*-alt" roles are config-selectable
    /// alternatives, offered at higher tiers — the llm-alt precedent).
    pub role: String,
    /// Tiers this model is OFFERED at (§6.2 — selection changes offers,
    /// never deletes installed weights).
    pub tiers: Vec<u8>,
    pub license: License,
    pub total_bytes: u64,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct License {
    pub name: String,
    pub url: String,
    /// §5.3: display + explicit acceptance BEFORE download (the §13.7
    /// byte-zero gate).
    pub acceptance_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    /// `hf:{org}/{name}` resolving to
    /// `https://huggingface.co/{org}/{name}/resolve/{revision}/{path}`,
    /// or a literal `http://…` base (stub servers; LAN mirrors).
    pub repo: String,
    /// Immutable revision pin (commit hash). No branch refs.
    pub revision: String,
    pub path: String,
    /// Lowercase hex SHA-256 of the complete file.
    pub sha256: String,
    pub bytes: u64,
}

impl FileEntry {
    /// Resolve the download URL (§5.1 rules).
    pub fn url(&self) -> String {
        if let Some(rest) = self.repo.strip_prefix("hf:") {
            format!(
                "https://huggingface.co/{rest}/resolve/{}/{}",
                self.revision, self.path
            )
        } else {
            format!("{}/{}", self.repo.trim_end_matches('/'), self.path)
        }
    }

    /// Basename for display and error messages only. The on-disk location
    /// is `models_dir/<model_id>/<path>` (path-preserving downloads, plan
    /// P7.4 decision 1): nested entries like DFN5B's `visual/model.onnx`
    /// and `textual/model.onnx` share this basename, so it pins NOTHING on
    /// disk — never join `file_name()` to a model dir to resolve a file,
    /// join `path`.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// B55 fail-closed: an all-zero sha or the UNPINNED-P6.3 revision
    /// placeholder means this entry has not been through a spike session.
    /// The downloader refuses it pre-flight; consent never enqueues it;
    /// settings renders it as pending, not failed.
    pub fn is_pinned(&self) -> bool {
        !(self.sha256.bytes().all(|b| b == b'0') || self.revision == "UNPINNED-P6.3")
    }
}

impl ModelEntry {
    /// Every file pinned (see [`FileEntry::is_pinned`]).
    pub fn is_pinned(&self) -> bool {
        self.files.iter().all(FileEntry::is_pinned)
    }
}

impl Manifest {
    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Models offered at a tier (§6.2).
    pub fn offered_at(&self, tier: u8) -> Vec<&ModelEntry> {
        self.models
            .iter()
            .filter(|m| m.tiers.contains(&tier))
            .collect()
    }

    /// The live disk budget shown on the consent card (§5.4: "the consent
    /// dialog always shows the live manifest sum").
    pub fn total_bytes_at(&self, tier: u8) -> u64 {
        self.offered_at(tier).iter().map(|m| m.total_bytes).sum()
    }

    /// §5.1: also written to `{models_dir}/manifest.json` for the debug
    /// panel.
    pub fn write_to(&self, models_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(models_dir)?;
        let tmp = models_dir.join("manifest.json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).expect("manifest json"),
        )?;
        std::fs::rename(tmp, models_dir.join("manifest.json"))
    }
}

/// The compiled-in manifest for this release. LLM + ASR pins are REAL as
/// of the P6.3 spike session 1 (docs/SPIKE-P6.3.md — SHA-256 measured on
/// downloaded artifacts, revisions = the HF repo commits served at pin
/// time). The embedder pins are REAL as of the P7 embed spike (B73,
/// docs/SPIKE-P7-EMBED.md): EmbeddingGemma-300m q8 is the text-embedder
/// default, Qwen3-Embedding-0.6B int8 the configured alternative, and
/// DFN5B is enumerated in full (graph + ~400 external-data files).
pub fn compiled_manifest() -> Manifest {
    let pinned = |repo: &str, revision: &str, path: &str, sha256: &str, bytes: u64| FileEntry {
        repo: repo.into(),
        revision: revision.into(),
        path: path.into(),
        sha256: sha256.into(),
        bytes,
    };
    // DFN5B at its single pinned Immich revision: map the generated literal
    // (path, sha, bytes) tuples into FileEntry rows. total_bytes is the SUM of
    // every enumerated file — never a hand-typed estimate.
    const DFN5B_REPO: &str = "hf:immich-app/ViT-H-14-378-quickgelu__dfn5b";
    const DFN5B_REVISION: &str = "a5925c6e44f6381544a7263296662135ff4df0ff";
    let dfn5b_files: Vec<FileEntry> = DFN5B_FILES
        .iter()
        .map(|(path, sha256, bytes)| pinned(DFN5B_REPO, DFN5B_REVISION, path, sha256, *bytes))
        .collect();
    let dfn5b_total: u64 = dfn5b_files.iter().map(|f| f.bytes).sum();
    Manifest {
        manifest_version: 1,
        models: vec![
            // B68: E2B QAT q4_0 (official Google quant) is the default LLM
            // — half the footprint, 2× the speed, schema probe 50/50, and
            // the interactive parse fits §9's 2 s budget on the Tier-1
            // floor. Supervisor flags: --reasoning-budget 0 is REQUIRED
            // (E2B/E4B reason their budget away otherwise — spike).
            ModelEntry {
                id: "gemma-4-e2b-it-qat-q4_0".into(),
                role: "llm".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 3_906_881_888,
                files: vec![
                    pinned(
                        "hf:google/gemma-4-E2B-it-qat-q4_0-gguf",
                        "1894d1fc0a19d86697abd40483f5983c867df03f",
                        "gemma-4-E2B_q4_0-it.gguf",
                        "3646b4c147cd235a44d91df1546d3b7d8e29b547dbe4e1f80856419aa455e6fd",
                        3_349_514_112,
                    ),
                    pinned(
                        "hf:ggml-org/gemma-4-E2B-it-GGUF",
                        "a1dac71d3ab220618f5a7573a52acdc4baf3ae3b",
                        "mmproj-gemma-4-E2B-it-Q8_0.gguf",
                        "8a82e0fd831bb7cb5c8898b86393eb14042986b950a60e1034bf21d061aac8a8",
                        557_367_776,
                    ),
                ],
            },
            // MTP (multi-token prediction) E2B — the lossless-speculative
            // variant. Same Gemma 4 E2B target, plus a TINY MTP drafter
            // (~59 MB) that shares the target KV cache; the target verifies
            // every drafted token, so output is byte-identical to the plain
            // E2B path (Unsloth MTP card; mainline llama.cpp #23398 + #24282).
            //
            // GATED BY HARDWARE, NOT DEFAULT: MTP is a CUDA win (RTX 5080) and
            // a Metal LOSS — the draft-eval overhead exceeds the speculative
            // gain on Apple Silicon at every config (ggml-org/llama.cpp #23752,
            // closed: 11-28% SLOWER on Metal). So this entry is OFFERED at
            // tier 2 (the discrete-GPU tier) and the supervisor only passes the
            // `--spec-type draft-mtp` flags when the drafter file is present
            // AND the platform is non-Metal (launch.rs). The laptop keeps the
            // plain `gemma-4-e2b-it-qat-q4_0` entry above, unchanged.
            //
            // Repo: Unsloth's QAT GGUF (Q4_K_XL UD target + the root `mtp-`
            // drafter + Q8 mmproj). The drafter and target SHAs are REAL (HF
            // LFS pointers at the pinned revision); the mmproj SHA is the
            // Unsloth F16 projector. Requires a vendored llama.cpp built AFTER
            // 2026-06-08 (#24282 added the `gemma4-assistant` drafter arch +
            // E2B/E4B support) — the binary update is a separate, founder-owned
            // step (docs/PLAN-GEMMA-MTP.md).
            ModelEntry {
                id: "gemma-4-e2b-it-qat-q4_k_xl-mtp".into(),
                role: "llm-alt".into(),
                tiers: vec![2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 2_620_368_960 + 59_234_176 + 985_654_080,
                files: vec![
                    pinned(
                        "hf:unsloth/gemma-4-E2B-it-qat-GGUF",
                        "db01ae3ceeca98487bf3569814f832f5023cd48c",
                        "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",
                        "cd4526493dccbfd6791bee8822e37e30340074d1d4d9aada52ce09afefd6a33a",
                        2_620_368_960,
                    ),
                    // The MTP drafter (root `mtp-` file: a recent llama.cpp
                    // auto-detects it next to the target, but the supervisor
                    // passes it explicitly via --model-draft because our
                    // vendored binary loads from a pinned path, not `-hf`).
                    pinned(
                        "hf:unsloth/gemma-4-E2B-it-qat-GGUF",
                        "db01ae3ceeca98487bf3569814f832f5023cd48c",
                        "mtp-gemma-4-E2B-it.gguf",
                        "8702bf70ab5604dcc818f26ea144fd4237c9908c15992d5b34746c65039dc65d",
                        59_234_176,
                    ),
                    pinned(
                        "hf:unsloth/gemma-4-E2B-it-qat-GGUF",
                        "db01ae3ceeca98487bf3569814f832f5023cd48c",
                        "mmproj-F16.gguf",
                        "13c8966d1635d02e6727f27402880614906fa291850c07feda18dbcddf2291b6",
                        985_654_080,
                    ),
                ],
            },
            // MTP 26B-A4B — the Tier-2 quality upgrade for the discrete-GPU
            // box (RTX 5080, 16 GB VRAM). MoE with ~4B active params tolerates
            // partial CPU offload (RUNTIME §6.2), the Q4_K_XL target is 14.25 GB
            // (fits 16 GB with KV + the ~252 MB drafter), and MTP gives the big
            // dense-style speedup on CUDA. Same lossless-verify guarantee as the
            // E2B-MTP entry; same non-Metal launch gating. The 31B target
            // (17.3 GB) does NOT fit 16 GB VRAM, so 26B-A4B is the pick.
            ModelEntry {
                id: "gemma-4-26b-a4b-it-qat-q4_k_xl-mtp".into(),
                role: "llm-alt".into(),
                tiers: vec![2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 14_249_045_120 + 251_937_728 + 985_654_080,
                files: vec![
                    pinned(
                        "hf:unsloth/gemma-4-26B-A4B-it-qat-GGUF",
                        "02749a7b272109255a4c559a80894d3d9777574c",
                        "gemma-4-26B-A4B-it-qat-UD-Q4_K_XL.gguf",
                        "dcf179a91153e3a7ece792e48ef872180d9d6ef9b7677f0a0bd3e83cfe624d5e",
                        14_249_045_120,
                    ),
                    pinned(
                        "hf:unsloth/gemma-4-26B-A4B-it-qat-GGUF",
                        "02749a7b272109255a4c559a80894d3d9777574c",
                        "mtp-gemma-4-26B-A4B-it.gguf",
                        "62bd3af7f66c9308de9a5454233852f8c7324c93767e8dfb824ed45b9179864a",
                        251_937_728,
                    ),
                    pinned(
                        "hf:unsloth/gemma-4-26B-A4B-it-qat-GGUF",
                        "02749a7b272109255a4c559a80894d3d9777574c",
                        "mmproj-F16.gguf",
                        "13c8966d1635d02e6727f27402880614906fa291850c07feda18dbcddf2291b6",
                        985_654_080,
                    ),
                ],
            },
            // E4B stays the config-selectable bigger option (Tier 2+):
            // measurably better prose for captions, 2.9 s parses, 6.7 GB.
            ModelEntry {
                id: "gemma-4-e4b-it-q4_k_m".into(),
                role: "llm-alt".into(),
                tiers: vec![2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 5_895_164_352,
                files: vec![
                    pinned(
                        "hf:ggml-org/gemma-4-E4B-it-GGUF",
                        "2714b5519c6c3516b1000e7c5e1eba998dfe1fe8",
                        "gemma-4-E4B-it-Q4_K_M.gguf",
                        "90ce98129eb3e8cc57e62433d500c97c624b1e3af1fcc85dd3b55ad7e0313e9f",
                        5_335_289_824,
                    ),
                    pinned(
                        "hf:ggml-org/gemma-4-E4B-it-GGUF",
                        "2714b5519c6c3516b1000e7c5e1eba998dfe1fe8",
                        "mmproj-gemma-4-E4B-it-Q8_0.gguf",
                        "51d4b7fd825e4569f746b200fccc5332bf914e8ef7cbe447272ce4fec6df3db6",
                        559_874_528,
                    ),
                ],
            },
            // The 160 ms int8 export (April 2026 series) supersedes the
            // January repo the planning manifest referenced — per-chunk
            // exports; 160 ms is the spec's serving point (§3.2, spike).
            // Three transducer sessions + tokens, exactly as sherpa loads
            // them.
            ModelEntry {
                id: "nemotron-speech-streaming-en-0.6b-560ms-int8".into(),
                role: "asr".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "NVIDIA Open Model License".into(),
                    url: "https://developer.download.nvidia.com/licenses/nvidia-open-model-license-agreement-june-2024.pdf".into(),
                    acceptance_required: true,
                },
                total_bytes: 661_919_416,
                files: vec![
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
                        "52056fdc070914a48dcd68b31b44d6a6f5b85902",
                        "encoder.int8.onnx",
                        "7d932213491ad355c6e5576705dc3494731a52af87d7a1b954559340147909d8",
                        652_916_849,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
                        "52056fdc070914a48dcd68b31b44d6a6f5b85902",
                        "decoder.int8.onnx",
                        "0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98",
                        7_257_753,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
                        "52056fdc070914a48dcd68b31b44d6a6f5b85902",
                        "joiner.int8.onnx",
                        "a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23",
                        1_735_862,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
                        "52056fdc070914a48dcd68b31b44d6a6f5b85902",
                        "tokens.txt",
                        "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff",
                        8_952,
                    ),
                ],
            },
            // B74 / docs/PLAN-NEMOTRON-35.md — the Nemotron 3.5 ASR Streaming
            // 0.6B target (560 ms int8, csukuangfj2 export 2026-06-11). NATIVE
            // punctuation + capitalization + ~40 language-locales; the SPIKE
            // (docs/SPIKE-ASR35.md) confirmed the 560 ms export clears the
            // tail-truncation class baked into the 160 ms lookahead. Four-file
            // transducer layout, SAME basenames as the current ASR entry
            // (encoder/decoder/joiner.int8.onnx + tokens.txt) so
            // `runtime::launch::asr_wrapper_args` needs no path changes.
            //
            // NOT OFFERED YET (tiers: vec![] — appears in no tier sum, no
            // consent card): this is a STAGED pin, not a live swap. GO is
            // BLOCKED on the sherpa-onnx RUST crate shipping 3.5 streaming
            // support — as of 2026-06-14 the crate is pinned at 1.13.2 (May 14),
            // which predates 3.5 landing in their C++ master (~June 12,
            // PR 3671). The per-stream language option ('en'/'auto', README of
            // the export) is also a NEW binding not in any tagged crate. When a
            // crate release lands the support: (1) bump pp-asr-server's
            // sherpa-onnx pin, (2) flip these tiers to vec![1, 2] and demote the
            // 160 ms-lineage entry, (3) wire the language option, (4) rerun the
            // voice corpus + Alice WER STREAMED. SHAs/sizes/revision below are
            // REAL (HF API, revision = the repo's main sha at pin time); only
            // the runtime support is missing.
            ModelEntry {
                id: "nemotron-3.5-asr-streaming-0.6b-560ms-int8".into(),
                role: "asr".into(),
                // Empty = offered at no tier. The downloader never enqueues it,
                // the consent card never sums it, `total_bytes_at` ignores it.
                // Flipping to vec![1, 2] is the single line that makes it live,
                // gated by the PLAN-NEMOTRON-35.md go/no-go.
                tiers: vec![],
                license: License {
                    name: "NVIDIA Open Model License".into(),
                    url: "https://developer.download.nvidia.com/licenses/nvidia-open-model-license-agreement-june-2024.pdf".into(),
                    acceptance_required: true,
                },
                total_bytes: 657_395_114 + 14_978_075 + 9_504_438 + 131_440,
                files: vec![
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11",
                        "f4111f5f930348aa484ccf1779c5fb6f71e20dea",
                        "encoder.int8.onnx",
                        "4ff9fedb8f2324ad9736cad6c4a89063d8a428fe21364504ec613a3d60f749b4",
                        657_395_114,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11",
                        "f4111f5f930348aa484ccf1779c5fb6f71e20dea",
                        "decoder.int8.onnx",
                        "19f9c98fc6d0a2c33a65a43b36fdb2e914c26c0aa9764be3aebc502a1e982fb0",
                        14_978_075,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11",
                        "f4111f5f930348aa484ccf1779c5fb6f71e20dea",
                        "joiner.int8.onnx",
                        "4101c7c679a0bc30483794b27a059e34e79232aa2068d78d51231a22c8b0d7ce",
                        9_504_438,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11",
                        "f4111f5f930348aa484ccf1779c5fb6f71e20dea",
                        "tokens.txt",
                        "729cc103155bafa785f9cd45746cd41cabe97eab7182fc04d594129587958f8a",
                        131_440,
                    ),
                ],
            },
            // PLAN-NEMOTRON-35-SIDECAR — the Nemotron 3.5 export in the
            // PARAKEET-RS LAYOUT (the second staged entry the plan §6
            // calls for). parakeet-rs does NOT load the four-file sherpa
            // transducer export above; it loads a directory of
            // encoder.onnx + encoder.onnx.data + decoder_joint.onnx +
            // config.json + tokenizer.model (an FP32 ONNX, ~2.5 GB of
            // external data — NOT the int8 sherpa export). Served from the
            // crate author's HF repo (altunenes/parakeet-rs), subdir
            // nemotron-3.5-asr-streaming-0.6b-onnx, at the pinned commit.
            // config.json declares left_context=56 + vocab 13087 — the
            // multilingual-3.5 markers the plan cites.
            //
            // OFFERED ONLY when pp-asr-server is built `--features
            // engine-parakeet` AND this tier flips to vec![1, 2]. STAGED
            // (tiers: vec![]) until the WER/latency gate (§7) passes: the
            // downloader never enqueues it, the consent card never sums it.
            // Flipping the tier + building the parakeet feature is the live
            // swap; both revert in one line each (fully reversible — the
            // sherpa-onnx-crate child stays the default).
            ModelEntry {
                id: "nemotron-3.5-asr-streaming-0.6b-parakeet".into(),
                role: "asr".into(),
                tiers: vec![],
                license: License {
                    name: "NVIDIA Open Model License".into(),
                    url: "https://developer.download.nvidia.com/licenses/nvidia-open-model-license-agreement-june-2024.pdf".into(),
                    acceptance_required: true,
                },
                total_bytes: 2979 + 97_590_054 + 42_164_972 + 2_454_405_120 + 406_554,
                files: vec![
                    pinned(
                        "hf:altunenes/parakeet-rs",
                        "a95331a1f347c66d68bc7e34d3eb05963bbb2f4c",
                        "nemotron-3.5-asr-streaming-0.6b-onnx/config.json",
                        "b0289e196d11a17e3c661bbadfe455c87de4baffc1a5e652a5779f5d687c5db0",
                        2_979,
                    ),
                    pinned(
                        "hf:altunenes/parakeet-rs",
                        "a95331a1f347c66d68bc7e34d3eb05963bbb2f4c",
                        "nemotron-3.5-asr-streaming-0.6b-onnx/decoder_joint.onnx",
                        "634dfadf24cb4f73c2fae170b36611d68db48186426882cbc8f7e02ed9f2bb29",
                        97_590_054,
                    ),
                    pinned(
                        "hf:altunenes/parakeet-rs",
                        "a95331a1f347c66d68bc7e34d3eb05963bbb2f4c",
                        "nemotron-3.5-asr-streaming-0.6b-onnx/encoder.onnx",
                        "d569fbe78b48fbb04e169d324f5d25463838ceed7b5fc3bfe209872441979bd9",
                        42_164_972,
                    ),
                    pinned(
                        "hf:altunenes/parakeet-rs",
                        "a95331a1f347c66d68bc7e34d3eb05963bbb2f4c",
                        "nemotron-3.5-asr-streaming-0.6b-onnx/encoder.onnx.data",
                        "7584f85df76bc9ae6fbdfa53aa8d97b07a842525d1c501d536d77fd9e4f57ac7",
                        2_454_405_120,
                    ),
                    pinned(
                        "hf:altunenes/parakeet-rs",
                        "a95331a1f347c66d68bc7e34d3eb05963bbb2f4c",
                        "nemotron-3.5-asr-streaming-0.6b-onnx/tokenizer.model",
                        "ce3895e40806f02a26c3a225161b96ef682d6c0054bae32a245dec4258d7d291",
                        406_554,
                    ),
                ],
            },
            // B73: DFN5B (ViT-H-14-378-quickgelu, Immich's ONNX export) is the
            // CLIP embedder — founder-confirmed feasible on M-series. Pinned in
            // full: the visual tower's graph onnx PLUS ~400 external-data weight
            // files (the ort-load-bearing set; see dfn5b_files.rs). License is
            // Apple ASCL, served via the Immich repo; acceptance gates.
            ModelEntry {
                id: "ViT-H-14-378-quickgelu__dfn5b".into(),
                role: "embedder".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Apple Sample Code License (ASCL)".into(),
                    url: "https://huggingface.co/immich-app/ViT-H-14-378-quickgelu__dfn5b".into(),
                    acceptance_required: true,
                },
                total_bytes: dfn5b_total,
                files: dfn5b_files,
            },
            // FP16 single-file CLIP (docs/SPIKE-COREML.md). The SAME DFN5B graph
            // as the int8 entry above, but weights INLINED into one model.onnx per
            // tower - the form CoreML can load (the int8 external-data split cannot).
            // Measured 8.77x over CPU on Apple Silicon (CoreML/ANE), near-lossless
            // (COCO nDCG 0.8212 vs int8 0.8225). Selected on macOS when config names
            // this id; the int8 entry stays the CPU fallback.
            //
            // NOT YET HOSTED: this artifact is LOCALLY converted (the immich repo
            // serves fp32, not this single-file fp16), so the repo/revision below is
            // a NOMINAL pointer and a real download URL is a backlog item (host the
            // file + re-pin). Dev machines stage the files under models/<id>/ and
            // mark installed.json, so the downloader is never invoked. The sha256 +
            // bytes ARE the real local artifact (a future hosted copy must match).
            ModelEntry {
                id: "ViT-H-14-378-quickgelu__dfn5b-fp16".into(),
                role: "embedder".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Apple Sample Code License (ASCL)".into(),
                    url: "https://huggingface.co/immich-app/ViT-H-14-378-quickgelu__dfn5b".into(),
                    acceptance_required: true,
                },
                total_bytes: 1_265_962_399 + 708_726_647 + 3_642_073,
                files: vec![
                    pinned(
                        DFN5B_REPO,
                        "local-fp16-convert",
                        "visual/model.onnx",
                        "e30e7613f2cdf1eda55fa685b467e1e04e261f20c5a15d22238682189e45ef99",
                        1_265_962_399,
                    ),
                    pinned(
                        DFN5B_REPO,
                        "local-fp16-convert",
                        "textual/model.onnx",
                        "f2cc1e79707f394373083d26abd6a51a039e319cb1bd47c65a47f3786ba368d2",
                        708_726_647,
                    ),
                    pinned(
                        DFN5B_REPO,
                        "local-fp16-convert",
                        "textual/tokenizer.json",
                        "6d9109cc838977f3ca94a379eec36aecc7c807e1785cd729660ca2fc0171fb35",
                        3_642_073,
                    ),
                ],
            },
            // B73: EmbeddingGemma-300m q8 is the text-embedder DEFAULT — better
            // paraphrase separation than Qwen3, 768 dims (half the PPVEC bytes),
            // 316 MB. onnx-community export: graph + external .onnx_data weights
            // + tokenizer.json (the `tokenizers` crate loads it, L3). Gemma terms
            // gate (same flow as the LLM; a distinct model id keeps its own
            // acceptance record — existing behavior).
            ModelEntry {
                id: "embeddinggemma-300m-q8".into(),
                role: "text-embedder".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 329_781_810,
                files: vec![
                    pinned(
                        "hf:onnx-community/embeddinggemma-300m-ONNX",
                        "5090578d9565bb06545b4552f76e6bc2c93e4a66",
                        "onnx/model_quantized.onnx",
                        "172efde319fe1542dc41f31be6154910b05b78f7a861c265c4600eec906bd6d8",
                        567_874,
                    ),
                    pinned(
                        "hf:onnx-community/embeddinggemma-300m-ONNX",
                        "5090578d9565bb06545b4552f76e6bc2c93e4a66",
                        "onnx/model_quantized.onnx_data",
                        "705626e28e4c23c82ade34566b4197d97f534c12275fa406dfb71e9937d388c0",
                        308_890_624,
                    ),
                    pinned(
                        "hf:onnx-community/embeddinggemma-300m-ONNX",
                        "5090578d9565bb06545b4552f76e6bc2c93e4a66",
                        "tokenizer.json",
                        "4dda02faaf32bc91031dc8c88457ac272b00c1016cc679757d1c441b248b9c47",
                        20_323_312,
                    ),
                ],
            },
            // B73: Qwen3-Embedding-0.6B int8 stays the configured ALTERNATIVE
            // (role text-embedder-alt, offered at tier 2 only — the llm-alt
            // precedent). Apache-2.0: display notice, no acceptance gate.
            ModelEntry {
                id: "qwen3-embedding-0.6b-int8".into(),
                role: "text-embedder-alt".into(),
                tiers: vec![2],
                license: License {
                    name: "Apache-2.0".into(),
                    url: "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B".into(),
                    acceptance_required: false,
                },
                total_bytes: 624_951_244,
                files: vec![
                    pinned(
                        "hf:onnx-community/Qwen3-Embedding-0.6B-ONNX",
                        "c25a394dd583836952667c12f008335071b3f43d",
                        "onnx/model_int8.onnx",
                        "6d0ea863f78b4a84afa3c7fcba1ec341572b5e28121aef77b7092b1dfdf679c7",
                        613_527_539,
                    ),
                    pinned(
                        "hf:onnx-community/Qwen3-Embedding-0.6B-ONNX",
                        "c25a394dd583836952667c12f008335071b3f43d",
                        "tokenizer.json",
                        "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a",
                        11_423_705,
                    ),
                ],
            },
        ],
    }
}

/// Recorded license acceptances (§5.3): model id → (license url,
/// timestamp), persisted in app data; texts/links remain viewable in
/// settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Acceptances {
    /// model id → acceptance record.
    pub accepted: BTreeMap<String, AcceptanceRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceRecord {
    pub license_url: String,
    /// RFC 3339.
    pub at: String,
}

impl Acceptances {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).expect("acceptances json"),
        )?;
        std::fs::rename(tmp, path)
    }

    pub fn accept(&mut self, model_id: &str, license_url: &str, at_rfc3339: &str) {
        self.accepted.insert(
            model_id.to_owned(),
            AcceptanceRecord {
                license_url: license_url.to_owned(),
                at: at_rfc3339.to_owned(),
            },
        );
    }

    /// The §13.7 gate input: is download permitted for this model?
    pub fn permits(&self, model: &ModelEntry) -> bool {
        !model.license.acceptance_required || self.accepted.contains_key(&model.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_manifest_pins_every_file_and_sums_tier_bytes() {
        let m = compiled_manifest();
        assert!(m.manifest_version >= 1);
        for model in &m.models {
            assert!(!model.files.is_empty());
            assert!(!model.license.url.is_empty());
            for f in &model.files {
                assert_eq!(f.sha256.len(), 64, "exact SHA-256 pin shape");
                assert!(f.bytes > 0);
                assert!(!f.revision.is_empty(), "immutable revision pin");
            }
        }
        // Tier-1 bundle with the B73 embedder pins REAL: E2B (3.91 GB) + ASR
        // (0.66 GB) + DFN5B int8 (3.95 GB, graph + ~400 external-data files) +
        // EmbeddingGemma-300m q8 (0.33 GB, the text-embedder default) +
        // DFN5B-fp16 (1.98 GB, the single-file CoreML CLIP, also offered at
        // tiers 1-2) = 10_830_366_615 bytes. Qwen3-alt is tier-2-only, so it is
        // NOT counted here. Exact, not a range — every byte traces to a pinned
        // file. (The fp16 bundle byte is nominal: dev machines stage it locally;
        // a hosted download URL is a backlog item, see the entry comment.)
        assert_eq!(m.total_bytes_at(1), 10_830_366_615, "tier-1 pinned sum");
        assert_eq!(m.total_bytes_at(0), 0, "tier 0 offers NOTHING");
    }

    /// The MTP (multi-token-prediction) LLM variants are pinned, tier-2-only
    /// (the discrete-GPU tier — MTP is a CUDA win, a Metal loss, #23752), and
    /// each ships its `mtp-` drafter file beside the target. They are
    /// llm-alt: the shipped default LLM (`gemma-4-e2b-it-qat-q4_0`) is
    /// untouched, so this never changes the tier-1 floor or the laptop path.
    #[test]
    fn mtp_llm_variants_are_pinned_tier2_and_ship_a_drafter() {
        let m = compiled_manifest();
        for id in [
            "gemma-4-e2b-it-qat-q4_k_xl-mtp",
            "gemma-4-26b-a4b-it-qat-q4_k_xl-mtp",
        ] {
            let model = m.model(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(model.is_pinned(), "{id} fully pinned (real SHAs)");
            assert_eq!(model.role, "llm-alt", "{id} is a selectable alternative");
            assert_eq!(model.tiers, vec![2], "{id} offered at the GPU tier only");
            assert!(
                model.files.iter().any(|f| f.path.starts_with("mtp-")),
                "{id} ships the mtp- drafter beside the target"
            );
            assert_eq!(
                model.total_bytes,
                model.files.iter().map(|f| f.bytes).sum::<u64>(),
                "{id} total_bytes is the live file sum"
            );
        }
        // The default LLM is NOT the MTP entry — the laptop path is unchanged.
        assert!(
            m.model("gemma-4-e2b-it-qat-q4_0").is_some(),
            "the plain E2B default still exists"
        );
        // MTP entries are tier-2-only, so the tier-1 floor sum is unchanged
        // (guarded above in the pinned-sum test).
    }

    #[test]
    fn b73_embedders_are_pinned_offered_and_downloadable() {
        let m = compiled_manifest();
        // The plan flips the P6.3-era unpinned embedder estimates to REAL
        // pins: every embedder model now resolves and is fully pinned (no
        // all-zero sha, no UNPINNED-P6.3 revision), so consent offers them
        // and the downloader will fetch them (B55 stops refusing).
        for id in [
            "ViT-H-14-378-quickgelu__dfn5b",
            "embeddinggemma-300m-q8",
            "qwen3-embedding-0.6b-int8",
        ] {
            let model = m.model(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(model.is_pinned(), "{id} fully pinned");
        }

        // EmbeddingGemma is the text-embedder DEFAULT — offered at tier 1.
        let gemma = m.model("embeddinggemma-300m-q8").unwrap();
        assert_eq!(gemma.role, "text-embedder");
        assert!(
            gemma.tiers.contains(&1),
            "default offered at the tier-1 floor"
        );
        assert!(gemma.license.acceptance_required, "Gemma terms gate");

        // Qwen3 is the configured ALTERNATIVE — tier-2-only, no gate.
        let qwen = m.model("qwen3-embedding-0.6b-int8").unwrap();
        assert_eq!(qwen.role, "text-embedder-alt");
        assert_eq!(
            qwen.tiers,
            vec![2],
            "alt follows the llm-alt tier precedent"
        );
        assert!(!qwen.license.acceptance_required, "Apache-2.0: notice only");

        // DFN5B is enumerated in full: graph onnx + ~400 external-data files,
        // and total_bytes is the exact sum of every enumerated file.
        let dfn = m.model("ViT-H-14-378-quickgelu__dfn5b").unwrap();
        assert_eq!(dfn.files.len(), 400, "visual/ + textual/ enumerated whole");
        assert!(
            dfn.files.iter().any(|f| f.path == "visual/model.onnx"),
            "the visual graph onnx is present"
        );
        assert!(
            dfn.files
                .iter()
                .any(|f| f.path.starts_with("visual/visual.transformer.")),
            "external-data weights are pinned, not just the graph"
        );
        assert_eq!(
            dfn.total_bytes,
            dfn.files.iter().map(|f| f.bytes).sum::<u64>(),
            "total_bytes is the live file sum, never an estimate"
        );
    }

    /// B74: the Nemotron 3.5 entry is a STAGED pin — fully resolvable
    /// (real SHAs) so a future swap is a one-line tier flip, but offered at
    /// NO tier so it never enters a consent sum, the downloader never
    /// enqueues it, and the live ASR path (the 560 ms-lineage entry) is
    /// untouched. GO is blocked on the sherpa-onnx Rust crate (see
    /// docs/PLAN-NEMOTRON-35.md); this test guards the "doesn't ship by
    /// accident" property until then.
    #[test]
    fn nemotron_35_is_pinned_but_offered_at_no_tier() {
        let m = compiled_manifest();
        let n35 = m
            .model("nemotron-3.5-asr-streaming-0.6b-560ms-int8")
            .expect("3.5 staged entry present");
        assert!(n35.is_pinned(), "real SHAs — resolvable when GO lands");
        assert_eq!(n35.role, "asr");
        assert!(
            n35.tiers.is_empty(),
            "STAGED: offered at no tier until the crate supports 3.5"
        );
        // The four-file transducer layout sherpa loads, same basenames as the
        // live entry so asr_wrapper_args needs no path change on swap.
        assert_eq!(n35.files.len(), 4);
        for name in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ] {
            assert!(n35.files.iter().any(|f| f.path == name), "{name} pinned");
        }
        assert_eq!(
            n35.total_bytes,
            n35.files.iter().map(|f| f.bytes).sum::<u64>(),
            "total_bytes is the live file sum"
        );
        // The live ASR path is the 560 ms-lineage entry and is the ONLY asr
        // model offered at the tier-1 floor — the staged entry must not leak in.
        let offered_asr: Vec<&str> = m
            .offered_at(1)
            .iter()
            .filter(|e| e.role == "asr")
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            offered_asr,
            vec!["nemotron-speech-streaming-en-0.6b-560ms-int8"],
            "only the current ASR model is offered; 3.5 stays staged"
        );
    }

    /// PLAN-NEMOTRON-35-SIDECAR: the parakeet-layout 3.5 entry is a SECOND
    /// staged pin (parakeet-rs loads a different model format than sherpa).
    /// Fully resolvable (real SHAs/revision), but offered at NO tier until
    /// the gate passes AND the engine-parakeet feature is built — so it
    /// never ships by accident and the live sherpa path is untouched.
    #[test]
    fn nemotron_35_parakeet_entry_is_pinned_but_staged() {
        let m = compiled_manifest();
        let pk = m
            .model("nemotron-3.5-asr-streaming-0.6b-parakeet")
            .expect("parakeet-layout 3.5 entry present");
        assert!(pk.is_pinned(), "real SHAs — resolvable when GO lands");
        assert_eq!(pk.role, "asr");
        assert!(pk.tiers.is_empty(), "STAGED: offered at no tier");
        // The parakeet-rs directory layout (NOT the four-file sherpa export):
        // config.json + encoder.onnx + .data + decoder_joint.onnx + tokenizer.
        assert_eq!(pk.files.len(), 5);
        for name in [
            "nemotron-3.5-asr-streaming-0.6b-onnx/config.json",
            "nemotron-3.5-asr-streaming-0.6b-onnx/encoder.onnx",
            "nemotron-3.5-asr-streaming-0.6b-onnx/encoder.onnx.data",
            "nemotron-3.5-asr-streaming-0.6b-onnx/decoder_joint.onnx",
            "nemotron-3.5-asr-streaming-0.6b-onnx/tokenizer.model",
        ] {
            assert!(pk.files.iter().any(|f| f.path == name), "{name} pinned");
        }
        assert_eq!(
            pk.total_bytes,
            pk.files.iter().map(|f| f.bytes).sum::<u64>(),
            "total_bytes is the file sum"
        );
        // Never offered at the tier-1 floor (the staging guarantee).
        assert!(
            !m.offered_at(1).iter().any(|e| e.id == pk.id),
            "parakeet 3.5 must not leak into the offered set"
        );
    }

    #[test]
    fn hf_repo_resolves_to_pinned_revision_urls() {
        let f = FileEntry {
            repo: "hf:ggml-org/gemma-4-e4b-it-GGUF".into(),
            revision: "abc123".into(),
            path: "gemma-4-e4b-it-Q4_K_M.gguf".into(),
            sha256: "0".repeat(64),
            bytes: 1,
        };
        assert_eq!(
            f.url(),
            "https://huggingface.co/ggml-org/gemma-4-e4b-it-GGUF/resolve/abc123/gemma-4-e4b-it-Q4_K_M.gguf"
        );
        let http = FileEntry {
            repo: "http://127.0.0.1:9999".into(),
            ..f
        };
        assert_eq!(
            http.url(),
            "http://127.0.0.1:9999/gemma-4-e4b-it-Q4_K_M.gguf"
        );
    }

    #[test]
    fn manifest_json_round_trips_and_writes_to_models_dir() {
        let dir = tempfile::tempdir().unwrap();
        let m = compiled_manifest();
        m.write_to(dir.path()).unwrap();
        let read: Manifest =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(read, m);
    }

    #[test]
    fn acceptance_gate_distinguishes_required_from_notice_only() {
        let m = compiled_manifest();
        let gemma = m.model("gemma-4-e4b-it-q4_k_m").unwrap();
        let qwen = m.model("qwen3-embedding-0.6b-int8").unwrap();
        let mut acc = Acceptances::default();
        assert!(!acc.permits(gemma), "acceptance_required gates");
        assert!(acc.permits(qwen), "Apache-2.0: notice only, no gate");
        acc.accept(&gemma.id, &gemma.license.url, "2026-06-11T00:00:00Z");
        assert!(acc.permits(gemma));
    }

    #[test]
    fn acceptances_persist_with_model_id_url_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acceptances.json");
        let mut acc = Acceptances::default();
        acc.accept(
            "gemma-4-e4b-it-q4_k_m",
            "https://ai.google.dev/gemma/terms",
            "2026-06-11T08:00:00Z",
        );
        acc.save(&path).unwrap();
        let loaded = Acceptances::load(&path);
        assert_eq!(loaded, acc);
        let rec = &loaded.accepted["gemma-4-e4b-it-q4_k_m"];
        assert_eq!(rec.license_url, "https://ai.google.dev/gemma/terms");
        assert_eq!(rec.at, "2026-06-11T08:00:00Z");
    }
}
