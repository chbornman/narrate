//! Path rows: claims about where a hash's bytes were last observed.
//!
//! Contract: spec/LIBRARY.md §3 (paths and states), §3.1 (current-best-path
//! resolution), §8 (derived availability), §1.3 (in-place overwrite
//! protocol), §7.4 (relink invariant).

use rusqlite::{Connection, OptionalExtension, params};

use crate::id::{ContentHash, UtcMillis};

/// `stale_reason` column values (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    Moved,
    Deleted,
    Superseded,
    RootRemoved,
}

impl StaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleReason::Moved => "moved",
            StaleReason::Deleted => "deleted",
            StaleReason::Superseded => "superseded",
            StaleReason::RootRemoved => "root-removed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "moved" => StaleReason::Moved,
            "deleted" => StaleReason::Deleted,
            "superseded" => StaleReason::Superseded,
            "root-removed" => StaleReason::RootRemoved,
            _ => return None,
        })
    }
}

/// One `paths` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRow {
    pub path_id: String,
    pub image_hash: ContentHash,
    pub volume_id: String,
    pub root_id: Option<String>,
    pub rel_path: String,
    pub size: i64,
    pub mtime_ns: i64,
    pub active: bool,
    pub stale_reason: Option<StaleReason>,
}

pub(crate) fn row_to_path(r: &rusqlite::Row<'_>) -> rusqlite::Result<PathRow> {
    Ok(PathRow {
        path_id: r.get(0)?,
        image_hash: ContentHash::from_hex(&r.get::<_, String>(1)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        volume_id: r.get(2)?,
        root_id: r.get(3)?,
        rel_path: r.get(4)?,
        size: r.get(5)?,
        mtime_ns: r.get(6)?,
        active: r.get::<_, String>(7)? == "active",
        stale_reason: r
            .get::<_, Option<String>>(8)?
            .as_deref()
            .and_then(StaleReason::parse),
    })
}

const PATH_COLUMNS: &str = "path_id, image_hash, volume_id, root_id, rel_path, size, mtime_ns, \
                            state, stale_reason";
const PATH_COLUMNS_P: &str = "p.path_id, p.image_hash, p.volume_id, p.root_id, p.rel_path, \
                              p.size, p.mtime_ns, p.state, p.stale_reason";

/// The `active` row at `(volume_id, rel_path)`, if any (unique by index).
pub fn active_row_at(
    conn: &Connection,
    volume_id: &str,
    rel_path: &str,
) -> rusqlite::Result<Option<PathRow>> {
    conn.query_row(
        &format!(
            "SELECT {PATH_COLUMNS} FROM paths
             WHERE volume_id = ?1 AND rel_path = ?2 AND state = 'active'"
        ),
        params![volume_id, rel_path],
        row_to_path,
    )
    .optional()
}

/// All rows for an image, active first.
pub fn rows_for_image(conn: &Connection, hash: &ContentHash) -> rusqlite::Result<Vec<PathRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PATH_COLUMNS} FROM paths WHERE image_hash = ?1
         ORDER BY state, volume_id, rel_path"
    ))?;
    let rows = stmt.query_map(params![hash.as_str()], row_to_path)?;
    rows.collect()
}

/// Insert a fresh `active` path row (relink or first observation). The §7.4
/// relink invariant holds by construction: nothing else is touched here.
#[allow(clippy::too_many_arguments)]
pub fn insert_active(
    conn: &Connection,
    path_id: &str,
    hash: &ContentHash,
    volume_id: &str,
    root_id: Option<&str>,
    rel_path: &str,
    size: i64,
    mtime_ns: i64,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    let now_s = now.to_rfc3339();
    conn.execute(
        "INSERT INTO paths (path_id, image_hash, volume_id, root_id, rel_path,
                            size, mtime_ns, state, stale_reason,
                            first_seen_at, last_verified_at, stale_since)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', NULL, ?8, ?8, NULL)",
        params![
            path_id,
            hash.as_str(),
            volume_id,
            root_id,
            rel_path,
            size,
            mtime_ns,
            now_s
        ],
    )?;
    Ok(())
}

pub fn mark_stale(
    conn: &Connection,
    path_id: &str,
    reason: StaleReason,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE paths SET state = 'stale', stale_reason = ?2, stale_since = ?3
         WHERE path_id = ?1 AND state = 'active'",
        params![path_id, reason.as_str(), now.to_rfc3339()],
    )?;
    Ok(())
}

/// Reactivate a `root-removed` stale row whose location/size/mtime still
/// match — the §5 re-registration fast path ("relinks everything by hash"
/// with zero re-hashing; the hash is the one the stale row already carries).
pub fn reactivate(
    conn: &Connection,
    path_id: &str,
    root_id: &str,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE paths SET state = 'active', stale_reason = NULL, stale_since = NULL,
                          root_id = ?2, last_verified_at = ?3
         WHERE path_id = ?1",
        params![path_id, root_id, now.to_rfc3339()],
    )?;
    Ok(())
}

pub fn touch_verified(conn: &Connection, path_id: &str, now: UtcMillis) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE paths SET last_verified_at = ?2 WHERE path_id = ?1",
        params![path_id, now.to_rfc3339()],
    )?;
    Ok(())
}

pub fn update_size_mtime(
    conn: &Connection,
    path_id: &str,
    size: i64,
    mtime_ns: i64,
    now: UtcMillis,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE paths SET size = ?2, mtime_ns = ?3, last_verified_at = ?4
         WHERE path_id = ?1",
        params![path_id, size, mtime_ns, now.to_rfc3339()],
    )?;
    Ok(())
}

/// §7.2 R: when a relink lands for `hash`, any of its rows recently marked
/// `stale/deleted` (within the 10 s correlation window, or inside the same
/// scan batch) flips to `moved` — a move never re-ingests.
pub fn correlate_move(
    conn: &Connection,
    hash: &ContentHash,
    window_start: UtcMillis,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE paths SET stale_reason = 'moved'
         WHERE image_hash = ?1 AND state = 'stale' AND stale_reason = 'deleted'
           AND stale_since >= ?2",
        params![hash.as_str(), window_start.to_rfc3339()],
    )
}

/// Stale `superseded` rows at a location: the dormant-prior-version
/// breadcrumbs (§1.3). Newest first.
pub fn dormant_prior_versions(
    conn: &Connection,
    volume_id: &str,
    rel_path: &str,
) -> rusqlite::Result<Vec<ContentHash>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT image_hash FROM paths
         WHERE volume_id = ?1 AND rel_path = ?2
           AND state = 'stale' AND stale_reason = 'superseded'
         ORDER BY stale_since DESC",
    )?;
    let rows = stmt.query_map(params![volume_id, rel_path], |r| {
        let h: String = r.get(0)?;
        ContentHash::from_hex(&h).map_err(|_| rusqlite::Error::InvalidQuery)
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// Availability (§8) — derived, never stored
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// ≥ 1 active path on an online volume.
    Available,
    /// Active paths exist, all on offline volumes.
    Offline,
    /// No active paths (all stale).
    Missing,
}

pub fn availability(conn: &Connection, hash: &ContentHash) -> rusqlite::Result<Availability> {
    let (active, online): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COUNT(CASE WHEN v.state = 'online' THEN 1 END)
         FROM paths p JOIN volumes v ON v.volume_id = p.volume_id
         WHERE p.image_hash = ?1 AND p.state = 'active'",
        params![hash.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(if online > 0 {
        Availability::Available
    } else if active > 0 {
        Availability::Offline
    } else {
        Availability::Missing
    })
}

/// §3.1 resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BestPath {
    pub row: PathRow,
    /// Volume online? When `false`, the UI badges the path with the §8 state.
    pub online: bool,
    /// Mount point when online (absolute path = mount_point / rel_path).
    pub mount_point: Option<String>,
}

/// Current-best-path resolution (§3.1): among `active` rows — (1) online
/// beats offline; (2) under an active watched root beats outside; (3)
/// writable beats read-only; (4) most recent `last_verified_at`; (5)
/// lexicographically smallest `(volume_id, rel_path)`. If nothing is online,
/// the best offline row is returned with `online = false`.
pub fn best_path(conn: &Connection, hash: &ContentHash) -> rusqlite::Result<Option<BestPath>> {
    conn.query_row(
        &format!(
            "SELECT {PATH_COLUMNS_P}, v.state = 'online' AS online, v.mount_point
             FROM paths p
             JOIN volumes v ON v.volume_id = p.volume_id
             LEFT JOIN roots r ON r.root_id = p.root_id
             WHERE p.image_hash = ?1 AND p.state = 'active'
             ORDER BY (v.state = 'online') DESC,
                      (r.state = 'active') DESC,
                      (v.read_only = 0) DESC,
                      p.last_verified_at DESC,
                      p.volume_id ASC, p.rel_path ASC
             LIMIT 1"
        ),
        params![hash.as_str()],
        |r| {
            Ok(BestPath {
                row: row_to_path(r)?,
                online: r.get::<_, i64>(9)? != 0,
                mount_point: r.get(10)?,
            })
        },
    )
    .optional()
}
