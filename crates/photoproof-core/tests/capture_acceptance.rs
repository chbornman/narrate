//! spec/CAPTURE.md §13 acceptance criteria, mock-verified (packet P6.1).
//!
//! Naming carries the spec ids (BUILD-LOOP traceability): `c13_1_*` …
//! `c13_9_*`. Everything runs against the P1.2 scripted mocks
//! (MockTranscriber / MockVad) and the capture Clock seam — no real time,
//! no real audio, no sleeps.

mod common;

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::stream::{BoxStream, Stream};
use photoproof_connectors::ConnectorResult;
use photoproof_connectors::mock::{
    MockTranscriber, MockVad, ScriptEntry, ScriptedEvent, SpeechSpan,
};
use photoproof_connectors::transcriber::{
    AudioFrame, SegmentKind, StreamMs, Transcriber, TranscriptSegment,
};
use photoproof_core::capture::{
    Activity, CaptureEngine, Clock, CloseProcessing, FakeClock, MicState, NoCapture, NoFlush,
    SessionEngine, recover_crashed_sessions,
};
use photoproof_core::search::Searcher;
use photoproof_core::{
    ContentHash, EventDraft, EventStore, Kind, RemarkSource, SessionId, Source, StrokePayload,
    StrokePoint, Tool, UtcMillis,
};

use common::{DEVICE_ID, hash};

const SR: u32 = 16_000;
const WALL0: i64 = 1_780_000_000_000; // June 2026-ish epoch ms

fn ctx() -> photoproof_core::SessionContext {
    photoproof_core::SessionContext {
        app_version: "0.1.0-test".into(),
        device_id: DEVICE_ID.into(),
        root_context: None,
    }
}

fn open_store(dir: &tempfile::TempDir) -> (EventStore, SessionId) {
    let store = EventStore::open(dir.path().join("journal.db")).expect("open store");
    let session = store.open_session(ctx()).expect("open session");
    (store, session)
}

fn final_seg(utterance_id: u64, text: &str, onset: StreamMs, end: StreamMs) -> TranscriptSegment {
    TranscriptSegment {
        utterance_id,
        kind: SegmentKind::Final,
        text: text.into(),
        onset,
        end,
        confidence: Some(0.91),
        language: None,
    }
}

fn partial_seg(utterance_id: u64, text: &str, onset: StreamMs, now: StreamMs) -> TranscriptSegment {
    TranscriptSegment {
        utterance_id,
        kind: SegmentKind::Partial,
        text: text.into(),
        onset,
        end: now,
        confidence: None,
        language: None,
    }
}

/// A 50 ms frame of the given constant sample at stream position `at`.
fn frame(at: StreamMs, sample: f32) -> AudioFrame {
    AudioFrame {
        samples: vec![sample; (SR as u64 * 50 / 1000) as usize],
        captured_at: at,
    }
}

/// Drive the engine with contiguous 50 ms frames from stream `from` to
/// `until`, applying scope changes at scripted capture-clock instants. The
/// clock and the stream advance together; the caller keeps the capture
/// clock aligned with `from` (the first-ever push anchors the stream).
fn drive_span(
    engine: &mut CaptureEngine<'_, FakeClock>,
    clock: &FakeClock,
    store: &EventStore,
    from: StreamMs,
    until: StreamMs,
    scope_changes: &[(u64, Vec<ContentHash>)],
) -> Vec<photoproof_core::Event> {
    let mut committed = Vec::new();
    let mut t = from;
    while t < until {
        for (at, targets) in scope_changes {
            if *at == t {
                engine.set_scope(targets.clone());
            }
        }
        committed.extend(engine.push_audio(store, frame(t, 0.0)));
        clock.advance(50);
        t += 50;
    }
    committed
}

/// [`drive_span`] from stream 0 (first push anchors at mono 0).
fn drive(
    engine: &mut CaptureEngine<'_, FakeClock>,
    clock: &FakeClock,
    store: &EventStore,
    until: StreamMs,
    scope_changes: &[(u64, Vec<ContentHash>)],
) -> Vec<photoproof_core::Event> {
    drive_span(engine, clock, store, 0, until, scope_changes)
}

// ---------------------------------------------------------------------------
// §13.1 — binding under rapid selection change (the B1 script)
// ---------------------------------------------------------------------------

/// Segment 1 onset at T binds A despite the selection moving A→B at
/// T+800 ms and finalization arriving at T+2000 ms; segment 2 onset at
/// T+1100 ms binds B — regardless of finalization times. T = 1000.
#[test]
fn c13_1_binding_b1_script_segments_bind_their_own_onsets() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 1000,
                end: 2000,
            },
            SpeechSpan {
                onset: 2100,
                end: 4100,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 3000, // T+2000: finalizes long after the selection moved on
            event: ScriptedEvent::Segment(final_seg(1, "lovely gesture here", 1000, 1950)),
        },
        ScriptEntry {
            at: 4000,
            event: ScriptedEvent::Segment(final_seg(
                2,
                "but this one has better light",
                2100,
                3500,
            )),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(
        &mut engine,
        &clock,
        &store,
        4200,
        &[(1800, vec![hash(0xB)])], // A→B at T+800
    );

    assert_eq!(committed.len(), 2, "two finals → two events, never merged");
    let seg1 = &committed[0];
    let seg2 = &committed[1];
    assert_eq!(seg1.text.as_deref(), Some("lovely gesture here"));
    assert_eq!(seg1.targets, vec![hash(0xA)], "onset T was under scope A");
    assert_eq!(seg2.text.as_deref(), Some("but this one has better light"));
    assert_eq!(
        seg2.targets,
        vec![hash(0xB)],
        "onset T+1100 was under scope B"
    );
    assert_eq!(seg1.source, Source::Voice);
    // The B1 monologue legitimately split across two images (§5.3).
    assert_ne!(seg1.targets, seg2.targets);
}

/// §5.2: N = 0 — a selection change 50 ms before an onset binds the NEW
/// scope; no grace window.
#[test]
fn c13_1_no_grace_change_50ms_before_onset_binds_the_new_scope() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 1100,
            end: 2600,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 2500,
        event: ScriptedEvent::Segment(final_seg(1, "this belongs to the new scope", 1100, 1900)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    engine.arm();
    let committed = drive(
        &mut engine,
        &clock,
        &store,
        2700,
        &[(1050, vec![hash(0xB)])], // 50 ms before the onset
    );
    assert_eq!(committed.len(), 1);
    assert_eq!(
        committed[0].targets,
        vec![hash(0xB)],
        "speech 50 ms after a selection change binds the NEW scope (no grace)"
    );
}

/// §6.2/§5.1: `SpeechStart`'s timestamp is the *estimated onset*, NOT
/// detection time (silero detection lags true onset by up to 300 ms). With
/// 200 ms detection latency and a scope change A→B landing INSIDE the
/// detection-latency window — after the true onset, before the VAD reports
/// it — binding must be A (the §1 budget's hardest B1 case), and the
/// event's ts is the onset wall time, not the detection wall time.
#[test]
fn c13_1_binding_uses_estimated_onset_not_detection_time() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 1000,
            end: 2600,
        }],
    )
    .with_detection_delay(200);
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 2600,
        event: ScriptedEvent::Segment(final_seg(1, "still about the first image", 1000, 2500)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    engine.arm();
    // True onset 1000, detected at 1200; the selection moves at 1100.
    let committed = drive(
        &mut engine,
        &clock,
        &store,
        2700,
        &[(1100, vec![hash(0xB)])],
    );
    assert_eq!(committed.len(), 1);
    assert_eq!(
        committed[0].targets,
        vec![hash(0xA)],
        "binding = scope_at(estimated onset), never scope at detection time"
    );
    assert_eq!(
        committed[0].ts,
        UtcMillis::from_epoch_ms(WALL0 + 1000),
        "ts = wall clock at the estimated onset, not at detection"
    );
}

/// §5.1 cross-check: a late ASR token timestamp disagreeing with the VAD
/// onset across a scope change is logged to the debug panel and NEVER
/// silently rebinds.
#[test]
fn c13_1_token_time_disagreement_logs_and_never_rebinds() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 1000,
            end: 3050,
        }],
    );
    // The Final's own onset (the ASR-side timestamp) is 900 ms late —
    // past the scope change at 1800 — while the VAD onset said 1000.
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 3000,
        event: ScriptedEvent::Segment(final_seg(1, "still about the first image", 1900, 2000)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    engine.arm();
    let committed = drive(
        &mut engine,
        &clock,
        &store,
        3100,
        &[(1800, vec![hash(0xB)])],
    );
    assert_eq!(committed.len(), 1);
    assert_eq!(
        committed[0].targets,
        vec![hash(0xA)],
        "the VAD onset is authoritative; token times never rebind"
    );
    assert!(
        engine
            .debug_notes()
            .iter()
            .any(|n| n.contains("token-time cross-check")),
        "the disagreement is logged to the debug panel"
    );
}

/// A session-scope snapshot yields a session-level event with zero targets.
#[test]
fn c13_1_session_scope_onset_mints_a_zero_target_event() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 500,
            end: 2100,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 2000,
        event: ScriptedEvent::Segment(final_seg(1, "thinking out loud", 500, 1400)),
    }]);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session.clone());
    engine.arm(); // scope never set: session
    let committed = drive(&mut engine, &clock, &store, 2200, &[]);
    assert_eq!(committed.len(), 1);
    assert!(committed[0].targets.is_empty());
    assert_eq!(store.sessionlevel_events(&session).unwrap().len(), 1);
}

/// Empty/whitespace finals mint nothing; consecutive finals never merge.
#[test]
fn c13_1_empty_finals_mint_nothing_and_finals_never_merge() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 200,
                end: 700,
            },
            SpeechSpan {
                onset: 900,
                end: 1400,
            },
            SpeechSpan {
                onset: 1600,
                end: 2500,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 1000,
            event: ScriptedEvent::Segment(final_seg(1, "first thought", 200, 650)),
        },
        ScriptEntry {
            at: 1700,
            event: ScriptedEvent::Segment(final_seg(2, "   \n\t ", 900, 1300)),
        },
        ScriptEntry {
            at: 2400,
            event: ScriptedEvent::Segment(final_seg(3, "second thought", 1600, 2050)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(1)]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 2600, &[]);
    let texts: Vec<_> = committed
        .iter()
        .map(|e| e.text.as_deref().unwrap())
        .collect();
    assert_eq!(
        texts,
        vec!["first thought", "second thought"],
        "whitespace-only finals mint nothing; one final = one event"
    );
}

// ---------------------------------------------------------------------------
// §13.2 — no audio on disk
// ---------------------------------------------------------------------------

fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A full armed mock session — speech, finals, disarm, session close —
/// leaves ZERO audio bytes anywhere under the app universe (database, WAL,
/// temp dir), asserted by a filesystem snapshot diff plus a byte-scan for
/// the distinctive sample pattern that was pushed as audio. On disarm the
/// stream is CLOSED (not paused) and the ring buffer zeroes.
#[test]
fn c13_2_full_armed_session_leaves_zero_audio_bytes_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let before = walk_files(dir.path());

    // A distinctive, compressible-resistant audio fingerprint: any of these
    // f32 little-endian byte patterns appearing in ANY file = audio leaked.
    let fingerprint: Vec<f32> = vec![0.123_456_79, -0.654_321, 0.111_111_1, -0.999_999_9];
    let mut fingerprint_bytes = Vec::new();
    for s in &fingerprint {
        fingerprint_bytes.extend_from_slice(&s.to_le_bytes());
    }

    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 1550,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 1500,
        event: ScriptedEvent::Segment(final_seg(
            1,
            "the words survive; the audio does not",
            100,
            950,
        )),
    }]);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session.clone());
    engine.set_scope(vec![hash(7)]);
    engine.arm();
    let mut committed = Vec::new();
    let mut t = 0;
    while t < 1600 {
        let mut f = frame(t, 0.0);
        // Stamp the fingerprint into every frame's samples.
        f.samples[..fingerprint.len()].copy_from_slice(&fingerprint);
        committed.extend(engine.push_audio(&store, f));
        clock.advance(50);
        t += 50;
    }
    assert_eq!(committed.len(), 1, "the utterance minted");
    committed.extend(engine.disarm(&store));
    assert!(
        !engine.stream_open(),
        "disarm CLOSES the stream, never pauses"
    );
    assert!(engine.audio_is_zeroed(), "the ring buffer zeroes on disarm");
    store.close_session(&session, clock.wall()).unwrap();

    // Filesystem snapshot diff: only the database family may have changed.
    let after = walk_files(dir.path());
    for path in &after {
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("journal.db"),
            "unexpected file appeared: {path:?}"
        );
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !contains_subslice(&bytes, &fingerprint_bytes),
            "audio fingerprint found in {path:?}"
        );
    }
    assert!(after.len() >= before.len());
    // The transcript IS durable — only the audio is gone.
    let e = store.raw_event(&committed[0].id).unwrap().unwrap();
    assert_eq!(
        e.text.as_deref(),
        Some("the words survive; the audio does not")
    );
}

// ---------------------------------------------------------------------------
// §13.5 — correction folding visible in search
// ---------------------------------------------------------------------------

/// Voice remark "muddy light" corrected to "moody light": FTS "moody"
/// returns the image with the corrected quote as provenance; "muddy"
/// returns nothing; the journal fold shows corrected text with the
/// original still in the log.
#[test]
fn c13_5_correction_folds_into_fts_and_journal() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 2550,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 2500,
        event: ScriptedEvent::Segment(final_seg(
            1,
            "keep this one it has that muddy light I keep chasing",
            100,
            1900,
        )),
    }]);
    let image = hash(0x11);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session.clone());
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 2600, &[]);
    assert_eq!(committed.len(), 1);
    let voice = &committed[0];

    // §12.1: the correction mints kind=revision, source=typed, referencing
    // the ORIGINAL event id, carrying the FULL corrected text.
    store
        .append(
            &session,
            EventDraft::Revision {
                target: voice.id.clone(),
                text: "keep this one it has that moody light I keep chasing".into(),
            },
            None,
        )
        .unwrap();

    let searcher = Searcher::open(dir.path().join("journal.db")).unwrap();
    let moody = searcher.search("moody", &[]).unwrap();
    assert_eq!(moody.images.len(), 1);
    assert_eq!(moody.images[0].image_hash, image);
    match &moody.images[0].provenance {
        photoproof_core::search::Provenance::Quote(q) => {
            assert!(
                q.text.contains("moody light"),
                "corrected quote is provenance"
            );
            assert_eq!(q.event_id, voice.id, "provenance keys the chain root");
        }
        other => panic!("expected a quote provenance, got {other:?}"),
    }
    let muddy = searcher.search("muddy", &[]).unwrap();
    assert!(muddy.images.is_empty(), "the original text left the index");
    assert!(muddy.session_hits.is_empty());

    // The fold shows the corrected text; the original stays in the log.
    let folded = store.folded_journal(&image).unwrap();
    match &folded[0] {
        photoproof_core::JournalEntry::Remark {
            text, corrected, ..
        } => {
            assert!(text.contains("moody light"));
            assert!(corrected);
        }
        other => panic!("expected a remark, got {other:?}"),
    }
    let raw = store.raw_event(&voice.id).unwrap().unwrap();
    assert!(
        raw.text.unwrap().contains("muddy light"),
        "history is preserved"
    );

    // §12.1: a revision of the revision still references the ORIGINAL and
    // the chain folds to the latest by append order.
    store
        .append(
            &session,
            EventDraft::Revision {
                target: voice.id.clone(),
                text: "keep this one it has that brooding light I keep chasing".into(),
            },
            None,
        )
        .unwrap();
    let brooding = searcher.search("brooding", &[]).unwrap();
    assert_eq!(brooding.images.len(), 1);
    assert!(searcher.search("moody", &[]).unwrap().images.is_empty());
}

// ---------------------------------------------------------------------------
// §13.6 — session boundaries
// ---------------------------------------------------------------------------

#[test]
fn c13_6_29_min_gap_same_session_31_min_gap_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(WALL0);
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let first = sessions.id().clone();

    // 29-minute gap: same session.
    clock.advance(29 * 60 * 1000);
    sessions
        .on_activity(&store, &mut NoCapture, &mut NoFlush)
        .unwrap();
    assert_eq!(sessions.id(), &first);
    let last_activity_wall = sessions.last_activity_wall();

    // 31-minute gap: the NEXT activity opens a new session; the old one's
    // ended_at = its last activity time (never `now`).
    clock.advance(31 * 60 * 1000);
    sessions
        .on_activity(&store, &mut NoCapture, &mut NoFlush)
        .unwrap();
    let second = sessions.id().clone();
    assert_ne!(second, first);
    let rec = store.session(&first).unwrap().unwrap();
    assert_eq!(rec.ended_ts, Some(last_activity_wall));

    // The first event after the boundary carries the NEW session id.
    let e = store
        .append(
            &second,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "fresh session".into(),
                targets: vec![],
            },
            None,
        )
        .unwrap();
    assert_eq!(e.session_id, second);
}

#[test]
fn c13_6_kill_mid_session_recovery_enqueues_close_processing_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("journal.db");
    let dead;
    let last_ts;
    {
        // Process one dies with the session open.
        let store = EventStore::open(&db).unwrap();
        dead = store.open_session(ctx()).unwrap();
        let e = store
            .append(
                &dead,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "interrupted".into(),
                    targets: vec![],
                },
                None,
            )
            .unwrap();
        last_ts = e.ts;
    }
    // Next launch recovers BEFORE opening a new session.
    let store = EventStore::open(&db).unwrap();
    let recovered = recover_crashed_sessions(&store).unwrap();
    assert_eq!(recovered, vec![dead.clone()]);
    let rec = store.session(&dead).unwrap().unwrap();
    assert_eq!(rec.ended_ts, Some(last_ts), "ended_at = last event ts");
    assert_eq!(
        store.session_close_state(&dead).unwrap(),
        Some((false, false)),
        "closed_clean = false; close processing enqueued"
    );
    // Enqueued ONCE — idempotent across repeated recovery passes.
    assert!(recover_crashed_sessions(&store).unwrap().is_empty());
    assert_eq!(
        store.close_processing_pending().unwrap(),
        vec![dead.clone()]
    );
    // Recovery minted no events.
    assert_eq!(store.sessionlevel_events(&dead).unwrap().len(), 1);
    // The (empty) close-processing framework drains the queue exactly once.
    let mut processing = CloseProcessing::new();
    assert_eq!(processing.run_pending(&store).unwrap(), vec![dead]);
    assert!(store.close_processing_pending().unwrap().is_empty());
}

/// §2.1/§2.2: VAD speech activity refreshes the idle timer through the
/// CaptureDrain seam. The user last touched the UI at mono 0, then talks
/// (armed) from minute 29 through minute 31: a UI click at minute 30.5 —
/// mid-utterance, more than 30 minutes past the last UI activity — does
/// NOT rotate, the in-flight utterance is never bisected, and its final
/// mints into the SAME session.
#[test]
fn c13_6_speech_is_activity_no_boundary_mid_utterance() {
    const MIN: u64 = 60_000;
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(WALL0);
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let first = sessions.id().clone();

    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 29 * MIN,
            end: 31 * MIN,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 30 * MIN,
            event: ScriptedEvent::Segment(partial_seg(1, "this whole set", 29 * MIN, 30 * MIN)),
        },
        ScriptEntry {
            at: 31 * MIN,
            event: ScriptedEvent::Segment(final_seg(
                1,
                "this whole set is stronger than I remembered",
                29 * MIN,
                31 * MIN - 100,
            )),
        },
    ]);
    let image = hash(0x31);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), first.clone());
    engine.set_scope(vec![image.clone()]);
    engine.arm(); // mono 0, alongside the last UI activity

    // 29 minutes of UI idle (armed, silent), then a two-minute monologue
    // spanning the would-be 30-minute boundary.
    clock.advance(29 * MIN);
    let mut committed = drive_span(
        &mut engine,
        &clock,
        &store,
        29 * MIN,
        30 * MIN + 30_000,
        &[],
    );
    assert!(committed.is_empty(), "no final yet");
    assert_eq!(engine.streaming_count(), 1, "utterance in flight");

    // UI click at minute 30.5 — past the boundary on UI activity alone.
    assert_eq!(
        sessions
            .on_activity(&store, &mut engine, &mut NoFlush)
            .unwrap(),
        Activity::Same,
        "speech is activity (§2.1): no rotation across a speech-filled window"
    );
    assert_eq!(sessions.id(), &first);
    assert_eq!(
        engine.streaming_count(),
        1,
        "the in-flight utterance was not bisected (§2.2)"
    );
    assert_eq!(engine.abandoned_count(), 0);

    // The utterance finalizes into the SAME session.
    committed.extend(drive_span(
        &mut engine,
        &clock,
        &store,
        30 * MIN + 30_000,
        31 * MIN + 100,
        &[],
    ));
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].session_id, first);
    assert_eq!(committed[0].targets, vec![image]);
    assert_eq!(
        engine.abandoned_count(),
        0,
        "never bisected, never abandoned"
    );
}

/// The converse gate, plus §2.2 rotation follow-through: an armed but
/// SILENT mic is not activity — 31 idle minutes still rotate at the next
/// UI activity — the close drain disarms the mic, ended_at stays at the
/// LAST activity, and the engine follows the rotation through the seam:
/// the next commit mints into the NEW session with no manual set_session.
#[test]
fn c13_6_armed_silence_still_rotates_and_the_engine_follows_the_rotation() {
    const MIN: u64 = 60_000;
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(WALL0);
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let first = sessions.id().clone();

    let vad = MockVad::new(SR, vec![]); // never speaks
    let transcriber = MockTranscriber::new("mock-asr", SR);
    let image = hash(0x32);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), first.clone());
    engine.set_scope(vec![image.clone()]);
    engine.arm(); // mono 0 — the arm itself is the last activity

    // The capture stream keeps delivering frames; silence is NOT activity.
    drive_span(&mut engine, &clock, &store, 0, 500, &[]);
    clock.advance(31 * MIN);
    let Activity::Rotated { closed, opened } = sessions
        .on_activity(&store, &mut engine, &mut NoFlush)
        .unwrap()
    else {
        panic!("an armed-but-silent mic must not suppress the idle boundary");
    };
    assert_eq!(closed, first);
    assert_eq!(engine.mic(), MicState::Disarmed, "close drain disarmed");
    assert!(!engine.stream_open());
    assert!(engine.audio_is_zeroed());
    assert_eq!(
        store.session(&closed).unwrap().unwrap().ended_ts,
        Some(UtcMillis::from_epoch_ms(WALL0)),
        "ended_at = last activity (the arm at mono 0), never `now`"
    );
    // §2.2 follow-through: the engine was told about the rotation through
    // the seam — the stroke mints into the NEW session automatically.
    let stroke = engine
        .commit_stroke(&store, image, stroke_payload_ending_at(100))
        .unwrap();
    assert_eq!(stroke.session_id, opened);
    assert_eq!(sessions.id(), &opened);
}

/// §2.1: the mic toggle itself — arm AND disarm — is activity through the
/// seam; on UI touches alone each of these windows would have rotated.
#[test]
fn c13_6_mic_arm_and_disarm_are_activity() {
    const MIN: u64 = 60_000;
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(WALL0);
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let first = sessions.id().clone();
    let vad = MockVad::new(SR, vec![]);
    let transcriber = MockTranscriber::new("mock-asr", SR);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), first.clone());

    clock.advance(20 * MIN);
    engine.arm(); // minute 20
    clock.advance(25 * MIN); // minute 45: 45 past the last UI touch
    assert_eq!(
        sessions
            .on_activity(&store, &mut engine, &mut NoFlush)
            .unwrap(),
        Activity::Same,
        "mic ARM refreshed the idle timer"
    );
    clock.advance(20 * MIN); // minute 65
    engine.disarm(&store);
    clock.advance(25 * MIN); // minute 90: 45 past UI, 25 past the disarm
    assert_eq!(
        sessions
            .on_activity(&store, &mut engine, &mut NoFlush)
            .unwrap(),
        Activity::Same,
        "mic DISARM refreshed the idle timer"
    );
    assert_eq!(sessions.id(), &first);
    // No capture activity since: a true 31-minute gap rotates as ever.
    clock.advance(31 * MIN);
    assert!(matches!(
        sessions
            .on_activity(&store, &mut engine, &mut NoFlush)
            .unwrap(),
        Activity::Rotated { .. }
    ));
}

// ---------------------------------------------------------------------------
// §13.7 — ASR death
// ---------------------------------------------------------------------------

/// Kill the ASR mid-utterance: nothing minted from the partial, the mic
/// auto-disarms to Disarmed(error), the indicator degrades quietly, the
/// audio ring zeroes — and a typed note submitted immediately after lands
/// normally.
#[test]
fn c13_7_asr_death_mid_utterance_abandons_and_degrades_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 200,
            end: 2000,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 600,
            event: ScriptedEvent::Segment(partial_seg(1, "the hand is the", 200, 600)),
        },
        ScriptEntry {
            at: 900,
            event: ScriptedEvent::ConnectionLost,
        },
    ]);
    let image = hash(3);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session.clone());
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 1000, &[]);

    assert!(committed.is_empty(), "no event minted from the partial");
    assert_eq!(engine.mic(), MicState::DisarmedError, "mic auto-disarms");
    assert_eq!(engine.abandoned_count(), 1);
    assert!(
        engine.audio_is_zeroed(),
        "ring buffer zeroed on fatal error"
    );
    assert!(!engine.stream_open(), "stream torn down, not paused");
    let ind = engine.indicator();
    assert!(
        ind.degraded.asr_unavailable,
        "quiet degraded indicator state"
    );
    assert_eq!(ind.mic, MicState::DisarmedError);
    assert!(ind.streaming_utterance.is_none());
    assert!(store.events_for_image(&image).unwrap().is_empty());

    // A typed note submitted immediately after lands normally (§13.7) —
    // the typed path has zero model dependencies.
    let note = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "typed notes still work".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .unwrap();
    assert_eq!(note.kind, Kind::Remark);
    assert_eq!(store.events_for_image(&image).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// §6.4/§2.5 — disarm drain: trailing finals mint, then abandonment
// ---------------------------------------------------------------------------

/// Trailing finals after disarm mint NORMALLY (their onsets predate the
/// disarm).
#[test]
fn disarm_accepts_trailing_finals_which_mint_normally() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    // The final is scripted at a stream position the audio never reaches —
    // it arrives only as a trailing flush after end_stream().
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 99_000,
        event: ScriptedEvent::Segment(final_seg(1, "trailing final", 100, 850)),
    }]);
    let image = hash(9);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let mut committed = drive(&mut engine, &clock, &store, 1000, &[]);
    assert!(committed.is_empty(), "not finalized while armed");
    committed.extend(engine.disarm(&store));
    assert_eq!(committed.len(), 1, "the trailing final minted normally");
    assert_eq!(committed[0].targets, vec![image]);
    assert!(!engine.stream_open());
    assert!(engine.audio_is_zeroed());
    assert_eq!(engine.mic(), MicState::Disarmed);
}

/// A Transcriber that accepts the stream and then never yields: the drain
/// window cap (5 s on the capture clock) abandons in-flight utterances.
struct StuckTranscriber;

struct PendingForever;

impl Stream for PendingForever {
    type Item = ConnectorResult<TranscriptSegment>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Transcriber for StuckTranscriber {
    fn stream<'a>(
        &'a self,
        _audio: BoxStream<'a, AudioFrame>,
    ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
        Ok(Box::pin(PendingForever))
    }
    fn sample_rate(&self) -> u32 {
        SR
    }
    fn model_id(&self) -> &str {
        "stuck"
    }
}

#[test]
fn disarm_drain_caps_at_5_s_then_abandons() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 2550,
        }],
    );
    let transcriber = StuckTranscriber;
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(1)]);
    engine.arm();
    drive(&mut engine, &clock, &store, 500, &[]);
    assert_eq!(engine.streaming_count(), 1);
    engine.disarm(&store);
    assert!(engine.stream_open(), "drain window holds the stream open");
    clock.advance(4_999);
    engine.pump(&store);
    assert!(engine.stream_open(), "still within the 5 s window");
    clock.advance(1);
    engine.pump(&store);
    assert!(!engine.stream_open(), "5 s cap: stream closed");
    assert_eq!(engine.abandoned_count(), 1, "in-flight utterance abandoned");
    assert!(engine.audio_is_zeroed());
}

/// §2.5 step 1 through the session engine: the capture drain mints
/// trailing finals into the CLOSING session.
#[test]
fn session_close_drains_capture_into_the_closing_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(WALL0);
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let closing = sessions.id().clone();
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 99_000,
        event: ScriptedEvent::Segment(final_seg(1, "spoken right before quit", 100, 850)),
    }]);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), closing.clone());
    engine.set_scope(vec![hash(5)]);
    engine.arm();
    drive(&mut engine, &clock, &store, 1000, &[]);
    sessions
        .close_current(&store, &mut engine, &mut NoFlush)
        .unwrap();
    let events = store.events_for_image(&hash(5)).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].session_id, closing,
        "the trailing final minted into the CLOSING session"
    );
    assert!(!engine.stream_open());
    assert!(engine.audio_is_zeroed());
    assert!(store.session(&closing).unwrap().unwrap().ended_ts.is_some());
}

// ---------------------------------------------------------------------------
// §13.8 — stroke ↔ utterance linking (cases a–d)
// ---------------------------------------------------------------------------

fn stroke_payload_ending_at(t_last: u32) -> StrokePayload {
    StrokePayload {
        base_w: 40,
        orientation: 1,
        points: vec![
            StrokePoint {
                x: 100,
                y: 100,
                p: 1000,
                t: 0,
            },
            StrokePoint {
                x: 400,
                y: 400,
                p: 1000,
                t: t_last,
            },
        ],
        tool: Tool::Pencil,
    }
}

/// (a) Stroke inside an utterance's VAD span whose final lands after
/// pen-up → the stroke commits UNLINKED (in-flight suppression) and the
/// utterance carries `linked_event` → stroke when it commits.
#[test]
fn c13_8a_stroke_inside_streaming_span_utterance_carries_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x21);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 500,
            end: 4100,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 4000, // finalizes after pen-up
        event: ScriptedEvent::Segment(final_seg(1, "circle that hand", 500, 2900)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    drive(&mut engine, &clock, &store, 2000, &[]); // speech streaming, no final yet
    assert_eq!(engine.streaming_count(), 1);
    // Pen-up at mono 2000; the stroke began 800 ms earlier.
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(800))
        .unwrap();
    assert_eq!(stroke.linked_event, None, "in-flight suppression: unlinked");
    // The final lands; the utterance (later-committed) carries the link.
    let committed = drive(&mut engine, &clock, &store, 4200, &[]);
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].linked_event, Some(stroke.id.clone()));
    // Folds traverse the one-way pointer in BOTH directions: the stroke's
    // folded entry resolves the incoming link.
    let folded = store.folded_journal(&image).unwrap();
    let stroke_entry = folded
        .iter()
        .find_map(|e| match e {
            photoproof_core::JournalEntry::Stroke {
                id, linked_event, ..
            } if *id == stroke.id => Some(linked_event.clone()),
            _ => None,
        })
        .expect("stroke folds");
    assert_eq!(stroke_entry, Some(committed[0].id.clone()));
}

/// §9.2's reason for existing (the scenario that PINS the gate): at pen-up
/// an EARLIER committed utterance on the SAME image sits within the 10 s
/// nearest-fallback window while the stroke's TRUE partner is still in the
/// air. The suppression gate keeps the stroke unlinked — without it the
/// stroke grabs the committed utterance as a wrong, permanent backward
/// link — and the in-flight utterance carries the link when IT commits.
#[test]
fn c13_8a_suppression_blocks_wrong_nearest_fallback_while_true_partner_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x26);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 200,
                end: 1200,
            },
            SpeechSpan {
                onset: 5000,
                end: 9100,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 1200,
            event: ScriptedEvent::Segment(final_seg(1, "fix the horizon", 200, 1150)),
        },
        ScriptEntry {
            at: 9100, // finalizes after pen-up
            event: ScriptedEvent::Segment(final_seg(2, "circle that hand", 5000, 9000)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let first = drive(&mut engine, &clock, &store, 6000, &[]);
    assert_eq!(first.len(), 1, "the earlier utterance is committed");
    assert_eq!(
        engine.streaming_count(),
        1,
        "the true partner is in the air"
    );
    // Pen-up at mono 6000 (stroke spans 5500..6000): the COMMITTED
    // utterance (span 200..1150, same image) is ~4.4 s away — an eligible
    // nearest-fallback partner if the gate were gone.
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(500))
        .unwrap();
    assert_eq!(
        stroke.linked_event, None,
        "in-flight suppression (§9.2): the stroke must NOT link backward to \
         the earlier committed utterance while its true partner streams"
    );
    // The in-flight utterance commits and carries the link to the stroke.
    let second = drive_span(&mut engine, &clock, &store, 6000, 9300, &[]);
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].linked_event,
        Some(stroke.id.clone()),
        "the true partner carries the link at ITS commit"
    );
    assert_ne!(first[0].id, second[0].id);
}

/// (b) Stroke 4 s after an utterance ends, same image → the stroke
/// carries the backward link.
#[test]
fn c13_8b_stroke_4s_after_same_image_links_backward() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x22);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 200,
            end: 1000,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 950, // due while speech audio still flows (the mock's clock is consumed audio)
        event: ScriptedEvent::Segment(final_seg(1, "needs a tighter crop", 200, 950)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 1100, &[]);
    assert_eq!(committed.len(), 1);
    // The utterance span ended by mono 1000; pen-up at 5000 with a 1 s
    // stroke → span gap ≈ 4 s: within 10 s, same image.
    clock.advance(5_000 - clock.mono_ms());
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(1000))
        .unwrap();
    assert_eq!(stroke.linked_event, Some(committed[0].id.clone()));
}

/// (c) 11 s after → unlinked, permanently.
#[test]
fn c13_8c_stroke_11s_after_is_unlinked_forever() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x23);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 200,
            end: 1000,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 950,
        event: ScriptedEvent::Segment(final_seg(1, "shelve it", 200, 950)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 1100, &[]);
    assert_eq!(committed.len(), 1);
    // Speech ended by mono 1000; the stroke spans 12000..13000 → gap 11 s.
    clock.advance(13_000 - clock.mono_ms());
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(1000))
        .unwrap();
    assert_eq!(stroke.linked_event, None);
}

/// (d) Stroke near a session-scoped utterance, no overlap → unlinked
/// (the scope gate: session-level utterances never link via fallback).
#[test]
fn c13_8d_session_scoped_utterance_never_links_via_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x24);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 200,
            end: 1000,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 950,
        event: ScriptedEvent::Segment(final_seg(1, "general musing about the shoot", 200, 950)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    // Session scope (no selection) for the utterance.
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 1100, &[]);
    assert_eq!(committed.len(), 1);
    assert!(committed[0].targets.is_empty());
    // Then the user opens an image and draws 3 s later: near, no overlap.
    engine.set_scope(vec![image.clone()]);
    clock.advance(4_000 - clock.mono_ms());
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(500))
        .unwrap();
    assert_eq!(stroke.linked_event, None, "scope gate holds");
}

/// §9.1: among several committed candidates, the greatest overlap duration
/// wins; resolution runs once, at commit, and is never re-run.
#[test]
fn c13_8_greatest_overlap_wins_among_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0x25);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 200,
                end: 1200,
            },
            SpeechSpan {
                onset: 1400,
                end: 4600,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        ScriptEntry {
            at: 1500,
            event: ScriptedEvent::Segment(final_seg(1, "short remark", 200, 1150)),
        },
        ScriptEntry {
            at: 4300,
            event: ScriptedEvent::Segment(final_seg(2, "long detailed remark", 1400, 4300)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    engine.arm();
    let committed = drive(&mut engine, &clock, &store, 4700, &[]);
    assert_eq!(committed.len(), 2);
    // Stroke spanning 1000..4600: overlaps remark 1 by 200 ms (1000..1200)
    // and remark 2 by 3000 ms (1400..4400) → remark 2 wins.
    let stroke = engine
        .commit_stroke(&store, image, stroke_payload_ending_at(3600))
        .unwrap();
    assert_eq!(stroke.linked_event, Some(committed[1].id.clone()));
}

// ---------------------------------------------------------------------------
// §13.9 — multi-select rating (trace: ONE event, N ordered targets, dedupe)
// ---------------------------------------------------------------------------

/// Select 5 images, press 3 → one rating event with 5 ordered targets
/// (`event_targets.position` = selection order); merge of duplicate copies
/// (the N-sidecar rebuild input) dedupes to one event. Sidecar mirroring
/// itself is P2.1's §13 round-trip + I11.
#[test]
fn c13_9_multi_select_rating_one_event_five_ordered_targets_dedupes() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let selection: Vec<ContentHash> = vec![hash(5), hash(2), hash(9), hash(1), hash(7)];
    let e = store
        .append(
            &session,
            EventDraft::Rating {
                value: 3,
                targets: selection.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(e.targets, selection, "selection order preserved");
    // One event row; position = selection order, for all five images.
    for (pos, h) in selection.iter().enumerate() {
        let events = store.events_for_image(h).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, e.id);
        assert_eq!(events[0].targets[pos], *h);
    }
    // Rebuild-style dedupe: merging the same event five times (one copy
    // per sidecar) yields exactly one row.
    let copies = vec![e.clone(); 5];
    let report = store.merge(&copies).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.duplicates, 5);
    assert_eq!(store.events_for_image(&hash(5)).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// §11 — the indicator contract under streaming
// ---------------------------------------------------------------------------

/// §5.4: while an utterance is in flight the indicator shows the scope it
/// is BOUND to even when the live selection has changed; it reverts when
/// the segment finalizes. No text ever rides the indicator.
#[test]
fn indicator_streams_the_bound_scope_until_finalization() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 500,
            end: 3050,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 3000,
        event: ScriptedEvent::Segment(final_seg(1, "about the first one", 500, 2400)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    engine.arm();
    drive(&mut engine, &clock, &store, 1000, &[]);
    // Selection moves on while the utterance streams.
    engine.set_scope(vec![hash(0xB)]);
    let ind = engine.indicator();
    assert_eq!(ind.mic, MicState::ArmedSpeaking);
    assert_eq!(ind.current_scope.preview_hashes, vec![hash(0xB)]);
    let streaming = ind.streaming_utterance.expect("utterance in flight");
    assert_eq!(
        streaming.bound_scope.preview_hashes,
        vec![hash(0xA)],
        "the words in flight land on the snapshot at onset (§5.4)"
    );
    // Finalization reverts the indicator to the live scope alone.
    let committed = drive(&mut engine, &clock, &store, 3150, &[]);
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].targets, vec![hash(0xA)]);
    let ind = engine.indicator();
    assert_eq!(ind.mic, MicState::ArmedIdle);
    assert!(ind.streaming_utterance.is_none());
    assert!(!ind.degraded.asr_unavailable);
}

/// Arming against a not-ready ASR fails QUIETLY into Disarmed(error) +
/// degraded — and re-arming once it is ready recovers (§6.6).
#[test]
fn arming_not_ready_degrades_quietly_and_rearming_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let not_ready = MockTranscriber::new("mock-asr", SR).not_ready();
    let vad = MockVad::new(SR, vec![]);
    let mut engine = CaptureEngine::new(clock.clone(), &not_ready, Box::new(vad), session.clone());
    assert_eq!(engine.arm(), MicState::DisarmedError);
    assert!(engine.indicator().degraded.asr_unavailable);
    assert!(!engine.stream_open());
    // Typed path unaffected — degraded mode is exactly the journal product.
    store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "still journaling".into(),
                targets: vec![],
            },
            None,
        )
        .unwrap();
    drop(engine);

    let ready = MockTranscriber::new("mock-asr", SR);
    let vad = MockVad::new(SR, vec![]);
    let mut engine = CaptureEngine::new(clock.clone(), &ready, Box::new(vad), session);
    assert_eq!(engine.arm(), MicState::ArmedIdle);
    assert!(!engine.indicator().degraded.asr_unavailable);
}

/// EVENTS §1.2: the utterance's id and ts are fixed at VAD onset even
/// though the row lands at finalization — ULID order reflects capture
/// order when a typed note is committed mid-utterance.
#[test]
fn utterance_id_minted_at_onset_orders_before_a_mid_utterance_typed_note() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 200,
            end: 3050,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 3000,
        event: ScriptedEvent::Segment(final_seg(1, "slow to finalize", 200, 2100)),
    }]);
    let mut engine =
        CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session.clone());
    engine.set_scope(vec![hash(1)]);
    engine.arm();
    drive(&mut engine, &clock, &store, 1000, &[]);
    // Typed note lands while the utterance is still streaming.
    let note = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "typed in between".into(),
                targets: vec![hash(1)],
            },
            None,
        )
        .unwrap();
    let committed = drive(&mut engine, &clock, &store, 3150, &[]);
    assert_eq!(committed.len(), 1);
    assert!(
        committed[0].id < note.id,
        "voice id minted at onset precedes the later typed note in log order"
    );
}

// ---------------------------------------------------------------------------
// §6.3 endpointer feed — trailing silence ships, long silence does not
// ---------------------------------------------------------------------------

/// Founder dogfood (June 2026): the VAD gate shipped ONLY speech frames,
/// so the ASR's trailing-silence endpoint rules never saw silence and no
/// final ever minted while armed — partials forever, zero journal
/// entries. The engine now ships a TRAILING_SHIP_MS window after the gate
/// closes (enough for the server's rule2 = 1.2 s), and still ships
/// nothing once the window passes. The mock's stream clock advances only
/// on RECEIVED frames — exactly the real server's epistemics.
#[test]
fn c6_3_trailing_silence_reaches_the_endpointer_so_finals_mint_while_armed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        // The server-side endpoint: ~1.2 s of RECEIVED silence after the
        // utterance — reachable only because post-gate frames ship.
        ScriptEntry {
            at: 2100,
            event: ScriptedEvent::Segment(final_seg(0, "one two three test", 100, 900)),
        },
        // Far past the ship window (gate close + TRAILING_SHIP_MS): must
        // NOT fire while armed — long armed silence ships nothing.
        ScriptEntry {
            at: 6000,
            event: ScriptedEvent::Segment(final_seg(1, "ghost", 5000, 5400)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(1)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(&mut engine, &clock, &store, 8_000, &[]);
    assert_eq!(
        committed.len(),
        1,
        "the endpointed final minted WHILE ARMED; the ghost stayed starved"
    );
    assert_eq!(committed[0].text.as_deref(), Some("one two three test"));
    assert!(engine.mic().is_armed(), "no disarm was needed to finalize");
    assert_eq!(engine.streaming_count(), 0, "the utterance settled");

    // Disarm closes the input; the mock flushes the remainder — the ghost
    // mints as a trailing final inside the §6.4 drain window.
    let drained = engine.disarm(&store);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text.as_deref(), Some("ghost"));
    assert!(!engine.stream_open());
}

/// §6.5 verbatim protects the user's WORDS, not the tokenizer's plumbing:
/// BPE-style ASR tokens carry their word-boundary space, so real servers
/// deliver finals like " Slow down" — and untrimmed minting saved a
/// leading space on EVERY voice note (founder dogfood, June 12 2026,
/// confirmed in the store). Edges trim; interior spacing is untouched.
#[test]
fn voice_finals_mint_edge_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 100,
            end: 900,
        }],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 2100,
        event: ScriptedEvent::Segment(final_seg(0, " Slow  down ", 100, 900)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(1)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(&mut engine, &clock, &store, 4_000, &[]);
    assert_eq!(committed.len(), 1);
    assert_eq!(
        committed[0].text.as_deref(),
        Some("Slow  down"),
        "edges trimmed, interior spacing verbatim"
    );
}

// ---------------------------------------------------------------------------
// §5.1/§6.3 — VAD/ASR segment-count disagreement: onset-proximity association
// ---------------------------------------------------------------------------

/// pp_voice_bench defect (founder, first voice dogfood, June 2026),
/// reproduced exactly: three VAD onsets at ~0 s / 6.55 s / 14.1 s, but a
/// 0.8 s thought-pause splits the VAD at its 480 ms hang while the ASR's
/// 1.2 s trailing-silence rule MERGES the first two spans into final#0.
/// Old FIFO association then bound final#1 (real onset 14.1 s) to the
/// 6.55 s in-flight — wrong scope — and abandoned the 14.1 s in-flight at
/// disarm. Onset proximity binds final#1 to the THIRD onset's held
/// snapshot, and the merged-away second in-flight settles without minting
/// and without counting abandoned. The held VAD snapshot stays
/// authoritative for binding (§5.1); proximity only changes WHICH held
/// snapshot a segment claims.
#[test]
fn c13_1_vad_split_asr_merge_binds_next_final_by_onset_proximity() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 0,
                end: 5750,
            },
            // 0.8 s pause before this span: VAD splits, ASR will not.
            SpeechSpan {
                onset: 6550,
                end: 12_000,
            },
            SpeechSpan {
                onset: 14_100,
                end: 17_000,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        // final#0: the ASR endpointer merged the first two VAD spans — its
        // onset is ~0 and its end covers the SECOND span entirely.
        ScriptEntry {
            at: 13_300,
            event: ScriptedEvent::Segment(final_seg(
                1,
                "the gesture is lovely and the light holds it",
                0,
                12_000,
            )),
        },
        // final#1: real onset 14.1 s — must bind the THIRD held onset.
        ScriptEntry {
            at: 18_300,
            event: ScriptedEvent::Segment(final_seg(2, "this one is sharper", 14_100, 17_000)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(
        &mut engine,
        &clock,
        &store,
        18_500,
        &[
            (6_500, vec![hash(0xB)]),  // before the second (merged-away) onset
            (14_050, vec![hash(0xC)]), // before the third onset
        ],
    );

    assert_eq!(committed.len(), 2, "two finals, two events — no extra mint");
    assert_eq!(
        committed[0].text.as_deref(),
        Some("the gesture is lovely and the light holds it")
    );
    assert_eq!(
        committed[0].targets,
        vec![hash(0xA)],
        "final#0 binds the FIRST onset's held snapshot"
    );
    assert_eq!(committed[1].text.as_deref(), Some("this one is sharper"));
    assert_eq!(
        committed[1].targets,
        vec![hash(0xC)],
        "final#1 binds the THIRD onset's snapshot, not the stranded second"
    );
    assert_eq!(
        committed[1].ts,
        UtcMillis::from_epoch_ms(WALL0 + 14_100),
        "final#1's ts is the third VAD onset's wall time"
    );
    assert_eq!(
        engine.streaming_count(),
        0,
        "the merged-away second in-flight settled when final#0 minted"
    );
    assert_eq!(
        engine.abandoned_count(),
        0,
        "settling a merged onset is not abandonment"
    );
    assert!(
        engine
            .debug_notes()
            .iter()
            .any(|n| n.contains("merged into final")),
        "the retirement leaves a debug-panel note"
    );

    // Disarm: nothing left in flight, so nothing to abandon and nothing
    // extra mints in the drain window.
    let drained = engine.disarm(&store);
    assert!(drained.is_empty());
    assert_eq!(
        engine.abandoned_count(),
        0,
        "no phantom abandon at disarm — the old FIFO bug's second symptom"
    );
    assert!(!engine.stream_open());
}

/// B72: retirement is NOT coupled to minting. A whitespace-only final
/// (§6.5: mint nothing) that merged two VAD spans still consumed both
/// onsets — the associated one AND the merged-away one. If the merged-away
/// onset survived the early return, the mic would stick in ArmedSpeaking
/// with nobody speaking, the indicator would report a phantom streaming
/// utterance forever, and disarm would count a phantom abandon — the exact
/// symptom pair retirement exists to eliminate.
#[test]
fn c13_1_whitespace_merged_final_still_retires_the_merged_onset() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 0,
                end: 5750,
            },
            SpeechSpan {
                onset: 6550,
                end: 12_000,
            },
        ],
    );
    // The VAD fired twice on breath noise; the ASR endpointer merged the
    // spans and decoded nothing: one whitespace final covering both onsets.
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 13_300,
        event: ScriptedEvent::Segment(final_seg(1, "   \n\t ", 0, 12_000)),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(&mut engine, &clock, &store, 13_500, &[]);
    assert!(
        committed.is_empty(),
        "whitespace finals mint nothing (§6.5)"
    );
    assert_eq!(
        engine.streaming_count(),
        0,
        "BOTH consumed onsets settled — the merged-away one too"
    );
    assert_eq!(engine.mic(), MicState::ArmedIdle, "no stuck ArmedSpeaking");
    assert_eq!(
        engine.indicator().streaming_utterance,
        None,
        "no phantom streaming indicator"
    );

    let drained = engine.disarm(&store);
    assert!(drained.is_empty());
    assert_eq!(
        engine.abandoned_count(),
        0,
        "no phantom abandon at disarm for the merged-away onset"
    );
}

/// B72 + §9.2: a merged final's durable span covers the speech it absorbed.
/// The user draws a stroke at ~8 s while speaking the merged utterance's
/// second half; in-flight suppression commits the stroke unlinked with the
/// promise that the utterance carries the link. The utterance's effective
/// end must therefore extend over the retired onset's VAD end (12 s) — a
/// span stopping at the FIRST VAD end (5.75 s) would miss the stroke and
/// drop the link forever.
#[test]
fn c13_8a_merged_final_span_covers_retired_onsets_so_the_link_survives() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let image = hash(0xA);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 0,
                end: 5750,
            },
            // 0.8 s pause: VAD splits, the ASR endpointer will not.
            SpeechSpan {
                onset: 6550,
                end: 12_000,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![ScriptEntry {
        at: 13_300,
        event: ScriptedEvent::Segment(final_seg(
            1,
            "the gesture is lovely and the light holds it",
            0,
            12_000,
        )),
    }]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![image.clone()]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    drive(&mut engine, &clock, &store, 8_000, &[]);
    assert_eq!(engine.streaming_count(), 2, "both spans in the air");
    // Pen-up at mono 8000 (stroke spans 7500..8000), during the merged
    // utterance's second half: suppression keeps the stroke unlinked.
    let stroke = engine
        .commit_stroke(&store, image.clone(), stroke_payload_ending_at(500))
        .unwrap();
    assert_eq!(stroke.linked_event, None, "in-flight suppression (§9.2)");

    let committed = drive_span(&mut engine, &clock, &store, 8_000, 13_500, &[]);
    assert_eq!(committed.len(), 1, "one merged final, one event");
    assert_eq!(
        committed[0].linked_event,
        Some(stroke.id.clone()),
        "the merged utterance carries the link its suppression promised"
    );
    assert_eq!(
        committed[0].payload,
        Some(photoproof_core::Payload::Voice {
            conf_pm: Some(910),
            dur_ms: 12_000,
        }),
        "dur_ms spans onset through the RETIRED onset's VAD end"
    );
}

/// B72's skew bound: an ASR-split final (§5.3) whose own onset has no
/// unclaimed partner must NOT steal a distant utterance's held snapshot.
/// One continuous VAD span splits into two finals; the second final's
/// delivery lags past the NEXT utterance's VAD onset (long-utterance
/// decode latency). Unbounded proximity would claim that onset — wrong
/// targets, wrong minted ts, and the victim's own final falls to a fresh
/// mint. The bound makes the laggard fall through to §5.3 independent
/// binding by its own onset instead.
#[test]
fn c13_1_late_split_final_falls_through_to_independent_binding() {
    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    let vad = MockVad::new(
        SR,
        vec![
            SpeechSpan {
                onset: 200,
                end: 4_000,
            },
            SpeechSpan {
                onset: 8_000,
                end: 9_500,
            },
        ],
    );
    let transcriber = MockTranscriber::new("mock-asr", SR).with_script(vec![
        // The ASR split the first VAD span into two finals.
        ScriptEntry {
            at: 5_000,
            event: ScriptedEvent::Segment(final_seg(1, "lovely gesture here", 200, 1_900)),
        },
        // The split's second half arrives LATE — after the second VAD
        // onset (8 s, scope C) is already in flight and unclaimed. Its
        // own onset (2.5 s) is 5.5 s from that onset: past the bound.
        ScriptEntry {
            at: 9_000,
            event: ScriptedEvent::Segment(final_seg(2, "and the light holds it", 2_500, 4_000)),
        },
        ScriptEntry {
            at: 10_500,
            event: ScriptedEvent::Segment(final_seg(3, "this one is sharper", 8_000, 9_500)),
        },
    ]);
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    engine.set_scope(vec![hash(0xA)]);
    assert_eq!(engine.arm(), MicState::ArmedIdle);

    let committed = drive(
        &mut engine,
        &clock,
        &store,
        11_000,
        &[(7_900, vec![hash(0xC)])], // scope moves before the second onset
    );

    assert_eq!(committed.len(), 3, "three finals, three events");
    assert_eq!(committed[0].targets, vec![hash(0xA)]);
    assert_eq!(
        committed[1].targets,
        vec![hash(0xA)],
        "the late split final binds by ITS OWN onset's scope (§5.3), not \
         the distant unclaimed onset's snapshot"
    );
    assert_eq!(
        committed[1].ts,
        UtcMillis::from_epoch_ms(WALL0 + 2_500),
        "ts minted at the split final's own onset, not a stolen one"
    );
    assert_eq!(
        committed[2].targets,
        vec![hash(0xC)],
        "the second utterance keeps its held snapshot — nothing stole it"
    );
    assert_eq!(
        committed[2].ts,
        UtcMillis::from_epoch_ms(WALL0 + 8_000),
        "and keeps its onset-minted ts"
    );
    assert_eq!(engine.streaming_count(), 0);
    assert_eq!(engine.abandoned_count(), 0);
}

// ---------------------------------------------------------------------------
// §6.2 pre-roll — the gate's detection lag must not chop first words
// ---------------------------------------------------------------------------

/// Founder corpus (cold-starts card): the VAD opens its gate 100-400 ms
/// INTO the first word, and without a pre-roll that onset audio never
/// reached the recognizer ("Okay this contact" arrived as "This
/// contact"). The engine now retains up to PRE_ROLL_MS of withheld
/// frames and flushes them ahead of the first shipped frame. This test
/// records exactly which frames the transcriber RECEIVES: silence well
/// before the onset never ships; the pre-roll window immediately before
/// the VAD onset does.
#[test]
fn c6_2_pre_roll_ships_the_audio_from_before_the_vad_onset() {
    use photoproof_core::capture::PRE_ROLL_MS;

    struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<StreamMs>>>);
    struct RecorderStream<'a> {
        audio: BoxStream<'a, AudioFrame>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<StreamMs>>>,
    }
    impl Stream for RecorderStream<'_> {
        type Item = ConnectorResult<TranscriptSegment>;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            while let Poll::Ready(item) = this.audio.as_mut().poll_next(cx) {
                match item {
                    Some(frame) => this.seen.lock().unwrap().push(frame.captured_at),
                    None => return Poll::Ready(None),
                }
            }
            Poll::Pending
        }
    }
    impl Transcriber for Recorder {
        fn stream<'a>(
            &'a self,
            audio: BoxStream<'a, AudioFrame>,
        ) -> ConnectorResult<BoxStream<'a, ConnectorResult<TranscriptSegment>>> {
            Ok(Box::pin(RecorderStream {
                audio,
                seen: self.0.clone(),
            }))
        }
        fn sample_rate(&self) -> u32 {
            SR
        }
        fn model_id(&self) -> &str {
            "recorder"
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let (store, session) = open_store(&dir);
    let clock = FakeClock::new(WALL0);
    // Speech begins at 2000 ms; everything before is gate-closed silence.
    let vad = MockVad::new(
        SR,
        vec![SpeechSpan {
            onset: 2_000,
            end: 3_000,
        }],
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let transcriber = Recorder(seen.clone());
    let mut engine = CaptureEngine::new(clock.clone(), &transcriber, Box::new(vad), session);
    assert_eq!(engine.arm(), MicState::ArmedIdle);
    drive(&mut engine, &clock, &store, 3_000, &[]);

    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "frames shipped");
    let first = seen[0];
    assert!(
        first < 2_000,
        "pre-roll shipped audio from BEFORE the VAD onset (first shipped at {first} ms)"
    );
    assert!(
        2_000 - first <= PRE_ROLL_MS + 50,
        "pre-roll bounded: first shipped at {first} ms vs onset 2000 (cap {PRE_ROLL_MS} ms + one frame)"
    );
    // Long-before silence stays unshipped: nothing older than the cap
    // (plus one 50 ms frame of boundary slack) ever reaches the wire.
    assert!(
        seen.iter().all(|&t| t + PRE_ROLL_MS + 50 >= 2_000),
        "deep silence never ships: {:?}",
        &seen[..4.min(seen.len())]
    );
    // And the shipped sequence is in order (the flush precedes the live frame).
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "frames ship in order");
}
