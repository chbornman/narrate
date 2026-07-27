use std::path::{Path, PathBuf};
use std::sync::Arc;

use photoproof_connectors::embedder::Embedding;
use photoproof_connectors::mock::MockEmbedder;
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit, VectorStore};
use photoproof_core::library::{
    ArtifactKind, EmbeddingRig, FakeVolumeProbe, FullDecodeFormat, Library, LibraryOptions,
    OrphanRetentionMode, PlatformIdKind, ProbedVolume, QueueOptions, ScanOptions, artifact_path,
    full_artifact_path,
};
use photoproof_core::retrieval::{PpvecStore, inputs_hash};
use photoproof_core::{
    ContentHash, EventDraft, EventStore, RemarkSource, SessionContext, UtcMillis,
};
use rusqlite::{Connection, params};

fn probed(mount: &Path) -> ProbedVolume {
    ProbedVolume {
        mount_point: mount.to_path_buf(),
        platform_id: Some("retention-test-volume".into()),
        platform_kind: PlatformIdKind::LinuxFsUuid,
        label: Some("Retention".into()),
        fs_type: Some("ext4".into()),
        capacity_bytes: Some(1 << 30),
        read_only_flag: false,
        is_system_root: false,
        coarse_mtime: false,
    }
}

fn put_artifacts(cache: &Path, hash: &ContentHash) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for kind in [ArtifactKind::Thumb, ArtifactKind::Display] {
        let path = artifact_path(cache, hash, kind);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [7u8; 16]).unwrap();
        paths.push(path);
    }
    let full = full_artifact_path(cache, hash, FullDecodeFormat::Jpeg);
    std::fs::write(&full, [8u8; 32]).unwrap();
    paths.push(full);
    paths
}

#[allow(clippy::too_many_arguments)]
fn seed_image(
    conn: &Connection,
    cache: &Path,
    hash: &ContentHash,
    path_id: &str,
    volume_id: &str,
    root_id: &str,
    rel_path: &str,
    state: &str,
    stale_since: Option<&str>,
) -> Vec<PathBuf> {
    conn.execute(
        "INSERT OR IGNORE INTO images
           (image_hash, byte_size, format, exif_orientation, first_ingested_at)
         VALUES (?1, 3, 'jpeg', 1, '2026-01-01T00:00:00.000Z')",
        [hash.as_str()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO paths
           (path_id, image_hash, volume_id, root_id, rel_path, size, mtime_ns,
            state, stale_reason, first_seen_at, last_verified_at, stale_since)
         VALUES (?1, ?2, ?3, ?4, ?5, 3, 1, ?6,
                 CASE WHEN ?6 = 'stale' THEN 'deleted' END,
                 '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', ?7)",
        params![
            path_id,
            hash.as_str(),
            volume_id,
            root_id,
            rel_path,
            state,
            stale_since
        ],
    )
    .unwrap();
    for pass in ["preview", "image-embedding", "text-embedding"] {
        conn.execute(
            "INSERT INTO ingest_passes
               (image_hash, pass_name, pass_version, model_id, state, priority,
                attempts, error, enqueued_at, started_at, completed_at, not_before)
             VALUES (?1, ?2, 1, 'fixture-model', 'done', 3, 1, NULL,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z',
                     '2026-01-01T00:00:00.000Z', NULL)
             ON CONFLICT(image_hash, pass_name, pass_version)
             DO UPDATE SET state = 'done', error = NULL",
            params![hash.as_str(), pass],
        )
        .unwrap();
    }
    for kind in ["thumb", "display"] {
        conn.execute(
            "INSERT INTO preview_artifacts
               (image_hash, kind, source, width, height, bytes, format,
                needs_full_decode, generator_version, generated_at)
             VALUES (?1, ?2, 'original', 16, 16, 16, 'webp', 0, 1,
                     '2026-01-01T00:00:00.000Z')
             ON CONFLICT(image_hash, kind) DO NOTHING",
            params![hash.as_str(), kind],
        )
        .unwrap();
    }
    put_artifacts(cache, hash)
}

fn upsert_image_vector(store: &PpvecStore, kind: VecKind, hash: &ContentHash) {
    upsert_image_vector_model(store, kind, hash, "fixture-model");
}

fn upsert_image_vector_model(store: &PpvecStore, kind: VecKind, hash: &ContentHash, model: &str) {
    store
        .upsert(
            VecKey {
                space: VecSpace {
                    vec_kind: kind,
                    model_id: model.into(),
                },
                unit: VecUnit::Image {
                    image_hash: hash.to_string(),
                },
            },
            &Embedding {
                vector: vec![0.5, 0.25, -0.25, -0.5],
                model_id: model.into(),
            },
        )
        .unwrap();
}

fn upsert_annotation_vector(store: &PpvecStore, event_id: &str, model: &str) {
    store
        .upsert(
            VecKey {
                space: VecSpace {
                    vec_kind: VecKind::AnnotationChunk,
                    model_id: model.into(),
                },
                unit: VecUnit::AnnotationChunk {
                    event_id: event_id.into(),
                    chunk_index: 0,
                },
            },
            &Embedding {
                vector: vec![0.5, 0.25, -0.25, -0.5],
                model_id: model.into(),
            },
        )
        .unwrap();
}

fn insert_image_summary(conn: &Connection, hash: &ContentHash, id: &str, text: &str) {
    conn.execute(
        "INSERT INTO derived_summaries
           (id, scope, scope_key, text, model_id, prompt_ver, inputs_hash, generated_ts)
         VALUES (?1, 'image', ?2, ?3, 'summary-generator', 1, 'summary-inputs',
                 '2026-01-02T00:00:00.000Z')",
        params![id, hash.as_str(), text],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO summaries_fts(text, summary_id) VALUES (?1, ?2)",
        params![text, id],
    )
    .unwrap();
}

#[test]
fn retention_classifies_protects_reclaims_and_revives() {
    let tmp = tempfile::tempdir().unwrap();
    let mount = tmp.path().join("mount");
    let root_dir = mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let db = tmp.path().join("photoproof.db");
    let cache = tmp.path().join("cache");
    drop(EventStore::open(&db).unwrap());

    let probe = FakeVolumeProbe::new();
    probe.set_mounts(vec![probed(&mount)]);
    let lib = Arc::new(
        Library::open_with(
            &db,
            &cache,
            LibraryOptions {
                probe: Arc::new(probe),
                ..LibraryOptions::default()
            },
        )
        .unwrap(),
    );
    let root_id = lib.register_root(&root_dir, Some("photos")).unwrap();
    let volume_id = lib.roots().unwrap()[0].volume_id.clone();
    let now = UtcMillis::parse("2026-07-26T12:00:00.000Z").unwrap();
    let old = UtcMillis::from_epoch_ms(now.epoch_ms() - 31 * 86_400_000).to_rfc3339();
    let boundary = UtcMillis::from_epoch_ms(now.epoch_ms() - 30 * 86_400_000).to_rfc3339();
    let recent = UtcMillis::from_epoch_ms(now.epoch_ms() - 29 * 86_400_000).to_rfc3339();

    // One realistic image goes through scan so its post-reclaim relink can
    // prove the public lifecycle seam revives retired work.
    let old_bytes = b"old retained identity";
    std::fs::write(root_dir.join("old.jpg"), old_bytes).unwrap();
    lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
    let old_hash = ContentHash::from_bytes_of(old_bytes);
    std::fs::remove_file(root_dir.join("old.jpg")).unwrap();
    lib.scan_root(&root_id, &ScanOptions::default()).unwrap();

    let recent_hash = ContentHash::from_bytes_of(b"recent");
    let boundary_hash = ContentHash::from_bytes_of(b"boundary");
    let unknown_hash = ContentHash::from_bytes_of(b"unknown");
    let relinked_hash = ContentHash::from_bytes_of(b"relinked-before-sweep");
    let busy_hash = ContentHash::from_bytes_of(b"busy-running-pass");
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE paths SET stale_since = ?2 WHERE image_hash = ?1",
        params![old_hash.as_str(), old],
    )
    .unwrap();
    let mut old_files = seed_image(
        &conn,
        &cache,
        &old_hash,
        "old-extra-path",
        &volume_id,
        &root_id,
        "photos/old-copy.jpg",
        "stale",
        Some(&old),
    );
    let legacy_full = old_files[0]
        .parent()
        .unwrap()
        .join(format!("{}-full-v1.webp", old_hash));
    std::fs::write(&legacy_full, [9u8; 8]).unwrap();
    old_files.push(legacy_full);
    let recent_files = seed_image(
        &conn,
        &cache,
        &recent_hash,
        "recent-path",
        &volume_id,
        &root_id,
        "photos/recent.jpg",
        "stale",
        Some(&recent),
    );
    let boundary_files = seed_image(
        &conn,
        &cache,
        &boundary_hash,
        "boundary-path",
        &volume_id,
        &root_id,
        "photos/boundary.jpg",
        "stale",
        Some(&boundary),
    );
    let unknown_files = seed_image(
        &conn,
        &cache,
        &unknown_hash,
        "unknown-path",
        &volume_id,
        &root_id,
        "photos/unknown.jpg",
        "stale",
        None,
    );
    let relinked_files = seed_image(
        &conn,
        &cache,
        &relinked_hash,
        "relinked-stale-path",
        &volume_id,
        &root_id,
        "photos/relinked-old.jpg",
        "stale",
        Some(&old),
    );
    let busy_files = seed_image(
        &conn,
        &cache,
        &busy_hash,
        "busy-path",
        &volume_id,
        &root_id,
        "photos/busy.jpg",
        "stale",
        Some(&old),
    );
    conn.execute(
        "UPDATE ingest_passes SET state = 'running'
         WHERE image_hash = ?1 AND pass_name = 'preview'",
        [busy_hash.as_str()],
    )
    .unwrap();

    let store = EventStore::open(&db).unwrap();
    let session = store
        .open_session(SessionContext {
            app_version: "retention-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        })
        .unwrap();
    let orphan_only_event = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "authored orphan-only words".into(),
                targets: vec![old_hash.clone()],
            },
            None,
        )
        .unwrap();
    let shared_event = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "authored words shared with a recent orphan".into(),
                targets: vec![old_hash.clone(), recent_hash.clone()],
            },
            None,
        )
        .unwrap();

    let vectors_dir = tmp.path().join("vectors");
    let vectors = PpvecStore::open(&db, &vectors_dir).unwrap();
    for hash in [
        &old_hash,
        &recent_hash,
        &boundary_hash,
        &unknown_hash,
        &relinked_hash,
        &busy_hash,
    ] {
        upsert_image_vector(&vectors, VecKind::ImageClip, hash);
    }
    let summary_text = "retained rolling summary text";
    insert_image_summary(
        &conn,
        &old_hash,
        "01JRETENTIONIMAGE00000000000",
        summary_text,
    );
    for hash in [
        &old_hash,
        &recent_hash,
        &unknown_hash,
        &relinked_hash,
        &busy_hash,
    ] {
        upsert_image_vector(&vectors, VecKind::ImageSummary, hash);
    }
    upsert_annotation_vector(&vectors, orphan_only_event.id.as_str(), "fixture-model");
    upsert_annotation_vector(&vectors, shared_event.id.as_str(), "fixture-model");
    upsert_image_vector_model(&vectors, VecKind::ImageClip, &old_hash, "orphan-only");
    let orphan_only_space = VecSpace {
        vec_kind: VecKind::ImageClip,
        model_id: "orphan-only".into(),
    };
    let orphan_only_file = vectors.file_path(&orphan_only_space);
    assert!(orphan_only_file.exists());

    let dry = lib
        .doctor_with_retention(now, OrphanRetentionMode::ReportOnly)
        .unwrap();
    assert_eq!(dry.orphan_images, 6);
    assert_eq!(
        dry.retention_eligible, 3,
        "the exact 30-day boundary qualifies"
    );
    assert_eq!(dry.retention_deferred_recent, 1);
    assert_eq!(dry.retention_deferred_unknown_timestamp, 1);
    assert_eq!(dry.retention_deferred_busy, 1);
    assert_eq!(dry.journal_vector_rows_retained, 2);
    assert!(dry.retention_dry_run);
    assert_eq!(dry.reclaimed_images, 0);
    assert!(old_files.iter().all(|p| p.exists()));

    // A relink between the dry candidate snapshot and the destructive pass is
    // authoritative active-path protection, even though its stale tombstone
    // remains older than the boundary.
    conn.execute(
        "INSERT INTO paths
           (path_id, image_hash, volume_id, root_id, rel_path, size, mtime_ns,
            state, stale_reason, first_seen_at, last_verified_at, stale_since)
         VALUES ('relinked-active-path', ?1, ?2, ?3, 'photos/relinked-new.jpg',
                 3, 1, 'active', NULL, ?4, ?4, NULL)",
        params![relinked_hash.as_str(), volume_id, root_id, now.to_rfc3339()],
    )
    .unwrap();

    let reclaimed = lib
        .doctor_with_retention(now, OrphanRetentionMode::Reclaim)
        .unwrap();
    assert_eq!(reclaimed.orphan_images, 5);
    assert_eq!(reclaimed.retention_eligible, 2);
    assert_eq!(reclaimed.retention_deferred_busy, 1);
    assert_eq!(reclaimed.reclaimed_images, 2);
    assert_eq!(reclaimed.preview_rows_reclaimed, 4);
    assert_eq!(reclaimed.preview_files_reclaimed, 7);
    assert_eq!(reclaimed.preview_bytes_reclaimed, 64);
    assert_eq!(reclaimed.vector_rows_reclaimed, 5);
    assert_eq!(reclaimed.vector_spaces_compacted, 4);
    assert_eq!(reclaimed.journal_vector_rows_retained, 0);
    assert!(old_files.iter().all(|p| !p.exists()));
    assert!(boundary_files.iter().all(|p| !p.exists()));
    assert!(recent_files.iter().all(|p| p.exists()));
    assert!(unknown_files.iter().all(|p| p.exists()));
    assert!(relinked_files.iter().all(|p| p.exists()));
    assert!(busy_files.iter().all(|p| p.exists()));
    assert!(
        !orphan_only_file.exists(),
        "a space with no surviving rows does not leak a header-only ppvec"
    );

    let (clip_live, clip_total) = vectors
        .space_stats(&VecSpace {
            vec_kind: VecKind::ImageClip,
            model_id: "fixture-model".into(),
        })
        .unwrap();
    assert_eq!((clip_live, clip_total), (4, 4), "dead row compacted");
    let (summary_live, summary_total) = vectors
        .space_stats(&VecSpace {
            vec_kind: VecKind::ImageSummary,
            model_id: "fixture-model".into(),
        })
        .unwrap();
    assert_eq!(
        (summary_live, summary_total),
        (4, 4),
        "active, recent, timestamp-unknown, and busy summary vectors survive"
    );
    let retained_summary_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM derived_summaries ds
             JOIN summaries_fts sf ON sf.summary_id = ds.id
             WHERE ds.scope = 'image' AND ds.scope_key = ?1 AND ds.text = ?2",
            params![old_hash.as_str(), summary_text],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        retained_summary_rows, 1,
        "summary text and its sparse index remain durable rebuild input"
    );
    let (annotation_live, annotation_total) = vectors
        .space_stats(&VecSpace {
            vec_kind: VecKind::AnnotationChunk,
            model_id: "fixture-model".into(),
        })
        .unwrap();
    assert_eq!(
        (annotation_live, annotation_total),
        (1, 1),
        "the orphan-only chunk is reclaimed, while the recent sibling target \
         protects its shared authored-text chunk"
    );
    let authored = store.folded_journal(&old_hash).unwrap();
    assert_eq!(
        authored.len(),
        2,
        "retention never removes authored journal/search truth"
    );
    let old_preview_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM preview_artifacts WHERE image_hash = ?1",
            [old_hash.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_preview_rows, 0);
    let old_paths: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM paths WHERE image_hash = ?1 AND state = 'stale'",
            [old_hash.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_paths, 2, "path tombstones are retained");

    let again = lib
        .doctor_with_retention(now, OrphanRetentionMode::Reclaim)
        .unwrap();
    assert_eq!(again.retention_eligible, 2, "old tombstones stay visible");
    assert_eq!(again.reclaimed_images, 0, "cleanup is idempotent");
    assert_eq!(again.preview_rows_reclaimed, 0);
    assert_eq!(again.vector_rows_reclaimed, 0);

    // Reappearance of the same bytes relinks to the existing identity and
    // revives exactly the retention-cleaned preview/CLIP work.
    std::fs::write(root_dir.join("old.jpg"), old_bytes).unwrap();
    let relink = lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
    assert_eq!(relink.relinked, 1);
    assert_eq!(
        relink.retention_repairs_revived, 3,
        "repair health reports every retention-cleaned pass revived by relink"
    );
    let revived: Vec<(String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT pass_name, state, error FROM ingest_passes
                 WHERE image_hash = ?1
                   AND pass_name IN ('preview', 'image-embedding', 'text-embedding')
                 ORDER BY pass_name",
            )
            .unwrap();
        stmt.query_map([old_hash.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    assert_eq!(
        revived,
        vec![
            ("image-embedding".into(), "pending".into(), None),
            ("preview".into(), "pending".into(), None),
            ("text-embedding".into(), "pending".into(), None),
        ]
    );

    let text_embedder = MockEmbedder::text("fixture-model", 4);
    let text_rig: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: Some(&text_embedder),
        clip: None,
        vectors: &vectors,
    };
    lib.process_embedding_queue(&text_rig, &QueueOptions::default())
        .unwrap();
    let summary_key = VecKey {
        space: VecSpace {
            vec_kind: VecKind::ImageSummary,
            model_id: "fixture-model".into(),
        },
        unit: VecUnit::Image {
            image_hash: old_hash.to_string(),
        },
    };
    assert_eq!(
        vectors.row_inputs_hash(&summary_key).unwrap(),
        Some((inputs_hash(summary_text.as_bytes()), false)),
        "relink rebuilds the dense summary row directly from retained text"
    );
}

#[test]
fn interrupted_text_vector_reclamation_self_heals_on_retry_and_relink() {
    let tmp = tempfile::tempdir().unwrap();
    let mount = tmp.path().join("mount");
    let root_dir = mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let db = tmp.path().join("photoproof.db");
    let cache = tmp.path().join("cache");
    let store = EventStore::open(&db).unwrap();

    let probe = FakeVolumeProbe::new();
    probe.set_mounts(vec![probed(&mount)]);
    let lib = Library::open_with(
        &db,
        &cache,
        LibraryOptions {
            probe: Arc::new(probe),
            ..LibraryOptions::default()
        },
    )
    .unwrap();
    let root_id = lib.register_root(&root_dir, Some("photos")).unwrap();
    let volume_id = lib.roots().unwrap()[0].volume_id.clone();
    let now = UtcMillis::parse("2026-07-26T12:00:00.000Z").unwrap();
    let old = UtcMillis::from_epoch_ms(now.epoch_ms() - 31 * 86_400_000).to_rfc3339();
    let bytes = b"crash-recoverable authored vector";
    let hash = ContentHash::from_bytes_of(bytes);
    let conn = Connection::open(&db).unwrap();
    seed_image(
        &conn,
        &cache,
        &hash,
        "crash-path",
        &volume_id,
        &root_id,
        "photos/crash.jpg",
        "stale",
        Some(&old),
    );

    let session = store
        .open_session(SessionContext {
            app_version: "retention-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        })
        .unwrap();
    let event = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "the authored words survive derived cleanup".into(),
                targets: vec![hash.clone()],
            },
            None,
        )
        .unwrap();
    let vectors_dir = tmp.path().join("vectors");
    let vectors = PpvecStore::open(&db, &vectors_dir).unwrap();
    let summary_text = "retained summary survives interrupted cleanup";
    insert_image_summary(&conn, &hash, "01JRETENTIONCRASH0000000000", summary_text);
    upsert_annotation_vector(&vectors, event.id.as_str(), "fixture-model");
    upsert_image_vector(&vectors, VecKind::ImageSummary, &hash);
    let space = VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: "fixture-model".into(),
    };
    let summary_space = VecSpace {
        vec_kind: VecKind::ImageSummary,
        model_id: "fixture-model".into(),
    };
    let vector_file = vectors.file_path(&space);
    let summary_file = vectors.file_path(&summary_space);

    // Exact state after the retention metadata transaction committed but
    // before PPVEC sweep/compaction ran: the row is authoritatively dead, its
    // bytes/file still exist, and relink work is durably retired.
    conn.execute(
        "UPDATE vectors SET deleted = 1
         WHERE (vec_kind = 'annotation_chunk' AND event_id = ?1)
            OR (vec_kind = 'image_summary' AND image_hash = ?2)",
        params![event.id.as_str(), hash.as_str()],
    )
    .unwrap();
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'skipped', error = 'orphan-retention'
         WHERE image_hash = ?1 AND pass_name = 'text-embedding'",
        [hash.as_str()],
    )
    .unwrap();
    assert_eq!(vectors.space_stats(&space).unwrap(), (0, 1));
    assert_eq!(vectors.space_stats(&summary_space).unwrap(), (0, 1));
    assert!(vector_file.exists());
    assert!(summary_file.exists());

    let recovered = lib
        .doctor_with_retention(now, OrphanRetentionMode::Reclaim)
        .unwrap();
    assert_eq!(
        recovered.vector_rows_reclaimed, 0,
        "the interrupted transaction had already marked the row"
    );
    assert_eq!(
        recovered.vector_spaces_compacted, 2,
        "retry discovers and finishes both already-dead text spaces"
    );
    assert_eq!(vectors.space_stats(&space).unwrap(), (0, 0));
    assert_eq!(vectors.space_stats(&summary_space).unwrap(), (0, 0));
    assert!(!vector_file.exists());
    assert!(!summary_file.exists());
    assert_eq!(
        store.folded_journal(&hash).unwrap().len(),
        1,
        "authored truth survives the interrupted and recovered cleanup"
    );
    let retained_summary: String = conn
        .query_row(
            "SELECT text FROM derived_summaries
             WHERE scope = 'image' AND scope_key = ?1",
            [hash.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained_summary, summary_text);

    // The other filesystem interruption point: compaction metadata/file-row
    // deletion completed, but the final header-only orphan-file removal did
    // not. With no metadata row left, retry must still sweep the file.
    upsert_annotation_vector(&vectors, event.id.as_str(), "header-only-crash");
    let orphan_space = VecSpace {
        vec_kind: VecKind::AnnotationChunk,
        model_id: "header-only-crash".into(),
    };
    let orphan_file = vectors.file_path(&orphan_space);
    conn.execute(
        "DELETE FROM vectors
         WHERE vec_kind = 'annotation_chunk' AND model_id = 'header-only-crash'",
        [],
    )
    .unwrap();
    assert!(orphan_file.exists());

    let again = lib
        .doctor_with_retention(now, OrphanRetentionMode::Reclaim)
        .unwrap();
    assert_eq!(again.vector_rows_reclaimed, 0);
    assert_eq!(
        again.vector_spaces_compacted, 0,
        "the recovery itself is idempotent"
    );
    assert!(
        !orphan_file.exists(),
        "retry removes an orphan file left after interrupted compaction"
    );

    std::fs::write(root_dir.join("crash.jpg"), bytes).unwrap();
    lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
    let text_state: (String, Option<String>) = conn
        .query_row(
            "SELECT state, error FROM ingest_passes
             WHERE image_hash = ?1 AND pass_name = 'text-embedding'",
            [hash.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        text_state,
        ("pending".into(), None),
        "relink re-pends the authored-text rebuild after interrupted cleanup"
    );

    let text_embedder = MockEmbedder::text("fixture-model", 4);
    let rig: EmbeddingRig<'_, MockEmbedder, MockEmbedder> = EmbeddingRig {
        text: Some(&text_embedder),
        clip: None,
        vectors: &vectors,
    };
    lib.process_embedding_queue(&rig, &QueueOptions::default())
        .unwrap();
    assert_eq!(
        vectors.space_stats(&space).unwrap(),
        (1, 1),
        "the relink-repended pass rebuilds the reclaimed annotation vector"
    );
    assert_eq!(
        vectors.space_stats(&summary_space).unwrap(),
        (1, 1),
        "the same pass rebuilds the reclaimed summary vector from retained text"
    );
    let rebuilt_state: (String, Option<String>) = conn
        .query_row(
            "SELECT state, error FROM ingest_passes
             WHERE image_hash = ?1 AND pass_name = 'text-embedding'",
            [hash.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rebuilt_state, ("done".into(), None));
}
