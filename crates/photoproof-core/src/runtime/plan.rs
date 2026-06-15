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
    /// Tier-0-whole / §13.3: nothing spawns, nothing downloads. Only the
    /// LLM and ASR are external CHILDREN — the embedders run in-process
    /// (§3.3 ort exception), so an installed embedder's `Run` (P7.4: "load
    /// ort sessions") is never a spawned child and must not flip this. The
    /// Tier-0 floor returns NotConfigured for every slot anyway, so the
    /// whole-product criterion holds regardless of embedder state.
    pub fn spawns_nothing(&self) -> bool {
        !self.llm.spawns_child() && !self.asr.spawns_child()
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

/// Models whose installed `manifest_version` is BEHIND the running manifest:
/// their on-disk weights may be stale (a manifest upgrade could have changed the
/// entry's files/revision). Previously `InstalledRecord.manifest_version` was
/// written at install time and never read, so a manifest upgrade silently ran
/// old weights (STATE-INTEGRITY-AUDIT.md). This SURFACES the drift rather than
/// forcing a re-download: a global manifest bump for one model must not nuke
/// every other model's weights, so the shell logs this (and may offer a
/// re-download) while the models keep running. Pure + testable.
pub fn stale_installed_models(
    manifest: &Manifest,
    installed: &BTreeMap<String, InstalledRecord>,
) -> Vec<String> {
    let mut stale: Vec<String> = installed
        .iter()
        .filter(|(_, rec)| rec.manifest_version < manifest.manifest_version)
        .map(|(id, _)| id.clone())
        .collect();
    stale.sort(); // deterministic order for logs + tests
    stale
}

/// Embedder slot resolution WITH a bypass (STATE-INTEGRITY-AUDIT; founder: the
/// fp16 CLIP default is locally-converted and not hosted, so a fresh/unstaged
/// machine cannot download it). When the CONFIGURED embedder model is not
/// installed, fall back to ANY installed model of the same manifest `role`
/// offered at this tier, so the app uses a real embedder instead of going dark
/// on a download that can only 404. The configured model still wins when it IS
/// installed; an `External` config path is handled by the caller, not here.
/// Deterministic: installed ids are sorted, first same-role match wins.
fn embedder_plan(
    configured_id: &str,
    role: &str,
    tier: u8,
    manifest: &Manifest,
    installed: &BTreeMap<String, InstalledRecord>,
) -> ProcessPlan {
    let direct = local_model_plan(configured_id, tier, manifest, installed);
    if matches!(direct, ProcessPlan::Run { .. }) {
        return direct; // configured model is installed: it wins
    }
    // Configured model is not runnable (not installed / unknown / wrong tier).
    // Bypass to an installed same-role model offered at this tier.
    let mut candidates: Vec<&String> = installed
        .keys()
        .filter(|id| {
            manifest
                .model(id)
                .is_some_and(|e| e.role == role && e.tiers.contains(&tier))
        })
        .collect();
    candidates.sort();
    match candidates.first() {
        Some(fallback) => ProcessPlan::Run {
            model_id: (*fallback).clone(),
        },
        None => direct, // keep the honest NotConfigured (nothing to fall back to)
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
    // Embedders are in-process (§3.3) — never external children. Their
    // `Run` means "load the ort sessions", which the shell's EmbedderHost
    // consumes on a background thread (P7.4 decision 4); the same
    // installed/tier/backend gating as the children applies, so reuse it
    // verbatim. (P6.2 deferred this to a NotConfigured placeholder until
    // the P7 ort connector existed; P7.4 is that moment.)
    RuntimePlan {
        effective_tier,
        llm,
        asr,
        clip_embedder: embedder_plan(
            &config.embedder.model,
            "embedder",
            effective_tier,
            manifest,
            installed,
        ),
        text_embedder: embedder_plan(
            &config.embedder.text.model,
            "text-embedder",
            effective_tier,
            manifest,
            installed,
        ),
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
    fn stale_installed_models_flags_only_behind_records() {
        let manifest = compiled_manifest(); // manifest_version = 1
        let rec = |v: u32| InstalledRecord {
            manifest_version: v,
            when: "2026-06-11T00:00:00Z".into(),
        };
        let installed: BTreeMap<String, InstalledRecord> = [
            ("old-a".to_owned(), rec(0)),   // behind -> stale
            ("current".to_owned(), rec(1)), // at manifest version -> fine
            ("old-b".to_owned(), rec(0)),   // behind -> stale
        ]
        .into_iter()
        .collect();
        assert_eq!(
            stale_installed_models(&manifest, &installed),
            vec!["old-a".to_owned(), "old-b".to_owned()],
            "only records behind the manifest version are stale, sorted"
        );
        // A fully-current install reports nothing.
        let current: BTreeMap<String, InstalledRecord> =
            [("current".to_owned(), rec(1))].into_iter().collect();
        assert!(stale_installed_models(&manifest, &current).is_empty());
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
                "gemma-4-e2b-it-qat-q4_0",
                // the default ASR is now Nemotron 3.5 via parakeet (the int8
                // English transducer stays a config-selectable fallback)
                "nemotron-3.5-asr-streaming-0.6b-parakeet",
            ]),
        );
        assert_eq!(
            p.llm,
            ProcessPlan::Run {
                model_id: "gemma-4-e2b-it-qat-q4_0".into()
            }
        );
        assert_eq!(
            p.asr,
            ProcessPlan::Run {
                model_id: "nemotron-3.5-asr-streaming-0.6b-parakeet".into()
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

    /// P7.4 decision 4: an installed embedder converges on `Run` (the
    /// signal the EmbedderHost loads ort sessions on), but it is in-process
    /// — `spawns_nothing` (external children only) stays true. The default
    /// config names the DFN5B CLIP embedder + EmbeddingGemma text embedder.
    #[test]
    fn installed_embedders_run_in_process_without_spawning_a_child() {
        let cfg = from_toml_str("").unwrap().config;
        // The default CLIP id is the fp16 single-file export (CoreML/CUDA-ready).
        let clip_id = "ViT-H-14-378-quickgelu__dfn5b-fp16";
        let p = plan(
            &cfg,
            1,
            &compiled_manifest(),
            &installed(&[clip_id, "embeddinggemma-300m-q8"]),
        );
        assert_eq!(
            p.clip_embedder,
            ProcessPlan::Run {
                model_id: clip_id.into()
            }
        );
        assert_eq!(
            p.text_embedder,
            ProcessPlan::Run {
                model_id: "embeddinggemma-300m-q8".into()
            }
        );
        assert!(
            p.spawns_nothing(),
            "in-process embedders are never external children"
        );
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

    /// The embedder bypass (founder: the fp16 CLIP default is not hosted, so an
    /// unstaged machine cannot download it). The DEFAULT config names the fp16
    /// CLIP; if only the base dfn5b (same "embedder" role) is installed, the
    /// clip slot falls back to it instead of going dark on a 404 download.
    #[test]
    fn embedder_bypass_falls_back_to_installed_same_role_model() {
        let cfg = from_toml_str("").unwrap().config; // default CLIP = fp16
        let p = plan(
            &cfg,
            1,
            &compiled_manifest(),
            // fp16 NOT installed; the base dfn5b + the text embedder ARE.
            &installed(&["ViT-H-14-378-quickgelu__dfn5b", "embeddinggemma-300m-q8"]),
        );
        assert_eq!(
            p.clip_embedder,
            ProcessPlan::Run {
                model_id: "ViT-H-14-378-quickgelu__dfn5b".into()
            },
            "bypass to the installed base dfn5b when the fp16 default is absent"
        );
        assert_eq!(
            p.text_embedder,
            ProcessPlan::Run {
                model_id: "embeddinggemma-300m-q8".into()
            }
        );
    }

    /// With nothing of the embedder role installed, the bypass has nothing to
    /// fall back to: keep the honest NotConfigured (fixable by download), never
    /// invent a model.
    #[test]
    fn embedder_bypass_keeps_notconfigured_with_no_candidate() {
        let cfg = from_toml_str("").unwrap().config;
        let p = plan(&cfg, 1, &compiled_manifest(), &installed(&[]));
        assert!(
            matches!(
                p.clip_embedder,
                ProcessPlan::NotConfigured {
                    fixable_by_download: true,
                    ..
                }
            ),
            "no same-role model installed => honest NotConfigured: {:?}",
            p.clip_embedder
        );
    }
}
