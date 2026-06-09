//! The in-memory mock VectorStore: upsert/search round-trip, replace
//! semantics, logical delete, physical scrub (RETRIEVAL §1.3), compaction,
//! and integrity errors (dims/model mismatch, missing keys).

use photoproof_connectors::mock::{MockEmbedder, MockVectorStore};
use photoproof_connectors::{
    Embedder, Embedding, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError,
};
use pollster::block_on;

const MODEL: &str = "qwen3-embedding-0.6b-int8";

fn space() -> VecSpace {
    VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: MODEL.to_owned(),
    }
}

fn chunk_key(event_id: &str, chunk_index: u32) -> VecKey {
    VecKey {
        space: space(),
        unit: VecUnit::AnnotationChunk {
            event_id: event_id.to_owned(),
            chunk_index,
        },
    }
}

fn pinned(vector: Vec<f32>) -> Embedding {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    Embedding {
        vector: vector.iter().map(|x| x / norm).collect(),
        model_id: MODEL.to_owned(),
    }
}

#[test]
fn upsert_search_round_trip_via_mock_embedder() {
    // The full mock pipeline a retrieval test will use: deterministic mock
    // embeddings in, exact-match top hit out.
    let store = MockVectorStore::new();
    let embedder = MockEmbedder::text(MODEL, 32);

    let texts = [
        ("01HVAAAAAAAAAAAAAAAAAAAAAA", "fog swallowing the barn"),
        ("01HVBBBBBBBBBBBBBBBBBBBBBB", "quieter, melancholic series"),
        ("01HVCCCCCCCCCCCCCCCCCCCCCC", "harbor at night, too literal"),
    ];
    for (event_id, text) in texts {
        let e = block_on(embedder.embed_text(text)).unwrap();
        store.upsert(chunk_key(event_id, 0), &e).unwrap();
    }

    // Same text → same vector → cosine 1.0 top hit.
    let query = block_on(embedder.embed_text("quieter, melancholic series")).unwrap();
    let hits = store.search(&query, space(), 3).unwrap();
    assert_eq!(hits.len(), 3);
    let best = store
        .row(&chunk_key("01HVBBBBBBBBBBBBBBBBBBBBBB", 0))
        .unwrap();
    assert_eq!(hits[0].vector_id, best.vector_id);
    assert!((hits[0].score - 1.0).abs() < 1e-5);
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn search_orders_by_cosine_and_respects_k() {
    let store = MockVectorStore::new();
    store
        .upsert(chunk_key("e1", 0), &pinned(vec![1.0, 0.0, 0.0]))
        .unwrap();
    store
        .upsert(chunk_key("e2", 0), &pinned(vec![1.0, 1.0, 0.0]))
        .unwrap();
    store
        .upsert(chunk_key("e3", 0), &pinned(vec![0.0, 1.0, 0.0]))
        .unwrap();

    let query = pinned(vec![1.0, 0.0, 0.0]);
    let hits = store.search(&query, space(), 2).unwrap();
    assert_eq!(hits.len(), 2, "k must cap the hit list");
    let e1 = store.row(&chunk_key("e1", 0)).unwrap().vector_id;
    let e2 = store.row(&chunk_key("e2", 0)).unwrap().vector_id;
    assert_eq!(hits[0].vector_id, e1);
    assert_eq!(hits[1].vector_id, e2);
}

#[test]
fn upsert_replaces_in_place() {
    // A revision re-embeds the chunk (RETRIEVAL §1.1 maintenance table).
    let store = MockVectorStore::new();
    let key = chunk_key("e1", 0);
    store.upsert(key.clone(), &pinned(vec![1.0, 0.0])).unwrap();
    let id_before = store.row(&key).unwrap().vector_id;
    store.upsert(key.clone(), &pinned(vec![0.0, 1.0])).unwrap();

    assert_eq!(store.live_count(&space()), 1, "replace, not duplicate");
    let row = store.row(&key).unwrap();
    assert_eq!(row.vector_id, id_before);
    assert_eq!(row.vector, vec![0.0, 1.0]);
}

#[test]
fn mark_deleted_hides_from_search() {
    let store = MockVectorStore::new();
    store
        .upsert(chunk_key("e1", 0), &pinned(vec![1.0, 0.0]))
        .unwrap();
    store
        .upsert(chunk_key("e2", 0), &pinned(vec![0.0, 1.0]))
        .unwrap();
    store.mark_deleted(chunk_key("e1", 0)).unwrap();

    let hits = store.search(&pinned(vec![1.0, 0.0]), space(), 10).unwrap();
    let e2 = store.row(&chunk_key("e2", 0)).unwrap().vector_id;
    assert_eq!(
        hits.iter().map(|h| h.vector_id).collect::<Vec<_>>(),
        vec![e2]
    );
    assert_eq!(store.live_count(&space()), 1);
}

#[test]
fn scrub_zeroes_the_stored_bytes() {
    // RETRIEVAL §1.3: redaction is logical delete AND physical zero.
    let store = MockVectorStore::new();
    let key = chunk_key("e1", 0);
    store.upsert(key.clone(), &pinned(vec![0.6, 0.8])).unwrap();
    store.scrub(key.clone()).unwrap();

    let row = store.row(&key).unwrap();
    assert!(row.deleted);
    assert_eq!(row.vector, vec![0.0, 0.0], "scrub must zero the row bytes");
    assert!(
        store
            .search(&pinned(vec![0.6, 0.8]), space(), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn compact_drops_dead_rows_and_keeps_live_ids() {
    let store = MockVectorStore::new();
    store
        .upsert(chunk_key("e1", 0), &pinned(vec![1.0, 0.0]))
        .unwrap();
    store
        .upsert(chunk_key("e2", 0), &pinned(vec![0.0, 1.0]))
        .unwrap();
    let live_id = store.row(&chunk_key("e2", 0)).unwrap().vector_id;

    store.mark_deleted(chunk_key("e1", 0)).unwrap();
    store.compact(space()).unwrap();

    assert!(store.row(&chunk_key("e1", 0)).is_none(), "dead row dropped");
    assert_eq!(store.row(&chunk_key("e2", 0)).unwrap().vector_id, live_id);
    assert_eq!(store.live_count(&space()), 1);
}

#[test]
fn spaces_are_isolated_and_missing_space_searches_empty() {
    let store = MockVectorStore::new();
    store
        .upsert(chunk_key("e1", 0), &pinned(vec![1.0, 0.0]))
        .unwrap();

    let clip_space = VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: "ViT-H-14-378-quickgelu__dfn5b".to_owned(),
    };
    let clip_query = Embedding {
        vector: vec![1.0, 0.0],
        model_id: "ViT-H-14-378-quickgelu__dfn5b".to_owned(),
    };
    assert!(
        store
            .search(&clip_query, clip_space, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dimension_and_model_mismatches_are_errors() {
    let store = MockVectorStore::new();
    store
        .upsert(chunk_key("e1", 0), &pinned(vec![1.0, 0.0]))
        .unwrap();

    let wrong_dims = pinned(vec![1.0, 0.0, 0.0]);
    assert!(matches!(
        store.upsert(chunk_key("e2", 0), &wrong_dims),
        Err(VectorStoreError::DimensionMismatch {
            expected: 2,
            got: 3,
            ..
        })
    ));
    assert!(matches!(
        store.search(&wrong_dims, space(), 10),
        Err(VectorStoreError::DimensionMismatch { .. })
    ));

    let wrong_model = Embedding {
        vector: vec![1.0, 0.0],
        model_id: "some-other-model".to_owned(),
    };
    assert!(matches!(
        store.upsert(chunk_key("e3", 0), &wrong_model),
        Err(VectorStoreError::ModelMismatch { .. })
    ));
}

#[test]
fn missing_keys_are_not_found() {
    let store = MockVectorStore::new();
    assert!(matches!(
        store.mark_deleted(chunk_key("missing", 0)),
        Err(VectorStoreError::NotFound(_))
    ));
    assert!(matches!(
        store.scrub(chunk_key("missing", 0)),
        Err(VectorStoreError::NotFound(_))
    ));
}

#[test]
fn image_units_key_image_spaces() {
    let clip_space = VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: "mock-clip".to_owned(),
    };
    let key = VecKey {
        space: clip_space.clone(),
        unit: VecUnit::Image {
            image_hash: "ab12cd34".to_owned(),
        },
    };
    let store = MockVectorStore::new();
    let e = Embedding {
        vector: vec![1.0, 0.0],
        model_id: "mock-clip".to_owned(),
    };
    store.upsert(key.clone(), &e).unwrap();
    let hits = store.search(&e, clip_space, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(store.row(&key).unwrap().vector_id, hits[0].vector_id);
}
