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
                id: "nemotron-speech-streaming-en-0.6b-160ms-int8".into(),
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
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
                        "237e551abd7a411ef92d3595454d9f6ab5fe7d6c",
                        "encoder.int8.onnx",
                        "71111f61b18e1e65e01e369434a5c0434868d2f44892742ae54240600c681209",
                        652_916_849,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
                        "237e551abd7a411ef92d3595454d9f6ab5fe7d6c",
                        "decoder.int8.onnx",
                        "0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98",
                        7_257_753,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
                        "237e551abd7a411ef92d3595454d9f6ab5fe7d6c",
                        "joiner.int8.onnx",
                        "a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23",
                        1_735_862,
                    ),
                    pinned(
                        "hf:csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
                        "237e551abd7a411ef92d3595454d9f6ab5fe7d6c",
                        "tokens.txt",
                        "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff",
                        8_952,
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
        // (0.66 GB) + DFN5B (3.95 GB, graph + ~400 external-data files) +
        // EmbeddingGemma-300m q8 (0.33 GB, the text-embedder default) =
        // 8_852_035_496 bytes. Qwen3-alt is tier-2-only, so it is NOT counted
        // here. Exact, not a range — every byte traces to a pinned file.
        assert_eq!(m.total_bytes_at(1), 8_852_035_496, "tier-1 pinned sum");
        assert_eq!(m.total_bytes_at(0), 0, "tier 0 offers NOTHING");
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
