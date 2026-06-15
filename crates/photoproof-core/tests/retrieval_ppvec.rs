//! P7.1 acceptance — PPVEC v2 vector storage (spec/RETRIEVAL.md §1.2–1.3).
//!
//! Naming carries the spec ids (BUILD-LOOP traceability): `r13_12_*` for
//! the §13.12 criteria — int8/MRL-512 round-trip against the transient-f32
//! reference, exact header round-trip, and the redaction byte-scan
//! (mirroring EVENTS I8). The remaining tests pin the §1.3 lifecycle:
//! replacement upserts, logical deletes, and compaction with `file_row`
//! remapping.

use std::path::Path;

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::vector_store::{
    VecHit, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError,
};
use photoproof_core::retrieval::{
    DTYPE_INT8, MRL_DIMS, PpvecStore, SpaceReconcileReason, mrl_truncate_normalize,
};
use std::collections::HashMap;

const MODEL: &str = "mock-text-embedder";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("photoproof.db")
    }

    fn vectors_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("vectors")
    }

    fn open(&self) -> PpvecStore {
        PpvecStore::open(self.db(), self.vectors_dir()).unwrap()
    }

    fn conn(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(self.db()).unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .unwrap();
        conn
    }
}

fn space() -> VecSpace {
    VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: MODEL.into(),
    }
}

fn chunk_key(event_id: &str, chunk_index: u32) -> VecKey {
    VecKey {
        space: space(),
        unit: VecUnit::AnnotationChunk {
            event_id: event_id.into(),
            chunk_index,
        },
    }
}

fn emb(vector: Vec<f32>) -> Embedding {
    Embedding {
        vector,
        model_id: MODEL.into(),
    }
}

/// `cos(t) * e0 + sin(t) * e_axis` in `dims` dimensions — unit length by
/// construction, with a controlled similarity to `e0`.
fn planted(dims: usize, axis: usize, t: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; dims];
    v[0] = t.cos();
    v[axis] = t.sin();
    v
}

fn event_id_of(fx: &Fixture, vector_id: i64) -> String {
    fx.conn()
        .query_row(
            "SELECT event_id FROM vectors WHERE id = ?1",
            [vector_id],
            |r| r.get(0),
        )
        .unwrap()
}

fn read_file(store: &PpvecStore, space: &VecSpace) -> Vec<u8> {
    std::fs::read(store.file_path(space)).unwrap()
}

fn contains_row(file: &[u8], row: &[u8]) -> bool {
    file.windows(row.len()).any(|w| w == row)
}

// ---------------------------------------------------------------------------
// §13.12 — PPVEC v2 round-trip
// ---------------------------------------------------------------------------

/// An int8/MRL-512 space written, closed, reopened, and scanned returns
/// top-k identical to the transient-f32 reference path. Vectors are
/// planted at controlled angles whose score gaps (>= 0.019) exceed the
/// worst-case int8 quantization error on a dot product of these sparse
/// vectors (<= 2/254 ~ 0.008), so order equality is exact by construction,
/// not by luck.
#[test]
fn r13_12_round_trip_int8_mrl_512_matches_f32_reference() {
    let fx = Fixture::new();
    let dims = 1024; // > 512: exercises MRL truncation on every vector
    let mut reference: Vec<(String, Vec<f32>)> = Vec::new();

    {
        let store = fx.open();
        for j in 0..10u32 {
            // Angles 0.2 .. 2.0 rad; components inside the first 512 dims
            // survive truncation untouched.
            let v = planted(dims, (j + 1) as usize, 0.2 * (j + 1) as f32);
            let id = format!("ev-{j:02}");
            store.upsert(chunk_key(&id, 0), &emb(v.clone())).unwrap();
            reference.push((id, v));
        }
        // Mass at dim 600 is dropped by MRL truncation; the remainder
        // renormalizes to exactly e0 and must outrank everything.
        let mut tr = vec![0.0f32; dims];
        tr[0] = (0.5f32).sqrt();
        tr[600] = (0.5f32).sqrt();
        store
            .upsert(chunk_key("ev-trunc", 0), &emb(tr.clone()))
            .unwrap();
        reference.push(("ev-trunc".into(), tr));
    } // closed

    let store = fx.open(); // reopened
    let mut query = vec![0.0f32; dims];
    query[0] = 1.0;
    let hits = store.search(&emb(query.clone()), space(), 11).unwrap();
    assert_eq!(hits.len(), 11);

    // Transient-f32 reference: truncate + renormalize + dot, no
    // quantization anywhere.
    let qref = mrl_truncate_normalize(&query);
    let mut expected: Vec<(String, f32)> = reference
        .iter()
        .map(|(id, v)| {
            let p = mrl_truncate_normalize(v);
            let dot = p.iter().zip(&qref).map(|(a, b)| a * b).sum::<f32>();
            (id.clone(), dot)
        })
        .collect();
    expected.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    let got_order: Vec<String> = hits.iter().map(|h| event_id_of(&fx, h.vector_id)).collect();
    let want_order: Vec<String> = expected.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(got_order, want_order, "top-k order == f32 reference order");
    assert_eq!(got_order[0], "ev-trunc", "MRL truncation renormalizes");

    // Scores stay within quantization tolerance of the reference.
    for (hit, (_, want)) in hits.iter().zip(&expected) {
        assert!(
            (hit.score - want).abs() < 0.01,
            "score {} vs reference {want}",
            hit.score
        );
    }
}

/// The header round-trips dtype/dims/scale/offset exactly (§13.12).
#[test]
fn r13_12_header_round_trips_exactly() {
    let fx = Fixture::new();
    {
        let store = fx.open();
        store
            .upsert(chunk_key("ev-h", 0), &emb(planted(1024, 3, 0.4)))
            .unwrap();
    }
    let store = fx.open();
    let header = store.header(&space()).unwrap().expect("file exists");
    assert_eq!(header.dims, MRL_DIMS as u32);
    assert_eq!(header.dtype, DTYPE_INT8);
    assert_eq!(header.scale, vec![1.0f32 / 127.0; MRL_DIMS]);
    assert_eq!(header.offset, vec![0.0f32; MRL_DIMS]);
}

// ---------------------------------------------------------------------------
// §13.12 — redaction byte-scan (mirroring EVENTS I8)
// ---------------------------------------------------------------------------

/// `scrub` zeroes the stored int8 row bytes in place: after the call, a
/// byte-scan of the whole file proves the row's bytes are gone, while
/// every other row survives untouched and searchable.
#[test]
fn r13_12_redaction_scrub_zeroes_row_bytes_byte_scan() {
    let fx = Fixture::new();
    let store = fx.open();
    let dims = MRL_DIMS;

    // All-equal components quantize to a distinctive nonzero run:
    // 1/sqrt(512) * 127 rounds to 6 in every dimension.
    let victim = emb(vec![1.0; dims]);
    let victim_row = vec![6u8; dims];
    let neg = emb(vec![-1.0; dims]);
    let neg_row = vec![250u8; dims]; // -6 as u8
    let mut sparse_v = vec![0.0f32; dims];
    sparse_v[0] = 1.0;

    store.upsert(chunk_key("ev-victim", 0), &victim).unwrap();
    store.upsert(chunk_key("ev-neg", 0), &neg).unwrap();
    store
        .upsert(chunk_key("ev-sparse", 0), &emb(sparse_v))
        .unwrap();

    let before = read_file(&store, &space());
    assert!(
        contains_row(&before, &victim_row),
        "victim bytes present before scrub"
    );

    store.scrub(chunk_key("ev-victim", 0)).unwrap();

    let after = read_file(&store, &space());
    assert_eq!(before.len(), after.len(), "scrub zeroes in place");
    assert!(
        !contains_row(&after, &victim_row),
        "byte-scan: victim bytes absent from the entire file"
    );
    assert!(
        contains_row(&after, &neg_row),
        "unrelated rows survive untouched"
    );

    // The scrubbed row is out of search results immediately.
    let mut q = vec![0.0f32; dims];
    q[0] = 1.0;
    let hits = store.search(&emb(q), space(), 10).unwrap();
    assert_eq!(hits.len(), 2);
    let ids: Vec<String> = hits.iter().map(|h| event_id_of(&fx, h.vector_id)).collect();
    assert!(!ids.contains(&"ev-victim".to_string()));

    // Scrubbing a key that does not exist is an error, not a silent no-op.
    assert!(matches!(
        store.scrub(chunk_key("ev-nope", 0)),
        Err(VectorStoreError::NotFound(_))
    ));
}

/// REDACTION RACE (RETRIEVAL §13.5/§13.12, review L4-host). The embedding
/// drain reads a chunk body, releases its locks, runs the (slow) embed, then
/// lands in `upsert_with_meta`. If a redaction commits in that window — vector
/// rows marked deleted=1, flat-file bytes zeroed synchronously, the event
/// scrubbed — the stale upsert must NOT resurrect the row and write an
/// embedding of the redacted text. Here we drive the exact interleave: embed,
/// redact, then a STALE `upsert_with_meta` of the pre-redaction vector. The
/// guard must leave the row dead and the bytes zeroed.
#[test]
fn stale_upsert_after_redaction_does_not_resurrect_the_vector() {
    use photoproof_core::retrieval::VecMeta;
    use photoproof_core::{EventDraft, EventStore, RemarkSource, SessionContext};

    let fx = Fixture::new();
    // EventStore and PpvecStore share the same db file AND the same
    // vectors dir (default_vectors_dir == <db parent>/vectors == fx dir), so
    // the redaction's synchronous zeroing hits the same flat file the store
    // wrote — exactly the production wiring.
    let events = EventStore::open(fx.db()).unwrap();
    let sid = events
        .open_session(SessionContext {
            app_version: "0.0.0-test".into(),
            device_id: "a".repeat(32),
            root_context: None,
        })
        .unwrap();
    let target = photoproof_core::ContentHash::from_bytes_of(b"redaction-race-image");
    let ev = events
        .append(
            &sid,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "the secret coordinates are forty two north".into(),
                targets: vec![target],
            },
            None,
        )
        .unwrap();

    let store = fx.open();
    let key = chunk_key(ev.id.as_str(), 0);
    // 1. The drain's first embed lands a live vector for the event's chunk.
    let secret_vec = emb(vec![1.0; MRL_DIMS]);
    let secret_row = vec![6u8; MRL_DIMS]; // 1/sqrt(512)*127 -> 6 each dim
    store
        .upsert_with_meta(
            &key,
            &secret_vec,
            &VecMeta {
                inputs_hash: "h0".into(),
                char_start: Some(0),
                char_end: Some(10),
            },
        )
        .unwrap();
    assert!(
        contains_row(&read_file(&store, &space()), &secret_row),
        "the live embedding is on disk before redaction"
    );

    // 2. The user redacts the event: marks the vector deleted, zeroes the
    //    bytes synchronously (§13.5), scrubs the event (redacted_by set).
    events.redact(&ev.id).unwrap();
    assert!(
        !contains_row(&read_file(&store, &space()), &secret_row),
        "redaction zeroed the bytes synchronously"
    );

    // 3. The stale in-flight embed lands AFTER the redaction. Without the
    //    guard this would flip deleted back to 0 and re-write the secret.
    store
        .upsert_with_meta(
            &key,
            &secret_vec,
            &VecMeta {
                inputs_hash: "h0".into(),
                char_start: Some(0),
                char_end: Some(10),
            },
        )
        .expect("stale upsert returns Ok, just does nothing");

    // The bytes stay zeroed and the row stays out of search — permanently.
    assert!(
        !contains_row(&read_file(&store, &space()), &secret_row),
        "stale upsert must NOT resurrect the redacted vector's bytes"
    );
    let mut q = vec![0.0f32; MRL_DIMS];
    q[0] = 1.0;
    let hits = store.search(&emb(q), space(), 10).unwrap();
    assert!(
        hits.is_empty(),
        "redacted vector must not be served as a search hit"
    );
}

// ---------------------------------------------------------------------------
// §1.3 lifecycle: upsert-replace, logical delete, compaction
// ---------------------------------------------------------------------------

#[test]
fn upsert_replaces_in_place_and_undeletes() {
    let fx = Fixture::new();
    let store = fx.open();
    let key = chunk_key("ev-a", 0);

    store
        .upsert(key.clone(), &emb(planted(1024, 1, 1.2)))
        .unwrap();
    store.mark_deleted(key.clone()).unwrap();
    let mut q = vec![0.0f32; 1024];
    q[0] = 1.0;
    assert!(
        store
            .search(&emb(q.clone()), space(), 10)
            .unwrap()
            .is_empty(),
        "deleted rows are skipped"
    );

    // Replace: un-deletes, overwrites the same file row (file does not grow).
    let len_before = read_file(&store, &space()).len();
    store.upsert(key, &emb(planted(1024, 1, 0.1))).unwrap();
    assert_eq!(read_file(&store, &space()).len(), len_before);
    let hits = store.search(&emb(q), space(), 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!((hits[0].score - (0.1f32).cos()).abs() < 0.01);
}

#[test]
fn compaction_drops_dead_rows_and_remaps_file_rows() {
    let fx = Fixture::new();
    let store = fx.open();
    for j in 0..5u32 {
        let id = format!("ev-{j}");
        store
            .upsert(
                chunk_key(&id, 0),
                &emb(planted(1024, (j + 1) as usize, 0.2 * (j + 1) as f32)),
            )
            .unwrap();
    }
    store.mark_deleted(chunk_key("ev-0", 0)).unwrap();
    store.mark_deleted(chunk_key("ev-2", 0)).unwrap();
    store.scrub(chunk_key("ev-4", 0)).unwrap();

    let mut q = vec![0.0f32; 1024];
    q[0] = 1.0;
    let before: Vec<VecHit> = store.search(&emb(q.clone()), space(), 10).unwrap();
    assert_eq!(before.len(), 2);
    let before_ids: Vec<String> = before
        .iter()
        .map(|h| event_id_of(&fx, h.vector_id))
        .collect();

    // 3 dead of 5 exceeds the 20% threshold.
    assert!(store.compact_if_needed(&space()).unwrap());

    let header = store.header(&space()).unwrap().unwrap();
    let file = read_file(&store, &space());
    let data_offset = 16 + 8 * header.dims as usize;
    assert_eq!(
        file.len(),
        data_offset + 2 * header.dims as usize,
        "compacted file holds exactly the live rows"
    );

    // Same results after compaction; file_rows remapped densely.
    let after = store.search(&emb(q), space(), 10).unwrap();
    let after_ids: Vec<String> = after
        .iter()
        .map(|h| event_id_of(&fx, h.vector_id))
        .collect();
    assert_eq!(after_ids, before_ids);
    for (b, a) in before.iter().zip(&after) {
        assert!((b.score - a.score).abs() < 1e-6, "scores unchanged");
    }
    let rows: Vec<i64> = {
        let conn = fx.conn();
        let mut stmt = conn
            .prepare("SELECT file_row FROM vectors ORDER BY file_row")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows, vec![0, 1]);

    // Nothing left to reclaim.
    assert!(!store.compact_if_needed(&space()).unwrap());
}

/// A torn append (crash mid-write) leaves a partial tail row; the next
/// append truncates it instead of wedging the space behind a permanent
/// Corrupt error. Safe by the §1.3 write order: the metadata row commits
/// only after the file write + fsync, so a partial row can never have a
/// committed pointer.
#[test]
fn torn_append_tail_is_truncated_and_healed() {
    let fx = Fixture::new();
    let store = fx.open();
    store
        .upsert(chunk_key("ev-a", 0), &emb(planted(1024, 1, 0.4)))
        .unwrap();
    let path = store.file_path(&space());
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&[7u8; 13]).unwrap(); // the torn write
    }

    store
        .upsert(chunk_key("ev-b", 0), &emb(planted(1024, 2, 0.8)))
        .unwrap();
    let hits = store
        .search(&emb(planted(1024, 1, 0.4)), space(), 10)
        .unwrap();
    assert_eq!(hits.len(), 2, "space survives the torn write");

    // The data region is whole rows again: torn bytes gone, both rows in.
    let header = store.header(&space()).unwrap().unwrap();
    let data_offset = 16 + 8 * u64::from(header.dims);
    let len = std::fs::metadata(&path).unwrap().len();
    assert_eq!((len - data_offset) % u64::from(header.dims), 0);
    assert_eq!((len - data_offset) / u64::from(header.dims), 2);
}

/// A header torn by a crash during space creation (file ends inside the
/// header/params) is recreated on the next upsert instead of failing with
/// Corrupt forever: no row can ever have committed against a torn header.
#[test]
fn torn_header_is_recreated_on_next_upsert() {
    let fx = Fixture::new();
    let store = fx.open();
    let path = store.file_path(&space());
    std::fs::write(&path, b"PPVEC\x02\x00").unwrap(); // 8 of 16 fixed bytes

    store
        .upsert(chunk_key("ev-a", 0), &emb(planted(1024, 1, 0.4)))
        .unwrap();
    let hits = store
        .search(&emb(planted(1024, 1, 0.4)), space(), 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
}

/// Crash window between compaction's SQL remap commit and the file rename:
/// the marker committed with the remap lets `open` complete the rename, so
/// remapped pointers are never paired with pre-compaction bytes (which
/// would silently score wrong vectors and let the next sweep zero live
/// rows).
#[test]
fn compaction_crash_between_remap_and_rename_heals_on_open() {
    let fx = Fixture::new();
    let mut q = vec![0.0f32; 1024];
    q[0] = 1.0;
    let expected_ids: Vec<String>;
    {
        let store = fx.open();
        for j in 0..5u32 {
            store
                .upsert(
                    chunk_key(&format!("ev-{j}"), 0),
                    &emb(planted(1024, (j + 1) as usize, 0.2 * (j + 1) as f32)),
                )
                .unwrap();
        }
        store.mark_deleted(chunk_key("ev-0", 0)).unwrap();
        store.mark_deleted(chunk_key("ev-2", 0)).unwrap();
        store.scrub(chunk_key("ev-4", 0)).unwrap();

        let path = store.file_path(&space());
        let pre_compact = std::fs::read(&path).unwrap();
        assert!(store.compact_if_needed(&space()).unwrap());
        expected_ids = store
            .search(&emb(q.clone()), space(), 10)
            .unwrap()
            .iter()
            .map(|h| event_id_of(&fx, h.vector_id))
            .collect();

        // Reconstruct the crash state: the remap (and its marker) are
        // committed, but the rename never happened — compacted bytes still
        // in the temp file, pre-compaction bytes at the real path.
        let tmp = path.with_extension("ppvec.compact-tmp");
        std::fs::rename(&path, &tmp).unwrap();
        std::fs::write(&path, &pre_compact).unwrap();
        fx.conn()
            .execute(
                "INSERT INTO ppvec_compactions (vec_kind, model_id)
                 VALUES ('annotation_chunk', ?1)",
                [MODEL],
            )
            .unwrap();
    } // "crash"

    let store = fx.open(); // recovery completes the rename
    let path = store.file_path(&space());
    assert!(!path.with_extension("ppvec.compact-tmp").exists());
    assert_eq!(
        fx.conn()
            .query_row("SELECT COUNT(*) FROM ppvec_compactions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "marker cleared"
    );
    let ids: Vec<String> = store
        .search(&emb(q), space(), 10)
        .unwrap()
        .iter()
        .map(|h| event_id_of(&fx, h.vector_id))
        .collect();
    assert_eq!(ids, expected_ids, "post-recovery results match");
}

/// §1.3 prewarm: sequentially touches the whole space file (header +
/// rows) so the first scan runs from the page cache.
#[test]
fn prewarm_touches_the_whole_space_file() {
    let fx = Fixture::new();
    let store = fx.open();
    assert_eq!(store.prewarm(&space()).unwrap(), 0, "no file yet");
    store
        .upsert(chunk_key("ev-a", 0), &emb(planted(1024, 1, 0.4)))
        .unwrap();
    let len = std::fs::metadata(store.file_path(&space())).unwrap().len();
    assert_eq!(store.prewarm(&space()).unwrap(), len);
}

#[test]
fn model_and_dimension_mismatches_are_errors() {
    let fx = Fixture::new();
    let store = fx.open();
    store
        .upsert(chunk_key("ev-a", 0), &emb(planted(1024, 1, 0.3)))
        .unwrap();

    // Wrong model id on the embedding.
    let wrong = Embedding {
        vector: planted(1024, 1, 0.3),
        model_id: "other-model".into(),
    };
    assert!(matches!(
        store.upsert(chunk_key("ev-b", 0), &wrong),
        Err(VectorStoreError::ModelMismatch { .. })
    ));

    // Mixing source dimensions within one space is corruption.
    assert!(matches!(
        store.upsert(chunk_key("ev-c", 0), &emb(vec![1.0; 64])),
        Err(VectorStoreError::DimensionMismatch { .. })
    ));

    // A space with no rows yields an empty hit list, not an error.
    let empty_space = VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: MODEL.into(),
    };
    assert!(
        store
            .search(&emb(planted(1024, 1, 0.0)), empty_space, 5)
            .unwrap()
            .is_empty()
    );
}

/// Spaces shorter than the MRL target store at their native width — the
/// truncation is a cap, not a pad.
#[test]
fn small_dims_space_stores_native_width() {
    let fx = Fixture::new();
    let store = fx.open();
    let sp = VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: MODEL.into(),
    };
    let key = VecKey {
        space: sp.clone(),
        unit: VecUnit::Image {
            image_hash: "ab".repeat(32),
        },
    };
    store.upsert(key, &emb(planted(64, 3, 0.5))).unwrap();
    let header = store.header(&sp).unwrap().unwrap();
    assert_eq!(header.dims, 64);
    let hits = store.search(&emb(planted(64, 3, 0.5)), sp, 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert!((hits[0].score - 1.0).abs() < 0.01, "self-similarity ~ 1");
}

/// The flat-file name is `{vec_kind}.{model_id_sanitized}.ppvec` (§1.3).
#[test]
fn file_layout_per_space() {
    let fx = Fixture::new();
    let sp = VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: "Qwen/Embedding:0.6B".into(),
    };
    let store = fx.open();
    let path = store.file_path(&sp);
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "annotation_chunk.Qwen_Embedding_0.6B.ppvec"
    );
    assert_eq!(path.parent().unwrap(), Path::new(&fx.vectors_dir()));
}

// ---------------------------------------------------------------------------
// Startup-doctor space reconciliation (STATE-INTEGRITY-AUDIT.md)
// ---------------------------------------------------------------------------

/// Write one `image_clip` vector for `image_hash` under `model`.
fn upsert_clip(store: &PpvecStore, model: &str, image_hash: &str, axis: usize) {
    let key = VecKey {
        space: VecSpace {
            vec_kind: VecKind::ImageClip,
            model_id: model.into(),
        },
        unit: VecUnit::Image {
            image_hash: image_hash.into(),
        },
    };
    let e = Embedding {
        vector: planted(1024, axis, 0.3),
        model_id: model.into(),
    };
    store.upsert(key, &e).unwrap();
}

fn clip_space(model: &str) -> VecSpace {
    VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: model.into(),
    }
}

fn active_clip(model: &str) -> HashMap<VecKind, String> {
    HashMap::from([(VecKind::ImageClip, model.to_owned())])
}

/// The live duplicate-space bug (founder's machine: `dfn5b` lingered after the
/// swap to `dfn5b-fp16`): a superseded model's space whose ACTIVE replacement is
/// populated is dropped, rows and file, with no re-pend.
#[test]
fn reconcile_drops_superseded_clip_space_rows_and_file() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "old", "a", 1);
    upsert_clip(&store, "old", "b", 2);
    upsert_clip(&store, "new", "a", 1);
    upsert_clip(&store, "new", "b", 2);
    assert!(store.file_path(&clip_space("old")).exists());

    let report = store.reconcile_spaces(&active_clip("new")).unwrap();

    assert_eq!(report.reconciled.len(), 1, "only the old space acted on");
    assert_eq!(report.reconciled[0].model_id, "old");
    assert_eq!(
        report.reconciled[0].reason,
        SpaceReconcileReason::SupersededByActiveModel
    );
    assert!(report.repend.is_empty(), "superseded never needs a re-pend");
    // Old rows gone, old file swept; the active space is untouched.
    assert!(store.any_model_id(VecKind::ImageClip).unwrap().is_some());
    assert!(!store.file_path(&clip_space("old")).exists());
    assert!(store.file_path(&clip_space("new")).exists());
    let (live, _) = store.space_stats(&clip_space("new")).unwrap();
    assert_eq!(live, 2, "active space rows survive intact");
}

/// The CONSERVATIVE guard: a superseded space is NOT dropped while the active
/// model's space is empty (a half-finished re-embed), so the only copy of the
/// vectors is never lost.
#[test]
fn reconcile_keeps_superseded_when_active_space_is_empty() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "old", "a", 1);
    // active says "new" but nothing is embedded under it yet.
    let report = store.reconcile_spaces(&active_clip("new")).unwrap();
    assert!(report.is_empty(), "nothing safe to drop yet: {report:?}");
    assert!(store.file_path(&clip_space("old")).exists());
    let (live, _) = store.space_stats(&clip_space("old")).unwrap();
    assert_eq!(live, 1);
}

/// An ACTIVE space whose `.ppvec` file vanished (vectors dir mangled): the rows
/// are dangling pointers, so they are deleted and the embedding pass is flagged
/// for a re-pend rather than left to corrupt a re-embed at stale offsets.
#[test]
fn reconcile_repends_active_space_when_file_missing() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "new", "a", 1);
    upsert_clip(&store, "new", "b", 2);
    std::fs::remove_file(store.file_path(&clip_space("new"))).unwrap();

    let report = store.reconcile_spaces(&active_clip("new")).unwrap();

    assert_eq!(report.reconciled.len(), 1);
    assert_eq!(
        report.reconciled[0].reason,
        SpaceReconcileReason::DanglingActiveFileMissing
    );
    assert_eq!(
        report.repend,
        vec![(VecKind::ImageClip, "new".to_owned())],
        "the shell must re-pend the embedding pass to rebuild the space"
    );
    assert!(
        store.any_model_id(VecKind::ImageClip).unwrap().is_none(),
        "dangling rows are cleared so a re-embed rebuilds dense from row 0"
    );
}

/// A `.ppvec` file with no live rows pointing at it (a crash between metadata
/// delete and file removal) is swept as orphaned bytes.
#[test]
fn reconcile_sweeps_orphan_file() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "new", "a", 1);
    // A stray file no row references.
    let orphan = fx.vectors_dir().join("image_clip.ghost.ppvec");
    std::fs::write(&orphan, b"PPVEC\x02 not really a space").unwrap();

    let report = store.reconcile_spaces(&active_clip("new")).unwrap();

    assert_eq!(report.reconciled.len(), 1);
    assert_eq!(
        report.reconciled[0].reason,
        SpaceReconcileReason::OrphanFile
    );
    assert!(!orphan.exists(), "orphan bytes removed");
    assert!(
        store.file_path(&clip_space("new")).exists(),
        "the live space's file is kept"
    );
}

/// A healthy store, and a second run after a heal, both report nothing: the
/// doctor is safe to run on every startup.
#[test]
fn reconcile_is_idempotent_on_a_healthy_store() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "new", "a", 1);
    upsert_clip(&store, "old", "a", 1);

    let first = store.reconcile_spaces(&active_clip("new")).unwrap();
    assert_eq!(first.reconciled.len(), 1, "drops the superseded old space");

    let second = store.reconcile_spaces(&active_clip("new")).unwrap();
    assert!(second.is_empty(), "nothing left to do: {second:?}");
}

/// No active-model entry for a kind (embedder unloaded, nothing to compare to)
/// leaves every space of that kind alone: the doctor never guesses.
#[test]
fn reconcile_without_active_model_touches_nothing() {
    let fx = Fixture::new();
    let store = fx.open();
    upsert_clip(&store, "old", "a", 1);
    upsert_clip(&store, "new", "a", 1);
    let report = store.reconcile_spaces(&HashMap::new()).unwrap();
    assert!(
        report.is_empty(),
        "no active model => no judgement: {report:?}"
    );
    assert!(store.file_path(&clip_space("old")).exists());
    assert!(store.file_path(&clip_space("new")).exists());
}
