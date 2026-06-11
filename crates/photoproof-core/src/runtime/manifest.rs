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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    /// "llm" | "asr" | "embedder" | "text-embedder".
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

    /// Final on-disk filename (exact filename pin).
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
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

/// The compiled-in manifest for this release. SHA-256 pins are
/// PLACEHOLDERS until the P6.3 spike pins real artifacts on the founder
/// machine — no real download can begin against a zero pin (verification
/// would fail by construction), which is exactly right while no real
/// weights are in scope. Sizes are the §5.4 planning estimates.
pub fn compiled_manifest() -> Manifest {
    const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    let f = |repo: &str, path: &str, bytes: u64| FileEntry {
        repo: repo.into(),
        revision: "UNPINNED-P6.3".into(),
        path: path.into(),
        sha256: UNPINNED.into(),
        bytes,
    };
    Manifest {
        manifest_version: 1,
        models: vec![
            ModelEntry {
                id: "gemma-4-e4b-it-q4_k_m".into(),
                role: "llm".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Gemma Terms of Use".into(),
                    url: "https://ai.google.dev/gemma/terms".into(),
                    acceptance_required: true,
                },
                total_bytes: 5_460_000_000,
                files: vec![
                    f(
                        "hf:ggml-org/gemma-4-e4b-it-GGUF",
                        "gemma-4-e4b-it-Q4_K_M.gguf",
                        4_870_000_000,
                    ),
                    f(
                        "hf:ggml-org/gemma-4-e4b-it-GGUF",
                        "mmproj-gemma-4-e4b-it-f16.gguf",
                        590_000_000,
                    ),
                ],
            },
            ModelEntry {
                id: "nemotron-speech-streaming-en-0.6b-int8".into(),
                role: "asr".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "NVIDIA Open Model License".into(),
                    url: "https://developer.download.nvidia.com/licenses/nvidia-open-model-license-agreement-june-2024.pdf".into(),
                    acceptance_required: true,
                },
                total_bytes: 800_000_000,
                files: vec![f(
                    "hf:csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-2026-01-14",
                    "model.int8.onnx",
                    800_000_000,
                )],
            },
            ModelEntry {
                id: "ViT-H-14-378-quickgelu__dfn5b".into(),
                role: "embedder".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Apple DFN model license".into(),
                    url: "https://huggingface.co/apple/DFN5B-CLIP-ViT-H-14-378".into(),
                    // Spike confirms exact terms (§5.3); the conservative
                    // default gates.
                    acceptance_required: true,
                },
                total_bytes: 2_600_000_000,
                files: vec![
                    f("hf:immich-app/ViT-H-14-378-quickgelu__dfn5b", "visual/model.onnx", 2_400_000_000),
                    f("hf:immich-app/ViT-H-14-378-quickgelu__dfn5b", "textual/model.onnx", 200_000_000),
                ],
            },
            ModelEntry {
                id: "qwen3-embedding-0.6b-int8".into(),
                role: "text-embedder".into(),
                tiers: vec![1, 2],
                license: License {
                    name: "Apache-2.0".into(),
                    url: "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B".into(),
                    acceptance_required: false,
                },
                total_bytes: 600_000_000,
                files: vec![f("hf:Qwen/Qwen3-Embedding-0.6B", "model.int8.onnx", 600_000_000)],
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
        // §5.4: Tier-1 bundle ≈ 9.5 GB (planning estimate).
        let total = m.total_bytes_at(1);
        assert!(total > 9_000_000_000 && total < 10_000_000_000, "{total}");
        assert_eq!(m.total_bytes_at(0), 0, "tier 0 offers NOTHING");
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
