//! The sidecar document: model, canonical serialization, validation.
//!
//! Contract: spec/SIDECARS.md §3–§5. One format, three roles: the identical
//! document is the adjacent sidecar, the overflow-store entry, and the unit
//! of export.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::canonical;
use crate::event::Event;
use crate::id::{ContentHash, UtcMillis};

use super::serialize::{to_compact_canonical, to_pretty_canonical};

/// §3: positive identification marker. A file without it is not a sidecar.
pub const FORMAT_SIDECAR: &str = "photoproof-sidecar";
/// §12.2 manifest marker.
pub const FORMAT_MANIFEST: &str = "photoproof-export-manifest";
/// §3/§5.1: current sidecar schema version.
pub const SIDECAR_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Error)]
pub enum SidecarParseError {
    #[error("not valid UTF-8 JSON: {0}")]
    Json(String),
    #[error("top level is not a JSON object")]
    NotObject,
    #[error("missing or wrong `format` marker; not a Photoproof sidecar")]
    NotASidecar,
    #[error("invalid sidecar field {0}: {1}")]
    Invalid(&'static str, String),
}

/// §3.1: identity + advisory re-match snapshot. Hash always wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSnapshot {
    pub hash: ContentHash,
    pub filename: String,
    pub byte_size: u64,
}

/// §3.2: minimal denormalized session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub started_at: UtcMillis,
    pub app_version: String,
    /// Unknown members of this session value, preserved (§5.2).
    pub unknown: BTreeMap<String, Value>,
}

/// One `sessions` map value: structured metadata, or a non-conforming value
/// preserved verbatim (§5.2 — never dropped).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEntry {
    Meta(SessionMeta),
    Opaque(Value),
}

/// One element of the `events` array: a strict EVENTS §4 canonical event, or
/// an opaque blob preserved verbatim (§4: unknown kinds/fields/future event
/// versions are kept on rewrite, never indexed, never dropped).
#[derive(Debug, Clone, PartialEq)]
pub enum SidecarEntry {
    Canonical(Event),
    Opaque(Value),
}

impl SidecarEntry {
    /// Sort key for the `events` array (§3.5 rule 5: ascending by id).
    /// Opaque entries with a string `id` sort among real events; entries
    /// without one sort by their compact canonical bytes (deterministic,
    /// and `{` sorts after every ULID character).
    pub fn sort_key(&self) -> String {
        match self {
            SidecarEntry::Canonical(e) => e.id.as_str().to_owned(),
            SidecarEntry::Opaque(v) => opaque_sort_key(v),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            SidecarEntry::Canonical(e) => serde_json::from_slice(&canonical::canonical_json(e))
                .expect("canonical event JSON is valid JSON"),
            SidecarEntry::Opaque(v) => v.clone(),
        }
    }
}

/// Sort key for an opaque blob: its `id` member when a string, else its
/// compact canonical serialization.
pub fn opaque_sort_key(v: &Value) -> String {
    if let Some(id) = v.get("id").and_then(Value::as_str) {
        id.to_owned()
    } else {
        String::from_utf8(to_compact_canonical(v)).expect("canonical JSON is UTF-8")
    }
}

/// A parsed/buildable sidecar document (§3).
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarDoc {
    /// `None` only in session journals (§7.2: `image: null`).
    pub image: Option<ImageSnapshot>,
    /// Unknown members found inside `image` (§5.2).
    pub unknown_image: BTreeMap<String, Value>,
    /// Session ULID → metadata; exactly the sessions referenced (§3.2).
    pub sessions: BTreeMap<String, SessionEntry>,
    /// Sorted ascending by `sort_key` (§3.3).
    pub events: Vec<SidecarEntry>,
    /// Unknown top-level members (§5.2).
    pub unknown_top: BTreeMap<String, Value>,
}

impl SidecarDoc {
    pub fn new(image: Option<ImageSnapshot>) -> Self {
        Self {
            image,
            unknown_image: BTreeMap::new(),
            sessions: BTreeMap::new(),
            events: Vec::new(),
            unknown_top: BTreeMap::new(),
        }
    }

    /// Canonical file bytes (§3.5). Pure function of the document.
    pub fn to_bytes(&self) -> Vec<u8> {
        to_pretty_canonical(&self.to_value())
    }

    fn to_value(&self) -> Value {
        let mut top = Map::new();
        top.insert(
            "format".to_owned(),
            Value::String(FORMAT_SIDECAR.to_owned()),
        );
        top.insert(
            "schema_version".to_owned(),
            Value::Number(SIDECAR_SCHEMA_VERSION.into()),
        );
        top.insert(
            "image".to_owned(),
            match &self.image {
                None => Value::Null,
                Some(snap) => {
                    let mut img = Map::new();
                    img.insert("hash".to_owned(), Value::String(snap.hash.to_string()));
                    img.insert("filename".to_owned(), Value::String(snap.filename.clone()));
                    img.insert("byte_size".to_owned(), Value::Number(snap.byte_size.into()));
                    for (k, v) in &self.unknown_image {
                        img.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                    Value::Object(img)
                }
            },
        );
        let mut sessions = Map::new();
        for (ulid, entry) in &self.sessions {
            let v = match entry {
                SessionEntry::Meta(m) => {
                    let mut sv = Map::new();
                    sv.insert(
                        "started_at".to_owned(),
                        Value::String(m.started_at.to_rfc3339()),
                    );
                    sv.insert(
                        "app_version".to_owned(),
                        Value::String(m.app_version.clone()),
                    );
                    for (k, uv) in &m.unknown {
                        sv.entry(k.clone()).or_insert_with(|| uv.clone());
                    }
                    Value::Object(sv)
                }
                SessionEntry::Opaque(v) => v.clone(),
            };
            sessions.insert(ulid.clone(), v);
        }
        top.insert("sessions".to_owned(), Value::Object(sessions));

        let mut events: Vec<(String, Value)> = self
            .events
            .iter()
            .map(|e| (e.sort_key(), e.to_value()))
            .collect();
        events.sort_by(|a, b| a.0.cmp(&b.0));
        top.insert(
            "events".to_owned(),
            Value::Array(events.into_iter().map(|(_, v)| v).collect()),
        );

        for (k, v) in &self.unknown_top {
            top.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(top)
    }

    /// The canonical events in this document.
    pub fn canonical_events(&self) -> Vec<Event> {
        self.events
            .iter()
            .filter_map(|e| match e {
                SidecarEntry::Canonical(ev) => Some(ev.clone()),
                SidecarEntry::Opaque(_) => None,
            })
            .collect()
    }

    /// Number of opaque (preserved, unindexed) entries.
    pub fn opaque_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, SidecarEntry::Opaque(_)))
            .count()
    }
}

/// Outcome of reading a `*.photoproof.json` file (§4, §5.3, §12.2).
#[derive(Debug)]
pub enum ParsedSidecar {
    /// A current-version sidecar, fully modeled.
    Doc(Box<SidecarDoc>),
    /// §5.3: written by a newer Photoproof — opaque and inviolable.
    NewerVersion { schema_version: u64 },
    /// Our own export manifest (not a sidecar; not a foreign file either).
    Manifest,
}

/// §4 reader acceptance: valid UTF-8 JSON; top-level object;
/// `format == "photoproof-sidecar"`; positive-integer `schema_version`;
/// `image` an object with a 64-lowercase-hex `hash` (or `null`); `events`
/// an array. A malformed *event* never poisons the file (opaque blob).
pub fn parse_sidecar(bytes: &[u8]) -> Result<ParsedSidecar, SidecarParseError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| SidecarParseError::Json(e.to_string()))?;
    let Value::Object(mut top) = value else {
        return Err(SidecarParseError::NotObject);
    };
    match top.get("format").and_then(Value::as_str) {
        Some(FORMAT_SIDECAR) => {}
        Some(FORMAT_MANIFEST) => return Ok(ParsedSidecar::Manifest),
        _ => return Err(SidecarParseError::NotASidecar),
    }
    top.remove("format");

    let sv = top
        .remove("schema_version")
        .ok_or(SidecarParseError::Invalid(
            "schema_version",
            "missing".into(),
        ))?;
    let schema_version = sv
        .as_u64()
        .filter(|v| *v >= 1 && !sv.is_f64())
        .ok_or_else(|| SidecarParseError::Invalid("schema_version", sv.to_string()))?;
    if schema_version > SIDECAR_SCHEMA_VERSION {
        return Ok(ParsedSidecar::NewerVersion { schema_version });
    }

    let image_v = top
        .remove("image")
        .ok_or(SidecarParseError::Invalid("image", "missing".into()))?;
    let mut unknown_image = BTreeMap::new();
    let image = match image_v {
        Value::Null => None,
        Value::Object(mut img) => {
            let hash_s = img
                .remove("hash")
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or_else(|| SidecarParseError::Invalid("image.hash", "missing".into()))?;
            let hash = ContentHash::from_hex(&hash_s)
                .map_err(|e| SidecarParseError::Invalid("image.hash", e.to_string()))?;
            let filename = img
                .remove("filename")
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let byte_size = img
                .remove("byte_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            for (k, v) in img {
                unknown_image.insert(k, v);
            }
            Some(ImageSnapshot {
                hash,
                filename,
                byte_size,
            })
        }
        other => {
            return Err(SidecarParseError::Invalid("image", other.to_string()));
        }
    };

    let mut sessions = BTreeMap::new();
    if let Some(sv) = top.remove("sessions") {
        let Value::Object(map) = sv else {
            return Err(SidecarParseError::Invalid("sessions", sv.to_string()));
        };
        for (ulid, entry_v) in map {
            sessions.insert(ulid, parse_session_entry(entry_v));
        }
    }

    let events_v = top
        .remove("events")
        .ok_or(SidecarParseError::Invalid("events", "missing".into()))?;
    let Value::Array(raw_events) = events_v else {
        return Err(SidecarParseError::Invalid("events", "not an array".into()));
    };
    let mut events: Vec<SidecarEntry> = raw_events
        .into_iter()
        .map(|ev| {
            // Re-serialize to the EVENTS §4 compact normal form and run the
            // strict canonical parser; any failure preserves the blob (§4).
            let compact = to_compact_canonical(&ev);
            match canonical::parse_canonical(&compact) {
                Ok(e) => SidecarEntry::Canonical(e),
                Err(_) => SidecarEntry::Opaque(ev),
            }
        })
        .collect();
    events.sort_by_key(SidecarEntry::sort_key);

    let unknown_top: BTreeMap<String, Value> = top.into_iter().collect();

    Ok(ParsedSidecar::Doc(Box::new(SidecarDoc {
        image,
        unknown_image,
        sessions,
        events,
        unknown_top,
    })))
}

fn parse_session_entry(v: Value) -> SessionEntry {
    let Value::Object(map) = &v else {
        return SessionEntry::Opaque(v);
    };
    let started = map.get("started_at").and_then(Value::as_str);
    let app_version = map.get("app_version").and_then(Value::as_str);
    match (started.and_then(|s| UtcMillis::parse(s).ok()), app_version) {
        (Some(started_at), Some(app_version)) => {
            let unknown = map
                .iter()
                .filter(|(k, _)| k.as_str() != "started_at" && k.as_str() != "app_version")
                .map(|(k, val)| (k.clone(), val.clone()))
                .collect();
            SessionEntry::Meta(SessionMeta {
                started_at,
                app_version: app_version.to_owned(),
                unknown,
            })
        }
        _ => SessionEntry::Opaque(v),
    }
}

/// §3.4: reduce an opaque event blob to its redacted husk — retain only the
/// structural fields, scrub everything else *including unknown fields*
/// (redaction supremacy beats preservation), and add `redacted_by`.
pub fn huskify_opaque(v: &Value, redacted_by: &str) -> Value {
    const KEEP: [&str; 9] = [
        "id",
        "kind",
        "linked_event",
        "redacted_by",
        "session_id",
        "source",
        "target_event",
        "targets",
        "ts",
    ];
    let mut out = Map::new();
    if let Value::Object(map) = v {
        for k in KEEP {
            if let Some(val) = map.get(k) {
                out.insert(k.to_owned(), val.clone());
            }
        }
        if let Some(vv) = map.get("v") {
            out.insert("v".to_owned(), vv.clone());
        }
    }
    out.insert(
        "redacted_by".to_owned(),
        Value::String(redacted_by.to_owned()),
    );
    Value::Object(out)
}
