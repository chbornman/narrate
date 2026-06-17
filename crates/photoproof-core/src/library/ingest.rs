//! Ingest as versioned passes: the queue IS the set of `pending` rows in
//! `ingest_passes`; `running → pending` on startup is the entire
//! crash-recovery story.
//!
//! Contract: spec/LIBRARY.md §10 (DECISIONS L4).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::id::{ContentHash, UtcMillis};

/// Pass registry (§10.1). `pass_version` starts at 1 per pass.
/// `text-embedding` is P7.1's addition: RETRIEVAL §3 makes annotation-chunk
/// embedding "a versioned backfill pass (LIBRARY.md mechanics)", and the
/// queue is per-image, so the pass unit is "(re)embed every live chunk of
/// the events targeting this image".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassName {
    Hash,
    Exif,
    Preview,
    FullRawDecode,
    ImageEmbedding,
    TextEmbedding,
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
            PassName::TextEmbedding => "text-embedding",
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
            "text-embedding" => PassName::TextEmbedding,
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
///
/// `PRIORITY_INTERACTIVE` is the NEW top priority (June 2026, on-demand
/// full-raw-decode): a user staring at a "developing..." spinner in Look is
/// the most urgent work in the queue, ahead of even the live folder-watcher.
/// The view-time develop trigger enqueues at this priority; nothing else
/// does. (Renumbered so it sorts first while every existing constant keeps
/// its relative order.)
pub const PRIORITY_INTERACTIVE: i64 = 0; // P-1: view-time on-demand develop
pub const PRIORITY_WATCHER: i64 = 1; // P0: live-watcher discoveries
pub const PRIORITY_SCAN: i64 = 2; // P1: reconciliation / initial scans
pub const PRIORITY_BACKFILL: i64 = 3; // P2: regeneration (§9.8)
pub const PRIORITY_GPU: i64 = 4; // P3: model backfills

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
/// lifetime attempts go back to `pending`. Volume-offline rows are
/// rescued REGARDLESS of attempts, with a fresh budget: the lifetime cap
/// exists to stop reprocessing bad FILES, and databases from before the
/// defer_offline fix hold rows that burned all 10 on a flapping volume
/// (founder-machine find, June 2026).
pub fn retry_errors(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'pending', not_before = NULL,
             attempts = CASE WHEN error LIKE 'volume-offline%' THEN 0 ELSE attempts END
         WHERE state = 'error'
           AND (attempts < ?1 OR error LIKE 'volume-offline%')",
        params![MAX_LIFETIME_ATTEMPTS],
    )
}

/// An offline volume says nothing about the FILE: re-pend with the long
/// transient backoff and GIVE THE ATTEMPT BACK — neither the §10.5
/// auto-retry cap nor the lifetime cap may burn down while a volume is
/// merely unplugged. (The volume's online transition clears `not_before`
/// and re-pends ahead of the backoff anyway.)
pub fn defer_offline(conn: &Connection, item: &QueueItem, now: UtcMillis) -> rusqlite::Result<()> {
    defer(conn, item, "volume-offline: no online active path", now)
}

/// Re-pend with the long transient backoff, giving the attempt back: for
/// blockers that say nothing about the work unit itself (offline volume,
/// a prerequisite pass not finished yet). The error code is recorded for
/// debug-panel visibility.
pub fn defer(
    conn: &Connection,
    item: &QueueItem,
    error_code: &str,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    let backoff = TRANSIENT_BACKOFF_MS[TRANSIENT_BACKOFF_MS.len() - 1];
    let not_before = UtcMillis::from_epoch_ms(now.epoch_ms() + backoff);
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'pending', error = ?4, not_before = ?5,
             attempts = MAX(attempts - 1, 0)
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3",
        params![
            item.image_hash.as_str(),
            item.pass.as_str(),
            item.pass_version,
            error_code,
            not_before.to_rfc3339()
        ],
    )?;
    Ok(())
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
    // Exif + Preview both READ the original file, so an image whose only paths
    // are on an offline volume cannot proceed: skip it at claim time (true)
    // instead of claiming → discovering offline → deferring on every wake. That
    // claim/defer sweep is the "volume offline" log-spam churn (founder, June
    // 2026); skipping at claim lets the pump idle, and the volume's online
    // transition re-pends these rows (mark_online_locked) so work resumes.
    claim_next_of(conn, now, &[PassName::Exif, PassName::Preview], true)
}

/// Claim the next runnable pending row among `allowed` passes. The
/// embedding drain claims only the passes whose embedder is configured —
/// unconfigured model passes sit pending (idle, NotConfigured-style; never
/// errors), exactly like the rest of the degraded posture.
///
/// `require_online_path`: when true, only claim an image that has an ACTIVE path
/// on an ONLINE volume. File-reading passes (hash/exif/preview/full-raw-decode)
/// set this so an offline drive does not drive a perpetual claim→defer sweep;
/// the embedding passes set it false (they read cached previews/text, which
/// survive a drive going offline).
pub fn claim_next_of(
    conn: &Connection,
    now: UtcMillis,
    allowed: &[PassName],
    require_online_path: bool,
) -> rusqlite::Result<Option<QueueItem>> {
    if allowed.is_empty() {
        return Ok(None);
    }
    let now_s = now.to_rfc3339();
    // Pass names are registry constants, never user input; the IN list is
    // assembled from quoted static strings.
    let in_list = allowed
        .iter()
        .map(|p| format!("'{}'", p.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    // Gate file-reading passes on an online location (see `require_online_path`).
    // `image_hash` correlates to the outer row; `paths_by_image` indexes it.
    let online_filter = if require_online_path {
        "AND EXISTS (SELECT 1 FROM paths p JOIN volumes v ON v.volume_id = p.volume_id \
         WHERE p.image_hash = ingest_passes.image_hash AND p.state = 'active' \
         AND v.state = 'online')"
    } else {
        ""
    };
    let row = conn
        .query_row(
            &format!(
                "SELECT image_hash, pass_name, pass_version, attempts
                 FROM ingest_passes
                 WHERE state = 'pending'
                   AND pass_name IN ({in_list})
                   AND (not_before IS NULL OR not_before <= ?1)
                   {online_filter}
                 ORDER BY priority, enqueued_at
                 LIMIT 1"
            ),
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

/// Complete a claimed item — guarded to `running` rows only. The events
/// engine re-pends a RUNNING text-embedding pass when a journal change
/// lands mid-run (the pass snapshotted the old folded text); an
/// unconditional UPDATE here would clobber that re-pend back to 'done' and
/// the new words would never reach the vector index. A no-op completion
/// leaves the row 'pending' and the drain loop simply re-claims it.
pub fn mark_done(conn: &Connection, item: &QueueItem, now: UtcMillis) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'done', error = NULL, completed_at = ?4, not_before = NULL
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3
           AND state = 'running'",
        params![
            item.image_hash.as_str(),
            item.pass.as_str(),
            item.pass_version,
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

/// Complete a MODEL pass (image/text embedding), RECORDING the embedder's
/// `model_id` on the row. The plain [`mark_done`] leaves `model_id` NULL, which
/// made pass completion model-BLIND: swapping the CLIP/text model left every
/// pass `done` so the library was never re-embedded, and topic affinities
/// silently scored against a vector space the new model never wrote (the fp16
/// CLIP default regression, June 2026). Recording the model here is what lets
/// [`repend_passes_for_model`] detect a model change and re-pend. Same
/// `running`-only guard as [`mark_done`] (an events-engine re-pend mid-run must
/// win).
pub fn mark_done_with_model(
    conn: &Connection,
    item: &QueueItem,
    model_id: &str,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_passes
         SET state = 'done', error = NULL, completed_at = ?4, not_before = NULL,
             model_id = ?5
         WHERE image_hash = ?1 AND pass_name = ?2 AND pass_version = ?3
           AND state = 'running'",
        params![
            item.image_hash.as_str(),
            item.pass.as_str(),
            item.pass_version,
            now.to_rfc3339(),
            model_id
        ],
    )?;
    Ok(())
}

/// Skip `error` codes whose `skipped` row is TRANSIENT — the input does not
/// exist YET, but the image is real and SHOULD embed into the active space once
/// the input lands. Distinguished from PERMANENT skips (`root-removed`: the
/// image has no active path at all — nothing to embed; force-pending it would
/// only re-defer/churn the drain forever, which is exactly what that skip was
/// created to stop). Only these transient skips are revived on a model swap
/// (Seam 2 re-embed contract). Currently just preview-deferred HEIC/RAW: the
/// preview pass deferred until the decode worker lands, so the image-embedding
/// pass skipped with no preview to read (`embedding.rs::run_image_embedding_pass`).
const TRANSIENT_SKIP_CODES: &[&str] = &["preview-deferred"];

/// Re-pend a MODEL pass for re-embedding into the active model's space — the
/// Seam 2 "re-embed contract" (`docs/ARCHITECTURE-CONTRACTS.md`): on a model
/// swap, EVERY image that legitimately needs a vector in the new space is
/// re-pended, so none is silently left in the old space showing partial signal
/// forever.
///
/// Two cohorts are revived:
/// 1. **`done` rows whose recorded `model_id` differs** from the embedder now
///    configured (a NULL legacy row — written before [`mark_done_with_model`]
///    existed — counts as "different" and re-pends once). Pass completion is
///    otherwise model-blind, so without this a changed model scores against a
///    vector space it never wrote. This is what DETECTS a real swap, and is
///    naturally idempotent: the re-run records the current model, so the next
///    call is a no-op.
/// 2. **Transiently-`skipped` rows** ([`TRANSIENT_SKIP_CODES`]) AND
///    fewer-than-lifetime-cap **`error` rows** — but ONLY when cohort 1 found a
///    real swap. These rows carry no `model_id` (they never produced a vector),
///    so they can't self-gate on a model-change predicate; gating them on "a
///    `done` row actually changed model this call" keeps them from flipping
///    skipped→pending→skipped on every drain (churn) while still giving them a
///    fresh attempt at the NEW model when the space genuinely changes. A
///    PERMANENT skip (`root-removed` — no active path, nothing to embed) is left
///    alone by the code allow-list, and `pending` rows already point at the
///    active model. `running` rows are never touched (an events-engine mid-run
///    re-pend must win — see [`mark_done`]).
///
/// Returns the total rows re-pended across both cohorts.
pub fn repend_passes_for_model(
    conn: &Connection,
    pass: PassName,
    current_model_id: &str,
) -> rusqlite::Result<usize> {
    // Cohort 1: stale `done` rows. This count IS the "a real swap happened"
    // signal that gates cohort 2 below. Priority is left untouched so a watcher
    // P0 row keeps its lane (Seam 2: "WITHOUT demoting watcher P0 priority").
    let done_repended = conn.execute(
        "UPDATE ingest_passes
         SET state = 'pending', started_at = NULL, completed_at = NULL,
             not_before = NULL, error = NULL, attempts = 0
         WHERE pass_name = ?1 AND pass_version = ?2 AND state = 'done'
           AND (model_id IS NULL OR model_id <> ?3)",
        params![pass.as_str(), PASS_VERSION, current_model_id],
    )?;

    // Cohort 2: only react to a GENUINE swap (a done row changed model). With no
    // stale done rows there is nothing already embedded in an old space, so a
    // transient-skipped/error row is just normal not-yet-embeddable work the
    // pending path covers — reviving it here would only churn skipped↔pending.
    if done_repended == 0 {
        return Ok(0);
    }

    // The transient-skip allow-list is a small static set of registry strings,
    // never user input; the IN list is assembled from quoted constants.
    let transient_in = TRANSIENT_SKIP_CODES
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(",");
    let revived = conn.execute(
        &format!(
            "UPDATE ingest_passes
             SET state = 'pending', started_at = NULL, completed_at = NULL,
                 not_before = NULL, error = NULL, attempts = 0
             WHERE pass_name = ?1 AND pass_version = ?2
               AND (
                 (state = 'skipped' AND error IN ({transient_in}))
                 OR (state = 'error' AND attempts < ?3)
               )"
        ),
        params![pass.as_str(), PASS_VERSION, MAX_LIFETIME_ATTEMPTS],
    )?;

    Ok(done_repended + revived)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Connection) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("photoproof.db");
        // EventStore::open runs the schema migrations (mirrors Library::open).
        drop(crate::store::EventStore::open(&db).unwrap());
        let conn = crate::library::open_library_connection(&db).unwrap();
        (tmp, conn)
    }

    /// The events engine re-pends a RUNNING text-embedding pass when a
    /// journal change lands mid-run; a stale `mark_done` from the drain
    /// must not clobber that re-pend back to 'done', or the new words
    /// never reach the vector index (RETRIEVAL §1.1/§1.2).
    #[test]
    fn mark_done_only_completes_rows_still_running() {
        let (_tmp, conn) = setup();
        let hash = ContentHash::from_hex(&"ab".repeat(32)).unwrap();
        let now = UtcMillis::now();
        enqueue(
            &conn,
            &hash,
            PassName::TextEmbedding,
            PassState::Pending,
            PRIORITY_GPU,
            None,
            now,
        )
        .unwrap();
        let item = claim_next_of(&conn, now, &[PassName::TextEmbedding], false)
            .unwrap()
            .expect("claimable");

        // Simulate the mid-run re-pend (store::recompute_derived hook).
        conn.execute("UPDATE ingest_passes SET state = 'pending'", [])
            .unwrap();
        mark_done(&conn, &item, now).unwrap();
        let state: String = conn
            .query_row("SELECT state FROM ingest_passes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "pending", "stale completion must not win");

        // The unraced path still completes normally.
        let item = claim_next_of(&conn, now, &[PassName::TextEmbedding], false)
            .unwrap()
            .expect("claimable again");
        mark_done(&conn, &item, now).unwrap();
        let state: String = conn
            .query_row("SELECT state FROM ingest_passes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "done");
    }

    /// Volume-offline "pause" (founder: warn + pause, June 2026): a file-reading
    /// pass (Exif/Preview) for an image whose only active path is on an OFFLINE
    /// volume is skipped at claim time, so the pump idles instead of sweeping the
    /// whole offline backlog (claim→defer) on every wake. Embedding passes (which
    /// read cached previews) stay claimable, and the file pass resumes the moment
    /// the volume is back online.
    #[test]
    fn claim_skips_offline_volume_for_file_passes_not_embeddings() {
        let (_tmp, conn) = setup();
        let now = UtcMillis::now();
        let ts = now.to_rfc3339();
        let hash = ContentHash::from_hex(&"cd".repeat(32)).unwrap();
        conn.execute(
            "INSERT INTO volumes (volume_id, state, first_seen_at, last_seen_at)
             VALUES ('vol-1', 'offline', ?1, ?1)",
            params![ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO paths (path_id, image_hash, volume_id, rel_path, size,
                                mtime_ns, state, first_seen_at, last_verified_at)
             VALUES ('p1', ?1, 'vol-1', 'a.jpg', 1, 1, 'active', ?2, ?2)",
            params![hash.as_str(), ts],
        )
        .unwrap();
        enqueue(
            &conn,
            &hash,
            PassName::Preview,
            PassState::Pending,
            PRIORITY_WATCHER,
            None,
            now,
        )
        .unwrap();
        enqueue(
            &conn,
            &hash,
            PassName::TextEmbedding,
            PassState::Pending,
            PRIORITY_GPU,
            None,
            now,
        )
        .unwrap();

        // Offline: the file pass is NOT claimed (no claim/defer churn)...
        assert!(
            claim_next(&conn, now).unwrap().is_none(),
            "an offline-only image yields no file-reading claim"
        );
        // ...but the embedding pass runs anyway (cached previews survive offline).
        assert!(
            claim_next_of(&conn, now, &[PassName::TextEmbedding], false)
                .unwrap()
                .is_some(),
            "embedding is claimable while the source volume is offline"
        );

        // Volume returns: the file pass is claimable again.
        conn.execute(
            "UPDATE volumes SET state = 'online' WHERE volume_id = 'vol-1'",
            [],
        )
        .unwrap();
        let item = claim_next(&conn, now)
            .unwrap()
            .expect("file pass claimable once the volume is online");
        assert_eq!(item.pass, PassName::Preview);
    }

    /// Seam 2 re-embed contract (`docs/ARCHITECTURE-CONTRACTS.md`): a model swap
    /// must re-pend EVERY image that legitimately needs a vector in the new
    /// space — not just `done` rows. A TRANSIENTLY-`skipped` image-embedding pass
    /// (`preview-deferred` HEIC whose preview will land later) was silently left
    /// in the old space forever; it must be revived on the swap. A PERMANENT skip
    /// (`root-removed` — no file to embed) must be left alone, and a RUNNING row
    /// must never be disturbed.
    #[test]
    fn model_swap_repends_transient_skip_not_permanent_skip() {
        let (_tmp, conn) = setup();
        let now = UtcMillis::now();
        let ts = now.to_rfc3339();
        let pass = PassName::ImageEmbedding;

        // A `done` row recorded under the OLD model — this is what makes the call
        // recognize a genuine swap (cohort 1).
        let done_hash = ContentHash::from_hex(&"11".repeat(32)).unwrap();
        enqueue(
            &conn,
            &done_hash,
            pass,
            PassState::Pending,
            PRIORITY_GPU,
            None,
            now,
        )
        .unwrap();
        let done_item = claim_next_of(&conn, now, &[pass], false).unwrap().unwrap();
        mark_done_with_model(&conn, &done_item, "old-model", now).unwrap();

        // A TRANSIENTLY-skipped row: real image, preview deferred until HEIC decode.
        let skip_hash = ContentHash::from_hex(&"22".repeat(32)).unwrap();
        enqueue(
            &conn,
            &skip_hash,
            pass,
            PassState::Pending,
            PRIORITY_GPU,
            None,
            now,
        )
        .unwrap();
        let skip_item = claim_next_of(&conn, now, &[pass], false).unwrap().unwrap();
        mark_skipped(&conn, &skip_item, "preview-deferred", now).unwrap();

        // A PERMANENTLY-skipped row: orphaned image, no active path — nothing to
        // embed. Reviving it would only re-defer/churn the drain.
        let orphan_hash = ContentHash::from_hex(&"33".repeat(32)).unwrap();
        conn.execute(
            "INSERT INTO ingest_passes
               (image_hash, pass_name, pass_version, model_id, state, priority,
                attempts, error, enqueued_at, started_at, completed_at, not_before)
             VALUES (?1, ?2, ?3, NULL, 'skipped', ?4, 0, 'root-removed', ?5, NULL, ?5, NULL)",
            params![
                orphan_hash.as_str(),
                pass.as_str(),
                PASS_VERSION,
                PRIORITY_GPU,
                ts
            ],
        )
        .unwrap();

        // A RUNNING row must never be disturbed by the swap.
        let running_hash = ContentHash::from_hex(&"44".repeat(32)).unwrap();
        enqueue(
            &conn,
            &running_hash,
            pass,
            PassState::Pending,
            PRIORITY_GPU,
            None,
            now,
        )
        .unwrap();
        claim_next_of(&conn, now, &[pass], false).unwrap().unwrap(); // -> running

        // Swap to a NEW model.
        let repended = repend_passes_for_model(&conn, pass, "new-model").unwrap();

        let state_of = |hash: &ContentHash| -> String {
            conn.query_row(
                "SELECT state FROM ingest_passes WHERE image_hash = ?1 AND pass_name = ?2",
                params![hash.as_str(), pass.as_str()],
                |r| r.get(0),
            )
            .unwrap()
        };

        // done (old model) + transient skip are revived; permanent skip + running are NOT.
        assert_eq!(
            state_of(&done_hash),
            "pending",
            "stale-model done must re-pend"
        );
        assert_eq!(
            state_of(&skip_hash),
            "pending",
            "transiently-skipped (preview-deferred) must re-pend on swap"
        );
        assert_eq!(
            state_of(&orphan_hash),
            "skipped",
            "permanently-skipped (root-removed) must be left alone"
        );
        assert_eq!(
            state_of(&running_hash),
            "running",
            "a running row must never be disturbed by a swap"
        );
        assert_eq!(
            repended, 2,
            "exactly the done + transient-skip rows re-pended"
        );

        // Idempotent: drain every now-pending embed row to `done` under the NEW
        // model (as the real drain would), then a second swap call to the SAME
        // model is a no-op — the transient skip does NOT churn skipped<->pending
        // every drain, because cohort 2 only fires when a `done` row's model
        // genuinely changed.
        while let Some(item) = claim_next_of(&conn, now, &[pass], false).unwrap() {
            mark_done_with_model(&conn, &item, "new-model", now).unwrap();
        }
        let noop = repend_passes_for_model(&conn, pass, "new-model").unwrap();
        assert_eq!(
            noop, 0,
            "no genuine swap -> no re-pend (no skipped<->pending churn)"
        );
    }
}
