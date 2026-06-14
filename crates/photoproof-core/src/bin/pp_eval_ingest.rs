//! pp-eval-ingest — build a throwaway eval library from a captioned image
//! benchmark, so search ranking can be tuned against REAL photos.
//!
//! WHY this exists (founder, June 2026): tuning hybrid search needs a
//! ground-truth set of "this caption should retrieve this image". The only
//! such ground truth we can get headlessly is a public image-caption
//! benchmark (COCO / Flickr30k). This tool ingests such a folder into a
//! FRESH, separate library (its own db + cache + vectors under one dir),
//! CLIP-embeds the images with the SAME pinned towers the desktop app ships
//! (so the offline tune matches the search that ships), and emits a
//! golden-query set in the existing `retrieval_eval::QuerySet` shape where
//! each caption is a text->image query whose single relevant answer is the
//! captioned image's content hash. The eval library is throwaway and lives
//! at an arbitrary path the founder picks: it never touches the user's real
//! app-data journal, and re-running over the same lib + folder does not
//! double-ingest (scan is idempotent by content hash).
//!
//! USAGE
//!   pp-eval-ingest --images <folder> --lib <eval-lib-dir>
//!                  [--models-dir <dir>] [--captions <captions.json>]
//!                  [--out-golden <golden.json>] [--out-map <map.json>]
//!
//! The eval library is laid out under <eval-lib-dir>/:
//!   <dir>/photoproof.db   the SQLite metadata + queue
//!   <dir>/cache           the preview artifact cache
//!   <dir>/vectors         the PPVEC flat-file vector store
//!
//! EMBEDDING is best-effort. With models present (real `--models-dir`, or the
//! app-data `models/` default) the image-embedding pass runs and fills the
//! CLIP space. WITHOUT models the tool still scans, builds previews, and emits
//! the filename->hash map and the golden set; it just SKIPS embedding with a
//! clear warning. That keeps the tool runnable (and testable) on any machine
//! with no models and no benchmark data.
//!
//! CAPTIONS FORMAT (normalized; the caller prepares it from the raw COCO/Flickr
//! annotations): a JSON object mapping each image's path RELATIVE to the
//! `--images` folder to its list of caption strings:
//!   { "000000391895.jpg": ["a man riding a bike", "cyclist on a road"], ... }
//! Each caption becomes one golden query whose single relevant id is that
//! image's content hash. Captions whose image did not ingest are skipped
//! (counted in a warning) so a partial benchmark still produces a valid set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use photoproof_connectors::OrtEmbedder;
use photoproof_core::library::{EmbeddingRig, Library, QueueOptions, ScanOptions};
use photoproof_core::retrieval::{PpvecStore, default_vectors_dir};
use photoproof_core::retrieval_eval::{GoldenQuery, QuerySet};

/// The pinned CLIP model id the desktop app and the search-eval rig use
/// (`model_specs::clip_spec`). The eval library is fresh, so there is no
/// stored-vector id to resolve from; we embed under the same pinned id the
/// app ships so the offline tune ranks against the search that ships.
///
/// Override with `PP_EVAL_CLIP_MODEL` to embed under an alternate pinned id -
/// e.g. `ViT-H-14-378-quickgelu__dfn5b-fp16` to A/B the FP16 CoreML space
/// against the int8 CPU baseline (docs/SPIKE-COREML.md eval). Must be a known
/// `clip_spec` id with weights staged under `models_dir/<id>/`.
const DEFAULT_CLIP_MODEL_ID: &str = "ViT-H-14-378-quickgelu__dfn5b";

fn clip_model_id() -> String {
    std::env::var("PP_EVAL_CLIP_MODEL").unwrap_or_else(|_| DEFAULT_CLIP_MODEL_ID.to_string())
}

/// The default @k the emitted golden set pins (the runner's built-in default
/// too). Single-answer text->image queries: 10 is the standard cutoff.
const DEFAULT_GOLDEN_K: usize = 10;

struct Args {
    images: PathBuf,
    lib: PathBuf,
    /// `None` => default to the app-data `models/` dir if it exists, else skip
    /// embedding (headless, no-models path).
    models_dir: Option<PathBuf>,
    captions: Option<PathBuf>,
    out_golden: Option<PathBuf>,
    out_map: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: pp-eval-ingest --images <folder> --lib <eval-lib-dir> \
     [--models-dir <dir>] [--captions <captions.json>] \
     [--out-golden <golden.json>] [--out-map <map.json>]"
}

fn parse_args() -> Result<Args, String> {
    let mut images: Option<PathBuf> = None;
    let mut lib: Option<PathBuf> = None;
    let mut models_dir: Option<PathBuf> = None;
    let mut captions: Option<PathBuf> = None;
    let mut out_golden: Option<PathBuf> = None;
    let mut out_map: Option<PathBuf> = None;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    // Pull the value following a flag, erroring (not panicking) if absent —
    // the eval bin idiom (pp-retrieval-eval), so a misuse exits 2 cleanly.
    while let Some(flag) = it.next() {
        let mut val = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("flag {name} needs a value"))
        };
        match flag.as_str() {
            "--images" => images = Some(PathBuf::from(val("--images")?)),
            "--lib" => lib = Some(PathBuf::from(val("--lib")?)),
            "--models-dir" => models_dir = Some(PathBuf::from(val("--models-dir")?)),
            "--captions" => captions = Some(PathBuf::from(val("--captions")?)),
            "--out-golden" => out_golden = Some(PathBuf::from(val("--out-golden")?)),
            "--out-map" => out_map = Some(PathBuf::from(val("--out-map")?)),
            "-h" | "--help" => return Err(usage().to_string()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }

    Ok(Args {
        images: images.ok_or_else(|| format!("missing --images\n{}", usage()))?,
        lib: lib.ok_or_else(|| format!("missing --lib\n{}", usage()))?,
        models_dir,
        captions,
        out_golden,
        out_map,
    })
}

/// The app-data `models/` directory on this platform, if the desktop app has
/// laid it down. The default `--models-dir` so a founder machine "just works"
/// without spelling the path; absent it, embedding is skipped.
fn default_models_dir() -> Option<PathBuf> {
    // Mirror the app-data root the desktop uses (CLAUDE.md): on macOS,
    // ~/Library/Application Support/com.photoproof.desktop/models.
    let home = std::env::var_os("HOME")?;
    let dir = Path::new(&home).join("Library/Application Support/com.photoproof.desktop/models");
    dir.is_dir().then_some(dir)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    if !args.images.is_dir() {
        return Err(format!(
            "--images is not a directory: {}",
            args.images.display()
        ));
    }
    // The eval library lives entirely under --lib (db + cache + vectors), so a
    // throwaway benchmark library is one removable directory and never shares a
    // byte with the user's real app-data journal.
    std::fs::create_dir_all(&args.lib)
        .map_err(|e| format!("creating lib dir {}: {e}", args.lib.display()))?;
    let db_path = args.lib.join("photoproof.db");
    let cache_dir = args.lib.join("cache");

    // Default-probe library over a real on-disk folder, exactly as the desktop
    // does for a real root (pp-bench uses a fake probe only because it ingests
    // a synthetic corpus read-only; here we register the folder as a normal
    // root so scan + the passes behave identically to the app).
    let lib = Library::open(&db_path, &cache_dir)
        .map_err(|e| format!("opening eval library at {}: {e}", db_path.display()))?;
    let root = lib
        .register_root(&args.images, Some("eval"))
        .map_err(|e| format!("registering root {}: {e}", args.images.display()))?;

    // Scan is idempotent by content hash: re-running over the same lib + folder
    // re-stats and no-ops unchanged files, so this tool is safely re-runnable.
    let scan = lib
        .scan_root(&root, &ScanOptions::default())
        .map_err(|e| format!("scanning {}: {e}", args.images.display()))?;
    eprintln!(
        "scan: {} files seen, {} new images",
        scan.files_seen, scan.new_images
    );

    // Drain the exif + preview passes (the image-embedding pass needs the
    // display/thumb artifact to exist). process_queue does NOT touch the
    // embedding queue; that is process_embedding_queue below.
    let q = lib
        .process_queue(&QueueOptions::default())
        .map_err(|e| format!("draining preview/exif queue: {e}"))?;
    eprintln!("previews/exif: {} done, {} errors", q.done, q.errors);

    // Resolve the models dir: explicit flag, else the app-data default if the
    // desktop laid it down, else None (skip embedding).
    let models_dir = args.models_dir.clone().or_else(default_models_dir);
    match &models_dir {
        Some(dir) => embed_images(&lib, &db_path, &args.lib, dir)?,
        None => eprintln!(
            "no models dir (none given, no app-data models/ found): SKIPPING image embedding. \
             The filename->hash map and golden set still build; re-run with --models-dir to embed."
        ),
    }

    // Build the filename->hash map from the active path rows (public API: every
    // ingested image's active rel_path -> its content hash), keyed RELATIVE to
    // the --images folder so the keys match the captions file.
    let map = build_path_map(&lib, &root)?;
    eprintln!("mapped {} ingested files to content hashes", map.len());
    if let Some(out_map) = &args.out_map {
        let json =
            serde_json::to_string_pretty(&map).map_err(|e| format!("serializing map: {e}"))?;
        std::fs::write(out_map, json).map_err(|e| format!("writing {}: {e}", out_map.display()))?;
        eprintln!("wrote map -> {}", out_map.display());
    }

    // Build + emit the golden set from the captions file, if given.
    if let Some(captions_path) = &args.captions {
        let raw = std::fs::read_to_string(captions_path)
            .map_err(|e| format!("reading captions {}: {e}", captions_path.display()))?;
        let captions: BTreeMap<String, Vec<String>> = serde_json::from_str(&raw)
            .map_err(|e| format!("parsing captions {}: {e}", captions_path.display()))?;
        let (golden, skipped) = build_golden_set(&captions, &map);
        if skipped > 0 {
            eprintln!(
                "warning: {skipped} caption(s) skipped (their image did not ingest / is not in the map)"
            );
        }
        eprintln!(
            "golden set: {} queries over {} captioned images",
            golden.queries.len(),
            captions.len() - skipped_images(&captions, &map)
        );
        if let Some(out_golden) = &args.out_golden {
            let json = serde_json::to_string_pretty(&golden)
                .map_err(|e| format!("serializing golden set: {e}"))?;
            std::fs::write(out_golden, json)
                .map_err(|e| format!("writing {}: {e}", out_golden.display()))?;
            eprintln!("wrote golden set -> {}", out_golden.display());
        } else {
            eprintln!("(no --out-golden given; golden set built but not written)");
        }
    }

    Ok(())
}

/// Build the CLIP embedder + ppvec store and drain the image-embedding pass.
/// Failures to LOAD the embedder are a warning, not a hard error: the map and
/// golden set must still build on a machine whose models are missing/corrupt.
fn embed_images(
    lib: &Library,
    db_path: &Path,
    lib_dir: &Path,
    models_dir: &Path,
) -> Result<(), String> {
    let model_id = clip_model_id();
    let embedder = match photoproof_connectors::build_clip_embedder(&model_id, models_dir) {
        Ok(e) => e,
        Err(e) => {
            // No usable CLIP towers: skip embedding rather than abort, so the
            // headless map/golden path still completes.
            eprintln!(
                "warning: could not load CLIP embedder {model_id} from {}: {e}. \
                 SKIPPING image embedding.",
                models_dir.display()
            );
            return Ok(());
        }
    };
    eprintln!("[pp-eval-ingest] embedding under CLIP model id: {model_id}");
    // The vector store lives beside the eval db (its sibling vectors/), kept
    // under --lib so the throwaway library is one directory.
    let vectors_dir = default_vectors_dir(db_path).unwrap_or_else(|| lib_dir.join("vectors"));
    let vectors = PpvecStore::open(db_path, &vectors_dir)
        .map_err(|e| format!("opening vector store {}: {e}", vectors_dir.display()))?;

    // The image-embedding pass: text=None (no journal to embed in a benchmark
    // library), clip=Some(embedder). process_embedding_queue enqueues the
    // backfill rows and drains them, calling the CLIP image tower on each
    // cached preview. EmbeddingRig is generic over the text/clip embedder
    // types; with `text` None the text type parameter is otherwise unconstrained
    // and cannot be inferred, so we pin it to the CLIP embedder's type (it is
    // never constructed or called when `text` is None).
    let rig: EmbeddingRig<'_, OrtEmbedder, OrtEmbedder> = EmbeddingRig {
        text: None,
        clip: Some(&embedder),
        vectors: &vectors,
    };
    let report = lib
        .process_embedding_queue(&rig, &QueueOptions::default())
        .map_err(|e| format!("draining image-embedding queue: {e}"))?;
    eprintln!(
        "image embedding: {} processed, {} done, {} errors",
        report.processed, report.done, report.errors
    );
    Ok(())
}

/// The filename->hash map: every ingested image's ACTIVE path mapped to its
/// content hash, keyed RELATIVE to the `--images` folder so the keys line up
/// with the captions file. Drawn from the public `image_hashes` +
/// `paths_for_image` API (the `paths` table, `state='active'`). The `paths`
/// rel_path is VOLUME-relative; the registered root's own rel_path is the
/// prefix from the volume mount to the `--images` folder, so we strip it to get
/// folder-relative keys. Sorted (BTreeMap) so the JSON is stable across runs.
/// Only rows under THIS root are mapped (the eval lib has one root, but the
/// filter keeps the key space exactly the `--images` tree).
fn build_path_map(lib: &Library, root_id: &str) -> Result<BTreeMap<String, String>, String> {
    let root = lib
        .root(root_id)
        .map_err(|e| format!("reading root {root_id}: {e}"))?
        .ok_or_else(|| format!("root {root_id} not found after ingest"))?;
    // The volume-relative prefix of the root folder, normalized to a trailing
    // slash so a clean `strip_prefix` yields the folder-relative tail.
    let prefix = if root.rel_path.is_empty() {
        String::new()
    } else {
        format!("{}/", root.rel_path.trim_end_matches('/'))
    };

    let mut map = BTreeMap::new();
    let hashes = lib
        .image_hashes()
        .map_err(|e| format!("listing image hashes: {e}"))?;
    for hash in hashes {
        let rows = lib
            .paths_for_image(&hash)
            .map_err(|e| format!("listing paths for {}: {e}", hash.as_str()))?;
        for row in rows {
            if !row.active || row.root_id.as_deref() != Some(root_id) {
                continue;
            }
            // Strip the root prefix to fold the volume-relative path down to the
            // folder-relative key the captions file uses.
            let rel = row
                .rel_path
                .strip_prefix(&prefix)
                .unwrap_or(&row.rel_path)
                .to_string();
            map.insert(rel, hash.as_str().to_string());
        }
    }
    Ok(map)
}

/// Count of captioned images that did not ingest (not in the map) — for the
/// progress line. Pure helper over the two maps.
fn skipped_images(
    captions: &BTreeMap<String, Vec<String>>,
    map: &BTreeMap<String, String>,
) -> usize {
    captions
        .keys()
        .filter(|rel| !map.contains_key(*rel))
        .count()
}

/// Build the golden-query set: each caption of an ingested image becomes ONE
/// query whose single relevant id is that image's content hash. Captions whose
/// image is not in the map are skipped; the count of skipped CAPTIONS is
/// returned so the caller can warn. Queries are emitted in (rel_path, caption)
/// order for a deterministic file. This is the standard text->image retrieval
/// eval shape (single-answer queries) the `retrieval_eval::QuerySet` runner
/// scores.
fn build_golden_set(
    captions: &BTreeMap<String, Vec<String>>,
    map: &BTreeMap<String, String>,
) -> (QuerySet, usize) {
    let mut queries = Vec::new();
    let mut skipped = 0usize;
    for (rel_path, caps) in captions {
        let Some(hash) = map.get(rel_path) else {
            // The image for these captions did not ingest; every caption of it
            // is unscoreable (no relevant id), so skip them all.
            skipped += caps.len();
            continue;
        };
        for caption in caps {
            queries.push(GoldenQuery {
                query: caption.clone(),
                relevant: vec![hash.clone()],
                // Trace which image (and file) a query came from, for the
                // curator reading the report. No em-dash, no colon-prefix.
                notes: Some(format!("benchmark caption for {rel_path}")),
            });
        }
    }
    (
        QuerySet {
            default_k: Some(DEFAULT_GOLDEN_K),
            queries,
        },
        skipped,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-color JPEG (the cheapest valid image the ingest passes accept),
    /// written to `dir/name`. Distinct colors give distinct bytes => distinct
    /// content hashes, so the map has one entry per file (no dedup collapse).
    fn write_jpeg(dir: &Path, name: &str, color: [u8; 3]) {
        use image::codecs::jpeg::JpegEncoder;
        let img = image::RgbImage::from_pixel(64, 64, image::Rgb(color));
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 90)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .expect("encode test jpeg");
        std::fs::write(dir.join(name), bytes).expect("write test jpeg");
    }

    /// End-to-end ingest WITHOUT models: a tiny synthetic folder scans, builds
    /// previews, and the filename->hash map has one stable entry per file.
    /// Embedding is skipped (no models dir passed), proving the headless path.
    #[test]
    fn ingest_maps_files_to_stable_hashes_without_models() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let images = tmp.path().join("images");
        std::fs::create_dir_all(&images).expect("images dir");
        write_jpeg(&images, "red.jpg", [220, 30, 30]);
        write_jpeg(&images, "green.jpg", [30, 220, 30]);
        write_jpeg(&images, "blue.jpg", [30, 30, 220]);

        let lib_dir = tmp.path().join("lib");
        std::fs::create_dir_all(&lib_dir).expect("lib dir");
        let lib = Library::open(lib_dir.join("photoproof.db"), lib_dir.join("cache"))
            .expect("open eval library");
        let root = lib
            .register_root(&images, Some("eval"))
            .expect("register root");
        let scan = lib.scan_root(&root, &ScanOptions::default()).expect("scan");
        assert_eq!(scan.new_images, 3, "three distinct images ingested");
        lib.process_queue(&QueueOptions::default())
            .expect("drain previews");

        let map = build_path_map(&lib, &root).expect("build map");
        assert_eq!(map.len(), 3, "one map entry per file");
        assert!(map.contains_key("red.jpg"));
        assert!(map.contains_key("green.jpg"));
        assert!(map.contains_key("blue.jpg"));
        for hash in map.values() {
            assert_eq!(hash.len(), 64, "content hash is 64-char hex blake3");
        }

        // Idempotent: a second scan over the same lib + folder adds nothing.
        let scan2 = lib
            .scan_root(&root, &ScanOptions::default())
            .expect("rescan");
        assert_eq!(scan2.new_images, 0, "re-scan does not double-ingest");
        let map2 = build_path_map(&lib, &root).expect("rebuild map");
        assert_eq!(map, map2, "map is stable across re-ingest");
    }

    /// The golden builder: one query per caption, the right relevant hash, and
    /// a caption whose file is missing from the map is skipped (counted).
    #[test]
    fn golden_set_one_query_per_caption_skips_missing() {
        let mut map = BTreeMap::new();
        map.insert("a.jpg".to_string(), "a".repeat(64));
        map.insert("b.jpg".to_string(), "b".repeat(64));
        // "c.jpg" deliberately absent from the map (did not ingest).

        let mut captions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        captions.insert(
            "a.jpg".to_string(),
            vec!["red square".into(), "a crimson tile".into()],
        );
        captions.insert("b.jpg".to_string(), vec!["green field".into()]);
        captions.insert(
            "c.jpg".to_string(),
            vec!["never ingested".into(), "lost".into()],
        );

        let (golden, skipped) = build_golden_set(&captions, &map);

        // a.jpg: 2 captions, b.jpg: 1 caption => 3 queries; c.jpg's 2 skipped.
        assert_eq!(golden.queries.len(), 3);
        assert_eq!(skipped, 2);
        assert_eq!(golden.default_k, Some(DEFAULT_GOLDEN_K));

        // Each query's single relevant id is its image's hash.
        let a_hash = "a".repeat(64);
        let a_queries: Vec<_> = golden
            .queries
            .iter()
            .filter(|q| q.relevant == vec![a_hash.clone()])
            .collect();
        assert_eq!(a_queries.len(), 2, "both a.jpg captions point at a's hash");
        assert!(
            golden.queries.iter().all(|q| q.relevant.len() == 1),
            "single-answer text->image queries"
        );
        // No query references the un-ingested image.
        assert!(
            golden
                .queries
                .iter()
                .all(|q| q.relevant != vec!["c".repeat(64)])
        );
    }

    /// Round-trip: the emitted golden JSON parses back through the SAME
    /// `retrieval_eval::QuerySet` loader the runner uses (serde_json), so the
    /// file this tool writes is consumable by pp-retrieval-eval unchanged.
    #[test]
    fn golden_set_round_trips_through_queryset_loader() {
        let mut map = BTreeMap::new();
        map.insert("x.jpg".to_string(), "x".repeat(64));
        let mut captions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        captions.insert("x.jpg".to_string(), vec!["one".into(), "two".into()]);

        let (golden, _) = build_golden_set(&captions, &map);
        let json = serde_json::to_string_pretty(&golden).expect("serialize");
        let parsed: QuerySet = serde_json::from_str(&json).expect("parse back");
        assert_eq!(parsed, golden, "golden set round-trips byte-for-byte");
        assert_eq!(parsed.queries.len(), 2);
    }

    /// skipped_images counts captioned files absent from the map (the progress
    /// line), distinct from the skipped-CAPTION count the builder returns.
    #[test]
    fn skipped_images_counts_uningested_files() {
        let mut map = BTreeMap::new();
        map.insert("here.jpg".to_string(), "h".repeat(64));
        let mut captions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        captions.insert("here.jpg".to_string(), vec!["seen".into()]);
        captions.insert("gone.jpg".to_string(), vec!["c1".into(), "c2".into()]);
        assert_eq!(skipped_images(&captions, &map), 1);
    }
}
