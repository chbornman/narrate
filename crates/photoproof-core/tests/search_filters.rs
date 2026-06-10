//! P3.1 filter semantics (RETRIEVAL §4/§5.1): hard WHERE constraints,
//! applied per joined target; rating reads the `image_ratings` fold (E4/B4);
//! has-strokes reads `image_journal_stats`; folder/root/volume scope through
//! `paths`/`roots`/`volumes`; date chips split captured vs annotated.

use std::path::PathBuf;

use photoproof_core::id::{ContentHash, UtcMillis};
use photoproof_core::search::{
    Comparison, DateField, DateRange, EventKind, EventSource, Filter, PathMatch, ProjectRef,
    Provenance, SearchError, SearchResults, Searcher, StringMatch, VolumeFilter,
};
use photoproof_core::store::{
    EventDraft, EventStore, RemarkSource, RetractionSource, SessionContext,
};
use photoproof_core::{Event, SessionId, StrokePayload, StrokePoint, Tool};
use rusqlite::params;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

    fn remark(&self, text: &str, targets: &[&ContentHash]) -> Event {
        self.store
            .append(
                &self.session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: text.into(),
                    targets: targets.iter().map(|h| (*h).clone()).collect(),
                },
                None,
            )
            .unwrap()
    }

    fn voice_remark(&self, text: &str, target: &ContentHash) -> Event {
        self.store
            .append(
                &self.session,
                EventDraft::Remark {
                    source: RemarkSource::Voice {
                        conf_pm: 900,
                        dur_ms: 800,
                        linked_event: None,
                    },
                    text: text.into(),
                    targets: vec![target.clone()],
                },
                None,
            )
            .unwrap()
    }

    fn rate(&self, value: u8, target: &ContentHash) -> Event {
        self.store
            .append(
                &self.session,
                EventDraft::Rating {
                    value,
                    targets: vec![target.clone()],
                },
                None,
            )
            .unwrap()
    }

    fn stroke(&self, target: &ContentHash) -> Event {
        self.store
            .append(
                &self.session,
                EventDraft::Stroke {
                    payload: StrokePayload {
                        base_w: 40,
                        orientation: 1,
                        points: vec![StrokePoint {
                            x: 0,
                            y: 0,
                            p: 1000,
                            t: 0,
                        }],
                        tool: Tool::Pencil,
                    },
                    target: target.clone(),
                    linked_event: None,
                },
                None,
            )
            .unwrap()
    }

    fn retract(&self, target: &Event) {
        self.store
            .append(
                &self.session,
                EventDraft::Retraction {
                    target: target.id.clone(),
                    source: RetractionSource::System,
                },
                None,
            )
            .unwrap();
    }

    fn image_row(
        &self,
        hash: &ContentHash,
        capture_ts: Option<&str>,
        camera: Option<&str>,
        lens: Option<&str>,
    ) {
        self.conn()
            .execute(
                "INSERT INTO images (image_hash, byte_size, format, capture_ts, camera_model, \
                 lens_model, first_ingested_at) VALUES (?1, 1000, 'jpeg', ?2, ?3, ?4, ?5)",
                params![
                    hash.as_str(),
                    capture_ts,
                    camera,
                    lens,
                    UtcMillis::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    fn volume_row(&self, volume_id: &str, label: &str, state: &str) {
        self.conn()
            .execute(
                "INSERT INTO volumes (volume_id, label, state, first_seen_at, last_seen_at) \
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![volume_id, label, state, UtcMillis::now().to_rfc3339()],
            )
            .unwrap();
    }

    fn root_row(&self, root_id: &str, volume_id: &str, rel_path: &str, name: &str, state: &str) {
        self.conn()
            .execute(
                "INSERT INTO roots (root_id, volume_id, rel_path, display_name, state, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    root_id,
                    volume_id,
                    rel_path,
                    name,
                    state,
                    UtcMillis::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    fn path_row(
        &self,
        path_id: &str,
        hash: &ContentHash,
        volume_id: &str,
        root_id: Option<&str>,
        rel_path: &str,
        state: &str,
    ) {
        self.conn()
            .execute(
                "INSERT INTO paths (path_id, image_hash, volume_id, root_id, rel_path, size, \
                 mtime_ns, state, stale_reason, first_seen_at, last_verified_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1000, 0, ?6, ?7, ?8, ?8)",
                params![
                    path_id,
                    hash.as_str(),
                    volume_id,
                    root_id,
                    rel_path,
                    state,
                    if state == "stale" {
                        Some("moved")
                    } else {
                        None
                    },
                    UtcMillis::now().to_rfc3339()
                ],
            )
            .unwrap();
    }
}

fn hashes(r: &SearchResults) -> Vec<&str> {
    let mut v: Vec<&str> = r.images.iter().map(|i| i.image_hash.as_str()).collect();
    v.sort();
    v
}

fn sorted(mut v: Vec<&ContentHash>) -> Vec<&str> {
    v.sort();
    v.into_iter().map(|h| h.as_str()).collect()
}

// ---------------------------------------------------------------------------
// Per-target filtering (§4: an event targeting N images can survive for one
// target and be filtered for another — §13.7)
// ---------------------------------------------------------------------------

#[test]
fn image_scoped_filters_apply_per_joined_target() {
    let fx = fixture();
    let (leica, fuji) = (h("img-leica"), h("img-fuji"));
    fx.image_row(&leica, None, Some("Leica M11"), Some("Summilux 35"));
    fx.image_row(&fuji, None, Some("X100V"), Some("23mm f/2"));
    // ONE event targeting both images.
    fx.remark("fog at the barn for both frames", &[&leica, &fuji]);

    let searcher = fx.searcher();
    let all = searcher.search("fog ba", &[]).unwrap();
    assert_eq!(hashes(&all), sorted(vec![&leica, &fuji]));

    let only_leica = searcher
        .search(
            "fog ba",
            &[Filter::Camera(StringMatch::Exact("leica m11".into()))],
        )
        .unwrap();
    assert_eq!(hashes(&only_leica), vec![leica.as_str()]);

    let only_fuji = searcher
        .search(
            "fog ba",
            &[Filter::Lens(StringMatch::Contains("23MM".into()))],
        )
        .unwrap();
    assert_eq!(hashes(&only_fuji), vec![fuji.as_str()]);

    // An image with no `images` row at all fails image-metadata filters
    // (hard constraints) without suppressing the event's other targets.
    let ghost = h("img-ghost");
    fx.remark("fog rolling over the bay together", &[&ghost, &fuji]);
    let r = searcher
        .search(
            "fog",
            &[Filter::Camera(StringMatch::Contains("X100".into()))],
        )
        .unwrap();
    assert_eq!(hashes(&r), vec![fuji.as_str()]);
}

// ---------------------------------------------------------------------------
// Rating chip: the fold, value:0 vs unrated (E4/B4)
// ---------------------------------------------------------------------------

#[test]
fn rating_chip_explicit_zero_is_not_unrated() {
    let fx = fixture();
    let (zeroed, rated, unrated) = (h("img-zero"), h("img-three"), h("img-unrated"));
    for img in [&zeroed, &rated, &unrated] {
        fx.remark("fog study frame", &[img]);
    }
    fx.rate(0, &zeroed);
    fx.rate(3, &rated);

    let searcher = fx.searcher();
    let eq0 = searcher
        .search("fog", &[Filter::Rating(Comparison::Eq(0))])
        .unwrap();
    assert_eq!(hashes(&eq0), vec![zeroed.as_str()]);

    // Gte(0) means "has a rating ≥ 0" — explicit ratings only; an unrated
    // image has no `image_ratings` row and matches no rating filter.
    let gte0 = searcher
        .search("fog", &[Filter::Rating(Comparison::Gte(0))])
        .unwrap();
    assert_eq!(hashes(&gte0), sorted(vec![&zeroed, &rated]));

    let lte2 = searcher
        .search("fog", &[Filter::Rating(Comparison::Lte(2))])
        .unwrap();
    assert_eq!(hashes(&lte2), vec![zeroed.as_str()]);

    let between = searcher
        .search("fog", &[Filter::Rating(Comparison::Between(4, 2))]) // bounds normalize
        .unwrap();
    assert_eq!(hashes(&between), vec![rated.as_str()]);
}

#[test]
fn rating_chip_follows_the_fold() {
    let fx = fixture();
    let img = h("img-refold");
    fx.remark("fog over the jetty", &[&img]);
    fx.rate(4, &img);
    let downgrade = fx.rate(1, &img);

    let searcher = fx.searcher();
    let chip = [Filter::Rating(Comparison::Gte(3))];
    // Last live rating wins: 1.
    assert!(searcher.search("fog", &chip).unwrap().images.is_empty());
    // Retract the downgrade: the fold returns to 4.
    fx.retract(&downgrade);
    assert_eq!(searcher.search("fog", &chip).unwrap().images.len(), 1);
}

// ---------------------------------------------------------------------------
// Has-strokes chip on the FTS path (stats fold, B4 liveness)
// ---------------------------------------------------------------------------

#[test]
fn has_strokes_chip_reads_stats_fold() {
    let fx = fixture();
    let (inked, plain) = (h("img-ink"), h("img-noink"));
    fx.remark("fog over the inked frame", &[&inked]);
    fx.remark("fog over the plain frame", &[&plain]);
    let stroke = fx.stroke(&inked);

    let searcher = fx.searcher();
    let with = searcher.search("fog", &[Filter::HasStrokes(true)]).unwrap();
    assert_eq!(hashes(&with), vec![inked.as_str()]);
    let without = searcher
        .search("fog", &[Filter::HasStrokes(false)])
        .unwrap();
    assert_eq!(hashes(&without), vec![plain.as_str()]);

    // Redacting the stroke scrubs it: B4 — has_strokes counts non-retracted
    // AND non-scrubbed strokes only.
    fx.store.redact(&stroke.id).unwrap();
    let with = searcher.search("fog", &[Filter::HasStrokes(true)]).unwrap();
    assert!(with.images.is_empty());
    let without = searcher
        .search("fog", &[Filter::HasStrokes(false)])
        .unwrap();
    assert_eq!(hashes(&without), sorted(vec![&inked, &plain]));
}

// ---------------------------------------------------------------------------
// Folder / root scoping through active paths
// ---------------------------------------------------------------------------

#[test]
fn folder_subtree_and_name_scoping() {
    let fx = fixture();
    fx.volume_row("VOL1", "Archive", "online");
    fx.root_row("ROOT1", "VOL1", "photos", "Photos 2026", "active");

    let (iceland, icelandic, misc, stale) = (
        h("img-iceland"),
        h("img-icelandic"),
        h("img-misc"),
        h("img-stale"),
    );
    for (i, (img, rel)) in [
        (&iceland, "photos/2026/iceland/quiet/IMG_0001.jpg"),
        (&icelandic, "photos/2026/icelandic-light/IMG_0002.jpg"),
        (&misc, "photos/misc/IMG_iceland.jpg"), // folder name does NOT match
        (&stale, "photos/2026/iceland/IMG_0003.jpg"),
    ]
    .iter()
    .enumerate()
    {
        fx.image_row(img, None, None, None);
        fx.path_row(
            &format!("P{i}"),
            img,
            "VOL1",
            Some("ROOT1"),
            rel,
            if *img == &stale { "stale" } else { "active" },
        );
        fx.remark("fog frame in some folder", &[img]);
    }

    let searcher = fx.searcher();

    // Subtree: prefix is path-segment exact — `iceland` ≠ `icelandic-light`;
    // stale paths never scope.
    let r = searcher
        .search(
            "fog",
            &[Filter::Folder(PathMatch::Subtree(
                "photos/2026/iceland".into(),
            ))],
        )
        .unwrap();
    assert_eq!(hashes(&r), vec![iceland.as_str()]);

    // NameContains: matches folder names anywhere on the directory path —
    // never the filename.
    let r = searcher
        .search(
            "fog",
            &[Filter::Folder(PathMatch::NameContains("iceland".into()))],
        )
        .unwrap();
    assert_eq!(hashes(&r), sorted(vec![&iceland, &icelandic]));

    // Root chip: watched-root display name, active paths only.
    let r = searcher
        .search("fog", &[Filter::Root("photos 2026".into())])
        .unwrap();
    assert_eq!(hashes(&r), sorted(vec![&iceland, &icelandic, &misc]));
    let r = searcher
        .search("fog", &[Filter::Root("Other Root".into())])
        .unwrap();
    assert!(r.images.is_empty());
}

// ---------------------------------------------------------------------------
// Date chips: captured vs annotated (§5.1 DateField)
// ---------------------------------------------------------------------------

#[test]
fn captured_vs_annotated_date_chips() {
    let fx = fixture();
    let (march, may, undated) = (h("img-march"), h("img-may"), h("img-undated"));
    fx.image_row(&march, Some("2026-03-10T08:00:00Z"), None, None);
    fx.image_row(&may, Some("2026-05-20T08:00:00Z"), None, None);
    fx.image_row(&undated, None, None, None);
    for img in [&march, &may, &undated] {
        fx.remark("fog drifting in", &[img]); // annotated now (June 2026+)
    }

    let searcher = fx.searcher();
    let ts = |s: &str| UtcMillis::parse(s).unwrap();
    let captured_march = Filter::Date {
        field: DateField::Captured,
        range: DateRange::Absolute {
            start: Some(ts("2026-03-01T00:00:00.000Z")),
            end: Some(ts("2026-04-01T00:00:00.000Z")),
        },
    };

    // FTS path: capture window picks by EXIF date; undated never matches.
    let r = searcher
        .search("fog", std::slice::from_ref(&captured_march))
        .unwrap();
    assert_eq!(hashes(&r), vec![march.as_str()]);

    // Browse path: same chip, same scope.
    let r = searcher
        .search("", std::slice::from_ref(&captured_march))
        .unwrap();
    assert_eq!(hashes(&r), vec![march.as_str()]);

    // Annotated: all three were annotated at append time (now), none in
    // March.
    let annotated_march = Filter::Date {
        field: DateField::Annotated,
        range: DateRange::Absolute {
            start: Some(ts("2026-03-01T00:00:00.000Z")),
            end: Some(ts("2026-04-01T00:00:00.000Z")),
        },
    };
    let r = searcher
        .search("fog", std::slice::from_ref(&annotated_march))
        .unwrap();
    assert!(r.images.is_empty());
    let open_ended = Filter::Date {
        field: DateField::Annotated,
        range: DateRange::Absolute {
            start: Some(ts("2026-01-01T00:00:00.000Z")),
            end: None,
        },
    };
    let r = searcher
        .search("fog", std::slice::from_ref(&open_ended))
        .unwrap();
    assert_eq!(r.images.len(), 3);
    // Browse path resolves annotated dates against live events.
    let r = searcher
        .search("", std::slice::from_ref(&open_ended))
        .unwrap();
    assert_eq!(r.images.len(), 3);
    let r = searcher
        .search("", std::slice::from_ref(&annotated_march))
        .unwrap();
    assert!(r.images.is_empty());
}

// ---------------------------------------------------------------------------
// Source chip: event-scoped, applies to the matched event
// ---------------------------------------------------------------------------

#[test]
fn source_chip_filters_matched_events() {
    let fx = fixture();
    let img = h("img-sources");
    fx.image_row(&img, None, None, None); // browse iterates `images`
    fx.remark("fog note typed on the keyboard", &[&img]);
    let voiced = fx.voice_remark("fog note spoken aloud", &img);

    let searcher = fx.searcher();
    // Voice-only: the image surfaces through its voice event, and the quote
    // is the voice event's text even though a typed event also matches.
    let r = searcher
        .search("fog", &[Filter::Source(vec![EventSource::Voice])])
        .unwrap();
    assert_eq!(hashes(&r), vec![img.as_str()]);
    match &r.images[0].provenance {
        Provenance::Quote(q) => {
            assert_eq!(q.event_id, voiced.id);
            assert_eq!(q.source, EventSource::Voice);
        }
        other => panic!("expected Quote, got {other:?}"),
    }

    // Pencil never produces remark roots: hard constraint admits nothing.
    let r = searcher
        .search("fog", &[Filter::Source(vec![EventSource::Pencil])])
        .unwrap();
    assert!(r.images.is_empty());

    // Empty source set is an empty hard constraint.
    let r = searcher.search("fog", &[Filter::Source(vec![])]).unwrap();
    assert!(r.images.is_empty());

    // Browse path: source chips scope by the image's live events.
    let r = searcher
        .search("", &[Filter::Source(vec![EventSource::Voice])])
        .unwrap();
    assert_eq!(hashes(&r), vec![img.as_str()]);
    fx.retract(&voiced);
    let r = searcher
        .search("", &[Filter::Source(vec![EventSource::Voice])])
        .unwrap();
    assert!(r.images.is_empty());
}

// ---------------------------------------------------------------------------
// Volume chips: online / offline / named
// ---------------------------------------------------------------------------

#[test]
fn volume_chips_follow_path_availability() {
    let fx = fixture();
    fx.volume_row("VOLON", "FastSSD", "online");
    fx.volume_row("VOLOFF", "ColdArchive", "offline");
    let (online, offline, both) = (h("img-on"), h("img-off"), h("img-both"));
    for img in [&online, &offline, &both] {
        fx.image_row(img, None, None, None);
        fx.remark("fog framed for the volume test", &[img]);
    }
    fx.path_row("PV1", &online, "VOLON", None, "a/on.jpg", "active");
    fx.path_row("PV2", &offline, "VOLOFF", None, "b/off.jpg", "active");
    fx.path_row("PV3", &both, "VOLON", None, "a/both.jpg", "active");
    fx.path_row("PV4", &both, "VOLOFF", None, "b/both.jpg", "active");

    let searcher = fx.searcher();
    let r = searcher
        .search("fog", &[Filter::Volume(VolumeFilter::Online)])
        .unwrap();
    assert_eq!(hashes(&r), sorted(vec![&online, &both]));
    // Offline = not reachable now (LIBRARY §8 availability).
    let r = searcher
        .search("fog", &[Filter::Volume(VolumeFilter::Offline)])
        .unwrap();
    assert_eq!(hashes(&r), vec![offline.as_str()]);
    let r = searcher
        .search(
            "fog",
            &[Filter::Volume(VolumeFilter::Named("coldarchive".into()))],
        )
        .unwrap();
    assert_eq!(hashes(&r), sorted(vec![&offline, &both]));
}

// ---------------------------------------------------------------------------
// Combined chips stay conjunctive; M3-only filters error loudly
// ---------------------------------------------------------------------------

#[test]
fn chips_combine_conjunctively() {
    let fx = fixture();
    let (a, b) = (h("img-and-a"), h("img-and-b"));
    fx.image_row(&a, Some("2026-03-10T08:00:00Z"), Some("X100V"), None);
    fx.image_row(&b, Some("2026-03-12T08:00:00Z"), Some("Leica M11"), None);
    fx.remark("fog on both mornings", &[&a, &b]);
    fx.rate(5, &a);
    fx.rate(5, &b);

    let searcher = fx.searcher();
    let r = searcher
        .search(
            "fog",
            &[
                Filter::Rating(Comparison::Gte(4)),
                Filter::Camera(StringMatch::Contains("x100".into())),
            ],
        )
        .unwrap();
    assert_eq!(hashes(&r), vec![a.as_str()]);
}

#[test]
fn m3_only_filters_error_rather_than_silently_drop() {
    let fx = fixture();
    let searcher = fx.searcher();
    let err = searcher
        .search(
            "fog",
            &[Filter::Project(ProjectRef {
                raw: "Quiet Hours".into(),
                resolved: None,
            })],
        )
        .unwrap_err();
    assert!(matches!(err, SearchError::UnsupportedFilter("project")));
    let err = searcher
        .search("fog", &[Filter::Kind(vec![EventKind::Remark])])
        .unwrap_err();
    assert!(matches!(err, SearchError::UnsupportedFilter("kind")));
}
