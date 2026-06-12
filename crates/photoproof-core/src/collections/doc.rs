//! The collections portability document (spec/RETRIEVAL.md §10.2):
//! `appdata/collections.photoproof.json`, the file that makes collections
//! pass the "SQLite is a rebuildable index" test. Serialization reuses the
//! sidecar canonical form (SIDECARS §3.5) so the debounced writer's
//! mtime-stable byte comparison is exact.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::id::{ContentHash, UtcMillis, valid_ulid_str};
use crate::sidecar::to_pretty_canonical;

use super::CollectionsError;

/// §10.2: a single export file in app data, beside the sidecar set in the
/// one-click export.
pub const COLLECTIONS_FILENAME: &str = "collections.photoproof.json";

/// `version` member of the file. Future field additions ride a version
/// bump: a v1 reader treats a newer file as opaque (never merged, never
/// overwritten), mirroring SIDECARS §5.3.
pub const COLLECTIONS_VERSION: u64 = 1;

/// §10.1 `collections.status`: the one named mutable surface, together
/// with name/description (last-writer-wins by `updated_ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CollectionStatus {
    Active,
    Shelved,
    Done,
}

impl CollectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CollectionStatus::Active => "active",
            CollectionStatus::Shelved => "shelved",
            CollectionStatus::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Result<Self, CollectionsError> {
        match s {
            "active" => Ok(CollectionStatus::Active),
            "shelved" => Ok(CollectionStatus::Shelved),
            "done" => Ok(CollectionStatus::Done),
            other => Err(CollectionsError::Invalid(format!(
                "unknown collection status: {other}"
            ))),
        }
    }
}

/// One `collection_notes` row (§10.1): append-only, never edited or
/// deleted, so set-union by id is conflict-free (§10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEntry {
    /// ULID.
    pub id: String,
    /// RFC 3339 UTC, canonical millisecond form (EVENTS §1.3).
    pub ts: String,
    pub text: String,
}

/// One `collection_members` interval row (§10.1): membership is evented —
/// removing closes the open interval, re-adding opens a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// blake3-256, 64 lowercase hex. Hashes only: image redaction never
    /// touches membership rows (§10.2).
    pub image_hash: String,
    pub added_ts: String,
    /// `None` = currently a member.
    pub removed_ts: Option<String>,
}

/// One collection with its full history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionEntry {
    /// ULID.
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: CollectionStatus,
    pub created_ts: String,
    /// Metadata last-writer-wins key (§10.2) — touched ONLY by
    /// name/description/status edits, never by membership or notes, so a
    /// stale copy's metadata can never win on the back of a member add.
    pub updated_ts: String,
    /// Sorted by id (ULID order = time order).
    pub notes: Vec<NoteEntry>,
    /// Sorted by (image_hash, added_ts).
    pub members: Vec<MemberEntry>,
}

/// The whole file: collections sorted by id. Deterministic ordering makes
/// the serialized bytes a pure function of the content (the mtime-stable
/// no-op write depends on it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionsDoc {
    pub collections: Vec<CollectionEntry>,
}

/// Outcome of parsing a candidate collections file.
#[derive(Debug)]
pub enum ParsedCollections {
    Doc(CollectionsDoc),
    /// `version` > ours: opaque and inviolable (never merged, never
    /// overwritten) — mirrors SIDECARS §5.3.
    NewerVersion {
        version: u64,
    },
}

impl CollectionsDoc {
    /// Canonical pretty bytes (SIDECARS §3.5 rules, one trailing LF).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut top = Map::new();
        top.insert("version".into(), Value::Number(COLLECTIONS_VERSION.into()));
        top.insert(
            "collections".into(),
            Value::Array(self.collections.iter().map(collection_value).collect()),
        );
        to_pretty_canonical(&Value::Object(top))
    }

    /// Restore the canonical ordering after construction or merge.
    pub fn normalize(&mut self) {
        for c in &mut self.collections {
            c.notes.sort_by(|a, b| a.id.cmp(&b.id));
            c.members
                .sort_by(|a, b| (&a.image_hash, &a.added_ts).cmp(&(&b.image_hash, &b.added_ts)));
        }
        self.collections.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

fn collection_value(c: &CollectionEntry) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), Value::String(c.id.clone()));
    m.insert("name".into(), Value::String(c.name.clone()));
    m.insert("description".into(), Value::String(c.description.clone()));
    m.insert("status".into(), Value::String(c.status.as_str().into()));
    m.insert("created_ts".into(), Value::String(c.created_ts.clone()));
    m.insert("updated_ts".into(), Value::String(c.updated_ts.clone()));
    m.insert(
        "notes".into(),
        Value::Array(
            c.notes
                .iter()
                .map(|n| {
                    let mut nm = Map::new();
                    nm.insert("id".into(), Value::String(n.id.clone()));
                    nm.insert("ts".into(), Value::String(n.ts.clone()));
                    nm.insert("text".into(), Value::String(n.text.clone()));
                    Value::Object(nm)
                })
                .collect(),
        ),
    );
    m.insert(
        "members".into(),
        Value::Array(
            c.members
                .iter()
                .map(|mb| {
                    let mut mm = Map::new();
                    mm.insert("image_hash".into(), Value::String(mb.image_hash.clone()));
                    mm.insert("added_ts".into(), Value::String(mb.added_ts.clone()));
                    // The §10.2 example shows removed_ts explicitly null
                    // for open intervals; keep it present so the wire
                    // shape never depends on membership state.
                    mm.insert(
                        "removed_ts".into(),
                        mb.removed_ts
                            .clone()
                            .map(Value::String)
                            .unwrap_or(Value::Null),
                    );
                    Value::Object(mm)
                })
                .collect(),
        ),
    );
    Value::Object(m)
}

/// Strict parse of the §10.2 file. Validation is strict on the fields the
/// merge rules depend on (ids, hashes, timestamps); unknown members are
/// ignored because any future writer that adds fields bumps `version`,
/// which routes the whole file to the opaque `NewerVersion` path instead.
pub fn parse_collections(bytes: &[u8]) -> Result<ParsedCollections, CollectionsError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| CollectionsError::Parse(format!("not JSON: {e}")))?;
    let Value::Object(top) = value else {
        return Err(CollectionsError::Parse("top level is not an object".into()));
    };
    let version = top
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| CollectionsError::Parse("missing or invalid version".into()))?;
    if version > COLLECTIONS_VERSION {
        return Ok(ParsedCollections::NewerVersion { version });
    }
    let list = top
        .get("collections")
        .and_then(Value::as_array)
        .ok_or_else(|| CollectionsError::Parse("missing collections array".into()))?;

    // Duplicate collection ids inside ONE file are a producer bug, not a
    // merge input; folding them silently would hide corruption upstream.
    // The same stance applies to duplicate note ids (file-wide: ULIDs are
    // globally unique, and `collection_notes.id` is the table's primary
    // key) — accepting them would let last-row-wins resolution bypass the
    // merge rules when the collection is new to this database.
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut seen_notes: BTreeSet<String> = BTreeSet::new();
    let mut collections = Vec::with_capacity(list.len());
    for (i, cv) in list.iter().enumerate() {
        let c = parse_collection(cv)
            .map_err(|e| CollectionsError::Parse(format!("collections[{i}]: {e}")))?;
        if seen.insert(c.id.clone(), ()).is_some() {
            return Err(CollectionsError::Parse(format!(
                "duplicate collection id {}",
                c.id
            )));
        }
        for n in &c.notes {
            if !seen_notes.insert(n.id.clone()) {
                return Err(CollectionsError::Parse(format!(
                    "collections[{i}]: duplicate note id {}",
                    n.id
                )));
            }
        }
        collections.push(c);
    }
    let mut doc = CollectionsDoc { collections };
    doc.normalize();
    Ok(ParsedCollections::Doc(doc))
}

/// Best-effort salvage of a file the strict parse rejected: every
/// collection entry that individually validates is recovered; the rest
/// are reported. The sidecar precedent (SIDECARS §12.3) quarantines per
/// FILE because each file holds one image's journal — collections
/// concentrate ALL user truth in one file, so one garbled note must not
/// cost the user every intact collection. Strict `parse_collections`
/// remains the contract for compliant files; this runs only after it
/// has already failed, and the original bytes are preserved aside by
/// the caller before anything is imported.
pub(crate) fn salvage_collections(bytes: &[u8]) -> (CollectionsDoc, Vec<String>) {
    let nothing = (CollectionsDoc::default(), Vec::new());
    let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(bytes) else {
        return nothing; // not JSON at all: no entry boundaries to salvage
    };
    // A missing/garbled version cannot be trusted to be ours, and a newer
    // version is opaque by contract — recover nothing in either case.
    match top.get("version").and_then(Value::as_u64) {
        Some(v) if v <= COLLECTIONS_VERSION => {}
        _ => return nothing,
    }
    let Some(list) = top.get("collections").and_then(Value::as_array) else {
        return nothing;
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut seen_notes: BTreeSet<String> = BTreeSet::new();
    let mut collections = Vec::new();
    let mut errors = Vec::new();
    for (i, cv) in list.iter().enumerate() {
        match parse_collection(cv) {
            Ok(c) => {
                if !seen.insert(c.id.clone()) {
                    errors.push(format!(
                        "collections[{i}]: duplicate collection id {}",
                        c.id
                    ));
                    continue;
                }
                if let Some(n) = c.notes.iter().find(|n| seen_notes.contains(&n.id)) {
                    errors.push(format!("collections[{i}]: duplicate note id {}", n.id));
                    continue;
                }
                seen_notes.extend(c.notes.iter().map(|n| n.id.clone()));
                collections.push(c);
            }
            Err(e) => errors.push(format!("collections[{i}]: {e}")),
        }
    }
    let mut doc = CollectionsDoc { collections };
    doc.normalize();
    (doc, errors)
}

fn parse_collection(v: &Value) -> Result<CollectionEntry, String> {
    let obj = v.as_object().ok_or("not an object")?;
    let id = req_str(obj, "id")?;
    if !valid_ulid_str(&id) {
        return Err(format!("id is not a ULID: {id}"));
    }
    let name = req_str(obj, "name")?;
    let description = req_str(obj, "description")?;
    let status = CollectionStatus::parse(&req_str(obj, "status")?).map_err(|e| e.to_string())?;
    let created_ts = req_ts(obj, "created_ts")?;
    let updated_ts = req_ts(obj, "updated_ts")?;

    // Duplicate keys WITHIN one collection get the same strict rejection
    // as duplicate collection ids: the merge resolves duplicates only
    // across documents, so an in-file duplicate would otherwise fall
    // through to last-row-wins in the database upsert — which can
    // un-remove a member (a literal §13.9 violation).
    let mut notes = Vec::new();
    let mut note_ids: BTreeSet<String> = BTreeSet::new();
    for (i, nv) in req_array(obj, "notes")?.iter().enumerate() {
        let n = nv
            .as_object()
            .ok_or_else(|| format!("notes[{i}]: not an object"))?;
        let nid = req_str(n, "id").map_err(|e| format!("notes[{i}]: {e}"))?;
        if !valid_ulid_str(&nid) {
            return Err(format!("notes[{i}]: id is not a ULID: {nid}"));
        }
        if !note_ids.insert(nid.clone()) {
            return Err(format!("notes[{i}]: duplicate note id {nid}"));
        }
        notes.push(NoteEntry {
            id: nid,
            ts: req_ts(n, "ts").map_err(|e| format!("notes[{i}]: {e}"))?,
            text: req_str(n, "text").map_err(|e| format!("notes[{i}]: {e}"))?,
        });
    }

    let mut members = Vec::new();
    let mut member_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for (i, mv) in req_array(obj, "members")?.iter().enumerate() {
        let m = mv
            .as_object()
            .ok_or_else(|| format!("members[{i}]: not an object"))?;
        let hash = req_str(m, "image_hash").map_err(|e| format!("members[{i}]: {e}"))?;
        ContentHash::from_hex(&hash).map_err(|e| format!("members[{i}]: image_hash: {e}"))?;
        let removed_ts = match m.get("removed_ts") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                UtcMillis::parse(s.as_str())
                    .map_err(|e| format!("members[{i}]: removed_ts: {e}"))?;
                Some(s.clone())
            }
            Some(other) => return Err(format!("members[{i}]: removed_ts: {other}")),
        };
        let added_ts = req_ts(m, "added_ts").map_err(|e| format!("members[{i}]: {e}"))?;
        if !member_keys.insert((hash.clone(), added_ts.clone())) {
            return Err(format!(
                "members[{i}]: duplicate member key ({hash}, {added_ts})"
            ));
        }
        members.push(MemberEntry {
            image_hash: hash,
            added_ts,
            removed_ts,
        });
    }

    Ok(CollectionEntry {
        id,
        name,
        description,
        status,
        created_ts,
        updated_ts,
        notes,
        members,
    })
}

fn req_array<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, String> {
    obj.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing or non-array {key}"))
}

fn req_str(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing or non-string {key}"))
}

/// Timestamps must be the ONE canonical form (EVENTS §1.3 millisecond
/// RFC 3339 UTC): the §10.2 "earlier removal wins" rule compares strings,
/// and only a single fixed-width form makes lexicographic order equal
/// chronological order.
fn req_ts(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    let s = req_str(obj, key)?;
    UtcMillis::parse(&s).map_err(|e| format!("{key}: {e}"))?;
    Ok(s)
}
