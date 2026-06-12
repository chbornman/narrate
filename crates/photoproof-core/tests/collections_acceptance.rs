//! P7.3 acceptance: the collections store (spec/RETRIEVAL.md §10, B71).
//!
//! Covers the §13 items that belong to collections — item 9 (round-trip:
//! rebuild from `collections.photoproof.json`; stale-copy merges never
//! un-remove a member and never lose a note) — plus the §10.1 evented
//! membership history, the §10.2 debounced-writer mechanics (SIDECARS §9
//! windows, mtime-stable no-op), and the redaction independence rule
//! (membership rows carry hashes only).

use std::fs;

use photoproof_core::collections::{
    COLLECTIONS_FILENAME, CollectionStatus, Collections, CollectionsError, ParsedCollections,
    parse_collections,
};
use photoproof_core::{
    ContentHash, EventDraft, EventStore, RemarkSource, SessionContext, UtcMillis,
};

const DEVICE_ID: &str = "deadbeefdeadbeefdeadbeefdeadbeef";

fn hash(n: u8) -> ContentHash {
    ContentHash::from_hex(&format!("{n:02x}").repeat(32)).unwrap()
}

fn ts(s: &str) -> UtcMillis {
    UtcMillis::parse(s).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
    app_data: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("appdata");
        let db = app_data.join("photoproof.db");
        fs::create_dir_all(&app_data).unwrap();
        Self {
            _dir: dir,
            db,
            app_data,
        }
    }

    fn open(&self) -> Collections {
        Collections::open(&self.db, &self.app_data).unwrap()
    }

    fn file(&self) -> std::path::PathBuf {
        self.app_data.join(COLLECTIONS_FILENAME)
    }
}

// ---------------------------------------------------------------------------
// CRUD + evented membership (§10.1)
// ---------------------------------------------------------------------------

#[test]
fn create_rename_status_and_list() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("Quiet Hours", "the fog series", t0).unwrap();
    assert_eq!(rec.status, CollectionStatus::Active);
    assert_eq!(rec.created_ts, "2026-06-01T10:00:00.000Z");

    let t1 = ts("2026-06-01T11:00:00.000Z");
    c.rename(&rec.id, "Quiet Hours II", t1).unwrap();
    c.set_status(&rec.id, CollectionStatus::Shelved, t1)
        .unwrap();

    let listed = c.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Quiet Hours II");
    assert_eq!(listed[0].description, "the fog series");
    assert_eq!(listed[0].status, CollectionStatus::Shelved);
    assert_eq!(listed[0].updated_ts, "2026-06-01T11:00:00.000Z");

    // Validation + NotFound surfaces.
    assert!(matches!(
        c.create("   ", "", t1),
        Err(CollectionsError::Invalid(_))
    ));
    assert!(matches!(
        c.rename("01HV00000000000000000000ZZ", "x", t1),
        Err(CollectionsError::NotFound(_))
    ));
}

#[test]
fn membership_is_evented_with_queryable_history() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-01-10T10:00:00.000Z");
    let rec = c.create("Winter", "", t0).unwrap();
    let (a, b) = (hash(0xaa), hash(0xbb));

    // Add two; one is dropped in spring; re-added in summer.
    let added = c.add_images(&rec.id, &[a.clone(), b.clone()], t0).unwrap();
    assert_eq!(added, 2);
    // Re-adding a current member is a no-op, not a new interval.
    assert_eq!(
        c.add_images(&rec.id, std::slice::from_ref(&a), t0).unwrap(),
        0
    );

    let t1 = ts("2026-04-01T10:00:00.000Z");
    assert_eq!(
        c.remove_images(&rec.id, std::slice::from_ref(&b), t1)
            .unwrap(),
        1
    );
    // Removing a non-member is a no-op.
    assert_eq!(
        c.remove_images(&rec.id, std::slice::from_ref(&b), t1)
            .unwrap(),
        0
    );

    let t2 = ts("2026-07-01T10:00:00.000Z");
    assert_eq!(
        c.add_images(&rec.id, std::slice::from_ref(&b), t2).unwrap(),
        1
    );

    // Current membership: both (b re-added).
    assert_eq!(
        c.current_members(&rec.id).unwrap(),
        vec![a.clone(), b.clone()]
    );
    assert_eq!(c.get(&rec.id).unwrap().member_count, 2);

    // Full history: b carries TWO interval rows — the closed winter one
    // and the open summer one ("was in the series last winter, dropped in
    // spring"; §10.1 / M4 temporal questions).
    let history = c.membership_history(&rec.id).unwrap();
    let b_rows: Vec<_> = history
        .iter()
        .filter(|m| m.image_hash == b.as_str())
        .collect();
    assert_eq!(b_rows.len(), 2);
    assert_eq!(
        b_rows[0].removed_ts.as_deref(),
        Some("2026-04-01T10:00:00.000Z")
    );
    assert_eq!(b_rows[1].added_ts, "2026-07-01T10:00:00.000Z");
    assert_eq!(b_rows[1].removed_ts, None);

    // Membership-for-image answers from the open intervals only.
    let for_b = c.collections_for_image(&b).unwrap();
    assert_eq!(for_b.len(), 1);
    assert_eq!(for_b[0].id, rec.id);
    assert!(c.collections_for_image(&hash(0xcc)).unwrap().is_empty());
}

#[test]
fn same_millisecond_remove_and_readd_opens_a_distinct_interval() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("Fast", "", t0).unwrap();
    let img = hash(0x11);

    c.add_images(&rec.id, std::slice::from_ref(&img), t0)
        .unwrap();
    c.remove_images(&rec.id, std::slice::from_ref(&img), t0)
        .unwrap();
    c.add_images(&rec.id, std::slice::from_ref(&img), t0)
        .unwrap();

    let history = c.membership_history(&rec.id).unwrap();
    assert_eq!(history.len(), 2, "two distinct intervals, never collapsed");
    assert!(history[0].removed_ts.is_some());
    assert!(history[1].removed_ts.is_none());
    assert!(
        history[1].added_ts > history[0].added_ts,
        "the re-add lands strictly after the first interval"
    );
    assert!(
        history[0].removed_ts.as_deref().unwrap() >= history[0].added_ts.as_str(),
        "no interval closes before it opened"
    );
}

// ---------------------------------------------------------------------------
// The debounced writer (§10.2 via SIDECARS §9 mechanics)
// ---------------------------------------------------------------------------

#[test]
fn writer_debounces_then_caps_and_flushes_immediately_on_demand() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    c.create("Debounced", "", t0).unwrap();
    assert!(c.is_dirty());

    // Inside the 2 s quiet window: no write.
    assert!(
        !c.pump(UtcMillis::from_epoch_ms(t0.epoch_ms() + 1_000))
            .unwrap()
    );
    assert!(c.is_dirty());

    // Quiet window elapsed: the pump writes.
    assert!(
        c.pump(UtcMillis::from_epoch_ms(t0.epoch_ms() + 2_000))
            .unwrap()
    );
    assert!(!c.is_dirty());
    let bytes = fs::read(env.file()).unwrap();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("Debounced"));

    // Continuous mutations keep resetting the quiet window; the 5 s hard
    // cap flushes anyway (the "within 5 s" promise).
    let base = t0.epoch_ms() + 10_000;
    for i in 0..5 {
        c.add_note(
            &c.list().unwrap()[0].id,
            &format!("note {i}"),
            UtcMillis::from_epoch_ms(base + i * 1_000),
        )
        .unwrap();
    }
    assert!(
        c.pump(UtcMillis::from_epoch_ms(base + 5_000)).unwrap(),
        "hard cap fires while writes continue"
    );

    // Immediate flush bypasses both windows.
    c.add_note(
        &c.list().unwrap()[0].id,
        "last words",
        UtcMillis::from_epoch_ms(base + 6_000),
    )
    .unwrap();
    assert!(c.is_dirty());
    c.flush(UtcMillis::from_epoch_ms(base + 6_000)).unwrap();
    assert!(!c.is_dirty());
    let text = fs::read_to_string(env.file()).unwrap();
    assert!(text.contains("last words"));
}

#[test]
fn file_bytes_are_canonical_and_mtime_stable() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("Stable", "desc", t0).unwrap();
    c.add_images(&rec.id, &[hash(0x01)], t0).unwrap();
    c.flush(t0).unwrap();

    let bytes = fs::read(env.file()).unwrap();
    // Round-trip: parse → re-serialize → identical bytes (§3.5-style
    // determinism is what makes the mtime-stable comparison exact).
    let ParsedCollections::Doc(doc) = parse_collections(&bytes).unwrap() else {
        panic!("own file must parse as a current-version doc");
    };
    assert_eq!(doc.to_bytes(), bytes);

    // A flush with no changes must not touch the file (mtime-stable no-op).
    let mtime = fs::metadata(env.file()).unwrap().modified().unwrap();
    c.flush(ts("2026-06-01T11:00:00.000Z")).unwrap();
    assert_eq!(
        fs::metadata(env.file()).unwrap().modified().unwrap(),
        mtime,
        "identical bytes are skipped entirely"
    );
}

// ---------------------------------------------------------------------------
// §13.9 — round-trip and stale-copy merge
// ---------------------------------------------------------------------------

#[test]
fn rebuild_from_file_restores_everything_including_closed_intervals() {
    let env = Env::new();
    let (rec_id, expected_doc) = {
        let c = env.open();
        let t0 = ts("2026-01-10T10:00:00.000Z");
        let rec = c.create("Winter", "quiet, melancholic", t0).unwrap();
        c.add_note(&rec.id, "started during the cold snap", t0)
            .unwrap();
        c.add_images(&rec.id, &[hash(0xaa), hash(0xbb)], t0)
            .unwrap();
        c.remove_images(&rec.id, &[hash(0xbb)], ts("2026-04-01T10:00:00.000Z"))
            .unwrap();
        c.set_status(
            &rec.id,
            CollectionStatus::Done,
            ts("2026-05-01T10:00:00.000Z"),
        )
        .unwrap();
        c.flush(ts("2026-05-01T10:00:00.000Z")).unwrap();
        (rec.id.clone(), c.export_doc().unwrap())
    };

    // Delete SQLite; rebuild from the portability file alone (§13.9).
    fs::remove_file(&env.db).unwrap();
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(env.db.with_extension(format!("db{suffix}")));
    }
    let c = env.open();
    assert_eq!(
        c.export_doc().unwrap(),
        expected_doc,
        "collections, notes, and full membership history (closed intervals \
         included) survive byte-for-byte"
    );
    let rec = c.get(&rec_id).unwrap();
    assert_eq!(rec.status, CollectionStatus::Done);
    assert_eq!(rec.member_count, 1);
    assert_eq!(rec.note_count, 1);
    let history = c.membership_history(&rec_id).unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().any(|m| m.removed_ts.is_some()));
}

#[test]
fn merging_a_stale_copy_never_unremoves_a_member_or_loses_a_note() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-01-10T10:00:00.000Z");
    let rec = c.create("Series", "", t0).unwrap();
    c.add_images(&rec.id, &[hash(0xaa)], t0).unwrap();
    c.flush(t0).unwrap();
    let stale = fs::read(env.file()).unwrap(); // member still present, no notes

    // Move on: remove the member, add a note, rename.
    let t1 = ts("2026-02-01T10:00:00.000Z");
    c.remove_images(&rec.id, &[hash(0xaa)], t1).unwrap();
    c.add_note(&rec.id, "decided against it", t1).unwrap();
    c.rename(&rec.id, "Series, revised", t1).unwrap();
    c.flush(t1).unwrap();

    // Merge the stale copy back (restored old backup, second machine…).
    let stale_path = env.app_data.join("stale-copy.json");
    fs::write(&stale_path, &stale).unwrap();
    let t2 = ts("2026-03-01T10:00:00.000Z");
    c.import_file(&stale_path, t2).unwrap();

    let rec_now = c.get(&rec.id).unwrap();
    assert_eq!(rec_now.member_count, 0, "the removal never un-happens");
    assert_eq!(rec_now.note_count, 1, "no note is lost");
    assert_eq!(
        rec_now.name, "Series, revised",
        "stale metadata loses last-writer-wins"
    );

    // Idempotence: importing the same stale copy again changes nothing.
    assert!(!c.import_file(&stale_path, t2).unwrap().changed);
}

#[test]
fn two_replicas_converge_through_file_exchange() {
    // Two app instances diverge, then each imports the other's file: both
    // databases converge to identical documents (the C2-family laws).
    let env_a = Env::new();
    let env_b = Env::new();
    let a = env_a.open();
    let b = env_b.open();

    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = a.create("Shared", "", t0).unwrap();
    a.add_images(&rec.id, &[hash(0x01), hash(0x02)], t0)
        .unwrap();
    a.flush(t0).unwrap();
    b.import_file(&env_a.file(), t0).unwrap();

    // Diverge: A removes 0x01 and notes; B renames and adds 0x03.
    let t1 = ts("2026-06-02T10:00:00.000Z");
    a.remove_images(&rec.id, &[hash(0x01)], t1).unwrap();
    a.add_note(&rec.id, "dropped the blurry one", t1).unwrap();
    let t2 = ts("2026-06-03T10:00:00.000Z");
    b.rename(&rec.id, "Shared, renamed", t2).unwrap();
    b.add_images(&rec.id, &[hash(0x03)], t2).unwrap();

    a.flush(t2).unwrap();
    b.flush(t2).unwrap();
    let t3 = ts("2026-06-04T10:00:00.000Z");
    a.import_file(&env_b.file(), t3).unwrap();
    b.import_file(&env_a.file(), t3).unwrap();
    // Second exchange so B sees A's post-merge union too.
    a.flush(t3).unwrap();
    b.import_file(&env_a.file(), t3).unwrap();

    assert_eq!(a.export_doc().unwrap(), b.export_doc().unwrap());
    let merged = a.get(&rec.id).unwrap();
    assert_eq!(merged.name, "Shared, renamed");
    assert_eq!(merged.member_count, 2); // 0x02 + 0x03; 0x01 stays removed
    assert_eq!(merged.note_count, 1);
}

#[test]
fn union_duplicate_open_intervals_still_read_as_one_member() {
    // Two replicas both remove X then re-add it at DIFFERENT times: the
    // 10.2 union legitimately holds one closed interval plus TWO open
    // rows for the same image. Membership is a set; every read surface
    // must report ONE member, and a remove closes all open rows.
    let env_a = Env::new();
    let env_b = Env::new();
    let a = env_a.open();
    let b = env_b.open();
    let img = hash(0xee);

    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = a.create("Racing", "", t0).unwrap();
    a.add_images(&rec.id, std::slice::from_ref(&img), t0)
        .unwrap();
    a.flush(t0).unwrap();
    b.import_file(&env_a.file(), t0).unwrap();

    // Offline divergence: both remove, both re-add — at different times.
    let t1 = ts("2026-06-02T10:00:00.000Z");
    a.remove_images(&rec.id, std::slice::from_ref(&img), t1)
        .unwrap();
    a.add_images(
        &rec.id,
        std::slice::from_ref(&img),
        ts("2026-06-03T10:00:00.000Z"),
    )
    .unwrap();
    b.remove_images(&rec.id, std::slice::from_ref(&img), t1)
        .unwrap();
    b.add_images(
        &rec.id,
        std::slice::from_ref(&img),
        ts("2026-06-04T10:00:00.000Z"),
    )
    .unwrap();

    let t5 = ts("2026-06-05T10:00:00.000Z");
    a.flush(t5).unwrap();
    b.flush(t5).unwrap();
    a.import_file(&env_b.file(), t5).unwrap();

    // History keeps all three interval rows (closed + two open)...
    let history = a.membership_history(&rec.id).unwrap();
    assert_eq!(history.len(), 3, "union preserves both replicas' intervals");
    assert_eq!(
        history.iter().filter(|m| m.removed_ts.is_none()).count(),
        2,
        "two open rows for one image after the union"
    );
    // ...but membership is a SET: one image counts and lists once.
    assert_eq!(a.get(&rec.id).unwrap().member_count, 1);
    assert_eq!(a.current_members(&rec.id).unwrap(), vec![img.clone()]);
    assert_eq!(a.collections_for_image(&img).unwrap().len(), 1);

    // One remove closes the image's membership entirely and reports ONE
    // image removed, not two rows.
    let t6 = ts("2026-06-06T10:00:00.000Z");
    assert_eq!(
        a.remove_images(&rec.id, std::slice::from_ref(&img), t6)
            .unwrap(),
        1
    );
    assert_eq!(a.get(&rec.id).unwrap().member_count, 0);
    assert!(a.current_members(&rec.id).unwrap().is_empty());
}

#[test]
fn note_conflicts_are_reported_with_the_losing_copy() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("Notes", "", t0).unwrap();
    let note = c.add_note(&rec.id, "the original wording", t0).unwrap();

    // A tampered/corrupted copy carries the SAME note id with different
    // text. The merge resolves deterministically and the losing copy is
    // surfaced — never silently destroyed (notes are append-only, 10.1).
    let mut doc = c.export_doc().unwrap();
    doc.collections[0].notes[0].text = "zzz tampered wording".to_owned();
    let outcome = c.import_doc(&doc, ts("2026-06-02T10:00:00.000Z")).unwrap();
    assert_eq!(outcome.note_conflicts.len(), 1);
    assert_eq!(outcome.note_conflicts[0].note_id, note.id);
    assert_eq!(
        outcome.note_conflicts[0].loser.text, "zzz tampered wording",
        "lesser (ts, text) wins; the import's copy is the preserved loser"
    );
    assert_eq!(
        c.notes(&rec.id).unwrap()[0].text,
        "the original wording",
        "the stored note converged to the deterministic winner"
    );
}

// ---------------------------------------------------------------------------
// Guard rails: corrupt file, newer version, export placement
// ---------------------------------------------------------------------------

#[test]
fn parse_rejects_duplicate_member_keys_and_note_ids() {
    let img = "ab".repeat(32);
    let dup_member = format!(
        r#"{{"version": 1, "collections": [{{
            "id": "01HV00000000000000000000A1", "name": "x", "description": "",
            "status": "active",
            "created_ts": "2026-01-01T10:00:00.000Z", "updated_ts": "2026-01-01T10:00:00.000Z",
            "notes": [],
            "members": [
              {{"image_hash": "{img}", "added_ts": "2026-01-01T10:00:00.000Z",
                "removed_ts": "2026-01-02T10:00:00.000Z"}},
              {{"image_hash": "{img}", "added_ts": "2026-01-01T10:00:00.000Z",
                "removed_ts": null}}
            ]
        }}]}}"#
    );
    // Accepting this would let the second row's null removed_ts win by
    // file order in the new-collection import path — un-removing a member
    // (the 13.9 guarantee) without ever consulting the merge rules.
    assert!(matches!(
        parse_collections(dup_member.as_bytes()),
        Err(CollectionsError::Parse(msg)) if msg.contains("duplicate member key")
    ));

    let dup_note = r#"{"version": 1, "collections": [{
        "id": "01HV00000000000000000000A1", "name": "x", "description": "",
        "status": "active",
        "created_ts": "2026-01-01T10:00:00.000Z", "updated_ts": "2026-01-01T10:00:00.000Z",
        "notes": [
          {"id": "01HV0000000000000000000N01", "ts": "2026-01-01T10:00:00.000Z", "text": "a"},
          {"id": "01HV0000000000000000000N01", "ts": "2026-01-01T10:00:00.000Z", "text": "b"}
        ],
        "members": []
    }]}"#;
    assert!(matches!(
        parse_collections(dup_note.as_bytes()),
        Err(CollectionsError::Parse(msg)) if msg.contains("duplicate note id")
    ));
}

#[test]
fn corrupt_file_salvages_every_individually_intact_collection() {
    // One garbled entry must not cost the user the whole store: the file
    // is renamed aside (bytes preserved) AND the intact collections are
    // imported.
    let env = Env::new();
    let good_id = "01HV00000000000000000000A1";
    let content = format!(
        r#"{{"version": 1, "collections": [
          {{"id": "{good_id}", "name": "Intact", "description": "",
            "status": "active",
            "created_ts": "2026-01-01T10:00:00.000Z", "updated_ts": "2026-01-01T10:00:00.000Z",
            "notes": [], "members": []}},
          {{"id": "01HV00000000000000000000B2", "name": "Garbled", "description": "",
            "status": "active",
            "created_ts": "not a timestamp", "updated_ts": "2026-01-01T10:00:00.000Z",
            "notes": [], "members": []}}
        ]}}"#
    );
    fs::write(env.file(), content).unwrap();

    let c = env.open();
    let listed = c.list().unwrap();
    assert_eq!(listed.len(), 1, "the intact collection was salvaged");
    assert_eq!(listed[0].name, "Intact");
    let aside: Vec<_> = fs::read_dir(&env.app_data)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.contains(".corrupt-").then_some(name)
        })
        .collect();
    assert_eq!(aside.len(), 1, "the original bytes are preserved aside");
}

#[test]
fn corrupt_file_is_renamed_aside_never_overwritten_in_place() {
    let env = Env::new();
    fs::write(env.file(), b"{ not json").unwrap();
    let c = env.open();
    c.create("Fresh", "", ts("2026-06-01T10:00:00.000Z"))
        .unwrap();
    c.flush(ts("2026-06-01T10:00:00.000Z")).unwrap();

    let aside: Vec<_> = fs::read_dir(&env.app_data)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.contains(".corrupt-").then_some(name)
        })
        .collect();
    assert_eq!(aside.len(), 1, "the unparseable bytes are preserved aside");
    assert!(
        fs::read_to_string(env.file()).unwrap().contains("Fresh"),
        "the canonical file took the path back"
    );
}

#[test]
fn newer_version_file_is_opaque_inviolable_and_loud() {
    let env = Env::new();
    let foreign = b"{\n  \"collections\": [],\n  \"version\": 2\n}\n".to_vec();
    fs::write(env.file(), &foreign).unwrap();
    let c = env.open();
    // The lockout is reportable, not silent (SIDECARS 5.3: the user must
    // learn mirroring is off).
    assert_eq!(c.newer_version_lockout(), Some(2));

    let t0 = ts("2026-06-01T10:00:00.000Z");
    c.create("Local only", "", t0).unwrap();
    assert!(
        !c.pump(UtcMillis::from_epoch_ms(t0.epoch_ms() + 60_000))
            .unwrap()
    );
    // An explicit flush with pending mutations ERRORS: truth would
    // otherwise live in SQLite only while the caller believes it drained.
    assert!(matches!(
        c.flush(t0),
        Err(CollectionsError::NewerVersion(2))
    ));
    // The one-click export errors too: a database-derived file would
    // silently omit the v2 file's collections ("walk away with
    // everything" cannot be delivered by this build).
    assert!(matches!(
        c.export_to(&env.app_data.join("export"), t0),
        Err(CollectionsError::NewerVersion(2))
    ));
    assert_eq!(
        fs::read(env.file()).unwrap(),
        foreign,
        "a v2 file is never merged and never overwritten by a v1 writer"
    );
}

#[test]
fn export_to_writes_the_file_beside_an_export_and_rebuild_consumes_it() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("Exported", "", t0).unwrap();
    c.add_images(&rec.id, &[hash(0x42)], t0).unwrap();

    let export_dir = env.app_data.join("export");
    let path = c.export_to(&export_dir, t0).unwrap();
    assert_eq!(path, export_dir.join(COLLECTIONS_FILENAME));
    assert_eq!(
        fs::read(&path).unwrap(),
        fs::read(env.file()).unwrap(),
        "the export carries the same canonical bytes as the live file"
    );

    // RETRIEVAL 10.2: the export file is CONSUMED by rebuild-from-sidecars
    // itself — the fresh-machine restore (only the one-click export in
    // hand, empty app data) gets collections back with no extra step.
    let env2 = Env::new();
    let store = std::sync::Arc::new(EventStore::open(&env2.db).unwrap());
    let locator = photoproof_core::sidecar::MemoryLocator::new();
    let engine = photoproof_core::sidecar::SidecarEngine::new_shared(
        store,
        &env2.db,
        &env2.app_data,
        locator,
    )
    .unwrap();
    let report = photoproof_core::sidecar::rebuild_from_sidecars(
        &engine,
        &photoproof_core::sidecar::RebuildOptions::export_dir(&export_dir),
        t0,
    )
    .unwrap();
    assert!(
        report.failures.is_empty(),
        "no quarantine: {:?}",
        report.failures
    );
    assert!(report.collisions.is_empty());
    assert_eq!(
        report.collections_files_imported,
        vec![path],
        "the import is surfaced in the integrity report, never silent"
    );

    // The rebuilt database holds the collection, and the rebuild also
    // converged the app-data mirror so the next open needs no extra step.
    let c2 = env2.open();
    assert_eq!(c2.get(&rec.id).unwrap().member_count, 1);
    assert!(
        fs::read_to_string(env2.file())
            .unwrap()
            .contains("Exported"),
        "the app-data mirror carries the imported union"
    );
}

// ---------------------------------------------------------------------------
// Redaction independence (§10.2: hashes only)
// ---------------------------------------------------------------------------

#[test]
fn image_redaction_does_not_touch_membership_rows() {
    let env = Env::new();
    let c = env.open();
    let t0 = ts("2026-06-01T10:00:00.000Z");
    let rec = c.create("With journals", "", t0).unwrap();
    let img = hash(0x77);
    c.add_images(&rec.id, std::slice::from_ref(&img), t0)
        .unwrap();

    // A journaled remark on the member image, then redact it.
    let store = EventStore::open(&env.db).unwrap();
    let sid = store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .unwrap();
    let event = store
        .append(
            &sid,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "about to be redacted".into(),
                targets: vec![img.clone()],
            },
            None,
        )
        .unwrap();
    store.redact(&event.id).unwrap();

    // Membership untouched: the rows carry hashes only; the image simply
    // has no quotable journal anymore.
    assert_eq!(c.current_members(&rec.id).unwrap(), vec![img.clone()]);
    assert_eq!(c.collections_for_image(&img).unwrap().len(), 1);
    let history = c.membership_history(&rec.id).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].removed_ts.is_none());
}

// ---------------------------------------------------------------------------
// Migration hygiene
// ---------------------------------------------------------------------------

#[test]
fn user_version_regression_rerun_preserves_collection_rows() {
    // Migrations re-run when user_version regresses (the store's v5
    // downgrade-simulation precedent); collections are USER TRUTH and the
    // re-run must not drop them.
    let env = Env::new();
    let rec_id = {
        let c = env.open();
        let rec = c
            .create("Survivor", "", ts("2026-06-01T10:00:00.000Z"))
            .unwrap();
        rec.id
    };
    {
        let conn = rusqlite::Connection::open(&env.db).unwrap();
        conn.execute_batch("PRAGMA user_version = 5;").unwrap();
    }
    let c = env.open();
    assert_eq!(c.get(&rec_id).unwrap().name, "Survivor");
}
