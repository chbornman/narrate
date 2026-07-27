//! Manual topics store (DESIGN-TOPICS-COLLECTIONS.md): a topic is a SAVED
//! PHRASE, like a saved search. The Topics sidebar tab lists these alongside the
//! autosuggested (cluster-derived) ones, and selecting one scopes the grid to
//! its RANKED images — but a topic's images are ALWAYS computed affinity at read
//! time (`topic::topic_ranked_images`), never stored membership. That is exactly
//! what distinguishes a topic (continuous, fuzzy, a lens) from a collection
//! (discrete, evented, durable).
//!
//! The ranked image set is derived, but the saved phrase and its notes are
//! authored intent. `topics.photoproof.json` therefore travels with an explicit
//! journal export. It is deliberately export/import rather than an adjacent
//! per-image sidecar: topics have no natural photo-volume owner.
//!
//! Same connection shape as `collections` (its own connection over the shared
//! photoproof database, the Library pattern), so the schema it depends on is
//! already migrated by the time the shell opens it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

use crate::id::UtcMillis;
use crate::store::StoreError;

#[derive(Debug, Error)]
pub enum TopicsError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("topics portability file: {0}")]
    Portable(String),
    #[error("topic not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

pub const TOPICS_FILENAME: &str = "topics.photoproof.json";
const TOPICS_FORMAT: &str = "photoproof-topics";
const TOPICS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopicsDoc {
    format: String,
    version: u32,
    topics: Vec<PortableTopic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableTopic {
    id: String,
    phrase: String,
    space: String,
    created_ts: String,
    notes: Vec<PortableTopicNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableTopicNote {
    id: String,
    ts: String,
    text: String,
}

/// Which embedding space the user wants a saved topic pulled in. `None`
/// (the default) blends BOTH halves at the graph's `alpha_default`; the two
/// explicit variants pin one side. Stored as the lowercase string in the
/// `topics.space` column (NULL = `Blend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicSpace {
    /// Blend both spaces at the configured alpha (the default).
    Blend,
    /// `image_summary` (what you SAID) only.
    Annotation,
    /// `image_clip` (what it LOOKS like) only.
    Clip,
}

impl TopicSpace {
    /// The column value (`None` for `Blend`, so the default reads as NULL).
    fn as_db(self) -> Option<&'static str> {
        match self {
            TopicSpace::Blend => None,
            TopicSpace::Annotation => Some("annotation"),
            TopicSpace::Clip => Some("clip"),
        }
    }

    /// Parse the stored column. NULL is the canonical `Blend` default; an
    /// unknown non-null value is corrupt or belongs to a newer build and must
    /// not silently change the user's pinned topic into a healthy-looking
    /// blend.
    fn from_db(s: Option<&str>) -> Result<Self, TopicsError> {
        match s {
            None => Ok(TopicSpace::Blend),
            Some("annotation") => Ok(TopicSpace::Annotation),
            Some("clip") => Ok(TopicSpace::Clip),
            Some(other) => Err(TopicsError::Invalid(format!(
                "stored topic space is unsupported: {other}"
            ))),
        }
    }

    /// Resolve a caller-supplied string (the command layer's optional `space`
    /// argument) to a space. `None` or an empty/unknown string is `Blend`.
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some("annotation") => TopicSpace::Annotation,
            Some("clip") => TopicSpace::Clip,
            _ => TopicSpace::Blend,
        }
    }

    /// The canonical lowercase tag the command/DTO layer surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            TopicSpace::Blend => "blend",
            TopicSpace::Annotation => "annotation",
            TopicSpace::Clip => "clip",
        }
    }
}

/// One saved topic as the command layer reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicRecord {
    pub id: String,
    pub phrase: String,
    pub space: TopicSpace,
    pub created_ts: String,
}

/// One `topic_notes` row: append-only, never edited or deleted (the
/// `collection_notes` NoteEntry shape, mirrored for topics). A topic note is
/// the user's authored text ABOUT the topic — its definition, intent, what it
/// is for. WHY topics carry this when they otherwise persist nothing durable:
/// the saved phrase is regenerable intent, but a note is curated user truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicNote {
    /// ULID.
    pub id: String,
    /// RFC 3339 UTC.
    pub ts: String,
    pub text: String,
}

/// The manual-topics engine: its own connection over the shared photoproof
/// database (the `collections` pattern). Explicit journal export/import owns
/// the portable authored phrase/note document; affinity results remain derived.
pub struct Topics {
    conn: Mutex<Connection>,
}

impl Topics {
    /// Open over the shared database. The `topics` table is migrated by the
    /// EventStore open (schema user_version 13), exactly like `collections`
    /// relies on the same throwaway-open to have run its migration first.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, TopicsError> {
        let db_path = db_path.as_ref();
        // Throwaway open runs migrations (the documented Collections/Library
        // pattern); then a library connection with the §5.1 pragmas applied.
        drop(crate::store::EventStore::open(db_path)?);
        let conn = crate::library::open_library_connection(db_path)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Save a topic (a phrase + optional pinned space). The phrase is trimmed;
    /// an empty phrase is rejected (a blank saved search is meaningless). Saving
    /// the SAME phrase twice is allowed — a topic is an editable, removable
    /// saved search, and the user may legitimately keep two pins of one phrase
    /// in different spaces; the id (ULID) keeps them distinct.
    pub fn add(
        &self,
        phrase: &str,
        space: TopicSpace,
        now: UtcMillis,
    ) -> Result<TopicRecord, TopicsError> {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            return Err(TopicsError::Invalid("topic phrase is empty".into()));
        }
        let id = Ulid::new().to_string();
        let ts = now.to_rfc3339();
        {
            let conn = self.conn.lock().expect("topics mutex");
            conn.execute(
                "INSERT INTO topics (id, phrase, space, created_ts) VALUES (?1, ?2, ?3, ?4)",
                params![id, phrase, space.as_db(), ts],
            )?;
        }
        Ok(TopicRecord {
            id,
            phrase: phrase.to_owned(),
            space,
            created_ts: ts,
        })
    }

    /// All saved topics, newest first. Ordered by the CALLER-supplied
    /// `created_ts` (the authoritative save time), id DESC breaking same-ms ties
    /// deterministically. WHY not `ORDER BY id` alone: the ULID id is minted from
    /// the system clock at insert, NOT from the passed `now`, so id order need not
    /// agree with created_ts order; sorting on created_ts keeps "newest first"
    /// honest (and the test reproducible) regardless.
    pub fn list(&self) -> Result<Vec<TopicRecord>, TopicsError> {
        let conn = self.conn.lock().expect("topics mutex");
        let mut stmt = conn.prepare(
            "SELECT id, phrase, space, created_ts FROM topics ORDER BY created_ts DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, phrase, space, created_ts) = row?;
            out.push(TopicRecord {
                id,
                phrase,
                space: TopicSpace::from_db(space.as_deref())?,
                created_ts,
            });
        }
        Ok(out)
    }

    /// Remove a saved topic by id. A missing id is an error (the caller asked to
    /// remove THIS topic and must learn it was already gone), mirroring the
    /// collections NotFound posture.
    pub fn remove(&self, id: &str) -> Result<(), TopicsError> {
        let removed = {
            let conn = self.conn.lock().expect("topics mutex");
            conn.execute("DELETE FROM topics WHERE id = ?1", params![id])?
        };
        if removed == 0 {
            return Err(TopicsError::NotFound(id.to_owned()));
        }
        Ok(())
    }

    /// Append a note to a topic (append-only — there is no edit or delete, the
    /// `Collections::add_note` posture). The text is trimmed; an empty note is
    /// rejected. A missing topic id is `NotFound` (the FK guard, mirroring the
    /// collections `require_collection` check). ULID id + RFC 3339 UTC ts,
    /// exactly like a collection note.
    pub fn add_note(
        &self,
        topic_id: &str,
        text: &str,
        now: UtcMillis,
    ) -> Result<TopicNote, TopicsError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(TopicsError::Invalid("note text is empty".into()));
        }
        let note = TopicNote {
            id: Ulid::new().to_string(),
            ts: now.to_rfc3339(),
            text: text.to_owned(),
        };
        {
            let conn = self.conn.lock().expect("topics mutex");
            // FK guard: a note must hang off a real topic (the collections
            // require_collection precedent), so a stale id surfaces, never
            // silently writes an orphan row.
            let exists: bool = conn.query_row(
                "SELECT count(*) > 0 FROM topics WHERE id = ?1",
                params![topic_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(TopicsError::NotFound(topic_id.to_owned()));
            }
            conn.execute(
                "INSERT INTO topic_notes (id, topic_id, ts, text) VALUES (?1, ?2, ?3, ?4)",
                params![note.id, topic_id, note.ts, note.text],
            )?;
        }
        Ok(note)
    }

    /// A topic's notes in id order (ULID order = time order), the
    /// `Collections::notes` shape. A missing topic id is `NotFound`.
    pub fn notes(&self, topic_id: &str) -> Result<Vec<TopicNote>, TopicsError> {
        let conn = self.conn.lock().expect("topics mutex");
        let exists: bool = conn.query_row(
            "SELECT count(*) > 0 FROM topics WHERE id = ?1",
            params![topic_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(TopicsError::NotFound(topic_id.to_owned()));
        }
        let mut stmt =
            conn.prepare("SELECT id, ts, text FROM topic_notes WHERE topic_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![topic_id], |r| {
            Ok(TopicNote {
                id: r.get(0)?,
                ts: r.get(1)?,
                text: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Write every saved phrase and authored topic note beside the journal and
    /// collections export. Ordering and JSON shape are deterministic so a
    /// repeated export is diffable and checksummable.
    pub fn export_to(&self, destination: &Path) -> Result<PathBuf, TopicsError> {
        let doc = {
            let conn = self.conn.lock().expect("topics mutex");
            read_portable_doc(&conn)?
        };
        let mut bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|error| TopicsError::Portable(error.to_string()))?;
        bytes.push(b'\n');
        std::fs::create_dir_all(destination)?;
        let target = destination.join(TOPICS_FILENAME);
        crate::sidecar::write_atomic(&target, &bytes)?;
        Ok(target)
    }

    /// Union an exported topics document into the live database. Existing rows
    /// with the same id must be byte-for-byte equivalent; an ambiguous
    /// same-id/different-authored-content conflict aborts the entire
    /// transaction instead of silently choosing a winner.
    pub fn import_from(&self, path: &Path) -> Result<usize, TopicsError> {
        let bytes = std::fs::read(path)?;
        let doc: TopicsDoc = serde_json::from_slice(&bytes)
            .map_err(|error| TopicsError::Portable(error.to_string()))?;
        validate_portable_doc(&doc)?;
        let mut conn = self.conn.lock().expect("topics mutex");
        let tx = conn.transaction()?;
        let mut inserted = 0_usize;
        for topic in doc.topics {
            let stored = tx
                .query_row(
                    "SELECT phrase, COALESCE(space, 'blend'), created_ts
                       FROM topics WHERE id = ?1",
                    params![topic.id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let db_space = if topic.space == "blend" {
                None
            } else {
                Some(topic.space.as_str())
            };
            match stored {
                Some(existing)
                    if existing
                        != (
                            topic.phrase.clone(),
                            topic.space.clone(),
                            topic.created_ts.clone(),
                        ) =>
                {
                    return Err(TopicsError::Portable(format!(
                        "topic {} conflicts with existing authored content",
                        topic.id
                    )));
                }
                Some(_) => {}
                None => {
                    tx.execute(
                        "INSERT INTO topics (id, phrase, space, created_ts)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![topic.id, topic.phrase, db_space, topic.created_ts],
                    )?;
                    inserted += 1;
                }
            }
            for note in topic.notes {
                let stored = tx
                    .query_row(
                        "SELECT topic_id, ts, text FROM topic_notes WHERE id = ?1",
                        params![note.id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                match stored {
                    Some(existing)
                        if existing != (topic.id.clone(), note.ts.clone(), note.text.clone()) =>
                    {
                        return Err(TopicsError::Portable(format!(
                            "topic note {} conflicts with existing authored content",
                            note.id
                        )));
                    }
                    Some(_) => {}
                    None => {
                        tx.execute(
                            "INSERT INTO topic_notes (id, topic_id, ts, text)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![note.id, topic.id, note.ts, note.text],
                        )?;
                        inserted += 1;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(inserted)
    }
}

fn read_portable_doc(conn: &Connection) -> Result<TopicsDoc, TopicsError> {
    let mut stmt = conn.prepare(
        "SELECT id, phrase, COALESCE(space, 'blend'), created_ts
           FROM topics ORDER BY id",
    )?;
    let heads = stmt
        .query_map([], |row| {
            Ok(PortableTopic {
                id: row.get(0)?,
                phrase: row.get(1)?,
                space: row.get(2)?,
                created_ts: row.get(3)?,
                notes: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut note_stmt =
        conn.prepare("SELECT id, ts, text FROM topic_notes WHERE topic_id = ?1 ORDER BY id")?;
    let mut topics = Vec::with_capacity(heads.len());
    for mut topic in heads {
        topic.notes = note_stmt
            .query_map(params![topic.id], |row| {
                Ok(PortableTopicNote {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    text: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        topics.push(topic);
    }
    Ok(TopicsDoc {
        format: TOPICS_FORMAT.into(),
        version: TOPICS_VERSION,
        topics,
    })
}

fn validate_portable_doc(doc: &TopicsDoc) -> Result<(), TopicsError> {
    if doc.format != TOPICS_FORMAT || doc.version != TOPICS_VERSION {
        return Err(TopicsError::Portable(format!(
            "unsupported format/version {}/{}",
            doc.format, doc.version
        )));
    }
    let mut topic_ids = HashSet::new();
    let mut note_ids = HashSet::new();
    for topic in &doc.topics {
        if Ulid::from_string(&topic.id).is_err() || !topic_ids.insert(&topic.id) {
            return Err(TopicsError::Portable(format!(
                "invalid or duplicate topic id {}",
                topic.id
            )));
        }
        if topic.phrase.trim().is_empty() || topic.phrase.trim() != topic.phrase {
            return Err(TopicsError::Portable(format!(
                "topic {} has an invalid phrase",
                topic.id
            )));
        }
        if !matches!(topic.space.as_str(), "blend" | "annotation" | "clip") {
            return Err(TopicsError::Portable(format!(
                "topic {} has unsupported space {}",
                topic.id, topic.space
            )));
        }
        UtcMillis::parse(&topic.created_ts).map_err(|error| {
            TopicsError::Portable(format!("topic {} timestamp: {error}", topic.id))
        })?;
        for note in &topic.notes {
            if Ulid::from_string(&note.id).is_err() || !note_ids.insert(&note.id) {
                return Err(TopicsError::Portable(format!(
                    "invalid or duplicate topic-note id {}",
                    note.id
                )));
            }
            if note.text.trim().is_empty() || note.text.trim() != note.text {
                return Err(TopicsError::Portable(format!(
                    "topic note {} has invalid text",
                    note.id
                )));
            }
            UtcMillis::parse(&note.ts).map_err(|error| {
                TopicsError::Portable(format!("topic note {} timestamp: {error}", note.id))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Topics) {
        let dir = tempfile::tempdir().unwrap();
        let topics = Topics::open(dir.path().join("photoproof.db")).unwrap();
        (dir, topics)
    }

    /// CRUD round-trip: add saves the phrase + space, list returns it newest
    /// first, remove deletes it, removing a gone id errors.
    #[test]
    fn crud_round_trip() {
        let (_dir, topics) = open();
        // Distinct timestamps a full millisecond apart so the ULID time field
        // (not its random tail) decides order — within ONE millisecond ULIDs are
        // randomly ordered, which would make "newest first" flaky.
        let t0 = UtcMillis::from_epoch_ms(1_000_000);
        let t1 = UtcMillis::from_epoch_ms(1_000_001);
        let a = topics.add("harbor at dusk", TopicSpace::Blend, t0).unwrap();
        let b = topics.add("snow ridge", TopicSpace::Clip, t1).unwrap();
        assert_eq!(a.space, TopicSpace::Blend);
        assert_eq!(b.space, TopicSpace::Clip);

        let listed = topics.list().unwrap();
        assert_eq!(listed.len(), 2);
        // Newest first: b (later ULID timestamp) leads a.
        assert_eq!(listed[0].id, b.id);
        assert_eq!(listed[0].phrase, "snow ridge");
        assert_eq!(listed[0].space, TopicSpace::Clip);
        assert_eq!(listed[1].id, a.id);
        // The blend default round-trips through the NULL column.
        assert_eq!(listed[1].space, TopicSpace::Blend);

        topics.remove(&a.id).unwrap();
        let after = topics.list().unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, b.id);

        // Removing an already-gone id is an explicit NotFound, never a silent ok.
        assert!(matches!(
            topics.remove(&a.id),
            Err(TopicsError::NotFound(_))
        ));
    }

    #[test]
    fn list_surfaces_an_unknown_stored_space_instead_of_defaulting_to_blend() {
        let (_dir, topics) = open();
        let saved = topics
            .add("harbor", TopicSpace::Clip, UtcMillis::now())
            .unwrap();
        topics
            .conn
            .lock()
            .expect("topics mutex")
            .execute(
                "UPDATE topics SET space = 'future-space' WHERE id = ?1",
                params![saved.id],
            )
            .unwrap();

        assert!(matches!(
            topics.list(),
            Err(TopicsError::Invalid(detail))
                if detail.contains("stored topic space")
                    && detail.contains("future-space")
        ));
    }

    /// An empty / whitespace-only phrase is rejected (a blank saved search is
    /// meaningless), and the phrase is trimmed before saving.
    #[test]
    fn empty_phrase_rejected_and_trimmed() {
        let (_dir, topics) = open();
        let now = UtcMillis::now();
        assert!(matches!(
            topics.add("   ", TopicSpace::Blend, now),
            Err(TopicsError::Invalid(_))
        ));
        let t = topics.add("  fog  ", TopicSpace::Blend, now).unwrap();
        assert_eq!(t.phrase, "fog", "phrase is trimmed");
    }

    /// Topic notes round-trip: append + list, ORDERED BY id (the
    /// collection_notes contract), append-only, keyed to the topic id. The note
    /// id is a fresh `Ulid::new()` (wall clock, like a collection note), NOT
    /// minted from the passed `now`, so the test asserts the actual guarantee —
    /// the list is sorted ascending by id — rather than which note happened to
    /// win the same-millisecond random tail.
    #[test]
    fn topic_notes_append_and_list_in_time_order() {
        let (_dir, topics) = open();
        let t = topics
            .add(
                "harbor",
                TopicSpace::Blend,
                UtcMillis::from_epoch_ms(1_000_000),
            )
            .unwrap();

        let n0 = topics
            .add_note(
                &t.id,
                "what this topic is for",
                UtcMillis::from_epoch_ms(2_000_000),
            )
            .unwrap();
        let n1 = topics
            .add_note(
                &t.id,
                "refine toward dusk shots",
                UtcMillis::from_epoch_ms(2_000_001),
            )
            .unwrap();
        assert_ne!(n0.id, n1.id);

        let listed = topics.notes(&t.id).unwrap();
        assert_eq!(listed.len(), 2);
        // The ordering guarantee: ascending id (= time order, the journal
        // precedent). Both appended notes are present, round-tripping verbatim.
        let ids: Vec<&str> = listed.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "notes list ascending by id");
        let texts: std::collections::HashSet<&str> =
            listed.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            ["what this topic is for", "refine toward dusk shots"]
                .into_iter()
                .collect()
        );
        // RFC 3339 UTC ts, derived from the passed `now`, exactly like a
        // collection note.
        let by_text = |want: &str| listed.iter().find(|n| n.text == want).unwrap();
        assert_eq!(
            by_text("what this topic is for").ts,
            UtcMillis::from_epoch_ms(2_000_000).to_rfc3339()
        );
        assert_eq!(
            by_text("refine toward dusk shots").ts,
            UtcMillis::from_epoch_ms(2_000_001).to_rfc3339()
        );
    }

    /// An empty / whitespace-only note is rejected and the text is trimmed
    /// before saving (the collections add_note contract).
    #[test]
    fn topic_note_empty_rejected_and_trimmed() {
        let (_dir, topics) = open();
        let now = UtcMillis::now();
        let t = topics.add("fog", TopicSpace::Blend, now).unwrap();
        assert!(matches!(
            topics.add_note(&t.id, "   ", now),
            Err(TopicsError::Invalid(_))
        ));
        let n = topics.add_note(&t.id, "  the morning haze  ", now).unwrap();
        assert_eq!(n.text, "the morning haze", "note text is trimmed");
    }

    /// A note must hang off a real topic: appending to (or listing) a missing
    /// id is NotFound, never a silent orphan row (the FK guard).
    #[test]
    fn topic_note_requires_an_existing_topic() {
        let (_dir, topics) = open();
        let now = UtcMillis::now();
        assert!(matches!(
            topics.add_note("01MISSING", "orphan", now),
            Err(TopicsError::NotFound(_))
        ));
        assert!(matches!(
            topics.notes("01MISSING"),
            Err(TopicsError::NotFound(_))
        ));
    }

    /// The v15 migration applies cleanly on an EXISTING (pre-topic-notes) db:
    /// open the store twice over the same file (the second open re-runs the
    /// idempotent migration), and notes written under the first open survive.
    #[test]
    fn topic_notes_migration_applies_on_existing_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("photoproof.db");
        let id;
        let note_id;
        {
            let topics = Topics::open(&db).unwrap();
            let now = UtcMillis::from_epoch_ms(1_000_000);
            let t = topics.add("ridge line", TopicSpace::Blend, now).unwrap();
            let n = topics.add_note(&t.id, "the alpine series", now).unwrap();
            id = t.id;
            note_id = n.id;
        }
        // Re-open the same file: Topics::open runs the EventStore migration
        // again (idempotent), and the prior note must be intact.
        let topics = Topics::open(&db).unwrap();
        let listed = topics.notes(&id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, note_id);
        assert_eq!(listed[0].text, "the alpine series");
    }

    #[test]
    fn portable_export_import_preserves_authored_topics_and_notes() {
        let (_source_dir, source) = open();
        let saved = source
            .add(
                "harbor at dusk",
                TopicSpace::Annotation,
                UtcMillis::from_epoch_ms(1_000_000),
            )
            .unwrap();
        let note = source
            .add_note(
                &saved.id,
                "the working definition",
                UtcMillis::from_epoch_ms(2_000_000),
            )
            .unwrap();
        let export_dir = tempfile::tempdir().unwrap();
        let path = source.export_to(export_dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), TOPICS_FILENAME);

        let (_target_dir, target) = open();
        assert_eq!(target.import_from(&path).unwrap(), 2);
        let imported = target.list().unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].id, saved.id);
        assert_eq!(imported[0].phrase, saved.phrase);
        assert_eq!(imported[0].space, TopicSpace::Annotation);
        assert_eq!(target.notes(&saved.id).unwrap(), vec![note]);
        assert_eq!(target.import_from(&path).unwrap(), 0, "union is idempotent");
    }

    #[test]
    fn portable_conflict_aborts_the_whole_import() {
        let (_source_dir, source) = open();
        let first = source
            .add("first", TopicSpace::Blend, UtcMillis::from_epoch_ms(1_000))
            .unwrap();
        let second = source
            .add("second", TopicSpace::Clip, UtcMillis::from_epoch_ms(2_000))
            .unwrap();
        let export_dir = tempfile::tempdir().unwrap();
        let path = source.export_to(export_dir.path()).unwrap();

        let (_target_dir, target) = open();
        target
            .add(
                "unrelated",
                TopicSpace::Blend,
                UtcMillis::from_epoch_ms(3_000),
            )
            .unwrap();
        {
            let conn = target.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO topics (id, phrase, space, created_ts)
                 VALUES (?1, 'conflicting', NULL, ?2)",
                params![second.id, second.created_ts],
            )
            .unwrap();
        }
        assert!(matches!(
            target.import_from(&path),
            Err(TopicsError::Portable(detail)) if detail.contains("conflicts")
        ));
        assert!(
            !target
                .list()
                .unwrap()
                .iter()
                .any(|topic| topic.id == first.id),
            "the earlier insert in the same import transaction rolled back"
        );
    }

    #[test]
    fn portable_document_rejects_unknown_format_without_mutation() {
        let (_dir, topics) = open();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            br#"{"format":"other","version":1,"topics":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            topics.import_from(file.path()),
            Err(TopicsError::Portable(detail)) if detail.contains("unsupported")
        ));
        assert!(topics.list().unwrap().is_empty());
    }

    /// `TopicSpace::parse` maps the command layer's optional string to a space,
    /// defaulting unknown/absent to blend.
    #[test]
    fn space_parse_defaults_to_blend() {
        assert_eq!(TopicSpace::parse(None), TopicSpace::Blend);
        assert_eq!(TopicSpace::parse(Some("")), TopicSpace::Blend);
        assert_eq!(TopicSpace::parse(Some("nonsense")), TopicSpace::Blend);
        assert_eq!(
            TopicSpace::parse(Some("annotation")),
            TopicSpace::Annotation
        );
        assert_eq!(TopicSpace::parse(Some("clip")), TopicSpace::Clip);
    }
}
