//! The runtime plan (spec/RUNTIME.md §4.4, §6.2, §7): config + effective
//! tier + installed weights → what each connector does. Pure — the shell
//! maps plans onto supervisors and the download manager.
//!
//! The honest floor, restated: **Tier 0 means NO runtime task exists at
//! all** — zero children, zero download activity, not merely hidden UI
//! (the §13.3 whole-product criterion).

use std::collections::BTreeMap;

use photoproof_connectors::config::{AsrBackend, Config, LlmBackend};

use super::download::InstalledRecord;
use super::manifest::Manifest;

/// What one connector does under the current plan.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessPlan {
    /// Feature dark, journal unaffected (§4.4); the fix surfaces in
    /// settings + debug panel.
    NotConfigured {
        reason: String,
        /// True when a download (after consent) would light it up —
        /// settings' "fix" affordance.
        fixable_by_download: bool,
    },
    /// Supervise a local child for this model (weights installed).
    Run { model_id: String },
    /// The user's external OAI endpoint (§4.4 escape hatch): no child,
    /// no download — reachable is Ready as far as gating cares.
    External { base_url: String },
}

impl ProcessPlan {
    pub fn spawns_child(&self) -> bool {
        matches!(self, ProcessPlan::Run { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimePlan {
    pub effective_tier: u8,
    pub llm: ProcessPlan,
    pub asr: ProcessPlan,
    /// In-process embedders (the §3.3 ort exception — NOT children; the
    /// real sessions are M3/P7 work behind this gate).
    pub clip_embedder: ProcessPlan,
    pub text_embedder: ProcessPlan,
}

impl RuntimePlan {
    /// Tier-0-whole / §13.3: nothing spawns, nothing downloads.
    pub fn spawns_nothing(&self) -> bool {
        !self.llm.spawns_child()
            && !self.asr.spawns_child()
            && !self.clip_embedder.spawns_child()
            && !self.text_embedder.spawns_child()
    }
}

fn dark(reason: &str) -> ProcessPlan {
    ProcessPlan::NotConfigured {
        reason: reason.to_owned(),
        fixable_by_download: false,
    }
}

fn local_model_plan(
    model_id: &str,
    tier: u8,
    manifest: &Manifest,
    installed: &BTreeMap<String, InstalledRecord>,
) -> ProcessPlan {
    let Some(entry) = manifest.model(model_id) else {
        return dark(&format!("config names unknown model {model_id:?}"));
    };
    if !entry.tiers.contains(&tier) {
        return dark(&format!("{model_id} is not offered at tier {tier}"));
    }
    if !installed.contains_key(model_id) {
        // §4.4: uninstalled model reference ⇒ NotConfigured, feature
        // dark, journal unaffected; consent + download flips it.
        return ProcessPlan::NotConfigured {
            reason: format!("{model_id} not installed"),
            fixable_by_download: true,
        };
    }
    ProcessPlan::Run {
        model_id: model_id.to_owned(),
    }
}

/// Resolve the plan. `effective_tier` comes from tier::decide_tier (user
/// override already applied — it always wins, §6.2).
pub fn plan(
    config: &Config,
    effective_tier: u8,
    manifest: &Manifest,
    installed: &BTreeMap<String, InstalledRecord>,
) -> RuntimePlan {
    // §1.4/§6.2: below the floor the app is fully functional as a
    // journal; no models, no downloads, no degradation of any M1 feature.
    if effective_tier == 0 {
        let floor = dark("tier 0: the full journal, no models (RUNTIME §1.4)");
        return RuntimePlan {
            effective_tier: 0,
            llm: floor.clone(),
            asr: floor.clone(),
            clip_embedder: floor.clone(),
            text_embedder: floor,
        };
    }
    let llm = match config.llm.backend {
        LlmBackend::LocalLlamacpp => {
            local_model_plan(&config.llm.model, effective_tier, manifest, installed)
        }
        LlmBackend::OpenaiCompatible => ProcessPlan::External {
            base_url: config.llm.openai_compatible.base_url.clone(),
        },
        LlmBackend::Anthropic => ProcessPlan::External {
            base_url: config.llm.anthropic.base_url.clone(),
        },
    };
    let asr = match config.asr.backend {
        AsrBackend::LocalSherpa => {
            local_model_plan(&config.asr.model, effective_tier, manifest, installed)
        }
        AsrBackend::Disabled => dark("asr backend disabled in config"),
    };
    // Embedders are in-process (§3.3) — never children; the plan only
    // gates the feature. Their `Run` means "load ort sessions" (M3/P7).
    let embedder_plan =
        |model: &str| match local_model_plan(model, effective_tier, manifest, installed) {
            ProcessPlan::Run { model_id } => ProcessPlan::NotConfigured {
                reason: format!("{model_id} installed; ort sessions land with M3 (P7)"),
                fixable_by_download: false,
            },
            other => other,
        };
    RuntimePlan {
        effective_tier,
        llm,
        asr,
        clip_embedder: embedder_plan(&config.embedder.model),
        text_embedder: embedder_plan(&config.embedder.text.model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::manifest::compiled_manifest;
    use photoproof_connectors::config::from_toml_str;

    fn installed(ids: &[&str]) -> BTreeMap<String, InstalledRecord> {
        ids.iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    InstalledRecord {
                        manifest_version: 1,
                        when: "2026-06-11T00:00:00Z".into(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn tier_0_plans_nothing_at_all() {
        let cfg = from_toml_str("").unwrap().config;
        let p = plan(&cfg, 0, &compiled_manifest(), &installed(&[]));
        assert!(p.spawns_nothing(), "Tier 0 is whole: zero children");
        for proc_plan in [&p.llm, &p.asr, &p.clip_embedder, &p.text_embedder] {
            assert!(
                matches!(
                    proc_plan,
                    ProcessPlan::NotConfigured {
                        fixable_by_download: false,
                        ..
                    }
                ),
                "no download affordance below the floor: {proc_plan:?}"
            );
        }
    }

    #[test]
    fn uninstalled_model_reference_is_not_configured_feature_dark() {
        let cfg = from_toml_str("").unwrap().config;
        let p = plan(&cfg, 1, &compiled_manifest(), &installed(&[]));
        assert!(
            matches!(
                &p.llm,
                ProcessPlan::NotConfigured {
                    fixable_by_download: true,
                    ..
                }
            ),
            "§4.4: uninstalled model ⇒ NotConfigured, fixable: {:?}",
            p.llm
        );
        assert!(p.spawns_nothing(), "nothing runs yet");
    }

    #[test]
    fn installed_models_run_and_unknown_ids_stay_dark() {
        let cfg = from_toml_str("").unwrap().config;
        let p = plan(
            &cfg,
            1,
            &compiled_manifest(),
            &installed(&[
                "gemma-4-e4b-it-q4_k_m",
                "nemotron-speech-streaming-en-0.6b-int8",
            ]),
        );
        assert_eq!(
            p.llm,
            ProcessPlan::Run {
                model_id: "gemma-4-e4b-it-q4_k_m".into()
            }
        );
        assert_eq!(
            p.asr,
            ProcessPlan::Run {
                model_id: "nemotron-speech-streaming-en-0.6b-int8".into()
            }
        );

        let bad = from_toml_str("[llm]\nmodel = \"never-heard-of-it\"\n")
            .unwrap()
            .config;
        let p = plan(&bad, 1, &compiled_manifest(), &installed(&[]));
        assert!(matches!(
            &p.llm,
            ProcessPlan::NotConfigured {
                fixable_by_download: false,
                ..
            }
        ));
    }

    #[test]
    fn external_backend_is_the_escape_hatch_no_child_no_download() {
        let cfg = from_toml_str("[llm]\nbackend = \"openai-compatible\"\n")
            .unwrap()
            .config;
        let p = plan(&cfg, 1, &compiled_manifest(), &installed(&[]));
        assert_eq!(
            p.llm,
            ProcessPlan::External {
                base_url: "http://127.0.0.1:11434/v1".into()
            }
        );
        assert!(!p.llm.spawns_child());
    }

    #[test]
    fn disabled_asr_backend_goes_dark() {
        let cfg = from_toml_str("[asr]\nbackend = \"disabled\"\n")
            .unwrap()
            .config;
        let p = plan(&cfg, 1, &compiled_manifest(), &installed(&[]));
        assert!(matches!(&p.asr, ProcessPlan::NotConfigured { .. }));
    }
}
