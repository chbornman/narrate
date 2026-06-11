//! Shared helpers for the spec/EVENTS.md test suites.
#![allow(dead_code)]

pub mod m1env;
pub mod synthlib;

use std::path::PathBuf;

use photoproof_core::{
    ContentHash, Event, EventDraft, EventId, EventStore, Kind, Minter, Payload, RemarkSource,
    RetractionSource, SessionId, Source, StrokePayload, StrokePoint, Tool,
};
use rusqlite::Connection;

pub const DEVICE_ID: &str = "0123456789abcdef0123456789abcdef";

pub struct TestStore {
    pub dir: tempfile::TempDir,
    pub store: EventStore,
    pub session: SessionId,
}

impl TestStore {
    pub fn path(&self) -> PathBuf {
        self.dir.path().join("journal.db")
    }

    /// A raw connection to the same database (WAL: sees committed state).
    pub fn raw_conn(&self) -> Connection {
        let conn = Connection::open(self.path()).expect("open raw conn");
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .unwrap();
        conn
    }
}

pub fn new_store() -> TestStore {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = EventStore::open(dir.path().join("journal.db")).expect("open store");
    let session = store
        .open_session(photoproof_core::SessionContext {
            app_version: "0.1.0".into(),
            device_id: DEVICE_ID.into(),
            root_context: None,
        })
        .expect("open session");
    TestStore {
        dir,
        store,
        session,
    }
}

pub fn hash(n: u8) -> ContentHash {
    ContentHash::from_bytes_of(&[n])
}

// -- drafts -----------------------------------------------------------------

pub fn d_remark(text: &str, targets: Vec<ContentHash>) -> EventDraft {
    EventDraft::Remark {
        source: RemarkSource::Typed,
        text: text.into(),
        targets,
    }
}

pub fn d_voice(text: &str, targets: Vec<ContentHash>, linked: Option<EventId>) -> EventDraft {
    EventDraft::Remark {
        source: RemarkSource::Voice {
            conf_pm: Some(912),
            dur_ms: 4210,
            linked_event: linked,
        },
        text: text.into(),
        targets,
    }
}

pub fn d_rating(value: u8, targets: Vec<ContentHash>) -> EventDraft {
    EventDraft::Rating { value, targets }
}

pub fn stroke_payload() -> StrokePayload {
    StrokePayload {
        base_w: 40,
        orientation: 1,
        points: vec![
            StrokePoint {
                x: 100,
                y: 200,
                p: 1000,
                t: 0,
            },
            StrokePoint {
                x: 300,
                y: 400,
                p: 800,
                t: 16,
            },
        ],
        tool: Tool::Pencil,
    }
}

pub fn d_stroke(target: ContentHash, linked: Option<EventId>) -> EventDraft {
    EventDraft::Stroke {
        payload: stroke_payload(),
        target,
        linked_event: linked,
    }
}

pub fn d_revision(target: EventId, text: &str) -> EventDraft {
    EventDraft::Revision {
        target,
        text: text.into(),
    }
}

pub fn d_retract(target: EventId) -> EventDraft {
    EventDraft::Retraction {
        target,
        source: RetractionSource::System,
    }
}

// -- manual event construction (merge inputs) --------------------------------

/// Builds well-formed events with monotonic ids, for `merge()` inputs.
pub struct Gen {
    pub minter: Minter,
    pub session: SessionId,
}

impl Gen {
    pub fn new(session: SessionId) -> Self {
        Self {
            minter: Minter::new(),
            session,
        }
    }

    fn base(&self, source: Source, kind: Kind) -> Event {
        let m = self.minter.mint();
        Event {
            id: m.id,
            v: 1,
            session_id: self.session.clone(),
            ts: m.ts,
            source,
            kind,
            targets: Vec::new(),
            text: None,
            payload: None,
            target_event: None,
            linked_event: None,
            redacted_by: None,
        }
    }

    pub fn remark(&self, text: &str, targets: Vec<ContentHash>) -> Event {
        let mut e = self.base(Source::Typed, Kind::Remark);
        e.targets = targets;
        e.text = Some(text.into());
        e
    }

    pub fn voice_remark(&self, text: &str, targets: Vec<ContentHash>) -> Event {
        let mut e = self.base(Source::Voice, Kind::Remark);
        e.targets = targets;
        e.text = Some(text.into());
        e.payload = Some(Payload::Voice {
            conf_pm: Some(900),
            dur_ms: 1500,
        });
        e
    }

    pub fn rating(&self, value: u8, targets: Vec<ContentHash>) -> Event {
        let mut e = self.base(Source::Typed, Kind::Rating);
        e.targets = targets;
        e.payload = Some(Payload::Rating { value });
        e
    }

    pub fn stroke(&self, target: ContentHash, linked: Option<EventId>) -> Event {
        let mut e = self.base(Source::Pencil, Kind::Stroke);
        e.targets = vec![target];
        e.payload = Some(Payload::Stroke(stroke_payload()));
        e.linked_event = linked;
        e
    }

    pub fn revision(&self, target: EventId, text: &str) -> Event {
        let mut e = self.base(Source::Typed, Kind::Revision);
        e.target_event = Some(target);
        e.text = Some(text.into());
        e
    }

    pub fn retraction(&self, target: EventId) -> Event {
        let mut e = self.base(Source::System, Kind::Retraction);
        e.target_event = Some(target);
        e
    }

    pub fn redaction(&self, target: EventId) -> Event {
        let mut e = self.base(Source::System, Kind::Redaction);
        e.target_event = Some(target);
        e
    }
}

// -- inspection ---------------------------------------------------------------

/// All FTS hits for a query, as root event ids.
pub fn fts_roots(conn: &Connection, query: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT root_event_id FROM fts_map WHERE fts_rowid IN \
             (SELECT rowid FROM event_fts WHERE event_fts MATCH ?1) ORDER BY root_event_id",
        )
        .unwrap();
    let rows = stmt.query_map([query], |r| r.get::<_, String>(0)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

pub fn vector_count_for(conn: &Connection, event_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM vectors WHERE event_id = ?1",
        [event_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// Insert a fake vector row referencing a chain root (RETRIEVAL normally
/// owns this; tests use it to observe invalidation).
pub fn insert_vector(conn: &Connection, event_id: &str) {
    conn.execute(
        "INSERT INTO vectors (vec_kind, event_id, image_hash, chunk_idx, model_id, dims, vec, created_ts) \
         VALUES ('annotation_chunk', ?1, NULL, 0, 'test-model', 4, x'00000000', '2026-06-09T00:00:00.000Z')",
        [event_id],
    )
    .unwrap();
}

fn dump_query(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let ncols = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            let mut parts = Vec::with_capacity(ncols);
            for i in 0..ncols {
                let v: rusqlite::types::Value = row.get(i)?;
                parts.push(format!("{v:?}"));
            }
            Ok(parts.join("|"))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// Truth tables, ordered deterministically.
pub fn dump_truth(conn: &Connection) -> Vec<String> {
    let mut out = dump_query(
        conn,
        "SELECT id, v, session_id, ts, source, kind, text, payload, target_event, linked_event, \
                redacted_by IS NOT NULL \
         FROM annotation_events ORDER BY id",
    );
    out.extend(dump_query(
        conn,
        "SELECT event_id, image_hash, position FROM event_targets ORDER BY event_id, position",
    ));
    out
}

/// Derived state, ordered deterministically. FTS rows are keyed by root id
/// (raw rowids are an allocation detail). `since_ts` is wall-clock volatile
/// and excluded when `with_dirty_ts` is false.
pub fn dump_derived(conn: &Connection, with_dirty_ts: bool) -> Vec<String> {
    let mut out = dump_query(
        conn,
        "SELECT event_id, redaction_id, ts FROM redactions ORDER BY event_id",
    );
    out.extend(dump_query(
        conn,
        "SELECT image_hash, rating, event_id, ts FROM image_ratings ORDER BY image_hash",
    ));
    out.extend(dump_query(
        conn,
        "SELECT image_hash, event_count, has_text, has_strokes, last_ts \
         FROM image_journal_stats ORDER BY image_hash",
    ));
    out.extend(dump_query(
        conn,
        "SELECT m.root_event_id, f.body FROM fts_map m \
         LEFT JOIN event_fts f ON f.rowid = m.fts_rowid ORDER BY m.root_event_id",
    ));
    if with_dirty_ts {
        out.extend(dump_query(
            conn,
            "SELECT image_hash, reason, since_ts FROM sidecar_dirty ORDER BY image_hash",
        ));
    } else {
        out.extend(dump_query(
            conn,
            "SELECT image_hash, reason FROM sidecar_dirty ORDER BY image_hash",
        ));
    }
    out.extend(dump_query(
        conn,
        "SELECT vec_kind, event_id, image_hash, chunk_idx, model_id FROM vectors \
         ORDER BY event_id, image_hash, chunk_idx",
    ));
    out
}
