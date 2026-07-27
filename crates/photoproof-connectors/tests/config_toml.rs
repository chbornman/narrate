//! Typed-config TOML parse tests (spec/RUNTIME.md §4.4): the literal spec
//! schema parses, defaults apply, unknown keys warn, and every hard-error
//! rule (literal API key, invalid tier/gpu_layers/chunk_ms, bad enums/types)
//! rejects at parse time.

use photoproof_connectors::config::{
    AsrBackend, AsrDevice, Config, ConfigError, EmbedDevice, EmbedderBackend, GpuLayers,
    LlmBackend, TextEmbedderBackend, Tier, from_toml_str,
};

/// The literal TOML block from RUNTIME §4.4, comments included.
const SPEC_TOML: &str = r#"
[runtime]
tier = "auto"            # "auto" | 0 | 1 | 2  — user override always wins
models_dir = ""          # default: {app-data}/models
vram_headroom_mb = 2048  # never plan above (detected VRAM - headroom), §9

[llm]
backend = "local-llamacpp"   # "local-llamacpp" | "openai-compatible" | "anthropic"
model = "gemma-4-e2b-it-qat-q4_0"   # manifest model id (local) or API model name

[llm.local-llamacpp]
ctx_size = 16384             # TOTAL; divided across slots (#11681) → 8192/lane
parallel_slots = 2           # 1 interactive + 1 background lane
gpu_layers = "auto"          # "auto" | integer | 0 (CPU)
startup_timeout_secs = 120

[llm.openai-compatible]      # any OAI-speaking endpoint: vLLM, Ollama, hosted
base_url = "http://127.0.0.1:11434/v1"
api_key_ref = ""             # optional; "keychain:photoproof/openai-compat"

[llm.anthropic]              # M5: thin adapter behind LanguageModel —
base_url = "https://api.anthropic.com"          # maps ChatRequest→Messages API,
api_key_ref = "keychain:photoproof/anthropic"   # json_schema→tool-forced
model = "claude-sonnet-latest"                  # structured output.

[asr]
backend = "local-sherpa"     # "local-sherpa" | "disabled"
model = "nemotron-3.5-asr-streaming-0.6b-parakeet"   # int8 English fallback: nemotron-speech-streaming-en-0.6b-560ms-int8
chunk_ms = 560               # 80 | 160 | 560 | 1120 (model-supported)
device = "cpu"               # "cpu" (default, all tiers) | "gpu"

[embedder]
backend = "local-ort"        # "local-ort" | "openai-compatible"
model = "ViT-H-14-378-quickgelu__dfn5b"   # immutable hosted default on every platform; fp16 stays staged until hosted
device = "auto"              # "auto" | "cpu" | "gpu"

[embedder.text]              # the text-embedding model (§3.3): annotation
backend = "local-ort"        # chunks & summaries. "local-ort" |
model = "embeddinggemma-300m-q8"  # "local-llamacpp" (/v1/embeddings) |
device = "cpu"               # "openai-compatible" (B73: Qwen3-0.6B alt)
"#;

#[test]
fn spec_literal_block_parses_with_no_warnings() {
    let loaded = from_toml_str(SPEC_TOML).expect("spec §4.4 block must parse");
    assert!(loaded.unknown_keys.is_empty(), "{:?}", loaded.unknown_keys);
    let c = &loaded.config;

    assert_eq!(c.runtime.tier, Tier::Auto);
    assert_eq!(c.runtime.models_dir, "");
    assert_eq!(c.runtime.vram_headroom_mb, 2048);

    assert_eq!(c.llm.backend, LlmBackend::LocalLlamacpp);
    assert_eq!(c.llm.model, "gemma-4-e2b-it-qat-q4_0");
    assert_eq!(c.llm.local_llamacpp.ctx_size, 16384);
    assert_eq!(c.llm.local_llamacpp.parallel_slots, 2);
    assert_eq!(c.llm.local_llamacpp.gpu_layers, GpuLayers::Auto);
    assert_eq!(c.llm.local_llamacpp.startup_timeout_secs, 120);
    assert_eq!(
        c.llm.openai_compatible.base_url,
        "http://127.0.0.1:11434/v1"
    );
    assert_eq!(c.llm.openai_compatible.api_key_ref, "");
    assert_eq!(c.llm.anthropic.base_url, "https://api.anthropic.com");
    assert_eq!(c.llm.anthropic.api_key_ref, "keychain:photoproof/anthropic");
    assert_eq!(c.llm.anthropic.model, "claude-sonnet-latest");

    assert_eq!(c.asr.backend, AsrBackend::LocalSherpa);
    assert_eq!(c.asr.model, "nemotron-3.5-asr-streaming-0.6b-parakeet");
    assert_eq!(c.asr.chunk_ms, 560);
    assert_eq!(c.asr.device, AsrDevice::Cpu);

    assert_eq!(c.embedder.backend, EmbedderBackend::LocalOrt);
    assert_eq!(c.embedder.model, "ViT-H-14-378-quickgelu__dfn5b");
    assert_eq!(c.embedder.device, EmbedDevice::Auto);
    assert_eq!(c.embedder.text.backend, TextEmbedderBackend::LocalOrt);
    assert_eq!(c.embedder.text.model, "embeddinggemma-300m-q8");
    assert_eq!(c.embedder.text.device, EmbedDevice::Cpu);
}

#[test]
fn spec_block_equals_defaults() {
    // §4.4 shows defaults: parsing it must equal an empty config. The immutable
    // hosted int8 CLIP is the fail-closed default even in CUDA builds until the
    // staged fp16 export has immutable hosted pins.
    let from_spec = from_toml_str(SPEC_TOML).unwrap().config;
    let from_empty = from_toml_str("").unwrap().config;
    assert_eq!(from_empty, Config::default());
    assert_eq!(from_empty.embedder.model, "ViT-H-14-378-quickgelu__dfn5b");
    assert_eq!(from_spec, from_empty);
}

#[test]
fn missing_sections_take_defaults() {
    // Only override one value; everything else stays at defaults.
    let loaded = from_toml_str("[asr]\nbackend = \"disabled\"\n").unwrap();
    assert_eq!(loaded.config.asr.backend, AsrBackend::Disabled);
    assert_eq!(loaded.config.asr.chunk_ms, 560);
    assert_eq!(loaded.config.llm, Config::default().llm);
    assert_eq!(loaded.config.runtime, Config::default().runtime);
}

#[test]
fn unknown_keys_warn_but_parse() {
    let toml = "[runtime]\nbogus = 1\n\n[mystery]\nx = true\n";
    let loaded = from_toml_str(toml).expect("unknown keys must not fail the parse");
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.unknown_keys.contains(&"runtime.bogus".to_string()));
    assert!(loaded.unknown_keys.iter().any(|k| k.starts_with("mystery")));
}

#[test]
fn tier_accepts_integers_and_auto() {
    for (input, expected) in [
        ("tier = \"auto\"", Tier::Auto),
        ("tier = 0", Tier::Fixed(0)),
        ("tier = 1", Tier::Fixed(1)),
        ("tier = 2", Tier::Fixed(2)),
    ] {
        let loaded = from_toml_str(&format!("[runtime]\n{input}\n")).unwrap();
        assert_eq!(loaded.config.runtime.tier, expected, "{input}");
    }
}

#[test]
fn tier_rejects_out_of_range_and_junk() {
    for input in ["tier = 3", "tier = -1", "tier = \"turbo\"", "tier = true"] {
        let result = from_toml_str(&format!("[runtime]\n{input}\n"));
        assert!(matches!(result, Err(ConfigError::Parse(_))), "{input}");
    }
}

#[test]
fn gpu_layers_accepts_auto_and_integers() {
    let toml = "[llm.local-llamacpp]\ngpu_layers = 28\n";
    let loaded = from_toml_str(toml).unwrap();
    assert_eq!(
        loaded.config.llm.local_llamacpp.gpu_layers,
        GpuLayers::Layers(28)
    );

    let toml = "[llm.local-llamacpp]\ngpu_layers = 0\n";
    let loaded = from_toml_str(toml).unwrap();
    assert_eq!(
        loaded.config.llm.local_llamacpp.gpu_layers,
        GpuLayers::Layers(0)
    );
}

#[test]
fn gpu_layers_rejects_negative_and_junk() {
    for input in ["gpu_layers = -4", "gpu_layers = \"all\""] {
        let result = from_toml_str(&format!("[llm.local-llamacpp]\n{input}\n"));
        assert!(matches!(result, Err(ConfigError::Parse(_))), "{input}");
    }
}

#[test]
fn literal_api_key_is_a_hard_error() {
    // RUNTIME §4.4: "A literal key found in config is rejected at parse
    // time with a hard error."
    let toml = "[llm.anthropic]\napi_key_ref = \"sk-ant-li7eral-key-material\"\n";
    let result = from_toml_str(toml);
    assert!(matches!(
        result,
        Err(ConfigError::LiteralApiKey {
            field: "llm.anthropic.api_key_ref"
        })
    ));

    let toml = "[llm.openai-compatible]\napi_key_ref = \"sk-proj-abc123\"\n";
    let result = from_toml_str(toml);
    assert!(matches!(
        result,
        Err(ConfigError::LiteralApiKey {
            field: "llm.openai-compatible.api_key_ref"
        })
    ));
}

#[test]
fn malformed_keychain_refs_are_rejected() {
    for bad in [
        "keychain:",
        "keychain:noslash",
        "keychain:/account",
        "keychain:service/",
    ] {
        let toml = format!("[llm.openai-compatible]\napi_key_ref = \"{bad}\"\n");
        assert!(
            matches!(from_toml_str(&toml), Err(ConfigError::LiteralApiKey { .. })),
            "{bad}"
        );
    }
}

#[test]
fn wellformed_keychain_ref_accepted() {
    let toml = "[llm.openai-compatible]\napi_key_ref = \"keychain:photoproof/openai-compat\"\n";
    let loaded = from_toml_str(toml).unwrap();
    assert_eq!(
        loaded.config.llm.openai_compatible.api_key_ref,
        "keychain:photoproof/openai-compat"
    );
}

#[test]
fn chunk_ms_must_be_model_supported() {
    for ok in [80u32, 160, 560, 1120] {
        let loaded = from_toml_str(&format!("[asr]\nchunk_ms = {ok}\n")).unwrap();
        assert_eq!(loaded.config.asr.chunk_ms, ok);
    }
    let result = from_toml_str("[asr]\nchunk_ms = 100\n");
    assert!(matches!(
        result,
        Err(ConfigError::InvalidValue {
            field: "asr.chunk_ms",
            ..
        })
    ));
}

#[test]
fn unknown_backend_values_are_rejected() {
    for toml in [
        "[llm]\nbackend = \"python-server\"\n",
        "[asr]\nbackend = \"whisper\"\n",
        "[embedder]\nbackend = \"local-llamacpp\"\n", // only valid for embedder.text
        "[embedder.text]\nbackend = \"docker\"\n",
        "[asr]\ndevice = \"npu\"\n",
    ] {
        assert!(
            matches!(from_toml_str(toml), Err(ConfigError::Parse(_))),
            "{toml}"
        );
    }
}

#[test]
fn embedder_text_accepts_local_llamacpp() {
    let toml = "[embedder.text]\nbackend = \"local-llamacpp\"\n";
    let loaded = from_toml_str(toml).unwrap();
    assert_eq!(
        loaded.config.embedder.text.backend,
        TextEmbedderBackend::LocalLlamacpp
    );
}

#[test]
fn wrong_types_are_rejected() {
    for toml in [
        "[runtime]\nvram_headroom_mb = \"lots\"\n",
        "[llm.local-llamacpp]\nctx_size = \"big\"\n",
        "not even toml ===",
    ] {
        assert!(
            matches!(from_toml_str(toml), Err(ConfigError::Parse(_))),
            "{toml}"
        );
    }
}
