//! P3.1 latency smoke (RETRIEVAL §4, §13.1): search-as-you-type queries over
//! a generated >1M-row corpus (events + fts + targets + images at realistic
//! ratios for the reference 50k-image journal-bearing library: 150k remark
//! roots ≈ 3 per image, 100k rating events, 50k stroke events) must come
//! back inside the 100 ms budget.
//!
//! `#[ignore]`d: corpus generation makes the run too slow for the default
//! debug suite. Run it in release:
//!
//! ```text
//! cargo test -p photoproof-core --release --test search_latency -- --ignored --nocapture
//! ```
//!
//! The latency assertion only arms in release builds; debug runs still
//! execute and print the measured numbers.

use std::path::PathBuf;
use std::time::Instant;

use photoproof_core::id::UtcMillis;
use photoproof_core::search::{Comparison, Filter, Searcher};
use photoproof_core::store::EventStore;
use rusqlite::{Connection, params};

const IMAGES: u64 = 50_000;
const REMARKS: u64 = 150_000; // remark roots = FTS rows (~3 per image)
const RATING_EVENTS: u64 = 100_000;
const STROKE_EVENTS: u64 = 50_000;

/// Deterministic xorshift64* — reproducible corpus, no extra deps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Zipf-ish skew toward the front of the range.
    fn skewed(&mut self, n: u64) -> u64 {
        let r = (self.below(10_000) as f64) / 10_000.0;
        ((r * r) * n as f64) as u64
    }
}

const VOCAB: &[&str] = &[
    "the",
    "light",
    "fog",
    "barn",
    "ridge",
    "harbor",
    "quiet",
    "morning",
    "evening",
    "slow",
    "water",
    "tide",
    "winter",
    "spring",
    "dusk",
    "dawn",
    "frame",
    "keep",
    "anchor",
    "series",
    "melancholic",
    "mournful",
    "tone",
    "shadow",
    "grain",
    "contrast",
    "soft",
    "hard",
    "edge",
    "line",
    "pier",
    "boat",
    "gull",
    "rope",
    "wood",
    "stone",
    "wall",
    "field",
    "tree",
    "branch",
    "snow",
    "rain",
    "cloud",
    "bank",
    "drift",
    "swallow",
    "fade",
    "glow",
    "ember",
    "cold",
    "warm",
    "blue",
    "amber",
    "mist",
    "haze",
    "salt",
    "wind",
    "still",
    "motion",
    "blur",
    "sharp",
    "focus",
    "depth",
    "layer",
    "distance",
    "horizon",
    "silhouette",
    "figure",
    "walk",
    "stand",
    "lean",
    "wait",
    "ferry",
    "crossing",
    "north",
    "south",
    "coast",
    "cliff",
    "moss",
    "lichen",
    "tundra",
    "iceland",
    "december",
    "january",
    "study",
    "sketch",
    "note",
    "remark",
    "voice",
    "whisper",
    "loud",
    "muted",
    "palette",
    "print",
    "sequence",
    "spine",
    "open",
    "close",
    "first",
    "last",
    "best",
    "weak",
    "strong",
    "maybe",
    "definitely",
    "discard",
    "revisit",
    "compare",
    "pair",
    "triptych",
    "single",
    "wide",
    "tight",
    "low",
    "high",
    "under",
    "over",
    "between",
    "against",
    "through",
    "beyond",
    "behind",
    "before",
    "after",
    "again",
    "almost",
    "barely",
    "exactly",
    "roughly",
    "slightly",
    "deeply",
    "truly",
];

fn ulid(i: u64) -> String {
    ulid::Ulid::from_parts(1_600_000_000_000 + i, (i as u128) << 16 | 0xBEEF).to_string()
}

fn hash(i: u64) -> String {
    format!("{i:064x}")
}

fn build_corpus(path: &PathBuf) -> (u64, u64) {
    // Schema via the real migrations.
    drop(EventStore::open(path).unwrap());
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF; PRAGMA cache_size = -131072;",
    )
    .unwrap();
    let mut rng = Rng(0x0BAD_5EED_0BAD_5EED);
    let mut total_rows: u64 = 0;

    conn.execute_batch("BEGIN").unwrap();
    {
        let mut img = conn
            .prepare(
                "INSERT INTO images (image_hash, byte_size, format, capture_ts, camera_model, \
                 lens_model, first_ingested_at) VALUES (?1, ?2, 'jpeg', ?3, ?4, ?5, ?6)",
            )
            .unwrap();
        let cameras = ["X100V", "Leica M11", "Z8", "EOS R5", "GFX 100"];
        let lenses = ["23mm f/2", "Summilux 35", "40mm f/2", "RF 50mm", "GF 63mm"];
        let now = UtcMillis::now().to_rfc3339();
        for i in 0..IMAGES {
            // Capture dates spread over ~2 years.
            let capture = UtcMillis::from_epoch_ms(
                1_700_000_000_000 + (rng.below(730) * 86_400_000 + rng.below(86_400_000)) as i64,
            );
            img.execute(params![
                hash(i),
                1000 + i as i64,
                capture.to_rfc3339(),
                cameras[(i % 5) as usize],
                lenses[(i % 5) as usize],
                now
            ])
            .unwrap();
        }
        total_rows += IMAGES;

        let session = ulid(u64::MAX / 2);
        let mut ev = conn
            .prepare(
                "INSERT INTO annotation_events (id, v, session_id, ts, source, kind, text, payload) \
                 VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .unwrap();
        let mut map = conn
            .prepare("INSERT INTO fts_map(fts_rowid, root_event_id) VALUES (?1, ?2)")
            .unwrap();
        let mut fts = conn
            .prepare("INSERT INTO event_fts(rowid, body) VALUES (?1, ?2)")
            .unwrap();
        let mut tgt = conn
            .prepare(
                "INSERT INTO event_targets(event_id, image_hash, position) VALUES (?1, ?2, ?3)",
            )
            .unwrap();
        let mut text = String::with_capacity(256);
        for i in 0..REMARKS {
            let id = ulid(i);
            let ts =
                UtcMillis::from_epoch_ms(1_700_000_000_000 + (rng.below(730) * 86_400_000) as i64);
            text.clear();
            for w in 0..(6 + rng.below(13)) {
                if w > 0 {
                    text.push(' ');
                }
                text.push_str(VOCAB[rng.skewed(VOCAB.len() as u64) as usize]);
            }
            let voice = rng.below(2) == 0;
            ev.execute(params![
                id,
                session,
                ts.to_rfc3339(),
                if voice { "voice" } else { "typed" },
                "remark",
                text,
                if voice {
                    Some("{\"conf_pm\":900,\"dur_ms\":1500}")
                } else {
                    None
                },
            ])
            .unwrap();
            map.execute(params![i as i64 + 1, id]).unwrap();
            fts.execute(params![i as i64 + 1, text]).unwrap();
            // 85% single-target, 15% multi-target (2–3 images).
            let n_targets = match rng.below(100) {
                0..=84 => 1,
                85..=94 => 2,
                _ => 3,
            };
            let first = rng.below(IMAGES);
            for t in 0..n_targets {
                tgt.execute(params![id, hash((first + t * 7919) % IMAGES), t as i64])
                    .unwrap();
            }
            total_rows += 3 + n_targets;
        }

        // Rating events (structured, never FTS) + their fold rows.
        let mut rate_fold = conn
            .prepare(
                "INSERT OR REPLACE INTO image_ratings(image_hash, rating, event_id, ts) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .unwrap();
        for i in 0..RATING_EVENTS {
            let id = ulid(REMARKS + i);
            let ts =
                UtcMillis::from_epoch_ms(1_700_000_000_000 + (rng.below(730) * 86_400_000) as i64)
                    .to_rfc3339();
            let value = rng.below(6) as i64;
            ev.execute(params![
                id,
                session,
                ts,
                "typed",
                "rating",
                None::<String>,
                Some(format!("{{\"value\":{value}}}")),
            ])
            .unwrap();
            let img = rng.below(IMAGES);
            tgt.execute(params![id, hash(img), 0]).unwrap();
            // Greatest-id rating per image wins the fold; ulid(i) ascends.
            rate_fold
                .execute(params![hash(img), value, id, ts])
                .unwrap();
            total_rows += 2;
        }

        // Stroke events (payload-only, never FTS).
        for i in 0..STROKE_EVENTS {
            let id = ulid(REMARKS + RATING_EVENTS + i);
            let ts =
                UtcMillis::from_epoch_ms(1_700_000_000_000 + (rng.below(730) * 86_400_000) as i64)
                    .to_rfc3339();
            ev.execute(params![
                id,
                session,
                ts,
                "pencil",
                "stroke",
                None::<String>,
                Some(
                    "{\"base_w\":40,\"orientation\":1,\"points\":[[100,100,1000,0]],\
                     \"tool\":\"pencil\"}"
                ),
            ])
            .unwrap();
            tgt.execute(params![id, hash(rng.below(IMAGES)), 0])
                .unwrap();
            total_rows += 2;
        }
    }
    conn.execute_batch("COMMIT").unwrap();

    // Derived stats fold (P5), grouped once like rebuild does.
    let stats: u64 = conn
        .execute(
            "INSERT INTO image_journal_stats(image_hash, event_count, has_text, has_strokes, last_ts) \
             SELECT t.image_hash, COUNT(*), MAX(e.kind = 'remark'), MAX(e.kind = 'stroke'), MAX(e.ts) \
             FROM event_targets t JOIN annotation_events e ON e.id = t.event_id \
             GROUP BY t.image_hash",
            [],
        )
        .unwrap() as u64;
    total_rows += stats;

    // §4's correctness-of-performance dependency: fresh statistics + merged
    // FTS segments, exactly as the store runs them after rebuild/merge.
    conn.execute_batch("ANALYZE; INSERT INTO event_fts(event_fts) VALUES('optimize');")
        .unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();

    let fts_rows: u64 = conn
        .query_row("SELECT count(*) FROM event_fts", [], |r| r.get::<_, i64>(0))
        .unwrap() as u64;
    (total_rows + fts_rows, fts_rows)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

#[test]
#[ignore = "generates a >1M-row corpus; run in release with --ignored"]
fn latency_smoke_search_as_you_type_on_1m_row_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");

    let t0 = Instant::now();
    let (total_rows, fts_rows) = build_corpus(&path);
    println!(
        "corpus: {total_rows} rows total ({REMARKS} remarks + {RATING_EVENTS} ratings + \
         {STROKE_EVENTS} strokes, {fts_rows} fts rows, {IMAGES} images) \
         built in {:.1}s",
        t0.elapsed().as_secs_f64()
    );
    assert!(total_rows >= 1_000_000, "corpus must reach 1M rows");

    let searcher = Searcher::open(&path).unwrap();
    let chip = [Filter::Rating(Comparison::Gte(3))];

    // Search-as-you-type query stream: progressive prefixes of two-word
    // queries drawn from the corpus vocabulary (incl. the nasty 2-char
    // common prefixes the §1.1 prefix indexes exist for), half with a
    // rating chip. Deterministic via the same RNG.
    let mut rng = Rng(0xFACE_FEED_F00D_D00D);
    let mut keystrokes: Vec<(String, bool)> = Vec::new();
    for q in 0..60 {
        let w1 = VOCAB[rng.skewed(VOCAB.len() as u64) as usize];
        let w2 = VOCAB[rng.skewed(VOCAB.len() as u64) as usize];
        let full = format!("{w1} {w2}");
        let with_chip = q % 2 == 0;
        // Type it out: every prefix with the last token ≥ 2 chars (§4).
        for end in 2..=full.chars().count() {
            let prefix: String = full.chars().take(end).collect();
            if prefix
                .split_whitespace()
                .last()
                .is_none_or(|t| t.chars().count() < 2)
            {
                continue;
            }
            keystrokes.push((prefix, with_chip));
        }
    }

    // Warm the cache (the spec's prewarm analog; first-query cold start is
    // excluded from the budget per RETRIEVAL §1.3).
    for (q, with_chip) in keystrokes.iter().take(20) {
        let filters: &[Filter] = if *with_chip { &chip } else { &[] };
        searcher.search(q, filters).unwrap();
    }

    let mut timed: Vec<(f64, &str)> = Vec::with_capacity(keystrokes.len());
    let mut result_rows = 0usize;
    for (q, with_chip) in &keystrokes {
        let filters: &[Filter] = if *with_chip { &chip } else { &[] };
        let t = Instant::now();
        let r = searcher.search(q, filters).unwrap();
        timed.push((t.elapsed().as_secs_f64() * 1000.0, q.as_str()));
        result_rows += r.images.len();
    }
    timed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (ms, q) in timed.iter().rev().take(8) {
        println!("slowest: {ms:8.2} ms  {q:?}");
    }
    let samples_ms: Vec<f64> = timed.iter().map(|(ms, _)| *ms).collect();
    let (p50, p95, max) = (
        percentile(&samples_ms, 0.50),
        percentile(&samples_ms, 0.95),
        samples_ms[samples_ms.len() - 1],
    );
    println!(
        "latency over {} keystroke queries: p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.2} ms \
         ({result_rows} result rows returned)",
        samples_ms.len()
    );

    // §13.1 budget: p95 < 100 ms keystroke → result rows. Asserted in
    // release only (debug builds report without failing).
    if !cfg!(debug_assertions) {
        assert!(
            p95 < 100.0,
            "p95 {p95:.2} ms exceeds the 100 ms search-as-you-type budget"
        );
    }
}

/// Diagnostic cost split: the §4 statement (FTS rank over all matches +
/// LIMITed join) vs result/provenance assembly. Not a gate; prints data.
#[test]
#[ignore = "diagnostic breakdown; run in release with --ignored"]
fn latency_breakdown_probe() {
    use photoproof_core::search::image_hits_sql_for_tests;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    build_corpus(&path);
    let searcher = Searcher::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let (sql, _) = image_hits_sql_for_tests(&[], UtcMillis::now()).unwrap();
    let count_sql = format!("SELECT count(*) FROM ({sql})");
    for q in ["th", "the", "slow", "melanch", "fog ba", "quiet morn"] {
        let m = photoproof_core::search::fts_match_query(q).unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT count(*) FROM event_fts WHERE event_fts MATCH ?1",
                [&m],
                |r| r.get(0),
            )
            .unwrap();
        searcher.search(q, &[]).unwrap(); // warm
        let t = Instant::now();
        let n: i64 = conn.query_row(&count_sql, [&m], |r| r.get(0)).unwrap();
        let stmt_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        let r = searcher.search(q, &[]).unwrap();
        let full_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{q:12} matches={total:7} joinrows={n:5} stmt={stmt_ms:7.2}ms \
             full={full_ms:7.2}ms assembly={:7.2}ms images={}",
            full_ms - stmt_ms,
            r.images.len()
        );
    }
}
