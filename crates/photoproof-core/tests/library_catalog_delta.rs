use photoproof_core::library::{Library, QueueOptions};
use rusqlite::{Connection, params};

struct Fixture {
    _tmp: tempfile::TempDir,
    lib: Library,
    conn: Connection,
    hash: String,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("photoproof.db");
    let lib = Library::open(&db, tmp.path().join("previews")).unwrap();
    let conn = Connection::open(&db).unwrap();
    let hash = "ab".repeat(32);
    conn.execute_batch(
        "INSERT INTO volumes
           (volume_id, state, first_seen_at, last_seen_at)
         VALUES ('v1', 'online', '2026-01-01T00:00:00.000Z',
                 '2026-01-01T00:00:00.000Z');
         INSERT INTO roots
           (root_id, volume_id, rel_path, display_name, state, created_at)
         VALUES ('r1', 'v1', 'photos', 'Photos', 'active',
                 '2026-01-01T00:00:00.000Z');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO images
           (image_hash, byte_size, format, first_ingested_at)
         VALUES (?1, 1, 'jpeg', '2026-01-01T00:00:00.000Z')",
        [&hash],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO paths
           (path_id, image_hash, volume_id, root_id, rel_path, size, mtime_ns,
            state, first_seen_at, last_verified_at)
         VALUES ('p1', ?1, 'v1', 'r1', 'photos/a.jpg', 1, 1, 'active',
                 '2026-01-01T00:00:00.000Z',
                 '2026-01-01T00:00:00.000Z')",
        [&hash],
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        lib,
        conn,
        hash,
    }
}

#[test]
fn folder_delta_catches_up_missed_changes_and_resolves_current_state() {
    let f = fixture();
    let initial = f.lib.list_folder_delta("r1", "", 0).unwrap();
    assert!(initial.reset);
    assert_eq!(initial.upserts.len(), 1);
    assert!(initial.removed_hashes.is_empty());

    // Several backend changes can land before the UI observes a notification.
    // They coalesce to one current GridItem-compatible upsert.
    f.conn
        .execute(
            "UPDATE images SET capture_ts = '2025-12-31T23:00:00.000Z'
             WHERE image_hash = ?1",
            [&f.hash],
        )
        .unwrap();
    f.conn
        .execute(
            "INSERT INTO image_ratings(image_hash, rating, event_id, ts)
             VALUES (?1, 4, 'rating-event', '2026-01-01T00:01:00.000Z')",
            [&f.hash],
        )
        .unwrap();
    f.conn
        .execute(
            "INSERT INTO preview_artifacts
               (image_hash, kind, source, width, height, bytes, format,
                needs_full_decode, generator_version, generated_at)
             VALUES (?1, 'thumb', 'original', 512, 341, 100, 'webp', 0, 1,
                     '2026-01-01T00:02:00.000Z')",
            [&f.hash],
        )
        .unwrap();

    let caught_up = f
        .lib
        .list_folder_delta("r1", "", initial.to_revision)
        .unwrap();
    assert!(!caught_up.reset);
    assert!(caught_up.to_revision > initial.to_revision);
    assert_eq!(caught_up.upserts.len(), 1);
    assert_eq!(caught_up.upserts[0].rating, Some(4));
    assert!(caught_up.upserts[0].preview_ready);
    assert_eq!(
        caught_up.upserts[0].capture_ts.as_deref(),
        Some("2025-12-31T23:00:00.000Z")
    );
    assert!(caught_up.removed_hashes.is_empty());

    // A move produces a removal in the old scope and an upsert in the new one
    // from the same revision.
    f.conn
        .execute(
            "UPDATE paths SET rel_path = 'photos/trip/a.jpg' WHERE path_id = 'p1'",
            [],
        )
        .unwrap();
    let old_folder = f
        .lib
        .list_folder_delta("r1", "", caught_up.to_revision)
        .unwrap();
    assert!(old_folder.upserts.is_empty());
    assert_eq!(old_folder.removed_hashes[0].as_str(), f.hash);

    let new_folder = f
        .lib
        .list_folder_delta("r1", "trip", caught_up.to_revision)
        .unwrap();
    assert_eq!(new_folder.upserts.len(), 1);
    assert!(new_folder.removed_hashes.is_empty());
}

#[test]
fn folder_delta_falls_back_for_compacted_or_future_revision() {
    let f = fixture();
    let initial = f.lib.list_folder_delta("r1", "", 0).unwrap();

    // Simulate maintenance having compacted every change older than the clock.
    f.conn
        .execute_batch(
            "DELETE FROM folder_change_log;
             UPDATE folder_change_clock SET revision = revision + 1
             WHERE singleton = 1;",
        )
        .unwrap();
    let compacted = f
        .lib
        .list_folder_delta("r1", "", initial.to_revision)
        .unwrap();
    assert!(compacted.reset);
    assert_eq!(compacted.upserts.len(), 1);

    let future = f
        .lib
        .list_folder_delta("r1", "", compacted.to_revision + 100)
        .unwrap();
    assert!(future.reset);
    assert_eq!(future.upserts.len(), 1);
}

#[test]
fn folder_delta_is_scoped_to_direct_children() {
    let f = fixture();
    let initial = f.lib.list_folder_delta("r1", "", 0).unwrap();
    f.conn
        .execute(
            "UPDATE paths SET rel_path = 'photos/trip/a.jpg' WHERE path_id = 'p1'",
            [],
        )
        .unwrap();
    let settled = f
        .lib
        .list_folder_delta("r1", "trip", initial.to_revision)
        .unwrap();

    f.conn
        .execute(
            "UPDATE images SET capture_ts = '2026-02-01T00:00:00.000Z'
             WHERE image_hash = ?1",
            params![f.hash],
        )
        .unwrap();
    let root = f
        .lib
        .list_folder_delta("r1", "", settled.to_revision)
        .unwrap();
    assert!(root.upserts.is_empty());
    assert!(root.removed_hashes.is_empty());

    let trip = f
        .lib
        .list_folder_delta("r1", "trip", settled.to_revision)
        .unwrap();
    assert_eq!(trip.upserts.len(), 1);
}

#[test]
fn hot_catalog_paths_publish_separate_fixed_label_wait_and_operation_series() {
    let f = fixture();
    f.lib.list_folder("r1", "").unwrap();
    f.lib.list_folder_delta("r1", "", 0).unwrap();
    f.lib.active_pass_counters().unwrap();
    f.lib
        .process_queue(&QueueOptions {
            max_items: Some(1),
            ..QueueOptions::default()
        })
        .unwrap();

    let metrics = f.lib.catalog_metrics_snapshot();
    assert_eq!(metrics.len(), 8);
    for pair in [
        ("activity.wait", "activity.operation"),
        ("folder_list.wait", "folder_list.operation"),
        ("folder_delta.wait", "folder_delta.operation"),
        ("queue_claim.wait", "queue_claim.operation"),
    ] {
        let wait = metrics
            .iter()
            .find(|stage| stage.stage == pair.0)
            .expect("wait series");
        let operation = metrics
            .iter()
            .find(|stage| stage.stage == pair.1)
            .expect("operation series");
        assert_eq!(wait.count, 1, "{} count", pair.0);
        assert_eq!(operation.count, 1, "{} count", pair.1);
        for value in [
            wait.p50_ms,
            wait.p95_ms,
            wait.p99_ms,
            wait.max_ms,
            operation.p50_ms,
            operation.p95_ms,
            operation.p99_ms,
            operation.max_ms,
        ] {
            assert!(value.is_finite() && value >= 0.0);
        }
    }
    assert!(
        metrics.iter().all(|stage| !stage.stage.contains("photos")
            && !stage.stage.contains("r1")
            && !stage.stage.contains(&f.hash)),
        "metric labels never contain user-derived scope or identity"
    );
}
