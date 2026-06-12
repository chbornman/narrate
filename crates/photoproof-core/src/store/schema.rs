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
-- `has_text` added in P4.1 (DECISIONS B34): the has-journal dot needs
-- remark-or-stroke evidence, so a rating-only journal must be
-- distinguishable without a per-image fold. Extended in place in the v1 DDL:
-- no deployed databases exist pre-dogfood (flagged in the P4.1 report).
CREATE TABLE image_journal_stats (
  image_hash   TEXT PRIMARY KEY,
  event_count  INTEGER NOT NULL,    -- live (non-retracted) events targeting the image
  has_text     INTEGER NOT NULL,    -- 0/1: any live, non-scrubbed remark (B34)
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

/// One `PRAGMA wal_checkpoint(TRUNCATE)` attempt. Returns `true` when the
/// checkpoint completed and the WAL was truncated (`busy == 0`); `false`
/// when a concurrent reader blocked it. §7 step 8 / §5.1 require the
/// truncation to actually happen — the busy result must not be treated as
/// success (it leaves scrubbed plaintext in the WAL).
pub(crate) fn checkpoint_truncate_once(conn: &Connection) -> rusqlite::Result<bool> {
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    Ok(busy == 0)
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
-- SIDECARS §11 step 4: durable propagation queue for redactions whose
-- adjacent sidecars sit on offline/unwritable volumes. Drained at volume
-- mount and at each reconciliation scan: rewrite, verify by re-read, dequeue.
-- One row per (scrubbed chain member, target image).
CREATE TABLE redaction_queue (
  event_id    TEXT NOT NULL CHECK (length(event_id) = 26),
  image_hash  TEXT NOT NULL
                CHECK (length(image_hash) = 64 AND image_hash NOT GLOB '*[^0-9a-f]*'),
  volume_id   TEXT NOT NULL DEFAULT '',   -- LIBRARY volume id when known, else ''
  queued_at   TEXT NOT NULL,
  PRIMARY KEY (event_id, image_hash)
) STRICT, WITHOUT ROWID;

-- SIDECARS §3.1/§12.1: the advisory image snapshot (filename + byte_size)
-- used when serializing a sidecar from the index. Refreshed on rewrite when
-- the image is reachable; learned from parsed sidecars otherwise. Advisory
-- only — hash always wins.
CREATE TABLE sidecar_image_snapshots (
  image_hash  TEXT PRIMARY KEY
                CHECK (length(image_hash) = 64 AND image_hash NOT GLOB '*[^0-9a-f]*'),
  filename    TEXT NOT NULL,
  byte_size   INTEGER NOT NULL CHECK (byte_size >= 0)
) STRICT, WITHOUT ROWID;

-- SIDECARS §5.2: unknown-field preservation. Unknown members found at the
-- top level ('top'), inside `image` ('image'), inside a `sessions` value
-- ('session:<ulid>'), or a whole non-conforming `sessions` value
-- ('sessions'). Keyed by the journal owner (image hash or session ulid) so
-- a rewrite from the index loses nothing. Values are compact canonical JSON.
CREATE TABLE sidecar_unknown_fields (
  owner_kind  TEXT NOT NULL CHECK (owner_kind IN ('image','session')),
  owner_key   TEXT NOT NULL,
  scope       TEXT NOT NULL,
  key         TEXT NOT NULL,
  value_json  TEXT NOT NULL,
  PRIMARY KEY (owner_kind, owner_key, scope, key)
) STRICT, WITHOUT ROWID;

-- SIDECARS §4/§5.2: events that fail v1 canonical validation (unknown kinds,
-- unknown fields, future event versions) are preserved verbatim as opaque
-- blobs: kept on rewrite, not indexed, surfaced in the integrity report.
-- sort_key = the entry's `id` member when it is a string (sorts among real
-- events), else the compact canonical bytes. Redaction supremacy scrubs
-- opaque bodies whose id enters the redaction registry (§3.4).
CREATE TABLE sidecar_opaque_events (
  owner_kind  TEXT NOT NULL CHECK (owner_kind IN ('image','session')),
  owner_key   TEXT NOT NULL,
  sort_key    TEXT NOT NULL,
  body_json   TEXT NOT NULL,
  PRIMARY KEY (owner_kind, owner_key, sort_key)
) STRICT, WITHOUT ROWID;
"#;

/// Migration slot pre-allocated to packet P2.2 (spec/LIBRARY.md tables).
/// Only P2.2 edits this constant.
///
/// spec/LIBRARY.md §6 verbatim, plus one free column addition
/// (`ingest_passes.not_before`, retry backoff eligibility — §10.5; column
/// additions are free per §6).
const LIBRARY_SCHEMA_SQL: &str = r#"
CREATE TABLE volumes (
  volume_id       TEXT PRIMARY KEY,             -- app-assigned ULID
  marker_ulid     TEXT UNIQUE,                  -- from .photoproof-volume; NULL if none
  platform_id     TEXT,                         -- platform-native id; NULL if unavailable
  platform_kind   TEXT CHECK (platform_kind IN ('macos-uuid','win-serial','linux-fsuuid','heuristic')),
  label TEXT, fs_type TEXT, capacity_bytes INTEGER,
  read_only       INTEGER NOT NULL DEFAULT 0,
  state           TEXT NOT NULL DEFAULT 'offline' CHECK (state IN ('online','offline')),
  mount_point     TEXT,                         -- current; NULL when offline
  first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
);

CREATE TABLE roots (
  root_id       TEXT PRIMARY KEY,               -- ULID
  volume_id     TEXT NOT NULL REFERENCES volumes(volume_id),
  rel_path      TEXT NOT NULL,                  -- '' = volume root; '/'-separated UTF-8
  display_name  TEXT,
  state         TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','removed')),
  created_at TEXT NOT NULL, removed_at TEXT,
  UNIQUE (volume_id, rel_path)
);

CREATE TABLE images (
  image_hash        TEXT PRIMARY KEY,           -- blake3-256, 64 lowercase hex
  byte_size         INTEGER NOT NULL,
  format            TEXT NOT NULL,              -- 'jpeg','png','tiff','webp','heic','raw'
  raw_subtype       TEXT,                       -- 'cr3','nef',…; NULL for non-RAW
  pixel_width INTEGER, pixel_height INTEGER,    -- as stored (pre-orientation)
  exif_orientation  INTEGER NOT NULL DEFAULT 1, -- 1..8; 1 if absent
  -- EXIF subset (§9.6): nullable, read-only, rebuildable
  capture_ts TEXT,                              -- RFC3339 UTC; NULL if undated
  capture_tz_offset TEXT,                       -- '+02:00' etc.
  camera_make TEXT, camera_model TEXT, lens_model TEXT,
  focal_length_mm REAL, iso INTEGER, f_number REAL,
  exposure_time TEXT, gps_lat REAL, gps_lon REAL,  -- exposure as written, e.g. '1/250'
  first_ingested_at TEXT NOT NULL
);

CREATE TABLE paths (
  path_id          TEXT PRIMARY KEY,            -- ULID
  image_hash       TEXT NOT NULL REFERENCES images(image_hash),
  volume_id        TEXT NOT NULL REFERENCES volumes(volume_id),
  root_id          TEXT REFERENCES roots(root_id),
  rel_path         TEXT NOT NULL,
  size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,  -- mtime: ns since epoch, fs precision
  state            TEXT NOT NULL CHECK (state IN ('active','stale')),
  stale_reason     TEXT CHECK (stale_reason IN ('moved','deleted','superseded','root-removed')),
  first_seen_at TEXT NOT NULL, last_verified_at TEXT NOT NULL, stale_since TEXT
);
CREATE UNIQUE INDEX paths_active_loc ON paths(volume_id, rel_path) WHERE state = 'active';
CREATE INDEX paths_by_image ON paths(image_hash, state);
CREATE INDEX paths_stale_loc ON paths(volume_id, rel_path) WHERE state = 'stale';

CREATE TABLE ingest_passes (
  image_hash    TEXT NOT NULL REFERENCES images(image_hash),
  pass_name     TEXT NOT NULL,                  -- registry, §10.1
  pass_version  INTEGER NOT NULL,
  model_id      TEXT,                           -- NULL for non-model passes
  state         TEXT NOT NULL CHECK (state IN ('pending','running','done','error','skipped')),
  priority      INTEGER NOT NULL DEFAULT 2,     -- §10.3
  attempts      INTEGER NOT NULL DEFAULT 0,
  error         TEXT,                           -- last error; NULL unless 'error'
  enqueued_at TEXT NOT NULL, started_at TEXT, completed_at TEXT,
  not_before  TEXT,                             -- retry backoff eligibility (§10.5)
  PRIMARY KEY (image_hash, pass_name, pass_version)
);
CREATE INDEX ingest_queue ON ingest_passes(state, priority, enqueued_at)
  WHERE state = 'pending';

CREATE TABLE preview_artifacts (
  image_hash         TEXT NOT NULL REFERENCES images(image_hash),
  kind               TEXT NOT NULL CHECK (kind IN ('thumb','display')),
  source             TEXT NOT NULL CHECK (source IN ('embedded','full-decode','original')),
  width INTEGER NOT NULL, height INTEGER NOT NULL,  -- display-oriented dimensions
  bytes INTEGER NOT NULL, format TEXT NOT NULL DEFAULT 'webp',
  needs_full_decode  INTEGER NOT NULL DEFAULT 0,    -- §9.3 threshold miss
  generator_version  INTEGER NOT NULL,              -- bump → regeneration (§9.8)
  generated_at       TEXT NOT NULL,
  PRIMARY KEY (image_hash, kind)
);
"#;

/// Migration slot pre-allocated to packet P3.1 (RETRIEVAL §4 search layer).
/// Only P3.1 edits this constant.
const SEARCH_SCHEMA_SQL: &str = r#"
-- RETRIEVAL §4 filter-only browse orders by capture date descending with a
-- deterministic hash tiebreak; this index makes the LIMITed page an
-- early-stop index scan instead of a full sort of `images`.
CREATE INDEX idx_images_capture_ts ON images(capture_ts DESC, image_hash);

-- Reverse stroke↔utterance resolution for Quote.linked_stroke (RETRIEVAL
-- §5.4, X2: the link lives on the later-committed event, pointing
-- backward): a batched lookup over the result set's root event ids.
CREATE INDEX idx_events_linked ON annotation_events(linked_event)
  WHERE linked_event IS NOT NULL;
"#;

/// Migration slot pre-allocated to packet P6.1 (spec/CAPTURE.md §2.3
/// session bookkeeping). Only P6.1 edits this constant.
const CAPTURE_SCHEMA_SQL: &str = r#"
-- CAPTURE §2.3: the rebuildable session bookkeeping pair (closed_clean,
-- close_processing_done). A separate capture-owned table — NOT columns on
-- `sessions` — so the EVENTS §5.2 truth shape and §8 merge semantics stay
-- untouched. Index-only, never in sidecars; absence of a row simply means
-- "no close recorded by this process" (pre-P6.1 closes, foreign rows).
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test) and the table is version-independent state.
CREATE TABLE IF NOT EXISTS capture_session_state (
  session_id             TEXT PRIMARY KEY CHECK (length(session_id) = 26),
  closed_clean           INTEGER NOT NULL CHECK (closed_clean IN (0, 1)),
  close_processing_done  INTEGER NOT NULL DEFAULT 0
                           CHECK (close_processing_done IN (0, 1))
) STRICT, WITHOUT ROWID;
"#;

/// Migration slot pre-allocated to packet P7.1 (spec/RETRIEVAL.md §1.2
/// `vectors` DDL). Only P7.1 edits this constant.
const RETRIEVAL_SCHEMA_SQL: &str = r#"
-- RETRIEVAL §1.2 supersedes the v1 reference-design `vectors` table (EVENTS
-- marked it "REFERENCE DESIGN ONLY — RETRIEVAL owns details"). Vector bytes
-- move out of SQLite into the PPVEC flat files (§1.3); this table keeps the
-- metadata: keys, file_row pointer, inputs_hash staleness, deleted flag.
-- Dropping is safe: vectors are derived, disposable, and rebuildable from
-- the event log + previews; nothing wrote rows before this packet.
DROP INDEX IF EXISTS idx_vectors_event;
DROP INDEX IF EXISTS idx_vectors_image;
DROP TABLE IF EXISTS vectors;
CREATE TABLE vectors (
  id           INTEGER PRIMARY KEY,                -- rowid
  vec_kind     TEXT    NOT NULL
               CHECK (vec_kind IN ('annotation_chunk','image_summary','image_clip')),
  model_id     TEXT    NOT NULL,                   -- Embedder::model_id()
  dims         INTEGER NOT NULL,                   -- Embedder::dimensions()
  event_id     TEXT,                               -- ULID; set iff annotation_chunk
  image_hash   TEXT,                               -- blake3 hex; set iff image_summary/image_clip
  chunk_index  INTEGER NOT NULL DEFAULT 0,         -- 0..n within one event's text
  char_start   INTEGER,                            -- char offsets (Unicode scalars) into the
  char_end     INTEGER,                            --   folded text, for quote extraction
  file_row     INTEGER NOT NULL,                   -- row index in the flat file for (vec_kind, model_id)
  inputs_hash  TEXT    NOT NULL,                   -- blake3 of embedded text (or preview bytes) +
                                                   --   prefix scheme version + instruct template
                                                   --   version (RETRIEVAL §1.2); staleness check
  created_ts   TEXT    NOT NULL,                   -- RFC 3339 UTC
  deleted      INTEGER NOT NULL DEFAULT 0,         -- awaiting compaction; search skips
  CHECK ((event_id IS NULL) <> (image_hash IS NULL))
) STRICT;
CREATE UNIQUE INDEX vectors_chunk
  ON vectors(vec_kind, model_id, event_id, chunk_index) WHERE event_id IS NOT NULL;
CREATE UNIQUE INDEX vectors_image
  ON vectors(vec_kind, model_id, image_hash) WHERE image_hash IS NOT NULL;
CREATE UNIQUE INDEX vectors_row ON vectors(vec_kind, model_id, file_row);
"#;

/// Migration slot for the P7.1 review fixes (crash-atomic PPVEC
/// compaction). Only that fix set edits this constant.
const RETRIEVAL_FIXES_SCHEMA_SQL: &str = r#"
-- Two-phase PPVEC compaction journal (RETRIEVAL §1.3 "remap file_row in one
-- transaction"): the file rename cannot live inside a SQLite transaction, so
-- compaction commits this marker WITH the remap, renames the rewritten file,
-- then clears the marker. A marker found at open means the rename may not
-- have happened yet; recovery completes (or discards) it before any read
-- can pair remapped pointers with the pre-compaction file.
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test) and a pending marker must survive that re-run.
CREATE TABLE IF NOT EXISTS ppvec_compactions (
  vec_kind  TEXT NOT NULL,
  model_id  TEXT NOT NULL,
  PRIMARY KEY (vec_kind, model_id)
) STRICT, WITHOUT ROWID;
"#;

/// Migration slot pre-allocated to packet P7.3 (spec/RETRIEVAL.md §10.1
/// collections store, B71 naming). Only P7.3 edits this constant.
const COLLECTIONS_SCHEMA_SQL: &str = r#"
-- RETRIEVAL §10.1 verbatim. These three tables are USER TRUTH (B71), not
-- index: they fail the "SQLite is a rebuildable index" test unless mirrored
-- to appdata/collections.photoproof.json (§10.2), which crate::collections
-- maintains through the SIDECARS §9 debounced-writer mechanics.
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test) and user truth must survive that re-run.
CREATE TABLE IF NOT EXISTS collections (
  id          TEXT PRIMARY KEY,               -- ULID
  name        TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  status      TEXT NOT NULL CHECK (status IN ('active','shelved','done')),
  created_ts  TEXT NOT NULL,                  -- RFC 3339 UTC
  updated_ts  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS collection_notes ( -- append-only, like the event log
  id            TEXT PRIMARY KEY,             -- ULID
  collection_id TEXT NOT NULL REFERENCES collections(id),
  ts            TEXT NOT NULL,
  text          TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS collection_members ( -- membership is evented, not destructive
  collection_id TEXT NOT NULL REFERENCES collections(id),
  image_hash    TEXT NOT NULL,
  added_ts      TEXT NOT NULL,
  removed_ts    TEXT,                         -- NULL = currently a member
  PRIMARY KEY (collection_id, image_hash, added_ts)
);
"#;

/// Create or upgrade the schema (versioned by `user_version`). Returns the
/// version found before any migration ran, so the caller can trigger
/// post-migration recomputes (the schema layer cannot run folds).
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<i64> {
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
    if version < 4 {
        conn.execute_batch(SEARCH_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 4")?;
    }
    if version < 5 {
        // v5: `image_journal_stats.has_text` (B37). Fresh databases get the
        // column from the v1 DDL; databases created before P4.1 need the
        // ALTER. Values are recomputed by the caller (EventStore::open runs
        // rebuild_derived when migrating from 1..5) — the DEFAULT here is a
        // placeholder, never trusted.
        let has_column: bool = conn.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('image_journal_stats')
             WHERE name = 'has_text'",
            [],
            |r| r.get(0),
        )?;
        if !has_column {
            conn.execute_batch(
                "ALTER TABLE image_journal_stats
                 ADD COLUMN has_text INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        run_pragma(conn, "PRAGMA user_version = 5")?;
    }
    if version < 6 {
        conn.execute_batch(CAPTURE_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 6")?;
    }
    if version < 7 {
        conn.execute_batch(RETRIEVAL_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 7")?;
    }
    if version < 8 {
        conn.execute_batch(RETRIEVAL_FIXES_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 8")?;
    }
    if version < 9 {
        conn.execute_batch(COLLECTIONS_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 9")?;
    }
    Ok(version)
}
