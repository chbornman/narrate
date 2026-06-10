//! Scope & capture commands (CAPTURE §3–4, §8, §10–11) — moved verbatim
//! from the old commands.rs (FOUNDATIONS split; owned by no parallel
//! stage). P5.1 adds `add_stroke`, the grease pencil's one new command.

use photoproof_core::{
    ContentHash, EventDraft, EventId, EventStore, RemarkSource, SessionId, StrokePayload,
    StrokePoint, Tool, UtcMillis,
};
use tauri::{AppHandle, Emitter};

use super::{S, emit_pulse, indicator};
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
        scope.set(hashes, UtcMillis::now()).view()
    };
    let _ = handle.emit("indicator-state", indicator(&app));
    Ok(view)
}

#[tauri::command]
pub fn indicator_state(app: S<'_>) -> IndicatorState {
    indicator(&app)
}

/// Typed note (CAPTURE §4): binds to the current scope snapshot at submit
/// time; verbatim text (one trailing newline trimmed); empty mints nothing.
/// Returns whether an event was committed.
#[tauri::command]
pub fn add_note(app: S<'_>, handle: AppHandle, text: String) -> CmdResult<bool> {
    app.touch()?;
    let Some(text) = normalize_note(&text) else {
        return Ok(false);
    };
    let targets = app
        .scope
        .lock()
        .expect("scope mutex")
        .current()
        .targets
        .clone();
    let session = app.session_id();
    app.store.append(
        &session,
        EventDraft::Remark {
            source: RemarkSource::Typed,
            text,
            targets,
        },
        None,
    )?;
    emit_pulse(&handle, "remark");
    Ok(true)
}

/// Rating keys 0–5 (CAPTURE §10, DECISIONS C6): bound to the current scope
/// at keystroke time; multi-select mints ONE event targeting all N (selection
/// order); 0 = explicit clear; session scope → rating keys do nothing.
#[tauri::command]
pub fn set_rating(app: S<'_>, handle: AppHandle, value: u8) -> CmdResult<bool> {
    app.touch()?;
    if value > 5 {
        return Err(CmdError::Invalid(format!("rating out of range: {value}")));
    }
    let targets = {
        let scope = app.scope.lock().expect("scope mutex");
        let snap = scope.current();
        if snap.targets.is_empty() {
            return Ok(false); // session scope: rating keys do nothing
        }
        snap.targets.clone()
    };
    let session = app.session_id();
    app.store
        .append(&session, EventDraft::Rating { value, targets }, None)?;
    emit_pulse(&handle, "rating");
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

/// Wire payload → core `StrokePayload`, re-checking every §8.2 range at the
/// command boundary for honest error messages (core's `append` re-validates
/// the canonical encoding it has owned since P1.1). `base_w` sanity is
/// command-side: core leaves it unbounded; 1..=10000 (up to the whole long
/// edge) is the sane envelope for a per-stroke width.
fn stroke_payload(dto: StrokePayloadDto) -> CmdResult<StrokePayload> {
    let invalid = |msg: String| CmdError::Invalid(format!("invalid stroke payload: {msg}"));
    let Some(tool) = Tool::parse(&dto.tool) else {
        return Err(invalid(format!("unknown tool {:?}", dto.tool)));
    };
    if !(1..=8).contains(&dto.orientation) {
        return Err(invalid(format!(
            "orientation {} not in 1..=8",
            dto.orientation
        )));
    }
    if dto.base_w == 0 || dto.base_w > 10_000 {
        return Err(invalid(format!("base_w {} not in 1..=10000", dto.base_w)));
    }
    if dto.points.is_empty() || dto.points.len() > 8192 {
        return Err(invalid(format!(
            "{} points not in 1..=8192",
            dto.points.len()
        )));
    }
    let mut prev_t: u32 = 0;
    let mut points = Vec::with_capacity(dto.points.len());
    for (i, [x, y, p, t]) in dto.points.iter().copied().enumerate() {
        if !(-2500..=12500).contains(&x) || !(-2500..=12500).contains(&y) {
            return Err(invalid(format!(
                "point {i} coordinates out of -2500..=12500"
            )));
        }
        if !(0..=1000).contains(&p) {
            return Err(invalid(format!("point {i} pressure out of 0..=1000")));
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
    let target = ContentHash::from_hex(hash)
        .map_err(|e| CmdError::Invalid(format!("bad image hash: {e}")))?;
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
