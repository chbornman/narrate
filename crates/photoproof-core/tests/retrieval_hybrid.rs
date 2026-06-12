//! P7.2 acceptance — the §5 hybrid pipeline (spec/RETRIEVAL.md §5.1–5.4,
//! §6, §7.1, §10.3, §13.3/13.4), mock-verified: scripted MockLanguageModel
//! for the parse stage, pinned MockEmbedder vectors for controlled
//! similarity, the REAL PpvecStore (local derived storage, deterministic)
//! behind the VectorStore seam.
//!
//! Covered here: the §7.1 worked example reproduced end to end (parse JSON
//! under a stubbed LanguageModel, the dropped collection clause, per-signal
//! ranks, fused scores to 6 decimal places, result order, exact provenance
//! span); the no-LLM degenerate path equals M1 byte-for-byte; the §5.1
//! fallback triggers (model error, garbage JSON, over-budget parse,
//! everything-dropped); the hallucination firewall dropping exactly the
//! bad clauses; §10.3 collection resolution + the members filter, fully
//! degraded (no models); B69 S4 always-votes; S3 ranks but never quotes
//! summary text.

mod common;

use std::time::Duration;

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::error::ConnectorError;
use photoproof_connectors::mock::{MockEmbedder, MockLanguageModel};
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit};
use photoproof_core::collections::{CollectionStatus, Collections};
use photoproof_core::library::{EmbeddingRig, QueueOptions};
use photoproof_core::retrieval::{
    ChunkContext, PpvecStore, VecMeta, chunk_folded_text, instruct_query,
};
use photoproof_core::search::{
    CollectionRef, Filter, FusionWeights, HybridOptions, HybridRig, NoModel, Provenance,
    SearchError, SignalId, keyword_only_rig,
};
use photoproof_core::{ContentHash, EventDraft, UtcMillis};

use common::m1env::M1Env;
use common::{d_remark, d_voice};

const TEXT_MODEL: &str = "mock-qwen3-embedding";
const CLIP_MODEL: &str = "mock-dfn5b-clip";
const DIMS: usize = 512;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Hx {
    env: M1Env,
    vectors: PpvecStore,
    text: MockEmbedder,
    clip: MockEmbedder,
    llm: MockLanguageModel,
    /// The three ingested images, hash-sorted.
    hashes: Vec<ContentHash>,
}

impl Hx {
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
        assert_eq!(hashes.len(), 3);
        let vectors = PpvecStore::open(&env.db, env.app_data.join("vectors")).unwrap();
        Self {
            env,
            vectors,
            text: MockEmbedder::text(TEXT_MODEL, DIMS),
            clip: MockEmbedder::clip(CLIP_MODEL, DIMS),
            llm: MockLanguageModel::new("mock-gemma"),
            hashes,
        }
    }

    /// Append a remark at a crafted wall time (the §7.1 events live in
    /// last winter's window; `mint_at` keeps ULIDs monotonic regardless).
    fn append_at(&self, draft: EventDraft, at: &str) -> photoproof_core::Event {
        let minted = self.env.store.mint_at(ts(at));
        self.env
            .store
            .append(&self.env.session, draft, Some(minted))
            .unwrap()
    }

    /// Drain the text-embedding queue (chunks of every indexable event).
    fn drain_text_embeddings(&self) {
        let rig: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
            text: Some(&self.text),
            clip: None,
            vectors: &self.vectors,
        };
        self.env
            .lib
            .process_embedding_queue(&rig, &QueueOptions::default())
            .unwrap();
    }

    /// The exact text the embedding pass sends to the embedder for a
    /// short event (folded text + the §2 tiny-chunk prefix), derived the
    /// same way the pass derives it.
    fn embed_text_of(&self, hash: &ContentHash, event_text: &str, date: &str) -> String {
        let conn = self.env.conn();
        let rel: String = conn
            .query_row(
                "SELECT rel_path FROM paths WHERE image_hash = ?1 AND state = 'active'",
                [hash.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let folder = std::path::Path::new(&rel)
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .filter(|d| !d.is_empty());
        let ctx = ChunkContext {
            date: Some(date.to_owned()),
            folder,
            collection: None,
        };
        let chunks = chunk_folded_text(event_text, &ctx);
        assert_eq!(chunks.len(), 1, "fixture texts are single-chunk");
        chunks[0].embed_text.clone()
    }

    /// Insert one image-keyed vector (image_summary / image_clip) directly
    /// — the generation passes for these spaces are later packets; the
    /// query path is what P7.2 owns.
    fn upsert_image_vector(&self, kind: VecKind, model: &str, hash: &ContentHash, v: Vec<f32>) {
        let key = VecKey {
            space: VecSpace {
                vec_kind: kind,
                model_id: model.to_owned(),
            },
            unit: VecUnit::Image {
                image_hash: hash.as_str().to_owned(),
            },
        };
        let emb = Embedding {
            vector: l2(v),
            model_id: model.to_owned(),
        };
        self.vectors
            .upsert_with_meta(
                &key,
                &emb,
                &VecMeta {
                    inputs_hash: "test-fixture".into(),
                    char_start: None,
                    char_end: None,
                },
            )
            .unwrap();
    }
}

/// A small decodable JPEG whose bytes are unique per seed.
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

fn ts(s: &str) -> UtcMillis {
    UtcMillis::parse(s).unwrap()
}

/// A unit vector with `dot` projected on dimension 0 (the pinned query
/// direction) and the remainder on `aux` — controlled cosine similarity.
fn unit(dot: f32, aux: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; DIMS];
    v[0] = dot;
    v[aux] = (1.0 - dot * dot).sqrt();
    v
}

fn e0() -> Vec<f32> {
    let mut v = vec![0.0f32; DIMS];
    v[0] = 1.0;
    v
}

fn l2(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

/// Truncate to 6 decimal places — the §7.1 walkthrough sums per-term
/// 6-decimal values, so its totals are floor-truncated, not rounded
/// (0.043997884… is quoted as 0.043997).
fn floor6(x: f32) -> i64 {
    (f64::from(x) * 1e6).floor() as i64
}

fn hash_of(r: &photoproof_core::search::ImageResult) -> String {
    r.image_hash.to_string()
}

fn rank_of(r: &photoproof_core::search::ImageResult, signal: SignalId) -> Option<u32> {
    r.debug
        .as_ref()
        .and_then(|d| d.per_signal.iter().find(|(s, _, _)| *s == signal))
        .and_then(|(_, rank, _)| *rank)
}

fn chip_collection(name: &str) -> Filter {
    Filter::Collection(CollectionRef {
        raw: name.to_owned(),
        resolved: None,
    })
}

// ---------------------------------------------------------------------------
// §7.1 worked example — acceptance 13.6
// ---------------------------------------------------------------------------

const SEMANTIC: &str = "quieter, melancholic series I was considering";
const TQ3: &str =
    "something quieter in these three\u{2026} almost mournful, could anchor the slow series";

/// The §7.1 parse JSON, verbatim from the spec.
fn worked_example_parse_json() -> String {
    serde_json::json!({
        "filters": [
            { "type": "date", "field": "annotated",
              "relative": { "unit": "season", "season": "winter", "n": 1 } },
            { "type": "collection", "name": "quieter melancholic series" }
        ],
        "semantic": SEMANTIC,
        "visual": false
    })
    .to_string()
}

#[test]
fn r13_6_worked_example_7_1_reproduced() {
    let hx = Hx::new();
    let (a, b, c) = (&hx.hashes[0], &hx.hashes[1], &hx.hashes[2]);

    // Collections store: Quiet Hours (active), Harbor Nights (shelved).
    let collections = Collections::open(&hx.env.db, &hx.env.app_data).unwrap();
    collections
        .create("Quiet Hours", "", ts("2026-01-02T19:00:00.000Z"))
        .unwrap();
    let harbor = collections
        .create("Harbor Nights", "", ts("2026-01-03T19:00:00.000Z"))
        .unwrap();
    collections
        .set_status(
            &harbor.id,
            CollectionStatus::Shelved,
            ts("2026-02-01T00:00:00.000Z"),
        )
        .unwrap();

    // Last winter's events (annotated window [2025-12-01, 2026-03-01) when
    // run June 2026 — the §7.1 date resolution).
    let q3 = hx.append_at(
        d_voice(TQ3, vec![a.clone()], None),
        "2026-01-14T21:08:11.000Z",
    );
    hx.append_at(
        d_remark(
            "quieter, melancholic series I was considering \u{2014} but maybe not this one",
            vec![a.clone()],
        ),
        "2026-01-20T10:00:00.000Z",
    );
    hx.append_at(
        d_remark(
            "quieter, melancholic series I was considering",
            vec![b.clone()],
        ),
        "2026-01-21T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("maybe quieter for the slow series", vec![c.clone()]),
        "2026-01-22T10:00:00.000Z",
    );

    // Pinned similarities: the query embeds to e0; chunk similarity
    // descends A (via Q3) > B > C > A's second event — S1 image ranks
    // A 1, B 2, C 3 (max per image).
    hx.text.set_text_embedding(instruct_query(SEMANTIC), e0());
    hx.text
        .set_text_embedding(hx.embed_text_of(a, TQ3, "2026-01-14"), unit(0.9, 1));
    hx.text.set_text_embedding(
        hx.embed_text_of(
            a,
            "quieter, melancholic series I was considering \u{2014} but maybe not this one",
            "2026-01-20",
        ),
        unit(0.65, 2),
    );
    hx.text.set_text_embedding(
        hx.embed_text_of(
            b,
            "quieter, melancholic series I was considering",
            "2026-01-21",
        ),
        unit(0.8, 3),
    );
    hx.text.set_text_embedding(
        hx.embed_text_of(c, "maybe quieter for the slow series", "2026-01-22"),
        unit(0.7, 4),
    );
    hx.drain_text_embeddings();

    // S3: the §7.1 example fuses summaries as ONE combined sub-list at an
    // effective 0.7 (the spec's own footnote); here that is the
    // image_summary vector sub-list (A rank 1, C rank 2) with the
    // sub-list weight set to 0.7 — weights are §5.3 defaults, not
    // constants, and the §12 eval owns tuning them.
    hx.upsert_image_vector(VecKind::ImageSummary, TEXT_MODEL, a, unit(0.95, 5));
    hx.upsert_image_vector(VecKind::ImageSummary, TEXT_MODEL, c, unit(0.5, 6));

    hx.llm.push_response(worked_example_parse_json());

    let rig: HybridRig<'_, MockLanguageModel, MockEmbedder, MockEmbedder> = HybridRig {
        llm: Some(&hx.llm),
        text: Some(&hx.text),
        clip: None, // §7.1: S4 does not participate in this walkthrough
        vectors: Some(&hx.vectors),
    };
    let opts = HybridOptions {
        now: Some(ts("2026-06-15T12:00:00.000Z")), // "run June 2026"
        include_debug: true,
        weights: FusionWeights {
            s3_each: 0.7,
            ..FusionWeights::default()
        },
        ..HybridOptions::default()
    };
    let raw =
        "pull up the images I was considering for that quieter, melancholic series last winter";
    let out = hx
        .env
        .searcher
        .hybrid_search(raw, &[], &rig, &opts)
        .unwrap();

    // Stage 1 — the parse echo: one Date filter survives; the collection
    // clause dropped below the 0.80 threshold with the best candidate
    // named (the spec's illustrative 0.41 is metric-shape dependent; the
    // normative artifacts are the threshold, the drop, and the reason).
    let parsed = &out.query.parsed;
    assert!(!parsed.fallback);
    assert!(!parsed.visual);
    assert_eq!(parsed.semantic.as_deref(), Some(SEMANTIC));
    assert_eq!(parsed.keywords.as_deref(), Some(SEMANTIC));
    assert_eq!(parsed.filters.len(), 1);
    assert!(matches!(parsed.filters[0], Filter::Date { .. }));
    assert_eq!(parsed.dropped.len(), 1);
    assert_eq!(parsed.dropped[0].raw["type"], "collection");
    assert!(
        parsed.dropped[0].reason.contains("\u{2265} 0.80")
            && parsed.dropped[0].reason.contains("best 'Quiet Hours'"),
        "reason: {}",
        parsed.dropped[0].reason
    );

    // The parse request carried the grounding lists and constrained
    // decoding (§5.1 prompt structure is normative).
    let req = &hx.llm.requests()[0];
    assert_eq!(req.temperature, 0.0);
    assert!(req.json_schema.is_some());
    let system = &req.messages[0].content;
    assert!(system.contains("Quiet Hours (active)"), "{system}");
    assert!(system.contains("Harbor Nights (shelved)"), "{system}");
    assert!(system.contains("2026-06-15"), "{system}");

    // Stage 3 — fused order A, B, C with the §7.1 scores to 6 decimals
    // (0.043997 / 0.032522 / 0.027163; the walkthrough sums 6-decimal
    // truncated terms, so totals compare floor-truncated).
    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string(), b.to_string(), c.to_string()]
    );
    assert_eq!(floor6(out.images[0].score), 43_997);
    assert_eq!(floor6(out.images[1].score), 32_522);
    assert_eq!(floor6(out.images[2].score), 27_163);

    // Per-signal ranks (§7.1 stage 2): S1 A/B/C = 1/2/3; S2 B 1, A 2;
    // S3 A 1, C 2.
    let (ra, rb, rc) = (&out.images[0], &out.images[1], &out.images[2]);
    assert_eq!(rank_of(ra, SignalId::S1AnnotationChunk), Some(1));
    assert_eq!(rank_of(rb, SignalId::S1AnnotationChunk), Some(2));
    assert_eq!(rank_of(rc, SignalId::S1AnnotationChunk), Some(3));
    assert_eq!(rank_of(rb, SignalId::S2EventFts), Some(1));
    assert_eq!(rank_of(ra, SignalId::S2EventFts), Some(2));
    assert_eq!(rank_of(ra, SignalId::S3Summaries), Some(1));
    assert_eq!(rank_of(rc, SignalId::S3Summaries), Some(2));

    // Stage 4 — A's provenance is the §7.1 Quote: event Q3, voice, the
    // exact chunk span of the folded text.
    match &ra.provenance {
        Provenance::Quote(q) => {
            assert_eq!(q.event_id, q3.id);
            assert_eq!(q.ts.to_rfc3339(), "2026-01-14T21:08:11.000Z");
            assert_eq!(q.source, photoproof_core::Source::Voice);
            assert_eq!(q.text, TQ3);
            assert_eq!(q.char_start, 0);
            assert_eq!(q.char_end, TQ3.chars().count() as u32);
        }
        other => panic!("expected Quote provenance for A, got {other:?}"),
    }
    assert!(out.session_hits.is_empty());
}

// ---------------------------------------------------------------------------
// Degenerate case: no models == M1, byte for byte
// ---------------------------------------------------------------------------

#[test]
fn no_models_hybrid_equals_m1_search() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark(
            "the fog swallowing the barn, keep this one",
            vec![a.clone()],
        ),
        "2026-02-01T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog bank ate the whole ridge", vec![b.clone()]),
        "2026-02-02T10:00:00.000Z",
    );

    let m1 = hx.env.searcher.search("fog ba", &[]).unwrap();
    let hybrid = hx
        .env
        .searcher
        .hybrid_search(
            "fog ba",
            &[],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap();

    assert_eq!(hybrid.images, m1.images);
    assert_eq!(hybrid.session_hits, m1.session_hits);
    // The echo too: same filters (none), same keywords, no semantic (no
    // embedder could consume one), no fallback flag.
    assert_eq!(hybrid.query.parsed, m1.query.parsed);
}

// ---------------------------------------------------------------------------
// §5.1 fallback triggers — acceptance 13.3
// ---------------------------------------------------------------------------

fn fallback_fixture() -> Hx {
    let hx = Hx::new();
    hx.append_at(
        d_remark("quiet fog over the harbor", vec![hx.hashes[0].clone()]),
        "2026-02-03T10:00:00.000Z",
    );
    hx
}

fn llm_only_rig(llm: &MockLanguageModel) -> HybridRig<'_, MockLanguageModel, NoModel, NoModel> {
    HybridRig {
        llm: Some(llm),
        text: None,
        clip: None,
        vectors: None,
    }
}

#[test]
fn r13_3_model_error_falls_back_to_whole_query_fts() {
    let hx = fallback_fixture();
    hx.llm.push_error(ConnectorError::NotReady("llm"));
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "quiet fog",
            &[],
            &llm_only_rig(&hx.llm),
            &HybridOptions::default(),
        )
        .unwrap();
    assert!(out.query.parsed.fallback, "fallback flag visible (§13.3)");
    assert!(out.query.parsed.filters.is_empty(), "zero filters");
    assert_eq!(out.query.parsed.keywords.as_deref(), Some("quiet fog"));
    assert_eq!(
        out.images.len(),
        1,
        "results still flow — no user-facing error"
    );
}

#[test]
fn r13_3_garbage_json_falls_back() {
    let hx = fallback_fixture();
    hx.llm.push_response("the model went off-script entirely");
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "quiet fog",
            &[],
            &llm_only_rig(&hx.llm),
            &HybridOptions::default(),
        )
        .unwrap();
    assert!(out.query.parsed.fallback);
    assert_eq!(out.images.len(), 1);
}

#[test]
fn r13_3_over_budget_parse_is_discarded() {
    let hx = fallback_fixture();
    // A perfectly valid parse that arrives after the budget must not
    // shape the results (§5.1: < 1.5 s; a zero budget makes any answer
    // late, deterministically).
    hx.llm.push_response(
        serde_json::json!({
            "filters": [{ "type": "rating", "op": "gte", "value": 4 }],
            "semantic": "quiet fog",
            "visual": false
        })
        .to_string(),
    );
    let opts = HybridOptions {
        parse_budget: Duration::ZERO,
        ..HybridOptions::default()
    };
    let out = hx
        .env
        .searcher
        .hybrid_search("quiet fog", &[], &llm_only_rig(&hx.llm), &opts)
        .unwrap();
    assert!(out.query.parsed.fallback);
    assert!(out.query.parsed.filters.is_empty());
    assert_eq!(out.images.len(), 1);
}

#[test]
fn r13_3_everything_dropped_and_null_semantic_falls_back() {
    let hx = fallback_fixture();
    hx.llm.push_response(
        serde_json::json!({
            "filters": [{ "type": "aperture", "value": "f/2.8" }],
            "semantic": null,
            "visual": false
        })
        .to_string(),
    );
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "quiet fog",
            &[],
            &llm_only_rig(&hx.llm),
            &HybridOptions::default(),
        )
        .unwrap();
    assert!(out.query.parsed.fallback);
    // The reject stays visible in the debug panel even through fallback.
    assert_eq!(out.query.parsed.dropped.len(), 1);
    assert!(
        out.query.parsed.dropped[0]
            .reason
            .contains("unknown filter type 'aperture'")
    );
    assert_eq!(out.images.len(), 1);
}

// ---------------------------------------------------------------------------
// Hallucination firewall — acceptance 13.4
// ---------------------------------------------------------------------------

#[test]
fn r13_4_firewall_drops_exactly_the_bad_clauses_and_executes_the_rest() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    // Vocabulary: only A's camera is known to the library.
    hx.env
        .conn()
        .execute(
            "UPDATE images SET camera_model = 'FUJIFILM X-T5' WHERE image_hash = ?1",
            [a.as_str()],
        )
        .unwrap();
    hx.append_at(
        d_remark("fog over the water", vec![a.clone()]),
        "2026-03-03T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog in the pines", vec![b.clone()]),
        "2026-03-04T10:00:00.000Z",
    );

    hx.llm.push_response(
        serde_json::json!({
            "filters": [
                { "type": "aperture", "value": "f/2.8" },                      // unknown type
                { "type": "rating", "op": "gte", "value": 9 },                 // out of range
                { "type": "camera", "value": "Leica M11" },                    // vocab miss
                { "type": "collection", "name": "totally imaginary set" },     // unresolvable
                { "type": "camera", "value": "x-t5" },                         // valid
                { "type": "date", "field": "annotated",
                  "relative": { "unit": "year", "year": 2026 } }               // valid
            ],
            "semantic": "fog",
            "visual": false
        })
        .to_string(),
    );

    let opts = HybridOptions {
        now: Some(ts("2026-06-15T12:00:00.000Z")),
        ..HybridOptions::default()
    };
    let out = hx
        .env
        .searcher
        .hybrid_search("fog on the x-t5", &[], &llm_only_rig(&hx.llm), &opts)
        .unwrap();

    let parsed = &out.query.parsed;
    assert!(!parsed.fallback, "a partial drop never fails the query");
    assert_eq!(parsed.filters.len(), 2, "the two valid clauses execute");
    let reasons: Vec<&str> = parsed.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(
        reasons.len(),
        4,
        "exactly the bad clauses drop: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("unknown filter type 'aperture'"))
    );
    assert!(reasons.iter().any(|r| r.contains("rating 9 out of range")));
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("camera 'Leica M11' matches nothing"))
    );
    assert!(reasons.iter().any(|r| r.contains("no collection")));

    // Execution: only A passes the camera filter (B's remark matches the
    // FTS but B has no camera) — hard constraints filter, never rank.
    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string()]
    );
    match &out.images[0].provenance {
        Provenance::Quote(q) => assert!(q.text.contains("fog over the water")),
        other => panic!("expected Quote, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §10.3 collections in search — fully degraded (no models anywhere)
// ---------------------------------------------------------------------------

#[test]
fn r10_3_collection_chip_resolves_and_constrains_to_current_members() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark("fog at dawn", vec![a.clone()]),
        "2026-02-05T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog at dusk", vec![b.clone()]),
        "2026-02-06T10:00:00.000Z",
    );

    let collections = Collections::open(&hx.env.db, &hx.env.app_data).unwrap();
    let quiet = collections
        .create("Quiet Hours", "", ts("2026-01-02T19:00:00.000Z"))
        .unwrap();
    collections
        .add_images(
            &quiet.id,
            &[a.clone(), b.clone()],
            ts("2026-01-05T00:00:00.000Z"),
        )
        .unwrap();
    // B was removed: a recorded removal keeps it out of current members.
    collections
        .remove_images(
            &quiet.id,
            std::slice::from_ref(b),
            ts("2026-01-06T00:00:00.000Z"),
        )
        .unwrap();

    // Near-exact chip name resolves (normalized Jaro-Winkler >= 0.80).
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "fog",
            &[chip_collection("quiet hours")],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap();
    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string()],
        "current members only; the removed image stays out"
    );
    assert!(out.query.parsed.dropped.is_empty());

    // An unresolvable chip name is a HARD error, never a silent drop: the
    // §13.4 firewall covers LLM hallucinations, but a chip is typed user
    // intent — un-constraining it would broaden results invisibly (a
    // chip-only query would degrade to a whole-library browse), exactly
    // what filter.rs forbids for unresolved collections one layer down.
    let err = hx
        .env
        .searcher
        .hybrid_search(
            "fog",
            &[chip_collection("zzz qqq")],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(&err, SearchError::UnresolvedCollection { raw, reason }
            if raw == "zzz qqq" && reason.contains("no collection")),
        "expected UnresolvedCollection, got {err}"
    );
}

/// The chip-only sharp edge of the hard-error rule: an empty text query
/// with an unresolvable collection chip must NOT fall through to a
/// whole-library browse (the chip was the entire constraint).
#[test]
fn r10_3_chip_only_query_with_unresolvable_chip_errors_instead_of_browsing() {
    let hx = Hx::new();
    hx.append_at(
        d_remark("fog at dawn", vec![hx.hashes[0].clone()]),
        "2026-02-05T10:00:00.000Z",
    );

    let err = hx
        .env
        .searcher
        .hybrid_search(
            "",
            &[chip_collection("zzz qqq")],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(&err, SearchError::UnresolvedCollection { .. }),
        "expected UnresolvedCollection, got {err}"
    );
}

#[test]
fn r10_3_status_breaks_resolution_ties() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark("winter fog", vec![a.clone()]),
        "2026-02-07T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("winter fog too", vec![b.clone()]),
        "2026-02-08T10:00:00.000Z",
    );

    let collections = Collections::open(&hx.env.db, &hx.env.app_data).unwrap();
    // Two same-name collections, different status: active wins the tie.
    let done = collections
        .create("Winter Set", "", ts("2026-01-02T19:00:00.000Z"))
        .unwrap();
    collections
        .set_status(
            &done.id,
            CollectionStatus::Done,
            ts("2026-01-03T00:00:00.000Z"),
        )
        .unwrap();
    collections
        .add_images(
            &done.id,
            std::slice::from_ref(b),
            ts("2026-01-04T00:00:00.000Z"),
        )
        .unwrap();
    let active = collections
        .create("Winter Set", "", ts("2026-01-05T19:00:00.000Z"))
        .unwrap();
    collections
        .add_images(
            &active.id,
            std::slice::from_ref(a),
            ts("2026-01-06T00:00:00.000Z"),
        )
        .unwrap();

    let out = hx
        .env
        .searcher
        .hybrid_search(
            "winter fog",
            &[chip_collection("winter set")],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap();
    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string()],
        "active > shelved > done at equal similarity"
    );
}

// ---------------------------------------------------------------------------
// B69 — S4 votes on every semantic query; weight, not exclusion
// ---------------------------------------------------------------------------

#[test]
fn b69_image_clip_votes_on_semantic_queries_without_a_gate() {
    let hx = Hx::new();
    let (a, x) = (&hx.hashes[0], &hx.hashes[1]);
    // A is annotated; X has no journal at all — the clip signal is its
    // only way in (and an annotated image only gains signals, never
    // loses them).
    hx.append_at(
        d_remark("harbor fog rolling in", vec![a.clone()]),
        "2026-02-09T10:00:00.000Z",
    );
    hx.clip.set_text_embedding("harbor fog", e0());
    hx.upsert_image_vector(VecKind::ImageClip, CLIP_MODEL, x, unit(0.9, 7));
    hx.upsert_image_vector(VecKind::ImageClip, CLIP_MODEL, a, unit(0.8, 8));

    let rig: HybridRig<'_, NoModel, NoModel, MockEmbedder> = HybridRig {
        llm: None,
        text: None,
        clip: Some(&hx.clip),
        vectors: Some(&hx.vectors),
    };
    let opts = HybridOptions {
        include_debug: true,
        ..HybridOptions::default()
    };
    // No LLM, visual flag never set, S1 union S2 is well above nothing —
    // under the old §5.2 gate S4 would sit out; B69 says it votes.
    let out = hx
        .env
        .searcher
        .hybrid_search("harbor fog", &[], &rig, &opts)
        .unwrap();

    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string(), x.to_string()],
        "own words outrank clip by weight (1.0 vs 0.5), not by exclusion"
    );
    match &out.images[0].provenance {
        Provenance::Quote(q) => assert!(q.text.contains("harbor fog")),
        other => panic!("expected Quote for the annotated image, got {other:?}"),
    }
    assert_eq!(
        out.images[1].provenance,
        Provenance::VisualMatch,
        "clip-only evidence labels itself honestly — no fake quote (§6)"
    );
    assert_eq!(rank_of(&out.images[1], SignalId::S4ImageClip), Some(1));
    assert_eq!(rank_of(&out.images[0], SignalId::S4ImageClip), Some(2));
}

// ---------------------------------------------------------------------------
// S3 — summaries rank, but are never quoted (E4 / §5.4)
// ---------------------------------------------------------------------------

fn insert_summary(conn: &rusqlite::Connection, scope: &str, scope_key: &str, text: &str) {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO derived_summaries
           (id, scope, scope_key, text, model_id, prompt_ver, inputs_hash, generated_ts)
         VALUES (?1, ?2, ?3, ?4, 'mock-gemma', 1, 'fixture', '2026-02-01T00:00:00.000Z')",
        rusqlite::params![id, scope, scope_key, text],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO summaries_fts (text, summary_id) VALUES (?1, ?2)",
        rusqlite::params![text, id],
    )
    .unwrap();
}

fn insert_image_summary(hx: &Hx, hash: &ContentHash, text: &str) {
    insert_summary(&hx.env.conn(), "image", hash.as_str(), text);
}

#[test]
fn s3_summary_hits_rank_images_but_never_appear_as_provenance() {
    let hx = Hx::new();
    let (a, b, c) = (&hx.hashes[0], &hx.hashes[1], &hx.hashes[2]);
    hx.append_at(d_remark("fog", vec![a.clone()]), "2026-02-10T10:00:00.000Z");
    hx.append_at(
        d_remark("a quiet evening, no notes", vec![b.clone()]),
        "2026-02-11T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog at dawn over the ridge", vec![c.clone()]),
        "2026-02-12T10:00:00.000Z",
    );
    // C's summary matches the query — it boosts C above A. B's summary
    // matches too, but B's own events have no fog evidence: a summary may
    // RANK an image, never explain it (E4), so B drops out entirely
    // rather than rendering derived prose.
    insert_image_summary(
        &hx,
        c,
        "she keeps returning to the fog set, dawn over the ridge",
    );
    insert_image_summary(&hx, b, "the fog haze collection candidate");

    // No models anywhere: the summaries_fts sub-list is plain SQL and
    // works fully degraded.
    let opts = HybridOptions {
        include_debug: true,
        ..HybridOptions::default()
    };
    let out = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &keyword_only_rig(), &opts)
        .unwrap();

    let order: Vec<String> = out.images.iter().map(hash_of).collect();
    assert_eq!(
        order,
        vec![c.to_string(), a.to_string()],
        "C outranks A via the S3 boost; B is gone (no quotable evidence)"
    );
    match &out.images[0].provenance {
        Provenance::Quote(q) => {
            assert!(q.text.contains("fog at dawn"), "event words, {q:?}");
            assert!(
                !q.text.contains("she keeps returning"),
                "summary text must never be quoted (E4)"
            );
        }
        other => panic!("expected Quote, got {other:?}"),
    }
    assert!(
        rank_of(&out.images[0], SignalId::S3Summaries).is_some(),
        "the debug panel names the summary signal that ranked C"
    );
}

/// §5.4: the S3 provenance re-run queries "that image's own events" — its
/// candidate universe is the image's events, NOT the global top-500 bm25
/// cut. An image whose own matching event ranks below the global cut (a
/// common term in a journal-heavy library) must still surface with its
/// verbatim quote rather than being dropped or mislabeled.
#[test]
fn s3_provenance_rerun_is_scoped_to_the_images_own_events() {
    let hx = Hx::new();
    let (a, x) = (&hx.hashes[0], &hx.hashes[1]);
    // 600 terse "fog" remarks on A out-rank (bm25 favors short documents)
    // X's single long-winded remark, pushing it below the GLOBAL top-500
    // cut that S2 takes (spec-sanctioned for S2, §4).
    for _ in 0..600 {
        hx.append_at(d_remark("fog", vec![a.clone()]), "2026-02-10T10:00:00.000Z");
    }
    hx.append_at(
        d_remark(
            "the fog at midnight sat heavy on the water while every light \
             along the pier dissolved into the same grey nothing and I \
             stayed at the rail far longer than I had planned to stay",
            vec![x.clone()],
        ),
        "2026-02-10T11:00:00.000Z",
    );
    // Premise check: the M1/S2 path genuinely misses X (its event sits
    // outside the global top 500), so only S3 can rank it.
    let m1 = hx.env.searcher.search("fog", &[]).unwrap();
    assert_eq!(
        m1.images.iter().map(hash_of).collect::<Vec<_>>(),
        vec![a.to_string()],
        "fixture premise: X is below the global bm25 cut"
    );

    // X's summary matches the query, so S3 ranks it; the §5.4 re-run must
    // then recover X's OWN matching event as the quote.
    insert_image_summary(&hx, x, "she keeps returning to the fog set");

    let out = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &keyword_only_rig(), &HybridOptions::default())
        .unwrap();
    let xr = out
        .images
        .iter()
        .find(|r| hash_of(r) == x.to_string())
        .expect("X surfaces via S3 with recoverable own-words evidence");
    match &xr.provenance {
        Provenance::Quote(q) => assert!(
            q.text.contains("fog at midnight"),
            "the quote is X's own event text, {q:?}"
        ),
        other => panic!("expected Quote (never VisualMatch — §6), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §9.5 / §13.5 — redaction propagation into derived rows
// ---------------------------------------------------------------------------

fn match_count(conn: &rusqlite::Connection, term: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM summaries_fts WHERE summaries_fts MATCH ?1",
        [term],
        |r| r.get(0),
    )
    .unwrap()
}

fn summary_count(conn: &rusqlite::Connection, scope: &str, key: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM derived_summaries WHERE scope = ?1 AND scope_key = ?2",
        [scope, key],
        |r| r.get(0),
    )
    .unwrap()
}

/// §9.5: redacting an event deletes — in the same call — every derived
/// summary whose inputs may have included it (image, session, and folder
/// scopes), their `summaries_fts` rows, and the chain's sentiment rows.
/// §13.5: gone by the time `redact` returns; a paraphrase of scrubbed
/// content must not stay byte-scannable or keep ranking S3.
#[test]
fn redaction_deletes_dependent_summaries_their_fts_rows_and_sentiment() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    let secret = hx.append_at(
        d_remark("the secret ritual at the jetty", vec![a.clone()]),
        "2026-02-10T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("plain notes elsewhere", vec![b.clone()]),
        "2026-02-11T10:00:00.000Z",
    );

    // Derived rows whose inputs may include the event: A's image summary,
    // the session's summary, a folder rollup, the event's sentiment row.
    // B's image summary is the survival control.
    insert_image_summary(&hx, a, "keeps returning to the secret ritual at the jetty");
    insert_image_summary(&hx, b, "an unrelated elsewhere candidate");
    let conn = hx.env.conn();
    insert_summary(
        &conn,
        "session",
        hx.env.session.as_str(),
        "the session circled the secret ritual",
    );
    insert_summary(
        &conn,
        "folder",
        "photos",
        "the folder gathers the ritual set",
    );
    conn.execute(
        "INSERT INTO sentiment_scores (event_id, score, model_id, prompt_ver, scored_ts)
         VALUES (?1, 1, 'mock-gemma', 1, '2026-02-12T00:00:00.000Z')",
        [secret.id.as_str()],
    )
    .unwrap();

    // The paraphrase ranks before the redaction (the leak being closed).
    assert_eq!(match_count(&conn, "ritual"), 3);

    hx.env.store.redact(&secret.id).unwrap();

    // §13.5: absent the moment the call returns.
    assert_eq!(summary_count(&conn, "image", a.as_str()), 0);
    assert_eq!(summary_count(&conn, "session", hx.env.session.as_str()), 0);
    assert_eq!(summary_count(&conn, "folder", "photos"), 0);
    assert_eq!(
        match_count(&conn, "ritual"),
        0,
        "paraphrase out of the index"
    );
    let sentiment: i64 = conn
        .query_row("SELECT count(*) FROM sentiment_scores", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sentiment, 0, "the chain's sentiment rows are deleted");
    // The unrelated summary survives — deletion is scoped, not a wipe.
    assert_eq!(summary_count(&conn, "image", b.as_str()), 1);
    assert_eq!(match_count(&conn, "elsewhere"), 1);

    // And the live S3 path agrees: nothing ranks via the dead paraphrase.
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "ritual",
            &[],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap();
    assert!(out.images.is_empty() && out.session_hits.is_empty());
}

/// The merge path learns redactions too (§8 redaction supremacy): a
/// redaction event arriving via merge propagates into derived rows the
/// same way the local `redact()` call does.
#[test]
fn merge_learned_redaction_purges_dependent_summaries() {
    let hx = Hx::new();
    let a = &hx.hashes[0];
    let victim = hx.append_at(
        d_remark("the secret ritual at the jetty", vec![a.clone()]),
        "2026-02-10T10:00:00.000Z",
    );
    insert_image_summary(&hx, a, "keeps returning to the secret ritual");
    let conn = hx.env.conn();
    assert_eq!(match_count(&conn, "ritual"), 1);

    // A redaction event from another replica condemning the local victim.
    let minted = hx.env.store.mint_at(ts("2026-02-12T09:00:00.000Z"));
    let redaction = photoproof_core::Event {
        id: minted.id,
        v: 1,
        session_id: hx.env.session.clone(),
        ts: minted.ts,
        source: photoproof_core::Source::System,
        kind: photoproof_core::Kind::Redaction,
        targets: Vec::new(),
        text: None,
        payload: None,
        target_event: Some(victim.id.clone()),
        linked_event: None,
        redacted_by: None,
    };
    hx.env.store.merge(&[redaction]).unwrap();

    assert_eq!(summary_count(&conn, "image", a.as_str()), 0);
    assert_eq!(match_count(&conn, "ritual"), 0);
}

// ---------------------------------------------------------------------------
// §11 — rebuild covers BOTH FTS tables
// ---------------------------------------------------------------------------

/// §11: "wipe both FTS tables; re-fold every live remark chain root and
/// live summary." A rebuild after FTS drift (the §11 trigger: tokenizer or
/// fold-rule change) must leave `summaries_fts` exactly mirroring the
/// surviving `derived_summaries` rows — missing rows restored, orphans
/// (rows whose summary was deleted while the index was out of sync) gone.
#[test]
fn s11_rebuild_derived_rebuilds_summaries_fts_from_summary_rows() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark("the fog swallowing the barn", vec![a.clone()]),
        "2026-02-01T10:00:00.000Z",
    );
    insert_image_summary(&hx, a, "keeps returning to the fog set");
    insert_image_summary(&hx, b, "an unrelated elsewhere candidate");

    // Simulate index drift: the summary index lost a row and kept an
    // orphan whose backing summary row no longer exists.
    let conn = hx.env.conn();
    conn.execute(
        "DELETE FROM summaries_fts WHERE summaries_fts MATCH 'fog'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO summaries_fts (text, summary_id) \
         VALUES ('orphan zanzibar paraphrase', 'no-such-summary')",
        [],
    )
    .unwrap();
    assert_eq!(match_count(&conn, "fog"), 0, "fixture starts drifted");
    assert_eq!(match_count(&conn, "zanzibar"), 1);

    hx.env.store.rebuild_derived().unwrap();

    // The index again mirrors derived_summaries, row for row.
    assert_eq!(match_count(&conn, "fog"), 1, "missing row restored");
    assert_eq!(match_count(&conn, "elsewhere"), 1, "intact row survives");
    assert_eq!(match_count(&conn, "zanzibar"), 0, "orphan row dropped");
    let (rows, summaries): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM summaries_fts), \
                    (SELECT count(*) FROM derived_summaries)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, summaries, "one FTS row per summary, no extras");

    // And the live S3 path consumes the rebuilt index: the summary ranks
    // its image again through the model-free pipeline.
    let out = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &keyword_only_rig(), &HybridOptions::default())
        .unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].image_hash, *a);
}

// ---------------------------------------------------------------------------
// §5.1/§5.2 degraded-mode contract — vector signals fail, search answers
// ---------------------------------------------------------------------------

/// An embedder that is configured but broken at query time — the process
/// died between Ready and this call. Every request fails the way a dead
/// child fails.
struct ErroringEmbedder;

impl photoproof_connectors::embedder::Embedder for ErroringEmbedder {
    async fn embed_text(&self, _text: &str) -> photoproof_connectors::ConnectorResult<Embedding> {
        Err(ConnectorError::NotReady("embedder died"))
    }

    async fn embed_image(
        &self,
        _img: &photoproof_connectors::embedder::DecodedImage,
    ) -> photoproof_connectors::ConnectorResult<Embedding> {
        Err(ConnectorError::NotReady("embedder died"))
    }

    fn dimensions(&self) -> usize {
        DIMS
    }

    fn model_id(&self) -> &str {
        TEXT_MODEL
    }
}

/// §5.2 (module contract: a failed vector signal "degrades to absent with
/// a logged warning — search never errors"): an embedder that errors on
/// every query call must leave keyword results intact and return Ok.
#[test]
fn erroring_embedder_at_query_time_degrades_to_keyword_results() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark("the fog swallowing the barn", vec![a.clone()]),
        "2026-02-01T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog bank ate the whole ridge", vec![b.clone()]),
        "2026-02-02T10:00:00.000Z",
    );
    // Real vectors exist on disk — the failure is the query-side embed.
    hx.drain_text_embeddings();

    let broken = ErroringEmbedder;
    let rig: HybridRig<'_, NoModel, ErroringEmbedder, NoModel> = HybridRig {
        llm: None,
        text: Some(&broken),
        clip: None,
        vectors: Some(&hx.vectors),
    };
    let out = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &rig, &HybridOptions::default())
        .unwrap();
    let keyword = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &keyword_only_rig(), &HybridOptions::default())
        .unwrap();
    assert_eq!(
        out.images.iter().map(hash_of).collect::<Vec<_>>(),
        keyword.images.iter().map(hash_of).collect::<Vec<_>>(),
        "S2 alone carries the result set when every embed call fails"
    );
    assert_eq!(out.session_hits, keyword.session_hits);
}

/// Same contract, one layer down: the flat vector FILE is damaged or gone
/// (disk fault, partial restore) while the `vectors` metadata still points
/// at it. The store-level read fails or comes back empty; the search call
/// still answers with the FTS results and never surfaces an error.
#[test]
fn damaged_or_missing_vector_file_degrades_to_keyword_results() {
    let hx = Hx::new();
    let (a, b) = (&hx.hashes[0], &hx.hashes[1]);
    hx.append_at(
        d_remark("the fog swallowing the barn", vec![a.clone()]),
        "2026-02-01T10:00:00.000Z",
    );
    hx.append_at(
        d_remark("fog bank ate the whole ridge", vec![b.clone()]),
        "2026-02-02T10:00:00.000Z",
    );
    hx.drain_text_embeddings();
    let path = hx.vectors.file_path(&VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: TEXT_MODEL.into(),
    });
    assert!(path.exists(), "fixture wrote real vectors");

    let rig: HybridRig<'_, NoModel, MockEmbedder, NoModel> = HybridRig {
        llm: None,
        text: Some(&hx.text),
        clip: None,
        vectors: Some(&hx.vectors),
    };
    let keyword = hx
        .env
        .searcher
        .hybrid_search("fog", &[], &keyword_only_rig(), &HybridOptions::default())
        .unwrap();
    let expect_keyword = |label: &str| {
        let out = hx
            .env
            .searcher
            .hybrid_search("fog", &[], &rig, &HybridOptions::default())
            .unwrap_or_else(|e| panic!("{label}: search errored: {e}"));
        assert_eq!(
            out.images.iter().map(hash_of).collect::<Vec<_>>(),
            keyword.images.iter().map(hash_of).collect::<Vec<_>>(),
            "{label}: keyword results intact"
        );
    };

    // Damaged: the header bytes are garbage, so the store read errors.
    std::fs::write(&path, b"torn").unwrap();
    expect_keyword("torn file");

    // Gone entirely: the store reads an empty space.
    std::fs::remove_file(&path).unwrap();
    expect_keyword("missing file");
}
