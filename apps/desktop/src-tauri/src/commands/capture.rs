//! Scope & capture commands (CAPTURE §3–4, §8, §10–11) — moved verbatim
//! from the old commands.rs (FOUNDATIONS split; owned by no parallel
//! stage). P5.1 adds `add_stroke`, the grease pencil's one new command.

use photoproof_core::event::{
    ORIENTATION_MAX, ORIENTATION_MIN, STROKE_COORD_MAX, STROKE_COORD_MIN, STROKE_MAX_POINTS,
    STROKE_PRESSURE_MAX,
};
use photoproof_core::{
    ContentHash, EventDraft, EventId, EventStore, RemarkSource, SessionId, StrokePayload,
    StrokePoint, Tool,
};
use tauri::{AppHandle, Emitter};

use super::{S, announce_events, emit_journal_changed, emit_pulse, hashes, indicator, parse_hash};
use crate::dto::{IndicatorState, ScopeView, StrokeCommitDto, StrokePayloadDto};
use crate::error::{CmdError, CmdResult};
use crate::note::normalize_note;

/// The UI reports its selection/view-derived target list (ordered); the core
/// echoes the scope back (UI §3.4: the UI performs no scope logic).
#[tauri::command]
pub fn set_scope(app: S<'_>, handle: AppHandle, targets: Vec<String>) -> CmdResult<ScopeView> {
    app.touch()?;
    let hashes: Result<Vec<ContentHash>, _> =
        targets.iter().map(|t| ContentHash::from_hex(t)).collect();
    let hashes = hashes.map_err(|e| CmdError::Invalid(format!("bad target hash: {e}")))?;
    let view = {
        let mut scope = app.scope.lock().expect("scope mutex");
        scope.set(hashes.clone())
    };
    // P6.4: the capture engine's scope ring gets every update too — VOICE
    // binding snapshots `scope_at(onset)` from the ring (CAPTURE §5.1), so
    // it must be current the moment speech starts, armed or not.
    if let Some(engine) = app.capture.lock().expect("capture mutex").as_mut() {
        engine.set_scope(hashes);
    }
    let _ = handle.emit("indicator-state", indicator(&app));
    Ok(view)
}

/// M-key toggle (CAPTURE §6.4). Arm opens the supervised ASR stream — an
/// `Err` at open IS the readiness answer and lands `Disarmed(error)`
/// quietly — then starts the `pp-mic` audio thread. Disarm stops audio,
/// accepts trailing finals (engine-bounded, ≤ 5 s), zeroes the ring, and
/// joins the thread. Echoes the §11 indicator either way.
#[tauri::command]
pub fn toggle_mic(app: S<'_>, handle: AppHandle) -> CmdResult<IndicatorState> {
    app.touch()?; // the toggle is user activity (§2.1); may rotate first
    let armed_now = {
        let mut capture = app.capture.lock().expect("capture mutex");
        match capture.as_mut() {
            // The in-process VAD never built: nothing to toggle.
            None => false,
            Some(engine) if engine.mic().is_armed() => {
                let events = engine.disarm(&app.store);
                let draining = engine.stream_open();
                drop(capture);
                // The thread sees the disarmed engine and exits; take()
                // joins it (MicHandle::drop) and the cpal stream closes.
                drop(app.mic.lock().expect("mic mutex").take());
                announce_events(&handle, &events);
                if draining {
                    // A trailing final is still due (§6.4): with the mic
                    // thread gone nothing pumps — the drain thread does.
                    crate::mic::spawn_disarm_drain(handle.clone());
                }
                false
            }
            Some(engine) => {
                let state = engine.arm();
                drop(capture);
                if state.is_armed() {
                    *app.mic.lock().expect("mic mutex") = Some(crate::mic::start(handle.clone()));
                }
                state.is_armed()
            }
        }
    };
    // §5.2: downloads throttle while capture is live (the pacer reads
    // this per chunk).
    app.runtime
        .capture_live
        .store(armed_now, std::sync::atomic::Ordering::Relaxed);
    let state = indicator(&app);
    let _ = handle.emit("indicator-state", state.clone());
    Ok(state)
}

#[tauri::command]
pub fn indicator_state(app: S<'_>) -> IndicatorState {
    indicator(&app)
}

/// Append one typed remark over an explicit target list (CAPTURE §4 text
/// rules: verbatim, one trailing newline trimmed; empty mints nothing).
/// Returns whether an event was committed.
pub(crate) fn mint_note(
    store: &EventStore,
    session: &SessionId,
    text: &str,
    targets: Vec<ContentHash>,
) -> CmdResult<bool> {
    let Some(text) = normalize_note(text) else {
        return Ok(false);
    };
    store.append(
        session,
        EventDraft::Remark {
            source: RemarkSource::Typed,
            text,
            targets,
        },
        None,
    )?;
    Ok(true)
}

/// Append one rating over an explicit target list (CAPTURE §10, C6: 0 =
/// explicit clear). Zero targets = session scope: rating keys do nothing.
pub(crate) fn mint_rating(
    store: &EventStore,
    session: &SessionId,
    value: u8,
    targets: Vec<ContentHash>,
) -> CmdResult<bool> {
    if value > 5 {
        return Err(CmdError::Invalid(format!("rating out of range: {value}")));
    }
    if targets.is_empty() {
        return Ok(false); // session scope: rating keys do nothing
    }
    store.append(session, EventDraft::Rating { value, targets }, None)?;
    Ok(true)
}

/// The command's target list: the current scope snapshot (CAPTURE §3/§4 —
/// the N transient and rating keys), or the explicit single image the
/// journal-panel composer names (BACKLOG "compose entries from the journal
/// panel") — the deliberate panel-context variant: panel-composed entries
/// bind to the PANEL's image, never the grid write-scope.
fn note_targets(app: &S<'_>, target: Option<&str>) -> CmdResult<Vec<ContentHash>> {
    match target {
        Some(hash) => Ok(vec![parse_hash(hash)?]),
        None => Ok(app
            .scope
            .lock()
            .expect("scope mutex")
            .current()
            .targets
            .clone()),
    }
}

/// Typed note (CAPTURE §4): binds to the current scope snapshot at submit
/// time, or — `target` given — to that single image (the journal-panel
/// composer's explicit binding; see `note_targets`). Verbatim text (one
/// trailing newline trimmed); empty mints nothing. Returns whether an
/// event was committed.
#[tauri::command]
pub fn add_note(
    app: S<'_>,
    handle: AppHandle,
    text: String,
    target: Option<String>,
) -> CmdResult<bool> {
    app.touch()?;
    let targets = note_targets(&app, target.as_deref())?;
    let session = app.session_id();
    if !mint_note(&app.store, &session, &text, targets.clone())? {
        return Ok(false);
    }
    emit_pulse(&handle, "remark");
    emit_journal_changed(&handle, hashes(&targets));
    Ok(true)
}

/// Rating keys 0–5 (CAPTURE §10, DECISIONS C6): bound to the current scope
/// at keystroke time; multi-select mints ONE event targeting all N (selection
/// order); 0 = explicit clear; session scope → rating keys do nothing.
/// `target` is the journal-panel composer's explicit single-image binding
/// (never session scope, so it always rates — see `note_targets`).
#[tauri::command]
pub fn set_rating(
    app: S<'_>,
    handle: AppHandle,
    value: u8,
    target: Option<String>,
) -> CmdResult<bool> {
    app.touch()?;
    let targets = note_targets(&app, target.as_deref())?;
    let session = app.session_id();
    if !mint_rating(&app.store, &session, value, targets.clone())? {
        return Ok(false);
    }
    emit_pulse(&handle, "rating");
    emit_journal_changed(&handle, hashes(&targets));
    Ok(true)
}

/// Generic activity report (CAPTURE §2.1: keyboard/pointer input, view
/// changes). The frontend throttles; this refreshes the idle timer and
/// echoes the CURRENT (post-touch) session id — session closure is lazy
/// (§2.2: the 30-minute boundary applies at the next activity), so this
/// echo is how the frontend observes a rotation and clears its
/// session-scoped pencil undo stack (§8.5).
#[tauri::command]
pub fn report_activity(app: S<'_>) -> CmdResult<String> {
    app.touch()?;
    Ok(app.session_id().as_str().to_owned())
}

// ---------------------------------------------------------------------------
// The grease pencil (P5.1 — CAPTURE §8, EVENTS §3.3, DECISIONS X1/C5)
// ---------------------------------------------------------------------------

/// `base_w` sanity envelope, command-side ONLY: core deliberately leaves
/// the width unbounded, so unlike the spec-pinned core ranges beside it
/// this value is shell-owned. 1..=10000 means "up to the whole long edge"
/// (base_w is in ten-thousandths of the display-oriented long edge) — a
/// sane ceiling for a per-stroke width.
const MAX_BASE_W: u32 = 10_000;

/// Wire payload → core `StrokePayload`, re-checking every §8.2 range at the
/// command boundary for honest error messages (core's `append` re-validates
/// the canonical encoding it has owned since P1.1). The spec ranges are
/// core's published consts (event.rs) so this boundary check provably
/// cannot drift from the canonical re-validation; `base_w` sanity is
/// command-side (see `MAX_BASE_W`).
fn stroke_payload(dto: StrokePayloadDto) -> CmdResult<StrokePayload> {
    let invalid = |msg: String| CmdError::Invalid(format!("invalid stroke payload: {msg}"));
    let Some(tool) = Tool::parse(&dto.tool) else {
        return Err(invalid(format!("unknown tool {:?}", dto.tool)));
    };
    // The rejection messages interpolate the same consts the predicates
    // check, so a retuned bound (MAX_BASE_W especially, the one shell-owned
    // knob here) can never produce an error citing a stale range.
    if !(ORIENTATION_MIN..=ORIENTATION_MAX).contains(&dto.orientation) {
        return Err(invalid(format!(
            "orientation {} not in {ORIENTATION_MIN}..={ORIENTATION_MAX}",
            dto.orientation
        )));
    }
    if dto.base_w == 0 || dto.base_w > MAX_BASE_W {
        return Err(invalid(format!(
            "base_w {} not in 1..={MAX_BASE_W}",
            dto.base_w
        )));
    }
    if dto.points.is_empty() || dto.points.len() > STROKE_MAX_POINTS {
        return Err(invalid(format!(
            "{} points not in 1..={STROKE_MAX_POINTS}",
            dto.points.len()
        )));
    }
    // The wire carries i64; the core consts are the target types — widen
    // them once for the range checks.
    let coords = i64::from(STROKE_COORD_MIN)..=i64::from(STROKE_COORD_MAX);
    let mut prev_t: u32 = 0;
    let mut points = Vec::with_capacity(dto.points.len());
    for (i, [x, y, p, t]) in dto.points.iter().copied().enumerate() {
        if !coords.contains(&x) || !coords.contains(&y) {
            return Err(invalid(format!(
                "point {i} coordinates out of {STROKE_COORD_MIN}..={STROKE_COORD_MAX}"
            )));
        }
        if !(0..=i64::from(STROKE_PRESSURE_MAX)).contains(&p) {
            return Err(invalid(format!(
                "point {i} pressure out of 0..={STROKE_PRESSURE_MAX}"
            )));
        }
        let t = u32::try_from(t).map_err(|_| invalid(format!("point {i} time negative")))?;
        if i == 0 && t != 0 {
            return Err(invalid("t[0] must be 0".into()));
        }
        if t < prev_t {
            return Err(invalid(format!("point {i} time decreases")));
        }
        prev_t = t;
        // Casts proven lossless by the range checks above.
        points.push(StrokePoint {
            x: x as i32,
            y: y as i32,
            p: p as u16,
            t,
        });
    }
    Ok(StrokePayload {
        base_w: dto.base_w,
        orientation: dto.orientation,
        points,
        tool,
    })
}

/// Mint one stroke event bound to the single VIEWED image (CAPTURE §8:
/// always `single`; the scope ring buffer is NOT consulted). The stroke
/// commits UNLINKED — link resolution is P6.1's commit-time job (X2/C5).
pub(crate) fn mint_stroke(
    store: &EventStore,
    session: &SessionId,
    hash: &str,
    payload: StrokePayloadDto,
) -> CmdResult<EventId> {
    let target = parse_hash(hash)?;
    let payload = stroke_payload(payload)?;
    let event = store.append(
        session,
        EventDraft::Stroke {
            payload,
            target,
            linked_event: None,
        },
        None,
    )?;
    Ok(event.id)
}

/// Pen-up commit (CAPTURE §8.4): `kind=stroke`, `source=pencil`, one
/// event per pen-down→pen-up. Returns the event id plus the session it
/// landed in (the frontend's SESSION-SCOPED undo stack needs both —
/// §8.5/C4: a stack entry from a closed session must never be Ctrl+Z'd)
/// and pulses the indicator — the entire visible feedback.
#[tauri::command]
pub fn add_stroke(
    app: S<'_>,
    handle: AppHandle,
    hash: String,
    payload: StrokePayloadDto,
) -> CmdResult<StrokeCommitDto> {
    app.touch()?; // pen contact is activity (CAPTURE §2.1) — may rotate
    let session = app.session_id();
    let id = mint_stroke(&app.store, &session, &hash, payload)?;
    emit_pulse(&handle, "stroke");
    emit_journal_changed(&handle, vec![hash]);
    Ok(StrokeCommitDto {
        id: id.as_str().to_owned(),
        session_id: session.as_str().to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Tests — stroke minting + validation against a temp EventStore (the
// journal.rs fixture pattern); CAPTURE §13.4's command/controller slice.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use photoproof_core::{Kind, Payload, RetractionSource, SessionContext, Source};

    use super::*;
    use crate::commands::journal::journal_entries;

    fn store_fixture() -> (tempfile::TempDir, EventStore, SessionId, ContentHash) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let session = store
            .open_session(SessionContext {
                app_version: "0.0.1-test".into(),
                device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
                root_context: None,
            })
            .expect("open session");
        let hash = ContentHash::from_hex(&"ab".repeat(32)).expect("hash");
        (tmp, store, session, hash)
    }

    fn wire(points: Vec<[i64; 4]>) -> StrokePayloadDto {
        StrokePayloadDto {
            base_w: 40,
            orientation: 1,
            points,
            tool: "pencil".into(),
        }
    }

    fn three_points() -> Vec<[i64; 4]> {
        vec![
            [4312, 2210, 1000, 0],
            [4330, 2204, 820, 9],
            [4391, 2188, 770, 17],
        ]
    }

    /// The journal-panel composer (BACKLOG "compose entries from the
    /// journal panel"): a typed remark bound to ONE explicit target — the
    /// panel's image, never the grid write-scope (the scope ring buffer is
    /// not consulted on this path at all).
    #[test]
    fn mint_note_binds_to_the_explicit_panel_target() {
        let (_tmp, store, session, hash) = store_fixture();
        assert!(mint_note(&store, &session, "quiet keeper\n", vec![hash.clone()]).unwrap());
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "remark");
        assert_eq!(rows[0].source, "typed");
        assert_eq!(
            rows[0].text.as_deref(),
            Some("quiet keeper"),
            "verbatim, one trailing newline trimmed (CAPTURE §4)"
        );
        assert_eq!(rows[0].targets, vec![hash.as_str().to_owned()]);
        // Empty/whitespace mints nothing (CAPTURE §4).
        assert!(!mint_note(&store, &session, "   \n", vec![hash.clone()]).unwrap());
        assert_eq!(journal_entries(&store, &hash).unwrap().len(), 1);
    }

    /// Composer rating: explicit single target commits (never session
    /// scope), 0 clears through the same fold (C6), range still checked.
    #[test]
    fn mint_rating_commits_explicit_targets_and_declines_session_scope() {
        let (_tmp, store, session, hash) = store_fixture();
        assert!(mint_rating(&store, &session, 4, vec![hash.clone()]).unwrap());
        assert_eq!(store.current_rating(&hash).unwrap(), Some(4));
        assert!(mint_rating(&store, &session, 0, vec![hash.clone()]).unwrap());
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.last().unwrap().rating, Some(0), "0 = explicit clear");
        // Zero targets = session scope: rating keys do nothing (§10).
        assert!(!mint_rating(&store, &session, 3, vec![]).unwrap());
        assert!(mint_rating(&store, &session, 6, vec![hash]).is_err());
    }

    #[test]
    fn add_stroke_mints_a_single_target_pencil_event_unlinked() {
        let (_tmp, store, session, hash) = store_fixture();
        let id = mint_stroke(&store, &session, hash.as_str(), wire(three_points())).unwrap();
        let e = store.raw_event(&id).unwrap().expect("event exists");
        assert_eq!(e.kind, Kind::Stroke);
        assert_eq!(e.source, Source::Pencil);
        assert_eq!(
            e.targets,
            vec![hash.clone()],
            "bound to the viewed image only"
        );
        assert_eq!(
            e.linked_event, None,
            "C5: strokes commit unlinked until P6.1"
        );
        let Some(Payload::Stroke(p)) = e.payload else {
            panic!("stroke payload missing");
        };
        assert_eq!(p.base_w, 40);
        assert_eq!(p.orientation, 1);
        assert_eq!(p.points.len(), 3);
        assert_eq!((p.points[1].x, p.points[1].y), (4330, 2204));
        assert_eq!(
            (p.points[1].p, p.points[1].t),
            (820, 9),
            "p/t round-trip exactly"
        );
    }

    #[test]
    fn stroke_validation_rejects_every_8_2_violation() {
        let (_tmp, store, session, hash) = store_fixture();
        let h = hash.as_str();
        let cases: Vec<(&str, StrokePayloadDto)> = vec![
            (
                "tool",
                StrokePayloadDto {
                    tool: "marker".into(),
                    ..wire(three_points())
                },
            ),
            (
                "orientation 0",
                StrokePayloadDto {
                    orientation: 0,
                    ..wire(three_points())
                },
            ),
            (
                "orientation 9",
                StrokePayloadDto {
                    orientation: 9,
                    ..wire(three_points())
                },
            ),
            (
                "base_w 0",
                StrokePayloadDto {
                    base_w: 0,
                    ..wire(three_points())
                },
            ),
            (
                "base_w 10001",
                StrokePayloadDto {
                    base_w: 10_001,
                    ..wire(three_points())
                },
            ),
            ("zero points", wire(vec![])),
            (
                "8193 points",
                wire((0..8193).map(|i| [0, 0, 1000, i]).collect()),
            ),
            ("x low", wire(vec![[-2501, 0, 1000, 0]])),
            ("x high", wire(vec![[12_501, 0, 1000, 0]])),
            ("y high", wire(vec![[0, 12_501, 1000, 0]])),
            ("pressure", wire(vec![[0, 0, 1001, 0]])),
            ("t0 nonzero", wire(vec![[0, 0, 1000, 5]])),
            ("t negative", wire(vec![[0, 0, 1000, 0], [1, 1, 1000, -3]])),
            (
                "t decreases",
                wire(vec![[0, 0, 1000, 0], [9, 9, 1000, 9], [9, 12, 1000, 8]]),
            ),
        ];
        for (label, dto) in cases {
            assert!(
                mint_stroke(&store, &session, h, dto).is_err(),
                "{label} must be rejected"
            );
        }
        // The boundary itself is valid: 8192 points, full coordinate range.
        let mut pts: Vec<[i64; 4]> = (0..8191).map(|i| [i % 10_000, i % 10_000, 0, i]).collect();
        pts.insert(0, [-2500, 12_500, 1000, 0]);
        pts[1][3] = 1; // keep t non-decreasing after the prepend
        assert!(mint_stroke(&store, &session, h, wire(pts)).is_ok());
    }

    /// CAPTURE §13.4, the command/controller slice (P1.1/P2.1 proved the
    /// fold/rebuild halves): undo twice appends two tombstones, mutates
    /// ZERO rows, and the fold excludes exactly the two retracted strokes.
    #[test]
    fn c13_4_undo_twice_appends_two_tombstones_zero_rows_mutated() {
        let (_tmp, store, session, hash) = store_fixture();
        let mut ids = Vec::new();
        for i in 0..3 {
            let pts = vec![
                [i * 100, i * 100, 1000, 0],
                [i * 100 + 40, i * 100 + 40, 1000, 8],
            ];
            ids.push(mint_stroke(&store, &session, hash.as_str(), wire(pts)).unwrap());
        }
        let before: Vec<_> = store.events_for_image(&hash).unwrap();
        assert_eq!(before.len(), 3);
        // Undo = retraction of the most recent non-retracted stroke (§8.5);
        // the frontend stack pops ids[2] then ids[1], through the SAME
        // tombstone path the journal panel's Retract uses.
        for id in [&ids[2], &ids[1]] {
            store
                .append(
                    &session,
                    EventDraft::Retraction {
                        target: id.clone(),
                        source: RetractionSource::System,
                    },
                    None,
                )
                .unwrap();
        }
        let after = store.events_for_image(&hash).unwrap();
        assert_eq!(after.len(), 5, "two tombstones APPENDED");
        for e in &before {
            let now = store.raw_event(&e.id).unwrap().expect("row survives");
            assert_eq!(&now, e, "zero rows mutated");
        }
        // Overlay + fold exclude the retracted pair; the first stroke stands.
        let folded = store.folded_journal(&hash).unwrap();
        assert_eq!(folded.len(), 1);
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.iter().filter(|r| !r.retracted).count(), 1);
        assert_eq!(rows.iter().filter(|r| r.retracted).count(), 2);
        assert!(rows.iter().all(|r| r.kind == "stroke"));
    }
}
