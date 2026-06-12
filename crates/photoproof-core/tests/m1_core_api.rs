//! P4.1 A1–A4: the core APIs the desktop shell consumes instead of its
//! P3.2 read-only-connection workarounds (gridq.rs / locator.rs / Box::leak).
//!
//! - `EventStore::open_sessions` / `last_event_ts` (CAPTURE §2.4 recovery)
//! - `EventStore::journal_stats` batched read + the B34 `has_text` flag
//! - `Library::list_folder` / `Library::folder_tree` batched grid reads
//! - `Library: ImageLocator` (DECISIONS B29's stated home)
//! - `SidecarEngine::new_shared` (Arc store, no `Box::leak`)

mod common;

use std::sync::Arc;

use photoproof_core::library::Library;
use photoproof_core::sidecar::{AdjacentLocation, ImageLocator, SidecarEngine};
use photoproof_core::{ContentHash, EventStore, JournalStats, SessionContext, UtcMillis};

use common::{DEVICE_ID, d_rating, d_remark, d_retract, d_stroke, hash, new_store};

// ---------------------------------------------------------------------------
// Library-row seeding (the read side under test; the write side is ingest,
// which needs files on disk and is exercised by the m1_e2e_* suites)
// ---------------------------------------------------------------------------

fn seed_volume(conn: &rusqlite::Connection, volume_id: &str, online: bool, read_only: bool) {
    conn.execute(
        "INSERT INTO volumes (volume_id, state, mount_point, read_only, first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![
            volume_id,
            if online { "online" } else { "offline" },
            if online { Some("/mnt/test") } else { None },
            read_only as i64,
        ],
    )
    .unwrap();
}

fn seed_root(conn: &rusqlite::Connection, root_id: &str, volume_id: &str, rel_path: &str) {
    conn.execute(
        "INSERT INTO roots (root_id, volume_id, rel_path, display_name, state, created_at)
         VALUES (?1, ?2, ?3, 'Test', 'active', '2026-01-01T00:00:00Z')",
        rusqlite::params![root_id, volume_id, rel_path],
    )
    .unwrap();
}

fn seed_image(conn: &rusqlite::Connection, hash: &ContentHash, capture_ts: Option<&str>) {
    conn.execute(
        "INSERT INTO images (image_hash, byte_size, format, first_ingested_at, capture_ts)
         VALUES (?1, 1000, 'jpeg', '2026-02-01T00:00:00Z', ?2)",
        rusqlite::params![hash.as_str(), capture_ts],
    )
    .unwrap();
}

fn seed_path(
    conn: &rusqlite::Connection,
    path_id: &str,
    hash: &ContentHash,
    volume_id: &str,
    root_id: &str,
    rel_path: &str,
) {
    conn.execute(
        "INSERT INTO paths (path_id, image_hash, volume_id, root_id, rel_path, size,
                            mtime_ns, state, first_seen_at, last_verified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1000, 0, 'active',
                 '2026-02-01T00:00:00Z', '2026-02-01T00:00:00Z')",
        rusqlite::params![path_id, hash.as_str(), volume_id, root_id, rel_path],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// A1 — open_sessions / last_event_ts (crash-recovery reads)
// ---------------------------------------------------------------------------

#[test]
fn a1_open_sessions_and_last_event_ts_support_crash_recovery() {
    let t = new_store();
    // The harness session is open.
    let open = t.store.open_sessions().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, t.session);
    assert!(open[0].ended_ts.is_none());

    // No events yet: last_event_ts is None (caller falls back to started_ts).
    assert_eq!(t.store.last_event_ts(&t.session).unwrap(), None);

    let ev = t
        .store
        .append(&t.session, d_remark("note", vec![hash(1)]), None)
        .unwrap();
    assert_eq!(t.store.last_event_ts(&t.session).unwrap(), Some(ev.ts));

    // Recovery closes at the last event's ts; open_sessions then drains.
    t.store.close_session(&t.session, ev.ts).unwrap();
    assert!(t.store.open_sessions().unwrap().is_empty());

    // Multiple dead sessions come back in ascending id order.
    let s1 = t
        .store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .unwrap();
    let s2 = t
        .store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .unwrap();
    let ids: Vec<_> = t
        .store
        .open_sessions()
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, vec![s1, s2]);
}

// ---------------------------------------------------------------------------
// A1 — journal_stats batched read + B34 has_text
// ---------------------------------------------------------------------------

fn stats_for<'a>(stats: &'a [JournalStats], h: &ContentHash) -> Option<&'a JournalStats> {
    stats.iter().find(|s| s.image == *h)
}

#[test]
fn a1_journal_stats_is_batched_and_rating_only_has_no_text() {
    let t = new_store();
    let (worded, rated, stroked, untouched) = (hash(1), hash(2), hash(3), hash(4));
    t.store
        .append(&t.session, d_remark("the hand", vec![worded.clone()]), None)
        .unwrap();
    t.store
        .append(&t.session, d_rating(4, vec![rated.clone()]), None)
        .unwrap();
    t.store
        .append(&t.session, d_stroke(stroked.clone(), None), None)
        .unwrap();

    let all = [
        worded.clone(),
        rated.clone(),
        stroked.clone(),
        untouched.clone(),
    ];
    let stats = t.store.journal_stats(&all).unwrap();
    assert_eq!(stats.len(), 3, "untouched image has no row");
    assert!(stats_for(&stats, &untouched).is_none());

    let w = stats_for(&stats, &worded).unwrap();
    assert!(w.has_text && !w.has_strokes && w.has_journal());
    assert_eq!(w.event_count, 1);

    // B34/UI §3.7: a rating-only journal must not light the has-journal dot.
    let r = stats_for(&stats, &rated).unwrap();
    assert!(!r.has_text && !r.has_strokes && !r.has_journal());
    assert_eq!(r.event_count, 1, "the rating still counts as a live event");

    let s = stats_for(&stats, &stroked).unwrap();
    assert!(!s.has_text && s.has_strokes && s.has_journal());

    // Ascending image-hash order.
    let mut sorted = stats.clone();
    sorted.sort_by(|a, b| a.image.as_str().cmp(b.image.as_str()));
    assert_eq!(stats, sorted);
}

#[test]
fn a1_has_text_clears_when_all_remarks_retract_but_rating_survives() {
    // The exact edge the shell's EXISTS workaround got wrong (flagged in
    // gridq.rs): all remarks retracted, image still rated → no dot.
    let t = new_store();
    let img = hash(10);
    let remark = t
        .store
        .append(&t.session, d_remark("temporary", vec![img.clone()]), None)
        .unwrap();
    t.store
        .append(&t.session, d_rating(3, vec![img.clone()]), None)
        .unwrap();
    t.store
        .append(&t.session, d_retract(remark.id.clone()), None)
        .unwrap();

    let stats = t.store.journal_stats(std::slice::from_ref(&img)).unwrap();
    let s = stats_for(&stats, &img).unwrap();
    assert!(!s.has_text, "retracted remark is not words evidence");
    assert!(!s.has_journal());
    assert_eq!(s.event_count, 1, "only the rating is live");
    assert_eq!(t.store.current_rating(&img).unwrap(), Some(3));
}

#[test]
fn a1_has_text_clears_on_redaction_and_i7_rebuild_agrees() {
    let t = new_store();
    let img = hash(11);
    let remark = t
        .store
        .append(&t.session, d_remark("secret", vec![img.clone()]), None)
        .unwrap();
    t.store
        .append(&t.session, d_stroke(img.clone(), None), None)
        .unwrap();
    t.store.redact(&remark.id).unwrap();

    let read = || {
        let stats = t.store.journal_stats(std::slice::from_ref(&img)).unwrap();
        stats_for(&stats, &img).unwrap().clone()
    };
    let s = read();
    assert!(
        !s.has_text,
        "a redacted remark is a [redacted] stub (Q2), not words evidence"
    );
    assert!(s.has_strokes && s.has_journal());
    assert_eq!(s.event_count, 2, "the scrubbed stub still counts (B4)");

    // I7: incremental maintenance equals from-scratch recomputation.
    t.store.rebuild_derived().unwrap();
    assert_eq!(read(), s);
}

// ---------------------------------------------------------------------------
// A2 — Library::list_folder / folder_tree
// ---------------------------------------------------------------------------

struct LibFixture {
    _dir: tempfile::TempDir,
    store: EventStore,
    session: photoproof_core::SessionId,
    lib: Library,
    conn: rusqlite::Connection,
}

fn lib_fixture() -> LibFixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("photoproof.db");
    let store = EventStore::open(&db).unwrap();
    let session = store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .unwrap();
    let lib = Library::open(&db, dir.path().join("cache")).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .unwrap();
    LibFixture {
        _dir: dir,
        store,
        session,
        lib,
        conn,
    }
}

#[test]
fn a2_list_folder_returns_direct_children_with_badges() {
    let f = lib_fixture();
    seed_volume(&f.conn, "vol1", true, false);
    seed_root(&f.conn, "root1", "vol1", "photos");
    let (a, b, c) = (hash(1), hash(2), hash(3));
    for (i, h) in [&a, &b, &c].iter().enumerate() {
        seed_image(&f.conn, h, Some("2026-03-01T00:00:00Z"));
        seed_path(
            &f.conn,
            &format!("p{i}"),
            h,
            "vol1",
            "root1",
            &format!("photos/iceland/img_{i}.jpg"),
        );
    }
    // A nested image must NOT appear in the folder's direct listing.
    let nested = hash(4);
    seed_image(&f.conn, &nested, None);
    seed_path(
        &f.conn,
        "p9",
        &nested,
        "vol1",
        "root1",
        "photos/iceland/sub/deep.jpg",
    );

    // Journal + rating (derived folds maintained by append).
    f.store
        .append(
            &f.session,
            d_remark("the hand is the whole picture", vec![a.clone()]),
            None,
        )
        .unwrap();
    f.store
        .append(&f.session, d_rating(4, vec![b.clone()]), None)
        .unwrap();

    let items = f.lib.list_folder("root1", "iceland").unwrap();
    assert_eq!(items.len(), 3, "direct children only");
    let find = |h: &ContentHash| items.iter().find(|i| i.hash == *h).unwrap();
    assert!(find(&a).has_journal);
    assert_eq!(find(&a).rating, None);
    // UI §3.7 / B34: rating keys cause no visual change to any thumbnail —
    // a rating-only journal must not light the has-journal dot.
    assert!(!find(&b).has_journal);
    assert_eq!(find(&b).rating, Some(4));
    assert!(!find(&c).has_journal);
    assert_eq!(find(&c).rating, None);
    assert_eq!(find(&a).rel_path, "iceland/img_0.jpg");
    assert_eq!(find(&a).file_name, "img_0.jpg");
    assert_eq!(find(&a).capture_ts.as_deref(), Some("2026-03-01T00:00:00Z"));
    assert_eq!(find(&a).first_ingested_at, "2026-02-01T00:00:00Z");
    assert!(!find(&a).offline);

    // Unknown root errors instead of returning an empty grid.
    assert!(f.lib.list_folder("no-such-root", "").is_err());
}

#[test]
fn list_images_returns_badge_rows_in_input_order_and_skips_unknown_hashes() {
    // The collection-members grid read (RETRIEVAL §10, rail Collections
    // tab): same badge shape as list_folder, keyed by explicit hashes.
    let f = lib_fixture();
    seed_volume(&f.conn, "vol1", true, false);
    seed_root(&f.conn, "root1", "vol1", "photos");
    let (a, b) = (hash(40), hash(41));
    for (i, h) in [&a, &b].iter().enumerate() {
        seed_image(&f.conn, h, Some("2026-03-01T00:00:00Z"));
        seed_path(
            &f.conn,
            &format!("pl{i}"),
            h,
            "vol1",
            "root1",
            &format!("photos/iceland/img_{i}.jpg"),
        );
    }
    f.store
        .append(&f.session, d_remark("keeper", vec![b.clone()]), None)
        .unwrap();

    // Membership outlives files (§10.1 evented removal): a hash the index
    // does not know yields no row instead of an error.
    let ghost = hash(42);
    let items = f.lib.list_images(&[b.clone(), ghost, a.clone()]).unwrap();
    assert_eq!(
        items.iter().map(|i| i.hash.clone()).collect::<Vec<_>>(),
        vec![b.clone(), a.clone()],
        "input order preserved, unknown hash skipped"
    );
    assert!(items[0].has_journal);
    assert!(!items[1].has_journal);
    // rel_path is root-relative, exactly like list_folder's rows; root_id
    // rides along (the frontend's stack pairing keys on it — a collection
    // grid mixes roots).
    assert_eq!(items[1].rel_path, "iceland/img_0.jpg");
    assert_eq!(items[1].file_name, "img_0.jpg");
    assert_eq!(items[1].root_id.as_deref(), Some("root1"));
    assert!(!items[0].offline);
}

#[test]
fn list_images_renders_indexed_members_whose_paths_all_went_stale() {
    // Membership outlives files (RETRIEVAL §10.1, B71): a member whose
    // every path row went stale — the file was deleted on disk, or its
    // root was removed (a supported, reversible operation that keeps
    // images/journals/previews for relink) — is still IN the index and
    // must still render in the collection grid, offline-badged. Dropping
    // it would make the rail badge (member_count, collections side) and
    // the grid disagree forever, with no UI surface left to remove the
    // phantom member from.
    let f = lib_fixture();
    seed_volume(&f.conn, "vol1", true, false);
    seed_root(&f.conn, "root1", "vol1", "photos");
    let gone = hash(50);
    seed_image(&f.conn, &gone, None);
    seed_path(
        &f.conn,
        "ps0",
        &gone,
        "vol1",
        "root1",
        "photos/iceland/gone.jpg",
    );
    f.conn
        .execute(
            "UPDATE paths SET state = 'stale', stale_reason = 'root-removed',
                    stale_since = '2026-03-01T00:00:00Z'
             WHERE path_id = 'ps0'",
            [],
        )
        .unwrap();

    let items = f.lib.list_images(std::slice::from_ref(&gone)).unwrap();
    assert_eq!(items.len(), 1, "stale-path member still renders");
    assert!(items[0].offline, "no active online path reads as offline");
    assert_eq!(items[0].rel_path, "iceland/gone.jpg");
    assert_eq!(items[0].file_name, "gone.jpg");

    // An ACTIVE path, when one exists, beats any stale one as the
    // representative — even when the stale rel_path sorts first.
    let moved = hash(51);
    seed_image(&f.conn, &moved, None);
    seed_path(
        &f.conn,
        "ps1",
        &moved,
        "vol1",
        "root1",
        "photos/aaa/old.jpg",
    );
    f.conn
        .execute(
            "UPDATE paths SET state = 'stale', stale_reason = 'moved',
                    stale_since = '2026-03-01T00:00:00Z'
             WHERE path_id = 'ps1'",
            [],
        )
        .unwrap();
    seed_path(
        &f.conn,
        "ps2",
        &moved,
        "vol1",
        "root1",
        "photos/zzz/new.jpg",
    );
    let items = f.lib.list_images(std::slice::from_ref(&moved)).unwrap();
    assert_eq!(items[0].rel_path, "zzz/new.jpg");
    assert!(!items[0].offline);
}

#[test]
fn a2_offline_badge_when_only_paths_are_on_offline_volumes() {
    let f = lib_fixture();
    seed_volume(&f.conn, "vol-on", true, false);
    seed_volume(&f.conn, "vol-off", false, false);
    seed_root(&f.conn, "root1", "vol-off", "");
    let only_offline = hash(10);
    seed_image(&f.conn, &only_offline, None);
    seed_path(&f.conn, "p1", &only_offline, "vol-off", "root1", "img.jpg");
    let also_online = hash(11);
    seed_image(&f.conn, &also_online, None);
    seed_path(&f.conn, "p2", &also_online, "vol-off", "root1", "img2.jpg");
    seed_path(&f.conn, "p3", &also_online, "vol-on", "root1", "img2.jpg");

    let items = f.lib.list_folder("root1", "").unwrap();
    let find = |h: &ContentHash| items.iter().find(|i| i.hash == *h).unwrap();
    assert!(find(&only_offline).offline);
    assert!(!find(&also_online).offline);
}

#[test]
fn a2_rating_zero_is_explicit_and_distinct_from_unrated() {
    // DECISIONS C6: 0 = explicit clear; the fold (EVENTS E4) keeps the last
    // live rating — value 0 — distinct from never-rated NULL.
    let f = lib_fixture();
    seed_volume(&f.conn, "vol1", true, false);
    seed_root(&f.conn, "root1", "vol1", "");
    let rated_zero = hash(20);
    let unrated = hash(21);
    for (i, h) in [&rated_zero, &unrated].iter().enumerate() {
        seed_image(&f.conn, h, None);
        seed_path(
            &f.conn,
            &format!("pz{i}"),
            h,
            "vol1",
            "root1",
            &format!("z{i}.jpg"),
        );
    }
    f.store
        .append(&f.session, d_rating(3, vec![rated_zero.clone()]), None)
        .unwrap();
    f.store
        .append(&f.session, d_rating(0, vec![rated_zero.clone()]), None)
        .unwrap();
    let items = f.lib.list_folder("root1", "").unwrap();
    let find = |h: &ContentHash| items.iter().find(|i| i.hash == *h).unwrap();
    assert_eq!(find(&rated_zero).rating, Some(0));
    assert_eq!(find(&unrated).rating, None);
}

#[test]
fn a2_folder_tree_builds_nested_dirs() {
    let f = lib_fixture();
    seed_volume(&f.conn, "vol1", true, false);
    seed_root(&f.conn, "root1", "vol1", "photos");
    seed_image(&f.conn, &hash(30), None);
    seed_image(&f.conn, &hash(31), None);
    seed_path(
        &f.conn,
        "t1",
        &hash(30),
        "vol1",
        "root1",
        "photos/2026/iceland/a.jpg",
    );
    seed_path(
        &f.conn,
        "t2",
        &hash(31),
        "vol1",
        "root1",
        "photos/2026/harbor/b.jpg",
    );
    let tree = f.lib.folder_tree("root1").unwrap();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].name, "2026");
    assert_eq!(tree[0].rel_path, "2026");
    let names: Vec<&str> = tree[0].children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["harbor", "iceland"]);
    assert_eq!(tree[0].children[1].rel_path, "2026/iceland");
}

// ---------------------------------------------------------------------------
// A3 — Library implements ImageLocator (B29)
// ---------------------------------------------------------------------------

#[test]
fn a3_library_locates_writable_readonly_and_offline_images() {
    let f = lib_fixture();
    seed_volume(&f.conn, "vol-rw", true, false);
    seed_volume(&f.conn, "vol-ro", true, true);
    seed_volume(&f.conn, "vol-off", false, false);
    seed_root(&f.conn, "root1", "vol-rw", "");
    seed_root(&f.conn, "root2", "vol-ro", "");
    seed_root(&f.conn, "root3", "vol-off", "");

    let writable = hash(40);
    seed_image(&f.conn, &writable, None);
    seed_path(&f.conn, "w1", &writable, "vol-rw", "root1", "a/img.jpg");
    let readonly = hash(41);
    seed_image(&f.conn, &readonly, None);
    seed_path(&f.conn, "r1", &readonly, "vol-ro", "root2", "b/img.jpg");
    let offline = hash(42);
    seed_image(&f.conn, &offline, None);
    seed_path(&f.conn, "o1", &offline, "vol-off", "root3", "c/img.jpg");

    let locate = |h: &ContentHash| ImageLocator::locate(&f.lib, h);
    assert_eq!(
        locate(&writable),
        AdjacentLocation::Writable {
            image_path: std::path::PathBuf::from("/mnt/test/a/img.jpg"),
        }
    );
    assert_eq!(
        locate(&readonly),
        AdjacentLocation::Unwritable {
            image_path: Some(std::path::PathBuf::from("/mnt/test/b/img.jpg")),
            volume_id: Some("vol-ro".into()),
        }
    );
    assert_eq!(
        locate(&offline),
        AdjacentLocation::Offline {
            volume_id: Some("vol-off".into()),
        }
    );
    assert_eq!(
        locate(&hash(99)),
        AdjacentLocation::Offline { volume_id: None },
        "unknown hash"
    );
}

// ---------------------------------------------------------------------------
// A4 — SidecarEngine::new_shared over Arc<EventStore> + Arc<Library>
// ---------------------------------------------------------------------------

#[test]
fn a4_new_shared_engine_flushes_through_arc_store_and_arc_library_locator() {
    let dir = tempfile::tempdir().unwrap();
    let mount = dir.path().join("mount");
    std::fs::create_dir_all(mount.join("a")).unwrap();
    let img_path = mount.join("a/img.jpg");
    std::fs::write(&img_path, b"image-bytes").unwrap();
    let image = ContentHash::from_bytes_of(b"image-bytes");

    let db = dir.path().join("photoproof.db");
    let app_data = dir.path().join("app-data");
    std::fs::create_dir_all(&app_data).unwrap();

    let store = Arc::new(EventStore::open(&db).unwrap());
    let library = Arc::new(Library::open(&db, dir.path().join("cache")).unwrap());
    // Seed the library view of the file (writable, online at the temp mount).
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO volumes (volume_id, state, mount_point, read_only,
                                  first_seen_at, last_seen_at)
             VALUES ('vol1', 'online', ?1, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [mount.to_str().unwrap()],
        )
        .unwrap();
        seed_root(&conn, "root1", "vol1", "");
        seed_image(&conn, &image, None);
        seed_path(&conn, "p1", &image, "vol1", "root1", "a/img.jpg");
    }

    // The exact shell wiring (state.rs): shared store, library as locator,
    // no Box::leak anywhere.
    let engine: SidecarEngine<'static, Arc<Library>> =
        SidecarEngine::new_shared(store.clone(), &db, &app_data, library.clone()).unwrap();

    let session = store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .unwrap();
    store
        .append(
            &session,
            d_remark("written via Arc", vec![image.clone()]),
            None,
        )
        .unwrap();
    let flushed = engine.flush_all(UtcMillis::now()).unwrap();
    assert_eq!(flushed.len(), 1);

    let sidecar = mount.join("a/img.jpg.photoproof.json");
    assert!(
        sidecar.exists(),
        "adjacent sidecar written beside the image"
    );
    let bytes = std::fs::read(&sidecar).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("written via Arc"));
    assert!(text.contains(image.as_str()));
}
