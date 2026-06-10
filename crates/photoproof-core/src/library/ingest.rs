//! Ingest as versioned passes: the queue IS the set of `pending` rows in
//! `ingest_passes`; `running → pending` on startup is the entire
//! crash-recovery story.
//!
//! Contract: spec/LIBRARY.md §10 (DECISIONS L4).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::id::{ContentHash, UtcMillis};

/// Pass registry (§10.1). `pass_version` starts at 1 per pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassName {
    Hash,
    Exif,
    Preview,
    FullRawDecode,
    ImageEmbedding,
    Caption,
}

impl PassName {
    pub fn as_str(self) -> &'static str {
        match self {
            PassName::Hash => "hash",
            PassName::Exif => "exif",
            PassName::Preview => "preview",
            PassName::FullRawDecode => "full-raw-decode",
            PassName::ImageEmbedding => "image-embedding",
            PassName::Caption => "caption",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hash" => PassName::Hash,
            "exif" => PassName::Exif,
            "preview" => PassName::Preview,
            "full-raw-decode" => PassName::FullRawDecode,
            "image-embedding" => PassName::ImageEmbedding,
            "caption" => PassName::Caption,
            _ => return None,
        })
    }
}

pub const PASS_VERSION: i64 = 1;

/// Pass state machine (§10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassState {
    Pending,
    Running,
    Done,
    Error,
    Skipped,
}

impl PassState {
    pub fn as_str(self) -> &'static str {
        match self {
            PassState::Pending => "pending",
            PassState::Running => "running",
            PassState::Done => "done",
            PassState::Error => "error",
            PassState::Skipped => "skipped",
        }
    }
}

/// Priorities (§10.3, lower = sooner).
pub const PRIORITY_WATCHER: i64 = 0; // P0: live-watcher discoveries
pub const PRIORITY_SCAN: i64 = 1; // P1: reconciliation / initial scans
pub const PRIORITY_BACKFILL: i64 = 2; // P2: full-raw-decode, regeneration
pub const PRIORITY_GPU: i64 = 3; // P3: model backfills

/// Retry policy (§10.5).
pub const TRANSIENT_BACKOFF_MS: [i64; 2] = [60_000, 600_000]; // 1 min, 10 min
pub const MAX_AUTO_ATTEMPTS: i64 = 3;
pub const MAX_LIFETIME_ATTEMPTS: i64 = 10;

/// Per-pass counters (§10.6).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassCounters {
    pub pending: u64,
    pub running: u64,
    pub done: u64,
    pub error: u64,
    pub skipped: u64,
}

/// One claimed work unit.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub image_hash: ContentHash,
    pub pass: PassName,
    pub pass_version: i64,
    pub attempts: i64,
}

/// Startup crash recovery (§10.2): every `running` row reverts to `pending`.
/// No leases, no heartbeats — a single process owns the DB.
pub fn recover_running(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ingest_passes SET state = 'pending', started_at = NULL
         WHERE state = 'running'",
        [],
    )
}

/// Restart / 6-hour-tick retry (§10.5): `error` rows with fewer than 10
/// lifetime attempts go back to `pending`.
pub fn retry_errors(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'pending', not_before = NULL
         WHERE state = 'error' AND attempts < ?1",
        params![MAX_LIFETIME_ATTEMPTS],
    )
}

/// Insert a pass row at enqueue (idempotent: the PK upsert keeps the
/// existing row — re-enqueueing done work is a no-op, §10.4).
pub fn enqueue(
    conn: &Connection,
    hash: &ContentHash,
    pass: PassName,
    state: PassState,
    priority: i64,
    error: Option<&str>,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO ingest_passes
           (image_hash, pass_name, pass_version, model_id, state, priority,
            attempts, error, enqueued_at, started_at, completed_at, not_before)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, 0, ?6, ?7,
                 CASE WHEN ?4 = 'done' THEN ?7 END,
                 CASE WHEN ?4 IN ('done','skipped') THEN ?7 END, NULL)
         ON CONFLICT(image_hash, pass_name, pass_version) DO NOTHING",
        params![
            hash.as_str(),
            pass.as_str(),
            PASS_VERSION,
            state.as_str(),
            priority,
            error,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Bump a pending row's priority upward (promotion rule, §10.3). Never
/// demotes.
pub fn promote(
    conn: &Connection,
    hash: &ContentHash,
    pass: PassName,
    priority: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_passes SET priority = ?3
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?4
           AND state = 'pending' AND priority > ?3",
        params![hash.as_str(), pass.as_str(), priority, PASS_VERSION],
    )?;
    Ok(())
}

/// Claim the next runnable pending row: `(priority, enqueued_at)` order
/// (§10.3), honoring retry backoff (`not_before`). Only passes with an M1
/// worker are claimed — `full-raw-decode` (M1.5) and the model passes stay
/// pending in the queue by design.
pub fn claim_next(conn: &Connection, now: UtcMillis) -> rusqlite::Result<Option<QueueItem>> {
    let now_s = now.to_rfc3339();
    let row = conn
        .query_row(
            "SELECT image_hash, pass_name, pass_version, attempts
             FROM ingest_passes
             WHERE state = 'pending'
               AND pass_name IN ('exif','preview')
               AND (not_before IS NULL OR not_before <= ?1)
             ORDER BY priority, enqueued_at
             LIMIT 1",
            params![now_s],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((hash, pass, version, attempts)) = row else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'running', started_at = ?4, attempts = attempts + 1
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
        params![hash, pass, version, now_s],
    )?;
    Ok(Some(QueueItem {
        image_hash: ContentHash::from_hex(&hash).map_err(|_| rusqlite::Error::InvalidQuery)?,
        pass: PassName::parse(&pass).ok_or(rusqlite::Error::InvalidQuery)?,
        pass_version: version,
        attempts: attempts + 1,
    }))
}

pub fn mark_done(conn: &Connection, item: &QueueItem, now: UtcMillis) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'done', error = NULL, completed_at = ?4, not_before = NULL
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
        params![
            item.image_hash.as_str(),
            item.pass.as_str(),
            item.pass_version,
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

pub fn mark_skipped(
    conn: &Connection,
    item: &QueueItem,
    error_code: &str,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'skipped', error = ?4, completed_at = ?5, not_before = NULL
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
        params![
            item.image_hash.as_str(),
            item.pass.as_str(),
            item.pass_version,
            error_code,
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

/// Record a failure (§10.5). Transient → re-`pending` with backoff (1 min
/// then 10 min) until 3 attempts, then `error`. Permanent → `error` at once.
pub fn mark_failed(
    conn: &Connection,
    item: &QueueItem,
    error: &str,
    transient: bool,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    let to_error = !transient || item.attempts >= MAX_AUTO_ATTEMPTS;
    if to_error {
        conn.execute(
            "UPDATE ingest_passes
             SET state = 'error', error = ?4, completed_at = ?5, not_before = NULL
             WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
            params![
                item.image_hash.as_str(),
                item.pass.as_str(),
                item.pass_version,
                error,
                now.to_rfc3339()
            ],
        )?;
    } else {
        let backoff_idx = ((item.attempts - 1).max(0) as usize).min(TRANSIENT_BACKOFF_MS.len() - 1);
        let not_before =
            UtcMillis::from_epoch_ms(now.epoch_ms() + TRANSIENT_BACKOFF_MS[backoff_idx]);
        conn.execute(
            "UPDATE ingest_passes
             SET state = 'pending', error = ?4, not_before = ?5
             WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
            params![
                item.image_hash.as_str(),
                item.pass.as_str(),
                item.pass_version,
                error,
                not_before.to_rfc3339()
            ],
        )?;
    }
    Ok(())
}

/// §10.6 counters: `(pass_name, pass_version) → {pending, running, done,
/// error, skipped}`.
pub fn pass_counters(conn: &Connection) -> rusqlite::Result<BTreeMap<(String, i64), PassCounters>> {
    let mut stmt = conn.prepare(
        "SELECT pass_name, pass_version, state, COUNT(*)
         FROM ingest_passes GROUP BY pass_name, pass_version, state",
    )?;
    let mut map: BTreeMap<(String, i64), PassCounters> = BTreeMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    for row in rows {
        let (name, version, state, count) = row?;
        let c = map.entry((name, version)).or_default();
        match state.as_str() {
            "pending" => c.pending = count,
            "running" => c.running = count,
            "done" => c.done = count,
            "error" => c.error = count,
            "skipped" => c.skipped = count,
            _ => {}
        }
    }
    Ok(map)
}

/// Sentinel `image_hash` for placeholder skip rows (§5.2 / P11): a
/// placeholder has no content hash by definition (its bytes were never read),
/// but the debug-panel visibility contract wants an `ingest_passes` row. The
/// sentinel is deterministic per location so re-checks update one row and
/// hydration can clear it. Flagged in the packet report (build-pass
/// resolution).
pub fn placeholder_sentinel(volume_id: &str, rel_path: &str) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"photoproof-placeholder\0");
    hasher.update(volume_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(rel_path.as_bytes());
    ContentHash::from_hex(hasher.finalize().to_hex().as_str()).expect("canonical hex")
}

/// Record (or refresh) a placeholder skip row.
pub fn record_placeholder(
    conn: &Connection,
    volume_id: &str,
    rel_path: &str,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    let sentinel = placeholder_sentinel(volume_id, rel_path);
    conn.execute(
        "INSERT INTO ingest_passes
           (image_hash, pass_name, pass_version, model_id, state, priority,
            attempts, error, enqueued_at, started_at, completed_at, not_before)
         VALUES (?1, 'hash', ?2, NULL, 'skipped', 2, 0, ?3, ?4, NULL, ?4, NULL)
         ON CONFLICT(image_hash, pass_name, pass_version)
           DO UPDATE SET completed_at = ?4",
        params![
            sentinel.as_str(),
            PASS_VERSION,
            format!("placeholder: {rel_path}"),
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

/// Clear a placeholder skip row once the path hydrates and ingests normally.
pub fn clear_placeholder(
    conn: &Connection,
    volume_id: &str,
    rel_path: &str,
) -> rusqlite::Result<()> {
    let sentinel = placeholder_sentinel(volume_id, rel_path);
    conn.execute(
        "DELETE FROM ingest_passes
         WHERE image_hash = ?1 AND pass_name = 'hash' AND state = 'skipped'",
        params![sentinel.as_str()],
    )?;
    Ok(())
}

/// All live placeholder sentinel hashes (the §5.2 re-check set).
pub fn placeholder_sentinels(
    conn: &Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT image_hash FROM ingest_passes
         WHERE pass_name = 'hash' AND state = 'skipped' AND error LIKE 'placeholder:%'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Clear specific sentinel rows (hydrated paths that re-verified on the
/// reconciliation fast path).
pub fn clear_placeholder_sentinels(
    conn: &Connection,
    sentinels: &[String],
) -> rusqlite::Result<()> {
    for s in sentinels {
        conn.execute(
            "DELETE FROM ingest_passes
             WHERE image_hash = ?1 AND pass_name = 'hash' AND state = 'skipped'",
            params![s],
        )?;
    }
    Ok(())
}
