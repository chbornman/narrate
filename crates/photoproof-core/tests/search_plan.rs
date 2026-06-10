//! P3.1 plan-shape gates (RETRIEVAL §4, §13.11): the MATCH is resolved
//! inside the MATERIALIZED CTE before any join — the documented 650×
//! FTS-join planner trap — and `snippet()` is evaluated for at most the
//! LIMITed hit set. Plus the search-as-you-type cancellation mechanism
//! (`sqlite3_interrupt`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use photoproof_core::id::UtcMillis;
use photoproof_core::search::{
    Comparison, Filter, SearchError, Searcher, image_hits_sql_for_tests,
};
use photoproof_core::store::EventStore;
use rusqlite::types::Value;
use rusqlite::{Connection, StatementStatus, params, params_from_iter};

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    // Create + migrate the real schema (EVENTS §5 + search migration slot).
    drop(EventStore::open(&path).unwrap());
    (dir, path)
}

/// Synthetic 26-char ULIDs, ascending.
fn ulid(i: u64) -> String {
    ulid::Ulid::from_parts(1_700_000_000_000 + i, u128::from(i)).to_string()
}

fn hash(i: u64) -> String {
    format!("{i:064x}")
}

/// Seed `n` remark roots ("the fog … barn …", all matching `"fog" "ba"*`),
/// each targeting its own image, straight into the truth + index tables —
/// the §4 statement's join spine.
fn seed_fts_corpus(path: &PathBuf, n: u64) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        let session = ulid(999_999);
        let mut ev = conn
            .prepare(
                "INSERT INTO annotation_events \
                 (id, v, session_id, ts, source, kind, text, payload) \
                 VALUES (?1, 1, ?2, ?3, 'typed', 'remark', ?4, NULL)",
            )
            .unwrap();
        let mut map = conn
            .prepare("INSERT INTO fts_map(fts_rowid, root_event_id) VALUES (?1, ?2)")
            .unwrap();
        let mut fts = conn
            .prepare("INSERT INTO event_fts(rowid, body) VALUES (?1, ?2)")
            .unwrap();
        let mut tgt = conn
            .prepare("INSERT INTO event_targets(event_id, image_hash, position) VALUES (?1, ?2, 0)")
            .unwrap();
        for i in 0..n {
            let id = ulid(i);
            let ts = UtcMillis::from_epoch_ms(1_700_000_000_000 + i as i64).to_rfc3339();
            let text = format!("the fog rolled over ridge number {i} and the barn light faded");
            ev.execute(params![id, session, ts, text]).unwrap();
            map.execute(params![i as i64 + 1, id]).unwrap();
            fts.execute(params![i as i64 + 1, text]).unwrap();
            tgt.execute(params![id, hash(i)]).unwrap();
        }
    }
    conn.execute_batch("COMMIT; ANALYZE;").unwrap();
}

/// EXPLAIN QUERY PLAN rows: (id, parent, detail).
fn plan(conn: &Connection, sql: &str, params: &[Value]) -> Vec<(i64, i64, String)> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(3)?))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Walk a node's parent chain; true if `ancestor` is on it.
fn under(plan: &[(i64, i64, String)], mut id: i64, ancestor: i64) -> bool {
    while id != 0 {
        let Some(&(_, parent, _)) = plan.iter().find(|(n, _, _)| *n == id) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        id = parent;
    }
    false
}

fn assert_plan_shape(plan: &[(i64, i64, String)]) {
    // The hits CTE is materialized.
    let materialize = plan
        .iter()
        .find(|(_, _, d)| d.starts_with("MATERIALIZE"))
        .unwrap_or_else(|| panic!("no MATERIALIZE node in plan: {plan:?}"));

    // Exactly one access to the FTS virtual table, and it lives inside the
    // materialized CTE — the MATCH is resolved before any join (§13.11).
    let vtab: Vec<&(i64, i64, String)> = plan
        .iter()
        .filter(|(_, _, d)| d.contains("VIRTUAL TABLE INDEX"))
        .collect();
    assert_eq!(vtab.len(), 1, "one FTS access expected: {plan:?}");
    assert!(
        vtab[0].1 == materialize.0 || under(plan, vtab[0].0, materialize.0),
        "MATCH must be resolved inside the materialized CTE: {plan:?}"
    );

    // FTS5 consumed `ORDER BY rank`: no sorter inside the CTE, which is
    // what keeps snippet() evaluation bounded by the LIMIT.
    assert!(
        !plan
            .iter()
            .any(|(_, _, d)| d.contains("TEMP B-TREE FOR ORDER BY")),
        "CTE ordering must be consumed by FTS5, not a temp B-tree: {plan:?}"
    );

    // The outer query is driven by the LIMITed hit set and joins through
    // fts_map → event_targets via index SEARCHes — never an FTS-driven
    // outer join.
    assert!(
        plan.iter().any(|(_, _, d)| d.starts_with("SCAN h")),
        "outer query must scan the materialized hits: {plan:?}"
    );
    assert!(
        plan.iter()
            .any(|(_, _, d)| d.starts_with("SEARCH m") && d.contains("PRIMARY KEY")),
        "fts_map must be probed by rowid: {plan:?}"
    );
    assert!(
        plan.iter().any(|(_, _, d)| d.starts_with("SEARCH t")),
        "event_targets must be probed by event_id: {plan:?}"
    );
}

#[test]
fn plan_match_resolved_in_materialized_cte() {
    let (_dir, path) = temp_db();
    seed_fts_corpus(&path, 50);
    let conn = Connection::open(&path).unwrap();
    let now = UtcMillis::now();

    // Bare statement.
    let (sql, fparams) = image_hits_sql_for_tests(&[], now).unwrap();
    let mut params = vec![Value::Text("\"fog\" \"ba\"*".into())];
    params.extend(fparams);
    assert_plan_shape(&plan(&conn, &sql, &params));

    // With filter chips (joins through images + EXISTS probes).
    let filters = vec![
        Filter::Rating(Comparison::Gte(3)),
        Filter::Camera(photoproof_core::search::StringMatch::Exact("X100V".into())),
    ];
    let (sql, fparams) = image_hits_sql_for_tests(&filters, now).unwrap();
    let mut params = vec![Value::Text("\"fog\" \"ba\"*".into())];
    params.extend(fparams);
    assert_plan_shape(&plan(&conn, &sql, &params));
}

#[test]
fn snippet_cost_bounded_by_limited_hit_set() {
    let (_dir, path) = temp_db();
    const DOCS: u64 = 20_000;
    seed_fts_corpus(&path, DOCS);
    let conn = Connection::open(&path).unwrap();
    let q = "\"fog\" \"ba\"*";

    // The corpus matches far more rows than the §4 LIMIT.
    let total: i64 = conn
        .query_row(
            "SELECT count(*) FROM event_fts WHERE event_fts MATCH ?1",
            [q],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(total as u64, DOCS);

    // The real statement.
    let (sql, _) = image_hits_sql_for_tests(&[], UtcMillis::now()).unwrap();
    let real_vm = {
        let mut stmt = conn.prepare(&sql).unwrap();
        let n = stmt.query_map([q], |_| Ok(())).unwrap().count();
        assert_eq!(n, 500, "hit set is LIMITed to 500");
        assert_eq!(
            stmt.get_status(StatementStatus::Sort),
            0,
            "FTS5 must consume the rank ordering; a sorter would evaluate \
             snippet() per candidate row pre-LIMIT (the §4 trap)"
        );
        stmt.get_status(StatementStatus::VmStep)
    };

    // The documented trap, induced deliberately: ordering by the bm25 alias
    // is not consumable by FTS5's xBestIndex, so the CTE sorts through a
    // temp B-tree and the SELECT list — snippet() included — is evaluated
    // for every matching row before the LIMIT. Same rows out, vastly more
    // work: the contrast is the bounded-cost demonstration.
    let trap_sql = sql.replace("ORDER BY rank", "ORDER BY s");
    let trap_vm = {
        let mut stmt = conn.prepare(&trap_sql).unwrap();
        let n = stmt.query_map([q], |_| Ok(())).unwrap().count();
        assert_eq!(n, 500);
        assert!(
            stmt.get_status(StatementStatus::Sort) >= 1,
            "the alias form must sort (otherwise this guard tests nothing)"
        );
        stmt.get_status(StatementStatus::VmStep)
    };

    assert!(
        i64::from(trap_vm) > i64::from(real_vm) * 5,
        "the real statement must terminate early at the LIMIT \
         (real VmStep {real_vm}, trap VmStep {trap_vm})"
    );
}

#[test]
fn interrupt_cancels_inflight_query_and_searcher_recovers() {
    let (_dir, path) = temp_db();
    seed_fts_corpus(&path, 20_000);
    let searcher = Searcher::open(&path).unwrap();

    // Race an interrupter against a search loop until one search observes
    // SQLITE_INTERRUPT; an interrupt landing between statements is a no-op,
    // so retry until it lands mid-flight.
    let interrupted = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let searcher = &searcher;
        let interrupted = &interrupted;
        scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !interrupted.load(Ordering::SeqCst) && Instant::now() < deadline {
                match searcher.search("fo ba", &[]) {
                    Ok(_) => {}
                    Err(SearchError::Interrupted) => {
                        interrupted.store(true, Ordering::SeqCst);
                    }
                    Err(e) => panic!("unexpected search error: {e}"),
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while !interrupted.load(Ordering::SeqCst) && Instant::now() < deadline {
            searcher.interrupt();
            std::thread::sleep(Duration::from_micros(500));
        }
    });
    assert!(
        interrupted.load(Ordering::SeqCst),
        "no search observed the interrupt within the deadline"
    );

    // The searcher stays usable after a cancellation.
    let results = searcher.search("fog ba", &[]).unwrap();
    assert_eq!(results.images.len(), 500);
}
