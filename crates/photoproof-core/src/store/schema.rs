//! SQLite schema, migrations, and connection pragmas.
//!
//! Contract: spec/EVENTS.md §5 (and DECISIONS P18 for the operational
//! pragmas). The truth tables are rebuildable only from sidecars; the
//! derived tables are rebuildable from the truth tables.

use std::path::Path;

use rusqlite::Connection;

/// §5.2–5.5, verbatim from the spec.
const SCHEMA_SQL: &str = r#"
CREATE TABLE annotation_events (
  id            TEXT PRIMARY KEY CHECK (length(id) = 26),
  v             INTEGER NOT NULL DEFAULT 1,
  session_id    TEXT NOT NULL CHECK (length(session_id) = 26),
  ts            TEXT NOT NULL
                  CHECK (ts GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
  source        TEXT NOT NULL CHECK (source IN ('voice','typed','pencil','system')),
  kind          TEXT NOT NULL CHECK (kind IN
                  ('remark','rating','stroke','revision','retraction','redaction')),
  text          TEXT,
  payload       TEXT,               -- canonical JSON object (§4), kind-specific
  target_event  TEXT CHECK (target_event IS NULL OR length(target_event) = 26),
  linked_event  TEXT CHECK (linked_event IS NULL OR length(linked_event) = 26),
  redacted_by   TEXT CHECK (redacted_by IS NULL OR length(redacted_by) = 26),

  -- source × kind matrix (§2.2)
  CHECK ( (kind = 'remark'     AND source IN ('voice','typed'))
       OR (kind = 'rating'     AND source = 'typed')
       OR (kind = 'stroke'     AND source = 'pencil')
       OR (kind = 'revision'   AND source = 'typed')
       OR (kind = 'retraction' AND source IN ('voice','system'))
       OR (kind = 'redaction'  AND source = 'system') ),

  -- structural shape per kind
  CHECK ( (target_event IS NOT NULL) = (kind IN ('revision','retraction','redaction')) ),
  CHECK ( linked_event IS NULL OR kind IN ('stroke','remark') ),
  CHECK ( kind IN ('remark','revision') OR text IS NULL ),
  CHECK ( kind NOT IN ('retraction','redaction')
          OR (text IS NULL AND payload IS NULL AND redacted_by IS NULL) ),
  -- content present unless scrubbed
  CHECK ( kind NOT IN ('remark','revision') OR text IS NOT NULL OR redacted_by IS NOT NULL ),
  CHECK ( kind NOT IN ('rating','stroke') OR payload IS NOT NULL OR redacted_by IS NOT NULL ),
  CHECK ( kind <> 'rating' OR redacted_by IS NOT NULL
          OR CAST(json_extract(payload,'$.value') AS INTEGER) BETWEEN 0 AND 5 )
) STRICT;

CREATE INDEX idx_events_session ON annotation_events(session_id, id);
-- (target_event, kind): retracted(id) checks and the meta-closure's kind dispatch
-- (§6.1, §10.1) are answered from the index alone, no row fetch.
CREATE INDEX idx_events_target  ON annotation_events(target_event, kind) WHERE target_event IS NOT NULL;
-- No index on kind alone: six values, too low-selectivity to earn one. The redaction
-- registry rebuild gets a partial index instead:
CREATE INDEX idx_events_redactions ON annotation_events(id) WHERE kind = 'redaction';

CREATE TABLE event_targets (
  event_id    TEXT    NOT NULL CHECK (length(event_id) = 26),
  image_hash  TEXT    NOT NULL
                CHECK (length(image_hash) = 64 AND image_hash NOT GLOB '*[^0-9a-f]*'),
  position    INTEGER NOT NULL CHECK (position >= 0),
  PRIMARY KEY (event_id, image_hash),
  UNIQUE (event_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_targets_image ON event_targets(image_hash, event_id);

CREATE TABLE sessions (             -- storage here; lifecycle in CAPTURE (§9)
  id            TEXT PRIMARY KEY CHECK (length(id) = 26),
  started_ts    TEXT NOT NULL,
  ended_ts      TEXT,               -- NULL while open
  app_version   TEXT NOT NULL,      -- semver of the writing build
  device_id    TEXT NOT NULL,       -- 32 lowercase hex; random per install
  root_context  TEXT                -- JSON written by CAPTURE; see §9
) STRICT;

-- Redaction registry: O(1) supremacy lookup. Pure index over kind='redaction' rows.
CREATE TABLE redactions (
  event_id      TEXT PRIMARY KEY,   -- the scrubbed/condemned event
  redaction_id  TEXT NOT NULL,      -- the redaction event recording the act
  ts            TEXT NOT NULL       -- redaction event's ts
) STRICT, WITHOUT ROWID;

-- Rating fold, for structured filters.
CREATE TABLE image_ratings (
  image_hash  TEXT PRIMARY KEY,
  rating      INTEGER NOT NULL CHECK (rating BETWEEN 0 AND 5),
  event_id    TEXT NOT NULL,        -- the winning rating event
  ts          TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Journal-stats fold: one row per image with any live journal presence (P5).
CREATE TABLE image_journal_stats (
  image_hash   TEXT PRIMARY KEY,
  event_count  INTEGER NOT NULL,    -- live (non-retracted) events targeting the image
  has_strokes  INTEGER NOT NULL,    -- 0/1: any live stroke
  last_ts      TEXT NOT NULL        -- ts of the most recent live event
) STRICT, WITHOUT ROWID;

-- Durable dirty set consumed by the sidecar writer (SIDECARS owns consumption).
CREATE TABLE sidecar_dirty (
  image_hash  TEXT PRIMARY KEY,
  reason      TEXT NOT NULL CHECK (reason IN ('append','fold','redaction')),
  since_ts    TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Vectors: REFERENCE DESIGN ONLY — RETRIEVAL owns details.
CREATE TABLE vectors (
  vec_id      INTEGER PRIMARY KEY,
  vec_kind    TEXT NOT NULL CHECK (vec_kind IN ('annotation_chunk','image_summary','image_clip')),
  event_id    TEXT,                 -- chain-root event id, for annotation_chunk
  image_hash  TEXT,                 -- for image_summary / image
  chunk_idx   INTEGER NOT NULL DEFAULT 0,
  model_id    TEXT NOT NULL,
  dims        INTEGER NOT NULL,
  vec         BLOB NOT NULL,        -- f32 little-endian, dims entries
  created_ts  TEXT NOT NULL,
  CHECK ((event_id IS NULL) <> (image_hash IS NULL))
) STRICT;
CREATE INDEX idx_vectors_event ON vectors(event_id)   WHERE event_id IS NOT NULL;
CREATE INDEX idx_vectors_image ON vectors(image_hash) WHERE image_hash IS NOT NULL;

-- rowid mapping: one FTS row per chain ROOT (remark), stable across revisions (§5.4, P3).
CREATE TABLE fts_map (
  fts_rowid      INTEGER PRIMARY KEY AUTOINCREMENT,
  root_event_id  TEXT NOT NULL UNIQUE
) STRICT;

CREATE VIRTUAL TABLE event_fts USING fts5(
  body,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);

-- §7 step 8 / I8: PRAGMA secure_delete covers ordinary tables, but FTS5
-- deletes only tombstone tokens; the persistent FTS5 secure-delete option
-- makes index deletes physically overwrite token data, so scrubbed
-- plaintext cannot linger in event_fts segments.
INSERT INTO event_fts(event_fts, rank) VALUES('secure-delete', 1);

-- §5.5 append-only enforcement (defense in depth).
CREATE TRIGGER trg_events_no_delete BEFORE DELETE ON annotation_events
BEGIN SELECT RAISE(ABORT, 'annotation_events is append-only'); END;

-- The sole permitted mutation: the redaction scrub (§7).
CREATE TRIGGER trg_events_scrub_only BEFORE UPDATE ON annotation_events
WHEN NOT (  OLD.redacted_by IS NULL AND NEW.redacted_by IS NOT NULL
        AND NEW.text IS NULL AND NEW.payload IS NULL
        AND NEW.id = OLD.id AND NEW.v = OLD.v
        AND NEW.session_id = OLD.session_id AND NEW.ts = OLD.ts
        AND NEW.source = OLD.source AND NEW.kind = OLD.kind
        AND NEW.target_event IS OLD.target_event
        AND NEW.linked_event IS OLD.linked_event )
BEGIN SELECT RAISE(ABORT, 'only the redaction scrub may update annotation_events'); END;

CREATE TRIGGER trg_targets_no_update BEFORE UPDATE ON event_targets
BEGIN SELECT RAISE(ABORT, 'event_targets is append-only'); END;
CREATE TRIGGER trg_targets_no_delete BEFORE DELETE ON event_targets
BEGIN SELECT RAISE(ABORT, 'event_targets is append-only'); END;
"#;

/// Run a pragma statement, consuming an optional returned row (pragmas are
/// inconsistent about returning their new value).
pub(crate) fn run_pragma(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let _ = rows.next()?;
    Ok(())
}

/// §5.1 connection pragmas, applied to every connection.
pub(crate) fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    run_pragma(conn, "PRAGMA journal_mode = WAL")?;
    run_pragma(conn, "PRAGMA synchronous = NORMAL")?;
    run_pragma(conn, "PRAGMA secure_delete = ON")?;
    run_pragma(conn, "PRAGMA foreign_keys = OFF")?;
    run_pragma(conn, "PRAGMA cache_size = -65536")?;
    run_pragma(conn, "PRAGMA mmap_size = 268435456")?;
    run_pragma(conn, "PRAGMA temp_store = MEMORY")?;
    run_pragma(conn, "PRAGMA busy_timeout = 5000")?;
    Ok(())
}

/// Open a connection with the §5.1 pragmas applied.
pub(crate) fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

/// Migration slot pre-allocated to packet P2.1 (spec/SIDECARS.md tables).
/// Only P2.1 edits this constant.
const SIDECARS_SCHEMA_SQL: &str = r#"
-- (P2.1 sidecar tables go here)
"#;

/// Migration slot pre-allocated to packet P2.2 (spec/LIBRARY.md tables).
/// Only P2.2 edits this constant.
const LIBRARY_SCHEMA_SQL: &str = r#"
-- (P2.2 library tables go here)
"#;

/// Create the schema if the database is new (versioned by `user_version`).
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 1")?;
    }
    if version < 2 {
        conn.execute_batch(SIDECARS_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 2")?;
    }
    if version < 3 {
        conn.execute_batch(LIBRARY_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 3")?;
    }
    Ok(())
}
