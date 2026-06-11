//! P3.1 acceptance: the §7.2 worked example end-to-end, the M1 result
//! contract, provenance (Quote spans/highlights, Stroke, FilterOnly),
//! session hits (R4), and fold/retraction/redaction visibility.
//!
//! Contract: spec/RETRIEVAL.md §4, §5.4, §6, §7.2, §13.2/5/6/7.

use std::path::PathBuf;

use photoproof_core::id::{ContentHash, UtcMillis};
use photoproof_core::search::{
    Comparison, DateField, DateRange, EventSource, Filter, Provenance, SearchResults, Searcher,
    SignalId, fts_match_query, render_with_sentinels,
};
use photoproof_core::store::{EventDraft, EventStore, RemarkSource, SessionContext};
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
            device_id: "ab".repeat(16),
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

    fn rate(&self, value: u8, target: &ContentHash) {
        self.store
            .append(
                &self.session,
                EventDraft::Rating {
                    value,
                    targets: vec![target.clone()],
                },
                None,
            )
            .unwrap();
    }

    fn stroke(&self, target: &ContentHash, linked: Option<&Event>) -> Event {
        self.store
            .append(
                &self.session,
                EventDraft::Stroke {
                    payload: StrokePayload {
                        base_w: 40,
                        orientation: 1,
                        points: vec![StrokePoint {
                            x: 100,
                            y: 100,
                            p: 1000,
                            t: 0,
                        }],
                        tool: Tool::Pencil,
                    },
                    target: target.clone(),
                    linked_event: linked.map(|e| e.id.clone()),
                },
                None,
            )
            .unwrap()
    }

    fn searcher(&self) -> Searcher {
        Searcher::open(&self.path).unwrap()
    }

    /// Insert a library `images` row (search filters target LIBRARY §6
    /// columns; the library packet owns ingest, tests seed rows directly).
    fn image_row(&self, hash: &ContentHash, capture_ts: Option<&str>, camera: Option<&str>) {
        let conn = rusqlite::Connection::open(&self.path).unwrap();
        conn.execute(
            "INSERT INTO images (image_hash, byte_size, format, capture_ts, camera_model, \
             first_ingested_at) VALUES (?1, 1000, 'jpeg', ?2, ?3, ?4)",
            params![
                hash.as_str(),
                capture_ts,
                camera,
                UtcMillis::now().to_rfc3339()
            ],
        )
        .unwrap();
    }
}

fn image_hashes(r: &SearchResults) -> Vec<&str> {
    r.images.iter().map(|i| i.image_hash.as_str()).collect()
}

fn quote_of(r: &SearchResults, idx: usize) -> &photoproof_core::search::Quote {
    match &r.images[idx].provenance {
        Provenance::Quote(q) => q,
        other => panic!("expected Quote provenance, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §7.2 — the normative M1 walkthrough
// ---------------------------------------------------------------------------

#[test]
fn worked_example_7_2_fog_ba_with_rating_chip() {
    let fx = fixture();
    let (x, y, z) = (h("image-X"), h("image-Y"), h("image-Z"));

    // The two §7.2 events: one single-target, one multi-target.
    let ev_x = fx.remark("the fog swallowing the barn, keep this one", &[&x]);
    let ev_yz = fx.remark("fog bank ate the whole ridge", &[&y, &z]);

    // Ratings: Y has rating 2 → filtered out; the same event still
    // surfaces Z (rating 4).
    fx.rate(5, &x);
    fx.rate(2, &y);
    fx.rate(4, &z);

    // Background corpus that does not match the query, so the two events'
    // bm25 scores carry a meaningful (positive-IDF) signal.
    for i in 0..10 {
        let filler = h(&format!("filler-{i}"));
        fx.remark(
            &format!("quiet harbor evening study number {i} with long light and slow water"),
            &[&filler],
        );
    }

    // Query construction (§7.2: prefix index `prefix='2 3'` serves "ba").
    assert_eq!(
        fts_match_query("fog ba").as_deref(),
        Some("\"fog\" \"ba\"*")
    );

    let searcher = fx.searcher();
    let results = searcher
        .search_with(
            "fog ba",
            &[Filter::Rating(Comparison::Gte(3))],
            &photoproof_core::search::SearchOptions {
                now: None,
                include_debug: true,
            },
        )
        .unwrap();

    // Grouping: by image, each carrying its best (lowest) bm25; Y filtered
    // out per-target while the same event still surfaces Z (§13.7).
    //
    // FLAGGED spec deviation (see the packet report): §7.2 displays X
    // (−7.2) ranked above Z (−5.9). With the spec's exact texts — equal
    // term frequencies, X's document longer (8 tokens vs 6) — real FTS5
    // bm25 *cannot* rank X first under any corpus (FTS5 clamps
    // non-positive IDF, and BM25 length normalization then always favors
    // the shorter document; measured −3.25 for X vs −3.54 for Z on this
    // corpus). The engine's true ordering is asserted; every other §7.2
    // artifact is reproduced exactly.
    assert_eq!(image_hashes(&results), vec![z.as_str(), x.as_str()]);
    assert!(
        results.images[0].score < results.images[1].score,
        "first image must carry the better (lower) bm25: {} vs {}",
        results.images[0].score,
        results.images[1].score
    );

    // X's provenance: Quote with highlight ranges from the snippet
    // sentinels — the §7.2 artifact, exactly.
    let qx = quote_of(&results, 1);
    assert_eq!(qx.event_id, ev_x.id);
    assert_eq!(qx.session_id, fx.session);
    assert_eq!(qx.ts, ev_x.ts);
    assert_eq!(qx.source, EventSource::Typed);
    assert_eq!(qx.text, "the fog swallowing the barn, keep this one");
    assert_eq!(
        render_with_sentinels(&qx.text, &qx.highlights),
        "the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one"
    );
    assert_eq!(qx.char_start, 0);
    assert_eq!(qx.char_end, qx.text.chars().count() as u32);
    assert_eq!(qx.linked_stroke, None);

    // Z's provenance quotes the multi-target event.
    let qz = quote_of(&results, 0);
    assert_eq!(qz.event_id, ev_yz.id);
    assert_eq!(
        render_with_sentinels(&qz.text, &qz.highlights),
        "⟦fog⟧ ⟦ba⟧nk ate the whole ridge"
    );

    // No session-level remarks in play.
    assert!(results.session_hits.is_empty());

    // Echo: raw string + the M1 ParsedQuery (keywords = FTS remainder).
    assert_eq!(results.query.raw, "fog ba");
    assert_eq!(results.query.parsed.keywords.as_deref(), Some("fog ba"));
    assert_eq!(results.query.parsed.semantic, None);
    assert!(!results.query.parsed.visual);
    assert!(!results.query.parsed.fallback);
    assert!(results.query.parsed.dropped.is_empty());
    assert_eq!(results.query.parsed.filters.len(), 1);

    // Debug scores (dev builds): S2 ranks in final order.
    let dbg = results.images[0].debug.as_ref().unwrap();
    assert_eq!(dbg.per_signal.len(), 1);
    assert_eq!(dbg.per_signal[0].0, SignalId::S2EventFts);
    assert_eq!(dbg.per_signal[0].1, Some(1));
    assert_eq!(
        results.images[1].debug.as_ref().unwrap().per_signal[0].1,
        Some(2)
    );

    // last_annotated_ts flows from image_journal_stats.
    assert!(results.images[0].last_annotated_ts.is_some());

    // Without the debug option the field stays empty (release contract).
    let plain = searcher
        .search("fog ba", &[Filter::Rating(Comparison::Gte(3))])
        .unwrap();
    assert!(plain.images.iter().all(|i| i.debug.is_none()));
}

// ---------------------------------------------------------------------------
// Session hits (R4): a separate list, never attributed to images
// ---------------------------------------------------------------------------

#[test]
fn session_level_hits_listed_separately() {
    let fx = fixture();
    let img = h("img-1");
    fx.remark("fog on the ridge", &[&img]);
    let session_remark = fx.remark("fog session note, no image selected", &[]);

    let searcher = fx.searcher();
    let results = searcher.search("fog", &[]).unwrap();
    assert_eq!(image_hashes(&results), vec![img.as_str()]);
    assert_eq!(results.session_hits.len(), 1);
    let sh = &results.session_hits[0];
    assert_eq!(sh.session_id, fx.session);
    assert_eq!(sh.quote.event_id, session_remark.id);
    assert!(sh.quote.text.contains("session note"));
    assert!(!sh.quote.highlights.is_empty());

    // An image-scoped chip can never hold for a zero-target event: the
    // session list empties rather than mis-attributing (R4).
    fx.rate(4, &img);
    let results = searcher
        .search("fog", &[Filter::Rating(Comparison::Gte(3))])
        .unwrap();
    assert_eq!(image_hashes(&results), vec![img.as_str()]);
    assert!(results.session_hits.is_empty());

    // Event-scoped chips still apply to session hits.
    let results = searcher
        .search("fog", &[Filter::Source(vec![EventSource::Typed])])
        .unwrap();
    assert_eq!(results.session_hits.len(), 1);
    let results = searcher
        .search("fog", &[Filter::Source(vec![EventSource::Voice])])
        .unwrap();
    assert!(results.session_hits.is_empty());
    assert!(results.images.is_empty());
}

// ---------------------------------------------------------------------------
// Folded text: revisions, retraction, redaction (§1.1, §13.5)
// ---------------------------------------------------------------------------

#[test]
fn quotes_come_from_folded_text() {
    let fx = fixture();
    let img = h("img-rev");
    let root = fx.remark("the foggy barn sketch original wording", &[&img]);
    fx.store
        .append(
            &fx.session,
            EventDraft::Revision {
                target: root.id.clone(),
                text: "fog drifting past the barn at dawn".into(),
            },
            None,
        )
        .unwrap();

    let searcher = fx.searcher();
    let results = searcher.search("fog ba", &[]).unwrap();
    assert_eq!(results.images.len(), 1);
    let q = quote_of(&results, 0);
    // The quote is the revised (effective) text, attributed to the root.
    assert_eq!(q.event_id, root.id);
    assert_eq!(q.text, "fog drifting past the barn at dawn");
    assert_eq!(q.char_end as usize, q.text.chars().count());

    // The original wording is out of the index.
    assert!(
        searcher
            .search("original wording", &[])
            .unwrap()
            .images
            .is_empty()
    );
    assert!(searcher.search("foggy", &[]).unwrap().images.is_empty());
}

#[test]
fn retracted_and_redacted_text_absent_immediately() {
    let fx = fixture();
    let (a, b) = (h("img-a"), h("img-b"));
    let ra = fx.remark("fog over the west barn", &[&a]);
    let rb = fx.remark("fog over the east barn", &[&b]);

    let searcher = fx.searcher();
    assert_eq!(searcher.search("fog", &[]).unwrap().images.len(), 2);

    fx.store
        .append(
            &fx.session,
            EventDraft::Retraction {
                target: ra.id.clone(),
                source: photoproof_core::store::RetractionSource::System,
            },
            None,
        )
        .unwrap();
    let results = searcher.search("fog", &[]).unwrap();
    assert_eq!(image_hashes(&results), vec![b.as_str()]);

    fx.store.redact(&rb.id).unwrap();
    assert!(searcher.search("fog", &[]).unwrap().images.is_empty());
}

// ---------------------------------------------------------------------------
// Unicode: char (Unicode-scalar) spans and highlight offsets (§1.2)
// ---------------------------------------------------------------------------

#[test]
fn unicode_spans_and_highlights() {
    let fx = fixture();
    let img = h("img-uni");
    // Multi-byte text before and inside the match window; long enough that
    // the 12-token snippet window truncates with ellipses on both sides.
    let text = "Sebastião était là tôt ce matin gris — 霧の朝 ☔ very quiet mood out on the \
                pier while gulls drifted past and the ropes creaked against wet wood — then \
                the melancholic barn appeared through fog while we waited for the ferry and \
                the light kept fading slowly over the cold water";
    fx.remark(text, &[&img]);

    let searcher = fx.searcher();
    let results = searcher.search("melanch", &[]).unwrap();
    assert_eq!(results.images.len(), 1);
    let q = quote_of(&results, 0);

    // The window is an exact char-span of the folded text.
    let by_chars: String = text
        .chars()
        .skip(q.char_start as usize)
        .take((q.char_end - q.char_start) as usize)
        .collect();
    assert_eq!(by_chars, q.text);
    assert!(q.char_start > 0, "window must be truncated from the left");

    // The highlight covers exactly the typed prefix of "melancholic".
    assert_eq!(q.highlights.len(), 1);
    let (hs, he) = q.highlights[0];
    let hl: String = q
        .text
        .chars()
        .skip(hs as usize)
        .take((he - hs) as usize)
        .collect();
    assert_eq!(hl, "melanch");

    // Diacritic folding (`remove_diacritics 2`): plain-ascii query matches;
    // the highlight conservatively keeps the whole folded token.
    let results = searcher.search("sebastiao", &[]).unwrap();
    assert_eq!(results.images.len(), 1);
    let q = quote_of(&results, 0);
    let (hs, he) = q.highlights[0];
    let hl: String = q
        .text
        .chars()
        .skip(hs as usize)
        .take((he - hs) as usize)
        .collect();
    assert_eq!(hl, "Sebastião");
}

// ---------------------------------------------------------------------------
// Filter-only browse: capture date descending, FilterOnly / Stroke (§4, §5.4)
// ---------------------------------------------------------------------------

#[test]
fn filter_only_browse_orders_by_capture_date_descending() {
    let fx = fixture();
    let (a, b, c) = (h("img-a"), h("img-b"), h("img-c"));
    fx.image_row(&a, Some("2026-03-05T10:00:00Z"), None);
    fx.image_row(&b, Some("2026-04-01T10:00:00Z"), None);
    fx.image_row(&c, None, None); // undated: sorts last
    fx.rate(4, &a);
    fx.rate(5, &b);
    fx.rate(3, &c);

    let searcher = fx.searcher();
    let results = searcher
        .search("", &[Filter::Rating(Comparison::Gte(3))])
        .unwrap();
    assert_eq!(
        image_hashes(&results),
        vec![b.as_str(), a.as_str(), c.as_str()]
    );
    for img in &results.images {
        assert_eq!(img.provenance, Provenance::FilterOnly);
        assert_eq!(img.score, 0.0);
        assert!(img.debug.is_none());
    }
    assert!(results.session_hits.is_empty());
    assert_eq!(results.query.parsed.keywords, None);

    // Rating chip excludes below-threshold images per the fold.
    let results = searcher
        .search("", &[Filter::Rating(Comparison::Gte(5))])
        .unwrap();
    assert_eq!(image_hashes(&results), vec![b.as_str()]);

    // Empty query + zero filters = browse everything.
    let results = searcher.search("  ", &[]).unwrap();
    assert_eq!(results.images.len(), 3);
}

#[test]
fn has_strokes_browse_carries_stroke_provenance() {
    let fx = fixture();
    let (inked, plain) = (h("img-inked"), h("img-plain"));
    fx.image_row(&inked, Some("2026-01-01T00:00:00Z"), None);
    fx.image_row(&plain, Some("2026-01-02T00:00:00Z"), None);
    fx.remark("a note so the plain image has journal presence", &[&plain]);
    let s1 = fx.stroke(&inked, None);
    let s2 = fx.stroke(&inked, None);

    let searcher = fx.searcher();
    let results = searcher.search("", &[Filter::HasStrokes(true)]).unwrap();
    assert_eq!(image_hashes(&results), vec![inked.as_str()]);
    match &results.images[0].provenance {
        Provenance::Stroke {
            event_id,
            session_id,
            ts,
        } => {
            // The most recent live stroke is referenced.
            assert_eq!(event_id, &s2.id);
            assert_eq!(session_id, &fx.session);
            assert_eq!(*ts, s2.ts);
        }
        other => panic!("expected Stroke provenance, got {other:?}"),
    }

    let results = searcher.search("", &[Filter::HasStrokes(false)]).unwrap();
    assert_eq!(image_hashes(&results), vec![plain.as_str()]);
    assert_eq!(results.images[0].provenance, Provenance::FilterOnly);

    // Retract both strokes: the stats fold flips and the chip follows.
    for s in [&s1, &s2] {
        fx.store
            .append(
                &fx.session,
                EventDraft::Retraction {
                    target: s.id.clone(),
                    source: photoproof_core::store::RetractionSource::System,
                },
                None,
            )
            .unwrap();
    }
    let results = searcher.search("", &[Filter::HasStrokes(true)]).unwrap();
    assert!(results.images.is_empty());
}

// ---------------------------------------------------------------------------
// Quote.linked_stroke (X2: both link directions)
// ---------------------------------------------------------------------------

#[test]
fn quote_resolves_linked_stroke_in_both_directions() {
    let fx = fixture();

    // Backward link carried by the later voice remark (circle, then speak).
    let img1 = h("img-link-1");
    let stroke1 = fx.stroke(&img1, None);
    fx.store
        .append(
            &fx.session,
            EventDraft::Remark {
                source: RemarkSource::Voice {
                    conf_pm: Some(950),
                    dur_ms: 1200,
                    linked_event: Some(stroke1.id.clone()),
                },
                text: "fog pooling exactly here".into(),
                targets: vec![img1.clone()],
            },
            None,
        )
        .unwrap();

    // Backward link carried by the later stroke (speak, then circle).
    let img2 = h("img-link-2");
    let remark2 = fx.remark("fog hugging the waterline", &[&img2]);
    let stroke2 = fx.stroke(&img2, Some(&remark2));

    let searcher = fx.searcher();
    let results = searcher.search("fog", &[]).unwrap();
    let by_hash = |hash: &ContentHash| {
        results
            .images
            .iter()
            .find(|i| i.image_hash == *hash)
            .unwrap()
    };
    match &by_hash(&img1).provenance {
        Provenance::Quote(q) => {
            assert_eq!(q.source, EventSource::Voice);
            assert_eq!(q.linked_stroke.as_ref(), Some(&stroke1.id));
        }
        other => panic!("expected Quote, got {other:?}"),
    }
    match &by_hash(&img2).provenance {
        Provenance::Quote(q) => assert_eq!(q.linked_stroke.as_ref(), Some(&stroke2.id)),
        other => panic!("expected Quote, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Annotated-date chip applies to the matched event (FTS path)
// ---------------------------------------------------------------------------

#[test]
fn annotated_date_filters_matched_events() {
    let fx = fixture();
    let img = h("img-dated");
    let ev = fx.remark("fog in the annotated window", &[&img]);

    let searcher = fx.searcher();
    let day = 86_400_000;
    let around = |offset_days: i64, span_days: i64| Filter::Date {
        field: DateField::Annotated,
        range: DateRange::Absolute {
            start: Some(UtcMillis::from_epoch_ms(
                ev.ts.epoch_ms() + offset_days * day,
            )),
            end: Some(UtcMillis::from_epoch_ms(
                ev.ts.epoch_ms() + (offset_days + span_days) * day,
            )),
        },
    };
    // Window containing the event's ts.
    let results = searcher.search("fog", &[around(-1, 2)]).unwrap();
    assert_eq!(results.images.len(), 1);
    // Window strictly after it.
    let results = searcher.search("fog", &[around(1, 2)]).unwrap();
    assert!(results.images.is_empty());
}
