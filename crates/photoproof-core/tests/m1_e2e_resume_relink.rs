//! P4.1 C2 — full-stack interrupt/resume + relink flow:
//! ingest interrupted (in-process cancel) → restart → resume to completion
//! (no duplicates, no misses) → annotate → move the file externally while
//! the app is "stopped" → startup reconciliation relinks → search still
//! finds the image by content hash, journal intact at the new path.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use photoproof_core::library::{Availability, QueueOptions, ScanOptions};
use photoproof_core::search::SearchOptions;
use photoproof_core::{ContentHash, EventDraft, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, SynthSpec};

fn hash_at(env: &M1Env, vol_rel: &str) -> ContentHash {
    let conn = env.conn();
    let h: String = conn
        .query_row(
            "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
            [vol_rel],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("no active path for {vol_rel}: {e}"));
    ContentHash::from_hex(&h).unwrap()
}

#[test]
fn c2_interrupted_ingest_resumes_then_relinks_after_external_move() {
    let mut env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let n = 300;
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(n));
    let root_id = env.register("photos");

    // -- interrupted rounds: cancel mid-scan and mid-queue, then restart ----
    for round in 0..3u64 {
        let cancel: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let delay = 15 + round * 60;
        let killer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            c2.store(true, Ordering::Relaxed);
        });
        let _ = env.lib.scan_root(
            &root_id,
            &ScanOptions {
                cancel: Some(Arc::clone(&cancel)),
                ..Default::default()
            },
        );
        let _ = env.lib.process_queue(&QueueOptions {
            cancel: Some(Arc::clone(&cancel)),
            max_items: None,
        });
        killer.join().unwrap();
        env = env.reopen(); // running→pending recovery, temp sweep
        env.probe_volumes();
    }

    // -- resume to completion: no duplicates, no misses ----------------------
    let report = env.scan(&root_id);
    assert!(!report.cancelled);
    env.drain();
    assert_eq!(
        env.lib.image_count().unwrap(),
        tree.unique_hashes as i64,
        "exactly one images row per unique hash after resume"
    );
    {
        let conn = env.conn();
        for (rel, hash) in tree.expected() {
            let stored: String = conn
                .query_row(
                    "SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'",
                    [format!("photos/{rel}")],
                    |r| r.get(0),
                )
                .unwrap_or_else(|e| panic!("missing {rel}: {e}"));
            assert_eq!(&stored, hash.as_str(), "{rel}");
        }
    }
    env.assert_db_integrity();

    // -- annotate ---------------------------------------------------------------
    let moved_rel = &tree.files[7].rel_path; // a JPEG-rich slot
    let image = hash_at(&env, &format!("photos/{moved_rel}"));
    env.store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "the keeper from the rough water set".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    env.flush_sidecars();
    let sidecar_old = root_dir.join(format!("{moved_rel}.photoproof.json"));
    assert!(sidecar_old.exists());

    // -- external move while "stopped" -------------------------------------------
    // The user reorganizes: file (and its sidecar, as a careful user would)
    // moves into a new folder outside the app.
    let new_rel = "keepers/best-of-2026.jpg";
    let to = root_dir.join(new_rel);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(root_dir.join(moved_rel), &to).unwrap();
    std::fs::rename(
        &sidecar_old,
        root_dir.join(format!("{new_rel}.photoproof.json")),
    )
    .unwrap();

    // Restart: startup reconciliation must relink with zero re-hash of the
    // moved file's bytes proving content identity at most once (§13.2/B17).
    let env = env.reopen();
    env.probe_volumes();
    env.scan(&root_id);
    env.drain();

    // Same identity, new path, journal intact.
    assert_eq!(
        env.lib.image_count().unwrap(),
        tree.unique_hashes as i64,
        "relink created no new image"
    );
    let best = env.lib.best_path(&image).unwrap().unwrap();
    assert_eq!(best.row.rel_path, format!("photos/{new_rel}"));
    assert_eq!(
        env.lib.availability(&image).unwrap(),
        Availability::Available
    );
    let journal = env.store.folded_journal(&image).unwrap();
    assert_eq!(journal.len(), 1, "journal followed the hash");

    // -- search still finds it by content hash ------------------------------------
    let results = env
        .searcher
        .search_with(
            "rough water keeper",
            &[],
            &SearchOptions {
                now: Some(UtcMillis::now()),
                include_debug: false,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(
        results.images.iter().any(|r| r.image_hash == image),
        "search finds the moved image by content hash"
    );

    // The grid lists it in its new folder, dot lit.
    let items = env.lib.list_folder(&root_id, "keepers").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].hash, image);
    assert!(items[0].has_journal);
}
