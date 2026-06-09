//! Deterministic mock `Reranker`.
//!
//! Default ranking: case-insensitive query-token overlap with each
//! candidate's text, descending; ties keep the incoming (fused) order —
//! deterministic and intuitively "relevance-shaped" for tests. A fixed
//! permutation can be scripted instead.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::error::{ConnectorError, ConnectorResult};
use crate::reranker::{CandidateText, Reranker};

enum Mode {
    TokenOverlap,
    Fixed(Vec<usize>),
}

pub struct MockReranker {
    model_id: String,
    mode: Mode,
    seen_queries: Mutex<Vec<String>>,
}

impl MockReranker {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            mode: Mode::TokenOverlap,
            seen_queries: Mutex::new(Vec::new()),
        }
    }

    /// Always return exactly this order (must match the candidate count at
    /// call time, else the call errors — catches test wiring bugs).
    #[must_use]
    pub fn with_fixed_order(mut self, order: Vec<usize>) -> Self {
        self.mode = Mode::Fixed(order);
        self
    }

    /// Queries received so far, in call order.
    pub fn queries(&self) -> Vec<String> {
        self.seen_queries.lock().unwrap().clone()
    }
}

impl Reranker for MockReranker {
    fn rerank(&self, query: &str, candidates: &[CandidateText]) -> ConnectorResult<Vec<usize>> {
        self.seen_queries.lock().unwrap().push(query.to_owned());
        match &self.mode {
            Mode::Fixed(order) => {
                if order.len() != candidates.len() {
                    return Err(ConnectorError::Decode(format!(
                        "mock reranker: fixed order has {} entries for {} candidates",
                        order.len(),
                        candidates.len()
                    )));
                }
                Ok(order.clone())
            }
            Mode::TokenOverlap => {
                let query_tokens: HashSet<String> = tokens(query).collect();
                let mut order: Vec<usize> = (0..candidates.len()).collect();
                let scores: Vec<usize> = candidates
                    .iter()
                    .map(|c| {
                        tokens(&c.text)
                            .filter(|t| query_tokens.contains(t))
                            .collect::<HashSet<_>>()
                            .len()
                    })
                    .collect();
                // Stable sort: ties keep the incoming fused order.
                order.sort_by(|&a, &b| scores[b].cmp(&scores[a]));
                Ok(order)
            }
        }
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|t| !t.is_empty())
}
