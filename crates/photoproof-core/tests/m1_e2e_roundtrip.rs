//! P4.1 C1 — full-stack M1 round-trip on a real temp filesystem:
//! ingest (synthetic tree) → annotate every kind via EventStore → search via
//! Searcher → sidecars flushed → DELETE the SQLite index (+ caches) →
//! rebuild-from-sidecars + re-ingest → identical folded journals (canonical
//! bytes) AND identical search results.
//!
//! This is EVENTS I3 / SIDECARS §13.1 / RETRIEVAL §13.8 executed across the
//! whole stack at once, wired exactly like the desktop shell (A3/A4 APIs).

mod common;

use std::collections::BTreeMap;

use photoproof_core::search::{Comparison, Filter, SearchOptions, SearchResults};
use photoproof_core::sidecar::{RebuildOptions, rebuild_from_sidecars};
use photoproof_core::{ContentHash, EventDraft, EventStore, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, SynthSpec};
use common::{d_rating, d_retract, d_revision, d_stroke};

fn hash_at(env: &M1Env, tree_root: &str, rel: &str) -> ContentHash {
    let conn = env.conn();
    let h: String = conn
        .query_row(
            "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
            [format!("{tree_root}/{rel}")],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("no active path for {rel}: {e}"));
    ContentHash::from_hex(&h).unwrap()
}

/// Canonical journal snapshot per image: the I3 byte-exact form.
fn journal_bytes(env: &M1Env, images: &[ContentHash]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for image in images {
        let mut bytes = Vec::new();
        for e in env.store.events_for_image(image).unwrap() {
            bytes.extend_from_slice(&EventStore::canonical_json(&e));
            bytes.push(b'\n');
        }
        // Folded view too: fold results must survive the rebuild, not just
        // raw rows.
        for entry in env.store.folded_journal(image).unwrap() {
            bytes.extend_from_slice(format!("{entry:?}\n").as_bytes());
        }
        out.insert(image.as_str().to_owned(), bytes);
    }
    out
}

fn run_searches(env: &M1Env) -> Vec<SearchResults> {
    let opts = SearchOptions {
        now: Some(UtcMillis::parse("2026-06-10T12:00:00.000Z").unwrap()),
        include_debug: false,
        ..SearchOptions::default()
    };
    let mut out = Vec::new();
    for (q, filters) in [
        ("driftwood", vec![]),
        ("harsh shadow", vec![]),
        ("corrected exposure", vec![]),
        ("", vec![Filter::Rating(Comparison::Gte(4))]),
        ("", vec![Filter::HasStrokes(true)]),
    ] {
        out.push(env.searcher.search_with(q, &filters, &opts).unwrap());
    }
    out
}

#[test]
fn c1_ingest_annotate_search_rebuild_round_trip() {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(40));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();
    assert_eq!(
        env.lib.image_count().unwrap(),
        tree.unique_hashes as i64,
        "one images row per unique hash (duplicates collapse)"
    );

    // -- annotate: every kind ------------------------------------------------
    let a = hash_at(&env, "photos", &tree.files[1].rel_path);
    let b = hash_at(&env, "photos", &tree.files[2].rel_path);
    let c = hash_at(&env, "photos", &tree.files[3].rel_path);
    let d = hash_at(&env, "photos", &tree.files[4].rel_path);
    let s = &env.session;

    let multi = env
        .store
        .append(
            s,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "the driftwood line pulls the eye through both frames".into(),
                targets: vec![a.clone(), b.clone()],
            },
            None,
        )
        .unwrap();
    let _ = multi;
    let shadow = env
        .store
        .append(
            s,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "harsh flash shadow on the left wall".into(),
                targets: vec![c.clone()],
            },
            None,
        )
        .unwrap();
    // Revision: folded text wins, FTS follows it.
    env.store
        .append(
            s,
            d_revision(
                shadow.id.clone(),
                "harsh shadow corrected exposure in print",
            ),
            None,
        )
        .unwrap();
    env.store
        .append(s, d_rating(5, vec![a.clone()]), None)
        .unwrap();
    env.store
        .append(s, d_rating(4, vec![d.clone()]), None)
        .unwrap();
    env.store
        .append(s, d_stroke(b.clone(), None), None)
        .unwrap();
    // A retracted remark: must fold out everywhere and stay folded out after
    // the rebuild.
    let retracted = env
        .store
        .append(
            s,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "transient thought about driftwood".into(),
                targets: vec![d.clone()],
            },
            None,
        )
        .unwrap();
    env.store
        .append(s, d_retract(retracted.id.clone()), None)
        .unwrap();
    // A redacted remark: scrubbed stub everywhere, forever.
    let secret = env
        .store
        .append(
            s,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "embarrassing private note about the client".into(),
                targets: vec![c.clone()],
            },
            None,
        )
        .unwrap();
    env.engine.redact(&secret.id, UtcMillis::now()).unwrap();
    // A session-level remark: lands in the session journal (S4).
    env.store
        .append(
            s,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "session-level note about the whole shoot".into(),
                targets: vec![],
            },
            None,
        )
        .unwrap();

    // -- search + sidecars, then snapshot ------------------------------------
    let images = [a.clone(), b.clone(), c.clone(), d.clone()];
    let before_search = run_searches(&env);
    assert!(
        before_search[0]
            .images
            .iter()
            .any(|r| r.image_hash == a || r.image_hash == b),
        "driftwood finds the multi-target remark's images"
    );
    assert!(
        !before_search[0].images.iter().any(|r| r.image_hash == d),
        "retracted remark must not surface d"
    );

    env.flush_sidecars();
    env.engine.flush_session(&env.session).unwrap();
    let before_journals = journal_bytes(&env, &images);
    let before_stats = env.store.journal_stats(&images).unwrap();
    let before_grid = env.lib.list_folder(&root_id, "").unwrap();
    let _ = before_grid;

    // Sidecar files exist beside the images (adjacent slots).
    let sidecar_a = env
        .mount
        .join("photos")
        .join(format!("{}.photoproof.json", tree.files[1].rel_path));
    assert!(sidecar_a.exists(), "adjacent sidecar for image a");

    // -- the restore drill ----------------------------------------------------
    let env = env.wipe_index_and_reopen();
    // Re-ingest the unchanged tree (same bytes ⇒ same identities)…
    let root_id2 = env.register("photos");
    env.scan(&root_id2);
    env.drain();
    // …then rebuild the journal from sidecar truth + rebuild derived state.
    let report = rebuild_from_sidecars(
        &env.engine,
        &RebuildOptions::live(vec![root_dir.clone()]),
        UtcMillis::now(),
    )
    .unwrap();
    assert!(
        report.failures.is_empty(),
        "rebuild parse failures: {:?}",
        report.failures
    );
    assert!(report.events_imported > 0);
    env.store.rebuild_derived().unwrap();

    // -- identical journals ----------------------------------------------------
    let after_journals = journal_bytes(&env, &images);
    assert_eq!(
        before_journals, after_journals,
        "folded journals and canonical event bytes identical after rebuild"
    );
    // I11: the multi-target event holds exactly one row.
    {
        let conn = env.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM annotation_events WHERE text LIKE '%driftwood line%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "multi-target remark deduped to one row (I11)");
        // Redacted text is gone from the rebuilt database too.
        let scrubbed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM annotation_events WHERE text LIKE '%embarrassing%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scrubbed, 0, "redacted content never returns (I8)");
    }

    // -- identical search results ----------------------------------------------
    let after_search = run_searches(&env);
    assert_eq!(
        before_search, after_search,
        "search results identical after delete-db + rebuild"
    );

    // -- identical badges --------------------------------------------------------
    let after_stats = env.store.journal_stats(&images).unwrap();
    assert_eq!(before_stats, after_stats, "journal_stats fold identical");
    let after_grid = env.lib.list_folder(&root_id2, "").unwrap();
    let _ = after_grid;

    env.assert_db_integrity();
}
