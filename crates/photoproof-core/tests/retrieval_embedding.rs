//! P7.1 acceptance — §2 chunking and the embedding ingest passes
//! (spec/RETRIEVAL.md §1.1 maintenance, §2, §3; LIBRARY §10 queue
//! mechanics / DECISIONS L4), mock-verified against the deterministic
//! `MockEmbedder`.
//!
//! Covered here: chunk boundaries/offsets/tiny-chunk prefix; the
//! NotConfigured-idle posture (no embedder => rows sit pending, zero
//! errors); inputs_hash staleness (re-annotation re-pends and re-embeds
//! only what changed); revision/redaction invalidation propagating into
//! zeroed flat-file bytes on the next drain (the §13.12 byte-scan,
//! end-to-end through the events engine).

mod common;

use photoproof_connectors::embedder::Embedder;
use photoproof_connectors::mock::MockEmbedder;
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit, VectorStore};
use photoproof_core::library::{EmbeddingRig, QueueOptions};
use photoproof_core::retrieval::{ChunkContext, PpvecStore, chunk_folded_text, inputs_hash};
use photoproof_core::{ContentHash, EventDraft, RemarkSource};

use common::m1env::M1Env;
use common::{d_remark, d_revision};

const TEXT_MODEL: &str = "mock-qwen3-embedding";
const CLIP_MODEL: &str = "mock-dfn5b-clip";
const DIMS: usize = 512;

// ---------------------------------------------------------------------------
// §2 chunking
// ---------------------------------------------------------------------------

#[test]
fn s2_short_text_is_one_chunk_spanning_the_whole_text() {
    let text = "Fog swallowing the barn, keep this one.";
    let chunks = chunk_folded_text(text, &ChunkContext::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].index, 0);
    assert_eq!(chunks[0].char_start, 0);
    assert_eq!(chunks[0].char_end, text.chars().count() as u32);
    assert_eq!(chunks[0].text, text);
    // No context metadata => no prefix even for a tiny chunk.
    assert_eq!(chunks[0].embed_text, text);
}

#[test]
fn s2_offsets_are_unicode_scalars_not_bytes() {
    let text = "Sebasti\u{e3}o \u{e0} caf\u{e9} \u{2014} \u{fc}ber alles";
    let chunks = chunk_folded_text(text, &ChunkContext::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].char_end as usize, text.chars().count());
    assert!(text.len() > text.chars().count(), "fixture is multi-byte");
}

#[test]
fn s2_long_monologue_chunks_with_overlap_and_sentence_snap() {
    // ~1200 words, a sentence every 12 words: far past the 512-token
    // single-chunk threshold, with sentence ends available inside every
    // snap window.
    let mut text = String::new();
    for i in 0..1200 {
        text.push_str(&format!("word{i:04}"));
        text.push_str(if i % 12 == 11 { ". " } else { " " });
    }
    let text = text.trim_end().to_string();
    let chars: Vec<char> = text.chars().collect();
    let chunks = chunk_folded_text(&text, &ChunkContext::default());
    assert!(chunks.len() >= 3, "got {} chunks", chunks.len());

    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.index as usize, i);
        // Offsets address the folded text exactly.
        let span: String = chars[c.char_start as usize..c.char_end as usize]
            .iter()
            .collect();
        assert_eq!(span, c.text);
        // Non-final boundaries snapped backward to a sentence end.
        if i + 1 < chunks.len() {
            assert!(
                c.text.ends_with('.'),
                "chunk {i} should end at a sentence end, got ...{:?}",
                &c.text[c.text.len().saturating_sub(12)..]
            );
            // 64-token overlap: the next chunk starts before this one ends.
            assert!(chunks[i + 1].char_start < c.char_end);
        }
        // Long chunks are not "tiny": never prefixed.
        assert_eq!(c.embed_text, c.text);
    }
    assert_eq!(chunks[0].char_start, 0);
    assert_eq!(
        chunks.last().unwrap().char_end as usize,
        chars.len(),
        "chunks cover the whole text"
    );
}

#[test]
fn s2_tiny_chunk_prefix_is_embed_time_only_and_deterministic() {
    let ctx = ChunkContext {
        date: Some("2026-01-14".into()),
        folder: Some("2026/iceland".into()),
        collection: Some("Quiet Hours".into()),
    };
    // One sentence: under ~2 sentences => prefixed at embed time.
    let one = chunk_folded_text("love this one", &ctx);
    assert_eq!(one[0].text, "love this one");
    assert_eq!(
        one[0].embed_text,
        "[2026-01-14 \u{b7} 2026/iceland \u{b7} Quiet Hours] love this one"
    );
    // Deterministic: same inputs, same bytes.
    assert_eq!(chunk_folded_text("love this one", &ctx), one);

    // Two sentences: no prefix.
    let two = chunk_folded_text("Love this one. Keep it.", &ctx);
    assert_eq!(two[0].embed_text, "Love this one. Keep it.");
}

// ---------------------------------------------------------------------------
// Embedding ingest passes (mock-verified)
// ---------------------------------------------------------------------------

struct Rig {
    env: M1Env,
    store: PpvecStore,
    text: MockEmbedder,
    clip: MockEmbedder,
    hashes: Vec<ContentHash>,
}

impl Rig {
    /// Full M1 stack + three ingested JPEGs + the PPVEC store.
    fn new() -> Self {
        let env = M1Env::new();
        let root = env.register("photos");
        let dir = env.mount.join("photos");
        for seed in 0..3u32 {
            let img = unique_jpeg(seed);
            std::fs::write(dir.join(format!("img{seed}.jpg")), img).unwrap();
        }
        env.scan(&root);
        env.drain();
        let mut hashes = env.lib.image_hashes().unwrap();
        hashes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(hashes.len(), 3);
        let store = PpvecStore::open(&env.db, env.app_data.join("vectors")).unwrap();
        Self {
            env,
            store,
            text: MockEmbedder::text(TEXT_MODEL, DIMS),
            clip: MockEmbedder::clip(CLIP_MODEL, DIMS),
            hashes,
        }
    }

    fn drain_embeddings(&self) -> photoproof_core::library::QueueReport {
        let rig = EmbeddingRig {
            text: Some(&self.text),
            clip: Some(&self.clip),
            vectors: &self.store,
        };
        self.env
            .lib
            .process_embedding_queue(&rig, &QueueOptions::default())
            .unwrap()
    }

    fn text_space(&self) -> VecSpace {
        VecSpace {
            vec_kind: VecKind::AnnotationChunk,
            model_id: TEXT_MODEL.into(),
        }
    }

    fn pass_state(&self, hash: &ContentHash, pass: &str) -> String {
        self.env
            .conn()
            .query_row(
                "SELECT state FROM ingest_passes
                 WHERE image_hash = ?1 AND pass_name = ?2",
                [hash.as_str(), pass],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.env.conn().query_row(sql, [], |r| r.get(0)).unwrap()
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

/// No embedder configured: nothing is claimed, nothing errors — the rows
/// (once backfilled) sit pending, NotConfigured-style, exactly like the
/// rest of the degraded posture.
#[test]
fn passes_sit_idle_when_no_embedder_is_configured() {
    let rig = Rig::new();
    rig.env
        .store
        .append(
            &rig.env.session,
            d_remark("quiet light", vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();

    // Wholly unconfigured: no rows are even created.
    let none: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: None,
        clip: None,
        vectors: &rig.store,
    };
    let report = rig
        .env
        .lib
        .process_embedding_queue(&none, &QueueOptions::default())
        .unwrap();
    assert_eq!((report.processed, report.errors), (0, 0));
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM ingest_passes
             WHERE pass_name IN ('text-embedding','image-embedding')"
        ),
        0
    );

    // Rows backfilled, embedder gone again: rows stay pending, no errors.
    rig.env.lib.enqueue_embedding_backfill(true, true).unwrap();
    let report = rig
        .env
        .lib
        .process_embedding_queue(&none, &QueueOptions::default())
        .unwrap();
    assert_eq!((report.processed, report.errors), (0, 0));
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM ingest_passes
             WHERE pass_name IN ('text-embedding','image-embedding')
               AND state = 'pending'"
        ),
        6
    );
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM ingest_passes
             WHERE pass_name IN ('text-embedding','image-embedding')
               AND state = 'error'"
        ),
        0
    );
}

/// Partial configuration drains only the configured kind; configuring the
/// second embedder later picks the rest up. B69: the clip signal is built
/// for every image regardless of journal coverage — additive, never
/// retired.
#[test]
fn passes_drain_per_configured_embedder() {
    let rig = Rig::new();
    let text_only: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: Some(&rig.text),
        clip: None,
        vectors: &rig.store,
    };
    let report = rig
        .env
        .lib
        .process_embedding_queue(&text_only, &QueueOptions::default())
        .unwrap();
    assert_eq!(report.done, 3, "text passes drained for all images");
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM ingest_passes WHERE pass_name = 'image-embedding'"),
        0,
        "unconfigured kind not even backfilled yet"
    );

    let report = rig.drain_embeddings();
    assert_eq!(report.done, 3, "clip passes drained once configured");
    for h in &rig.hashes {
        assert_eq!(rig.pass_state(h, "image-embedding"), "done");
    }
    // One image_clip vector per image, regardless of journal coverage.
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM vectors WHERE vec_kind = 'image_clip' AND deleted = 0"),
        3
    );
}

#[test]
fn retained_image_summary_text_rebuilds_one_idempotent_dense_row() {
    let rig = Rig::new();
    let image = &rig.hashes[0];
    let text_only: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: Some(&rig.text),
        clip: None,
        vectors: &rig.store,
    };
    rig.env
        .lib
        .process_embedding_queue(&text_only, &QueueOptions::default())
        .unwrap();
    assert_eq!(
        rig.pass_state(image, "text-embedding"),
        "done",
        "fixture starts from a library whose original text backfill completed"
    );

    let first = "Mist over the harbor; she keeps returning to the quiet frames.";
    rig.env
        .conn()
        .execute(
            "INSERT INTO derived_summaries
               (id, scope, scope_key, text, model_id, prompt_ver, inputs_hash, generated_ts)
             VALUES ('01JEMBEDSUMMARY000000000000', 'image', ?1, ?2,
                     'mock-summary-generator', 1, 'summary-inputs-v1',
                     '2026-07-26T12:00:00.000Z')",
            rusqlite::params![image.as_str(), first],
        )
        .unwrap();

    rig.env
        .lib
        .process_embedding_queue(&text_only, &QueueOptions::default())
        .unwrap();

    let space = VecSpace {
        vec_kind: VecKind::ImageSummary,
        model_id: TEXT_MODEL.into(),
    };
    let key = VecKey {
        space: space.clone(),
        unit: VecUnit::Image {
            image_hash: image.to_string(),
        },
    };
    assert_eq!(rig.store.space_stats(&space).unwrap(), (1, 1));
    assert_eq!(
        rig.store.row_inputs_hash(&key).unwrap(),
        Some((inputs_hash(first.as_bytes()), false))
    );
    let first_vector = rig.store.fetch(&key).unwrap().unwrap();

    // A regenerated retained summary is detected even if its text pass is
    // already done. The vector upsert replaces in place: there is still
    // exactly one row, and unchanged text remains a no-op.
    let revised = "Mist over the harbor; the quiet blue frames are now the anchors.";
    let conn = rig.env.conn();
    conn.execute(
        "UPDATE derived_summaries
         SET text = ?2, inputs_hash = 'summary-inputs-v2',
             generated_ts = '2026-07-26T13:00:00.000Z'
         WHERE scope = 'image' AND scope_key = ?1",
        rusqlite::params![image.as_str(), revised],
    )
    .unwrap();
    drop(conn);
    rig.env
        .lib
        .process_embedding_queue(&text_only, &QueueOptions::default())
        .unwrap();
    assert_eq!(rig.store.space_stats(&space).unwrap(), (1, 1));
    assert_eq!(
        rig.store.row_inputs_hash(&key).unwrap(),
        Some((inputs_hash(revised.as_bytes()), false))
    );
    assert_ne!(rig.store.fetch(&key).unwrap().unwrap(), first_vector);

    let unchanged = rig
        .env
        .lib
        .process_embedding_queue(&text_only, &QueueOptions::default())
        .unwrap();
    assert_eq!(unchanged.processed, 0);
    assert_eq!(
        rig.store.space_stats(&space).unwrap(),
        (1, 1),
        "retries never append duplicate image-summary rows"
    );
}

/// A CLIP MODEL SWAP re-embeds the library. Pass completion used to be
/// model-BLIND, so changing the embedder (the fp16 CLIP default change) left
/// every image-embedding pass `done` and topic affinities scored against a
/// vector space the new model never wrote — every affinity silently 0. The
/// model-aware re-pend must notice the new model_id and re-embed every image
/// under it, then NOT churn once the models match.
#[test]
fn a_clip_model_swap_repends_and_reembeds_the_library() {
    let rig = Rig::new();
    // First drain under the original CLIP model: 3 text (no annotations => no
    // chunks, still done) + 3 clip.
    let report = rig.drain_embeddings();
    assert_eq!(report.done, 6);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'image_clip' \
             AND model_id = '{CLIP_MODEL}' AND deleted = 0"
        )),
        3
    );
    // The passes now RECORD the model they embedded with (was NULL before).
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM ingest_passes WHERE pass_name = 'image-embedding' \
             AND state = 'done' AND model_id = '{CLIP_MODEL}'"
        )),
        3
    );

    // Swap the CLIP model (keep the text model), as the fp16 default did.
    const NEW_CLIP_MODEL: &str = "mock-dfn5b-clip-fp16";
    let new_clip = MockEmbedder::clip(NEW_CLIP_MODEL, DIMS);
    let swapped = EmbeddingRig {
        text: Some(&rig.text),
        clip: Some(&new_clip),
        vectors: &rig.store,
    };
    let report = rig
        .env
        .lib
        .process_embedding_queue(&swapped, &QueueOptions::default())
        .unwrap();
    // The 3 image passes were re-pended and re-embedded under the new model;
    // the text passes already match the current text model, so they don't churn.
    assert_eq!(report.done, 3, "clip re-embedded under the new model");
    // Vectors now exist under the NEW model — the space topic affinities query.
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'image_clip' \
             AND model_id = '{NEW_CLIP_MODEL}' AND deleted = 0"
        )),
        3
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM ingest_passes WHERE pass_name = 'image-embedding' \
             AND state = 'done' AND model_id = '{NEW_CLIP_MODEL}'"
        )),
        3
    );
    // Idempotent: a second drain with the same models re-pends nothing.
    let report = rig
        .env
        .lib
        .process_embedding_queue(&swapped, &QueueOptions::default())
        .unwrap();
    assert_eq!(report.done, 0, "no re-pend once the model matches");
}

/// SKIP-ALREADY-CORRECT EMBEDDINGS (self-heal 3B): regenerating the preview
/// FILE with DIFFERENT bytes for the SAME image + model + preview-generator
/// version must NOT re-embed. The old recipe hashed the raw preview bytes, so a
/// regen looked stale and re-embedded the whole library (the ~414-image churn).
/// The new recipe folds (image_hash, model_id, generator_version) instead — the
/// bytes can differ freely and the pass stays a no-op.
#[test]
fn regenerating_preview_bytes_does_not_reembed_same_image_and_model() {
    use photoproof_core::library::{ArtifactKind, PassName};

    let rig = Rig::new();
    // First drain: every image embedded once under the CLIP model.
    let report = rig.drain_embeddings();
    assert_eq!(report.done, 6, "3 text + 3 clip on first drain");
    let clip_vectors = || {
        rig.count(&format!(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'image_clip' \
             AND model_id = '{CLIP_MODEL}' AND deleted = 0"
        ))
    };
    assert_eq!(clip_vectors(), 3);
    // Capture the stored staleness hashes so we can prove they don't change.
    let stored_hashes = || -> Vec<String> {
        let conn = rig.env.conn();
        let mut stmt = conn
            .prepare(
                "SELECT inputs_hash FROM vectors WHERE vec_kind = 'image_clip' \
                 AND deleted = 0 ORDER BY inputs_hash",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    let before = stored_hashes();

    // Rewrite each preview artifact file with DIFFERENT but still-decodable
    // bytes — simulating a preview regen that produced new pixels-on-disk for
    // the same picture (e.g. a re-develop that did NOT bump the generator
    // version). The embed pass reads whichever of Display/Thumb exists.
    for (i, hash) in rig.hashes.iter().enumerate() {
        let mut rewrote = false;
        for kind in [ArtifactKind::Display, ArtifactKind::Thumb] {
            if let Some(rec) = rig.env.lib.preview_artifact(hash, kind).unwrap()
                && rec.file.exists()
            {
                // A fresh, byte-different JPEG (different seed) for the SAME image.
                std::fs::write(&rec.file, unique_jpeg(1000 + i as u32)).unwrap();
                rewrote = true;
            }
        }
        assert!(
            rewrote,
            "fixture: image {i} had a preview artifact to rewrite"
        );
    }

    // Re-pend the image-embedding pass so the drain actually re-examines each
    // image (otherwise the rows are already `done` and never reconsidered).
    rig.env.lib.repend_pass(PassName::ImageEmbedding).unwrap();

    // Drain again: the staleness hash is computed over (image, model, generator
    // version) — all unchanged — so each pass short-circuits to done WITHOUT a
    // new embedding. The drain still counts them done (no-op completion).
    let report = rig.drain_embeddings();
    assert_eq!(report.done, 3, "3 clip passes re-examined and completed");

    // No NEW vectors, and the stored staleness hashes are byte-identical: the
    // regen did not churn the embeddings.
    assert_eq!(clip_vectors(), 3, "still exactly one vector per image");
    assert_eq!(
        stored_hashes(),
        before,
        "staleness hash is independent of the preview file bytes"
    );
}

/// The end-to-end happy path: chunks + clip vectors with §1.2 metadata,
/// multi-target events stored once, searches resolving back to events.
#[test]
fn text_and_image_passes_embed_with_metadata() {
    let rig = Rig::new();
    // Two sentences => no tiny-chunk prefix; the embed text IS the folded
    // text, so the search side can reproduce it.
    let shared = "Fog swallowing the barn. Keep this one.";
    let multi = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark(shared, vec![rig.hashes[0].clone(), rig.hashes[1].clone()]),
            None,
        )
        .unwrap();
    rig.env
        .store
        .append(
            &rig.env.session,
            d_remark(
                "Harsh noon shadows. Probably a reject.",
                vec![rig.hashes[0].clone()],
            ),
            None,
        )
        .unwrap();

    let report = rig.drain_embeddings();
    assert_eq!(report.errors, 0);
    assert_eq!(report.done, 6, "3 text + 3 image passes");

    // One annotation_chunk row per (event, chunk) — the multi-target event
    // stores ONE vector; image association resolves through event_targets
    // at query time (§1.2).
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'annotation_chunk' AND deleted = 0"
        ),
        2
    );
    let (chars_start, chars_end): (i64, i64) = rig
        .env
        .conn()
        .query_row(
            "SELECT char_start, char_end FROM vectors WHERE event_id = ?1",
            [multi.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(chars_start, 0);
    assert_eq!(chars_end, shared.chars().count() as i64);

    // The §1.2 inputs_hash recipe over the embed text.
    let stored_hash: String = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash FROM vectors WHERE event_id = ?1",
            [multi.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored_hash, inputs_hash(shared.as_bytes()));

    // Vector search finds the chunk from its own text.
    let query = pollster::block_on(rig.text.embed_text(shared)).unwrap();
    let hits = rig.store.search(&query, rig.text_space(), 5).unwrap();
    assert_eq!(hits.len(), 2);
    let top_event: String = rig
        .env
        .conn()
        .query_row(
            "SELECT event_id FROM vectors WHERE id = ?1",
            [hits[0].vector_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(top_event, multi.id.to_string());
    assert!(
        hits[0].score > 0.98,
        "self-similarity, got {}",
        hits[0].score
    );
}

/// Tiny chunks embed with the deterministic metadata prefix — date and
/// folder — which never appears in the stored span (§2: embed time only).
#[test]
fn s2_tiny_chunk_prefix_uses_annotation_date_and_folder() {
    let rig = Rig::new();
    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark("quiet light", vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();
    rig.drain_embeddings();

    let date = &e.ts.to_rfc3339()[..10];
    let folder: String = {
        let rel: String = rig
            .env
            .conn()
            .query_row(
                "SELECT rel_path FROM paths WHERE image_hash = ?1 AND state = 'active'",
                [rig.hashes[0].as_str()],
                |r| r.get(0),
            )
            .unwrap();
        std::path::Path::new(&rel)
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    };
    let expected_embed_text = format!("[{date} \u{b7} {folder}] quiet light");

    let stored: String = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash FROM vectors WHERE event_id = ?1",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, inputs_hash(expected_embed_text.as_bytes()));

    // The prefixed text is what the embedder saw: querying with it scores
    // ~1 against the stored vector (deterministic mock).
    let q = pollster::block_on(rig.text.embed_text(&expected_embed_text)).unwrap();
    let hits = rig.store.search(&q, rig.text_space(), 1).unwrap();
    assert!(hits[0].score > 0.98);
}

/// inputs_hash staleness: a new annotation re-pends the image's
/// text-embedding pass (the §1.1 "enqueue embed" hook) and the re-run
/// embeds only the new chunk — fresh rows are skipped untouched.
#[test]
fn reannotation_repends_and_reembeds_only_what_changed() {
    let rig = Rig::new();
    rig.env
        .store
        .append(
            &rig.env.session,
            d_remark("First note. Keep.", vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();
    rig.drain_embeddings();
    assert_eq!(rig.pass_state(&rig.hashes[0], "text-embedding"), "done");
    let first_created: String = rig
        .env
        .conn()
        .query_row(
            "SELECT created_ts FROM vectors WHERE vec_kind = 'annotation_chunk'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Re-annotation re-pends exactly the touched image.
    rig.env
        .store
        .append(
            &rig.env.session,
            d_remark("Second thought. Reject.", vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();
    assert_eq!(rig.pass_state(&rig.hashes[0], "text-embedding"), "pending");
    assert_eq!(rig.pass_state(&rig.hashes[1], "text-embedding"), "done");

    let report = rig.drain_embeddings();
    assert_eq!(report.errors, 0);
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'annotation_chunk' AND deleted = 0"
        ),
        2
    );
    // The fresh chunk was skipped, not rewritten: created_ts unchanged.
    let still: String = rig
        .env
        .conn()
        .query_row(
            "SELECT created_ts FROM vectors
             WHERE vec_kind = 'annotation_chunk' ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still, first_created);
}

/// A revision invalidates the root's vectors (deleted=1, §1.1) and the
/// next drain re-embeds the new folded text with fresh offsets.
#[test]
fn revision_invalidates_and_reembeds_folded_text() {
    let rig = Rig::new();
    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark("Original wording here.", vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();
    rig.drain_embeddings();

    let revised = "Corrected wording, rather different. Keep it.";
    rig.env
        .store
        .append(&rig.env.session, d_revision(e.id.clone(), revised), None)
        .unwrap();
    // The events engine marked the chunk rows dead in the same
    // transaction (RETRIEVAL §1.1).
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM vectors WHERE deleted = 1"),
        1
    );
    assert_eq!(rig.pass_state(&rig.hashes[0], "text-embedding"), "pending");

    rig.drain_embeddings();
    let (hash_stored, char_end): (String, i64) = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash, char_end FROM vectors WHERE event_id = ?1 AND deleted = 0",
            [e.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(hash_stored, inputs_hash(revised.as_bytes()));
    assert_eq!(char_end, revised.chars().count() as i64);
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM vectors WHERE deleted = 1"),
        0,
        "dead rows swept by the drain"
    );
}

/// Redaction end-to-end: the instant `redact()` returns, the flat-file
/// bytes are zeroed (§13.5 — synchronous, never deferred to a drain) and a
/// byte-scan of the store file proves absence (§13.12 mirrored through the
/// full stack; the trait-level scrub test lives in retrieval_ppvec.rs).
/// The next drain then reclaims the metadata rows.
#[test]
fn r13_12_redaction_zeroes_flat_file_bytes_through_drain() {
    let rig = Rig::new();
    let secret = "Secret harbor location. Never share this.";
    // Pin a distinctive embedding: all-equal components quantize to a
    // recognizable nonzero byte run (1/sqrt(512) * 127 rounds to 6).
    rig.text.set_text_embedding(secret, vec![1.0; DIMS]);
    let marker_row = vec![6u8; DIMS];

    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark(secret, vec![rig.hashes[0].clone()]),
            None,
        )
        .unwrap();
    rig.drain_embeddings();

    let path = rig.store.file_path(&rig.text_space());
    let before = std::fs::read(&path).unwrap();
    assert!(
        before.windows(DIMS).any(|w| w == marker_row),
        "marker bytes present before redaction"
    );

    rig.env.store.redact(&e.id).unwrap();
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM vectors WHERE deleted = 1"),
        1,
        "redaction marks the vector dead in the same transaction"
    );
    // §13.5: zeroed the instant the redact call returns — NO drain ran yet.
    let immediately = std::fs::read(&path).unwrap();
    assert!(
        !immediately.windows(DIMS).any(|w| w == marker_row),
        "byte-scan: redacted vector bytes zeroed before redact() returned"
    );

    rig.drain_embeddings();
    let after = std::fs::read(&path).unwrap();
    assert!(
        !after.windows(DIMS).any(|w| w == marker_row),
        "byte-scan: redacted vector bytes absent from the entire file"
    );
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM vectors WHERE vec_kind = 'annotation_chunk'"),
        0,
        "no metadata row survives either"
    );

    // And the redacted text never comes back through search.
    let q = pollster::block_on(rig.text.embed_text(secret)).unwrap();
    assert!(
        rig.store
            .search(&q, rig.text_space(), 5)
            .unwrap()
            .is_empty()
    );
}

/// §1.1: session-level remarks (zero image targets) ARE indexed. No
/// per-image queue row can reach them, so the drain sweeps them directly;
/// staleness still applies (a second drain rewrites nothing), and a
/// revision re-embeds the new folded text.
#[test]
fn session_level_remarks_are_embedded() {
    let rig = Rig::new();
    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark(
                "Whole shoot felt rushed today. Reschedule the harbor set.",
                vec![],
            ),
            None,
        )
        .unwrap();

    rig.drain_embeddings();
    let (stored_hash, created): (String, String) = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash, created_ts FROM vectors WHERE event_id = ?1 AND deleted = 0",
            [e.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        stored_hash,
        inputs_hash("Whole shoot felt rushed today. Reschedule the harbor set.".as_bytes()),
        "session-level remark embedded with the bare folded text"
    );

    // Fresh on the next drain: swept, not rewritten.
    rig.drain_embeddings();
    let still: String = rig
        .env
        .conn()
        .query_row(
            "SELECT created_ts FROM vectors WHERE event_id = ?1",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still, created);

    // A revision invalidates and the sweep re-embeds the new text.
    let revised = "Whole shoot felt unhurried, actually. Keep the harbor set.";
    rig.env
        .store
        .append(&rig.env.session, d_revision(e.id.clone(), revised), None)
        .unwrap();
    rig.drain_embeddings();
    let new_hash: String = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash FROM vectors WHERE event_id = ?1 AND deleted = 0",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_hash, inputs_hash(revised.as_bytes()));
}

/// A journal change landing while the image's text-embedding pass is
/// RUNNING must not be lost: the §1.1 re-pend hook covers 'running' rows
/// and a stale `mark_done` cannot clobber the re-pend, so the next drain
/// re-embeds the new folded text.
#[test]
fn journal_change_during_running_pass_repends_and_reembeds() {
    let rig = Rig::new();
    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark(
                "Original mid-run wording. Keep.",
                vec![rig.hashes[0].clone()],
            ),
            None,
        )
        .unwrap();
    rig.drain_embeddings();
    assert_eq!(rig.pass_state(&rig.hashes[0], "text-embedding"), "done");

    // Simulate a drain that has claimed this image's pass (state =
    // 'running') when the user revises the remark.
    rig.env
        .conn()
        .execute(
            "UPDATE ingest_passes SET state = 'running'
             WHERE image_hash = ?1 AND pass_name = 'text-embedding'",
            [rig.hashes[0].as_str()],
        )
        .unwrap();
    let revised = "Revised while the pass was mid-flight. Still keep.";
    rig.env
        .store
        .append(&rig.env.session, d_revision(e.id.clone(), revised), None)
        .unwrap();
    assert_eq!(
        rig.pass_state(&rig.hashes[0], "text-embedding"),
        "pending",
        "the re-pend hook covers running rows"
    );

    rig.drain_embeddings();
    assert_eq!(rig.pass_state(&rig.hashes[0], "text-embedding"), "done");
    let stored: String = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash FROM vectors WHERE event_id = ?1 AND deleted = 0",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, inputs_hash(revised.as_bytes()));
}

/// §2/§13.8: a multi-target event's tiny-chunk prefix must not depend on
/// which image's pass claims it — the folder is resolved per EVENT
/// (smallest folder across targets), so the stored vector and its
/// inputs_hash are stable across drain orders and re-drains.
#[test]
fn multi_target_tiny_chunk_prefix_is_claim_order_independent() {
    let rig = Rig::new();
    let e = rig
        .env
        .store
        .append(
            &rig.env.session,
            d_remark(
                "quiet pair",
                vec![rig.hashes[0].clone(), rig.hashes[1].clone()],
            ),
            None,
        )
        .unwrap();
    rig.drain_embeddings();

    let date = &e.ts.to_rfc3339()[..10];
    // Both fixture images live in the same folder; the per-event rule
    // resolves to that folder deterministically.
    let expected = format!("[{date} \u{b7} photos] quiet pair");
    let (stored, created): (String, String) = rig
        .env
        .conn()
        .query_row(
            "SELECT inputs_hash, created_ts FROM vectors WHERE event_id = ?1",
            [e.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, inputs_hash(expected.as_bytes()));

    // Re-pend BOTH images and drain again: the chunk hashes fresh from
    // either claiming image — no rewrite churn.
    rig.env
        .conn()
        .execute(
            "UPDATE ingest_passes SET state = 'pending'
             WHERE pass_name = 'text-embedding'",
            [],
        )
        .unwrap();
    rig.drain_embeddings();
    let still: String = rig
        .env
        .conn()
        .query_row(
            "SELECT created_ts FROM vectors WHERE event_id = ?1",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still, created, "fresh from every claim order");
}

/// Voice remarks embed identically to typed ones (the §1.1 indexable set
/// is source-agnostic for remark text).
#[test]
fn voice_remarks_are_embedded_too() {
    let rig = Rig::new();
    rig.env
        .store
        .append(
            &rig.env.session,
            EventDraft::Remark {
                source: RemarkSource::Voice {
                    conf_pm: Some(912),
                    dur_ms: 1800,
                    linked_event: None,
                },
                text: "something quieter in these three".into(),
                targets: vec![rig.hashes[2].clone()],
            },
            None,
        )
        .unwrap();
    let report = rig.drain_embeddings();
    assert_eq!(report.errors, 0);
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM vectors WHERE vec_kind = 'annotation_chunk' AND deleted = 0"
        ),
        1
    );
}
