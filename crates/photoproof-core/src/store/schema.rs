//! SQLite schema, migrations, and connection pragmas.
//!
//! Contract: spec/EVENTS.md §5 (and DECISIONS P18 for the operational
//! pragmas). The truth tables are rebuildable only from sidecars; the
//! derived tables are rebuildable from the truth tables.

use std::path::Path;

use rusqlite::Connection;

use super::StoreError;

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

/// EVENTS.md §5.1 normative `busy_timeout` (ms): "writer + read pool share
/// one file". Spec-pinned, not a tuning knob — every connection to the
/// events database (the store's writer and read pool, the sidecar engine's
/// sibling connection, the checkpoint restore path) must keep this same
/// posture, so all of them name this constant. The library writer reuses it
/// too: its pragmas deliberately mirror §5.1 (DECISIONS P18). Exported
/// (`store::BUSY_TIMEOUT_MS`) because the desktop shell's debug-panel
/// raw-tail reader opens its own connection to the same file and must not
/// silently diverge from the spec value.
pub const BUSY_TIMEOUT_MS: u64 = 5000;

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
    run_pragma(conn, &format!("PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}"))?;
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
  -- 'archived' (v14): a lifecycle resting state, hidden from the active rail
  -- but NON-DESTRUCTIVE — unlike 'removed' it leaves `paths` and every image
  -- journal/collection membership untouched, so an archived root restores
  -- whole. The new value is on the constraint here for fresh DBs; existing
  -- DBs get it through the v14 table-rebuild migration.
  state         TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','archived','removed')),
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

/// Migration slot pre-allocated to packet P7.2 (the `derived_summaries`
/// half of the spec/RETRIEVAL.md §9 common-storage DDL + the §1.1
/// `summaries_fts` table this spec owns; the §9 `sentiment_scores` half
/// lives in the v11 fix slot below). Only P7.2 edits this constant.
const SUMMARIES_SCHEMA_SQL: &str = r#"
-- RETRIEVAL §9 common storage (the table only; the generation passes are
-- M2b/M3 work). Derived rows are retrieval FUEL ONLY (E4): never rendered
-- as prose in the product, disposable, rebuildable from the event log.
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test); these rows are derived and re-creatable, but
-- the re-run must not error on existing tables.
CREATE TABLE IF NOT EXISTS derived_summaries (
  id           TEXT PRIMARY KEY,              -- ULID
  scope        TEXT NOT NULL CHECK (scope IN ('image','folder','session')),
  scope_key    TEXT NOT NULL,                 -- image_hash | folder path | session_id
  text         TEXT NOT NULL,
  model_id     TEXT NOT NULL,
  prompt_ver   INTEGER NOT NULL,
  inputs_hash  TEXT NOT NULL,                 -- blake3 over sorted input event ids +
                                              --   folded-text hashes + prompt_ver
  generated_ts TEXT NOT NULL,
  UNIQUE (scope, scope_key, model_id)         -- one live row per scope+model
) STRICT;

-- RETRIEVAL §1.1: one row per derived summary; its own down-weighted ranked
-- list in fusion (§5.3). Same tokenizer recipe as event_fts (normative).
CREATE VIRTUAL TABLE IF NOT EXISTS summaries_fts USING fts5(
  text,
  summary_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);

-- Mirror event_fts hygiene: redaction deletes dependent summaries (§9.5),
-- and the scrub must not leave paraphrase tokens in dead FTS segments.
INSERT INTO summaries_fts(summaries_fts, rank) VALUES('secure-delete', 1);
"#;

/// Migration slot for the P7.2 review fixes. Only that fix set edits this
/// constant: the `sentiment_scores` half of the RETRIEVAL §9 common-storage
/// DDL, which the P7.2 packet cut without naming the cut.
const SUMMARIES_FIXES_SCHEMA_SQL: &str = r#"
-- RETRIEVAL §9 common storage, second table. §9.4 gates sentiment as
-- experimental — rows are written but consumed by nothing until the M3
-- evaluation passes — but the table must exist now: the §9.5/§1.1
-- redaction propagation ("delete the chain's sentiment rows", same
-- transaction as the content scrub) ships with the live summaries query
-- path and needs a table to act on, and the M2b/M3 writers expect the §9
-- DDL whole.
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test); rows are derived and re-creatable, but the
-- re-run must not error on an existing table.
CREATE TABLE IF NOT EXISTS sentiment_scores (
  event_id   TEXT PRIMARY KEY,                -- one score per text event
  score      INTEGER NOT NULL CHECK (score BETWEEN -2 AND 2),
  model_id   TEXT NOT NULL,
  prompt_ver INTEGER NOT NULL,
  scored_ts  TEXT NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Migration slot for the attention/engagement heatmap
/// (DESIGN-ATTENTION-HEATMAP.md). Only the heatmap packet edits this constant.
///
/// `image_dwell` — local, machine-observed dwell telemetry, kept SEPARATE from
/// the annotation event log (K14: the journal stays the user's own
/// words/marks). It is NOT in sidecars and never leaves the machine; a missing
/// row simply means "no observed dwell". Rebuildable-index rules do not apply
/// — it is fresh capture, not a fold of the log, so `rebuild_derived` preserves
/// it (like `sidecar_dirty`).
/// IF NOT EXISTS: migrations re-run on user_version regressions (the v5
/// downgrade-simulation test) and captured telemetry must survive that re-run.
const HEATMAP_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS image_dwell (
  image_hash   TEXT PRIMARY KEY,
  dwell_ms     INTEGER NOT NULL,
  focus_count  INTEGER NOT NULL,
  last_ts      TEXT NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Migration slot for manual topics (DESIGN-TOPICS-COLLECTIONS.md). Only the
/// topics packet edits this constant.
///
/// A `topic` is a SAVED PHRASE (like a saved search), not stored membership:
/// its images are ALWAYS computed affinity at read time (`topic_ranked_images`),
/// which is precisely what distinguishes a topic (continuous, fuzzy, a lens)
/// from a collection (discrete, evented, durable). So this table holds ONLY the
/// phrase + which embedding space to pull it in + when it was saved. No member
/// rows, no notes, no portability mirror: a topic is cheap, regenerable intent,
/// not user truth that must survive a rebuild (unlike collections, which ARE
/// mirrored to collections.photoproof.json). Losing a saved phrase costs the
/// user one retype, not a curated decision.
const TOPICS_SCHEMA_SQL: &str = r#"
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test) and a saved phrase must survive that re-run.
CREATE TABLE IF NOT EXISTS topics (
  id          TEXT PRIMARY KEY,               -- ULID
  phrase      TEXT NOT NULL,
  space       TEXT,                           -- NULL = blend both (the default);
                                              --   'annotation' | 'clip' to pin one
  created_ts  TEXT NOT NULL                   -- RFC 3339 UTC
);
"#;

/// Migration slot for the per-topic note log (founder decision: give topics
/// their own append-only running note, mirroring `collection_notes`). Only the
/// topic-notes packet edits this constant.
///
/// A topic note is the user's authored text ABOUT the topic — its definition,
/// what it is for, the refinement intent — keyed to the topic id, exactly the
/// shape `collection_notes` has for a collection. Append-only, never edited or
/// deleted (K14: the record preserves the user's own words). WHY this is the
/// ONE thing topics now persist that survives more than a retype: unlike the
/// saved phrase (cheap, regenerable intent — see TOPICS_SCHEMA_SQL above), a
/// note is curated authored text, so it is real user truth on the topic id.
const TOPIC_NOTES_SCHEMA_SQL: &str = r#"
-- IF NOT EXISTS: migrations re-run on user_version regressions (the v5
-- downgrade-simulation test) and an authored note must survive that re-run.
CREATE TABLE IF NOT EXISTS topic_notes ( -- append-only, like collection_notes
  id       TEXT PRIMARY KEY,             -- ULID
  topic_id TEXT NOT NULL REFERENCES topics(id),
  ts       TEXT NOT NULL,
  text     TEXT NOT NULL
);
"#;

/// The highest `user_version` this build knows how to produce. Bump this in
/// lockstep with the last `if version < N` block below. It is the upper bound
/// the downgrade guard enforces: a DB stamped higher than this was written by a
/// newer app and must not be opened by this one.
pub(crate) const CURRENT_VERSION: i64 = 15;

/// Create or upgrade the schema (versioned by `user_version`). Returns the
/// version found before any migration ran, so the caller can trigger
/// post-migration recomputes (the schema layer cannot run folds).
///
/// Refuses a database NEWER than [`CURRENT_VERSION`] (a downgrade): the old
/// binary cannot safely read a schema a newer app upgraded, and the migration
/// ladder only moves forward, so opening it would silently operate on a
/// partially-understood schema. We surface `IncompatibleVersion` instead.
pub(crate) fn migrate(conn: &Connection) -> Result<i64, StoreError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > CURRENT_VERSION {
        return Err(StoreError::IncompatibleVersion {
            found: version,
            supported: CURRENT_VERSION,
        });
    }
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
    if version < 10 {
        conn.execute_batch(SUMMARIES_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 10")?;
    }
    if version < 11 {
        conn.execute_batch(SUMMARIES_FIXES_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 11")?;
    }
    if version < 12 {
        // v12: the attention/engagement heatmap. The `image_dwell` telemetry
        // table, plus `image_journal_stats.stroke_count` (the heatmap's small
        // stroke factor — `has_strokes` already carries presence). Fresh
        // databases would still have no `stroke_count` column (it post-dates
        // the v1 DDL), so the ALTER is guarded like v5's `has_text`. The
        // recompute (EventStore::open runs rebuild_derived when migrating)
        // backfills the value — the DEFAULT here is a placeholder, never
        // trusted.
        conn.execute_batch(HEATMAP_SCHEMA_SQL)?;
        let has_column: bool = conn.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('image_journal_stats')
             WHERE name = 'stroke_count'",
            [],
            |r| r.get(0),
        )?;
        if !has_column {
            conn.execute_batch(
                "ALTER TABLE image_journal_stats
                 ADD COLUMN stroke_count INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        run_pragma(conn, "PRAGMA user_version = 12")?;
    }
    if version < 13 {
        // v13: manual topics (DESIGN-TOPICS-COLLECTIONS.md). A saved phrase
        // table only — a topic's images are always computed affinity, never
        // stored membership (that is what distinguishes it from a collection).
        conn.execute_batch(TOPICS_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 13")?;
    }
    if version < 14 {
        // v14: the root 'archived' lifecycle state (folder-tree improvements).
        // SQLite cannot widen a column CHECK in place, so the constraint is
        // rebuilt the canonical way: build the new table, copy rows, swap. The
        // copy is non-destructive — every existing root keeps its id, state,
        // and timestamps, so journals and collection memberships (keyed off the
        // image hash, never the root) are wholly untouched. Guarded by a column
        // probe so a fresh DB (already carrying the widened CHECK from the v1
        // DDL) skips the rebuild.
        let already_widened: bool = conn.query_row(
            // The fresh-DDL `roots` table's CHECK text contains 'archived';
            // the pre-v14 one does not. sqlite_master holds the creating SQL.
            "SELECT count(*) > 0 FROM sqlite_master
             WHERE type = 'table' AND name = 'roots' AND sql LIKE '%archived%'",
            [],
            |r| r.get(0),
        )?;
        if !already_widened {
            conn.execute_batch(
                "CREATE TABLE roots_v14 (
                   root_id       TEXT PRIMARY KEY,
                   volume_id     TEXT NOT NULL REFERENCES volumes(volume_id),
                   rel_path      TEXT NOT NULL,
                   display_name  TEXT,
                   state         TEXT NOT NULL DEFAULT 'active'
                                   CHECK (state IN ('active','archived','removed')),
                   created_at TEXT NOT NULL, removed_at TEXT,
                   UNIQUE (volume_id, rel_path)
                 );
                 INSERT INTO roots_v14
                   SELECT root_id, volume_id, rel_path, display_name, state,
                          created_at, removed_at
                   FROM roots;
                 DROP TABLE roots;
                 ALTER TABLE roots_v14 RENAME TO roots;",
            )?;
        }
        run_pragma(conn, "PRAGMA user_version = 14")?;
    }
    if version < 15 {
        // v15: the per-topic note log (topic_notes), mirroring collection_notes
        // (user_version 9). A topic gets an append-only running note keyed to
        // its id; this adds the table only, never touching how topic membership
        // /affinity is computed (still always read-time, never stored).
        conn.execute_batch(TOPIC_NOTES_SCHEMA_SQL)?;
        run_pragma(conn, "PRAGMA user_version = 15")?;
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The v14 migration widens `roots.state` to admit 'archived' by rebuilding
    /// the table, and it must do so NON-DESTRUCTIVELY: a pre-v14 database's
    /// rows survive verbatim, and the new state is accepted afterwards.
    #[test]
    fn v14_widens_roots_state_check_and_preserves_rows() {
        let conn = Connection::open_in_memory().unwrap();
        // Stand up the schema, then recreate `roots` in its PRE-v14 shape (the
        // narrow CHECK) so we exercise the table-rebuild branch a real upgrade
        // would take.
        migrate(&conn).unwrap();
        conn.execute_batch(
            "DROP TABLE roots;
             CREATE TABLE roots (
               root_id TEXT PRIMARY KEY,
               volume_id TEXT NOT NULL REFERENCES volumes(volume_id),
               rel_path TEXT NOT NULL,
               display_name TEXT,
               state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','removed')),
               created_at TEXT NOT NULL, removed_at TEXT,
               UNIQUE (volume_id, rel_path)
             );
             INSERT INTO volumes (volume_id, state, first_seen_at, last_seen_at)
               VALUES ('vol1', 'offline', 'x', 'x');
             INSERT INTO roots (root_id, volume_id, rel_path, display_name, state, created_at)
               VALUES ('r1', 'vol1', 'photos', 'Photos', 'active', 'x');",
        )
        .unwrap();
        run_pragma(&conn, "PRAGMA user_version = 13").unwrap();

        // 'archived' is rejected by the narrow CHECK before the migration.
        assert!(
            conn.execute(
                "UPDATE roots SET state = 'archived' WHERE root_id = 'r1'",
                []
            )
            .is_err(),
            "pre-v14 CHECK forbids 'archived'"
        );

        migrate(&conn).unwrap();

        // Row preserved verbatim through the rebuild.
        let (rel, name): (String, String) = conn
            .query_row(
                "SELECT rel_path, display_name FROM roots WHERE root_id = 'r1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((rel.as_str(), name.as_str()), ("photos", "Photos"));

        // 'archived' now accepted.
        conn.execute(
            "UPDATE roots SET state = 'archived' WHERE root_id = 'r1'",
            [],
        )
        .expect("v14 CHECK admits 'archived'");
        // A full migrate lands on the LATEST version (bumped to 15 by the
        // topic_notes migration, which runs after the v14 roots rebuild).
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 15);
    }

    /// A database stamped NEWER than this build supports (a downgrade) is refused
    /// rather than opened with partial schema knowledge (STATE-INTEGRITY-AUDIT.md
    /// "newer-version DB opens silently"). A current/older DB still opens.
    #[test]
    fn refuses_a_newer_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap(); // bring it to CURRENT_VERSION
        // Simulate a newer app having upgraded the DB.
        run_pragma(
            &conn,
            &format!("PRAGMA user_version = {}", CURRENT_VERSION + 1),
        )
        .unwrap();
        let err = migrate(&conn).unwrap_err();
        match err {
            StoreError::IncompatibleVersion { found, supported } => {
                assert_eq!(found, CURRENT_VERSION + 1);
                assert_eq!(supported, CURRENT_VERSION);
            }
            other => panic!("expected IncompatibleVersion, got {other:?}"),
        }
        // Exactly-current is fine (idempotent re-open), and so is older.
        run_pragma(&conn, &format!("PRAGMA user_version = {CURRENT_VERSION}")).unwrap();
        assert!(migrate(&conn).is_ok());
    }
}
