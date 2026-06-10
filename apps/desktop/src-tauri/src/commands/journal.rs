//! Per-image journal & truth commands (featureset §3, D2) — STAGE C OWNS
//! THIS FILE.
//!
//! - `image_journal`   → folded entries (EVENTS §6.2 via
//!   `store.folded_journal`) PLUS retracted content rows, flagged, for the
//!   per-session "show retracted" toggle (UI §8.2 — the fold hides them;
//!   the toggle needs them). Redacted events fold to inert "[redacted]"
//!   stubs.
//! - `image_metadata`  → read-only EXIF subset + file identity (K16: from
//!   the db's `images` row; no new parsing) + preview source/backfill state.
//! - `revise_event`    → inline Correct (EventDraft::Revision; the fold
//!   shows the corrected text, EVENTS §3.4).
//! - `retract_event`   → tombstone (EventDraft::Retraction, source=system —
//!   the journal-panel button, EVENTS §3.5).
//! - `unretract_event` → the retract-toast "Undo": RE-STATE — a NEW
//!   remark/rating/stroke carrying the folded content. Retraction-of-
//!   retraction is spec-forbidden (DECISIONS E4); no new core APIs.
//! - `redact_event`    → `engine.redact` (one transaction + immediate
//!   adjacent rewrite, EVENTS §7 / SIDECARS §11) → RedactReportDto with the
//!   offline-pending volume labels for the §8.4 toast copy.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;

use photoproof_core::library::{ArtifactKind, Library};
use photoproof_core::sidecar::SidecarEngine;
use photoproof_core::{
    ContentHash, Event, EventDraft, EventId, EventStore, JournalEntry as FoldedEntry, Kind,
    Payload, RemarkSource, RetractionSource, SessionId, UtcMillis,
};
use tauri::AppHandle;

use super::{S, emit_pulse};
use crate::dto::{ImageMetadataDto, JournalEntryDto, RedactReportDto};
use crate::error::{CmdError, CmdResult};
use crate::note::normalize_note;

// ---------------------------------------------------------------------------
// Fold helpers — a local mini-fold over the image closure. The store's
// canonical fold (folded_journal) hides retracted entries entirely
// (EVENTS I12); the journal UI needs them flagged, so retracted rows are
// re-derived here with the same chain rules (EVENTS §6.1).
// ---------------------------------------------------------------------------

fn parse_hash(hash: &str) -> CmdResult<ContentHash> {
    ContentHash::from_hex(hash).map_err(|e| CmdError::Invalid(format!("bad image hash: {e}")))
}

fn hashes(targets: &[ContentHash]) -> Vec<String> {
    targets.iter().map(|h| h.as_str().to_owned()).collect()
}

/// `retracted(id)` := ∃ retraction targeting id (retractions are never
/// themselves retracted or scrubbed — EVENTS §3.5–3.6).
fn retracted_ids(events: &BTreeMap<EventId, Event>) -> HashSet<EventId> {
    events
        .values()
        .filter(|e| e.kind == Kind::Retraction)
        .filter_map(|e| e.target_event.clone())
        .collect()
}

/// `root(e)`: follow revision hops to a non-revision event; cycles and
/// dangling hops yield None (inert, EVENTS §6.1).
fn root_of(events: &BTreeMap<EventId, Event>, e: &Event) -> Option<EventId> {
    let mut seen: HashSet<EventId> = HashSet::new();
    let mut cur = e;
    while cur.kind == Kind::Revision {
        if !seen.insert(cur.id.clone()) {
            return None;
        }
        cur = events.get(cur.target_event.as_ref()?)?;
    }
    Some(cur.id.clone())
}

/// Effective text of a remark root: greatest-id live (non-retracted,
/// non-scrubbed) revision in its chain, else its own text. Returns
/// `(text, corrected)` — the same rule the store fold applies (§6.1).
fn effective_text(
    events: &BTreeMap<EventId, Event>,
    retracted: &HashSet<EventId>,
    root: &Event,
) -> (Option<String>, bool) {
    let mut best: Option<String> = None;
    for e in events.values() {
        // Ascending id iteration: the last live match is the greatest.
        if e.kind != Kind::Revision || retracted.contains(&e.id) || e.scrubbed() {
            continue;
        }
        if root_of(events, e).as_ref() == Some(&root.id)
            && let Some(t) = &e.text
        {
            best = Some(t.clone());
        }
    }
    match best {
        Some(t) => (Some(t), true),
        None => (root.text.clone(), false),
    }
}

/// A retracted content event as a flagged journal row (struck-through
/// behind the toggle). Scrubbed-and-retracted still shows only the stub.
fn retracted_row(
    events: &BTreeMap<EventId, Event>,
    retracted: &HashSet<EventId>,
    e: &Event,
) -> JournalEntryDto {
    let base = JournalEntryDto {
        id: e.id.as_str().to_owned(),
        session_id: e.session_id.as_str().to_owned(),
        ts: e.ts.to_rfc3339(),
        kind: "redacted",
        source: e.source.as_str(),
        text: None,
        original_text: None,
        corrected: false,
        retracted: true,
        rating: None,
        targets: hashes(&e.targets),
        linked_event: None,
    };
    if e.scrubbed() {
        return base;
    }
    match e.kind {
        Kind::Remark => {
            let (text, corrected) = effective_text(events, retracted, e);
            JournalEntryDto {
                kind: "remark",
                original_text: if corrected { e.text.clone() } else { None },
                text,
                corrected,
                ..base
            }
        }
        Kind::Rating => {
            let value = match &e.payload {
                Some(Payload::Rating { value }) => Some(*value),
                _ => None,
            };
            JournalEntryDto {
                kind: "rating",
                rating: value,
                ..base
            }
        }
        _ => JournalEntryDto {
            kind: "stroke",
            linked_event: e.linked_event.as_ref().map(|l| l.as_str().to_owned()),
            ..base
        },
    }
}

/// The full journal-tab row set for one image: the canonical fold's live
/// entries + retracted rows, merged in log order (ULID order, EVENTS §1.3).
pub(crate) fn journal_entries(
    store: &EventStore,
    image: &ContentHash,
) -> CmdResult<Vec<JournalEntryDto>> {
    let folded = store.folded_journal(image)?;
    let by_id: BTreeMap<EventId, Event> = store
        .events_for_image(image)?
        .into_iter()
        .map(|e| (e.id.clone(), e))
        .collect();
    let retracted = retracted_ids(&by_id);
    let raw = |id: &EventId| -> CmdResult<&Event> {
        by_id
            .get(id)
            .ok_or_else(|| CmdError::Invalid(format!("folded event missing from closure: {id}")))
    };

    let mut rows: Vec<JournalEntryDto> = Vec::with_capacity(folded.len());
    for entry in folded {
        rows.push(match entry {
            FoldedEntry::Remark {
                id,
                ts,
                source,
                targets,
                text,
                corrected,
            } => JournalEntryDto {
                session_id: raw(&id)?.session_id.as_str().to_owned(),
                // Pre-revision original, for the "edited" expand (UI §8.2).
                original_text: if corrected {
                    raw(&id)?.text.clone()
                } else {
                    None
                },
                id: id.as_str().to_owned(),
                ts: ts.to_rfc3339(),
                kind: "remark",
                source: source.as_str(),
                text: Some(text),
                corrected,
                retracted: false,
                rating: None,
                targets: hashes(&targets),
                linked_event: None,
            },
            FoldedEntry::Rating {
                id,
                ts,
                targets,
                value,
            } => JournalEntryDto {
                session_id: raw(&id)?.session_id.as_str().to_owned(),
                source: raw(&id)?.source.as_str(),
                id: id.as_str().to_owned(),
                ts: ts.to_rfc3339(),
                kind: "rating",
                text: None,
                original_text: None,
                corrected: false,
                retracted: false,
                rating: Some(value),
                targets: hashes(&targets),
                linked_event: None,
            },
            FoldedEntry::Stroke {
                id,
                ts,
                target,
                linked_event,
                ..
            } => JournalEntryDto {
                session_id: raw(&id)?.session_id.as_str().to_owned(),
                source: raw(&id)?.source.as_str(),
                id: id.as_str().to_owned(),
                ts: ts.to_rfc3339(),
                kind: "stroke",
                text: None,
                original_text: None,
                corrected: false,
                retracted: false,
                rating: None,
                targets: vec![target.as_str().to_owned()],
                linked_event: linked_event.map(|l| l.as_str().to_owned()),
            },
            FoldedEntry::RedactedStub { id, ts, source, .. } => JournalEntryDto {
                session_id: raw(&id)?.session_id.as_str().to_owned(),
                id: id.as_str().to_owned(),
                ts: ts.to_rfc3339(),
                kind: "redacted",
                source: source.as_str(),
                text: None,
                original_text: None,
                corrected: false,
                retracted: false,
                rating: None,
                targets: Vec::new(),
                linked_event: None,
            },
        });
    }
    // Retracted content rows fold out of folded_journal; the toggle needs
    // them. Same admission filter as the canonical fold (direct targets).
    for e in by_id.values() {
        if e.kind.is_content() && retracted.contains(&e.id) && e.targets.contains(image) {
            rows.push(retracted_row(&by_id, &retracted, e));
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Re-state (the retract-toast Undo — DECISIONS E4)
// ---------------------------------------------------------------------------

/// Append a NEW content event carrying the retracted event's folded
/// content, into the CURRENT session ("I said it again", not "I un-struck
/// it"). Returns the re-stated kind for the pulse, or None when there is
/// nothing to re-state (not retracted, scrubbed, or content gone).
pub(crate) fn restate(
    store: &EventStore,
    session: &SessionId,
    id: &EventId,
) -> CmdResult<Option<&'static str>> {
    let Some(root) = store.raw_event(id)? else {
        return Ok(None);
    };
    if !root.kind.is_content() {
        return Ok(None);
    }
    // The closure that holds the root's retractions and revisions.
    let closure = match root.targets.first() {
        Some(first) => store.events_for_image(first)?,
        None => store.sessionlevel_events(&root.session_id)?,
    };
    let is_retracted = closure
        .iter()
        .any(|e| e.kind == Kind::Retraction && e.target_event.as_ref() == Some(&root.id));
    if !is_retracted || root.scrubbed() {
        return Ok(None); // double-Undo is a no-op; scrubbed content is gone
    }
    let by_id: BTreeMap<EventId, Event> = closure.into_iter().map(|e| (e.id.clone(), e)).collect();
    match root.kind {
        Kind::Remark => {
            let retracted = retracted_ids(&by_id);
            let (text, _) = effective_text(&by_id, &retracted, &root);
            let Some(text) = text else { return Ok(None) };
            store.append(
                session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text,
                    targets: root.targets.clone(),
                },
                None,
            )?;
            Ok(Some("remark"))
        }
        Kind::Rating => {
            let Some(Payload::Rating { value }) = root.payload else {
                return Ok(None);
            };
            store.append(
                session,
                EventDraft::Rating {
                    value,
                    targets: root.targets.clone(),
                },
                None,
            )?;
            Ok(Some("rating"))
        }
        Kind::Stroke => {
            let Some(Payload::Stroke(payload)) = root.payload.clone() else {
                return Ok(None);
            };
            let Some(target) = root.targets.first() else {
                return Ok(None);
            };
            store.append(
                session,
                EventDraft::Stroke {
                    payload,
                    target: target.clone(),
                    linked_event: None,
                },
                None,
            )?;
            Ok(Some("stroke"))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Redaction report (EVENTS §7 / SIDECARS §11 → the §8.4 toast copy)
// ---------------------------------------------------------------------------

/// Run the full redaction (index scrub + immediate adjacent rewrite +
/// offline queueing) and shape the receipt for the UI: scrubbed event ids
/// (target + revision chain), sidecars rewritten NOW, and the labels of
/// offline volumes whose sidecars are scrubbed on next mount.
pub(crate) fn redact_report(
    store: &EventStore,
    library: &Library,
    engine: &SidecarEngine<'static, Arc<Library>>,
    id: &EventId,
    now: UtcMillis,
) -> CmdResult<RedactReportDto> {
    let receipt = engine.redact(id, now)?;
    // Chain members: each minted marker's target (EVENTS §3.6 closure).
    let mut redacted = Vec::with_capacity(receipt.markers.len());
    for marker in &receipt.markers {
        if let Some(r) = store.raw_event(marker)?
            && let Some(target) = r.target_event
        {
            redacted.push(target.as_str().to_owned());
        }
    }
    // Offline-pending volume labels, deduped, for the "— until that volume
    // next connects" copy. Falls back to the volume id when unlabeled.
    let mut volumes: BTreeSet<String> = BTreeSet::new();
    for (_event, image) in &receipt.queued {
        for row in library.paths_for_image(image)? {
            if !row.active {
                continue;
            }
            if let Some(v) = library.volume(&row.volume_id)?
                && !v.online
            {
                volumes.insert(v.label.unwrap_or(row.volume_id.clone()));
            }
        }
    }
    Ok(RedactReportDto {
        redacted,
        sidecars_updated: receipt.adjacent_rewritten.len() + receipt.session_journals.len(),
        offline_pending: volumes.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// Metadata (read-only, K16)
// ---------------------------------------------------------------------------

/// "64.14160° N, 21.92740° W" — text only; the UI never maps (K16).
fn gps_text(lat: Option<f64>, lon: Option<f64>) -> Option<String> {
    let (lat, lon) = (lat?, lon?);
    Some(format!(
        "{:.5}° {}, {:.5}° {}",
        lat.abs(),
        if lat >= 0.0 { "N" } else { "S" },
        lon.abs(),
        if lon >= 0.0 { "E" } else { "W" },
    ))
}

/// EXIF subset + file identity from the `images` row, best path, and the
/// display preview artifact state (the UI renders these read-only).
pub(crate) fn metadata_dto(library: &Library, hash: &ContentHash) -> CmdResult<ImageMetadataDto> {
    let rec = library
        .image(hash)?
        .ok_or_else(|| CmdError::Invalid(format!("unknown image: {}", hash.as_str())))?;
    let best = library.best_path(hash)?;
    let (rel_path, abs_path) = match &best {
        Some(bp) => {
            let abs = if bp.online {
                bp.mount_point.as_deref().map(|m| {
                    Path::new(m)
                        .join(&bp.row.rel_path)
                        .to_string_lossy()
                        .into_owned()
                })
            } else {
                None
            };
            (bp.row.rel_path.clone(), abs)
        }
        None => (String::new(), None),
    };
    let artifact = library.preview_artifact(hash, ArtifactKind::Display)?;
    Ok(ImageMetadataDto {
        hash: hash.as_str().to_owned(),
        file_name: rel_path.rsplit('/').next().unwrap_or("").to_owned(),
        rel_path,
        abs_path,
        byte_size: rec.byte_size,
        format: rec.format.as_str().to_owned(),
        pixel_width: rec.pixel_width,
        pixel_height: rec.pixel_height,
        orientation: rec.exif_orientation,
        capture_ts: rec.capture_ts,
        camera_make: rec.camera_make,
        camera_model: rec.camera_model,
        lens_model: rec.lens_model,
        focal_length_mm: rec.focal_length_mm,
        iso: rec.iso,
        f_number: rec.f_number,
        exposure_time: rec.exposure_time,
        gps: gps_text(rec.gps_lat, rec.gps_lon),
        preview_source: artifact.as_ref().map(|a| a.source.as_str().to_owned()),
        // No display artifact yet = the backfill is still pending.
        preview_pending: artifact.as_ref().is_none_or(|a| a.needs_full_decode),
        first_ingested_at: rec.first_ingested_at,
    })
}

// ---------------------------------------------------------------------------
// Commands (INTEGRATION registers these in lib.rs)
// ---------------------------------------------------------------------------

/// Folded per-image journal, retracted rows included + flagged (UI §8).
#[tauri::command]
pub async fn image_journal(app: S<'_>, hash: String) -> CmdResult<Vec<JournalEntryDto>> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || journal_entries(&app.store, &parse_hash(&hash)?))
        .await
        .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Read-only EXIF subset for the Metadata tab (K16).
#[tauri::command]
pub async fn image_metadata(app: S<'_>, hash: String) -> CmdResult<ImageMetadataDto> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || metadata_dto(&app.library, &parse_hash(&hash)?))
        .await
        .map_err(|e| CmdError::Invalid(format!("task join: {e}")))?
}

/// Inline Correct → revision event (EVENTS §3.4; the fold flips to the
/// corrected text). Empty/whitespace text commits nothing.
#[tauri::command]
pub fn revise_event(
    app: S<'_>,
    handle: AppHandle,
    event_id: String,
    text: String,
) -> CmdResult<bool> {
    app.touch()?;
    let Some(text) = normalize_note(&text) else {
        return Ok(false);
    };
    let target = EventId::from_str_strict(&event_id)?;
    let session = app.session_id();
    app.store
        .append(&session, EventDraft::Revision { target, text }, None)?;
    emit_pulse(&handle, "revision");
    Ok(true)
}

/// Retract → tombstone (EVENTS §3.5, source=system: the journal-panel
/// button). The frontend's toast offers Undo (= re-state, E4).
#[tauri::command]
pub fn retract_event(app: S<'_>, handle: AppHandle, event_id: String) -> CmdResult<bool> {
    app.touch()?;
    let target = EventId::from_str_strict(&event_id)?;
    let session = app.session_id();
    app.store.append(
        &session,
        EventDraft::Retraction {
            target,
            source: RetractionSource::System,
        },
        None,
    )?;
    emit_pulse(&handle, "retraction");
    Ok(true)
}

/// The retract-toast "Undo": RE-STATE — a NEW event carrying the folded
/// content (retraction-of-retraction is spec-forbidden, DECISIONS E4).
#[tauri::command]
pub fn unretract_event(app: S<'_>, handle: AppHandle, event_id: String) -> CmdResult<bool> {
    app.touch()?;
    let id = EventId::from_str_strict(&event_id)?;
    let session = app.session_id();
    match restate(&app.store, &session, &id)? {
        Some(kind) => {
            emit_pulse(&handle, kind);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The one modal's commit: scrub everywhere (EVENTS §7; redaction wins).
#[tauri::command]
pub async fn redact_event(
    app: S<'_>,
    handle: AppHandle,
    event_id: String,
) -> CmdResult<RedactReportDto> {
    let app = app.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let id = EventId::from_str_strict(&event_id)?;
        redact_report(&app.store, &app.library, &app.engine, &id, UtcMillis::now())
    })
    .await
    .map_err(|e| CmdError::Invalid(format!("task join: {e}")))??;
    emit_pulse(&handle, "redaction");
    Ok(report)
}

// ---------------------------------------------------------------------------
// Tests — journal folds against a temp EventStore; metadata + redaction
// against a temp Library/SidecarEngine with an injected volume probe
// (state.rs / os.rs fixture pattern).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use photoproof_core::SessionContext;
    use photoproof_core::library::{
        FakeVolumeProbe, LibraryOptions, PlatformIdKind, ProbedVolume, ScanOptions,
    };

    use super::*;

    fn session_ctx() -> SessionContext {
        SessionContext {
            app_version: "0.0.1-test".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        }
    }

    /// Store-only fixture: journal folds need no library.
    fn store_fixture() -> (tempfile::TempDir, EventStore, SessionId, ContentHash) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = EventStore::open(tmp.path().join("photoproof.db")).expect("open store");
        let session = store.open_session(session_ctx()).expect("open session");
        let hash = ContentHash::from_hex(&"ab".repeat(32)).expect("hash");
        (tmp, store, session, hash)
    }

    fn remark(
        store: &EventStore,
        session: &SessionId,
        text: &str,
        targets: Vec<ContentHash>,
    ) -> EventId {
        store
            .append(
                session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: text.into(),
                    targets,
                },
                None,
            )
            .expect("append remark")
            .id
    }

    fn retract(store: &EventStore, session: &SessionId, target: &EventId) {
        store
            .append(
                session,
                EventDraft::Retraction {
                    target: target.clone(),
                    source: RetractionSource::System,
                },
                None,
            )
            .expect("append retraction");
    }

    #[test]
    fn folded_rows_carry_session_corrected_text_and_the_original() {
        let (_tmp, store, session, hash) = store_fixture();
        let id = remark(&store, &session, "muddy light", vec![hash.clone()]);
        store
            .append(
                &session,
                EventDraft::Revision {
                    target: id.clone(),
                    text: "moody light".into(),
                },
                None,
            )
            .unwrap();
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.len(), 1, "revisions are never standalone entries");
        let row = &rows[0];
        assert_eq!(row.id, id.as_str());
        assert_eq!(row.session_id, session.as_str());
        assert_eq!(row.kind, "remark");
        assert_eq!(row.text.as_deref(), Some("moody light"));
        assert!(row.corrected);
        assert_eq!(row.original_text.as_deref(), Some("muddy light"));
        assert!(!row.retracted);
    }

    #[test]
    fn rating_rows_fold_with_value_and_log_order_holds() {
        let (_tmp, store, session, hash) = store_fixture();
        let a = remark(&store, &session, "keeper", vec![hash.clone()]);
        let b = store
            .append(
                &session,
                EventDraft::Rating {
                    value: 4,
                    targets: vec![hash.clone()],
                },
                None,
            )
            .unwrap()
            .id;
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![a.as_str(), b.as_str()],
            "ULID order is log order"
        );
        assert_eq!(rows[1].kind, "rating");
        assert_eq!(rows[1].rating, Some(4));
        assert_eq!(rows[1].text, None);
    }

    #[test]
    fn retracted_rows_are_included_and_flagged_with_their_folded_text() {
        let (_tmp, store, session, hash) = store_fixture();
        let id = remark(&store, &session, "too literal", vec![hash.clone()]);
        store
            .append(
                &session,
                EventDraft::Revision {
                    target: id.clone(),
                    text: "far too literal".into(),
                },
                None,
            )
            .unwrap();
        retract(&store, &session, &id);
        // The canonical fold hides it entirely…
        assert!(store.folded_journal(&hash).unwrap().is_empty());
        // …the journal-tab rows keep it, flagged, with the folded text.
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].retracted);
        assert_eq!(rows[0].kind, "remark");
        assert_eq!(rows[0].text.as_deref(), Some("far too literal"));
        assert!(rows[0].corrected);
    }

    #[test]
    fn redacted_events_fold_to_inert_stubs() {
        let (_tmp, store, session, hash) = store_fixture();
        let id = remark(&store, &session, "the secret", vec![hash.clone()]);
        store.redact(&id).unwrap();
        let rows = journal_entries(&store, &hash).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "redacted");
        assert_eq!(rows[0].text, None, "content gone, continuity preserved");
        assert!(!rows[0].retracted);
    }

    #[test]
    fn restate_appends_a_new_remark_carrying_the_folded_text() {
        let (_tmp, store, session, hash) = store_fixture();
        let id = remark(&store, &session, "muddy", vec![hash.clone()]);
        store
            .append(
                &session,
                EventDraft::Revision {
                    target: id.clone(),
                    text: "moody".into(),
                },
                None,
            )
            .unwrap();
        retract(&store, &session, &id);
        let kind = restate(&store, &session, &id).unwrap();
        assert_eq!(kind, Some("remark"));
        let rows = journal_entries(&store, &hash).unwrap();
        // The retracted original stays retracted; a NEW live remark exists
        // with the folded text and the same targets (E4: re-state, never
        // un-retract).
        let live: Vec<_> = rows.iter().filter(|r| !r.retracted).collect();
        assert_eq!(live.len(), 1);
        assert_ne!(live[0].id, id.as_str());
        assert_eq!(live[0].text.as_deref(), Some("moody"));
        assert_eq!(live[0].targets, vec![hash.as_str().to_owned()]);
        assert!(rows.iter().any(|r| r.retracted && r.id == id.as_str()));
    }

    #[test]
    fn restate_declines_unretracted_and_scrubbed_targets() {
        let (_tmp, store, session, hash) = store_fixture();
        let live = remark(&store, &session, "still here", vec![hash.clone()]);
        assert_eq!(restate(&store, &session, &live).unwrap(), None);
        let gone = remark(&store, &session, "secret", vec![hash.clone()]);
        retract(&store, &session, &gone);
        store.redact(&gone).unwrap();
        assert_eq!(
            restate(&store, &session, &gone).unwrap(),
            None,
            "scrubbed content cannot be re-stated"
        );
    }

    #[test]
    fn restate_resurfaces_a_retracted_rating() {
        let (_tmp, store, session, hash) = store_fixture();
        for value in [3u8, 4] {
            store
                .append(
                    &session,
                    EventDraft::Rating {
                        value,
                        targets: vec![hash.clone()],
                    },
                    None,
                )
                .unwrap();
        }
        let rows = journal_entries(&store, &hash).unwrap();
        let four = rows
            .iter()
            .find(|r| r.rating == Some(4))
            .unwrap()
            .id
            .clone();
        let four = EventId::from_str_strict(&four).unwrap();
        retract(&store, &session, &four);
        assert_eq!(store.current_rating(&hash).unwrap(), Some(3));
        assert_eq!(restate(&store, &session, &four).unwrap(), Some("rating"));
        assert_eq!(
            store.current_rating(&hash).unwrap(),
            Some(4),
            "the re-stated rating wins the fold (last live rating)"
        );
    }

    // ---- metadata + redaction over a temp Library/engine ------------------

    struct Env {
        _tmp: tempfile::TempDir,
        store: Arc<EventStore>,
        lib: Arc<Library>,
        engine: SidecarEngine<'static, Arc<Library>>,
        probe: FakeVolumeProbe,
        session: SessionId,
        file: PathBuf,
    }

    fn probed(mount: &Path) -> ProbedVolume {
        ProbedVolume {
            mount_point: mount.to_path_buf(),
            platform_id: Some("uuid-journal-test".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("ShootDisk".into()),
            fs_type: Some("ext4".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }
    }

    /// Full-stack fixture (m1env pattern, minimal): one scanned root with
    /// one file; the bytes need not decode — hashing does not care, and
    /// the absent preview artifact is itself an assertion target.
    fn env() -> Env {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        let dir = mount.join("shoot");
        std::fs::create_dir_all(&dir).unwrap();
        let app_data = tmp.path().join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let file = dir.join("IMG_0001.jpg");
        std::fs::write(&file, b"not a real jpeg, hashing does not care").unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let db = app_data.join("photoproof.db");
        let store = Arc::new(EventStore::open(&db).expect("open store"));
        let lib = Arc::new(
            Library::open_with(
                &db,
                app_data.join("previews"),
                LibraryOptions {
                    probe: Arc::new(probe.clone()),
                    ..Default::default()
                },
            )
            .expect("open library"),
        );
        let engine = SidecarEngine::new_shared(store.clone(), &db, &app_data, lib.clone())
            .expect("open engine");
        let root_id = lib.register_root(&dir, Some("shoot")).unwrap();
        lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
        let session = store.open_session(session_ctx()).expect("open session");
        Env {
            _tmp: tmp,
            store,
            lib,
            engine,
            probe,
            session,
            file,
        }
    }

    fn only_hash(lib: &Library) -> ContentHash {
        let hashes = lib.image_hashes().unwrap();
        assert_eq!(hashes.len(), 1, "the scan ingests exactly one image");
        hashes[0].clone()
    }

    #[test]
    fn metadata_maps_file_identity_paths_and_preview_state() {
        let env = env();
        let hash = only_hash(&env.lib);
        let dto = metadata_dto(&env.lib, &hash).unwrap();
        assert_eq!(dto.hash, hash.as_str());
        assert_eq!(dto.file_name, "IMG_0001.jpg");
        assert_eq!(dto.rel_path, "shoot/IMG_0001.jpg"); // volume-relative
        assert_eq!(dto.abs_path.as_deref(), env.file.to_str());
        assert_eq!(dto.format, "jpeg");
        assert!(dto.byte_size > 0);
        assert_eq!(dto.orientation, 1); // EXIF absent → 1 (LIBRARY)
        assert_eq!(dto.gps, None);
        // No display artifact yet → the backfill is pending.
        assert_eq!(dto.preview_source, None);
        assert!(dto.preview_pending);
        assert!(metadata_dto(&env.lib, &ContentHash::from_hex(&"0".repeat(64)).unwrap()).is_err());
    }

    #[test]
    fn gps_formats_hemispheres_as_text() {
        assert_eq!(
            gps_text(Some(64.1416), Some(-21.9274)).as_deref(),
            Some("64.14160° N, 21.92740° W")
        );
        assert_eq!(
            gps_text(Some(-33.8688), Some(151.2093)).as_deref(),
            Some("33.86880° S, 151.20930° E")
        );
        assert_eq!(gps_text(Some(1.0), None), None);
    }

    #[test]
    fn redaction_report_counts_rewritten_sidecars_online() {
        let env = env();
        let hash = only_hash(&env.lib);
        let id = remark(&env.store, &env.session, "scrub me", vec![hash.clone()]);
        let rev = env
            .store
            .append(
                &env.session,
                EventDraft::Revision {
                    target: id.clone(),
                    text: "scrub me too".into(),
                },
                None,
            )
            .unwrap()
            .id;
        env.engine.flush_all(UtcMillis::now()).unwrap(); // adjacent sidecar exists
        let report =
            redact_report(&env.store, &env.lib, &env.engine, &id, UtcMillis::now()).unwrap();
        // Chain closure: the remark AND its revision are scrubbed (§3.6).
        assert_eq!(report.redacted.len(), 2);
        assert!(report.redacted.contains(&id.as_str().to_owned()));
        assert!(report.redacted.contains(&rev.as_str().to_owned()));
        assert_eq!(report.sidecars_updated, 1, "adjacent rewrite is immediate");
        assert!(report.offline_pending.is_empty());
        // The journal shows the stub afterwards.
        let rows = journal_entries(&env.store, &hash).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "redacted");
    }

    #[test]
    fn redaction_on_an_offline_volume_reports_the_pending_label() {
        let env = env();
        let hash = only_hash(&env.lib);
        let id = remark(
            &env.store,
            &env.session,
            "offline secret",
            vec![hash.clone()],
        );
        env.engine.flush_all(UtcMillis::now()).unwrap();
        env.probe.set_mounts(vec![]); // disk pulled
        env.lib.probe_volumes().unwrap();
        let report =
            redact_report(&env.store, &env.lib, &env.engine, &id, UtcMillis::now()).unwrap();
        assert_eq!(report.sidecars_updated, 0, "nothing reachable to rewrite");
        assert_eq!(report.offline_pending, vec!["ShootDisk".to_owned()]);
        // The pending queue holds the (event, image) pair for next mount.
        assert!(!env.engine.redaction_queue().unwrap().is_empty());
    }
}
