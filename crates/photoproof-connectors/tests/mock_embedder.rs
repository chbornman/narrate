//! The deterministic mock Embedder: same input → same L2-normalized
//! vector, pinned overrides for controlled-similarity tests, and the X3
//! text/CLIP split (text instance rejects embed_image).

use photoproof_connectors::mock::MockEmbedder;
use photoproof_connectors::{ConnectorError, DecodedImage, Embedder};
use pollster::block_on;

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn test_image(seed: u8) -> DecodedImage {
    DecodedImage {
        rgb8: (0..2 * 2 * 3)
            .map(|i| (i as u8).wrapping_add(seed))
            .collect(),
        width: 2,
        height: 2,
    }
}

#[test]
fn same_text_same_vector_different_text_different_vector() {
    let embedder = MockEmbedder::text("qwen3-embedding-0.6b-int8", 32);
    let a1 = block_on(embedder.embed_text("fog swallowing the barn")).unwrap();
    let a2 = block_on(embedder.embed_text("fog swallowing the barn")).unwrap();
    let b = block_on(embedder.embed_text("quieter, melancholic series")).unwrap();

    assert_eq!(a1, a2, "same input must yield the identical vector");
    assert_ne!(a1.vector, b.vector);
    assert_eq!(a1.vector.len(), 32);
    assert_eq!(a1.model_id, "qwen3-embedding-0.6b-int8");
}

#[test]
fn vectors_are_l2_normalized() {
    let embedder = MockEmbedder::text("mock-text", 64);
    let e = block_on(embedder.embed_text("some annotation chunk")).unwrap();
    assert!(
        (norm(&e.vector) - 1.0).abs() < 1e-5,
        "norm = {}",
        norm(&e.vector)
    );
}

#[test]
fn dimensions_and_model_id_reported() {
    let embedder = MockEmbedder::clip("ViT-H-14-378-quickgelu__dfn5b", 1024);
    assert_eq!(embedder.dimensions(), 1024);
    assert_eq!(embedder.model_id(), "ViT-H-14-378-quickgelu__dfn5b");
    let e = block_on(embedder.embed_text("short query")).unwrap();
    assert_eq!(e.vector.len(), 1024);
}

#[test]
fn distinct_model_instances_embed_differently() {
    // Two embedder instances behind the same trait (RUNTIME §3.3) must not
    // collide in vector space even on identical input text.
    let text = MockEmbedder::text("mock-text", 16);
    let clip = MockEmbedder::clip("mock-clip", 16);
    let a = block_on(text.embed_text("harbor nights")).unwrap();
    let b = block_on(clip.embed_text("harbor nights")).unwrap();
    assert_ne!(a.vector, b.vector);
}

#[test]
fn image_embedding_is_deterministic() {
    let embedder = MockEmbedder::clip("mock-clip", 32);
    let a1 = block_on(embedder.embed_image(&test_image(0))).unwrap();
    let a2 = block_on(embedder.embed_image(&test_image(0))).unwrap();
    let b = block_on(embedder.embed_image(&test_image(7))).unwrap();
    assert_eq!(a1, a2);
    assert_ne!(a1.vector, b.vector);
    assert!((norm(&a1.vector) - 1.0).abs() < 1e-5);
}

#[test]
fn text_instance_rejects_embed_image() {
    // RUNTIME §4 / DECISIONS X3: the TEXT embedder has no image tower.
    let embedder = MockEmbedder::text("mock-text", 32);
    let result = block_on(embedder.embed_image(&test_image(0)));
    assert!(matches!(
        result,
        Err(ConnectorError::Backend { status: 501, .. })
    ));
}

#[test]
fn pinned_vectors_override_and_are_normalized() {
    let embedder = MockEmbedder::text("mock-text", 4);
    embedder.set_text_embedding("the query", vec![2.0, 0.0, 0.0, 0.0]);
    let e = block_on(embedder.embed_text("the query")).unwrap();
    assert_eq!(e.vector, vec![1.0, 0.0, 0.0, 0.0]);

    // Unpinned inputs still take the deterministic default.
    let other = block_on(embedder.embed_text("something else")).unwrap();
    assert_ne!(other.vector, e.vector);

    let clip = MockEmbedder::clip("mock-clip", 4);
    clip.set_image_embedding(&test_image(0), vec![0.0, 3.0, 0.0, 0.0]);
    let img = block_on(clip.embed_image(&test_image(0))).unwrap();
    assert_eq!(img.vector, vec![0.0, 1.0, 0.0, 0.0]);
}
