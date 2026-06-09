//! The mock Reranker (RETRIEVAL §5.3 stage 3b): deterministic token-overlap
//! ordering with stable ties, scripted fixed orders, recorded queries.

use photoproof_connectors::mock::MockReranker;
use photoproof_connectors::{CandidateText, ConnectorError, Reranker};

fn candidates(texts: &[&str]) -> Vec<CandidateText> {
    texts
        .iter()
        .map(|t| CandidateText {
            text: (*t).to_owned(),
        })
        .collect()
}

#[test]
fn token_overlap_orders_by_relevance_with_stable_ties() {
    let reranker = MockReranker::new("mock-reranker");
    let cands = candidates(&[
        "harbor at night",                        // 0 overlapping tokens
        "the fog swallowing the barn, keep this", // 2 ("fog", "barn")
        "fog bank ate the ridge",                 // 1 ("fog")
        "morning light",                          // 0 — ties with index 0
    ]);
    let order = reranker.rerank("fog barn", &cands).unwrap();
    assert_eq!(
        order,
        vec![1, 2, 0, 3],
        "ties keep the incoming fused order"
    );

    // Determinism.
    assert_eq!(order, reranker.rerank("fog barn", &cands).unwrap());
    assert_eq!(reranker.queries(), vec!["fog barn", "fog barn"]);
    assert_eq!(reranker.model_id(), "mock-reranker");
}

#[test]
fn fixed_order_is_returned_verbatim() {
    let reranker = MockReranker::new("mock-reranker").with_fixed_order(vec![2, 0, 1]);
    let order = reranker
        .rerank("anything", &candidates(&["a", "b", "c"]))
        .unwrap();
    assert_eq!(order, vec![2, 0, 1]);
}

#[test]
fn fixed_order_length_mismatch_is_an_error() {
    let reranker = MockReranker::new("mock-reranker").with_fixed_order(vec![0, 1]);
    let result = reranker.rerank("anything", &candidates(&["a", "b", "c"]));
    assert!(matches!(result, Err(ConnectorError::Decode(_))));
}

#[test]
fn empty_candidates_yield_empty_order() {
    let reranker = MockReranker::new("mock-reranker");
    assert_eq!(reranker.rerank("query", &[]).unwrap(), Vec::<usize>::new());
}
