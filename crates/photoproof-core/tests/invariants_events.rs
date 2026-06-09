//! spec/EVENTS.md §11 — integrity invariants I1–I16, one named test each.

mod common;

use std::time::{Duration, Instant};

use common::*;
use photoproof_core::{
    AppendError, ContentHash, Event, EventStore, JournalEntry, Kind, Minted, Minter, UtcMillis,
    canonical_json, parse_canonical,
};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rusqlite::OptionalExtension;

// ---------------------------------------------------------------------------
// I1 — Append-only.
// ---------------------------------------------------------------------------

#[test]
fn i01_append_only() {
    let ts = new_store();
    ts.store
        .append(&ts.session, d_remark("immutable", vec![hash(1)]), None)
        .unwrap();
    let conn = ts.raw_conn();

    let err = conn
        .execute("UPDATE annotation_events SET text = 'hacked'", [])
        .unwrap_err()
        .to_string();
    assert!(err.contains("only the redaction scrub"), "{err}");

    let err = conn
        .execute("DELETE FROM annotation_events", [])
        .unwrap_err()
        .to_string();
    assert!(err.contains("append-only"), "{err}");

    let err = conn
        .execute("UPDATE event_targets SET position = 7", [])
        .unwrap_err()
        .to_string();
    assert!(err.contains("append-only"), "{err}");

    let err = conn
        .execute("DELETE FROM event_targets", [])
        .unwrap_err()
        .to_string();
    assert!(err.contains("append-only"), "{err}");
}

// ---------------------------------------------------------------------------
// I2 — Scrub is minimal: redact() changes exactly text, payload, redacted_by.
// ---------------------------------------------------------------------------

fn immutable_columns(conn: &rusqlite::Connection, id: &str) -> Vec<String> {
    conn.query_row(
        "SELECT id, v, session_id, ts, source, kind, \
                COALESCE(target_event,'-'), COALESCE(linked_event,'-') \
         FROM annotation_events WHERE id = ?1",
        [id],
        |r| {
            let mut v = Vec::new();
            for i in 0..8 {
                v.push(
                    r.get::<_, String>(i)
                        .unwrap_or_else(|_| r.get::<_, i64>(i).map(|n| n.to_string()).unwrap()),
                );
            }
            Ok(v)
        },
    )
    .unwrap()
}

fn scrub_state(conn: &rusqlite::Connection, id: &str) -> (bool, bool, bool) {
    conn.query_row(
        "SELECT text IS NULL, payload IS NULL, redacted_by IS NOT NULL \
         FROM annotation_events WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

#[test]
fn i02_scrub_is_minimal() {
    let ts = new_store();
    let a = hash(1);
    let v1 = ts
        .store
        .append(
            &ts.session,
            d_voice("secret words", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let rev = ts
        .store
        .append(
            &ts.session,
            d_revision(v1.id.clone(), "secret words two"),
            None,
        )
        .unwrap();
    let st = ts
        .store
        .append(&ts.session, d_stroke(a.clone(), Some(v1.id.clone())), None)
        .unwrap();

    let conn = ts.raw_conn();
    let victims = [v1.id.as_str(), rev.id.as_str(), st.id.as_str()];
    let before: Vec<Vec<String>> = victims
        .iter()
        .map(|id| immutable_columns(&conn, id))
        .collect();

    let chain_ids = ts.store.redact(&v1.id).unwrap();
    assert_eq!(chain_ids.len(), 2, "chain closure: remark + revision");
    ts.store.redact(&st.id).unwrap();

    for (i, id) in victims.iter().enumerate() {
        assert_eq!(
            immutable_columns(&conn, id),
            before[i],
            "every column except text/payload/redacted_by is byte-identical"
        );
        assert_eq!(scrub_state(&conn, id), (true, true, true));
    }
    // Target rows survive (§7 step 5).
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event_targets WHERE event_id IN (?1, ?2)",
            [v1.id.as_str(), st.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
}

// ---------------------------------------------------------------------------
// I3 — Round-trip: annotate (every kind) → export canonical JSON → rebuild
//      via merge → identical event set, identical folds, byte-identical JSON.
// ---------------------------------------------------------------------------

fn all_event_bytes(ts: &TestStore) -> Vec<(String, Vec<u8>)> {
    let conn = ts.raw_conn();
    let mut stmt = conn
        .prepare("SELECT id FROM annotation_events ORDER BY id")
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    ids.into_iter()
        .map(|id| {
            let e = ts
                .store
                .raw_event(&photoproof_core::EventId::from_str_strict(&id).unwrap())
                .unwrap()
                .unwrap();
            (id, canonical_json(&e))
        })
        .collect()
}

#[test]
fn i03_round_trip() {
    let ts = new_store();
    let (a, b) = (hash(1), hash(2));
    ts.store
        .append(
            &ts.session,
            d_remark("the spine of the sequence", vec![a.clone(), b.clone()]),
            None,
        )
        .unwrap();
    let v1 = ts
        .store
        .append(
            &ts.session,
            d_voice("muddy light", vec![a.clone()], None),
            None,
        )
        .unwrap();
    ts.store
        .append(&ts.session, d_revision(v1.id.clone(), "moody light"), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_stroke(a.clone(), Some(v1.id.clone())), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_rating(3, vec![a.clone(), b.clone()]), None)
        .unwrap();
    let r5 = ts
        .store
        .append(&ts.session, d_rating(5, vec![a.clone()]), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_retract(r5.id.clone()), None)
        .unwrap();
    // Session-level remark (zero targets) — must survive rebuild too (§2.3).
    ts.store
        .append(&ts.session, d_remark("session-level note", vec![]), None)
        .unwrap();
    // Redaction with chain closure.
    ts.store.redact(&v1.id).unwrap();

    let bytes = all_event_bytes(&ts);
    let fj_a = ts.store.folded_journal(&a).unwrap();
    let fj_b = ts.store.folded_journal(&b).unwrap();
    let cr_a = ts.store.current_rating(&a).unwrap();
    let cr_b = ts.store.current_rating(&b).unwrap();
    let sl = ts.store.sessionlevel_events(&ts.session).unwrap();
    let conn = ts.raw_conn();
    let truth = dump_truth(&conn);

    // "Delete SQLite": a brand-new store, rebuilt from the canonical bytes.
    let dir2 = tempfile::tempdir().unwrap();
    let store2 = EventStore::open(dir2.path().join("journal.db")).unwrap();
    let parsed: Vec<Event> = bytes
        .iter()
        .map(|(_, b)| parse_canonical(b).unwrap())
        .collect();
    let report = store2.merge(&parsed).unwrap();
    assert!(report.quarantined.is_empty());
    assert!(report.integrity_warnings.is_empty());
    assert_eq!(report.inserted, bytes.len());

    for (id, original) in &bytes {
        let e = store2
            .raw_event(&photoproof_core::EventId::from_str_strict(id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(&canonical_json(&e), original, "byte-identical for {id}");
    }
    assert_eq!(store2.folded_journal(&a).unwrap(), fj_a);
    assert_eq!(store2.folded_journal(&b).unwrap(), fj_b);
    assert_eq!(store2.current_rating(&a).unwrap(), cr_a);
    assert_eq!(store2.current_rating(&b).unwrap(), cr_b);
    assert_eq!(store2.sessionlevel_events(&ts.session).unwrap(), sl);
    let conn2 = rusqlite::Connection::open(dir2.path().join("journal.db")).unwrap();
    assert_eq!(dump_truth(&conn2), truth);
}

// ---------------------------------------------------------------------------
// I4 — Relink / sidecar re-match: events bind by hash, never by path, so a
//      file move never touches the journal (LIBRARY/SIDECARS own mechanics).
// ---------------------------------------------------------------------------

#[test]
fn i04_relink_binds_by_hash() {
    let ts = new_store();
    let image_bytes: &[u8] = b"\x89RAWDATA-image-payload";
    let at_old_path = ContentHash::from_bytes_of(image_bytes);
    let e = ts
        .store
        .append(
            &ts.session,
            d_remark("keeper", vec![at_old_path.clone()]),
            None,
        )
        .unwrap();
    // "Move the file": identity is the bytes, recomputed wherever they live.
    let at_new_path = ContentHash::from_bytes_of(image_bytes);
    assert_eq!(at_old_path, at_new_path);
    let folded = ts.store.folded_journal(&at_new_path).unwrap();
    assert_eq!(folded.len(), 1);
    // No path ever appears in an event row or canonical JSON (§1.1).
    let json = String::from_utf8(canonical_json(&e)).unwrap();
    assert!(!json.contains('/') && !json.contains('\\'));
}

// ---------------------------------------------------------------------------
// I5 — Canonical stability (§4.3 literals are covered in canonical_vectors.rs).
// ---------------------------------------------------------------------------

#[test]
fn i05_canonical_stability() {
    let g = Gen::new(
        photoproof_core::SessionId::from_str_strict("01JZ5C3HW0RD8PT2M6QK4V9XEA").unwrap(),
    );
    let remark = g.remark("plain note", vec![hash(1), hash(2)]);
    let voice = g.voice_remark("spoken note", vec![hash(1)]);
    let rating = g.rating(0, vec![hash(1)]);
    let stroke = g.stroke(hash(1), Some(voice.id.clone()));
    let revision = g.revision(voice.id.clone(), "spoken note, corrected");
    let retraction = g.retraction(rating.id.clone());
    let redaction = g.redaction(voice.id.clone());
    let scrubbed = voice.scrubbed_form(redaction.id.clone());

    for e in [
        remark, voice, rating, stroke, revision, retraction, redaction, scrubbed,
    ] {
        let b = canonical_json(&e);
        let parsed = parse_canonical(&b).expect("canonical bytes parse");
        assert_eq!(parsed, e);
        assert_eq!(canonical_json(&parsed), b, "byte-exact round trip");
        let s = String::from_utf8(b).unwrap();
        assert!(!s.contains("null"));
        assert!(!s.contains(' ') || e.text.is_some()); // compact outside strings
    }
}

// ---------------------------------------------------------------------------
// I6 — Merge is a set: random overlapping subsets in random orders yield
//      identical final state (truth + derived); merging twice is a no-op.
// ---------------------------------------------------------------------------

fn random_events(rng: &mut StdRng) -> Vec<Event> {
    let g = Gen::new(
        photoproof_core::SessionId::from_str_strict("01JZ5C3HW0RD8PT2M6QK4V9XEA").unwrap(),
    );
    let images: Vec<ContentHash> = (0..6).map(hash).collect();
    let mut events: Vec<Event> = Vec::new();
    let mut condemned: Vec<String> = Vec::new();
    for n in 0..60 {
        let pick_targets = |rng: &mut StdRng, min: usize| -> Vec<ContentHash> {
            let count = rng.gen_range(min..=2.max(min));
            let mut idx: Vec<usize> = (0..images.len()).collect();
            idx.shuffle(rng);
            idx.into_iter()
                .take(count)
                .map(|i| images[i].clone())
                .collect()
        };
        let earlier = |rng: &mut StdRng, events: &[Event], kinds: &[Kind]| -> Option<Event> {
            let c: Vec<&Event> = events.iter().filter(|e| kinds.contains(&e.kind)).collect();
            if c.is_empty() {
                None
            } else {
                Some(c[rng.gen_range(0..c.len())].clone())
            }
        };
        let roll = rng.gen_range(0..100);
        let e = if roll < 30 {
            g.remark(&format!("note {n}"), pick_targets(rng, 0))
        } else if roll < 45 {
            g.voice_remark(&format!("spoken {n}"), pick_targets(rng, 0))
        } else if roll < 60 {
            g.rating(rng.gen_range(0..=5), pick_targets(rng, 1))
        } else if roll < 72 {
            g.stroke(images[rng.gen_range(0..images.len())].clone(), None)
        } else if roll < 82 {
            match earlier(rng, &events, &[Kind::Remark, Kind::Revision]) {
                Some(t) => g.revision(t.id, &format!("corrected {n}")),
                None => g.remark(&format!("note {n}"), pick_targets(rng, 0)),
            }
        } else if roll < 93 {
            match earlier(
                rng,
                &events,
                &[Kind::Remark, Kind::Rating, Kind::Stroke, Kind::Revision],
            ) {
                Some(t) => g.retraction(t.id),
                None => g.remark(&format!("note {n}"), pick_targets(rng, 0)),
            }
        } else {
            // At most one redaction per target keeps redacted_by markers
            // deterministic (duplicate redactions union harmlessly but pick
            // an arrival-dependent marker).
            match earlier(rng, &events, &[Kind::Remark, Kind::Revision, Kind::Stroke]) {
                Some(t) if !condemned.contains(&t.id.to_string()) => {
                    condemned.push(t.id.to_string());
                    g.redaction(t.id)
                }
                _ => g.remark(&format!("note {n}"), pick_targets(rng, 0)),
            }
        };
        events.push(e);
    }
    events
}

fn full_state(path: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut out = dump_truth(&conn);
    out.extend(dump_derived(&conn, false));
    out
}

#[test]
fn i06_merge_is_a_set() {
    for seed in [11u64, 23, 47] {
        let mut rng = StdRng::seed_from_u64(seed);
        let events = random_events(&mut rng);
        for e in &events {
            e.validate().expect("generator emits valid events");
        }

        // Three overlapping subsets covering the set.
        let mut subsets: [Vec<Event>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for e in &events {
            let mask = rng.gen_range(1..8u8);
            for (k, subset) in subsets.iter_mut().enumerate() {
                if mask & (1 << k) != 0 {
                    subset.push(e.clone());
                }
            }
        }
        for s in &mut subsets {
            s.shuffle(&mut rng);
        }

        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let dir3 = tempfile::tempdir().unwrap();
        let p1 = dir1.path().join("j.db");
        let p2 = dir2.path().join("j.db");
        let p3 = dir3.path().join("j.db");

        {
            let s1 = EventStore::open(&p1).unwrap();
            s1.merge(&subsets[0]).unwrap();
            let snap = full_state(&p1);
            // Idempotence: merging anything twice is a no-op.
            s1.merge(&subsets[0]).unwrap();
            assert_eq!(
                full_state(&p1),
                snap,
                "seed {seed}: double-merge is a no-op"
            );
            s1.merge(&subsets[1]).unwrap();
            s1.merge(&subsets[2]).unwrap();
        }
        {
            let s2 = EventStore::open(&p2).unwrap();
            s2.merge(&subsets[2]).unwrap();
            s2.merge(&subsets[1]).unwrap();
            s2.merge(&subsets[0]).unwrap();
        }
        {
            // Everything at once, shuffled.
            let mut all = events.clone();
            all.shuffle(&mut rng);
            let s3 = EventStore::open(&p3).unwrap();
            s3.merge(&all).unwrap();
        }

        let f1 = full_state(&p1);
        assert_eq!(f1, full_state(&p2), "seed {seed}: order-independent");
        assert_eq!(f1, full_state(&p3), "seed {seed}: split-independent");
    }
}

// ---------------------------------------------------------------------------
// I7 — Derived = fold: rebuild_derived() changes nothing.
// ---------------------------------------------------------------------------

#[test]
fn i07_derived_equals_fold() {
    let ts = new_store();
    let (a, b, c) = (hash(1), hash(2), hash(3));
    let r1 = ts
        .store
        .append(
            &ts.session,
            d_remark("alpha keeper", vec![a.clone(), b.clone()]),
            None,
        )
        .unwrap();
    let v1 = ts
        .store
        .append(
            &ts.session,
            d_voice("bravo muddy", vec![b.clone()], None),
            None,
        )
        .unwrap();
    let rev1 = ts
        .store
        .append(&ts.session, d_revision(v1.id.clone(), "bravo moody"), None)
        .unwrap();
    ts.store
        .append(
            &ts.session,
            d_revision(rev1.id.clone(), "bravo moodier"),
            None,
        )
        .unwrap();
    ts.store
        .append(&ts.session, d_retract(rev1.id.clone()), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_rating(4, vec![a.clone(), c.clone()]), None)
        .unwrap();
    let r0 = ts
        .store
        .append(&ts.session, d_rating(2, vec![c.clone()]), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_retract(r0.id.clone()), None)
        .unwrap();
    let st = ts
        .store
        .append(&ts.session, d_stroke(c.clone(), None), None)
        .unwrap();
    ts.store.redact(&st.id).unwrap();
    ts.store.redact(&v1.id).unwrap();

    // Merge in foreign events, including a dangling revision (inert).
    let m1 = ts.store.mint();
    let dangling_target = ts.store.mint(); // never inserted
    let m2 = ts.store.mint();
    let foreign_remark = Event {
        id: m1.id,
        v: 1,
        session_id: ts.session.clone(),
        ts: m1.ts,
        source: photoproof_core::Source::Typed,
        kind: Kind::Remark,
        targets: vec![a.clone()],
        text: Some("foreign note".into()),
        payload: None,
        target_event: None,
        linked_event: None,
        redacted_by: None,
    };
    let dangling_revision = Event {
        id: m2.id,
        v: 1,
        session_id: ts.session.clone(),
        ts: m2.ts,
        source: photoproof_core::Source::Typed,
        kind: Kind::Revision,
        targets: vec![],
        text: Some("revision of a missing ancestor".into()),
        payload: None,
        target_event: Some(dangling_target.id),
        linked_event: None,
        redacted_by: None,
    };
    ts.store
        .merge(&[foreign_remark, dangling_revision])
        .unwrap();

    // A vector on a live root must survive rebuild.
    let conn = ts.raw_conn();
    insert_vector(&conn, r1.id.as_str());

    let before = dump_derived(&conn, true);
    ts.store.rebuild_derived().unwrap();
    let after = dump_derived(&conn, true);
    assert_eq!(
        before, after,
        "incremental maintenance == from-scratch recompute"
    );
}

// ---------------------------------------------------------------------------
// I8 — Redaction supremacy + physical hygiene byte-scan.
// ---------------------------------------------------------------------------

#[test]
fn i08_redaction_supremacy() {
    let marker = "plaintextmarkerbergamot";
    let ts = new_store();
    let a = hash(1);
    let v1 = ts
        .store
        .append(
            &ts.session,
            d_voice(&format!("contains {marker} secret"), vec![a.clone()], None),
            None,
        )
        .unwrap();
    // The stale sidecar: canonical bytes of the original, content included.
    let stale = canonical_json(&v1);

    ts.store.redact(&v1.id).unwrap();

    // Merge the stale original back: content stays scrubbed.
    let report = ts.store.merge(&[parse_canonical(&stale).unwrap()]).unwrap();
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.newly_scrubbed, 0);
    let e = ts.store.raw_event(&v1.id).unwrap().unwrap();
    assert!(e.scrubbed() && e.text.is_none() && e.payload.is_none());
    let folded = ts.store.folded_journal(&a).unwrap();
    assert!(matches!(folded[0], JournalEntry::RedactedStub { .. }));

    // Full-backup restore on a second machine: scrubbed there too.
    let dir2 = tempfile::tempdir().unwrap();
    let store2 = EventStore::open(dir2.path().join("j.db")).unwrap();
    let conn = ts.raw_conn();
    let redaction_id: String = conn
        .query_row("SELECT redaction_id FROM redactions LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let red_event = ts
        .store
        .raw_event(&photoproof_core::EventId::from_str_strict(&redaction_id).unwrap())
        .unwrap()
        .unwrap();
    // Stale content + the redaction event in one merge: redactions are
    // processed first (§8 step 1), so the content arrives pre-condemned.
    let report = store2
        .merge(&[parse_canonical(&stale).unwrap(), red_event.clone()])
        .unwrap();
    assert_eq!(report.scrubbed_on_arrival, 1);
    assert!(store2.raw_event(&v1.id).unwrap().unwrap().scrubbed());
    drop(store2);

    // Separate merges, content strictly first: the later redaction must
    // scrub the already-landed content ("learn it", newly_scrubbed).
    let dir3 = tempfile::tempdir().unwrap();
    let store3 = EventStore::open(dir3.path().join("j.db")).unwrap();
    store3.merge(&[parse_canonical(&stale).unwrap()]).unwrap();
    assert!(!store3.raw_event(&v1.id).unwrap().unwrap().scrubbed());
    let report = store3.merge(&[red_event]).unwrap();
    assert_eq!(report.newly_scrubbed, 1);
    assert!(store3.raw_event(&v1.id).unwrap().unwrap().scrubbed());
    drop(store3);

    // Byte-scan: with secure_delete=ON + WAL truncate, the plaintext appears
    // nowhere in the database or WAL files.
    let path = ts.path();
    drop(ts.store);
    for suffix in ["", "-wal", "-shm"] {
        let p = path.with_file_name(format!("journal.db{suffix}"));
        if p.exists() {
            let bytes = std::fs::read(&p).unwrap();
            assert!(
                !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
                "scrubbed plaintext found in {p:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// I9 — Rating fold under any interleaving.
// ---------------------------------------------------------------------------

#[test]
fn i09_rating_fold() {
    let ts = new_store();
    let (a, b) = (hash(1), hash(2));

    let r1 = ts
        .store
        .append(&ts.session, d_rating(3, vec![a.clone(), b.clone()]), None)
        .unwrap();
    let r2 = ts
        .store
        .append(&ts.session, d_rating(5, vec![a.clone()]), None)
        .unwrap();
    assert_eq!(ts.store.current_rating(&a).unwrap(), Some(5));
    assert_eq!(ts.store.current_rating(&b).unwrap(), Some(3));

    // Retracting the latest resurfaces the previous.
    ts.store
        .append(&ts.session, d_retract(r2.id), None)
        .unwrap();
    assert_eq!(ts.store.current_rating(&a).unwrap(), Some(3));
    // Retracting all returns to never-rated.
    ts.store
        .append(&ts.session, d_retract(r1.id), None)
        .unwrap();
    assert_eq!(ts.store.current_rating(&a).unwrap(), None);
    assert_eq!(ts.store.current_rating(&b).unwrap(), None);
    // value:0 is an explicit zero, distinct from never-rated.
    ts.store
        .append(&ts.session, d_rating(0, vec![a.clone()]), None)
        .unwrap();
    assert_eq!(ts.store.current_rating(&a).unwrap(), Some(0));

    // Randomized interleavings vs. an in-memory reference fold.
    let mut rng = StdRng::seed_from_u64(7);
    let images: Vec<ContentHash> = (10..14).map(hash).collect();
    // (event id order index, value, image indices, retracted)
    let mut model: Vec<(u8, Vec<usize>, bool)> = Vec::new();
    let mut ids: Vec<photoproof_core::EventId> = Vec::new();
    for _ in 0..100 {
        let live: Vec<usize> = model
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.2)
            .map(|(i, _)| i)
            .collect();
        if !live.is_empty() && rng.gen_bool(0.4) {
            let pick = live[rng.gen_range(0..live.len())];
            ts.store
                .append(&ts.session, d_retract(ids[pick].clone()), None)
                .unwrap();
            model[pick].2 = true;
        } else {
            let value = rng.gen_range(0..=5u8);
            let count = rng.gen_range(1..=images.len());
            let mut idx: Vec<usize> = (0..images.len()).collect();
            idx.shuffle(&mut rng);
            let chosen: Vec<usize> = idx.into_iter().take(count).collect();
            let targets: Vec<ContentHash> = chosen.iter().map(|&i| images[i].clone()).collect();
            let e = ts
                .store
                .append(&ts.session, d_rating(value, targets), None)
                .unwrap();
            ids.push(e.id);
            model.push((value, chosen, false));
        }
        // Compare per image: max-id (= insertion order) live rating.
        for (i, img) in images.iter().enumerate() {
            let expected = model
                .iter()
                .rev()
                .find(|(_, imgs, retracted)| !retracted && imgs.contains(&i))
                .map(|(v, _, _)| *v);
            assert_eq!(ts.store.current_rating(img).unwrap(), expected);
        }
    }
}

// ---------------------------------------------------------------------------
// I10 — Folds terminate: cycles inert; dangling chains activate on merge.
// ---------------------------------------------------------------------------

#[test]
fn i10_folds_terminate() {
    let ts = new_store();
    let a = hash(1);
    let m = ts
        .store
        .append(
            &ts.session,
            d_remark("original text", vec![a.clone()]),
            None,
        )
        .unwrap();
    let _r1 = ts
        .store
        .append(
            &ts.session,
            d_revision(m.id.clone(), "first correction"),
            None,
        )
        .unwrap();

    // A revision chain with a missing middle hop, ids minted after r1.
    let mid = ts.store.mint();
    let tail = ts.store.mint();
    let mk_rev = |minted: &Minted, target: photoproof_core::EventId, text: &str| Event {
        id: minted.id.clone(),
        v: 1,
        session_id: ts.session.clone(),
        ts: minted.ts,
        source: photoproof_core::Source::Typed,
        kind: Kind::Revision,
        targets: vec![],
        text: Some(text.into()),
        payload: None,
        target_event: Some(target),
        linked_event: None,
        redacted_by: None,
    };
    let rev_mid = mk_rev(&mid, m.id.clone(), "middle correction");
    let rev_tail = mk_rev(&tail, mid.id.clone(), "final correction");

    // Tail arrives first: its ancestor is missing — inert, not displayed.
    ts.store.merge(std::slice::from_ref(&rev_tail)).unwrap();
    let folded = ts.store.folded_journal(&a).unwrap();
    assert_eq!(folded.len(), 1);
    let JournalEntry::Remark { text, .. } = &folded[0] else {
        panic!("expected remark");
    };
    assert_eq!(text, "first correction", "dangling revision is inert");

    // The missing ancestor arrives: the chain activates, no repair pass.
    ts.store.merge(std::slice::from_ref(&rev_mid)).unwrap();
    let folded = ts.store.folded_journal(&a).unwrap();
    let JournalEntry::Remark {
        text, corrected, ..
    } = &folded[0]
    else {
        panic!("expected remark");
    };
    assert_eq!(text, "final correction");
    assert!(corrected);

    // A revision cycle (corrupt upstream data) is inert and terminates.
    let cx = ts.store.mint();
    let cy = ts.store.mint();
    let rev_x = mk_rev(&cx, cy.id.clone(), "cycle x");
    let rev_y = mk_rev(&cy, cx.id.clone(), "cycle y");
    ts.store.merge(&[rev_x, rev_y]).unwrap();
    // Folds must terminate and the cycle must not surface anywhere.
    let folded = ts.store.folded_journal(&a).unwrap();
    assert_eq!(folded.len(), 1);
    let session_fold = ts.store.folded_session(&ts.session).unwrap();
    assert!(
        session_fold
            .iter()
            .all(|e| !matches!(e, JournalEntry::Remark { text, .. } if text.contains("cycle")))
    );
}

// ---------------------------------------------------------------------------
// I11 — Dedupe: one event from N sidecars = one row, N target rows.
// ---------------------------------------------------------------------------

#[test]
fn i11_dedupe() {
    let ts = new_store();
    let g = Gen::new(ts.session.clone());
    let targets: Vec<ContentHash> = (1..=3).map(hash).collect();
    let e = g.remark("appears in three sidecars", targets);
    let bytes = canonical_json(&e);

    // Rebuild from its three sidecar copies: byte-identical canonical JSON.
    let copies: Vec<Event> = (0..3).map(|_| parse_canonical(&bytes).unwrap()).collect();
    let report = ts.store.merge(&copies).unwrap();
    assert_eq!(report.inserted, 1);
    assert_eq!(report.duplicates, 2);

    let conn = ts.raw_conn();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM annotation_events WHERE id = ?1",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    let target_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event_targets WHERE event_id = ?1",
            [e.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(target_rows, 3);
}

// ---------------------------------------------------------------------------
// I12 — Retraction folds out, fully.
// ---------------------------------------------------------------------------

#[test]
fn i12_retraction_folds_out() {
    let ts = new_store();
    let a = hash(1);
    let r = ts
        .store
        .append(
            &ts.session,
            d_remark("searchable zanzibar text", vec![a.clone()]),
            None,
        )
        .unwrap();
    let rating = ts
        .store
        .append(&ts.session, d_rating(4, vec![a.clone()]), None)
        .unwrap();
    let stroke = ts
        .store
        .append(&ts.session, d_stroke(a.clone(), None), None)
        .unwrap();
    let conn = ts.raw_conn();
    insert_vector(&conn, r.id.as_str());
    assert_eq!(fts_roots(&conn, "zanzibar"), vec![r.id.to_string()]);

    let retr = ts
        .store
        .append(&ts.session, d_retract(r.id.clone()), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_retract(rating.id.clone()), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_retract(stroke.id.clone()), None)
        .unwrap();

    // Raw reads and the sidecar feed keep everything.
    assert!(ts.store.raw_event(&r.id).unwrap().unwrap().text.is_some());
    let feed = ts.store.events_for_image(&a).unwrap();
    assert!(feed.iter().any(|e| e.id == r.id));
    assert!(feed.iter().any(|e| e.id == retr.id));

    // Folded journal: nothing (default view).
    assert!(ts.store.folded_journal(&a).unwrap().is_empty());
    // FTS row removed; vectors deleted; rating fold excludes it.
    assert!(fts_roots(&conn, "zanzibar").is_empty());
    assert_eq!(vector_count_for(&conn, r.id.as_str()), 0);
    assert_eq!(ts.store.current_rating(&a).unwrap(), None);
    // Stats fold: no live presence left.
    let stats: Option<i64> = conn
        .query_row(
            "SELECT event_count FROM image_journal_stats WHERE image_hash = ?1",
            [a.as_str()],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(stats, None);

    // Retraction-of-retraction: rejected at append…
    let err = ts
        .store
        .append(&ts.session, d_retract(retr.id.clone()), None)
        .unwrap_err();
    assert!(matches!(err, AppendError::RetractionOfRetraction));
    // …and quarantined at merge. Retracting a redaction is also invalid.
    let g = Gen::new(ts.session.clone());
    let bad = g.retraction(retr.id.clone());
    let report = ts.store.merge(&[bad]).unwrap();
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.inserted, 0);
}

// ---------------------------------------------------------------------------
// I13 — Order discipline: journal order = ascending ULID, never ts.
// ---------------------------------------------------------------------------

#[test]
fn i13_order_discipline() {
    let ts = new_store();
    let a = hash(1);
    let now = UtcMillis::now().epoch_ms();
    let m1 = ts.store.mint();
    let m2 = ts.store.mint();
    assert!(m2.id > m1.id);
    // The second event testifies to an *earlier* wall-clock time (NTP step).
    let first = ts
        .store
        .append(
            &ts.session,
            d_remark("first by id, later ts", vec![a.clone()]),
            Some(Minted {
                id: m1.id,
                ts: UtcMillis::from_epoch_ms(now + 5_000),
            }),
        )
        .unwrap();
    let second = ts
        .store
        .append(
            &ts.session,
            d_remark("second by id, earlier ts", vec![a.clone()]),
            Some(Minted {
                id: m2.id,
                ts: UtcMillis::from_epoch_ms(now - 60_000),
            }),
        )
        .unwrap();
    assert!(
        second.ts < first.ts,
        "regressed ts is preserved as testimony"
    );

    let folded = ts.store.folded_journal(&a).unwrap();
    let texts: Vec<&str> = folded
        .iter()
        .map(|e| match e {
            JournalEntry::Remark { text, .. } => text.as_str(),
            _ => panic!("remarks only"),
        })
        .collect();
    assert_eq!(
        texts,
        vec!["first by id, later ts", "second by id, earlier ts"],
        "events are never re-sorted by ts"
    );
    let feed = ts.store.events_for_image(&a).unwrap();
    assert!(feed.windows(2).all(|w| w[0].id < w[1].id));
}

// ---------------------------------------------------------------------------
// I14 — Monotonic mint, including across a wall-clock regression.
// ---------------------------------------------------------------------------

#[test]
fn i14_monotonic_mint() {
    let minter = Minter::new();
    let t0 = 1_780_000_000_000i64;
    // Steps include same-ms bursts, small regressions, and a > 60 s cull.
    let offsets = [
        0i64, 0, 0, 1, 1, 2, -5, -5, 3, 4, -120_000, -120_000, -119_999, 5, 6, 0, 7,
    ];
    let mut last: Option<photoproof_core::EventId> = None;
    for off in offsets {
        let m = minter.mint_at(UtcMillis::from_epoch_ms(t0 + off));
        if let Some(prev) = &last {
            assert!(m.id > *prev, "ids strictly increase: {prev} then {}", m.id);
        }
        // ts is testimony: it follows the (possibly regressed) clock.
        assert_eq!(m.ts.epoch_ms(), t0 + off);
        last = Some(m.id);
    }
}

// ---------------------------------------------------------------------------
// I15 — Scrubbed shape.
// ---------------------------------------------------------------------------

#[test]
fn i15_scrubbed_shape() {
    let ts = new_store();
    let a = hash(1);
    let v = ts
        .store
        .append(
            &ts.session,
            d_voice("private words", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let st = ts
        .store
        .append(&ts.session, d_stroke(a.clone(), Some(v.id.clone())), None)
        .unwrap();
    ts.store.redact(&v.id).unwrap();
    ts.store.redact(&st.id).unwrap();

    for (original, kept_link) in [(v.clone(), None), (st.clone(), Some(v.id.clone()))] {
        let e = ts.store.raw_event(&original.id).unwrap().unwrap();
        assert_eq!(e.id, original.id);
        assert_eq!(e.v, original.v);
        assert_eq!(e.session_id, original.session_id);
        assert_eq!(e.ts, original.ts);
        assert_eq!(e.source, original.source);
        assert_eq!(e.kind, original.kind);
        assert_eq!(e.targets, original.targets);
        assert_eq!(e.target_event, original.target_event);
        assert_eq!(e.linked_event, kept_link, "structural refs survive");
        assert!(e.text.is_none() && e.payload.is_none());
        assert!(e.redacted_by.is_some());

        let json = String::from_utf8(canonical_json(&e)).unwrap();
        assert!(json.contains("\"redacted_by\":"));
        assert!(!json.contains("\"text\":"));
        assert!(!json.contains("\"payload\":"));
        // Scrubbed canonical form round-trips byte-exactly.
        let parsed = parse_canonical(json.as_bytes()).unwrap();
        assert_eq!(canonical_json(&parsed), json.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// I16 — Fold reads are batched (≤ 3 SELECTs per §10.1) and fast.
// ---------------------------------------------------------------------------

#[test]
fn i16_fold_reads_batched() {
    let ts = new_store();
    let a = hash(1);
    let mut remark_ids = Vec::new();
    for i in 0..10 {
        let e = ts
            .store
            .append(
                &ts.session,
                d_remark(&format!("note {i}"), vec![a.clone()]),
                None,
            )
            .unwrap();
        remark_ids.push(e.id);
    }
    for i in 0..10 {
        ts.store
            .append(&ts.session, d_rating(i % 6, vec![a.clone()]), None)
            .unwrap();
    }
    for _ in 0..5 {
        ts.store
            .append(&ts.session, d_stroke(a.clone(), None), None)
            .unwrap();
    }
    for id in remark_ids.iter().take(5) {
        ts.store
            .append(&ts.session, d_retract(id.clone()), None)
            .unwrap();
    }

    // §10.1: covering scan + batched id fetch + one meta-closure round.
    let before = photoproof_core::store::read_select_count();
    let folded = ts.store.folded_journal(&a).unwrap();
    let used = photoproof_core::store::read_select_count() - before;
    assert!(
        used <= 3,
        "folded_journal used {used} SELECTs (max 3, §10.1)"
    );
    assert_eq!(folded.len(), 20); // 5 remarks + 10 ratings + 5 strokes live

    let before = photoproof_core::store::read_select_count();
    ts.store.events_for_image(&a).unwrap();
    let used = photoproof_core::store::read_select_count() - before;
    assert!(
        used <= 3,
        "events_for_image used {used} SELECTs (max 3, §10.1)"
    );

    // With a revision chain present the fixpoint needs exactly one more
    // round — still batched, never per-event probes.
    ts.store
        .append(
            &ts.session,
            d_revision(remark_ids[6].clone(), "corrected"),
            None,
        )
        .unwrap();
    let before = photoproof_core::store::read_select_count();
    ts.store.folded_journal(&a).unwrap();
    let used = photoproof_core::store::read_select_count() - before;
    assert!(used <= 4, "one extra closure round max, got {used}");
}

#[test]
fn i16_fold_5k_events_timing() {
    let ts = new_store();
    let a = hash(1);
    let g = Gen::new(ts.session.clone());
    let mut events: Vec<Event> = Vec::with_capacity(5_000);
    let mut remark_ids = Vec::new();
    for i in 0..4_200 {
        let e = g.remark(
            &format!("worst case remark number {i} about this image"),
            vec![a.clone()],
        );
        remark_ids.push(e.id.clone());
        events.push(e);
    }
    for i in 0..300 {
        events.push(g.rating((i % 6) as u8, vec![a.clone()]));
    }
    for _ in 0..300 {
        events.push(g.stroke(a.clone(), None));
    }
    for id in remark_ids.iter().take(150) {
        events.push(g.retraction(id.clone()));
    }
    for id in remark_ids.iter().skip(150).take(50) {
        events.push(g.revision(id.clone(), "corrected text"));
    }
    assert_eq!(events.len(), 5_000);
    let report = ts.store.merge(&events).unwrap();
    assert_eq!(report.inserted, 5_000);

    // Warm caches, then take the best of 5 folds.
    let warm = ts.store.folded_journal(&a).unwrap();
    assert_eq!(warm.len(), 4_200 - 150 + 300 + 300);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let folded = ts.store.folded_journal(&a).unwrap();
        let dt = t.elapsed();
        assert_eq!(folded.len(), warm.len());
        best = best.min(dt);
    }
    // Spec criterion: < 10 ms on the reference machine (release build).
    // Debug builds get headroom — the assertion still catches any N+1
    // regression, which is orders of magnitude slower.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(100)
    } else {
        Duration::from_millis(10)
    };
    assert!(
        best < budget,
        "5k-event fold took {best:?} (budget {budget:?})"
    );
    println!("5k-event fold best-of-5: {best:?}");
}

// ---------------------------------------------------------------------------
// Order-shuffled merge property (gate requirement; subsumed by I6 but kept
// as an explicitly-named test): merge is set-union by event id and the
// result is independent of insertion order.
// ---------------------------------------------------------------------------

#[test]
fn merge_order_shuffle_property() {
    let mut rng = StdRng::seed_from_u64(99);
    let events = random_events(&mut rng);
    let mut reference: Option<Vec<String>> = None;
    for round in 0..4 {
        let mut shuffled = events.clone();
        shuffled.shuffle(&mut rng);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.db");
        {
            let store = EventStore::open(&path).unwrap();
            store.merge(&shuffled).unwrap();
        }
        let state = full_state(&path);
        match &reference {
            None => reference = Some(state),
            Some(r) => assert_eq!(&state, r, "round {round}: order must not matter"),
        }
    }
}
