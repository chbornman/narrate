//! Batched grid/listing reads over the shared photoproof database.
//!
//! WORKAROUND (flagged in the packet report): `photoproof-core` exposes no
//! batched folder listing, no batched `image_journal_stats`/`image_ratings`
//! reader, and no open-sessions query. Listing a 20k-item folder through the
//! public per-image APIs (`paths_for_image` + `current_rating` per hash)
//! would be an N+1 over IPC-latency-critical reads, so this module opens its
//! own **read-only** connection to the same SQLite file (WAL makes sibling
//! read connections safe; the sidecar engine does the same internally).
//! Requested exports for the coordinator:
//! - `Library::list_folder(root_id, folder) -> Vec<…>` (join incl. badges)
//! - `EventStore::journal_stats(hashes)` / `EventStore::open_sessions()`
//!
//! When those land, this module shrinks to DTO mapping.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::dto::{FolderNode, GridItem};

pub fn open_read_only(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

/// Root-relative prefix ("" or "a/b") -> the volume-relative prefix string
/// images in that folder must start with.
fn folder_prefix(root_rel: &str, folder: &str) -> String {
    let mut p = String::new();
    if !root_rel.is_empty() {
        p.push_str(root_rel);
        p.push('/');
    }
    if !folder.is_empty() {
        p.push_str(folder);
        p.push('/');
    }
    p
}

/// Direct children images of one folder under one root, with badge data
/// (has-journal dot, folded rating, offline-volume badge) in one query.
pub fn list_folder(
    conn: &Connection,
    root_id: &str,
    root_rel: &str,
    folder: &str,
) -> rusqlite::Result<Vec<GridItem>> {
    let prefix = folder_prefix(root_rel, folder);
    // has_journal: the dulled-red dot is evidence of *annotations* — words and
    // marks. Rating keys must produce "no visual change to any thumbnail"
    // (UI §3.7), so a rating-only journal does not light the dot: live
    // journal presence (image_journal_stats, P5/B4) AND remark-or-stroke
    // evidence. Flagged: an exact `has_text` column on image_journal_stats
    // would remove the EXISTS (and the all-remarks-retracted-but-rated edge).
    let mut stmt = conn.prepare_cached(
        "SELECT p.rel_path, p.image_hash, i.capture_ts, i.first_ingested_at,
                (COALESCE(s.event_count, 0) > 0 AND EXISTS (
                   SELECT 1 FROM event_targets t
                   JOIN annotation_events e ON e.id = t.event_id
                   WHERE t.image_hash = p.image_hash
                     AND e.kind IN ('remark','stroke')
                )) AS has_journal,
                r.rating,
                NOT EXISTS (
                  SELECT 1 FROM paths p2
                  JOIN volumes v2 ON v2.volume_id = p2.volume_id
                  WHERE p2.image_hash = p.image_hash
                    AND p2.state = 'active' AND v2.state = 'online'
                ) AS offline
         FROM paths p
         JOIN images i ON i.image_hash = p.image_hash
         LEFT JOIN image_journal_stats s ON s.image_hash = p.image_hash
         LEFT JOIN image_ratings r ON r.image_hash = p.image_hash
         WHERE p.root_id = ?1 AND p.state = 'active'
           AND substr(p.rel_path, 1, length(?2)) = ?2
           AND instr(substr(p.rel_path, length(?2) + 1), '/') = 0
         ORDER BY p.rel_path",
    )?;
    let root_prefix_len = if root_rel.is_empty() {
        0
    } else {
        root_rel.len() + 1
    };
    let rows = stmt.query_map(params![root_id, prefix], |r| {
        let rel: String = r.get(0)?;
        let root_relative = rel.get(root_prefix_len..).unwrap_or("").to_owned();
        let file_name = root_relative
            .rsplit('/')
            .next()
            .unwrap_or(&root_relative)
            .to_owned();
        Ok(GridItem {
            hash: r.get(1)?,
            file_name,
            rel_path: root_relative,
            capture_ts: r.get(2)?,
            added_ts: r.get(3)?,
            has_journal: r.get::<_, i64>(4)? != 0,
            rating: r.get::<_, Option<i64>>(5)?.map(|v| v as u8),
            offline: r.get::<_, i64>(6)? != 0,
        })
    })?;
    rows.collect()
}

/// Folder tree of one root (the rail), derived from active path rows.
pub fn folder_tree(
    conn: &Connection,
    root_id: &str,
    root_rel: &str,
) -> rusqlite::Result<Vec<FolderNode>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT rel_path FROM paths WHERE root_id = ?1 AND state = 'active'",
    )?;
    let root_prefix_len = if root_rel.is_empty() {
        0
    } else {
        root_rel.len() + 1
    };
    let rels: Vec<String> = stmt
        .query_map(params![root_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;

    // Collect every ancestor directory (root-relative).
    let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for rel in &rels {
        let Some(rr) = rel.get(root_prefix_len..) else {
            continue;
        };
        let mut acc = String::new();
        let segs: Vec<&str> = rr.split('/').collect();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            dirs.insert(acc.clone());
        }
    }
    Ok(build_tree(&dirs))
}

fn build_tree(dirs: &std::collections::BTreeSet<String>) -> Vec<FolderNode> {
    // BTreeSet iterates parents before children (prefix order), so a simple
    // map from path -> node assembled bottom-up works.
    let mut roots: Vec<FolderNode> = Vec::new();
    let mut index: BTreeMap<String, FolderNode> = BTreeMap::new();
    for d in dirs {
        let name = d.rsplit('/').next().unwrap_or(d).to_owned();
        index.insert(
            d.clone(),
            FolderNode {
                name,
                rel_path: d.clone(),
                children: Vec::new(),
            },
        );
    }
    // Attach children to parents, deepest first so parents collect built
    // subtrees.
    let keys: Vec<String> = index.keys().rev().cloned().collect();
    for k in keys {
        let node = index.remove(&k).expect("present");
        match k.rsplit_once('/') {
            Some((parent, _)) if index.contains_key(parent) => {
                index
                    .get_mut(parent)
                    .expect("parent present")
                    .children
                    .insert(0, node);
            }
            _ => roots.insert(0, node),
        }
    }
    roots
}

/// Crash recovery (CAPTURE §2.4): sessions left open by a dead process.
pub fn open_sessions(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM sessions WHERE ended_ts IS NULL ORDER BY id")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// `ended_at` for a recovered session = ts of its last event, else None
/// (caller falls back to `started_ts`).
pub fn last_event_ts_ms(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT ts FROM annotation_events WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
        params![session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map(|opt| {
        opt.and_then(|s| {
            photoproof_core::UtcMillis::parse(&s)
                .ok()
                .map(|t| t.epoch_ms())
        })
    })
}

pub fn session_started_ms(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT started_ts FROM sessions WHERE id = ?1",
        params![session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map(|opt| {
        opt.and_then(|s| {
            photoproof_core::UtcMillis::parse(&s)
                .ok()
                .map(|t| t.epoch_ms())
        })
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Seed helpers for headless command-layer tests: insert library rows
    //! directly (the real writers live behind ingest, which needs files on
    //! disk; tests assert the read side).

    use rusqlite::{Connection, params};

    pub fn seed_volume(conn: &Connection, volume_id: &str, online: bool) {
        conn.execute(
            "INSERT INTO volumes (volume_id, state, mount_point, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            params![
                volume_id,
                if online { "online" } else { "offline" },
                if online { Some("/mnt/test") } else { None }
            ],
        )
        .unwrap();
    }

    pub fn seed_root(conn: &Connection, root_id: &str, volume_id: &str, rel_path: &str) {
        conn.execute(
            "INSERT INTO roots (root_id, volume_id, rel_path, display_name, state, created_at)
             VALUES (?1, ?2, ?3, 'Test', 'active', '2026-01-01T00:00:00Z')",
            params![root_id, volume_id, rel_path],
        )
        .unwrap();
    }

    pub fn seed_image(conn: &Connection, hash: &str, capture_ts: Option<&str>) {
        conn.execute(
            "INSERT INTO images (image_hash, byte_size, format, first_ingested_at, capture_ts)
             VALUES (?1, 1000, 'jpeg', '2026-02-01T00:00:00Z', ?2)",
            params![hash, capture_ts],
        )
        .unwrap();
    }

    pub fn seed_path(
        conn: &Connection,
        path_id: &str,
        hash: &str,
        volume_id: &str,
        root_id: &str,
        rel_path: &str,
    ) {
        conn.execute(
            "INSERT INTO paths (path_id, image_hash, volume_id, root_id, rel_path, size,
                                mtime_ns, state, first_seen_at, last_verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1000, 0, 'active',
                     '2026-02-01T00:00:00Z', '2026-02-01T00:00:00Z')",
            params![path_id, hash, volume_id, root_id, rel_path],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use photoproof_core::{ContentHash, EventDraft, EventStore, RemarkSource, SessionContext};

    struct Fixture {
        _dir: tempfile::TempDir,
        store: EventStore,
        rw: Connection,
        ro: Connection,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("photoproof.db");
        let store = EventStore::open(&db).unwrap();
        let rw = Connection::open(&db).unwrap();
        let ro = open_read_only(&db).unwrap();
        Fixture {
            _dir: dir,
            store,
            rw,
            ro,
        }
    }

    fn ctx() -> SessionContext {
        SessionContext {
            app_version: "0.1.0".into(),
            device_id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            root_context: None,
        }
    }

    fn h(n: u8) -> ContentHash {
        ContentHash::from_bytes_of(&[n])
    }

    #[test]
    fn list_folder_returns_direct_children_with_badges() {
        let f = fixture();
        seed_volume(&f.rw, "vol1", true);
        seed_root(&f.rw, "root1", "vol1", "photos");
        let (a, b, c) = (h(1), h(2), h(3));
        for (i, hash) in [&a, &b, &c].iter().enumerate() {
            seed_image(&f.rw, hash.as_str(), Some("2026-03-01T00:00:00Z"));
            seed_path(
                &f.rw,
                &format!("p{i}"),
                hash.as_str(),
                "vol1",
                "root1",
                &format!("photos/iceland/img_{i}.jpg"),
            );
        }
        // A nested image must NOT appear in the folder's direct listing.
        let nested = h(4);
        seed_image(&f.rw, nested.as_str(), None);
        seed_path(
            &f.rw,
            "p9",
            nested.as_str(),
            "vol1",
            "root1",
            "photos/iceland/sub/deep.jpg",
        );

        // Journal + rating on image a (derived tables maintained by append).
        let sid = f.store.open_session(ctx()).unwrap();
        f.store
            .append(
                &sid,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "the hand is the whole picture".into(),
                    targets: vec![a.clone()],
                },
                None,
            )
            .unwrap();
        f.store
            .append(
                &sid,
                EventDraft::Rating {
                    value: 4,
                    targets: vec![b.clone()],
                },
                None,
            )
            .unwrap();

        let items = list_folder(&f.ro, "root1", "photos", "iceland").unwrap();
        assert_eq!(items.len(), 3, "direct children only");
        let find = |hash: &ContentHash| items.iter().find(|i| i.hash == hash.as_str()).unwrap();
        assert!(find(&a).has_journal);
        assert_eq!(find(&a).rating, None);
        // UI §3.7: rating keys cause no visual change to any thumbnail — a
        // rating-only journal must not light the has-journal dot.
        assert!(!find(&b).has_journal);
        assert_eq!(find(&b).rating, Some(4));
        assert!(!find(&c).has_journal);
        assert_eq!(find(&c).rating, None);
        assert_eq!(find(&a).rel_path, "iceland/img_0.jpg");
        assert_eq!(find(&a).file_name, "img_0.jpg");
        assert!(!find(&a).offline);
    }

    #[test]
    fn offline_badge_when_only_paths_are_on_offline_volumes() {
        let f = fixture();
        seed_volume(&f.rw, "vol-on", true);
        seed_volume(&f.rw, "vol-off", false);
        seed_root(&f.rw, "root1", "vol-off", "");
        let only_offline = h(10);
        seed_image(&f.rw, only_offline.as_str(), None);
        seed_path(
            &f.rw,
            "p1",
            only_offline.as_str(),
            "vol-off",
            "root1",
            "img.jpg",
        );
        let also_online = h(11);
        seed_image(&f.rw, also_online.as_str(), None);
        seed_path(
            &f.rw,
            "p2",
            also_online.as_str(),
            "vol-off",
            "root1",
            "img2.jpg",
        );
        seed_path(
            &f.rw,
            "p3",
            also_online.as_str(),
            "vol-on",
            "root1",
            "img2.jpg",
        );

        let items = list_folder(&f.ro, "root1", "", "").unwrap();
        let find = |hash: &ContentHash| items.iter().find(|i| i.hash == hash.as_str()).unwrap();
        assert!(find(&only_offline).offline);
        assert!(!find(&also_online).offline);
    }

    #[test]
    fn rating_zero_is_explicit_and_distinct_from_unrated() {
        // DECISIONS C6: 0 = explicit clear; the fold (EVENTS E4) keeps the
        // last live rating — value 0 — distinct from never-rated NULL.
        let f = fixture();
        seed_volume(&f.rw, "vol1", true);
        seed_root(&f.rw, "root1", "vol1", "");
        let rated_zero = h(20);
        let unrated = h(21);
        for (i, hash) in [&rated_zero, &unrated].iter().enumerate() {
            seed_image(&f.rw, hash.as_str(), None);
            seed_path(
                &f.rw,
                &format!("pz{i}"),
                hash.as_str(),
                "vol1",
                "root1",
                &format!("z{i}.jpg"),
            );
        }
        let sid = f.store.open_session(ctx()).unwrap();
        f.store
            .append(
                &sid,
                EventDraft::Rating {
                    value: 3,
                    targets: vec![rated_zero.clone()],
                },
                None,
            )
            .unwrap();
        f.store
            .append(
                &sid,
                EventDraft::Rating {
                    value: 0,
                    targets: vec![rated_zero.clone()],
                },
                None,
            )
            .unwrap();
        let items = list_folder(&f.ro, "root1", "", "").unwrap();
        let find = |hash: &ContentHash| items.iter().find(|i| i.hash == hash.as_str()).unwrap();
        assert_eq!(find(&rated_zero).rating, Some(0));
        assert_eq!(find(&unrated).rating, None);
    }

    #[test]
    fn folder_tree_builds_nested_dirs() {
        let f = fixture();
        seed_volume(&f.rw, "vol1", true);
        seed_root(&f.rw, "root1", "vol1", "photos");
        let img = h(30);
        seed_image(&f.rw, img.as_str(), None);
        seed_image(&f.rw, h(31).as_str(), None);
        seed_path(
            &f.rw,
            "t1",
            img.as_str(),
            "vol1",
            "root1",
            "photos/2026/iceland/a.jpg",
        );
        seed_path(
            &f.rw,
            "t2",
            h(31).as_str(),
            "vol1",
            "root1",
            "photos/2026/harbor/b.jpg",
        );
        let tree = folder_tree(&f.ro, "root1", "photos").unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "2026");
        assert_eq!(tree[0].rel_path, "2026");
        let names: Vec<&str> = tree[0].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["harbor", "iceland"]);
        assert_eq!(tree[0].children[1].rel_path, "2026/iceland");
    }

    #[test]
    fn open_sessions_and_last_event_ts_support_crash_recovery() {
        let f = fixture();
        let sid = f.store.open_session(ctx()).unwrap();
        assert_eq!(open_sessions(&f.ro).unwrap(), vec![sid.as_str().to_owned()]);
        let ev = f
            .store
            .append(
                &sid,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "note".into(),
                    targets: vec![],
                },
                None,
            )
            .unwrap();
        let got = last_event_ts_ms(&f.ro, sid.as_str()).unwrap().unwrap();
        assert_eq!(got, ev.ts.epoch_ms());
        f.store
            .close_session(&sid, photoproof_core::UtcMillis::now())
            .unwrap();
        assert!(open_sessions(&f.ro).unwrap().is_empty());
    }
}
