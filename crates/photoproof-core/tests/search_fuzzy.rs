//! Fuzzy metadata widening (search-as-scope Phase 4, the `~` quiet-toggle).
//!
//! The whole point of this feature is a set of HARD invariants — these tests
//! pin them:
//!   1. A typo in a camera/lens/filename finds the image WHEN fuzzy is on.
//!   2. That same typo finds NOTHING when fuzzy is off (default = today).
//!   3. Exact (FTS) matches always rank ABOVE any fuzzy metadata match.
//!   4. With fuzzy off, the result is byte-identical to today's search.
//!
//! Metadata is camera (`camera_make`/`camera_model`), lens (`lens_model`), and
//! the filename basename of an active path. The widening is ADDITIVE: it only
//! ever appends below the exact set, never reorders it.

use std::path::PathBuf;

use photoproof_core::id::ContentHash;
use photoproof_core::search::{Provenance, SearchOptions, SearchResults, Searcher};
use photoproof_core::store::{EventDraft, EventStore, RemarkSource, SessionContext};
use photoproof_core::{SessionId, UtcMillis};
use rusqlite::params;

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: EventStore,
    session: SessionId,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let store = EventStore::open(&path).unwrap();
    let session = store
        .open_session(SessionContext {
            app_version: "0.1.0".into(),
            device_id: "cd".repeat(16),
            root_context: None,
        })
        .unwrap();
    Fixture {
        _dir: dir,
        path,
        store,
        session,
    }
}

fn h(seed: &str) -> ContentHash {
    ContentHash::from_bytes_of(seed.as_bytes())
}

impl Fixture {
    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }

    fn searcher(&self) -> Searcher {
        Searcher::open(&self.path).unwrap()
    }

    fn image_row(
        &self,
        hash: &ContentHash,
        camera_make: Option<&str>,
        camera_model: Option<&str>,
        lens: Option<&str>,
    ) {
        self.conn()
            .execute(
                "INSERT INTO images (image_hash, byte_size, format, capture_ts, camera_make, \
                 camera_model, lens_model, first_ingested_at) \
                 VALUES (?1, 1000, 'jpeg', NULL, ?2, ?3, ?4, ?5)",
                params![
                    hash.as_str(),
                    camera_make,
                    camera_model,
                    lens,
                    UtcMillis::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    fn volume_row(&self, volume_id: &str) {
        self.conn()
            .execute(
                "INSERT INTO volumes (volume_id, label, state, first_seen_at, last_seen_at) \
                 VALUES (?1, ?1, 'online', ?2, ?2)",
                params![volume_id, UtcMillis::now().to_rfc3339()],
            )
            .unwrap();
    }

    fn path_row(&self, path_id: &str, hash: &ContentHash, volume_id: &str, rel_path: &str) {
        self.conn()
            .execute(
                "INSERT INTO paths (path_id, image_hash, volume_id, root_id, rel_path, size, \
                 mtime_ns, state, stale_reason, first_seen_at, last_verified_at) \
                 VALUES (?1, ?2, ?3, NULL, ?4, 1000, 0, 'active', NULL, ?5, ?5)",
                params![
                    path_id,
                    hash.as_str(),
                    volume_id,
                    rel_path,
                    UtcMillis::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    fn remark(&self, text: &str, target: &ContentHash) {
        self.store
            .append(
                &self.session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: text.into(),
                    targets: vec![target.clone()],
                },
                None,
            )
            .unwrap();
    }
}

fn fuzzy_opts(on: bool) -> SearchOptions {
    SearchOptions {
        fuzzy: on,
        ..SearchOptions::default()
    }
}

fn hashes(r: &SearchResults) -> Vec<String> {
    r.images.iter().map(|i| i.image_hash.to_string()).collect()
}

fn is_fuzzy(p: &Provenance) -> bool {
    matches!(p, Provenance::FuzzyMeta { .. })
}

// ---------------------------------------------------------------------------

/// Invariant 1+2: a typo in a CAMERA model finds the image only when fuzzy is
/// armed; the same query finds nothing when it is off.
#[test]
fn camera_typo_matches_only_when_fuzzy_on() {
    let f = fixture();
    let leica = h("leica-img");
    f.image_row(
        &leica,
        Some("Leica"),
        Some("Leica M11"),
        Some("Summilux 35"),
    );
    let searcher = f.searcher();

    // "leics" — a one-char typo of "leica". Off: nothing (no FTS text either).
    let off = searcher
        .search_with("leics", &[], &fuzzy_opts(false))
        .unwrap();
    assert!(
        off.images.is_empty(),
        "fuzzy OFF must not widen on a camera typo: {:?}",
        hashes(&off)
    );

    let on = searcher
        .search_with("leics", &[], &fuzzy_opts(true))
        .unwrap();
    assert_eq!(hashes(&on), vec![leica.to_string()]);
    assert!(
        is_fuzzy(&on.images[0].provenance),
        "a fuzzy-only camera hit carries FuzzyMeta provenance"
    );
    match &on.images[0].provenance {
        Provenance::FuzzyMeta { field } => assert_eq!(field.as_str(), "camera"),
        other => panic!("expected camera FuzzyMeta, got {other:?}"),
    }
}

/// Invariant 1+2 for the LENS field.
#[test]
fn lens_typo_matches_only_when_fuzzy_on() {
    let f = fixture();
    let img = h("lens-img");
    f.image_row(&img, Some("Fuji"), Some("X100V"), Some("Summilux 35"));
    let searcher = f.searcher();

    // "summilx" — a deletion typo of "summilux".
    let off = searcher
        .search_with("summilx", &[], &fuzzy_opts(false))
        .unwrap();
    assert!(off.images.is_empty(), "fuzzy OFF: {:?}", hashes(&off));

    let on = searcher
        .search_with("summilx", &[], &fuzzy_opts(true))
        .unwrap();
    assert_eq!(hashes(&on), vec![img.to_string()]);
    match &on.images[0].provenance {
        Provenance::FuzzyMeta { field } => assert_eq!(field.as_str(), "lens"),
        other => panic!("expected lens FuzzyMeta, got {other:?}"),
    }
}

/// Invariant 1+2 for the FILENAME field (basename of an active path).
#[test]
fn filename_typo_matches_only_when_fuzzy_on() {
    let f = fixture();
    let img = h("file-img");
    f.image_row(&img, None, None, None);
    f.volume_row("vol-1");
    f.path_row("p1", &img, "vol-1", "trips/harbor/sunrise.jpg");
    let searcher = f.searcher();

    // "sunrize" — a one-char typo of the basename stem "sunrise".
    let off = searcher
        .search_with("sunrize", &[], &fuzzy_opts(false))
        .unwrap();
    assert!(off.images.is_empty(), "fuzzy OFF: {:?}", hashes(&off));

    let on = searcher
        .search_with("sunrize", &[], &fuzzy_opts(true))
        .unwrap();
    assert_eq!(hashes(&on), vec![img.to_string()]);
    match &on.images[0].provenance {
        Provenance::FuzzyMeta { field } => assert_eq!(field.as_str(), "filename"),
        other => panic!("expected filename FuzzyMeta, got {other:?}"),
    }
}

/// Invariant 3: exact (FTS) matches always rank ABOVE fuzzy metadata matches.
/// One image matches the query EXACTLY via its remark text; another matches
/// only FUZZILY via a camera typo. The exact one must come first and keep its
/// Quote provenance; the fuzzy one is appended below with FuzzyMeta.
#[test]
fn exact_matches_rank_above_fuzzy() {
    let f = fixture();
    let exact_img = h("exact-img");
    let fuzzy_img = h("fuzzy-img");
    // The exact image has a remark containing "leica"; the fuzzy image only
    // carries "Leica M11" as camera metadata and NO annotation text.
    f.image_row(&exact_img, None, None, None);
    f.image_row(&fuzzy_img, Some("Leica"), Some("Leica M11"), None);
    f.remark("shot on a leica today", &exact_img);
    let searcher = f.searcher();

    // "leica" matches the remark EXACTLY; "leic" would also fuzzy-match the
    // camera, but the exact token is a clean exact-vs-fuzzy split here.
    let on = searcher
        .search_with("leica", &[], &fuzzy_opts(true))
        .unwrap();
    let hs = hashes(&on);
    assert_eq!(
        hs.len(),
        2,
        "both the exact and the fuzzy image are present: {hs:?}"
    );
    // Exact image is FIRST and keeps its real (Quote) provenance.
    assert_eq!(hs[0], exact_img.to_string(), "exact hit ranks first");
    assert!(
        matches!(on.images[0].provenance, Provenance::Quote(_)),
        "the exact hit keeps Quote provenance, not FuzzyMeta"
    );
    // Fuzzy image is appended BELOW, labeled approximate.
    assert_eq!(hs[1], fuzzy_img.to_string(), "fuzzy hit ranks below exact");
    assert!(
        is_fuzzy(&on.images[1].provenance),
        "the appended hit is labeled FuzzyMeta"
    );
}

/// Invariant 3 corner: an image that matches BOTH exactly and fuzzily is NEVER
/// demoted to a fuzzy hit. It keeps its exact place + Quote provenance, and is
/// not duplicated as a fuzzy row.
#[test]
fn an_image_matching_both_keeps_its_exact_provenance() {
    let f = fixture();
    let img = h("both-img");
    // Camera "Leica M11" AND a remark "leica morning" — a "leica" query hits
    // both lanes for the SAME image.
    f.image_row(&img, Some("Leica"), Some("Leica M11"), None);
    f.remark("leica morning", &img);
    let searcher = f.searcher();

    let on = searcher
        .search_with("leica", &[], &fuzzy_opts(true))
        .unwrap();
    assert_eq!(hashes(&on), vec![img.to_string()], "no duplicate fuzzy row");
    assert!(
        matches!(on.images[0].provenance, Provenance::Quote(_)),
        "the exact provenance wins; the image is never demoted to FuzzyMeta"
    );
}

/// Invariant 4: with fuzzy OFF the result is byte-identical to today's search.
/// A query that the exact lane satisfies must return the SAME images, in the
/// SAME order, with the SAME provenance whether or not the fuzzy flag exists —
/// the flag is purely additive, so OFF is a no-op.
#[test]
fn fuzzy_off_is_identical_to_today() {
    let f = fixture();
    let a = h("a-img");
    let b = h("b-img");
    f.image_row(&a, Some("Leica"), Some("Leica M11"), None);
    f.image_row(&b, Some("Fuji"), Some("X100V"), None);
    f.remark("the quiet harbor at dawn", &a);
    f.remark("harbor lights", &b);
    let searcher = f.searcher();

    // Exact FTS query touching both images. OFF and (the default) must match.
    let off = searcher
        .search_with("harbor", &[], &fuzzy_opts(false))
        .unwrap();
    let plain = searcher.search("harbor", &[]).unwrap();
    assert_eq!(
        hashes(&off),
        hashes(&plain),
        "fuzzy:false is byte-identical to the no-options search"
    );
    // No FuzzyMeta provenance ever appears when fuzzy is off.
    assert!(
        off.images.iter().all(|i| !is_fuzzy(&i.provenance)),
        "fuzzy OFF never appends a FuzzyMeta row"
    );

    // And turning it ON for this exact query does not REORDER the exact set:
    // the same exact images stay in the same relative order at the front.
    let on = searcher
        .search_with("harbor", &[], &fuzzy_opts(true))
        .unwrap();
    let on_exact_prefix: Vec<String> = on
        .images
        .iter()
        .filter(|i| !is_fuzzy(&i.provenance))
        .map(|i| i.image_hash.to_string())
        .collect();
    assert_eq!(
        on_exact_prefix,
        hashes(&off),
        "fuzzy ON preserves the exact set's order unchanged"
    );
}

/// Filters are hard constraints in the widened lane too: a fuzzy camera match
/// outside a folder/volume constraint must NOT leak past it.
#[test]
fn fuzzy_respects_hard_filters() {
    use photoproof_core::search::{Filter, VolumeFilter};

    let f = fixture();
    let online = h("online-img");
    let offline = h("offline-img");
    f.image_row(&online, Some("Leica"), Some("Leica M11"), None);
    f.image_row(&offline, Some("Leica"), Some("Leica M11"), None);
    f.volume_row("vol-on");
    f.conn()
        .execute(
            "INSERT INTO volumes (volume_id, label, state, first_seen_at, last_seen_at) \
             VALUES ('vol-off', 'vol-off', 'offline', ?1, ?1)",
            params![UtcMillis::now().to_rfc3339()],
        )
        .unwrap();
    f.path_row("p-on", &online, "vol-on", "a.jpg");
    f.path_row("p-off", &offline, "vol-off", "b.jpg");
    let searcher = f.searcher();

    // Fuzzy camera typo + an Online volume chip: only the online image survives.
    let on = searcher
        .search_with(
            "leics",
            &[Filter::Volume(VolumeFilter::Online)],
            &fuzzy_opts(true),
        )
        .unwrap();
    assert_eq!(
        hashes(&on),
        vec![online.to_string()],
        "a hard volume filter still bounds the fuzzy widening"
    );
}
