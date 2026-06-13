//! "More like this" acceptance — `PpvecStore::similar_images` (B69
//! retrieval-stays-additive): nearest OTHER images by CLIP visual
//! similarity, in descending-similarity order, query image excluded.
//!
//! Reuses the §1.2 image-vector fixture shape from `retrieval_ppvec.rs`
//! (planted unit vectors at controlled angles) so ordering equality is
//! exact by construction, not by luck. Pins three properties the command
//! depends on: similarity ordering is the backend's order, the query image
//! never appears in its own results, and an un-embedded / empty space
//! yields an honest empty list rather than an error.

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit, VectorStore};
use photoproof_core::retrieval::PpvecStore;

const MODEL: &str = "mock-clip-embedder";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("photoproof.db")
    }

    fn vectors_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("vectors")
    }

    fn open(&self) -> PpvecStore {
        PpvecStore::open(self.db(), self.vectors_dir()).unwrap()
    }
}

fn clip_space() -> VecSpace {
    VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: MODEL.into(),
    }
}

fn image_key(image_hash: &str) -> VecKey {
    VecKey {
        space: clip_space(),
        unit: VecUnit::Image {
            image_hash: image_hash.into(),
        },
    }
}

fn emb(vector: Vec<f32>) -> Embedding {
    Embedding {
        vector,
        model_id: MODEL.into(),
    }
}

/// `cos(t) * e0 + sin(t) * e_axis` in `dims` dimensions — unit length by
/// construction, with a controlled similarity to `e0` (the query vector).
/// A larger `t` means a smaller cosine to `e0`, i.e. a more distant image.
fn planted(dims: usize, axis: usize, t: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; dims];
    v[0] = t.cos();
    v[axis] = t.sin();
    v
}

fn upsert_image(store: &PpvecStore, hash: &str, v: Vec<f32>) {
    store.upsert(image_key(hash), &emb(v)).unwrap();
}

/// The neighbors come back in descending-similarity order with the query
/// image itself excluded. The query is e0 (img-00); the others sit at
/// increasing angles, so the expected order is img-01, img-02, img-03 …
/// with the query absent. Angle gaps (>= 0.2 rad) make the score gaps far
/// exceed int8 quantization error, so the order is exact by construction.
#[test]
fn similar_orders_by_similarity_and_excludes_self() {
    let fx = Fixture::new();
    let store = fx.open();
    let dims = 64;
    // img-00 is e0 — the query image.
    upsert_image(&store, "img-00", planted(dims, 1, 0.0));
    // Increasing angle from e0 ⇒ decreasing similarity.
    upsert_image(&store, "img-01", planted(dims, 1, 0.3));
    upsert_image(&store, "img-02", planted(dims, 2, 0.6));
    upsert_image(&store, "img-03", planted(dims, 3, 0.9));
    upsert_image(&store, "img-04", planted(dims, 4, 1.2));

    let hits = store.similar_images("img-00", 10).unwrap();
    let hashes: Vec<&str> = hits.iter().map(|(h, _)| h.as_str()).collect();

    // Self is excluded; the rest are nearest-first.
    assert_eq!(hashes, vec!["img-01", "img-02", "img-03", "img-04"]);
    // Scores are descending (cosine to the query), all below the self 1.0.
    for w in hits.windows(2) {
        assert!(
            w[0].1 >= w[1].1,
            "similarity must be non-increasing: {:?}",
            hits
        );
    }
    assert!(
        hits.iter().all(|(h, _)| h != "img-00"),
        "the query image must never appear in its own results"
    );
}

/// `limit` bounds the neighbor count AFTER self-exclusion: with 5 stored
/// images and a query that is one of them, `limit = 2` returns exactly the
/// two nearest OTHER images (the over-fetch-by-one keeps self from eating a
/// slot).
#[test]
fn similar_respects_limit_after_self_exclusion() {
    let fx = Fixture::new();
    let store = fx.open();
    let dims = 64;
    upsert_image(&store, "q", planted(dims, 1, 0.0));
    upsert_image(&store, "n1", planted(dims, 1, 0.2));
    upsert_image(&store, "n2", planted(dims, 2, 0.4));
    upsert_image(&store, "n3", planted(dims, 3, 0.6));
    upsert_image(&store, "n4", planted(dims, 4, 0.8));

    let hits = store.similar_images("q", 2).unwrap();
    let hashes: Vec<&str> = hits.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(hashes, vec!["n1", "n2"]);
}

/// An empty index (no vectors at all) yields an empty list, never an error:
/// the "more like this" mechanism must be correct on a fresh/mock machine
/// before any embedding pass has run.
#[test]
fn similar_on_empty_index_is_empty_not_error() {
    let fx = Fixture::new();
    let store = fx.open();
    let hits = store.similar_images("anything", 10).unwrap();
    assert!(hits.is_empty());
}

/// A hash with no stored CLIP vector (the rest of the library IS embedded,
/// but this particular image was not yet) also yields an empty list, not an
/// error — there is no query vector to search from.
#[test]
fn similar_for_unembedded_image_is_empty() {
    let fx = Fixture::new();
    let store = fx.open();
    let dims = 64;
    upsert_image(&store, "embedded", planted(dims, 1, 0.0));
    // "missing" was never embedded.
    let hits = store.similar_images("missing", 10).unwrap();
    assert!(hits.is_empty());
}

/// The single-image case: an image that IS embedded but is the only one in
/// the space has no neighbors — self is excluded and nothing remains.
#[test]
fn similar_with_only_self_is_empty() {
    let fx = Fixture::new();
    let store = fx.open();
    let dims = 64;
    upsert_image(&store, "lonely", planted(dims, 1, 0.0));
    let hits = store.similar_images("lonely", 10).unwrap();
    assert!(hits.is_empty());
}
