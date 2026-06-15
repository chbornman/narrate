//! spec/EVENTS.md behavioral coverage beyond the §11 invariants: sessions
//! (§9, E7), the sidecar dirty queue (§5.3), FTS lifecycle (§5.4, §6.3),
//! link symmetry (§3.3, X2), and append/redact rejection rules (§3, §10).

mod common;

use common::*;
use photoproof_core::{
    AppendError, DwellSource, Event, EventDraft, JournalEntry, Kind, RedactError, RemarkSource,
    SessionContext, SessionId, SessionRecord, StoreError, UtcMillis, canonical_json,
};

// -- sessions (§9, E7) --------------------------------------------------------

#[test]
fn sessions_open_close_and_merge() {
    let ts = new_store();
    let rec = ts.store.session(&ts.session).unwrap().unwrap();
    assert_eq!(rec.app_version, "0.1.0");
    assert_eq!(rec.device_id, DEVICE_ID);
    assert!(rec.ended_ts.is_none());

    // Close once; closing again is a no-op (ended_ts set once).
    let ended = UtcMillis::now();
    ts.store.close_session(&ts.session, ended).unwrap();
    let rec = ts.store.session(&ts.session).unwrap().unwrap();
    assert_eq!(rec.ended_ts, Some(ended));
    ts.store
        .close_session(
            &ts.session,
            UtcMillis::from_epoch_ms(ended.epoch_ms() + 9999),
        )
        .unwrap();
    let rec2 = ts.store.session(&ts.session).unwrap().unwrap();
    assert_eq!(rec2.ended_ts, Some(ended), "ended_ts is set once");

    // Unknown session errors.
    let ghost = SessionId::from_str_strict("01JZ5C3HW0RD8PT2M6QK4V9XEB").unwrap();
    assert!(ts.store.close_session(&ghost, ended).is_err());

    // Merge: union by id; ended_ts := max(non-NULL); immutable-field
    // mismatch keeps local and warns.
    let other = SessionRecord {
        id: ghost.clone(),
        started_ts: UtcMillis::from_epoch_ms(1_700_000_000_000),
        ended_ts: None,
        app_version: "0.0.9".into(),
        device_id: DEVICE_ID.into(),
        root_context: Some(r#"{"roots":["r1"],"focus_path":"2026/iceland"}"#.into()),
    };
    let warnings = ts
        .store
        .merge_sessions(std::slice::from_ref(&other))
        .unwrap();
    assert!(warnings.is_empty());
    assert_eq!(ts.store.session(&ghost).unwrap().unwrap(), other);

    let later = UtcMillis::from_epoch_ms(1_700_000_100_000);
    let mut update = other.clone();
    update.ended_ts = Some(later);
    let warnings = ts.store.merge_sessions(&[update]).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(
        ts.store.session(&ghost).unwrap().unwrap().ended_ts,
        Some(later)
    );

    // max(non-NULL): an older ended_ts does not regress the local one.
    let mut older = other.clone();
    older.ended_ts = Some(UtcMillis::from_epoch_ms(1_700_000_050_000));
    ts.store.merge_sessions(&[older]).unwrap();
    assert_eq!(
        ts.store.session(&ghost).unwrap().unwrap().ended_ts,
        Some(later)
    );

    // Immutable mismatch: local wins, warned.
    let mut conflicting = other.clone();
    conflicting.app_version = "9.9.9".into();
    let warnings = ts.store.merge_sessions(&[conflicting]).unwrap();
    assert_eq!(warnings, vec![ghost.clone()]);
    assert_eq!(
        ts.store.session(&ghost).unwrap().unwrap().app_version,
        "0.0.9"
    );

    // device_id is validated at open_session.
    let err = ts.store.open_session(SessionContext {
        app_version: "0.1.0".into(),
        device_id: "NOT-HEX".into(),
        root_context: None,
    });
    assert!(err.is_err());
}

// -- sidecar dirty queue (§5.3, §7 step 7) -------------------------------------

#[test]
fn dirty_queue_priority_and_ack() {
    let ts = new_store();
    let (a, b) = (hash(1), hash(2));
    let r = ts
        .store
        .append(
            &ts.session,
            d_remark("note", vec![a.clone(), b.clone()]),
            None,
        )
        .unwrap();

    let dirty = ts.store.dirty_images().unwrap();
    assert_eq!(dirty.len(), 2);
    assert!(
        dirty
            .iter()
            .all(|d| d.reason == photoproof_core::DirtyReason::Append)
    );

    // Ack image b: row removed only via ack.
    let upto = dirty.iter().find(|d| d.image == b).unwrap().since_ts;
    ts.store.ack_dirty(&b, upto).unwrap();
    let dirty = ts.store.dirty_images().unwrap();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].image, a);

    // Redaction outranks and overwrites 'append'.
    ts.store.redact(&r.id).unwrap();
    let dirty = ts.store.dirty_images().unwrap();
    assert!(
        dirty
            .iter()
            .all(|d| d.reason == photoproof_core::DirtyReason::Redaction)
    );
    // 'redaction' rows are first-priority and never coalesced away by later
    // writes (§5.3): an append on the same image keeps the redaction reason.
    ts.store
        .append(&ts.session, d_remark("later note", vec![a.clone()]), None)
        .unwrap();
    let row_a = ts
        .store
        .dirty_images()
        .unwrap()
        .into_iter()
        .find(|d| d.image == a)
        .unwrap();
    assert_eq!(row_a.reason, photoproof_core::DirtyReason::Redaction);
    // And redaction rows order first.
    assert_eq!(
        ts.store.dirty_images().unwrap()[0].reason,
        photoproof_core::DirtyReason::Redaction
    );
}

// -- FTS lifecycle (§5.4, §6.3, P3) ---------------------------------------------

#[test]
fn fts_tracks_effective_text_per_chain_root() {
    let ts = new_store();
    let a = hash(1);
    let v = ts
        .store
        .append(
            &ts.session,
            d_voice("that muddy light", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let conn = ts.raw_conn();
    assert_eq!(fts_roots(&conn, "muddy"), vec![v.id.to_string()]);

    // Revision: the root's single FTS row now carries the corrected text.
    let rev = ts
        .store
        .append(
            &ts.session,
            d_revision(v.id.clone(), "that moody light"),
            None,
        )
        .unwrap();
    assert!(
        fts_roots(&conn, "muddy").is_empty(),
        "original text not indexed"
    );
    assert_eq!(
        fts_roots(&conn, "moody"),
        vec![v.id.to_string()],
        "keyed by root"
    );

    // Revision-of-revision: latest id wins; still one row.
    let rev2 = ts
        .store
        .append(
            &ts.session,
            d_revision(rev.id.clone(), "that smoky light"),
            None,
        )
        .unwrap();
    assert_eq!(fts_roots(&conn, "smoky"), vec![v.id.to_string()]);
    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM event_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(row_count, 1, "one FTS row per chain root");

    // Retracting the latest revision resurfaces the previous correction.
    ts.store
        .append(&ts.session, d_retract(rev2.id), None)
        .unwrap();
    assert_eq!(fts_roots(&conn, "moody"), vec![v.id.to_string()]);
    assert!(fts_roots(&conn, "smoky").is_empty());

    // Retracting the root removes the row entirely.
    ts.store
        .append(&ts.session, d_retract(v.id.clone()), None)
        .unwrap();
    assert!(fts_roots(&conn, "moody").is_empty());
    assert!(fts_roots(&conn, "light").is_empty());
    // fts_map rows are never deleted (stable rowids).
    let map_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM fts_map", [], |r| r.get(0))
        .unwrap();
    assert_eq!(map_count, 1);
}

#[test]
fn session_level_remarks_are_indexed_and_exported() {
    let ts = new_store();
    let e = ts
        .store
        .append(&ts.session, d_remark("freestanding thought", vec![]), None)
        .unwrap();
    let conn = ts.raw_conn();
    assert_eq!(fts_roots(&conn, "freestanding"), vec![e.id.to_string()]);

    // Zero-effective-target feed (§2.3): includes the session-level remark
    // and a dangling meta-event; excludes targeted events.
    ts.store
        .append(&ts.session, d_remark("targeted", vec![hash(1)]), None)
        .unwrap();
    let dangling_target = ts.store.mint();
    let m = ts.store.mint();
    let dangling = photoproof_core::Event {
        id: m.id,
        v: 1,
        session_id: ts.session.clone(),
        ts: m.ts,
        source: photoproof_core::Source::Typed,
        kind: Kind::Revision,
        targets: vec![],
        text: Some("revision of nothing yet".into()),
        payload: None,
        target_event: Some(dangling_target.id),
        linked_event: None,
        redacted_by: None,
    };
    ts.store.merge(std::slice::from_ref(&dangling)).unwrap();

    let sl = ts.store.sessionlevel_events(&ts.session).unwrap();
    let ids: Vec<_> = sl.iter().map(|e| e.id.to_string()).collect();
    assert!(ids.contains(&e.id.to_string()));
    assert!(ids.contains(&dangling.id.to_string()));
    assert_eq!(sl.len(), 2);

    // The session fold shows it; the journal stays ULID-ordered.
    let folded = ts.store.folded_session(&ts.session).unwrap();
    assert!(
        matches!(&folded[0], JournalEntry::Remark { text, .. } if text == "freestanding thought")
    );
}

// -- stroke↔utterance link symmetry (§3.3, X2) -----------------------------------

#[test]
fn linked_event_resolves_in_both_directions() {
    let ts = new_store();
    let a = hash(1);

    // Speak first, circle after: the stroke carries the backward link.
    let v1 = ts
        .store
        .append(
            &ts.session,
            d_voice("look at the hand", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let s1 = ts
        .store
        .append(&ts.session, d_stroke(a.clone(), Some(v1.id.clone())), None)
        .unwrap();

    // Circle first, speak after: the voice remark carries the backward link.
    let s2 = ts
        .store
        .append(&ts.session, d_stroke(a.clone(), None), None)
        .unwrap();
    ts.store
        .append(
            &ts.session,
            EventDraft::Remark {
                source: RemarkSource::Voice {
                    conf_pm: Some(950),
                    dur_ms: 900,
                    linked_event: Some(s2.id.clone()),
                },
                text: "and that edge".into(),
                targets: vec![a.clone()],
            },
            None,
        )
        .unwrap();

    let folded = ts.store.folded_journal(&a).unwrap();
    let stroke_links: Vec<(String, Option<String>)> = folded
        .iter()
        .filter_map(|e| match e {
            JournalEntry::Stroke {
                id, linked_event, ..
            } => Some((id.to_string(), linked_event.as_ref().map(|l| l.to_string()))),
            _ => None,
        })
        .collect();
    assert_eq!(stroke_links.len(), 2);
    // s1: forward (stored on the stroke itself).
    assert_eq!(
        stroke_links[0],
        (s1.id.to_string(), Some(v1.id.to_string()))
    );
    // s2: reverse (stored on the later voice remark, resolved at fold time).
    assert_eq!(stroke_links[1].0, s2.id.to_string());
    assert!(stroke_links[1].1.is_some(), "reverse link resolved");
}

// -- append/redact rejection rules (§2–3, §10) -------------------------------------

#[test]
fn append_and_redact_rejections() {
    let ts = new_store();
    let a = hash(1);
    let r = ts
        .store
        .append(&ts.session, d_remark("a note", vec![a.clone()]), None)
        .unwrap();
    let rating = ts
        .store
        .append(&ts.session, d_rating(4, vec![a.clone()]), None)
        .unwrap();

    // Rating with no targets: invalid (§2.3).
    assert!(matches!(
        ts.store.append(&ts.session, d_rating(3, vec![]), None),
        Err(AppendError::Validation(_))
    ));
    // Rating value > 5.
    assert!(matches!(
        ts.store
            .append(&ts.session, d_rating(6, vec![a.clone()]), None),
        Err(AppendError::Validation(_))
    ));
    // Empty remark text.
    assert!(matches!(
        ts.store
            .append(&ts.session, d_remark("   ", vec![a.clone()]), None),
        Err(AppendError::Validation(_))
    ));
    // Voice confidence out of range.
    assert!(matches!(
        ts.store.append(
            &ts.session,
            EventDraft::Remark {
                source: RemarkSource::Voice {
                    conf_pm: Some(1001),
                    dur_ms: 10,
                    linked_event: None
                },
                text: "x".into(),
                targets: vec![],
            },
            None
        ),
        Err(AppendError::Validation(_))
    ));
    // Revision of a rating: invalid target kind (§3.4).
    assert!(matches!(
        ts.store
            .append(&ts.session, d_revision(rating.id.clone(), "x"), None),
        Err(AppendError::InvalidTargetKind { .. })
    ));
    // Revision of a missing event: rejected at append (merge is the dangling
    // path, not append).
    let ghost = ts.store.mint();
    assert!(matches!(
        ts.store
            .append(&ts.session, d_revision(ghost.id.clone(), "x"), None),
        Err(AppendError::TargetNotFound(_))
    ));
    // Duplicate minted id.
    let m = ts.store.mint();
    ts.store
        .append(&ts.session, d_remark("first", vec![]), Some(m.clone()))
        .unwrap();
    assert!(matches!(
        ts.store
            .append(&ts.session, d_remark("again", vec![]), Some(m)),
        Err(AppendError::DuplicateId(_))
    ));

    // Redaction of a rating: not permitted — retract instead (§3.2, §3.6).
    assert!(matches!(
        ts.store.redact(&rating.id),
        Err(RedactError::InvalidKind(Kind::Rating))
    ));
    // Redacting twice: AlreadyScrubbed.
    ts.store.redact(&r.id).unwrap();
    assert!(matches!(
        ts.store.redact(&r.id),
        Err(RedactError::AlreadyScrubbed(_))
    ));
    // Redacting a retraction: invalid kind.
    let r2 = ts
        .store
        .append(&ts.session, d_remark("note two", vec![a.clone()]), None)
        .unwrap();
    let retr = ts
        .store
        .append(&ts.session, d_retract(r2.id), None)
        .unwrap();
    assert!(matches!(
        ts.store.redact(&retr.id),
        Err(RedactError::InvalidKind(Kind::Retraction))
    ));

    // Merge quarantines a source×kind matrix violation (§2.2).
    let g = Gen::new(ts.session.clone());
    let mut bad = g.rating(3, vec![a.clone()]);
    bad.source = photoproof_core::Source::Voice; // spoken ratings: not v1
    let report = ts.store.merge(&[bad]).unwrap();
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.inserted, 0);
}

// -- §8 batching discipline: > 10k merges commit in chunks, derived state in
// -- a single pass after the union, finished with ANALYZE + FTS optimize.

#[test]
fn merge_large_batch_uses_chunked_path() {
    let ts = new_store();
    let g = Gen::new(ts.session.clone());
    let images: Vec<_> = (1..=3).map(hash).collect();
    let mut events = Vec::with_capacity(10_500);
    let mut remark_ids = Vec::new();
    for i in 0..10_400 {
        let e = g.remark(
            &format!("bulk note {i}"),
            vec![images[i % images.len()].clone()],
        );
        remark_ids.push(e.id.clone());
        events.push(e);
    }
    // Sprinkle meta-events so the post-union recompute crosses chunks.
    for id in remark_ids.iter().take(50) {
        events.push(g.retraction(id.clone()));
    }
    for id in remark_ids.iter().skip(50).take(25) {
        events.push(g.revision(id.clone(), "bulk corrected"));
    }
    for id in remark_ids.iter().skip(75).take(25) {
        events.push(g.redaction(id.clone()));
    }
    let report = ts.store.merge(&events).unwrap();
    assert_eq!(report.inserted, events.len());
    assert!(report.quarantined.is_empty());

    let conn = ts.raw_conn();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM annotation_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, events.len() as i64);
    // Live FTS rows = remarks − retracted − redacted.
    let fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM event_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts, 10_400 - 50 - 25);
    assert_eq!(fts_roots(&conn, "corrected").len(), 25);
    // Scrubbed victims learned their markers.
    let scrubbed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM annotation_events WHERE redacted_by IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scrubbed, 25);
    // Fold still works over the merged state.
    let folded = ts.store.folded_journal(&images[0]).unwrap();
    assert!(!folded.is_empty());
    // And the result equals incremental-vs-rebuild (I7) on this path too.
    let before = dump_derived(&conn, true);
    ts.store.rebuild_derived().unwrap();
    assert_eq!(dump_derived(&conn, true), before);
}

// -- §8 step-3 corruption defenses (positive coverage) -------------------------

#[test]
fn merge_structural_mismatch_local_wins_and_warns() {
    let ts = new_store();
    let a = hash(1);
    let local = ts
        .store
        .append(
            &ts.session,
            d_remark("the local truth", vec![a.clone()]),
            None,
        )
        .unwrap();

    // Same id, structurally different content: corruption upstream — the
    // local row wins and the report carries an integrity warning.
    let mut impostor = local.clone();
    impostor.text = Some("upstream corruption".into());
    impostor.targets = vec![hash(2)];
    let report = ts.store.merge(&[impostor]).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.newly_scrubbed, 0);
    assert_eq!(report.integrity_warnings, vec![local.id.clone()]);

    let e = ts.store.raw_event(&local.id).unwrap().unwrap();
    assert_eq!(e.text.as_deref(), Some("the local truth"), "local wins");
    assert_eq!(e.targets, vec![a], "local target rows untouched");
}

#[test]
fn merge_incoming_scrubbed_form_scrubs_unscrubbed_local() {
    // §8 step 3 "learn it": an incoming scrubbed-FORM event (redacted_by
    // set) with NO redaction event anywhere — not in the batch, not in the
    // store — must still scrub the unscrubbed local row in place.
    let ts = new_store();
    let a = hash(1);
    let v = ts
        .store
        .append(
            &ts.session,
            d_voice("searchable bergamot words", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let conn = ts.raw_conn();
    insert_vector(&conn, v.id.as_str());
    assert_eq!(fts_roots(&conn, "bergamot"), vec![v.id.to_string()]);

    let marker = ts.store.mint().id; // no redaction event carries this id
    let scrubbed = v.scrubbed_form(marker.clone());
    let report = ts.store.merge(&[scrubbed]).unwrap();
    assert_eq!(report.newly_scrubbed, 1);
    assert_eq!(report.inserted, 0);
    assert!(report.integrity_warnings.is_empty());

    let e = ts.store.raw_event(&v.id).unwrap().unwrap();
    assert!(e.scrubbed() && e.text.is_none() && e.payload.is_none());
    assert_eq!(e.redacted_by, Some(marker), "marker learned from the form");
    // Purged from FTS and vectors; folds to a stub; dirty mark escalated.
    assert!(fts_roots(&conn, "bergamot").is_empty());
    assert_eq!(vector_count_for(&conn, v.id.as_str()), 0);
    let folded = ts.store.folded_journal(&a).unwrap();
    assert!(matches!(folded[0], JournalEntry::RedactedStub { .. }));
    let row = ts
        .store
        .dirty_images()
        .unwrap()
        .into_iter()
        .find(|d| d.image == a)
        .unwrap();
    assert_eq!(row.reason, photoproof_core::DirtyReason::Redaction);
}

// -- dirty-queue ack high-water mark (§5.3, P1) ---------------------------------

#[test]
fn ack_dirty_cannot_lose_newer_marks() {
    let ts = new_store();
    let a = hash(1);
    ts.store
        .append(&ts.session, d_remark("first", vec![a.clone()]), None)
        .unwrap();
    let t1 = ts
        .store
        .dirty_images()
        .unwrap()
        .into_iter()
        .find(|d| d.image == a)
        .unwrap()
        .since_ts;

    // A newer mark lands after the sidecar writer read the queue — possibly
    // within the same wall-clock millisecond. since_ts must advance
    // strictly, or the ack below would delete dirtiness it never saw.
    ts.store
        .append(&ts.session, d_remark("second", vec![a.clone()]), None)
        .unwrap();
    let t2 = ts
        .store
        .dirty_images()
        .unwrap()
        .into_iter()
        .find(|d| d.image == a)
        .unwrap()
        .since_ts;
    assert!(
        t2 > t1,
        "since_ts advances strictly on re-mark ({t1} -> {t2})"
    );

    // The ack computed from the stale read must not delete the newer mark.
    ts.store.ack_dirty(&a, t1).unwrap();
    assert!(
        ts.store
            .dirty_images()
            .unwrap()
            .iter()
            .any(|d| d.image == a),
        "ack with a stale since_ts lost a newer dirty mark"
    );
    // Acking the observed high-water mark clears the row.
    ts.store.ack_dirty(&a, t2).unwrap();
    assert!(
        ts.store
            .dirty_images()
            .unwrap()
            .iter()
            .all(|d| d.image != a)
    );
}

// -- blocked wal_checkpoint(TRUNCATE) surfaces (§5.1, §7 step 8, P2) -------------

#[test]
fn blocked_wal_checkpoint_is_an_error_not_silent() {
    let ts = new_store();
    let a = hash(1);
    let v = ts
        .store
        .append(&ts.session, d_voice("private words", vec![a], None), None)
        .unwrap();

    // A long-lived reader holding an open read transaction blocks the WAL
    // truncation (exactly what §5.1 forbids UI code from doing).
    let blocker = ts.raw_conn();
    blocker.execute_batch("BEGIN").unwrap();
    let _: i64 = blocker
        .query_row("SELECT COUNT(*) FROM annotation_events", [], |r| r.get(0))
        .unwrap();

    // redact() must surface the failed §7-step-8 hygiene, not swallow it.
    let err = ts.store.redact(&v.id).unwrap_err();
    assert!(
        matches!(err, RedactError::Store(StoreError::CheckpointBlocked)),
        "expected CheckpointBlocked, got {err:?}"
    );
    // The scrub itself is committed and durable; only WAL hygiene is pending.
    assert!(ts.store.raw_event(&v.id).unwrap().unwrap().scrubbed());

    // Once the reader releases, idle maintenance completes the truncation.
    blocker.execute_batch("ROLLBACK").unwrap();
    ts.store.maintain().unwrap();
    let wal_len = std::fs::metadata(ts.path().with_file_name("journal.db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(wal_len, 0, "WAL truncated once unblocked");
}

/// Redaction-supremacy recovery (§7-step-8): if a checkpoint stays blocked all
/// the way through shutdown (a reader never releases before drop), the scrubbed
/// plaintext lingers in `-wal`. The NEXT `open` must recover it: at open this is
/// the first connection, so its startup checkpoint truncates the leaked WAL.
#[test]
fn open_recovers_a_shutdown_blocked_wal_checkpoint() {
    let ts = new_store();
    let a = hash(2);
    let v = ts
        .store
        .append(
            &ts.session,
            d_voice("secret shutdown words", vec![a], None),
            None,
        )
        .unwrap();

    // A reader holding a snapshot blocks the checkpoint at redact AND at drop.
    let blocker = ts.raw_conn();
    blocker.execute_batch("BEGIN").unwrap();
    let _: i64 = blocker
        .query_row("SELECT COUNT(*) FROM annotation_events", [], |r| r.get(0))
        .unwrap();
    assert!(matches!(
        ts.store.redact(&v.id).unwrap_err(),
        RedactError::Store(StoreError::CheckpointBlocked)
    ));

    // Drop the store WHILE the reader still blocks: the Drop checkpoint cannot
    // truncate either, so the scrubbed plaintext lingers in -wal (the leak).
    let common::TestStore { dir, store, .. } = ts;
    drop(store);
    let wal_path = dir.path().join("journal.db-wal");
    let leaked = std::fs::read(&wal_path).unwrap_or_default();
    assert!(
        leaked
            .windows(b"secret shutdown words".len())
            .any(|w| w == b"secret shutdown words"),
        "precondition: the blocked shutdown left scrubbed plaintext in -wal"
    );

    // Reader releases; the next open() runs the recovery checkpoint.
    blocker.execute_batch("ROLLBACK").unwrap();
    drop(blocker);
    let reopened = photoproof_core::EventStore::open(dir.path().join("journal.db")).unwrap();
    assert_eq!(
        std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0),
        0,
        "open() recovers the leaked -wal by truncating it"
    );
    assert!(
        reopened.raw_event(&v.id).unwrap().unwrap().scrubbed(),
        "the recovered db still reflects the scrub"
    );
}

// -- idempotent re-merge heals an interrupted merge (§8, P3) ---------------------

fn payload_column(e: &Event) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&canonical_json(e)).unwrap();
    v.get("payload").map(|p| serde_json::to_string(p).unwrap())
}

/// Insert truth rows exactly as a committed merge chunk would, bypassing all
/// derived maintenance — the on-disk state after a crash between the §8
/// truth-union transactions and the final derived recompute.
fn raw_insert_truth(conn: &rusqlite::Connection, e: &Event) {
    conn.execute(
        "INSERT INTO annotation_events \
         (id, v, session_id, ts, source, kind, text, payload, target_event, linked_event, redacted_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            e.id.as_str(),
            e.v as i64,
            e.session_id.as_str(),
            e.ts.to_rfc3339(),
            e.source.as_str(),
            e.kind.as_str(),
            e.text,
            payload_column(e),
            e.target_event.as_ref().map(|t| t.as_str().to_owned()),
            e.linked_event.as_ref().map(|t| t.as_str().to_owned()),
            e.redacted_by.as_ref().map(|t| t.as_str().to_owned()),
        ],
    )
    .unwrap();
    for (i, h) in e.targets.iter().enumerate() {
        conn.execute(
            "INSERT INTO event_targets (event_id, image_hash, position) VALUES (?1, ?2, ?3)",
            rusqlite::params![e.id.as_str(), h.as_str(), i as i64],
        )
        .unwrap();
    }
}

#[test]
fn re_merge_heals_interrupted_merge() {
    let ts = new_store();
    let g = Gen::new(ts.session.clone());
    let (a, b) = (hash(1), hash(2));
    let m1 = g.remark("healable zeppelin note", vec![a.clone()]);
    let m2 = g.voice_remark("verbal cormorant note", vec![a.clone(), b.clone()]);
    let m3 = g.remark("condemned heliotrope note", vec![b.clone()]);
    let rating = g.rating(4, vec![b.clone()]);
    let rev = g.revision(m1.id.clone(), "healable zeppelin note corrected");
    let retr = g.retraction(m2.id.clone());
    let red = g.redaction(m3.id.clone());
    let events = vec![m1.clone(), m2.clone(), m3.clone(), rating, rev, retr, red];

    // Simulate the interruption: truth rows committed, derived state never
    // recomputed (even the per-chunk scrub of m3 is missing — strictly worse
    // than any real crash point).
    let conn = ts.raw_conn();
    for e in &events {
        raw_insert_truth(&conn, e);
    }
    assert!(
        fts_roots(&conn, "zeppelin").is_empty(),
        "derived state stale"
    );
    assert_eq!(ts.store.current_rating(&b).unwrap(), None);
    assert!(!ts.store.raw_event(&m3.id).unwrap().unwrap().scrubbed());

    // The idempotent re-merge of the same events must heal everything:
    // duplicates may not short-circuit the recompute.
    let report = ts.store.merge(&events).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.duplicates, events.len());
    assert_eq!(report.newly_scrubbed, 1, "m3's pending scrub applied");
    assert!(report.quarantined.is_empty());

    assert_eq!(fts_roots(&conn, "corrected"), vec![m1.id.to_string()]);
    assert!(
        fts_roots(&conn, "cormorant").is_empty(),
        "retraction folded"
    );
    assert!(
        fts_roots(&conn, "heliotrope").is_empty(),
        "redaction purged"
    );
    assert!(ts.store.raw_event(&m3.id).unwrap().unwrap().scrubbed());
    assert_eq!(ts.store.current_rating(&b).unwrap(), Some(4));

    // Healed incremental state equals a from-scratch rebuild (I7).
    let before = dump_derived(&conn, true);
    ts.store.rebuild_derived().unwrap();
    assert_eq!(dump_derived(&conn, true), before);
}

// -- redaction closure over dangling chains (§3.6, §7 step 2, P4) ----------------

#[test]
fn redact_dangling_chain_scrubs_reachable_members() {
    let ts = new_store();
    let g = Gen::new(ts.session.clone());

    // A revision chain whose ancestor sits on an unplugged drive: ghost is
    // never inserted; r1 arrives via merge (dangling, inert); r2 is a local
    // correction of r1.
    let ghost = ts.store.mint();
    let r1 = g.revision(ghost.id.clone(), "dangling secret one");
    ts.store.merge(std::slice::from_ref(&r1)).unwrap();
    let r2 = ts
        .store
        .append(
            &ts.session,
            d_revision(r1.id.clone(), "dangling secret two"),
            None,
        )
        .unwrap();

    // Redacting the local sub-root scrubs the whole reachable sub-chain.
    let acts = ts.store.redact(&r1.id).unwrap();
    assert_eq!(acts.len(), 2, "target + reachable revision descendant");
    for id in [&r1.id, &r2.id] {
        let e = ts.store.raw_event(id).unwrap().unwrap();
        assert!(e.scrubbed() && e.text.is_none(), "{id} scrubbed");
    }

    // Redacting a tail member walks up to the local sub-root first.
    let ghost2 = ts.store.mint();
    let r3 = g.revision(ghost2.id.clone(), "dangling secret three");
    ts.store.merge(std::slice::from_ref(&r3)).unwrap();
    let r4 = ts
        .store
        .append(
            &ts.session,
            d_revision(r3.id.clone(), "dangling secret four"),
            None,
        )
        .unwrap();
    let acts = ts.store.redact(&r4.id).unwrap();
    assert_eq!(acts.len(), 2, "reachable ancestor + target");
    for id in [&r3.id, &r4.id] {
        assert!(ts.store.raw_event(id).unwrap().unwrap().scrubbed());
    }
}

// -- append consults the redactions registry (§7 supremacy, P5) ------------------

#[test]
fn append_consults_redactions_registry() {
    // A dangling redaction learned via merge condemns an id whose row does
    // not exist yet; appending content for that id must be refused —
    // "every insert path checks the registry first" (§7).
    let ts = new_store();
    let minted = ts.store.mint();
    let g = Gen::new(ts.session.clone());
    let red = g.redaction(minted.id.clone());
    let report = ts.store.merge(&[red]).unwrap();
    assert_eq!(report.inserted, 1, "dangling redaction unions in (inert)");

    let err = ts
        .store
        .append(
            &ts.session,
            d_remark("posthumous plaintext", vec![hash(1)]),
            Some(minted.clone()),
        )
        .unwrap_err();
    assert!(
        matches!(err, AppendError::CondemnedId(ref id) if *id == minted.id),
        "expected CondemnedId, got {err:?}"
    );
    assert!(
        ts.store.raw_event(&minted.id).unwrap().is_none(),
        "no plaintext row may land for a condemned id"
    );
}

// -- idle maintenance hook (§5.1 operational rules, P6) --------------------------

#[test]
fn maintain_runs_idle_checkpoint_and_optimize() {
    let ts = new_store();
    ts.store
        .append(&ts.session, d_remark("note", vec![hash(1)]), None)
        .unwrap();
    let wal = ts.path().with_file_name("journal.db-wal");
    assert!(
        std::fs::metadata(&wal).unwrap().len() > 0,
        "sanity: WAL non-empty after writes"
    );

    ts.store.maintain().unwrap();
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len(),
        0,
        "idle checkpoint truncates the WAL (§5.1)"
    );

    // The store keeps working normally after maintenance.
    ts.store
        .append(&ts.session, d_remark("after idle", vec![hash(1)]), None)
        .unwrap();
    assert_eq!(ts.store.folded_journal(&hash(1)).unwrap().len(), 2);
}

// -- retraction-of-redaction invalid at append AND merge (§3.5–3.6, P7) ----------

#[test]
fn retraction_of_redaction_rejected_at_append_and_quarantined_at_merge() {
    let ts = new_store();
    let a = hash(1);
    let r = ts
        .store
        .append(
            &ts.session,
            d_remark("to be scrubbed", vec![a.clone()]),
            None,
        )
        .unwrap();
    let acts = ts.store.redact(&r.id).unwrap();
    let red_id = acts[0].clone();

    // Append: retracting a redaction is invalid.
    let err = ts
        .store
        .append(&ts.session, d_retract(red_id.clone()), None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            AppendError::InvalidTargetKind {
                meta: Kind::Retraction,
                target_kind: Kind::Redaction
            }
        ),
        "expected InvalidTargetKind, got {err:?}"
    );

    // Merge, the redaction already local: quarantined, never inserted.
    let g = Gen::new(ts.session.clone());
    let bad = g.retraction(red_id.clone());
    let report = ts.store.merge(&[bad]).unwrap();
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.inserted, 0);

    // Merge, the redaction arriving in the same batch: still quarantined.
    let r2 = ts
        .store
        .append(&ts.session, d_remark("second victim", vec![a]), None)
        .unwrap();
    let red2 = g.redaction(r2.id.clone());
    let bad2 = g.retraction(red2.id.clone());
    let report = ts.store.merge(&[red2, bad2]).unwrap();
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.inserted, 1);
    assert_eq!(report.newly_scrubbed, 1);
}

// -- redaction chain closure (§3.6, §7) ---------------------------------------------

#[test]
fn redaction_chain_closure_scrubs_whole_chain() {
    let ts = new_store();
    let a = hash(1);
    let v = ts
        .store
        .append(
            &ts.session,
            d_voice("sensitive call notes", vec![a.clone()], None),
            None,
        )
        .unwrap();
    let rev1 = ts
        .store
        .append(
            &ts.session,
            d_revision(v.id.clone(), "sensitive call notes v2"),
            None,
        )
        .unwrap();
    let rev2 = ts
        .store
        .append(
            &ts.session,
            d_revision(rev1.id.clone(), "sensitive call notes v3"),
            None,
        )
        .unwrap();

    // Redacting a *member* scrubs the entire chain, one redaction event per
    // member, ascending.
    let acts = ts.store.redact(&rev1.id).unwrap();
    assert_eq!(acts.len(), 3);
    assert!(acts.windows(2).all(|w| w[0] < w[1]));
    for id in [&v.id, &rev1.id, &rev2.id] {
        let e = ts.store.raw_event(id).unwrap().unwrap();
        assert!(e.scrubbed(), "{id} scrubbed by chain closure");
    }
    // Registry has one row per member.
    let conn = ts.raw_conn();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    // The fold shows a stub; FTS is empty.
    let folded = ts.store.folded_journal(&a).unwrap();
    assert!(matches!(
        folded[0],
        JournalEntry::RedactedStub {
            kind: Kind::Remark,
            ..
        }
    ));
    assert!(fts_roots(&conn, "sensitive").is_empty());
}

// -- v5 migration (B37/B38): pre-P4.1 databases gain has_text ----------------

#[test]
fn v5_migration_restores_has_text_on_pre_p41_databases() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("journal.db");
    let target = hash(77);
    {
        let store = photoproof_core::EventStore::open(&db).unwrap();
        let sid = store
            .open_session(SessionContext {
                app_version: "0.1.0".into(),
                device_id: DEVICE_ID.into(),
                root_context: None,
            })
            .unwrap();
        store
            .append(&sid, d_remark("words evidence", vec![target.clone()]), None)
            .unwrap();
    }
    // Simulate a database created before P4.1: no has_text column, v4.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE image_journal_stats DROP COLUMN has_text;
             PRAGMA user_version = 4;",
        )
        .unwrap();
    }
    // Reopen: migrate adds the column, rebuild_derived recomputes the fold.
    let store = photoproof_core::EventStore::open(&db).unwrap();
    let stats = store.journal_stats(std::slice::from_ref(&target)).unwrap();
    assert_eq!(stats.len(), 1);
    assert!(
        stats[0].has_text,
        "migrated stats must carry real fold values"
    );
    assert!(stats[0].has_journal());
}

// -- attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md) ---------------

/// record_dwell applies the tier rate and the per-episode cap (the rates/cap
/// are config, applied in the backend). Look = full weight, Grid = the small
/// fraction; both clamp at dwell_cap_ms per call.
#[test]
fn record_dwell_applies_tier_and_cap() {
    let ts = new_store();
    let look = hash(1);
    let grid = hash(2);
    let now = UtcMillis::now();

    // Look tier is full-weight (default rate 1.0): 5 s of elapsed -> 5000 ms.
    ts.store
        .record_dwell(&look, DwellSource::Look, 5_000, now)
        .unwrap();
    // Grid tier is ~0.15x (default): 5 s elapsed -> 750 ms.
    ts.store
        .record_dwell(&grid, DwellSource::Grid, 5_000, now)
        .unwrap();

    // A single huge episode is capped at 60 s per call (the walk-away guard),
    // even at the full Look rate.
    ts.store
        .record_dwell(&look, DwellSource::Look, 10_000_000, now)
        .unwrap();

    let conn = ts.raw_conn();
    let read = |h: &str| -> (i64, i64) {
        conn.query_row(
            "SELECT dwell_ms, focus_count FROM image_dwell WHERE image_hash = ?1",
            [h],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    let (look_ms, look_n) = read(look.as_str());
    // 5000 (first) + 60000 (capped second) = 65000; two episodes.
    assert_eq!(look_ms, 65_000, "Look full-rate + capped accrual");
    assert_eq!(look_n, 2, "two focus episodes counted");

    let (grid_ms, grid_n) = read(grid.as_str());
    assert_eq!(grid_ms, 750, "Grid tier is the small fraction of elapsed");
    assert_eq!(grid_n, 1);
}

/// image_intensity composes the weighted sum across dwell + event_count +
/// stroke_count and NORMALIZES the scope to 0..1 (the hottest image is 1.0).
#[test]
fn image_intensity_composite_and_normalization() {
    let ts = new_store();
    let hot = hash(1);
    let cool = hash(2);
    let cold = hash(3); // no signal at all
    let now = UtcMillis::now();

    // `hot` gets dwell + a remark + a stroke; `cool` gets only one remark.
    ts.store
        .record_dwell(&hot, DwellSource::Look, 60_000, now)
        .unwrap();
    ts.store
        .append(&ts.session, d_remark("note", vec![hot.clone()]), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_stroke(hot.clone(), None), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_remark("note", vec![cool.clone()]), None)
        .unwrap();

    // All-time (flat) so the assertion is about the composite, not recency.
    let scope = vec![hot.clone(), cool.clone(), cold.clone()];
    let scores = ts.store.image_intensity(&scope, true, now).unwrap();
    assert_eq!(scores.len(), 3);
    // Input order is preserved.
    assert_eq!(scores[0].image, hot);
    assert_eq!(scores[1].image, cool);
    assert_eq!(scores[2].image, cold);
    // Normalized: the hottest is exactly 1.0, the no-signal image is 0.0,
    // and the middle sits strictly between.
    assert_eq!(scores[0].intensity, 1.0, "hottest normalizes to 1.0");
    assert_eq!(scores[2].intensity, 0.0, "no-signal image is 0.0");
    assert!(
        scores[1].intensity > 0.0 && scores[1].intensity < 1.0,
        "a lesser-engaged image sits between 0 and 1: {}",
        scores[1].intensity
    );
}

/// Recency weighting (default) makes a stale-but-busy image rank BELOW a
/// fresh one with the same raw signal; all-time (flat) ties them.
#[test]
fn image_intensity_recency_vs_all_time() {
    let ts = new_store();
    let old = hash(1);
    let new = hash(2);

    // Identical raw signal (one capped Look episode each), but at very
    // different ages: `old` is 30 days stale, `new` is right now.
    let now = UtcMillis::now();
    let long_ago = UtcMillis::from_epoch_ms(now.epoch_ms() - 30 * 86_400_000);
    ts.store
        .record_dwell(&old, DwellSource::Look, 60_000, long_ago)
        .unwrap();
    ts.store
        .record_dwell(&new, DwellSource::Look, 60_000, now)
        .unwrap();

    let scope = vec![old.clone(), new.clone()];

    // All-time: equal raw signal -> equal normalized intensity (both 1.0).
    let flat = ts.store.image_intensity(&scope, true, now).unwrap();
    assert_eq!(flat[0].intensity, flat[1].intensity, "all-time ties them");
    assert_eq!(flat[1].intensity, 1.0);

    // Recency-weighted: the fresh one is hotter; the stale one decays below it.
    let recent = ts.store.image_intensity(&scope, false, now).unwrap();
    assert_eq!(recent[1].intensity, 1.0, "fresh image normalizes to 1.0");
    assert!(
        recent[0].intensity < recent[1].intensity,
        "30-day-stale image decays below the fresh one: {} vs {}",
        recent[0].intensity,
        recent[1].intensity
    );
}

/// stroke_count is maintained in the same recompute transaction as the event
/// insert/retract: it rises with each live stroke and falls when one is
/// retracted (non-live), exactly like event_count.
#[test]
fn stroke_count_maintained_across_insert_and_retract() {
    let ts = new_store();
    let img = hash(1);

    let s1 = ts
        .store
        .append(&ts.session, d_stroke(img.clone(), None), None)
        .unwrap();
    ts.store
        .append(&ts.session, d_stroke(img.clone(), None), None)
        .unwrap();

    let stats = ts.store.journal_stats(std::slice::from_ref(&img)).unwrap();
    assert_eq!(stats[0].stroke_count, 2, "two live strokes counted");
    assert!(stats[0].has_strokes);

    // Retract one: it becomes non-live, dropping the count to 1.
    ts.store
        .append(&ts.session, d_retract(s1.id), None)
        .unwrap();
    let stats = ts.store.journal_stats(std::slice::from_ref(&img)).unwrap();
    assert_eq!(stats[0].stroke_count, 1, "retracted stroke drops the count");
    assert!(stats[0].has_strokes, "one live stroke remains");
}

/// clear_dwell wipes the local dwell telemetry but leaves the annotation
/// counts (the user's journal) untouched.
#[test]
fn clear_dwell_wipes_telemetry_only() {
    let ts = new_store();
    let img = hash(1);
    let now = UtcMillis::now();

    ts.store
        .record_dwell(&img, DwellSource::Look, 60_000, now)
        .unwrap();
    ts.store
        .append(&ts.session, d_remark("note", vec![img.clone()]), None)
        .unwrap();

    let removed = ts.store.clear_dwell().unwrap();
    assert_eq!(removed, 1, "one dwell row removed");

    // Dwell intensity is gone; but the remark still counts (all-time so the
    // event_count alone drives a non-zero, top-of-scope intensity).
    let scores = ts
        .store
        .image_intensity(std::slice::from_ref(&img), true, now)
        .unwrap();
    assert_eq!(
        scores[0].intensity, 1.0,
        "annotation count survives the wipe"
    );
    // And the journal stats are intact.
    let stats = ts.store.journal_stats(std::slice::from_ref(&img)).unwrap();
    assert_eq!(stats[0].event_count, 1);
}
