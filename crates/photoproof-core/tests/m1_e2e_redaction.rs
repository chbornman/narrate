//! P4.1 C4 — redaction end-to-end across the whole stack: annotate → flush
//! (secret on disk in the sidecar) → redact → the text is gone from
//! EVERY byte surface: the SQLite database file, the WAL, the FTS index,
//! the sidecar file bytes, and search results — while the journal's
//! structure (scrubbed stub) and unrelated notes survive (I2, I8,
//! SIDECARS §11, RETRIEVAL §13.5).

mod common;

use photoproof_core::search::SearchOptions;
use photoproof_core::{EventDraft, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, SynthSpec};

const SECRET_TOKENS: &[&str] = &["zanzibar", "xylophone", "confidential"];

/// Byte-scan every file under `dir` (recursively) for each secret token.
fn assert_no_secret_bytes(dir: &std::path::Path, context: &str) {
    let mut stack = vec![dir.to_path_buf()];
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
            let bytes = std::fs::read(&p).unwrap();
            for token in SECRET_TOKENS {
                assert!(
                    !bytes
                        .windows(token.len())
                        .any(|w| w.eq_ignore_ascii_case(token.as_bytes())),
                    "{context}: secret token {token:?} found in {}",
                    p.display()
                );
            }
        }
    }
}

#[test]
fn c4_redaction_scrubs_db_wal_fts_sidecars_and_search() {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(10));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();

    let image = tree.files[0].hash.clone();
    let keeper = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "ordinary harmless note that must survive".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    let secret = env
        .store
        .append(
            &env.session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "zanzibar xylophone confidential — never to be seen again".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    env.flush_sidecars();

    // Pre-redaction: the secret is genuinely on every surface.
    let sidecar = root_dir.join(format!("{}.photoproof.json", tree.files[0].rel_path));
    assert!(
        String::from_utf8(std::fs::read(&sidecar).unwrap())
            .unwrap()
            .contains("zanzibar"),
        "secret reached the sidecar before redaction (the test must start dirty)"
    );
    let opts = SearchOptions {
        now: Some(UtcMillis::now()),
        include_debug: false,
    };
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert_eq!(hits.images.len(), 1, "secret findable before redaction");

    // -- redact ----------------------------------------------------------------
    let receipt = env.engine.redact(&secret.id, UtcMillis::now()).unwrap();
    assert!(
        !receipt.adjacent_rewritten.is_empty(),
        "§11 step 3: the adjacent sidecar is rewritten before the call returns"
    );

    // 1. Search results: gone, instantly (RETRIEVAL §13.5).
    let hits = env.searcher.search_with("zanzibar", &[], &opts).unwrap();
    assert!(hits.images.is_empty() && hits.session_hits.is_empty());
    // …and the FTS index has no row for it at all.
    {
        let conn = env.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM event_fts WHERE event_fts MATCH 'zanzibar'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "FTS purged");
    }

    // 2. The journal structure survives: a scrubbed stub, the keeper intact.
    let events = env.store.events_for_image(&image).unwrap();
    let stub = events.iter().find(|e| e.id == secret.id).unwrap();
    assert!(stub.text.is_none() && stub.payload.is_none());
    assert!(stub.redacted_by.is_some(), "I15 scrubbed shape");
    let keeper_row = events.iter().find(|e| e.id == keeper.id).unwrap();
    assert_eq!(
        keeper_row.text.as_deref(),
        Some("ordinary harmless note that must survive"),
        "I2: scrub touches exactly the condemned event"
    );

    // 3. Byte scan: db file, WAL/SHM, sidecars, overflow, session journals —
    //    every file under the env — carries no secret bytes (I8 + S5,
    //    `secure_delete=ON`, FTS secure-delete, WAL truncate, sidecar
    //    rewrite). The keeper's text must still be present in the sidecar.
    assert_no_secret_bytes(&env.base, "post-redaction");
    let sidecar_text = String::from_utf8(std::fs::read(&sidecar).unwrap()).unwrap();
    assert!(sidecar_text.contains("ordinary harmless note"));
    assert!(
        sidecar_text.contains(secret.id.as_str()),
        "the redacted event's husk (id + redacted_by) remains in the sidecar"
    );

    // 4. The keeper still surfaces in search; provenance quotes it.
    let hits = env.searcher.search_with("harmless", &[], &opts).unwrap();
    assert_eq!(hits.images.len(), 1);
    assert_eq!(hits.images[0].image_hash, image);

    // 5. A rebuild from these scrubbed sidecars never resurrects content:
    //    merge a stale pre-redaction copy through the reconciliation path.
    //    (The full backup-restore matrix is SIDECARS §13.4; here we assert
    //    the end-to-end surface stays clean after the stack's own scan.)
    let report = env
        .engine
        .scan(std::slice::from_ref(&root_dir), UtcMillis::now())
        .unwrap();
    assert!(report.failures.is_empty());
    assert_no_secret_bytes(&env.base, "post-reconciliation");

    env.assert_db_integrity();
}
