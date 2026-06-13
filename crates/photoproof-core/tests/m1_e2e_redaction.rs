//! P4.1 C4 — redaction end-to-end across the whole stack: annotate → flush
//! (secret on disk in the sidecar) → redact → the text is gone from
//! EVERY byte surface: the SQLite database file, the WAL, the FTS index,
//! the sidecar file bytes, and search results — while the journal's
//! structure (scrubbed stub) and unrelated notes survive (I2, I8,
//! SIDECARS §11, RETRIEVAL §13.5).

mod common;

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::mock::MockEmbedder;
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit};
use photoproof_core::library::{EmbeddingRig, QueueOptions};
use photoproof_core::retrieval::{PpvecStore, VecMeta};
use photoproof_core::search::SearchOptions;
use photoproof_core::{EventDraft, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, SynthSpec};

const SECRET_TOKENS: &[&str] = &["zanzibar", "xylophone", "confidential"];

/// Byte-scan every file under `dir` (recursively) for each secret token.
fn assert_no_secret_bytes(dir: &std::path::Path, context: &str) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if !d.exists() {
            continue;
        }
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let bytes = std::fs::read(&p).unwrap();
            for token in SECRET_TOKENS {
                assert!(
                    !bytes
                        .windows(token.len())
                        .any(|w| w.eq_ignore_ascii_case(token.as_bytes())),
                    "{context}: secret token {token:?} found in {}",
                    p.display()
                );
            }
        }
    }
}

#[test]
fn c4_redaction_scrubs_db_wal_fts_sidecars_and_search() {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(10));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();

    let image = tree.files[0].hash.clone();
    let keeper = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "ordinary harmless note that must survive".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    let secret = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "zanzibar xylophone confidential — never to be seen again".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    env.flush_sidecars();

    // Pre-redaction: the secret is genuinely on every surface.
    let sidecar = root_dir.join(format!("{}.photoproof.json", tree.files[0].rel_path));
    assert!(
        String::from_utf8(std::fs::read(&sidecar).unwrap())
            .unwrap()
            .contains("zanzibar"),
        "secret reached the sidecar before redaction (the test must start dirty)"
    );
    let opts = SearchOptions {
        now: Some(UtcMillis::now()),
        include_debug: false,
        ..SearchOptions::default()
    };
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert_eq!(hits.images.len(), 1, "secret findable before redaction");

    // -- redact ----------------------------------------------------------------
    let receipt = env.engine.redact(&secret.id, UtcMillis::now()).unwrap();
    assert!(
        !receipt.adjacent_rewritten.is_empty(),
        "§11 step 3: the adjacent sidecar is rewritten before the call returns"
    );

    // 1. Search results: gone, instantly (RETRIEVAL §13.5).
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert!(hits.images.is_empty() && hits.session_hits.is_empty());
    // …and the FTS index has no row for it at all.
    {
        let conn = env.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM event_fts WHERE event_fts MATCH 'zanzibar'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "FTS purged");
    }

    // 2. The journal structure survives: a scrubbed stub, the keeper intact.
    let events = env.store.events_for_image(&image).unwrap();
    let stub = events.iter().find(|e| e.id == secret.id).unwrap();
    assert!(stub.text.is_none() && stub.payload.is_none());
    assert!(stub.redacted_by.is_some(), "I15 scrubbed shape");
    let keeper_row = events.iter().find(|e| e.id == keeper.id).unwrap();
    assert_eq!(
        keeper_row.text.as_deref(),
        Some("ordinary harmless note that must survive"),
        "I2: scrub touches exactly the condemned event"
    );

    // 3. Byte scan: db file, WAL/SHM, sidecars, overflow, session journals —
    //    every file under the env — carries no secret bytes (I8 + S5,
    //    `secure_delete=ON`, FTS secure-delete, WAL truncate, sidecar
    //    rewrite). The keeper's text must still be present in the sidecar.
    assert_no_secret_bytes(&env.base, "post-redaction");
    let sidecar_text = String::from_utf8(std::fs::read(&sidecar).unwrap()).unwrap();
    assert!(sidecar_text.contains("ordinary harmless note"));
    assert!(
        sidecar_text.contains(secret.id.as_str()),
        "the redacted event's husk (id + redacted_by) remains in the sidecar"
    );

    // 4. The keeper still surfaces in search; provenance quotes it.
    let hits = env.searcher.search_with("harmless", &[], &opts).unwrap();
    assert_eq!(hits.images.len(), 1);
    assert_eq!(hits.images[0].image_hash, image);

    // 5. A rebuild from these scrubbed sidecars never resurrects content:
    //    merge a stale pre-redaction copy through the reconciliation path.
    //    (The full backup-restore matrix is SIDECARS §13.4; here we assert
    //    the end-to-end surface stays clean after the stack's own scan.)
    let report = env
        .engine
        .scan(std::slice::from_ref(&root_dir), UtcMillis::now())
        .unwrap();
    assert!(report.failures.is_empty());
    assert_no_secret_bytes(&env.base, "post-reconciliation");

    env.assert_db_integrity();
}

const TEXT_MODEL: &str = "mock-qwen3-embedding";
const DIMS: usize = 512;

/// Cross-feature scenario for the FULL redaction propagation surface in
/// one pass (RETRIEVAL §13.5, SIDECARS §11, EVENTS §7): one `redact()`
/// call must, before it returns, scrub the event row, purge FTS, zero the
/// annotation-chunk vector bytes in the flat file, delete dependent
/// derived summaries (and their `summaries_fts` rows) and sentiment rows,
/// zero the image's `image_summary` vector bytes, and rewrite the adjacent
/// sidecar — and a restored pre-redaction sidecar backup plus a fresh
/// embedding drain must not resurrect content on ANY of those surfaces.
///
/// Each surface has its own focused suite (m1_e2e_redaction,
/// retrieval_ppvec, retrieval_embedding, retrieval_hybrid,
/// sidecars_acceptance); this test exists because the user's promise is
/// the CONJUNCTION, and only a single scenario can catch one propagation
/// leg silently missing from the shared transaction.
#[test]
fn r13_5_one_redaction_scrubs_fts_vectors_summaries_sentiment_and_sidecars() {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(4));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();

    let image = tree.files[0].hash.clone();
    let keeper = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "ordinary harmless note that must survive".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    // Two sentences: long enough to dodge the §2 tiny-chunk prefix, so the
    // embedding pass sends exactly this text to the embedder and the
    // pinned marker vector below lands in the flat file.
    let secret_text =
        "Zanzibar xylophone meeting point, strictly confidential. Never to be seen again.";
    let secret = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: secret_text.into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();

    // Real embedding pass through the mock embedder: the secret's chunk
    // quantizes to a recognizable byte run (1/sqrt(512) * 127 rounds to 6).
    let vectors = PpvecStore::open(&env.db, env.app_data.join("vectors")).unwrap();
    let text_embedder = MockEmbedder::text(TEXT_MODEL, DIMS);
    text_embedder.set_text_embedding(secret_text, vec![1.0; DIMS]);
    let marker_row = vec![6u8; DIMS];
    let rig: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: Some(&text_embedder),
        clip: None,
        vectors: &vectors,
    };
    env.lib
        .process_embedding_queue(&rig, &QueueOptions::default())
        .unwrap();

    // An image_summary vector for the target image, same marker bytes
    // (inserted directly: summary GENERATION is a later packet; redaction
    // must already cover the query-path rows that exist today).
    let norm = 1.0 / (DIMS as f32).sqrt();
    vectors
        .upsert_with_meta(
            &VecKey {
                space: VecSpace {
                    vec_kind: VecKind::ImageSummary,
                    model_id: TEXT_MODEL.into(),
                },
                unit: VecUnit::Image {
                    image_hash: image.as_str().to_owned(),
                },
            },
            &Embedding {
                vector: vec![norm; DIMS],
                model_id: TEXT_MODEL.into(),
            },
            &VecMeta {
                inputs_hash: "fixture".into(),
                char_start: None,
                char_end: None,
            },
        )
        .unwrap();

    // Derived rows whose inputs may include the secret: an image summary
    // paraphrasing it (plus its FTS row) and the event's sentiment score.
    {
        let conn = env.conn();
        conn.execute(
            "INSERT INTO derived_summaries
               (id, scope, scope_key, text, model_id, prompt_ver, inputs_hash, generated_ts)
             VALUES ('01SUMMARYFIXTURE0000000000', 'image', ?1,
                     'keeps circling the zanzibar rendezvous', 'mock-gemma', 1,
                     'fixture', '2026-03-01T00:00:00.000Z')",
            [image.as_str()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO summaries_fts (text, summary_id)
             VALUES ('keeps circling the zanzibar rendezvous', '01SUMMARYFIXTURE0000000000')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sentiment_scores (event_id, score, model_id, prompt_ver, scored_ts)
             VALUES (?1, -1, 'mock-gemma', 1, '2026-03-01T00:00:00.000Z')",
            [secret.id.as_str()],
        )
        .unwrap();
    }
    env.flush_sidecars();

    // Pre-state: the secret is genuinely live on every surface.
    let sidecar = root_dir.join(format!("{}.photoproof.json", tree.files[0].rel_path));
    let stale_sidecar_bytes = std::fs::read(&sidecar).unwrap();
    assert!(
        String::from_utf8(stale_sidecar_bytes.clone())
            .unwrap()
            .contains("Zanzibar"),
        "secret reached the sidecar"
    );
    let opts = SearchOptions {
        now: Some(UtcMillis::now()),
        include_debug: false,
        ..SearchOptions::default()
    };
    assert_eq!(
        env.searcher
            .search_with("zanzibar", &[], &opts)
            .unwrap()
            .images
            .len(),
        1
    );
    let text_file = vectors.file_path(&VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: TEXT_MODEL.into(),
    });
    let summary_file = vectors.file_path(&VecSpace {
        vec_kind: VecKind::ImageSummary,
        model_id: TEXT_MODEL.into(),
    });
    let has_marker = |p: &std::path::Path| {
        std::fs::read(p)
            .unwrap()
            .windows(DIMS)
            .any(|w| w == marker_row)
    };
    assert!(has_marker(&text_file), "chunk marker bytes on disk");
    assert!(has_marker(&summary_file), "summary marker bytes on disk");

    // -- ONE redaction call ------------------------------------------------------
    let receipt = env.engine.redact(&secret.id, UtcMillis::now()).unwrap();
    assert!(!receipt.adjacent_rewritten.is_empty());

    // FTS + search: gone the moment the call returned.
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert!(hits.images.is_empty() && hits.session_hits.is_empty());
    // Vector bytes: physically zeroed in BOTH flat files, no drain ran.
    assert!(!has_marker(&text_file), "chunk vector bytes zeroed");
    assert!(!has_marker(&summary_file), "summary vector bytes zeroed");
    {
        let conn = env.conn();
        let live: i64 = conn
            .query_row("SELECT count(*) FROM vectors WHERE deleted = 0", [], |r| {
                r.get(0)
            })
            .unwrap();
        // The keeper's chunk vector is the only live row left.
        assert_eq!(live, 1, "secret + summary vectors marked dead");
        // Derived rows: summary, its FTS row, and the sentiment row gone.
        let (summaries, sfts, sentiment): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM derived_summaries),
                        (SELECT count(*) FROM summaries_fts),
                        (SELECT count(*) FROM sentiment_scores)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((summaries, sfts, sentiment), (0, 0, 0));
    }
    // Sidecar: husk (id + redacted_by) and the keeper survive; the bytes
    // of every file under the env — db, WAL, sidecars, overflow, vectors
    // dir — carry no secret.
    let sidecar_text = String::from_utf8(std::fs::read(&sidecar).unwrap()).unwrap();
    assert!(sidecar_text.contains(secret.id.as_str()));
    assert!(sidecar_text.contains("ordinary harmless note"));
    assert_no_secret_bytes(&env.base, "post-redaction");

    // -- restored pre-redaction backup + fresh drain ------------------------------
    // A stale sidecar backup re-imported through the stack's own scan must
    // not resurrect the content; the embedding queue it re-pends must not
    // re-embed scrubbed text either.
    std::fs::write(&sidecar, &stale_sidecar_bytes).unwrap();
    let report = env
        .engine
        .scan(std::slice::from_ref(&root_dir), UtcMillis::now())
        .unwrap();
    assert!(report.failures.is_empty());
    env.lib
        .process_embedding_queue(&rig, &QueueOptions::default())
        .unwrap();

    assert_no_secret_bytes(&env.base, "post-backup-restore");
    assert!(!has_marker(&text_file), "no re-embedding of scrubbed text");
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert!(hits.images.is_empty() && hits.session_hits.is_empty());
    // The keeper still stands on every surface.
    let hits = env.searcher.search_with("harmless", &[], &opts).unwrap();
    assert_eq!(hits.images.len(), 1);
    let events = env.store.events_for_image(&image).unwrap();
    assert_eq!(
        events
            .iter()
            .find(|e| e.id == keeper.id)
            .unwrap()
            .text
            .as_deref(),
        Some("ordinary harmless note that must survive")
    );

    env.assert_db_integrity();
}
