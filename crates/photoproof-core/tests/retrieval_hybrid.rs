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
    CollectionRef, Filter, FusionWeights, HybridOptions, HybridRig, NoModel, Provenance, SignalId,
    keyword_only_rig,
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

    // An unresolvable chip name drops with debug visibility — broadened
    // results, never an error (§13.4 applies to chips through the same
    // pipeline).
    let out = hx
        .env
        .searcher
        .hybrid_search(
            "fog",
            &[chip_collection("zzz qqq")],
            &keyword_only_rig(),
            &HybridOptions::default(),
        )
        .unwrap();
    assert_eq!(out.images.len(), 2, "clause dropped, query executes");
    assert_eq!(out.query.parsed.dropped.len(), 1);
    assert!(out.query.parsed.dropped[0].reason.contains("no collection"));
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

fn insert_image_summary(hx: &Hx, hash: &ContentHash, text: &str) {
    let conn = hx.env.conn();
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO derived_summaries
           (id, scope, scope_key, text, model_id, prompt_ver, inputs_hash, generated_ts)
         VALUES (?1, 'image', ?2, ?3, 'mock-gemma', 1, 'fixture', '2026-02-01T00:00:00.000Z')",
        rusqlite::params![id, hash.as_str(), text],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO summaries_fts (text, summary_id) VALUES (?1, ?2)",
        rusqlite::params![text, id],
    )
    .unwrap();
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
