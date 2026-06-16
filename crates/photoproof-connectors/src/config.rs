//! Typed connector configuration — the literal TOML schema of
//! spec/RUNTIME.md §4.4 (`{app-data}/config.toml`, runtime-relevant
//! sections).
//!
//! Parse rules (RUNTIME §4.4, normative):
//! - **Unknown keys warn; missing sections take defaults.** `from_toml_str`
//!   returns the parsed config plus the list of unknown key paths; logging
//!   the warning is the caller's job.
//! - **API keys are never stored in this file**: `api_key_ref` is an
//!   OS-keychain reference (`keychain:<service>/<account>`), resolved at
//!   call time. A literal key found in config is rejected at parse time
//!   with a hard error.
//! - A config naming an uninstalled model puts that connector in
//!   `NotConfigured` — that resolution happens in supervision
//!   (photoproof-core), not here.

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Hard parse-time error. Distinct from warnings (unknown keys), which
/// never fail the parse.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid config TOML: {0}")]
    Parse(#[from] toml::de::Error),
    /// RUNTIME §4.4: a literal key in config is a hard error; `api_key_ref`
    /// must be empty or `keychain:<service>/<account>`.
    #[error(
        "`{field}` must be empty or an OS-keychain reference \
         (`keychain:<service>/<account>`); literal API keys are never \
         stored in config"
    )]
    LiteralApiKey { field: &'static str },
    #[error("invalid value for `{field}`: {message}")]
    InvalidValue {
        field: &'static str,
        message: String,
    },
}

/// A successfully parsed config plus its unknown-key warnings.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: Config,
    /// Dotted paths of keys present in the TOML but unknown to the schema
    /// (e.g. `"runtime.bogus"`). RUNTIME §4.4: warn, never fail.
    pub unknown_keys: Vec<String>,
}

/// Parse + validate the runtime-relevant sections of `config.toml`.
pub fn from_toml_str(input: &str) -> Result<LoadedConfig, ConfigError> {
    let mut unknown_keys = Vec::new();
    let deserializer = toml::Deserializer::new(input);
    let config: Config = serde_ignored::deserialize(deserializer, |path| {
        unknown_keys.push(path.to_string());
    })?;
    config.validate()?;
    Ok(LoadedConfig {
        config,
        unknown_keys,
    })
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub llm: LlmConfig,
    pub asr: AsrConfig,
    pub embedder: EmbedderConfig,
}

impl Config {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_api_key_ref(
            &self.llm.openai_compatible.api_key_ref,
            "llm.openai-compatible.api_key_ref",
        )?;
        validate_api_key_ref(&self.llm.anthropic.api_key_ref, "llm.anthropic.api_key_ref")?;
        const CHUNK_MS: [u32; 4] = [80, 160, 560, 1120];
        if !CHUNK_MS.contains(&self.asr.chunk_ms) {
            return Err(ConfigError::InvalidValue {
                field: "asr.chunk_ms",
                message: format!(
                    "{} is not a model-supported chunk size (80 | 160 | 560 | 1120)",
                    self.asr.chunk_ms
                ),
            });
        }
        Ok(())
    }
}

fn validate_api_key_ref(value: &str, field: &'static str) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Ok(());
    }
    if let Some(rest) = value.strip_prefix("keychain:")
        && let Some((service, account)) = rest.split_once('/')
        && !service.is_empty()
        && !account.is_empty()
    {
        return Ok(());
    }
    Err(ConfigError::LiteralApiKey { field })
}

// ------------------------------------------------------------ [runtime] ----

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// "auto" | 0 | 1 | 2 — user override always wins (RUNTIME §6.2).
    pub tier: Tier,
    /// Empty = default `{app-data}/models` (resolved by the caller).
    pub models_dir: String,
    /// Never plan above (detected VRAM − headroom), RUNTIME §9.
    pub vram_headroom_mb: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            tier: Tier::Auto,
            models_dir: String::new(),
            vram_headroom_mb: 2048,
        }
    }
}

/// `tier = "auto" | 0 | 1 | 2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    #[default]
    Auto,
    Fixed(u8),
}

impl<'de> Deserialize<'de> for Tier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TierVisitor;
        impl Visitor<'_> for TierVisitor {
            type Value = Tier;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("\"auto\" or an integer tier 0, 1, or 2")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Tier, E> {
                if v == "auto" {
                    Ok(Tier::Auto)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Tier, E> {
                match v {
                    0..=2 => Ok(Tier::Fixed(v as u8)),
                    _ => Err(E::invalid_value(de::Unexpected::Signed(v), &self)),
                }
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Tier, E> {
                match v {
                    0..=2 => Ok(Tier::Fixed(v as u8)),
                    _ => Err(E::invalid_value(de::Unexpected::Unsigned(v), &self)),
                }
            }
        }
        deserializer.deserialize_any(TierVisitor)
    }
}

// ---------------------------------------------------------------- [llm] ----

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub backend: LlmBackend,
    /// Manifest model id (local) or API model name.
    pub model: String,
    #[serde(rename = "local-llamacpp")]
    pub local_llamacpp: LlamacppConfig,
    #[serde(rename = "openai-compatible")]
    pub openai_compatible: OpenAiCompatibleConfig,
    pub anthropic: AnthropicConfig,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            backend: LlmBackend::LocalLlamacpp,
            // B68 (P6.3 spike): E2B QAT q4_0 is the default — half the
            // footprint, 2× the speed, schema probe 50/50, and the
            // interactive parse fits §9's 2 s budget on the Tier-1 floor.
            // E4B (gemma-4-e4b-it-q4_k_m) remains config-selectable.
            //
            // MTP (multi-token-prediction) variants are also selectable on
            // discrete-GPU (tier-2) machines — set `model` to
            // `gemma-4-e2b-it-qat-q4_k_xl-mtp` (laptop-class E2B + drafter)
            // or `gemma-4-26b-a4b-it-qat-q4_k_xl-mtp` (the RTX-5080 quality
            // pick). These are CUDA-only wins: the supervisor strips the MTP
            // flags on Apple Silicon, where MTP is a NET LOSS (#23752), so
            // naming one on a Mac just runs the plain target. No model-pick
            // logic changes here — any manifest id flows through unchanged,
            // and tier/install gating is enforced in the runtime plan.
            // See docs/PLAN-GEMMA-MTP.md.
            model: "gemma-4-e2b-it-qat-q4_0".into(),
            local_llamacpp: LlamacppConfig::default(),
            openai_compatible: OpenAiCompatibleConfig::default(),
            anthropic: AnthropicConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmBackend {
    #[default]
    LocalLlamacpp,
    OpenaiCompatible,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LlamacppConfig {
    /// TOTAL; divided across slots (llama.cpp #11681) → 8192/lane.
    pub ctx_size: u32,
    /// 1 interactive + 1 background lane (RUNTIME §9).
    pub parallel_slots: u32,
    /// "auto" | integer | 0 (CPU).
    pub gpu_layers: GpuLayers,
    pub startup_timeout_secs: u64,
}

impl Default for LlamacppConfig {
    fn default() -> Self {
        Self {
            ctx_size: 16384,
            parallel_slots: 2,
            gpu_layers: GpuLayers::Auto,
            startup_timeout_secs: 120,
        }
    }
}

/// `gpu_layers = "auto" | integer | 0 (CPU)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuLayers {
    #[default]
    Auto,
    /// 0 = CPU.
    Layers(u32),
}

impl<'de> Deserialize<'de> for GpuLayers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct GpuLayersVisitor;
        impl Visitor<'_> for GpuLayersVisitor {
            type Value = GpuLayers;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("\"auto\" or a non-negative layer count")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<GpuLayers, E> {
                if v == "auto" {
                    Ok(GpuLayers::Auto)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<GpuLayers, E> {
                u32::try_from(v)
                    .map(GpuLayers::Layers)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<GpuLayers, E> {
                u32::try_from(v)
                    .map(GpuLayers::Layers)
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(v), &self))
            }
        }
        deserializer.deserialize_any(GpuLayersVisitor)
    }
}

/// Any OAI-speaking endpoint: vLLM, Ollama, hosted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    /// Optional; e.g. "keychain:photoproof/openai-compat".
    pub api_key_ref: String,
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key_ref: String::new(),
        }
    }
}

/// M5: thin adapter behind LanguageModel — maps ChatRequest → Messages API,
/// json_schema → tool-forced structured output.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub api_key_ref: String,
    pub model: String,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.anthropic.com".into(),
            api_key_ref: "keychain:photoproof/anthropic".into(),
            model: "claude-sonnet-latest".into(),
        }
    }
}

// ---------------------------------------------------------------- [asr] ----

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AsrConfig {
    pub backend: AsrBackend,
    pub model: String,
    /// 80 | 160 | 560 | 1120 (model-supported; validated at parse time).
    pub chunk_ms: u32,
    /// "cpu" (default, all tiers) | "gpu".
    pub device: AsrDevice,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            backend: AsrBackend::LocalSherpa,
            // Best validated default: Nemotron 3.5 via the parakeet-rs engine
            // (WER 1.25%, native punctuation + caps + multilingual; real-time on
            // M1 4.50x / 9900X 7.75x, PLAN-NEMOTRON-35-SIDECAR §11). The
            // pp-asr-server binary built with `engine-parakeet` runtime-dispatches
            // to the right engine by model layout, so the int8 English transducer
            // (`nemotron-speech-streaming-en-0.6b-560ms-int8`) stays a config-
            // selectable lighter fallback.
            model: "nemotron-3.5-asr-streaming-0.6b-parakeet".into(),
            chunk_ms: 560,
            device: AsrDevice::Cpu,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AsrBackend {
    #[default]
    LocalSherpa,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AsrDevice {
    #[default]
    Cpu,
    Gpu,
}

// ----------------------------------------------------------- [embedder] ----

/// The CLIP embedder (`image_clip` vectors + short S4 query embeddings).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct EmbedderConfig {
    pub backend: EmbedderBackend,
    /// Or ViT-H-14-quickgelu__dfn5b (224 px).
    pub model: String,
    pub device: EmbedDevice,
    /// `[embedder.text]` — the text-embedding model (RUNTIME §3.3,
    /// DECISIONS X3): annotation chunks & summaries. Query/document
    /// instruction prefixes: template owned by RETRIEVAL (R1); queries
    /// instructed, documents bare.
    pub text: TextEmbedderConfig,
}

impl Default for EmbedderConfig {
    fn default() -> Self {
        // PLATFORM-CONDITIONAL CLIP default.
        // - CUDA build -> fp16: TensorRT compiles the WHOLE graph as one unit, so
        //   fp16 is 54-85x with a fast load (no fragmentation).
        // - macOS + plain CPU builds -> the int8 base on CPU. The fp16 export
        //   *would* graduate to CoreML, but the DFN5B graph fragments into ~36
        //   CoreML partitions (one per transformer block), so a cache-WARM load
        //   measured 878s (~14.6 min) EVERY launch -- worse than int8/CPU's ~1.5s
        //   load. Folding the attention Sqrt did NOT reduce the partitions, so the
        //   fragmenting op is still unidentified (see the backlog investigation
        //   "CoreML CLIP graph fragmentation"). Until that lands, macOS ships
        //   int8/CPU: instant startup, background embedding. The fp16 model stays
        //   in the manifest and is one `[embedder] model = ...` line away.
        #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
        let model = "ViT-H-14-378-quickgelu__dfn5b-fp16";
        #[cfg(not(all(not(target_os = "macos"), feature = "cuda")))]
        let model = "ViT-H-14-378-quickgelu__dfn5b";
        Self {
            backend: EmbedderBackend::LocalOrt,
            model: model.into(),
            device: EmbedDevice::Auto,
            text: TextEmbedderConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderBackend {
    #[default]
    LocalOrt,
    OpenaiCompatible,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TextEmbedderConfig {
    pub backend: TextEmbedderBackend,
    pub model: String,
    pub device: EmbedDevice,
}

impl Default for TextEmbedderConfig {
    fn default() -> Self {
        Self {
            backend: TextEmbedderBackend::LocalOrt,
            // B73: EmbeddingGemma-300m q8 is the bake-off winner and the
            // shipped default; Qwen3-Embedding-0.6B is the configured
            // alternative (manifest role text-embedder-alt).
            model: "embeddinggemma-300m-q8".into(),
            device: EmbedDevice::Cpu,
        }
    }
}

/// `"local-ort" | "local-llamacpp" (/v1/embeddings) | "openai-compatible"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextEmbedderBackend {
    #[default]
    LocalOrt,
    LocalLlamacpp,
    OpenaiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedDevice {
    #[default]
    Auto,
    Cpu,
    Gpu,
}
