//! P7.4 L5 — the REAL-MODEL end-to-end retrieval test (PLAN-P7.4 lane L5):
//! a tmp library ingests three real corpus photographs, synthetic journal
//! notes are appended, the embedding passes run through the live in-process
//! ort embedders (EmbeddingGemma text + DFN5B CLIP, the B73 defaults), and
//! a hybrid query whose words appear in NO note must return the
//! semantically right image with Quote provenance.
//!
//! `#[ignore]` by default: it needs the local model snapshots at
//! /Users/bornman/spike-p7-embed/models (~5 GB, founder machine only) and
//! the real JPEG corpus in test-corpora/. It SKIPS CLEANLY (early return,
//! no failure) when either is absent, so `--ignored` runs stay green on
//! other machines. Run with:
//!
//!   cargo test -p photoproof-core --test retrieval_e2e_real_models \
//!     -- --ignored --nocapture
//!
//! WHY the rig here mirrors `search_wire.rs` exactly (llm: None, document
//! `embed_text` + core's `instruct_query` on the query side): this test's
//! whole value is proving the SHIPPED code path semantic, not a friendlier
//! variant of it.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use photoproof_connectors::vector_store::VectorStore;
use photoproof_connectors::{OrtEmbedder, TextRecipe};
use photoproof_core::library::{EmbeddingRig, QueueOptions};
use photoproof_core::retrieval::PpvecStore;
use photoproof_core::search::{HybridOptions, HybridRig, NoModel, Provenance};

use common::d_remark;
use common::m1env::M1Env;

/// The local spike snapshots (docs/SPIKE-P7-EMBED.md pins; founder machine).
const MODELS_ROOT: &str = "/Users/bornman/spike-p7-embed/models";

/// Three semantically distinct corpus images (eye-verified content):
/// a dog sitting in autumn leaves, a golden-hour stairwell portrait, and
/// a boy at a party with balloons. Distinct subjects so one query has one
/// honest answer.
const CORPUS: [&str; 3] = ["bissy-8.jpeg", "jamie_web1-15.jpeg", "IMG_2808.JPEG"];

/// One synthetic journal note per image. Deliberately phrased so the test
/// queries below share NO word with any note (checked by the test): a hit
/// can only come from the semantic signals (S1 text vectors, S4 CLIP),
/// never from S2 keyword FTS.
const NOTES: [(&str, &str); 3] = [
    (
        "bissy-8.jpeg",
        "Bissy posing in the drift of fallen oak leaves, low warm light on her coat - best frame of the backyard set",
    ),
    (
        "jamie_web1-15.jpeg",
        "she leaned back on the rail mid-flight, hard sundown blasting through the window grid behind her - moody, cinematic",
    ),
    (
        "IMG_2808.JPEG",
        "his birthday grin with the toy viking and the black balloons - pure goofball energy, family album material",
    ),
];

/// Semantic queries and the filename of the image each must rank first.
/// Word-disjoint from every note (asserted), so S2 FTS contributes nothing.
const QUERIES: [(&str, &str); 2] = [
    ("dog among autumn foliage", "bissy-8.jpeg"),
    ("evening portrait by a stairway", "jamie_web1-15.jpeg"),
];

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-corpora/jpeg-sample")
}

#[test]
#[ignore = "needs the local model snapshots (founder machine) + real corpus"]
fn p74_e2e_real_models_semantic_hybrid_search() {
    let models = PathBuf::from(MODELS_ROOT);
    let gemma_onnx = models.join("embeddinggemma/onnx/model_quantized.onnx");
    let gemma_tok = models.join("embeddinggemma/tokenizer.json");
    let clip_vis = models.join("dfn5b/visual/model.onnx");
    let clip_txt = models.join("dfn5b/textual/model.onnx");
    let clip_tok = models.join("dfn5b/textual/tokenizer.json");
    let corpus = corpus_dir();
    for p in [&gemma_onnx, &gemma_tok, &clip_vis, &clip_txt, &clip_tok] {
        if !p.exists() {
            eprintln!("skipping: model snapshot absent at {}", p.display());
            return;
        }
    }
    if !corpus.exists() {
        eprintln!("skipping: corpus absent at {}", corpus.display());
        return;
    }

    // Guard the test's own premise: the queries must stay word-disjoint
    // from the notes, or S2 FTS could carry a result and the semantic
    // claim would be hollow. (Folding here is approximate - lowercase
    // word split - which is exactly as strict as FTS tokenization needs.)
    for (query, _) in QUERIES {
        for (file, note) in NOTES {
            for qw in query.split_whitespace() {
                let qw = qw.to_lowercase();
                assert!(
                    !note
                        .to_lowercase()
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|w| w == qw),
                    "query word '{qw}' appears in the note for {file}; rewrite the fixture"
                );
            }
        }
    }

    // ---- tmp library: ingest the three corpus images -------------------
    let env = M1Env::new();
    let root = env.register("photos");
    for name in CORPUS {
        std::fs::copy(corpus.join(name), env.mount.join("photos").join(name)).unwrap();
    }
    let t = Instant::now();
    env.scan(&root);
    env.drain(); // ingest + preview passes (Display artifacts feed CLIP)
    eprintln!("[e2e] ingest+previews: {:.2?}", t.elapsed());

    // Map filename -> content hash for the assertions.
    let conn = env.conn();
    let mut by_name = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT rel_path, image_hash FROM paths WHERE state = 'active'")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        for row in rows {
            let (rel, hash) = row.unwrap();
            let name = std::path::Path::new(&rel)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            by_name.insert(name, hash);
        }
    }
    assert_eq!(by_name.len(), 3, "all three corpus images ingested");

    // ---- synthetic journal notes ---------------------------------------
    for (file, note) in NOTES {
        let hash = photoproof_core::ContentHash::from_hex(&by_name[file]).unwrap();
        env.store
            .append(&env.session, d_remark(note, vec![hash]), None)
            .unwrap();
    }

    // ---- real embedders (the shipped construction parameters) ----------
    let t = Instant::now();
    let text = OrtEmbedder::text(
        "embeddinggemma-300m-q8",
        TextRecipe::MeanPooled,
        &gemma_onnx,
        &gemma_tok,
        768,
    )
    .expect("load EmbeddingGemma");
    eprintln!("[e2e] EmbeddingGemma load: {:.2?}", t.elapsed());
    let t = Instant::now();
    let clip = OrtEmbedder::clip(
        "ViT-H-14-378-quickgelu__dfn5b",
        &clip_vis,
        &clip_txt,
        &clip_tok,
        1024,
    )
    .expect("load DFN5B");
    eprintln!("[e2e] DFN5B load: {:.2?}", t.elapsed());

    // ---- the embedding passes (the P7.1 queue, live models) ------------
    let vectors = PpvecStore::open(&env.db, env.app_data.join("vectors")).unwrap();
    let rig = EmbeddingRig {
        text: Some(&text),
        clip: Some(&clip),
        vectors: &vectors,
    };
    let t = Instant::now();
    let report = env
        .lib
        .process_embedding_queue(&rig, &QueueOptions::default())
        .unwrap();
    eprintln!(
        "[e2e] embedding drain: {:.2?} (processed {}, done {}, errors {}, transient {})",
        t.elapsed(),
        report.processed,
        report.done,
        report.errors,
        report.transient_retries
    );
    // 3 text-embedding + 3 image-embedding rows, all done, none erroring.
    assert_eq!(report.done, 6, "all six embedding pass rows completed");
    assert_eq!(report.errors, 0);
    assert_eq!(report.transient_retries, 0);

    // ---- hybrid queries through the SHIPPED rig shape -------------------
    // Mirrors search_wire.rs run_search: llm None (degenerate parse), both
    // embedders, the PPVEC store. Query-side text goes through core's
    // instruct_query inside hybrid.rs - the same bytes the app sends.
    let search_rig: HybridRig<'_, NoModel, OrtEmbedder, OrtEmbedder> = HybridRig {
        llm: None,
        text: Some(&text),
        clip: Some(&clip),
        vectors: Some(&vectors as &dyn VectorStore),
    };

    for (query, expected_file) in QUERIES {
        let t = Instant::now();
        let out = env
            .searcher
            .hybrid_search(query, &[], &search_rig, &HybridOptions::default())
            .unwrap();
        eprintln!("[e2e] query {query:?}: {:.2?}", t.elapsed());
        for r in &out.images {
            let name = by_name
                .iter()
                .find(|(_, h)| **h == r.image_hash.to_string())
                .map(|(n, _)| n.as_str())
                .unwrap_or("?");
            let prov = match &r.provenance {
                Provenance::Quote(q) => format!("Quote({:?})", q.text),
                other => format!("{other:?}"),
            };
            eprintln!("[e2e]   {:.4}  {name}  {prov}", r.score);
        }
        let expected_hash = &by_name[expected_file];
        assert!(!out.images.is_empty(), "query {query:?} returned nothing");
        assert_eq!(
            out.images[0].image_hash.to_string(),
            *expected_hash,
            "query {query:?} should rank {expected_file} first"
        );
        // Provenance must be honest: the winner has a journal note, and
        // word-disjointness means it was found semantically - the §5.4
        // chain should surface that note as a Quote (S1 evidence); a
        // VisualMatch label would mean S1 never voted for the winner.
        match &out.images[0].provenance {
            Provenance::Quote(q) => {
                let (_, note) = NOTES.iter().find(|(f, _)| *f == expected_file).unwrap();
                assert_eq!(&q.text, note, "the quote is the photographer's own note");
            }
            other => panic!("expected Quote provenance for {expected_file}, got {other:?}"),
        }
    }
}
