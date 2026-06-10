//! P4.1 C3 — the process-level `kill -9` ingest harness (FOUNDER-CHECKLIST
//! bucket 3). A child process (this test binary re-invoking itself, gated by
//! `PP_KILL9_BASE`) ingests a synthetic tree, annotates, and flushes
//! sidecars; the parent SIGKILLs it at randomized points, restarts ingest,
//! and asserts: no duplicate images, no missed files, no SQLite corruption
//! (`PRAGMA integrity_check` after every kill), no torn preview artifacts,
//! and sidecars converge to the journal.
//!
//! This is strictly stronger than the in-process cancellation tests
//! (LIBRARY §13.3) and the simulated-kill writer harness (SIDECARS §13.3):
//! a real SIGKILL severs the process mid-syscall with no cleanup at all.
//!
//! Randomized kill points: seed printed on every run; reproduce with
//! `PHOTOPROOF_TEST_KILL9_SEED=<seed>`.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use photoproof_core::sidecar::{FlushOutcome, WriteOutcome, is_temp_orphan};
use photoproof_core::{ContentHash, EventDraft, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, Rng, SynthSpec};

fn scaled(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn integrity_ok(db: &std::path::Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .unwrap();
    let answer: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(answer, "ok", "integrity_check after kill");
}

/// The child worker: a no-op unless spawned by the parent with
/// `PP_KILL9_BASE` set. Runs the real ingest + annotate + flush sequence to
/// completion (unless killed first).
#[test]
fn kill9_child() {
    let Ok(base) = std::env::var("PP_KILL9_BASE") else {
        return; // normal suite run: nothing to do
    };
    let round: u64 = std::env::var("PP_KILL9_ROUND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let env = M1Env::open(PathBuf::from(base));
    env.probe_volumes();
    let root_id: String = env
        .conn()
        .query_row(
            "SELECT root_id FROM roots WHERE state = 'active' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("parent registered the root");
    env.scan(&root_id);
    // Annotate the lexicographically-first image, so kills can land
    // mid-append and mid-sidecar-write, not only mid-ingest.
    if let Ok(h) = env.conn().query_row(
        "SELECT image_hash FROM paths WHERE state = 'active' ORDER BY rel_path LIMIT 1",
        [],
        |r| r.get::<_, String>(0),
    ) {
        let image = ContentHash::from_hex(&h).unwrap();
        env.store
            .append(
                &env.session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: format!("kill9 round {round}: this note must survive the crash"),
                    targets: vec![image],
                },
                None,
            )
            .unwrap();
        env.engine.flush_all(UtcMillis::now()).unwrap();
    }
    env.drain();
    env.engine.flush_all(UtcMillis::now()).unwrap();
}

#[test]
fn c3_kill9_process_harness() {
    let n = scaled("PHOTOPROOF_TEST_KILL9_FILES", 350) as usize;
    let rounds = scaled("PHOTOPROOF_TEST_KILL9_ROUNDS", 6);
    let seed = scaled(
        "PHOTOPROOF_TEST_KILL9_SEED",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
            | 1,
    );
    eprintln!("kill9 harness seed = {seed} (reproduce: PHOTOPROOF_TEST_KILL9_SEED={seed})");
    let mut rng = Rng::new(seed);

    // Parent sets the stage, then releases the database to the children.
    let mut env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(n));
    let root_id = env.register("photos");
    // One annotation up front, left dirty: the convergence assertions below
    // never depend on a child surviving long enough to append its own.
    env.store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "kill9 baseline: written before any kill".into(),
                targets: vec![tree.files[0].hash.clone()],
            },
            None,
        )
        .unwrap();
    let base = env.base.clone();
    let db = env.db.clone();
    let tmp = env.tmp.take();
    drop(env);

    let exe = std::env::current_exe().unwrap();
    for round in 0..rounds {
        let mut child = Command::new(&exe)
            .arg("kill9_child")
            .arg("--exact")
            .arg("--test-threads=1")
            .env("PP_KILL9_BASE", &base)
            .env("PP_KILL9_ROUND", round.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child ingest");
        // Randomized kill point: from before the child even opens the
        // database to deep inside queue processing / sidecar flushes.
        let sleep_ms = 30 + rng.below(1400);
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        let _ = child.kill(); // SIGKILL on unix
        let _ = child.wait();
        // The database must never be corrupt, immediately after every kill.
        integrity_ok(&db);
    }

    // -- parent restarts ingest and asserts convergence ----------------------
    let mut env = M1Env::open(base);
    env.tmp = tmp;
    env.probe_volumes();
    let report = env.scan(&root_id);
    assert!(!report.cancelled);
    env.drain();
    env.assert_db_integrity();

    // No duplicates: exactly one images row per unique hash.
    assert_eq!(
        env.lib.image_count().unwrap(),
        tree.unique_hashes as i64,
        "duplicate or missing images rows after {rounds} kills"
    );
    // No misses: every generated file is an active path with the right hash
    // (independent ground truth from the generator).
    {
        let conn = env.conn();
        let mut stmt = conn
            .prepare("SELECT image_hash FROM paths WHERE rel_path = ?1 AND state = 'active'")
            .unwrap();
        for (rel, hash) in tree.expected() {
            let stored: String = stmt
                .query_row([format!("photos/{rel}")], |r| r.get(0))
                .unwrap_or_else(|e| panic!("missing active path for {rel}: {e}"));
            assert_eq!(&stored, hash.as_str(), "{rel}");
        }
        // Queue fully settled: nothing pending/running/error.
        let (pending, running, error): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(CASE WHEN state = 'pending' THEN 1 END),
                        COUNT(CASE WHEN state = 'running' THEN 1 END),
                        COUNT(CASE WHEN state = 'error' THEN 1 END)
                 FROM ingest_passes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((pending, running, error), (0, 0, 0), "queue not settled");
    }

    // No torn cache artifacts: every artifact decodes; reopen swept temps.
    {
        let mut artifacts = 0usize;
        let mut stack = vec![env.cache.clone()];
        while let Some(d) = stack.pop() {
            if !d.exists() {
                continue;
            }
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    !name.starts_with(".pp-tmp-"),
                    "stranded preview temp {name} survived restart"
                );
                if name.ends_with(".webp") {
                    artifacts += 1;
                    image::open(&p).unwrap_or_else(|e| panic!("torn artifact {name}: {e}"));
                }
            }
        }
        assert_eq!(artifacts, 2 * tree.unique_hashes, "thumb + display each");
    }

    // Sidecars converge: drain the dirty queue, then reconcile the tree —
    // a second pass must rewrite nothing and find nothing corrupt.
    env.flush_sidecars();
    let rep1 = env
        .engine
        .scan(std::slice::from_ref(&root_dir), UtcMillis::now())
        .unwrap();
    assert!(
        rep1.failures.is_empty(),
        "sidecar parse failures: {:?}",
        rep1.failures
    );
    assert!(
        rep1.corrupt_renamed.is_empty(),
        "kill-9 must never tear a sidecar (atomic temp+rename): {:?}",
        rep1.corrupt_renamed
    );
    let rep2 = env
        .engine
        .scan(std::slice::from_ref(&root_dir), UtcMillis::now())
        .unwrap();
    assert!(
        rep2.converged_rewrites.is_empty() && rep2.events_imported == 0,
        "second reconciliation must be a no-op: rewrites {:?}, imported {}",
        rep2.converged_rewrites,
        rep2.events_imported
    );

    // The annotated image's sidecar byte-equals the journal serialization
    // (flush of identical bytes is the writer's own equality check).
    let annotated: String = env
        .conn()
        .query_row(
            "SELECT image_hash FROM paths WHERE state = 'active' ORDER BY rel_path LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let annotated = ContentHash::from_hex(&annotated).unwrap();
    let events = env.store.events_for_image(&annotated).unwrap();
    assert!(
        !events.is_empty(),
        "the parent's baseline annotation exists"
    );
    match env
        .engine
        .flush_image(&annotated, UtcMillis::now())
        .unwrap()
    {
        FlushOutcome::Adjacent {
            outcome: WriteOutcome::SkippedIdentical,
            path,
        } => {
            // Inert leftover temps (if any) are ignored by every scanner.
            for e in std::fs::read_dir(path.parent().unwrap()).unwrap().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.contains(".tmp-") {
                    assert!(is_temp_orphan(&name), "unexpected temp file {name}");
                }
            }
        }
        other => panic!("sidecar not converged with the journal: {other:?}"),
    }
}
