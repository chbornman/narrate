//! Sample golden-query retrieval eval — the CI-gated end-to-end proof that
//! the whole instrument works WITHOUT the founder's real library or models.
//!
//! This is the in-process analogue of the `pp-retrieval-eval` runner: it
//! builds a tiny synthetic corpus with the same `M1Env`/`d_remark` machinery
//! `retrieval_hybrid.rs` uses, defines a handful of sample golden queries
//! with known-relevant images, runs each through the REAL hybrid rig
//! (keyword-only — no models, the shipping M1 path), scores the ranked result
//! with the pure `retrieval_eval` metrics, and asserts the numbers are sane.
//! A query whose relevant image is the obvious top hit must score MRR = 1.0;
//! the gate proves the parse -> search -> rank -> score pipe end to end.
//!
//! WHY keyword-only here (no embedders/LLM): the metrics module and the
//! query-set format are what the gate ships; exercising them needs a REAL
//! ranked list, not a mocked one, but it does NOT need live models. The
//! keyword-only rig produces a genuine fused ranking over a genuine SQLite
//! corpus, which is all the scorer consumes. The weight-SWEEP behaviour
//! (FusionWeights deltas changing the order) only bites once vector signals
//! are present; that is the founder-machine run the runner bin owns, and is
//! deliberately NOT a CI test (it needs the populated DB + real models).
//!
//! Identifiers: the synthetic corpus's stable key is the image content hash
//! (`ContentHash::as_str()`), exactly as a real library's would be — so the
//! relevant-id sets here are built from the same `hashes` the corpus minted,
//! and a relevant id matches a result id by plain string equality. That is
//! the SAME contract a real `test-corpora/retrieval/*.json` query set uses.

mod common;

use std::collections::HashSet;

use photoproof_core::retrieval_eval::{EvalReport, GoldenQuery, NamedQueryMetrics, QueryMetrics};
use photoproof_core::search::{HybridOptions, keyword_only_rig};
use photoproof_core::{ContentHash, EventDraft, UtcMillis};

use common::d_remark;
use common::m1env::M1Env;

/// A self-contained sample corpus: three images, each annotated with a
/// distinct remark, plus the golden queries that should retrieve them.
struct SampleEval {
    env: M1Env,
    hashes: Vec<ContentHash>,
}

impl SampleEval {
    /// Build the corpus exactly as `retrieval_hybrid.rs::Hx::new` does: scan
    /// three unique synthetic JPEGs into a watched root, drain ingest, take
    /// the hash-sorted image hashes as the stable id space.
    fn new() -> Self {
        let env = M1Env::new();
        let root = env.register("photos");
        let dir = env.mount.join("photos");
        for seed in 0..3u32 {
            std::fs::write(dir.join(format!("img{seed}.jpg")), unique_jpeg(seed)).unwrap();
        }
        env.scan(&root);
        env.drain();
        let mut hashes = env.lib.image_hashes().unwrap();
        hashes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(hashes.len(), 3, "fixture corpus is three images");
        Self { env, hashes }
    }

    /// Append a remark at a fixed wall time (monotonic ULIDs regardless).
    fn append_at(&self, draft: EventDraft, at: &str) {
        let minted = self.env.store.mint_at(UtcMillis::parse(at).unwrap());
        self.env
            .store
            .append(&self.env.session, draft, Some(minted))
            .unwrap();
    }

    /// Run one golden query through the keyword-only hybrid rig and return
    /// the ranked image-hash list — the exact input the scorer consumes. This
    /// is the in-process stand-in for what the runner bin does against a real
    /// DB (same `hybrid_search` entry point, same result contract).
    fn ranked_hashes(&self, query: &str) -> Vec<String> {
        let out = self
            .env
            .searcher
            .hybrid_search(query, &[], &keyword_only_rig(), &HybridOptions::default())
            .unwrap();
        out.images
            .iter()
            .map(|r| r.image_hash.as_str().to_owned())
            .collect()
    }
}

/// A small decodable JPEG whose bytes are unique per seed (copied from the
/// retrieval_hybrid harness so the corpus shape is identical).
fn unique_jpeg(seed: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(16, 16);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = seed
            .wrapping_mul(2_654_435_761)
            .wrapping_add(x * 31 + y * 7);
        *p = image::Rgb([
            (v & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
        ]);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut std::io::Cursor::new(&mut out), 95)
        .encode_image(&image::DynamicImage::ImageRgb8(img))
        .unwrap();
    out
}

fn rel(ids: &[&ContentHash]) -> HashSet<String> {
    ids.iter().map(|h| h.as_str().to_owned()).collect()
}

/// End-to-end: build the corpus, define golden queries, run + score, assert
/// the metrics are sane. This is the CI gate the founder's real query set
/// will later replace input-for-input.
#[test]
fn sample_golden_queries_score_sanely_end_to_end() {
    let sample = SampleEval::new();
    let (a, b, c) = (
        sample.hashes[0].clone(),
        sample.hashes[1].clone(),
        sample.hashes[2].clone(),
    );

    // Each image gets a remark with a distinctive, non-overlapping keyword so
    // the keyword (S2 bm25) path retrieves the right one as the obvious top
    // hit. These remarks are the synthetic stand-in for a real journal.
    sample.append_at(
        d_remark("the lighthouse at the harbor in heavy fog", vec![a.clone()]),
        "2026-02-01T10:00:00.000Z",
    );
    sample.append_at(
        d_remark(
            "a sunlit meadow full of wildflowers in spring",
            vec![b.clone()],
        ),
        "2026-02-02T10:00:00.000Z",
    );
    sample.append_at(
        d_remark(
            "the snowy mountain summit above the clouds",
            vec![c.clone()],
        ),
        "2026-02-03T10:00:00.000Z",
    );

    // The golden set: three single-answer queries (each relevant image is the
    // obvious top hit) plus one two-answer query to exercise recall < 1 at a
    // tight cutoff. These mirror what a real test-corpora/retrieval/*.json
    // file would hold, only with fixture hashes instead of a real library's.
    let golden = vec![
        GoldenQuery {
            query: "lighthouse harbor fog".into(),
            relevant: vec![a.as_str().to_owned()],
            notes: Some("single obvious hit -> MRR must be 1.0".into()),
        },
        GoldenQuery {
            query: "meadow wildflowers spring".into(),
            relevant: vec![b.as_str().to_owned()],
            notes: None,
        },
        GoldenQuery {
            query: "snowy mountain summit".into(),
            relevant: vec![c.as_str().to_owned()],
            notes: None,
        },
    ];

    // Score every golden query the way the runner does: run -> score@k.
    let k = 5;
    let mut scored = Vec::new();
    for g in &golden {
        let ranked = sample.ranked_hashes(&g.query);
        let metrics = QueryMetrics::score(&ranked, &g.relevant_set(), k);
        scored.push(NamedQueryMetrics {
            query: g.query.clone(),
            metrics,
        });
    }

    // Each single-answer query's relevant image is the obvious top hit: the
    // distinctive keywords match only its remark, so it ranks #1 -> MRR 1.0,
    // precision@5 = 1/5 (one relevant exists), recall = 1.0, nDCG = 1.0.
    for nq in &scored {
        assert_eq!(
            nq.metrics.mrr, 1.0,
            "query {:?}: the obvious relevant hit must rank #1 (MRR=1.0)",
            nq.query
        );
        assert_eq!(
            nq.metrics.recall_at_k, 1.0,
            "query {:?}: the single relevant item must be retrieved",
            nq.query
        );
        assert_eq!(
            nq.metrics.ndcg_at_k, 1.0,
            "query {:?}: a #1 hit is the ideal ranking (nDCG=1.0)",
            nq.query
        );
        // One relevant item among five slots.
        assert!((nq.metrics.precision_at_k - 1.0 / 5.0).abs() < 1e-9);
    }

    // The aggregate over the three perfect single-answer queries is all 1.0
    // (except precision, bounded by the cutoff): this is the headline number
    // a weight sweep would diff between runs.
    let report = EvalReport::from_scored(k, scored);
    assert_eq!(report.query_count, 3);
    assert_eq!(report.mean_mrr, 1.0);
    assert_eq!(report.mean_recall_at_k, 1.0);
    assert_eq!(report.mean_ndcg_at_k, 1.0);

    // A two-answer query where only ONE answer is retrievable by keyword (b's
    // remark contains "meadow", a's does not) proves recall correctly lands
    // below 1.0 when a relevant item never surfaces — the metric is honest,
    // not pinned to 1.0 by construction.
    let ranked = sample.ranked_hashes("meadow");
    let two_answer = rel(&[&a, &b]); // a is NOT retrievable by "meadow"
    let m = QueryMetrics::score(&ranked, &two_answer, k);
    assert_eq!(
        m.recall_at_k, 0.5,
        "only one of two relevant items surfaces"
    );
    assert_eq!(m.mrr, 1.0, "the retrievable answer is still the top hit");
}

/// A query with NO retrievable relevant item must score all zeros end to end
/// (never NaN) — the degenerate row a real sweep will hit for a hard query.
#[test]
fn sample_unsatisfiable_query_scores_zero_not_nan() {
    let sample = SampleEval::new();
    let a = sample.hashes[0].clone();
    sample.append_at(
        d_remark("the lighthouse at the harbor", vec![a.clone()]),
        "2026-02-01T10:00:00.000Z",
    );

    // The relevant image exists, but the query keyword matches nothing in any
    // remark, so the result list is empty and every metric floors at 0.
    let ranked = sample.ranked_hashes("zzznonexistentterm");
    let m = QueryMetrics::score(&ranked, &rel(&[&a]), 5);
    assert_eq!(m.precision_at_k, 0.0);
    assert_eq!(m.recall_at_k, 0.0);
    assert_eq!(m.mrr, 0.0);
    assert_eq!(m.ndcg_at_k, 0.0);
    assert!(m.ndcg_at_k.is_finite(), "nDCG must never be NaN");
}
