//! P4.1 D — perf budget smoke tests. Each prints its measurement (run with
//! `--nocapture` to record numbers); hard budget assertions apply in release
//! only (debug builds get a generous sanity allowance, like B8).
//!
//! Budgets (machine-relative; reference numbers recorded in
//! docs/DOGFOOD-M1.md):
//! - fold of a 5k-event journal (post-merge state): ≤ 10 ms release (I16)
//! - search p95 over a synthetic library with REAL paths/previews joined:
//!   < 100 ms (RETRIEVAL §13.1; the 50k variant is `#[ignore]`)
//! - ingest throughput on the synthetic tree (recorded, no hard budget here:
//!   LIBRARY §12 budgets are NVMe-hardware-relative)
//! - first-1k-thumbnails proxy (LIBRARY L7: ≤ 60 s on founder hardware)
//! - rebuild-from-sidecars at 50k events (recorded; `#[ignore]` full scale)

mod common;

use std::time::Instant;

use photoproof_core::library::{ArtifactKind, QueueOptions};
use photoproof_core::search::{Comparison, Filter, SearchOptions};
use photoproof_core::sidecar::{RebuildOptions, rebuild_from_sidecars};
use photoproof_core::{EventDraft, RemarkSource, UtcMillis};

use common::m1env::M1Env;
use common::synthlib::{self, Rng, SynthSpec};
use common::{Gen, hash, new_store};

/// Serializes the measurements in this binary: parallel test threads
/// otherwise contend for cores and inflate every timing.
fn guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    match LOCK.get_or_init(|| std::sync::Mutex::new(())).lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn scaled(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

// ---------------------------------------------------------------------------
// D1 — fold timing, post-merge state (extends I16's worst case with a
// revision-chain-heavy composition: 600 revisions vs I16's 50)
// ---------------------------------------------------------------------------

#[test]
fn d1_fold_5k_events_post_merge_under_budget() {
    let _g = guard();
    let t = new_store();
    let g = Gen::new(t.session.clone());
    let image = hash(1);
    // 5k-event worst case on ONE image, arriving via merge (the backup/
    // restore path), with revision chains and retractions in the mix.
    let mut events = Vec::with_capacity(5_000);
    let mut roots = Vec::new();
    for i in 0..4_000 {
        let e = g.remark(
            &format!("note number {i} about the picture"),
            vec![image.clone()],
        );
        roots.push(e.id.clone());
        events.push(e);
    }
    for (i, root) in roots.iter().take(600).enumerate() {
        events.push(g.revision(root.clone(), &format!("corrected note {i}")));
    }
    for root in roots.iter().skip(4_000 - 300) {
        events.push(g.retraction(root.clone()));
    }
    for i in 0..100 {
        events.push(g.rating((i % 6) as u8, vec![image.clone()]));
    }
    let report = t.store.merge(&events).unwrap();
    assert_eq!(report.inserted, events.len());

    // Warm once (statement caches), then best-of-5 — the same hot-path
    // methodology as `invariants_events::i16_fold_5k_events_timing` (B8).
    // This composition is revision-chain-heavier than I16's (600 revisions
    // vs 50), so the chain-fixpoint round dominates.
    let folded = t.store.folded_journal(&image).unwrap();
    let mut best = std::time::Duration::MAX;
    for _ in 0..5 {
        let t0 = Instant::now();
        let again = t.store.folded_journal(&image).unwrap();
        best = best.min(t0.elapsed());
        assert_eq!(again.len(), folded.len());
    }
    eprintln!(
        "D1 fold post-merge (revision-heavy): {} events -> {} folded entries, \
         best-of-5 {:.2} ms",
        events.len(),
        folded.len(),
        ms(best)
    );
    let budget_ms = if cfg!(debug_assertions) { 100.0 } else { 10.0 };
    assert!(
        ms(best) <= budget_ms,
        "5k post-merge fold took {:.2} ms (> {budget_ms} ms budget)",
        ms(best)
    );
}

// ---------------------------------------------------------------------------
// D2 — ingest throughput + first-1k-thumbnails proxy on the synthetic tree
// ---------------------------------------------------------------------------

fn ingest_throughput(n: usize) {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let t0 = Instant::now();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(n));
    let gen_s = t0.elapsed().as_secs_f64();

    let root_id = env.register("photos");
    let t1 = Instant::now();
    env.scan(&root_id);
    let scan_s = t1.elapsed().as_secs_f64();

    // First-1k-thumbnails proxy (LIBRARY L7): drain in shell-sized batches,
    // note the moment ≥ min(1000, n) preview passes are done.
    let target = n.min(1_000) as u64;
    let t2 = Instant::now();
    let mut first_1k_s: Option<f64> = None;
    loop {
        let r = env
            .lib
            .process_queue(&QueueOptions {
                cancel: None,
                additional_cancel: None,
                max_items: Some(64),
                max_concurrency: None,
                excluded_embedding_root_ids: Vec::new(),
            })
            .unwrap();
        if first_1k_s.is_none() {
            let done = env
                .lib
                .pass_counters()
                .unwrap()
                .get(&("preview".to_string(), 1))
                .map(|c| c.done)
                .unwrap_or(0);
            if done >= target {
                first_1k_s = Some(t2.elapsed().as_secs_f64());
            }
        }
        if r.processed == 0 {
            break;
        }
    }
    let drain_s = t2.elapsed().as_secs_f64();
    assert_eq!(env.lib.image_count().unwrap(), tree.unique_hashes as i64);
    let thumb = env
        .lib
        .preview_artifact(&tree.files[0].hash, ArtifactKind::Thumb)
        .unwrap()
        .expect("thumb exists");
    assert!(thumb.file.exists());
    eprintln!(
        "D2 ingest throughput ({n} files): generate {gen_s:.2} s | scan {scan_s:.2} s \
         ({:.0} files/s) | drain {drain_s:.2} s ({:.0} files/s hash+exif+preview) | \
         first-{target}-thumbs {:.2} s",
        n as f64 / scan_s,
        n as f64 / drain_s,
        first_1k_s.unwrap_or(drain_s),
    );
}

#[test]
fn d2_ingest_throughput_synthetic_tree() {
    let _g = guard();
    // CI-suite size; the 5k spec number comes from the ignored variant.
    ingest_throughput(scaled("PHOTOPROOF_TEST_INGEST_N", 1_000));
}

#[test]
#[ignore = "5k full ingest-throughput measurement: run in release on perf-pass hardware"]
fn d2_ingest_throughput_5k_full() {
    let _g = guard();
    ingest_throughput(5_000);
}

// ---------------------------------------------------------------------------
// D3 — search p95 with REAL previews/paths joined (not the P3.1 synthetic
// row corpus): a fully ingested synthetic library + journal load.
// ---------------------------------------------------------------------------

const NOTE_WORDS: &[&str] = &[
    "driftwood",
    "harbor",
    "overcast",
    "granite",
    "spray",
    "gull",
    "keeper",
    "blurry",
    "underexposed",
    "portrait",
    "window",
    "light",
    "contact",
    "sheet",
    "reflection",
    "rope",
    "hull",
    "tide",
    "fog",
    "lighthouse",
];

fn search_p95(n_files: usize, n_notes: usize) {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(n_files));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();
    assert_eq!(env.lib.image_count().unwrap(), tree.unique_hashes as i64);

    // Journal load: notes (2–5 words from a small vocabulary), some ratings
    // and strokes, across a third of the library.
    let mut rng = Rng::new(0xD3);
    for i in 0..n_notes {
        let img = &tree.files[(rng.below(tree.files.len() as u64)) as usize].hash;
        let wc = 2 + rng.below(4) as usize;
        let text: Vec<&str> = (0..wc)
            .map(|_| NOTE_WORDS[(rng.below(NOTE_WORDS.len() as u64)) as usize])
            .collect();
        env.store
            .append(
                &env.session,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: format!("{} on frame {i}", text.join(" ")),
                    targets: vec![img.clone()],
                },
                None,
            )
            .unwrap();
        if i % 7 == 0 {
            env.store
                .append(
                    &env.session,
                    EventDraft::Rating {
                        value: (rng.below(6)) as u8,
                        targets: vec![img.clone()],
                    },
                    None,
                )
                .unwrap();
        }
    }

    let opts = SearchOptions {
        now: Some(UtcMillis::now()),
        include_debug: false,
        ..SearchOptions::default()
    };
    // Query mix: single terms, prefixes (search-as-you-type), phrases,
    // filter-combined — measured individually for the p50/p95.
    let mut latencies: Vec<f64> = Vec::new();
    let mut total_hits = 0usize;
    for round in 0..30 {
        let w1 = NOTE_WORDS[round % NOTE_WORDS.len()];
        let w2 = NOTE_WORDS[(round + 7) % NOTE_WORDS.len()];
        let queries: Vec<(String, Vec<Filter>)> = vec![
            (w1.to_string(), vec![]),
            (w1[..w1.len().min(4)].to_string(), vec![]), // prefix typing
            (format!("{w1} {w2}"), vec![]),
            (w1.to_string(), vec![Filter::Rating(Comparison::Gte(3))]),
        ];
        for (q, filters) in queries {
            let t0 = Instant::now();
            let r = env.searcher.search_with(&q, &filters, &opts).unwrap();
            latencies.push(ms(t0.elapsed()));
            total_hits += r.images.len();
        }
    }
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (p50, p95, max) = (
        percentile(&latencies, 0.50),
        percentile(&latencies, 0.95),
        latencies.last().copied().unwrap(),
    );
    eprintln!(
        "D3 search latency ({n_files} files ingested w/ previews, {n_notes} notes, \
         {} queries, {total_hits} hits): p50 {p50:.2} ms | p95 {p95:.2} ms | max {max:.2} ms",
        latencies.len()
    );
    // RETRIEVAL §13.1: p95 < 100 ms (release); debug gets a sanity bound.
    let budget = if cfg!(debug_assertions) { 500.0 } else { 100.0 };
    assert!(p95 < budget, "search p95 {p95:.2} ms (> {budget} ms)");
}

#[test]
fn d3_search_p95_on_ingested_synthetic_library() {
    let _g = guard();
    search_p95(
        scaled("PHOTOPROOF_TEST_SEARCH_FILES", 2_000),
        scaled("PHOTOPROOF_TEST_SEARCH_NOTES", 600),
    );
}

#[test]
#[ignore = "50k-file reference-library variant: run in release on perf-pass hardware"]
fn d3_search_p95_50k_full() {
    let _g = guard();
    search_p95(50_000, 5_000);
}

// ---------------------------------------------------------------------------
// D4 — rebuild-from-sidecars timing at 50k events
// ---------------------------------------------------------------------------

fn rebuild_timing(n_images: usize, events_per_image: usize) {
    let env = M1Env::new();
    let root_dir = env.mount.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    let tree = synthlib::generate(&root_dir, &SynthSpec::n(n_images));
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();

    // Build the journal via merge (bulk load), then flush every sidecar.
    let g = common::Gen::new(env.session.clone());
    let mut events = Vec::with_capacity(n_images * events_per_image);
    for f in &tree.files {
        for i in 0..events_per_image {
            events.push(g.remark(
                &format!("archived note {i} for this frame"),
                vec![f.hash.clone()],
            ));
        }
    }
    let n_events = events.len();
    env.store.merge(&events).unwrap();
    let t0 = Instant::now();
    env.flush_sidecars();
    let flush_s = t0.elapsed().as_secs_f64();

    // The restore drill: wipe the index, rebuild from sidecar truth.
    let env = env.wipe_index_and_reopen();
    let t1 = Instant::now();
    let report = rebuild_from_sidecars(
        &env.engine,
        &RebuildOptions::live(vec![root_dir.clone()]),
        UtcMillis::now(),
    )
    .unwrap();
    env.store.rebuild_derived().unwrap();
    let rebuild_s = t1.elapsed().as_secs_f64();
    assert!(report.failures.is_empty());
    assert!(report.events_imported >= n_events);
    // Re-ingest for completeness of the drill (library index side; the
    // fresh database has no roots — registration is part of the drill).
    let t2 = Instant::now();
    let root_id = env.register("photos");
    env.scan(&root_id);
    env.drain();
    let reingest_s = t2.elapsed().as_secs_f64();
    eprintln!(
        "D4 rebuild-from-sidecars: {n_events} events across {n_images} sidecars | \
         flush {flush_s:.2} s | rebuild+derived {rebuild_s:.2} s | re-ingest {reingest_s:.2} s"
    );
}

#[test]
fn d4_rebuild_from_sidecars_timing() {
    let _g = guard();
    rebuild_timing(
        scaled("PHOTOPROOF_TEST_REBUILD_IMAGES", 400),
        scaled("PHOTOPROOF_TEST_REBUILD_EVENTS_PER", 25),
    );
}

#[test]
#[ignore = "50k-event rebuild measurement: run in release on perf-pass hardware"]
fn d4_rebuild_from_sidecars_50k_events_full() {
    let _g = guard();
    rebuild_timing(2_000, 25);
}
