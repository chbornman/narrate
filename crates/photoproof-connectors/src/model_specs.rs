//! Pinned model-id -> on-disk-layout resolution, and the one place an
//! [`OrtEmbedder`] is constructed from a models directory.
//!
//! WHY this lives in the connectors crate (not the desktop app): the desktop
//! `EmbedderHost` and the retrieval-eval rig (`pp-retrieval-eval` / `pp-sweep`)
//! must build the SAME embedder stack from the SAME models dir, or the offline
//! sweep would tune against a different search than the one that ships. Both
//! callers need the spec resolution + the `OrtEmbedder::{text,clip}` call, so
//! it is lifted here behind one seam. The desktop host keeps its lifecycle
//! (converge loop, slots, background build); it just delegates the actual
//! "id + dir -> OrtEmbedder" step to these functions instead of duplicating the
//! recipe table.
//!
//! The three embedders are PINNED (the desktop manifest), so a match on id is
//! the simple, obvious resolution — no discovery, no heuristics. The on-disk
//! layout (`models_dir/<id>/...`) is exactly the path-preserving download
//! layout the desktop keeps, so the DFN5B visual tower's external-data files
//! load relative to `visual/model.onnx`.

use std::path::Path;

use crate::error::ConnectorResult;
use crate::ort_embedder::{OrtEmbedder, TextRecipe};

/// Which text recipe + which on-disk onnx/tokenizer a pinned text model id
/// resolves to. dims mirror the spike (docs/SPIKE-P7-EMBED.md): EmbeddingGemma
/// 768, Qwen3 1024.
struct TextSpec {
    recipe: TextRecipe,
    /// Relative to `models_dir/<id>/` — the manifest's pinned file paths.
    onnx: &'static str,
    tokenizer: &'static str,
    dims: usize,
}

/// Resolve a pinned TEXT embedder id to its recipe/paths/dims. `None` for an
/// id with no pinned spec (a config/manifest mismatch the caller reports).
fn text_spec(model_id: &str) -> Option<TextSpec> {
    match model_id {
        "embeddinggemma-300m-q8" => Some(TextSpec {
            recipe: TextRecipe::MeanPooled,
            onnx: "onnx/model_quantized.onnx",
            tokenizer: "tokenizer.json",
            dims: 768,
        }),
        "qwen3-embedding-0.6b-int8" => Some(TextSpec {
            recipe: TextRecipe::LastToken,
            onnx: "onnx/model_int8.onnx",
            tokenizer: "tokenizer.json",
            dims: 1024,
        }),
        _ => None,
    }
}

/// The DFN5B CLIP pair's on-disk layout. The visual tower's external-data
/// files load RELATIVE to `visual/model.onnx`, so the subdirectory layout the
/// path-preserving downloads keep is load-bearing here.
struct ClipSpec {
    visual: &'static str,
    textual: &'static str,
    tokenizer: &'static str,
    dims: usize,
}

/// Resolve a pinned CLIP embedder id to its tower paths/dims. `None` for an
/// unknown id.
fn clip_spec(model_id: &str) -> Option<ClipSpec> {
    match model_id {
        "ViT-H-14-378-quickgelu__dfn5b" => Some(ClipSpec {
            visual: "visual/model.onnx",
            textual: "textual/model.onnx",
            tokenizer: "textual/tokenizer.json",
            dims: 1024,
        }),
        _ => None,
    }
}

/// True iff `model_id` is a pinned TEXT embedder id (cheap, no IO). Lets a
/// caller decide whether to attempt a build before paying the load.
pub fn is_known_text_model(model_id: &str) -> bool {
    text_spec(model_id).is_some()
}

/// True iff `model_id` is a pinned CLIP embedder id (cheap, no IO).
pub fn is_known_clip_model(model_id: &str) -> bool {
    clip_spec(model_id).is_some()
}

/// Build the TEXT embedder for `model_id` from `models_dir/<id>/`.
///
/// `Err` carries a caller-friendly message for an unknown id (no pinned
/// recipe) OR a native load failure (missing/corrupt weights, an ort shape
/// error) — the caller decides whether that is "degrade to keyword-only"
/// (the eval rig) or "park the slot Failed" (the desktop host). The load is
/// SECONDS; callers off the hot path only.
pub fn build_text_embedder(model_id: &str, models_dir: &Path) -> ConnectorResult<OrtEmbedder> {
    let spec = text_spec(model_id).ok_or(crate::error::ConnectorError::NotReady(
        "no ort text recipe for this model id",
    ))?;
    let dir = models_dir.join(model_id);
    OrtEmbedder::text(
        model_id.to_owned(),
        spec.recipe,
        &dir.join(spec.onnx),
        &dir.join(spec.tokenizer),
        spec.dims,
    )
}

/// Build the CLIP embedder pair (visual + textual towers) for `model_id` from
/// `models_dir/<id>/`. `Err` semantics mirror [`build_text_embedder`].
pub fn build_clip_embedder(model_id: &str, models_dir: &Path) -> ConnectorResult<OrtEmbedder> {
    let spec = clip_spec(model_id).ok_or(crate::error::ConnectorError::NotReady(
        "no ort clip recipe for this model id",
    ))?;
    let dir = models_dir.join(model_id);
    OrtEmbedder::clip(
        model_id.to_owned(),
        &dir.join(spec.visual),
        &dir.join(spec.textual),
        &dir.join(spec.tokenizer),
        spec.dims,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolution truth: the pinned ids map to the right recipe + dims; an id
    /// with no spec resolves to None. (Mirrors the desktop host's old
    /// `pinned_ids_resolve_to_recipes`, now anchored where the table lives.)
    #[test]
    fn pinned_ids_resolve_to_recipes() {
        assert_eq!(text_spec("embeddinggemma-300m-q8").unwrap().dims, 768);
        assert_eq!(
            text_spec("embeddinggemma-300m-q8").unwrap().recipe,
            TextRecipe::MeanPooled
        );
        assert_eq!(text_spec("qwen3-embedding-0.6b-int8").unwrap().dims, 1024);
        assert_eq!(
            text_spec("qwen3-embedding-0.6b-int8").unwrap().recipe,
            TextRecipe::LastToken
        );
        assert_eq!(
            clip_spec("ViT-H-14-378-quickgelu__dfn5b").unwrap().dims,
            1024
        );
        assert!(text_spec("nope").is_none());
        assert!(clip_spec("nope").is_none());
    }

    #[test]
    fn known_model_predicates_match_specs() {
        assert!(is_known_text_model("embeddinggemma-300m-q8"));
        assert!(is_known_clip_model("ViT-H-14-378-quickgelu__dfn5b"));
        assert!(!is_known_text_model("ViT-H-14-378-quickgelu__dfn5b"));
        assert!(!is_known_clip_model("embeddinggemma-300m-q8"));
        assert!(!is_known_text_model("nope"));
    }

    /// An unknown id fails BEFORE any IO (no recipe), with a message — never a
    /// panic. The real-weights build path is exercised by the desktop host's
    /// ignored snapshot test and the eval bin's live run.
    #[test]
    fn unknown_ids_error_without_io() {
        let dir = std::path::Path::new("/nonexistent/models");
        assert!(build_text_embedder("not-a-text", dir).is_err());
        assert!(build_clip_embedder("not-a-clip", dir).is_err());
    }
}
