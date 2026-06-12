//! Rebuild-from-sidecars (spec/SIDECARS.md §12.3) — the proof of K11:
//! sidecars are the canonical truth; SQLite is a rebuildable index.
//!
//! Accepts either an export directory or the live world (watched roots +
//! overflow + session journals). The manifest, when present, is advisory —
//! a cross-check, never a requirement.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::canonical;
use crate::event::{Event, Kind};
use crate::id::{ContentHash, EventId, SessionId, UtcMillis};

use super::doc::{ParsedSidecar, SidecarDoc, parse_sidecar};
use super::engine::{
    ImageLocator, IntegrityReport, SidecarEngine, SidecarError, Unparseable, classify_unparseable,
    walk_sidecar_files,
};
use super::paths::{overflow_dir, sessions_dir};
use super::writer::write_atomic;

/// What to rebuild from.
#[derive(Debug, Clone, Default)]
pub struct RebuildOptions {
    /// Directory roots to scan: an export directory, or the watched library
    /// roots for a live-world rebuild.
    pub roots: Vec<PathBuf>,
    /// Also scan the engine's app-data overflow store and session-journal
    /// directory (the live-world case; an export already contains both).
    pub include_app_data: bool,
    /// §12.3 step 8: post-rebuild convergence rewrites any scanned sidecar
    /// that is now a subset of the union. Disable for read-only inputs.
    pub converge: bool,
}

impl RebuildOptions {
    /// Rebuild from an export directory (self-contained; no app-data scan,
    /// no rewrites of the export files).
    pub fn export_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![dir.into()],
            include_app_data: false,
            converge: false,
        }
    }

    /// Live-world rebuild: watched roots + overflow + session journals,
    /// converging files to the union afterwards.
    pub fn live(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            include_app_data: true,
            converge: true,
        }
    }
}

/// §12.3. The engine must wrap a **fresh** store (the caller deletes the old
/// database; rebuild never deletes rows). Returns the integrity report —
/// always produced, never silent.
pub fn rebuild_from_sidecars<L: ImageLocator>(
    engine: &SidecarEngine<'_, L>,
    opts: &RebuildOptions,
    now: UtcMillis,
) -> Result<IntegrityReport, SidecarError> {
    let mut report = IntegrityReport::default();

    // -- Step 1: scan ---------------------------------------------------------
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &opts.roots {
        files.extend(walk_sidecar_files(root)?);
    }
    if opts.include_app_data {
        files.extend(walk_sidecar_files(&overflow_dir(engine.app_data()))?);
        files.extend(walk_sidecar_files(&sessions_dir(engine.app_data()))?);
    }
    files.sort();
    files.dedup();
    // `collections.photoproof.json` (RETRIEVAL §10.2, P7.3) sits beside the
    // manifest in every export directory and matches the sidecar suffix,
    // but it is not event truth: crate::collections owns its parse and
    // union-merge, so the files are split out of the event scan here and
    // consumed through that module below (§10.2 is normative: the export's
    // collections file is "consumed by rebuild-from-sidecars" — a fresh
    // machine restoring from the one-click export gets its collections back
    // from THIS path, not from app data it does not have).
    let mut collections_files: Vec<PathBuf> = Vec::new();
    files.retain(|p| {
        let is_collections = p
            .file_name()
            .map(|n| n.eq_ignore_ascii_case(crate::collections::COLLECTIONS_FILENAME))
            .unwrap_or(false);
        if is_collections {
            collections_files.push(p.clone());
        }
        !is_collections
    });

    // -- Step 2: parse & validate (failures quarantined, nothing aborts) ------
    let mut docs: Vec<(PathBuf, SidecarDoc, Vec<u8>)> = Vec::new();
    let mut manifests: Vec<(PathBuf, Value)> = Vec::new();
    for path in files {
        report.files_scanned += 1;
        let bytes = fs::read(&path)?;
        match parse_sidecar(&bytes) {
            Ok(ParsedSidecar::Doc(doc)) => {
                report.files_parsed += 1;
                docs.push((path, *doc, bytes));
            }
            Ok(ParsedSidecar::NewerVersion { .. }) => {
                // §5.3: opaque and inviolable; counted, never imported.
                report.newer_version_files.push(path);
            }
            Ok(ParsedSidecar::Manifest) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => manifests.push((path, v)),
                Err(e) => report.failures.push((path, e.to_string())),
            },
            Err(e) => match classify_unparseable(&bytes, &e) {
                Unparseable::Collision => report.collisions.push(path),
                // Rebuild quarantines + reports; renaming aside is the live
                // reconciliation's job (§10.3), not the rebuild's.
                Unparseable::Corrupt => report.failures.push((path, e.to_string())),
            },
        }
    }

    // -- Step 3 (pass 1): redaction registry first — supremacy in force
    //    before any content lands.
    let mut markers: BTreeMap<EventId, Event> = BTreeMap::new();
    for (_, doc, _) in &docs {
        for e in doc.canonical_events() {
            if e.kind == Kind::Redaction {
                markers.entry(e.id.clone()).or_insert(e);
            }
        }
    }
    let marker_events: Vec<Event> = markers.into_values().collect();
    let mrep = engine.store.merge(&marker_events)?;
    report.events_imported += mrep.inserted;
    for (offset, reason) in &mrep.quarantined {
        report
            .quarantined
            .push((PathBuf::from("<rebuild pass 1>"), *offset, reason.clone()));
    }

    // -- Step 4 (pass 2): union by event id across all copies, with
    //    redaction supremacy (scrubbed beats unscrubbed) and the §10.2
    //    deterministic conflict rule (byte-wise lesser canonical wins;
    //    loser preserved verbatim in the report).
    let mut union: BTreeMap<EventId, (Event, Vec<u8>)> = BTreeMap::new();
    for (_, doc, _) in &docs {
        for e in doc.canonical_events() {
            if e.kind == Kind::Redaction {
                continue; // pass 1
            }
            let bytes = canonical::canonical_json(&e);
            match union.get_mut(&e.id) {
                None => {
                    union.insert(e.id.clone(), (e, bytes));
                }
                Some((kept, kept_bytes)) => {
                    if bytes == *kept_bytes {
                        report.duplicates_deduped += 1;
                        continue;
                    }
                    // Conflict. Decide the winner deterministically.
                    let incoming_wins = match (kept.scrubbed(), e.scrubbed()) {
                        (true, false) => false, // scrubbed copy beats content
                        (false, true) => true,
                        _ => bytes < *kept_bytes, // byte-wise lesser wins
                    };
                    report.id_conflicts.push(e.id.to_string());
                    if incoming_wins {
                        report.conflict_losers.push((
                            e.id.to_string(),
                            String::from_utf8(kept_bytes.clone()).expect("canonical JSON is UTF-8"),
                        ));
                        *kept = e;
                        *kept_bytes = bytes;
                    } else {
                        report.conflict_losers.push((
                            e.id.to_string(),
                            String::from_utf8(bytes).expect("canonical JSON is UTF-8"),
                        ));
                    }
                }
            }
        }
    }

    // -- Steps 5–6: insert. `EventStore::merge` owns the insertion
    //    discipline (ascending-id sort, ~10k-event transactions, FTS +
    //    derived in a single pass after the union, ANALYZE / FTS optimize /
    //    wal_checkpoint for large merges) per EVENTS §8.
    let events: Vec<Event> = union.into_values().map(|(e, _)| e).collect();
    let mrep = engine.store.merge(&events)?;
    report.events_imported += mrep.inserted;
    report.duplicates_deduped += mrep.duplicates;
    report.redactions_enforced += mrep.scrubbed_on_arrival + mrep.newly_scrubbed;
    for id in &mrep.integrity_warnings {
        report.id_conflicts.push(id.to_string());
    }
    for (offset, reason) in &mrep.quarantined {
        report
            .quarantined
            .push((PathBuf::from("<rebuild pass 2>"), *offset, reason.clone()));
    }

    // Side tables per document: sessions (first-seen wins, mismatches
    // reported), advisory snapshots, unknown fields, opaque entries.
    for (_, doc, _) in &docs {
        engine.ingest_side_tables(doc, &mut report)?;
    }

    // -- Step 7: manifest cross-check (advisory, never a requirement).
    for (mpath, manifest) in &manifests {
        cross_check_manifest(mpath, manifest, &mut report);
    }

    // -- Collections (RETRIEVAL §10.2): union-merge every scanned
    //    `collections.photoproof.json` into the same database the engine
    //    wraps. Failures are reported, never aborting — the event rebuild
    //    above already succeeded.
    if !collections_files.is_empty() {
        import_collections_files(engine, collections_files, now, &mut report);
    }

    // -- Step 8: post-rebuild convergence — rewrite any scanned sidecar now
    //    a subset of the union; the dirty queue then drains to empty by
    //    construction (DB == sidecars).
    if opts.converge {
        let mut verified: BTreeMap<ContentHash, bool> = BTreeMap::new();
        for (path, doc, original) in &docs {
            match &doc.image {
                Some(snap) => {
                    let canonical_bytes = engine.image_doc(&snap.hash)?.to_bytes();
                    let ok = if canonical_bytes == *original {
                        true
                    } else {
                        match write_atomic(path, &canonical_bytes) {
                            Ok(_) => {
                                report.converged_rewrites.push(path.clone());
                                true
                            }
                            Err(e) => {
                                report.failures.push((path.clone(), e.to_string()));
                                false
                            }
                        }
                    };
                    let entry = verified.entry(snap.hash.clone()).or_insert(true);
                    *entry &= ok;
                }
                None => {
                    let Some(sid) = session_owner_of(doc) else {
                        continue;
                    };
                    let Some(canon) = engine.session_doc(&sid)? else {
                        continue;
                    };
                    let canonical_bytes = canon.to_bytes();
                    if canonical_bytes != *original {
                        match write_atomic(path, &canonical_bytes) {
                            Ok(_) => report.converged_rewrites.push(path.clone()),
                            Err(e) => report.failures.push((path.clone(), e.to_string())),
                        }
                    }
                }
            }
        }
        for (hash, ok) in verified {
            if ok {
                engine.ack_dirty_row(&hash)?;
            }
        }
    }

    Ok(report)
}

/// RETRIEVAL §10.2: import scanned collections files through the
/// collections store (its parse + union-merge are the ONLY implementation
/// of the §10.2 rules — the rebuild never re-states them). Opening the
/// store over the engine's database also reconciles/converges the app-data
/// mirror, so the imported union lands there too.
fn import_collections_files<L: ImageLocator>(
    engine: &SidecarEngine<'_, L>,
    paths: Vec<PathBuf>,
    now: UtcMillis,
    report: &mut IntegrityReport,
) {
    use crate::collections::{Collections, CollectionsError};

    let collections = match Collections::open(engine.db_path(), engine.app_data()) {
        Ok(c) => c,
        Err(e) => {
            // Surface the failure on every file the user expected imported.
            for path in paths {
                report
                    .failures
                    .push((path, format!("collections store open failed: {e}")));
            }
            return;
        }
    };
    for path in paths {
        match collections.import_file(&path, now) {
            Ok(outcome) => {
                for c in outcome.note_conflicts {
                    report.collection_note_conflicts.push((
                        c.note_id,
                        format!(
                            "collection {}: losing copy ts={} text={:?}",
                            c.collection_id, c.loser.ts, c.loser.text
                        ),
                    ));
                }
                report.collections_files_imported.push(path);
            }
            // §5.3: opaque and inviolable; counted, never imported.
            Err(CollectionsError::NewerVersion(_)) => report.newer_version_files.push(path),
            Err(e) => report.failures.push((path, e.to_string())),
        }
    }
    // Mirror the merged union out to app data now rather than leaving it
    // to the next mutation; a lockout (newer-version app-data file owns
    // the mirror path) surfaces as a reported failure, never silently.
    if let Err(e) = collections.flush(now) {
        report
            .failures
            .push((engine.app_data().to_path_buf(), e.to_string()));
    }
}

/// The owning session of a session journal (`image: null`): the first
/// `sessions` key, else the first event's `session_id`.
fn session_owner_of(doc: &SidecarDoc) -> Option<SessionId> {
    doc.sessions
        .keys()
        .find_map(|k| SessionId::from_str_strict(k).ok())
        .or_else(|| {
            doc.canonical_events()
                .first()
                .map(|e| SessionId::from_str_strict(e.session_id.as_str()).expect("valid ULID"))
        })
}

/// §12.3 step 7: manifest discrepancies — missing/extra files, count
/// mismatches. Purely advisory.
fn cross_check_manifest(mpath: &Path, manifest: &Value, report: &mut IntegrityReport) {
    let dir = mpath.parent().unwrap_or_else(|| Path::new("."));
    let label = mpath.display();

    let images = manifest
        .get("images")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(counted) = manifest.pointer("/counts/images").and_then(Value::as_u64)
        && counted as usize != images.len()
    {
        report.manifest_discrepancies.push(format!(
            "{label}: counts.images = {counted} but images lists {}",
            images.len()
        ));
    }

    let mut listed: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in &images {
        let Some(rel) = entry.get("sidecar").and_then(Value::as_str) else {
            continue;
        };
        let p = dir.join(rel);
        listed.insert(p.clone());
        if !p.is_file() {
            report
                .manifest_discrepancies
                .push(format!("{label}: missing sidecar {rel}"));
        }
    }
    for entry in manifest
        .get("session_journals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(rel) = entry.get("path").and_then(Value::as_str) else {
            continue;
        };
        let p = dir.join(rel);
        listed.insert(p.clone());
        if !p.is_file() {
            report
                .manifest_discrepancies
                .push(format!("{label}: missing session journal {rel}"));
        }
    }

    // Extra files present beside the manifest but unlisted.
    for sub in ["sidecars", "sessions"] {
        let root = dir.join(sub);
        if !root.is_dir() {
            continue;
        }
        if let Ok(found) = walk_sidecar_files(&root) {
            for f in found {
                if !listed.contains(&f) {
                    report
                        .manifest_discrepancies
                        .push(format!("{label}: file not in manifest: {}", f.display()));
                }
            }
        }
    }
}
