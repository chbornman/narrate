//! One-click full export (spec/SIDECARS.md §12.1–§12.2).
//!
//! Exported sidecars are serialized **from the index** in canonical form —
//! the index is, by the merge invariant, the union of every journal the app
//! has ever read — and are hash-named like overflow entries. Only annotated
//! images export. Export output *is* the rebuild input (one format, three
//! roles), and the export always includes the entire overflow truth and all
//! session journals (§8.3, the honesty caveat).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::id::UtcMillis;

use super::doc::{FORMAT_MANIFEST, SIDECAR_SCHEMA_VERSION};
use super::engine::{ImageLocator, SidecarEngine, SidecarError};
use super::paths::{SIDECAR_SUFFIX, fanout_rel_path};
use super::serialize::to_pretty_canonical;
use super::writer::write_atomic;

/// §12.2: manifest schema version (versioned independently of the sidecar
/// schema).
pub const MANIFEST_SCHEMA_VERSION: u64 = 1;

/// §12.2 `volumes` entry — context for humans re-locating images, never
/// identity. Volume identity is owned by spec/LIBRARY.md (packet P2.2);
/// this is the narrow seam the library layer fills in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub id: String,
    pub label: String,
    /// UTC RFC 3339.
    pub last_seen: String,
}

/// Receipt of one export run.
#[derive(Debug)]
pub struct ExportReport {
    pub dir: PathBuf,
    pub manifest_path: PathBuf,
    pub images: usize,
    pub events: usize,
    pub sessions: usize,
    pub redactions: usize,
    /// Absolute paths of the exported per-image sidecars.
    pub sidecars: Vec<PathBuf>,
    /// Absolute paths of the exported session journals.
    pub session_journals: Vec<PathBuf>,
}

impl<L: ImageLocator> SidecarEngine<'_, L> {
    /// §12.1: produce a full export under `dest` (the caller picks the
    /// directory; `paths::default_export_dir_name` gives the default name).
    /// Explicit export is an immediate-flush trigger (§9.1), so the live
    /// write targets are drained first.
    pub fn export(
        &self,
        dest: &Path,
        app_version: &str,
        now: UtcMillis,
        volumes: &[VolumeInfo],
    ) -> Result<ExportReport, SidecarError> {
        // §9.1: explicit export bypasses the debounce.
        self.flush_all(now)?;
        for sid in self.session_journal_owners()? {
            self.flush_session(&sid)?;
        }
        fs::create_dir_all(dest)?;

        // sidecars/<h0..2>/<h2..4>/<hash>.photoproof.json — one per
        // annotated image (no journal → no file).
        let mut manifest_images: Vec<Value> = Vec::new();
        let mut sidecars = Vec::new();
        for hash in self.annotated_images()? {
            let doc = self.image_doc(&hash)?;
            let rel = Path::new("sidecars").join(fanout_rel_path(&hash));
            let path = dest.join(&rel);
            write_atomic(&path, &doc.to_bytes())?;

            let mut entry = Map::new();
            entry.insert("hash".into(), Value::String(hash.as_str().to_owned()));
            let filenames: Vec<Value> = doc
                .image
                .as_ref()
                .map(|s| s.filename.clone())
                .filter(|f| !f.is_empty())
                .into_iter()
                .map(Value::String)
                .collect();
            entry.insert("filenames".into(), Value::Array(filenames));
            entry.insert(
                "byte_size".into(),
                Value::Number(doc.image.as_ref().map(|s| s.byte_size).unwrap_or(0).into()),
            );
            entry.insert(
                "event_count".into(),
                Value::Number((doc.events.len() as u64).into()),
            );
            entry.insert("sidecar".into(), Value::String(rel_path_string(&rel)));
            manifest_images.push(Value::Object(entry));
            sidecars.push(path);
        }

        // sessions/<session-ulid>.photoproof.json — session journals (§7.2).
        let mut manifest_sessions: Vec<Value> = Vec::new();
        let mut session_journals = Vec::new();
        for sid in self.session_journal_owners()? {
            let Some(doc) = self.session_doc(&sid)? else {
                continue; // no zero-target events → no file (§7.2)
            };
            let rel = Path::new("sessions").join(format!("{}{SIDECAR_SUFFIX}", sid.as_str()));
            let path = dest.join(&rel);
            write_atomic(&path, &doc.to_bytes())?;

            let mut entry = Map::new();
            entry.insert("session".into(), Value::String(sid.as_str().to_owned()));
            entry.insert("path".into(), Value::String(rel_path_string(&rel)));
            manifest_sessions.push(Value::Object(entry));
            session_journals.push(path);
        }

        // §12.2 counts.
        let events = self.count("SELECT COUNT(*) FROM annotation_events")?.max(0) as usize;
        let sessions = self
            .count("SELECT COUNT(*) FROM (SELECT DISTINCT session_id FROM annotation_events)")?
            .max(0) as usize;
        let redactions = self.count("SELECT COUNT(*) FROM redactions")?.max(0) as usize;

        let mut counts = Map::new();
        counts.insert(
            "images".into(),
            Value::Number((manifest_images.len() as u64).into()),
        );
        counts.insert("events".into(), Value::Number((events as u64).into()));
        counts.insert("sessions".into(), Value::Number((sessions as u64).into()));
        counts.insert(
            "redactions".into(),
            Value::Number((redactions as u64).into()),
        );

        let mut manifest = Map::new();
        manifest.insert("format".into(), Value::String(FORMAT_MANIFEST.to_owned()));
        manifest.insert(
            "schema_version".into(),
            Value::Number(MANIFEST_SCHEMA_VERSION.into()),
        );
        manifest.insert(
            "sidecar_schema_version".into(),
            Value::Number(SIDECAR_SCHEMA_VERSION.into()),
        );
        manifest.insert("app_version".into(), Value::String(app_version.to_owned()));
        // The manifest is metadata, not journal truth — it is allowed this
        // one nondeterminism (§12.2).
        manifest.insert("exported_at".into(), Value::String(now.to_rfc3339()));
        manifest.insert("counts".into(), Value::Object(counts));
        manifest.insert("images".into(), Value::Array(manifest_images));
        manifest.insert("session_journals".into(), Value::Array(manifest_sessions));
        manifest.insert(
            "volumes".into(),
            Value::Array(
                volumes
                    .iter()
                    .map(|v| {
                        let mut m = Map::new();
                        m.insert("id".into(), Value::String(v.id.clone()));
                        m.insert("label".into(), Value::String(v.label.clone()));
                        m.insert("last_seen".into(), Value::String(v.last_seen.clone()));
                        Value::Object(m)
                    })
                    .collect(),
            ),
        );

        let manifest_path = dest.join(format!("manifest{SIDECAR_SUFFIX}"));
        write_atomic(
            &manifest_path,
            &to_pretty_canonical(&Value::Object(manifest)),
        )?;

        Ok(ExportReport {
            dir: dest.to_path_buf(),
            manifest_path,
            images: sidecars.len(),
            events,
            sessions,
            redactions,
            sidecars,
            session_journals,
        })
    }
}

/// Manifest paths are relative with `/` separators on every platform
/// (deterministic bytes, §12.2 example form).
fn rel_path_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
