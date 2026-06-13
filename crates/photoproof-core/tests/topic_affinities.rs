//! `topic_affinities` acceptance (DESIGN-SEMANTIC-GRAPH.md, v1): the looks/said
//! blend over REAL stored vectors, scored through the PPVEC cosine kernel.
//!
//! The test plants `image_clip` and `image_summary` vectors at controlled
//! angles and uses fixed-vector test embedders so the topic's reference vector
//! is known exactly — the blend `α·visual + (1−α)·annotation` is then checked
//! by construction, not by luck, at α = 1 / 0 / 0.5. It also pins the
//! graceful-degradation contract: a half whose space is empty contributes 0.

use photoproof_connectors::embedder::{DecodedImage, Embedder, Embedding};
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit, VectorStore};
use photoproof_connectors::{ConnectorError, ConnectorResult};
use photoproof_core::retrieval::PpvecStore;
use photoproof_core::topic::topic_affinities;

const CLIP_MODEL: &str = "test-clip";
const TEXT_MODEL: &str = "test-text";
const DIMS: usize = 8;

/// An embedder that always returns one fixed vector — so a topic's reference
/// vector in each space is known exactly and the blend is provable.
struct FixedEmbedder {
    model_id: String,
    vector: Vec<f32>,
}

impl Embedder for FixedEmbedder {
    async fn embed_text(&self, _text: &str) -> ConnectorResult<Embedding> {
        Ok(Embedding {
            vector: self.vector.clone(),
            model_id: self.model_id.clone(),
        })
    }
    async fn embed_image(&self, _img: &DecodedImage) -> ConnectorResult<Embedding> {
        Err(ConnectorError::NotReady("test embedder has no image tower"))
    }
    fn dimensions(&self) -> usize {
        self.vector.len()
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// `cos(t)·e0 + sin(t)·e_axis` — unit length, controlled cosine to `e0`.
fn planted(axis: usize, t: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; DIMS];
    v[0] = t.cos();
    v[axis] = t.sin();
    v
}

fn upsert(store: &PpvecStore, kind: VecKind, model: &str, hash: &str, v: Vec<f32>) {
    let key = VecKey {
        space: VecSpace {
            vec_kind: kind,
            model_id: model.to_owned(),
        },
        unit: VecUnit::Image {
            image_hash: hash.to_owned(),
        },
    };
    store
        .upsert(
            key,
            &Embedding {
                vector: v,
                model_id: model.to_owned(),
            },
        )
        .unwrap();
}

fn open_store() -> (tempfile::TempDir, PpvecStore) {
    let dir = tempfile::tempdir().unwrap();
    let store =
        PpvecStore::open(dir.path().join("photoproof.db"), dir.path().join("vectors")).unwrap();
    (dir, store)
}

/// The topic embeds to e0 in BOTH spaces. img-A is visually e0 (cos 1) but
/// annotation-orthogonal (cos 0); img-B is the reverse. At α = 1 A wins, at
/// α = 0 B wins, at α = 0.5 they tie — the blend is exactly the slider.
#[test]
fn blend_follows_alpha_over_real_vectors() {
    let (_dir, store) = open_store();
    let a = "aa".repeat(32);
    let b = "bb".repeat(32);

    // img-A: clip aligned to e0 (cos 1), summary orthogonal (cos 0).
    upsert(&store, VecKind::ImageClip, CLIP_MODEL, &a, planted(1, 0.0));
    upsert(
        &store,
        VecKind::ImageSummary,
        TEXT_MODEL,
        &a,
        planted(1, std::f32::consts::FRAC_PI_2),
    );
    // img-B: the reverse — clip orthogonal, summary aligned.
    upsert(
        &store,
        VecKind::ImageClip,
        CLIP_MODEL,
        &b,
        planted(1, std::f32::consts::FRAC_PI_2),
    );
    upsert(
        &store,
        VecKind::ImageSummary,
        TEXT_MODEL,
        &b,
        planted(1, 0.0),
    );

    // Both topic embeddings are e0.
    let mut e0 = vec![0.0f32; DIMS];
    e0[0] = 1.0;
    let clip = FixedEmbedder {
        model_id: CLIP_MODEL.into(),
        vector: e0.clone(),
    };
    let text = FixedEmbedder {
        model_id: TEXT_MODEL.into(),
        vector: e0,
    };
    let scope = vec![a.clone(), b.clone()];
    let topics = vec!["e-zero".to_owned()];

    let aff = |alpha: f64| {
        let r = topic_affinities(&scope, &topics, alpha, &store, Some(&text), Some(&clip));
        let get = |hash: &str| {
            r.images
                .iter()
                .find(|i| i.image_hash == hash)
                .unwrap()
                .scores[0]
                .affinity
        };
        (get(&a), get(&b), r)
    };

    // α = 1 (pure looks): A (clip-aligned) >> B.
    let (a1, b1, r1) = aff(1.0);
    assert!(a1 > 0.9 && b1 < 0.1, "α=1 visual: A={a1} B={b1}");
    assert!(r1.visual_ready && r1.annotation_ready);
    // α = 0 (pure said): B (summary-aligned) >> A.
    let (a0, b0, _) = aff(0.0);
    assert!(b0 > 0.9 && a0 < 0.1, "α=0 annotation: A={a0} B={b0}");
    // α = 0.5: the two images tie (each ~0.5).
    let (ah, bh, _) = aff(0.5);
    assert!((ah - bh).abs() < 1e-5, "α=0.5 tie: A={ah} B={bh}");
    assert!((ah - 0.5).abs() < 1e-3, "α=0.5 midpoint ~0.5: {ah}");
}

/// One half un-embedded: with NO clip space the visual term is 0, so at α = 0.5
/// only half the annotation cosine lands — the lens stays correct (and never
/// errors) before a full embed pass.
#[test]
fn missing_visual_space_halves_the_blend_gracefully() {
    let (_dir, store) = open_store();
    let a = "cc".repeat(32);
    // Only a summary vector exists, aligned to e0.
    upsert(
        &store,
        VecKind::ImageSummary,
        TEXT_MODEL,
        &a,
        planted(1, 0.0),
    );

    let mut e0 = vec![0.0f32; DIMS];
    e0[0] = 1.0;
    let clip = FixedEmbedder {
        model_id: CLIP_MODEL.into(),
        vector: e0.clone(),
    };
    let text = FixedEmbedder {
        model_id: TEXT_MODEL.into(),
        vector: e0,
    };
    let scope = vec![a.clone()];
    let topics = vec!["e-zero".to_owned()];
    let r = topic_affinities(&scope, &topics, 0.5, &store, Some(&text), Some(&clip));
    let aff = r.images[0].scores[0].affinity;
    // Annotation cos = 1, visual cos = 0 (no clip row) → 0.5·0 + 0.5·1 = 0.5.
    assert!((aff - 0.5).abs() < 1e-3, "half-blend: {aff}");
    assert!(!r.visual_ready, "no clip rows ⇒ visual half not ready");
    assert!(r.annotation_ready);
}
