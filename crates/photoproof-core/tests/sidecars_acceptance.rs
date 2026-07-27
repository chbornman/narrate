//! spec/SIDECARS.md §13 acceptance criteria, one named test per criterion
//! (`s13_NN_*`), plus the §8.3/§9.x behaviors the build-loop gate names.
//!
//! Volume identity and writability detection are owned by spec/LIBRARY.md
//! (packet P2.2); these tests drive the engine through the `ImageLocator`
//! seam (`MemoryLocator`) that P2.2 will implement for real.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use photoproof_core::sidecar::{
    AdjacentLocation, Fault, FaultPoint, FlushOutcome, ImageSnapshot, MemoryLocator, ParsedSidecar,
    RebuildOptions, SIMULATED_KILL, SidecarDoc, SidecarEngine, WriteOutcome, clean_orphan_temps,
    overflow_path, parse_sidecar, rebuild_from_sidecars, session_journal_path,
    sidecar_path_for_image, to_pretty_canonical, write_atomic, write_atomic_faulty,
};
use photoproof_core::{ContentHash, EventStore, SessionContext, UtcMillis};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn t(s: &str) -> UtcMillis {
    UtcMillis::parse(s).expect("test timestamp")
}

fn ms(base: UtcMillis, delta: i64) -> UtcMillis {
    UtcMillis::from_epoch_ms(base.epoch_ms() + delta)
}

struct World {
    ts: common::TestStore,
    app_data: tempfile::TempDir,
    images: tempfile::TempDir,
    locator: MemoryLocator,
}

impl World {
    fn new() -> Self {
        Self {
            ts: common::new_store(),
            app_data: tempfile::tempdir().expect("app_data tempdir"),
            images: tempfile::tempdir().expect("images tempdir"),
            locator: MemoryLocator::new(),
        }
    }

    fn engine(&self) -> SidecarEngine<'_, &MemoryLocator> {
        SidecarEngine::new(
            &self.ts.store,
            self.ts.path(),
            self.app_data.path(),
            &self.locator,
        )
        .expect("engine")
    }

    /// Create an image file in the world's root, registered writable.
    fn image(&self, name: &str, content: &[u8]) -> (PathBuf, ContentHash) {
        let p = self.images.path().join(name);
        fs::write(&p, content).expect("write image");
        let h = ContentHash::from_bytes_of(content);
        self.locator.set(
            h.clone(),
            AdjacentLocation::Writable {
                image_path: p.clone(),
            },
        );
        (p, h)
    }

    fn root(&self) -> PathBuf {
        self.images.path().to_path_buf()
    }
}

/// Minimal valid sidecar bytes for the writer-level tests.
fn doc_bytes(filename: &str) -> Vec<u8> {
    let snap = ImageSnapshot {
        hash: ContentHash::from_bytes_of(filename.as_bytes()),
        filename: filename.to_owned(),
        byte_size: 7,
    };
    SidecarDoc::new(Some(snap)).to_bytes()
}

fn read_to_string(p: &Path) -> String {
    String::from_utf8(fs::read(p).expect("read file")).expect("utf8")
}

fn list_temps(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains(".tmp-"))
                .unwrap_or(false)
        })
        .collect()
}

/// Recursively collect (relative path, bytes) under `root`.
fn dir_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).expect("under root").to_path_buf();
                out.insert(rel, fs::read(&p).expect("read"));
            }
        }
    }
    out
}

fn fts_match_count(db: &Path, query: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.busy_timeout(Duration::from_millis(5000)).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM event_fts WHERE event_fts MATCH ?1",
        [query],
        |r| r.get(0),
    )
    .expect("fts query")
}

fn sql_count(db: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.busy_timeout(Duration::from_millis(5000)).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).expect("count query")
}

const NOW: &str = "2026-06-09T12:00:00.000Z";

// ---------------------------------------------------------------------------
// §13.1 round trip
// ---------------------------------------------------------------------------

#[test]
fn s13_01_round_trip() {
    let app_data = tempfile::tempdir().unwrap();
    let images = tempfile::tempdir().unwrap();
    let dbdir = tempfile::tempdir().unwrap();
    let db = dbdir.path().join("journal.db");
    let locator = MemoryLocator::new();
    let now = t(NOW);

    let make_image = |name: &str, content: &[u8]| -> (PathBuf, ContentHash) {
        let p = images.path().join(name);
        fs::write(&p, content).unwrap();
        let h = ContentHash::from_bytes_of(content);
        locator.set(
            h.clone(),
            AdjacentLocation::Writable {
                image_path: p.clone(),
            },
        );
        (p, h)
    };
    let (_pa, ha) = make_image("IMG_A.arw", b"image-a-bytes");
    let (_pb, hb) = make_image("IMG_B.arw", b"image-b-bytes");
    let (_pc, hc) = make_image("IMG_C.arw", b"image-c-bytes");

    let export1 = dbdir.path().join("export1");
    let export2 = dbdir.path().join("export2");

    // Phase 1: annotate (multi-target remark, rating + retraction, voice
    // remark + revision, stroke, redaction, session-level remark), flush,
    // export, snapshot the folded state.
    let (session, folded_a, folded_b, folded_c, folded_sess, bytes_a, bytes_b, bytes_c) = {
        let store = EventStore::open(&db).unwrap();
        let session = store
            .open_session(SessionContext {
                app_version: "0.1.0".into(),
                device_id: common::DEVICE_ID.into(),
                root_context: None,
            })
            .unwrap();
        let engine = SidecarEngine::new(&store, &db, app_data.path(), &locator).unwrap();

        store
            .append(
                &session,
                common::d_remark(
                    "bookends of the harbor sequence",
                    vec![ha.clone(), hb.clone()],
                ),
                None,
            )
            .unwrap();
        let rating = store
            .append(&session, common::d_rating(3, vec![ha.clone()]), None)
            .unwrap();
        store
            .append(&session, common::d_retract(rating.id.clone()), None)
            .unwrap();
        let voice = store
            .append(
                &session,
                common::d_voice("the muddy light holds it", vec![ha.clone()], None),
                None,
            )
            .unwrap();
        store
            .append(
                &session,
                common::d_stroke(ha.clone(), Some(voice.id.clone())),
                None,
            )
            .unwrap();
        store
            .append(
                &session,
                common::d_revision(voice.id.clone(), "the moody light holds it"),
                None,
            )
            .unwrap();
        let secret = store
            .append(
                &session,
                common::d_remark("a private aside about a third party", vec![hc.clone()]),
                None,
            )
            .unwrap();
        store
            .append(
                &session,
                common::d_remark("good session overall", vec![]),
                None,
            )
            .unwrap();

        engine.redact(&secret.id, now).unwrap();
        engine.flush_all(now).unwrap();
        engine.flush_session(&session).unwrap();
        engine.export(&export1, "0.1.0", now, &[]).unwrap();

        let canon = |h: &ContentHash| -> Vec<Vec<u8>> {
            store
                .events_for_image(h)
                .unwrap()
                .iter()
                .map(EventStore::canonical_json)
                .collect()
        };
        (
            session.clone(),
            store.folded_journal(&ha).unwrap(),
            store.folded_journal(&hb).unwrap(),
            store.folded_journal(&hc).unwrap(),
            store.folded_session(&session).unwrap(),
            canon(&ha),
            canon(&hb),
            canon(&hc),
        )
    };

    // Delete SQLite + caches: the index is gone; sidecars are the truth.
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(dbdir.path().join(format!("journal.db{suffix}")));
    }

    // Phase 2: rebuild from the live world (adjacent sidecars + app-data
    // overflow + session journals).
    let store2 = EventStore::open(&db).unwrap();
    let engine2 = SidecarEngine::new(&store2, &db, app_data.path(), &locator).unwrap();
    let report = rebuild_from_sidecars(
        &engine2,
        &RebuildOptions::live(vec![images.path().to_path_buf()]),
        now,
    )
    .unwrap();
    assert!(report.files_parsed >= 4, "3 adjacent + 1 session journal");
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    // Byte-identical folded journal: event rows, targets, fold results.
    let canon2 = |h: &ContentHash| -> Vec<Vec<u8>> {
        store2
            .events_for_image(h)
            .unwrap()
            .iter()
            .map(EventStore::canonical_json)
            .collect()
    };
    assert_eq!(bytes_a, canon2(&ha));
    assert_eq!(bytes_b, canon2(&hb));
    assert_eq!(bytes_c, canon2(&hc));
    assert_eq!(folded_a, store2.folded_journal(&ha).unwrap());
    assert_eq!(folded_b, store2.folded_journal(&hb).unwrap());
    assert_eq!(folded_c, store2.folded_journal(&hc).unwrap());
    assert_eq!(folded_sess, store2.folded_session(&session).unwrap());

    // The dirty queue is empty by construction after rebuild (DB == sidecars).
    assert!(store2.dirty_images().unwrap().is_empty());

    // A fresh export is byte-identical, manifest `exported_at` excepted.
    engine2
        .export(&export2, "0.1.0", t("2026-06-10T08:00:00.000Z"), &[])
        .unwrap();
    let snap1 = dir_snapshot(&export1);
    let snap2 = dir_snapshot(&export2);
    assert_eq!(
        snap1.keys().collect::<Vec<_>>(),
        snap2.keys().collect::<Vec<_>>()
    );
    for (rel, bytes1) in &snap1 {
        let bytes2 = &snap2[rel];
        if rel == Path::new("manifest.photoproof.json") {
            let mut m1: Value = serde_json::from_slice(bytes1).unwrap();
            let mut m2: Value = serde_json::from_slice(bytes2).unwrap();
            m1.as_object_mut().unwrap().remove("exported_at");
            m2.as_object_mut().unwrap().remove("exported_at");
            assert_eq!(m1, m2, "manifest differs beyond exported_at");
        } else {
            assert_eq!(bytes1, bytes2, "export file {} differs", rel.display());
        }
    }
}

// ---------------------------------------------------------------------------
// §13.2 latency (debounce timing via injected clocks)
// ---------------------------------------------------------------------------

#[test]
fn s13_02_latency_under_sustained_bursts() {
    let w = World::new();
    let engine = w.engine();
    let (p, h) = w.image("IMG_0001.jpg", b"burst-image");
    let sidecar = sidecar_path_for_image(&p);
    let base = t(NOW);

    // Sustained burst: one typed note every 500 ms for 4.5 s; pump every
    // 250 ms. The 2 s quiet window never opens, so the 5 s hard cap is the
    // contract under test.
    let mut appended: Vec<(photoproof_core::EventId, i64)> = Vec::new();
    let mut first_seen: BTreeMap<photoproof_core::EventId, i64> = BTreeMap::new();
    for step in 0..=40 {
        let now = ms(base, step * 250);
        if step % 2 == 0 && step <= 18 {
            let e =
                w.ts.store
                    .append(
                        &w.ts.session,
                        common::d_remark(&format!("note {step}"), vec![h.clone()]),
                        None,
                    )
                    .unwrap();
            engine.note_event(&h, now);
            appended.push((e.id.clone(), now.epoch_ms()));
        }
        let flushed = engine.pump(now).unwrap();
        if step * 250 < 5000 {
            assert!(
                flushed.is_empty(),
                "debounce must hold until the 5 s cap (step {step})"
            );
            assert!(!sidecar.exists());
        }
        if sidecar.exists() {
            let bytes = fs::read(&sidecar).unwrap();
            let ParsedSidecar::Doc(doc) = parse_sidecar(&bytes).unwrap() else {
                panic!("sidecar must stay valid");
            };
            for e in doc.canonical_events() {
                first_seen.entry(e.id).or_insert(now.epoch_ms());
            }
        }
    }
    for (id, appended_at) in &appended {
        let seen = first_seen
            .get(id)
            .unwrap_or_else(|| panic!("event {id} never reached the sidecar"));
        assert!(
            seen - appended_at <= 5_000,
            "event {id} took {} ms to land",
            seen - appended_at
        );
    }

    // Quiet-window path: a lone note flushes after exactly 2 s of quiet.
    let lone_at = ms(base, 11_000);
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("lone note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.note_event(&h, lone_at);
    assert!(engine.pump(ms(base, 12_999)).unwrap().is_empty());
    let flushed = engine.pump(ms(base, 13_000)).unwrap();
    assert_eq!(flushed.len(), 1, "2 s quiet window flushes");
}

// ---------------------------------------------------------------------------
// §13.3 crash safety (kill -9 fault injection, 1000×)
// ---------------------------------------------------------------------------

#[test]
fn s13_03_kill_during_write() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("IMG_0001.jpg.photoproof.json");
    let old = doc_bytes("old.jpg");
    let new = doc_bytes("new.jpg");
    write_atomic(&target, &old).unwrap();

    let points = [
        FaultPoint::DuringTempWrite,
        FaultPoint::BeforeTempFsync,
        FaultPoint::BeforeRename,
        FaultPoint::AfterRename,
    ];
    for i in 0..1000usize {
        let fault = Fault {
            point: points[i % 4],
            partial: i % new.len(),
        };
        let err = write_atomic_faulty(&target, &new, Some(fault))
            .expect_err("simulated kill must surface");
        assert_eq!(err.kind(), SIMULATED_KILL);

        // Invariant: the target always parses and equals either the old or
        // the new complete document.
        let cur = fs::read(&target).unwrap();
        assert!(cur == old || cur == new, "iteration {i}: torn target");
        assert!(matches!(parse_sidecar(&cur), Ok(ParsedSidecar::Doc(_))));
        if cur == new {
            fs::remove_file(&target).unwrap();
            write_atomic(&target, &old).unwrap();
        }
    }

    // Orphan temps left by the kills are cleaned on the next run.
    let _ = write_atomic_faulty(
        &target,
        &new,
        Some(Fault {
            point: FaultPoint::BeforeRename,
            partial: 0,
        }),
    );
    assert!(!list_temps(dir.path()).is_empty(), "kill leaves a temp");
    write_atomic(&target, &new).unwrap();
    assert!(list_temps(dir.path()).is_empty(), "next write cleans temps");
    assert_eq!(fs::read(&target).unwrap(), new);
}

// ---------------------------------------------------------------------------
// §13.4 redaction propagation (offline queue, scrub-before-UI-confirm)
// ---------------------------------------------------------------------------

#[test]
fn s13_04_redaction_propagation_offline_volume() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_X.arw", b"offline-volume-image");
    let sidecar = sidecar_path_for_image(&p);

    let secret =
        w.ts.store
            .append(
                &w.ts.session,
                common::d_voice(
                    "the seekrit phrase about a third party",
                    vec![h.clone()],
                    None,
                ),
                None,
            )
            .unwrap();
    engine.flush_all(now).unwrap();
    assert!(read_to_string(&sidecar).contains("seekrit"));
    let backup = fs::read(&sidecar).unwrap();

    // Volume goes offline; the user redacts.
    w.locator
        .set(h.clone(), AdjacentLocation::Offline { volume_id: None });
    let receipt = engine.redact(&secret.id, now).unwrap();

    // §11 step 2: a durable scrubbed overflow record exists *before* the UI
    // could confirm (i.e. at redact() return), and the queue holds the copy.
    let ov = overflow_path(w.app_data.path(), &h);
    assert!(receipt.overflow_records.contains(&ov));
    let ov_text = read_to_string(&ov);
    assert!(!ov_text.contains("seekrit"), "overflow record is scrubbed");
    assert!(ov_text.contains("redacted_by"));
    assert!(!receipt.queued.is_empty());
    assert!(!engine.redaction_queue().unwrap().is_empty());
    // The unreachable adjacent copy still holds the content (it is exactly
    // what the mount-time drain must scrub).
    assert!(read_to_string(&sidecar).contains("seekrit"));

    // Mount the volume: the adjacent sidecar is scrubbed, the queue drains.
    w.locator.set(
        h.clone(),
        AdjacentLocation::Writable {
            image_path: p.clone(),
        },
    );
    engine.on_volume_mount(now).unwrap();
    let text = read_to_string(&sidecar);
    assert!(!text.contains("seekrit"), "adjacent scrubbed at mount");
    assert!(text.contains("redacted_by"));
    assert!(
        engine.redaction_queue().unwrap().is_empty(),
        "queue drained"
    );
    assert!(!ov.exists(), "overflow entry migrated away after mount");

    // Restore a pre-redaction copy from "backup": merge imports none of the
    // content, FTS finds nothing, the file is rewritten scrubbed.
    fs::write(&sidecar, &backup).unwrap();
    engine.scan(&[w.root()], now).unwrap();
    let text = read_to_string(&sidecar);
    assert!(!text.contains("seekrit"), "backup re-scrubbed on merge");
    assert_eq!(fts_match_count(&w.ts.path(), "seekrit"), 0);
    let raw = w.ts.store.raw_event(&secret.id).unwrap().unwrap();
    assert!(raw.scrubbed());
    assert!(raw.text.is_none() && raw.payload.is_none());
}

// ---------------------------------------------------------------------------
// §13.5 unknown-field preservation + v2 inviolability
// ---------------------------------------------------------------------------

#[test]
fn s13_05_unknown_field_preservation() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_U.jpg", b"unknown-field-image");
    let sidecar = sidecar_path_for_image(&p);

    // A foreign sidecar carrying unknowns at every level, one event with an
    // unknown extra field, and one whole unknown kind.
    let evgen = common::Gen::new(w.ts.session.clone());
    let clean = evgen.remark("a clean foreign note", vec![h.clone()]);
    let extra = evgen.remark("a note with an extra field", vec![h.clone()]);
    let started =
        w.ts.store
            .session(&w.ts.session)
            .unwrap()
            .unwrap()
            .started_ts;

    let clean_v: Value = serde_json::from_slice(&EventStore::canonical_json(&clean)).unwrap();
    let mut extra_v: Value = serde_json::from_slice(&EventStore::canonical_json(&extra)).unwrap();
    extra_v
        .as_object_mut()
        .unwrap()
        .insert("x_evt".into(), json!(true));
    let unknown_kind = json!({
        "id": "01JXJ9A1B2C3D4E5F6G7H8J9K0",
        "kind": "hologram",
        "session_id": w.ts.session.as_str(),
        "x_payload": {"depth": 3},
    });

    let foreign = json!({
        "format": "photoproof-sidecar",
        "schema_version": 1,
        "image": {
            "hash": h.as_str(),
            "filename": "IMG_U.jpg",
            "byte_size": 19,
            "x_img": "snapshot-extra",
        },
        "sessions": {
            w.ts.session.as_str(): {
                "started_at": started.to_rfc3339(),
                "app_version": "0.1.0",
                "x_sess": 2,
            },
        },
        "events": [clean_v, extra_v, unknown_kind],
        "x_future": {"a": 1},
    });
    fs::write(&sidecar, to_pretty_canonical(&foreign)).unwrap();

    let report = engine.scan(&[w.root()], now).unwrap();
    assert_eq!(
        report.unknown_kinds_preserved, 2,
        "extra-field + unknown kind"
    );

    // Force a rewrite from the index and check every unknown survives in
    // canonical sorted position.
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("a local note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let text = read_to_string(&sidecar);
    for needle in [
        "x_future",
        "x_img",
        "x_sess",
        "x_evt",
        "hologram",
        "x_payload",
    ] {
        assert!(text.contains(needle), "{needle} lost on rewrite");
    }
    assert!(text.contains("a clean foreign note"), "imported event kept");
    assert!(text.contains("a local note"));
    // Lexicographic key order at top level: events < format < image <
    // schema_version < sessions < x_future.
    let pos = |k: &str| {
        text.find(&format!("\"{k}\""))
            .unwrap_or_else(|| panic!("{k} missing"))
    };
    assert!(pos("events") < pos("format"));
    assert!(pos("format") < pos("image"));
    assert!(pos("image") < pos("schema_version"));
    assert!(pos("schema_version") < pos("sessions"));
    assert!(pos("sessions") < pos("x_future"));
    // The rewrite is canonical: re-serializing the parsed doc is identity.
    let bytes = fs::read(&sidecar).unwrap();
    let ParsedSidecar::Doc(doc) = parse_sidecar(&bytes).unwrap() else {
        panic!("must parse");
    };
    assert_eq!(doc.to_bytes(), bytes, "file is in canonical form");

    // A v2-marked file is never modified and never imported, and reported.
    let (p2, h2) = w.image("IMG_V2.jpg", b"newer-version-image");
    let sidecar2 = sidecar_path_for_image(&p2);
    let v2 = json!({
        "format": "photoproof-sidecar",
        "schema_version": 2,
        "image": {"hash": h2.as_str(), "filename": "IMG_V2.jpg", "byte_size": 1},
        "events": [{"id": "01JXJ9B4C5D6E7F8G9H0J1K2M3", "kind": "remark", "text": "from the future"}],
        "sessions": {},
    });
    let v2_bytes = to_pretty_canonical(&v2);
    fs::write(&sidecar2, &v2_bytes).unwrap();
    let report = engine.scan(&[w.root()], now).unwrap();
    assert!(report.newer_version_files.contains(&sidecar2));
    assert_eq!(fs::read(&sidecar2).unwrap(), v2_bytes, "v2 file inviolable");
    assert!(w.ts.store.events_for_image(&h2).unwrap().is_empty());

    // §5.3 step 3: new events for that image route to overflow.
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note for the v2-held image", vec![h2.clone()]),
            None,
        )
        .unwrap();
    let outcome = engine.flush_image(&h2, now).unwrap();
    assert!(matches!(outcome, FlushOutcome::Overflow { .. }));
    assert_eq!(
        fs::read(&sidecar2).unwrap(),
        v2_bytes,
        "v2 file still untouched"
    );
}

// ---------------------------------------------------------------------------
// §13.6 determinism (independent of write order / count / machine)
// ---------------------------------------------------------------------------

#[test]
fn s13_06_byte_determinism() {
    let w1 = World::new();
    let engine1 = w1.engine();
    let now = t(NOW);
    let (_p1, h) = w1.image("IMG_D.jpg", b"determinism-image");

    let r1 = w1
        .ts
        .store
        .append(
            &w1.ts.session,
            common::d_remark("first", vec![h.clone()]),
            None,
        )
        .unwrap();
    w1.ts
        .store
        .append(&w1.ts.session, common::d_rating(4, vec![h.clone()]), None)
        .unwrap();
    w1.ts
        .store
        .append(
            &w1.ts.session,
            common::d_revision(r1.id.clone(), "first, corrected"),
            None,
        )
        .unwrap();
    w1.ts
        .store
        .append(&w1.ts.session, common::d_stroke(h.clone(), None), None)
        .unwrap();
    engine1.flush_all(now).unwrap();

    // Serialized repeatedly: identical bytes; rewriting is a no-op.
    let d1 = engine1.image_doc(&h).unwrap().to_bytes();
    assert_eq!(d1, engine1.image_doc(&h).unwrap().to_bytes());
    assert_eq!(d1, engine1.image_doc(&h).unwrap().to_bytes());
    let FlushOutcome::Adjacent { path, outcome } = engine1.flush_image(&h, now).unwrap() else {
        panic!("adjacent");
    };
    assert_eq!(outcome, WriteOutcome::SkippedIdentical);
    assert_eq!(fs::read(&path).unwrap(), d1);

    // A second machine holding the same journal (merged in shuffled order,
    // split across two batches) writes byte-identical files.
    let w2 = World::new();
    let engine2 = w2.engine();
    let p2 = w2.images.path().join("IMG_D.jpg");
    fs::write(&p2, b"determinism-image").unwrap();
    w2.locator.set(
        h.clone(),
        AdjacentLocation::Writable {
            image_path: p2.clone(),
        },
    );
    let mut events = w1.ts.store.events_for_image(&h).unwrap();
    events.reverse();
    let (batch1, batch2) = events.split_at(2);
    w2.ts.store.merge(batch2).unwrap();
    w2.ts.store.merge(batch1).unwrap();
    // Session metadata travels via the sidecar in reality; merge it the same
    // way here so both machines hold identical session denormalization.
    let rec = w1.ts.store.session(&w1.ts.session).unwrap().unwrap();
    w2.ts.store.merge_sessions(&[rec]).unwrap();
    engine2.flush_all(now).unwrap();
    let sc2 = sidecar_path_for_image(&p2);
    assert_eq!(fs::read(&sc2).unwrap(), d1, "machines agree byte-for-byte");

    // After rebuild from the file alone: byte-identical again.
    let w3 = World::new();
    fs::copy(
        sidecar_path_for_image(&w1.images.path().join("IMG_D.jpg")),
        w3.images.path().join("IMG_D.jpg.photoproof.json"),
    )
    .unwrap();
    let engine3 = w3.engine();
    rebuild_from_sidecars(
        &engine3,
        &RebuildOptions {
            roots: vec![w3.root()],
            include_app_data: false,
            converge: false,
        },
        now,
    )
    .unwrap();
    assert_eq!(engine3.image_doc(&h).unwrap().to_bytes(), d1);
    // (Cross-platform hash-compare runs in CI on the OS matrix; the bytes
    // contain no platform-dependent content by construction.)
}

// ---------------------------------------------------------------------------
// §13.7 multi-target dedupe
// ---------------------------------------------------------------------------

#[test]
fn s13_07_multi_target_dedupe() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p1, h1) = w.image("IMG_1.jpg", b"multi-target-1");
    let (p2, h2) = w.image("IMG_2.jpg", b"multi-target-2");
    let (p3, h3) = w.image("IMG_3.jpg", b"multi-target-3");

    let remark =
        w.ts.store
            .append(
                &w.ts.session,
                common::d_remark(
                    "one remark across three frames",
                    vec![h1.clone(), h2.clone(), h3.clone()],
                ),
                None,
            )
            .unwrap();
    engine.flush_all(now).unwrap();

    // The event appears in all three sidecars with its complete target list,
    // byte-identical across copies.
    let mut copies = Vec::new();
    for p in [&p1, &p2, &p3] {
        let bytes = fs::read(sidecar_path_for_image(p)).unwrap();
        let ParsedSidecar::Doc(doc) = parse_sidecar(&bytes).unwrap() else {
            panic!("parse");
        };
        let evs = doc.canonical_events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].targets, vec![h1.clone(), h2.clone(), h3.clone()]);
        copies.push(EventStore::canonical_json(&evs[0]));
    }
    assert!(copies.windows(2).all(|w| w[0] == w[1]));

    // Rebuild from the three copies yields exactly one row + 3 ordered
    // target rows.
    let w2 = World::new();
    for (i, p) in [&p1, &p2, &p3].iter().enumerate() {
        fs::copy(
            sidecar_path_for_image(p),
            w2.images
                .path()
                .join(format!("IMG_{}.jpg.photoproof.json", i + 1)),
        )
        .unwrap();
    }
    let engine2 = w2.engine();
    rebuild_from_sidecars(
        &engine2,
        &RebuildOptions {
            roots: vec![w2.root()],
            include_app_data: false,
            converge: false,
        },
        now,
    )
    .unwrap();
    let db = w2.ts.path();
    assert_eq!(sql_count(&db, "SELECT COUNT(*) FROM annotation_events"), 1);
    assert_eq!(sql_count(&db, "SELECT COUNT(*) FROM event_targets"), 3);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let targets: Vec<(String, i64)> = conn
        .prepare(
            "SELECT image_hash, position FROM event_targets WHERE event_id = ?1 ORDER BY position",
        )
        .unwrap()
        .query_map([remark.id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        targets,
        vec![
            (h1.as_str().to_owned(), 0),
            (h2.as_str().to_owned(), 1),
            (h3.as_str().to_owned(), 2)
        ]
    );
}

// ---------------------------------------------------------------------------
// §13.8 collision safety
// ---------------------------------------------------------------------------

#[test]
fn s13_08_collision_safety() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);

    // A user's own JSON file wearing our suffix.
    let (p1, h1) = w.image("IMG_0001.jpg", b"collision-image-1");
    let user_json = sidecar_path_for_image(&p1);
    fs::write(&user_json, br#"{"my": "own notes", "stars": 5}"#).unwrap();
    // A user's plain-text file wearing our suffix.
    let (p2, h2) = w.image("IMG_0002.jpg", b"collision-image-2");
    let user_text = sidecar_path_for_image(&p2);
    fs::write(&user_text, b"just a text file, not json at all\n").unwrap();

    for h in [&h1, &h2] {
        w.ts.store
            .append(
                &w.ts.session,
                common::d_remark("note for collided image", vec![(*h).clone()]),
                None,
            )
            .unwrap();
    }
    let user_json_before = fs::read(&user_json).unwrap();
    let user_text_before = fs::read(&user_text).unwrap();

    // Flush: the journal lands in overflow; the user files are never
    // modified or renamed.
    for h in [&h1, &h2] {
        let outcome = engine.flush_image(h, now).unwrap();
        assert!(matches!(outcome, FlushOutcome::Overflow { .. }));
        assert!(overflow_path(w.app_data.path(), h).exists());
    }
    assert_eq!(fs::read(&user_json).unwrap(), user_json_before);
    assert_eq!(fs::read(&user_text).unwrap(), user_text_before);

    // The report says so, once per file, re-checked at each scan.
    let report = engine.scan(&[w.root()], now).unwrap();
    assert!(report.collisions.contains(&user_json));
    assert!(report.collisions.contains(&user_text));
    let report = engine.scan(&[w.root()], now).unwrap();
    assert!(
        report.collisions.contains(&user_json),
        "re-checked each scan"
    );
    assert_eq!(fs::read(&user_json).unwrap(), user_json_before);
    assert_eq!(fs::read(&user_text).unwrap(), user_text_before);
}

// ---------------------------------------------------------------------------
// §13.9 overflow migration
// ---------------------------------------------------------------------------

#[test]
fn s13_09_overflow_migration() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);

    // Two images on a read-only volume.
    let (p1, h1) = w.image("IMG_RO_1.arw", b"read-only-image-1");
    let (p2, h2) = w.image("IMG_RO_2.arw", b"read-only-image-2");
    for (p, h) in [(&p1, &h1), (&p2, &h2)] {
        w.locator.set(
            (*h).clone(),
            AdjacentLocation::Unwritable {
                image_path: Some((*p).clone()),
                volume_id: Some("vol-ro".into()),
            },
        );
    }
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("annotated while read-only", vec![h1.clone(), h2.clone()]),
            None,
        )
        .unwrap();
    w.ts.store
        .append(&w.ts.session, common::d_rating(5, vec![h1.clone()]), None)
        .unwrap();
    engine.flush_all(now).unwrap();

    // Overflow entries exist in byte-identical sidecar format, including the
    // real image filename (§8.1).
    for (h, name) in [(&h1, "IMG_RO_1.arw"), (&h2, "IMG_RO_2.arw")] {
        let ov = overflow_path(w.app_data.path(), h);
        let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&ov).unwrap()).unwrap() else {
            panic!("overflow entry parses");
        };
        assert_eq!(doc.image.as_ref().unwrap().filename, name);
    }
    assert!(!sidecar_path_for_image(&p1).exists());

    // Remount writable → adjacent sidecars appear with full history,
    // overflow entries are gone.
    for (p, h) in [(&p1, &h1), (&p2, &h2)] {
        w.locator.set(
            (*h).clone(),
            AdjacentLocation::Writable {
                image_path: (*p).clone(),
            },
        );
    }
    engine.on_volume_mount(now).unwrap();
    for (p, h) in [(&p1, &h1), (&p2, &h2)] {
        let sc = sidecar_path_for_image(p);
        let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&sc).unwrap()).unwrap() else {
            panic!("adjacent parses");
        };
        assert_eq!(
            doc.canonical_events().len(),
            w.ts.store.events_for_image(h).unwrap().len(),
            "full history adjacent"
        );
        assert!(!overflow_path(w.app_data.path(), h).exists());
    }

    // A second migration run is a no-op (and mtime-stable).
    let mtime = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
    let m1 = mtime(&sidecar_path_for_image(&p1));
    let mut report = photoproof_core::sidecar::IntegrityReport::default();
    let migrated = engine.migrate_overflow(now, &mut report).unwrap();
    assert_eq!(migrated, 0);
    assert_eq!(mtime(&sidecar_path_for_image(&p1)), m1);
}

// ---------------------------------------------------------------------------
// §13.10 re-matching — every row of the §10.3 table
// ---------------------------------------------------------------------------

#[test]
fn s13_10_rematch_orphan_sidecar_image_absent() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);

    // A sidecar arrives (backup drop-in) for an image we have never seen.
    let evgen = common::Gen::new(w.ts.session.clone());
    let lost = ContentHash::from_bytes_of(b"a lost image");
    let e = evgen.remark("note about the lost frame", vec![lost.clone()]);
    let ev: Value = serde_json::from_slice(&EventStore::canonical_json(&e)).unwrap();
    let started =
        w.ts.store
            .session(&w.ts.session)
            .unwrap()
            .unwrap()
            .started_ts;
    let foreign = json!({
        "format": "photoproof-sidecar",
        "schema_version": 1,
        "image": {"hash": lost.as_str(), "filename": "LOST_42.arw", "byte_size": 123},
        "sessions": {w.ts.session.as_str(): {
            "started_at": started.to_rfc3339(), "app_version": "0.1.0"}},
        "events": [ev],
    });
    // Deliberately non-canonical layout: proves the orphan is left untouched.
    let orphan = w.images.path().join("LOST_42.arw.photoproof.json");
    fs::write(&orphan, serde_json::to_vec(&foreign).unwrap()).unwrap();
    let before = fs::read(&orphan).unwrap();

    engine.scan(&[w.root()], now).unwrap();
    // Imported: searchable, journal intact; snapshot kept for the hunt.
    assert_eq!(w.ts.store.events_for_image(&lost).unwrap().len(), 1);
    let snap = engine.image_doc(&lost).unwrap();
    assert_eq!(snap.image.as_ref().unwrap().filename, "LOST_42.arw");
    assert_eq!(snap.image.as_ref().unwrap().byte_size, 123);
    // The orphan file is left in place untouched.
    assert_eq!(fs::read(&orphan).unwrap(), before);

    // When a file with that hash is later ingested anywhere, everything
    // reattaches automatically.
    let (pl, _) = w.image("FOUND_AGAIN.arw", b"a lost image");
    engine.scan(&[w.root()], now).unwrap();
    let sc = sidecar_path_for_image(&pl);
    let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&sc).unwrap()).unwrap() else {
        panic!("reattached sidecar parses");
    };
    assert_eq!(doc.image.as_ref().unwrap().hash, lost);
    assert_eq!(doc.canonical_events().len(), 1);
}

#[test]
fn s13_10_rematch_regenerates_deleted_sidecar() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_DEL.jpg", b"deleted-sidecar-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);
    let canonical = fs::read(&sc).unwrap();

    fs::remove_file(&sc).unwrap();
    engine.scan(&[w.root()], now).unwrap();
    assert_eq!(
        fs::read(&sc).unwrap(),
        canonical,
        "regenerated from the index"
    );
}

#[test]
fn s13_10_rematch_stale_subset_rewritten_to_superset() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_STALE.jpg", b"stale-sidecar-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("one", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);
    let stale = fs::read(&sc).unwrap();

    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("two", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let superset = fs::read(&sc).unwrap();
    assert_ne!(stale, superset);

    // Restore the stale subset; reconciliation unions (no-op) and rewrites.
    fs::write(&sc, &stale).unwrap();
    engine.scan(&[w.root()], now).unwrap();
    assert_eq!(fs::read(&sc).unwrap(), superset);
    assert_eq!(w.ts.store.events_for_image(&h).unwrap().len(), 2);
}

#[test]
fn s13_10_rematch_foreign_superset_imported_and_converged() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_FOREIGN.jpg", b"foreign-sidecar-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("local note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);

    // A second machine wrote events we lack; its sidecar lands here.
    let evgen = common::Gen::new(w.ts.session.clone());
    let foreign_event = evgen.remark("note from the other machine", vec![h.clone()]);
    let ParsedSidecar::Doc(mut doc) = parse_sidecar(&fs::read(&sc).unwrap()).unwrap() else {
        panic!("parse");
    };
    doc.events
        .push(photoproof_core::sidecar::SidecarEntry::Canonical(
            foreign_event.clone(),
        ));
    fs::write(&sc, doc.to_bytes()).unwrap();

    engine.scan(&[w.root()], now).unwrap();
    // Union imported the foreign event; the file converged to the union.
    let ids: Vec<_> =
        w.ts.store
            .events_for_image(&h)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
    assert!(ids.contains(&foreign_event.id));
    assert_eq!(
        fs::read(&sc).unwrap(),
        engine.image_doc(&h).unwrap().to_bytes()
    );
}

#[test]
fn s13_10_rematch_swapped_images_both_journals_survive() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (pa, ha) = w.image("IMG_SWAP_A.jpg", b"swap-image-a");
    let (pb, hb) = w.image("IMG_SWAP_B.jpg", b"swap-image-b");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("journal of A", vec![ha.clone()]),
            None,
        )
        .unwrap();
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("journal of B", vec![hb.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();

    // The user swaps the image files; the sidecars stay behind. Adjacency
    // must never bind: hash does.
    fs::write(&pa, b"swap-image-b").unwrap();
    fs::write(&pb, b"swap-image-a").unwrap();
    w.locator.set(
        ha.clone(),
        AdjacentLocation::Writable {
            image_path: pb.clone(),
        },
    );
    w.locator.set(
        hb.clone(),
        AdjacentLocation::Writable {
            image_path: pa.clone(),
        },
    );

    engine.scan(&[w.root()], now).unwrap();

    // Both journals end up keyed correctly: the slot beside each path holds
    // the journal of the image actually present.
    let check = |p: &Path, h: &ContentHash, text: &str| {
        let ParsedSidecar::Doc(doc) =
            parse_sidecar(&fs::read(sidecar_path_for_image(p)).unwrap()).unwrap()
        else {
            panic!("parse");
        };
        assert_eq!(&doc.image.as_ref().unwrap().hash, h);
        let evs = doc.canonical_events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].text.as_deref(), Some(text));
    };
    check(&pa, &hb, "journal of B");
    check(&pb, &ha, "journal of A");
    // Nothing lost in the index either.
    assert_eq!(w.ts.store.events_for_image(&ha).unwrap().len(), 1);
    assert_eq!(w.ts.store.events_for_image(&hb).unwrap().len(), 1);
}

#[test]
fn s13_10_rematch_snapshot_drift_hash_wins() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_DRIFT.jpg", b"drift-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);

    // Corrupt the snapshot (filename/byte_size lie); hash still matches.
    let ParsedSidecar::Doc(mut doc) = parse_sidecar(&fs::read(&sc).unwrap()).unwrap() else {
        panic!("parse");
    };
    let snap = doc.image.as_mut().unwrap();
    snap.filename = "WRONG_NAME.jpg".into();
    snap.byte_size = 999_999;
    fs::write(&sc, doc.to_bytes()).unwrap();

    engine.scan(&[w.root()], now).unwrap();
    let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&sc).unwrap()).unwrap() else {
        panic!("parse");
    };
    let snap = doc.image.as_ref().unwrap();
    assert_eq!(
        snap.filename, "IMG_DRIFT.jpg",
        "snapshot refreshed on rewrite"
    );
    assert_eq!(snap.byte_size, b"drift-image".len() as u64);
    assert_eq!(snap.hash, h);
}

#[test]
fn s13_10_rematch_corrupt_renamed_aside_and_regenerated() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_CORRUPT.jpg", b"corrupt-sidecar-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);
    let canonical = fs::read(&sc).unwrap();

    // Damaged but still self-identifying as ours (the format marker
    // survives): invalid JSON that only a Photoproof writer could have left.
    let garbage: &[u8] = b"{\"events\": [, \"format\": \"photoproof-sidecar\", TRUNC";
    fs::write(&sc, garbage).unwrap();

    let report = engine.scan(&[w.root()], now).unwrap();
    assert_eq!(report.corrupt_renamed.len(), 1);
    let (orig, aside) = &report.corrupt_renamed[0];
    assert_eq!(orig, &sc);
    // Preserved aside for manual inspection, never deleted.
    assert!(aside.exists());
    assert_eq!(fs::read(aside).unwrap(), garbage);
    assert!(
        aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(".corrupt-")
    );
    // A fresh sidecar was written from the index.
    assert_eq!(fs::read(&sc).unwrap(), canonical);
}

// ---------------------------------------------------------------------------
// §13.11 mtime-stable writer
// ---------------------------------------------------------------------------

#[test]
fn s13_11_mtime_stable_writer() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_MT.jpg", b"mtime-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc = sidecar_path_for_image(&p);

    let meta_before = fs::metadata(&sc).unwrap();
    #[cfg(unix)]
    let ino_before = std::os::unix::fs::MetadataExt::ino(&meta_before);

    // Flushing identical bytes performs no write: no temp file, no rename,
    // no mtime touch (the WriteOutcome is the instrumentation).
    let FlushOutcome::Adjacent { outcome, .. } = engine.flush_image(&h, now).unwrap() else {
        panic!("adjacent");
    };
    assert_eq!(outcome, WriteOutcome::SkippedIdentical);
    assert!(list_temps(w.images.path()).is_empty(), "no temp created");
    let meta_after = fs::metadata(&sc).unwrap();
    assert_eq!(
        meta_before.modified().unwrap(),
        meta_after.modified().unwrap(),
        "mtime untouched"
    );
    #[cfg(unix)]
    assert_eq!(
        ino_before,
        std::os::unix::fs::MetadataExt::ino(&meta_after),
        "inode unchanged (no rename happened)"
    );

    // A one-event change still writes normally.
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("another", vec![h.clone()]),
            None,
        )
        .unwrap();
    let FlushOutcome::Adjacent { outcome, .. } = engine.flush_image(&h, now).unwrap() else {
        panic!("adjacent");
    };
    assert_eq!(outcome, WriteOutcome::Written);

    // Repeated reconciliation passes over a converged library leave every
    // sidecar's mtime untouched.
    engine.scan(&[w.root()], now).unwrap();
    let snapshot: Vec<(PathBuf, SystemTime)> = dir_snapshot(w.images.path())
        .keys()
        .map(|rel| {
            let p = w.images.path().join(rel);
            let m = fs::metadata(&p).unwrap().modified().unwrap();
            (p, m)
        })
        .collect();
    engine.scan(&[w.root()], now).unwrap();
    engine.scan(&[w.root()], now).unwrap();
    for (p, m) in snapshot {
        assert_eq!(
            fs::metadata(&p).unwrap().modified().unwrap(),
            m,
            "{} touched by a converged scan",
            p.display()
        );
    }
}

// ---------------------------------------------------------------------------
// §7.2 / §8.3 / §12 — session journals, overflow in export, manifest
// ---------------------------------------------------------------------------

#[test]
fn s08_3_export_includes_overflow_and_session_journals() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (_pa, ha) = w.image("IMG_ADJ.jpg", b"adjacent-image");
    // An image whose only truth lives in overflow (unwritable volume).
    let hb = ContentHash::from_bytes_of(b"overflow-only-image");
    w.locator.set(
        hb.clone(),
        AdjacentLocation::Unwritable {
            image_path: None,
            volume_id: Some("vol-ro".into()),
        },
    );

    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("adjacent note", vec![ha.clone()]),
            None,
        )
        .unwrap();
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("overflow note", vec![hb.clone()]),
            None,
        )
        .unwrap();
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("session-level note", vec![]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();

    let export = w.app_data.path().join("export");
    let report = engine.export(&export, "0.1.0", now, &[]).unwrap();
    assert_eq!(report.images, 2, "overflow-only image exports identically");
    assert_eq!(report.session_journals.len(), 1);
    for p in report.sidecars.iter().chain(&report.session_journals) {
        assert!(p.exists());
    }

    let manifest: Value =
        serde_json::from_slice(&fs::read(&report.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["format"], "photoproof-export-manifest");
    assert_eq!(manifest["counts"]["images"], 2);
    assert_eq!(manifest["counts"]["events"], 3);
    assert_eq!(manifest["counts"]["sessions"], 1);
    assert_eq!(manifest["images"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["session_journals"].as_array().unwrap().len(), 1);

    // Export output is the rebuild input: a fresh store rebuilt from the
    // export directory holds everything, session-level events included.
    let w2 = World::new();
    let engine2 = w2.engine();
    let report =
        rebuild_from_sidecars(&engine2, &RebuildOptions::export_dir(&export), now).unwrap();
    assert_eq!(report.events_imported, 3);
    assert!(report.manifest_discrepancies.is_empty());
    assert_eq!(w2.ts.store.events_for_image(&ha).unwrap().len(), 1);
    assert_eq!(w2.ts.store.events_for_image(&hb).unwrap().len(), 1);
    assert_eq!(
        w2.ts
            .store
            .sessionlevel_events(&w.ts.session)
            .unwrap()
            .len(),
        1
    );

    // The manifest is advisory: a missing file is reported, never fatal.
    fs::remove_file(report_path_of_first_image(&export)).unwrap();
    let w3 = World::new();
    let engine3 = w3.engine();
    let report =
        rebuild_from_sidecars(&engine3, &RebuildOptions::export_dir(&export), now).unwrap();
    assert!(!report.manifest_discrepancies.is_empty());
}

fn report_path_of_first_image(export: &Path) -> PathBuf {
    let manifest: Value =
        serde_json::from_slice(&fs::read(export.join("manifest.photoproof.json")).unwrap())
            .unwrap();
    let rel = manifest["images"][0]["sidecar"].as_str().unwrap();
    export.join(rel)
}

#[test]
fn s07_2_session_journal_lifecycle() {
    let w = World::new();
    let engine = w.engine();

    // A session with no zero-target events gets no file.
    let (_p, h) = w.image("IMG_S.jpg", b"session-test-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("targeted", vec![h.clone()]),
            None,
        )
        .unwrap();
    assert!(engine.flush_session(&w.ts.session).unwrap().is_none());
    assert!(!session_journal_path(w.app_data.path(), &w.ts.session).exists());

    // Zero-target events live in the session journal: identical envelope,
    // image: null, that one session in the map.
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("session-level", vec![]),
            None,
        )
        .unwrap();
    let (path, _) = engine.flush_session(&w.ts.session).unwrap().unwrap();
    assert_eq!(path, session_journal_path(w.app_data.path(), &w.ts.session));
    let text = read_to_string(&path);
    assert!(text.contains("\"image\": null"));
    assert!(text.contains("session-level"));
    let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&path).unwrap()).unwrap() else {
        panic!("session journal parses as a sidecar");
    };
    assert!(doc.image.is_none());
    assert_eq!(doc.sessions.len(), 1);
    assert_eq!(doc.canonical_events().len(), 1);
    // Canonical bytes: rewriting is mtime-stable.
    let (_, outcome) = engine.flush_session(&w.ts.session).unwrap().unwrap();
    assert_eq!(outcome, WriteOutcome::SkippedIdentical);
}

// ---------------------------------------------------------------------------
// §3.5 / §3.6 — canonical serialization, byte-exact against the spec example
// ---------------------------------------------------------------------------

#[test]
fn s03_6_spec_example_is_byte_exact() {
    // Extract the §3.6 complete example from the normative spec and verify
    // the serializer reproduces it byte-for-byte (parse → re-serialize is
    // the identity on canonical files).
    let spec =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/SIDECARS.md"))
            .expect("spec/SIDECARS.md readable");
    let after = spec.split("### 3.6").nth(1).expect("§3.6 present");
    let fenced = after
        .split("```json\n")
        .nth(1)
        .and_then(|s| s.split("```").next())
        .expect("fenced example present");
    let example = fenced.as_bytes();

    let ParsedSidecar::Doc(doc) = parse_sidecar(example).expect("example parses") else {
        panic!("example is a sidecar");
    };
    // Every event in the example is strict EVENTS §4 canonical.
    assert_eq!(doc.canonical_events().len(), 8);
    assert_eq!(doc.opaque_count(), 0);
    assert_eq!(
        doc.to_bytes(),
        example,
        "serializer must reproduce the §3.6 example byte-for-byte"
    );
}

// ---------------------------------------------------------------------------
// §2.2 — case-only rename of the image is a relink
// ---------------------------------------------------------------------------

#[test]
fn s02_2_case_only_rename_relinks_sidecar() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p_old, h) = w.image("img_case.jpg", b"case-rename-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();
    let sc_old = sidecar_path_for_image(&p_old);
    assert!(sc_old.exists());

    // Case-only rename of the image (case-sensitive volume): relink.
    let p_new = w.images.path().join("IMG_CASE.JPG");
    fs::rename(&p_old, &p_new).unwrap();
    w.locator.set(
        h.clone(),
        AdjacentLocation::Writable {
            image_path: p_new.clone(),
        },
    );

    // On the next write the sidecar is renamed to match: new name written
    // and verified, old name deleted.
    engine.flush_image(&h, now).unwrap();
    let sc_new = sidecar_path_for_image(&p_new);
    assert!(sc_new.exists(), "new-case sidecar written");
    let actual_names: Vec<String> = fs::read_dir(w.images.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let new_name = sc_new.file_name().unwrap().to_string_lossy();
    let old_name = sc_old.file_name().unwrap().to_string_lossy();
    assert!(
        actual_names.iter().any(|name| name == new_name.as_ref()),
        "directory entry uses the image's new byte-exact spelling: {actual_names:?}"
    );
    assert!(
        !actual_names.iter().any(|name| name == old_name.as_ref()),
        "old directory-entry spelling removed after verification: {actual_names:?}"
    );
    let ParsedSidecar::Doc(doc) = parse_sidecar(&fs::read(&sc_new).unwrap()).unwrap() else {
        panic!("parse");
    };
    assert_eq!(doc.image.as_ref().unwrap().hash, h);
    assert_eq!(doc.image.as_ref().unwrap().filename, "IMG_CASE.JPG");
}

// ---------------------------------------------------------------------------
// §9.3 / §9.4 — orphan temps and the transient backoff ladder
// ---------------------------------------------------------------------------

#[test]
fn s09_3_orphan_temps_ignored_and_cleaned() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (_p, h) = w.image("IMG_T.jpg", b"temp-test-image");
    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("note", vec![h.clone()]),
            None,
        )
        .unwrap();
    engine.flush_all(now).unwrap();

    // A fresh orphan temp for a target nothing rewrites: ignored by
    // scanners (never parsed, never merged), not deleted while younger than
    // one hour.
    let orphan = w
        .images
        .path()
        .join("IMG_GONE.jpg.photoproof.json.tmp-deadbeef");
    fs::write(&orphan, b"{ partial garbage").unwrap();
    let report = engine.scan(&[w.root()], now).unwrap();
    assert!(orphan.exists(), "young temp survives the scan");
    assert!(report.failures.is_empty(), "temps are never parsed");
    assert!(report.collisions.is_empty());

    // §9.3: deleted when the same target is next written.
    let same_target = w
        .images
        .path()
        .join("IMG_T.jpg.photoproof.json.tmp-0badf00d");
    fs::write(&same_target, b"{ partial garbage").unwrap();
    engine.flush_image(&h, now).unwrap();
    assert!(
        !same_target.exists(),
        "next write of the target cleans temps"
    );

    // §9.3: deleted when older than one hour (cutoff injected here).
    let removed =
        clean_orphan_temps(w.images.path(), SystemTime::now() + Duration::from_secs(10)).unwrap();
    assert_eq!(removed, 1);
    assert!(!orphan.exists());
}

#[test]
fn s09_4_transient_failure_backoff_ladder() {
    use photoproof_core::sidecar::Debouncer;
    let mut deb = Debouncer::new();
    let h = common::hash(1);
    let base = t(NOW);

    deb.note_event(&h, base);
    // Quiet window passes; due.
    assert_eq!(deb.due(ms(base, 2_000)), vec![h.clone()]);
    // Backoff ladder: 1 s, 5 s, 30 s, 5 min, then repeats the last step.
    let mut at = 2_000;
    for backoff in [1_000i64, 5_000, 30_000, 300_000, 300_000] {
        deb.note_failure(&h, ms(base, at), "ENOSPC".into());
        assert!(
            deb.due(ms(base, at + backoff - 1)).is_empty(),
            "retry gated until backoff {backoff}"
        );
        assert_eq!(deb.due(ms(base, at + backoff)), vec![h.clone()]);
        at += backoff;
    }
    assert_eq!(deb.attempts(&h), 5);
    // The durable entry persists until an explicit success.
    assert!(deb.is_pending(&h));
    deb.clear(&h);
    assert!(!deb.is_pending(&h));
}

// ---------------------------------------------------------------------------
// §11 step 3 — session-level redaction rewrites the session journal
// ---------------------------------------------------------------------------

#[test]
fn s11_session_level_redaction_scrubs_journal() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);

    let secret =
        w.ts.store
            .append(
                &w.ts.session,
                common::d_remark("a session-level seekrit", vec![]),
                None,
            )
            .unwrap();
    let (path, _) = engine.flush_session(&w.ts.session).unwrap().unwrap();
    assert!(read_to_string(&path).contains("seekrit"));

    // Session journals live in app data — always reachable; the rewrite is
    // immediate (bypasses debounce) and the receipt records it.
    let receipt = engine.redact(&secret.id, now).unwrap();
    assert_eq!(receipt.session_journals, vec![path.clone()]);
    assert!(receipt.queued.is_empty());
    let text = read_to_string(&path);
    assert!(!text.contains("seekrit"));
    assert!(text.contains("redacted_by"));
    assert_eq!(fts_match_count(&w.ts.path(), "seekrit"), 0);
}

// ---------------------------------------------------------------------------
// §10.2 — same id, different content (deterministic conflict rule)
// ---------------------------------------------------------------------------

#[test]
fn s10_2_same_id_different_content_conflict() {
    let now = t(NOW);
    let w = World::new();
    let evgen = common::Gen::new(w.ts.session.clone());
    let h = ContentHash::from_bytes_of(b"conflict-image");
    let a = evgen.remark("copy alpha", vec![h.clone()]);
    let mut b = a.clone();
    b.text = Some("copy beta".into());

    let started =
        w.ts.store
            .session(&w.ts.session)
            .unwrap()
            .unwrap()
            .started_ts;
    let mk = |e: &photoproof_core::Event, name: &str| -> (PathBuf, Value) {
        let ev: Value = serde_json::from_slice(&EventStore::canonical_json(e)).unwrap();
        let doc = json!({
            "format": "photoproof-sidecar",
            "schema_version": 1,
            "image": {"hash": h.as_str(), "filename": name, "byte_size": 1},
            "sessions": {w.ts.session.as_str(): {
                "started_at": started.to_rfc3339(), "app_version": "0.1.0"}},
            "events": [ev],
        });
        (w.images.path().join(format!("{name}.photoproof.json")), doc)
    };
    let (pa, da) = mk(&a, "copy_a.jpg");
    let (pb, db) = mk(&b, "copy_b.jpg");
    fs::write(&pa, to_pretty_canonical(&da)).unwrap();
    fs::write(&pb, to_pretty_canonical(&db)).unwrap();

    let engine = w.engine();
    let report = rebuild_from_sidecars(
        &engine,
        &RebuildOptions {
            roots: vec![w.root()],
            include_app_data: false,
            converge: false,
        },
        now,
    )
    .unwrap();

    // The byte-wise lesser canonical serialization wins ("copy alpha" <
    // "copy beta" at the text field); the loser is preserved verbatim.
    let kept = w.ts.store.raw_event(&a.id).unwrap().unwrap();
    assert_eq!(kept.text.as_deref(), Some("copy alpha"));
    assert!(report.id_conflicts.contains(&a.id.to_string()));
    assert_eq!(report.conflict_losers.len(), 1);
    assert!(report.conflict_losers[0].1.contains("copy beta"));
}

// ---------------------------------------------------------------------------
// §9.4 — offline volumes defer; the durable queue carries the flush
// ---------------------------------------------------------------------------

#[test]
fn s09_4_offline_volume_defers_until_mount() {
    let w = World::new();
    let engine = w.engine();
    let now = t(NOW);
    let (p, h) = w.image("IMG_OFF.jpg", b"offline-image");
    w.locator
        .set(h.clone(), AdjacentLocation::Offline { volume_id: None });

    w.ts.store
        .append(
            &w.ts.session,
            common::d_remark("queued note", vec![h.clone()]),
            None,
        )
        .unwrap();
    assert!(matches!(
        engine.flush_image(&h, now).unwrap(),
        FlushOutcome::Deferred
    ));
    // The durable dirty entry persists.
    assert_eq!(w.ts.store.dirty_images().unwrap().len(), 1);
    assert!(!sidecar_path_for_image(&p).exists());

    // Flush happens at mount.
    w.locator.set(
        h.clone(),
        AdjacentLocation::Writable {
            image_path: p.clone(),
        },
    );
    engine.on_volume_mount(now).unwrap();
    assert!(sidecar_path_for_image(&p).exists());
    assert!(w.ts.store.dirty_images().unwrap().is_empty());
}
