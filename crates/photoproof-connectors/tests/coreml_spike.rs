//! PLAN-PERF P2 CoreML SPIKE — measurement harness (docs/SPIKE-COREML.md).
//!
//! These tests are `#[ignore]` by default: they need the local DFN5B CLIP
//! snapshot + the COCO source images, and they are MEASUREMENTS, not gates.
//! Run on this machine with:
//!
//!   # Does the linked onnxruntime even carry the CoreML EP?
//!   cargo test -p photoproof-connectors --test coreml_spike \
//!       coreml_spike_provider_available -- --ignored --nocapture
//!
//!   # CPU vs CoreML img/s + embedding cosine accuracy over N COCO images.
//!   cargo test -p photoproof-connectors --test coreml_spike \
//!       coreml_spike_clip_image_cpu_vs_coreml -- --ignored --nocapture
//!
//! The harness drives CPU vs CoreML by toggling the SAME runtime env knob the
//! shipped code reads (`PHOTOPROOF_ORT_COREML`), so it measures the exact path
//! a flag-flipped operator would get, not a private back door. WHY env-toggle
//! and not a session arg: `build_session` is private and intentionally has no
//! public CoreML parameter (the spike must not widen the shipped API); the env
//! knob is the whole surface.

use std::path::{Path, PathBuf};
use std::time::Instant;

use photoproof_connectors::{DecodedImage, Embedder, OrtEmbedder};

/// The pinned DFN5B CLIP id + its app-data layout (mirrors model_specs).
const CLIP_MODEL_ID: &str = "ViT-H-14-378-quickgelu__dfn5b";
const CLIP_EDGE: u32 = 378;
/// How many COCO images to push through the visual tower per EP. Enough to
/// average out per-image jitter; small enough that the slow CPU tower (~20
/// img/min in the wild) finishes in a couple minutes.
const SAMPLE_N: usize = 60;
/// Untimed warmups before each timed sweep: CoreML compiles/specializes its
/// graph on the FIRST run, so timing run #1 would charge the spike for a
/// one-time compile that a long ingest never re-pays.
const WARMUP: usize = 3;

fn models_dir() -> PathBuf {
    // The real app-data models dir on this machine (CLAUDE.md runtime path).
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home)
        .join("Library/Application Support/com.photoproof.desktop/models")
        .join(CLIP_MODEL_ID)
}

/// The FP16 single-file re-export staged alongside the int8 (the FP16 follow-up,
/// docs/SPIKE-COREML.md). Weights are INLINED into one `model.onnx` per tower
/// (no external-data fan-out), which is the form that clears the CoreML load
/// blocker. int8 stays the CPU default; this dir is the CoreML candidate.
fn models_dir_fp16() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home)
        .join("Library/Application Support/com.photoproof.desktop/models")
        .join(format!("{CLIP_MODEL_ID}-fp16"))
}

/// COCO source images. The eval library's `paths` table points here; the
/// images live in the MAIN checkout (not this worktree), so use an absolute
/// path. Override with COCO_IMAGES_DIR if they move.
fn coco_images_dir() -> PathBuf {
    if let Ok(p) = std::env::var("COCO_IMAGES_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from("/Users/bornman/projects/photoproof/test-corpora/retrieval/coco5k/images")
}

fn clip_paths() -> (PathBuf, PathBuf, PathBuf) {
    let dir = models_dir();
    (
        dir.join("visual/model.onnx"),
        dir.join("textual/model.onnx"),
        dir.join("textual/tokenizer.json"),
    )
}

/// Replicate core's CLIP geometry (resize-shortest-side + center-crop to
/// 378x378 sRGB u8). Kept here (not imported from core) because connectors
/// cannot depend on core; the connector's own `embed_image` does the /255 +
/// normalize + CHW. Functionally the same square the app feeds.
fn decode_clip_square(path: &Path) -> Option<DecodedImage> {
    let img = image::open(path).ok()?;
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    if w == 0 || h == 0 {
        return None;
    }
    // Resize shortest side to the edge, preserving aspect, then center-crop.
    let scale = CLIP_EDGE as f32 / w.min(h) as f32;
    let nw = (w as f32 * scale).round().max(CLIP_EDGE as f32) as u32;
    let nh = (h as f32 * scale).round().max(CLIP_EDGE as f32) as u32;
    let resized = image::imageops::resize(&rgb, nw, nh, image::imageops::FilterType::CatmullRom);
    let x0 = (nw - CLIP_EDGE) / 2;
    let y0 = (nh - CLIP_EDGE) / 2;
    let cropped = image::imageops::crop_imm(&resized, x0, y0, CLIP_EDGE, CLIP_EDGE).to_image();
    Some(DecodedImage {
        width: CLIP_EDGE,
        height: CLIP_EDGE,
        rgb8: cropped.into_raw(),
    })
}

/// Load up to `n` decoded COCO squares (sorted for determinism).
fn load_sample(n: usize) -> Vec<DecodedImage> {
    let dir = coco_images_dir();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jpg"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
        .into_iter()
        .filter_map(|p| decode_clip_square(&p))
        .take(n)
        .collect()
}

/// Build the CLIP embedder with CoreML on/off by toggling the shipped env knob.
fn build_clip(coreml: bool) -> OrtEmbedder {
    if coreml {
        // SAFETY: single-threaded test setup; no other thread reads env here.
        unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    } else {
        unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    }
    let (vis, txt, tok) = clip_paths();
    let e = OrtEmbedder::clip(CLIP_MODEL_ID, &vis, &txt, &tok, 1024).expect("load clip");
    // Leave the env clean for the next builder.
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    e
}

/// Like [`build_clip`] but returns the load error instead of panicking — the
/// CoreML path can fail to load the external-data visual tower.
fn build_clip_fallible(coreml: bool) -> Result<OrtEmbedder, photoproof_connectors::ConnectorError> {
    if coreml {
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    } else {
        unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    }
    let (vis, txt, tok) = clip_paths();
    let r = OrtEmbedder::clip(CLIP_MODEL_ID, &vis, &txt, &tok, 1024);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    r
}

fn embed_all(e: &OrtEmbedder, imgs: &[DecodedImage]) -> Vec<Vec<f32>> {
    imgs.iter()
        .map(|img| {
            pollster::block_on(e.embed_image(img))
                .expect("embed_image")
                .vector
        })
        .collect()
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    // Both are L2-normalized by the embedder, so cosine is the dot product.
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Q1: does the onnxruntime `ort` linked even CARRY the CoreML EP? This is the
/// key build-time finding: the prebuilt onnxruntime may omit it (CoreML is a
/// build-time onnxruntime option). Uses the public `ExecutionProvider`
/// trait's `is_available()`, which queries `GetAvailableProviders`.
#[test]
#[ignore = "spike measurement; run with --ignored --nocapture on macOS"]
fn coreml_spike_provider_available() {
    use ort::ep::CoreML;
    use ort::ep::ExecutionProvider;
    let available = CoreML::default()
        .is_available()
        .expect("query available providers");
    println!("[coreml-spike] CoreML EP present in linked onnxruntime: {available}");
    println!(
        "[coreml-spike] target_vendor=apple, supported_by_platform: {}",
        cfg!(target_vendor = "apple")
    );
    assert!(
        available,
        "the linked onnxruntime does NOT carry the CoreML EP — see docs/SPIKE-COREML.md (needs a CoreML-enabled onnxruntime build)"
    );
}

/// Probe: isolate the CoreML + external-data interaction. The DFN5B VISUAL
/// tower stores its weights as ~397 sibling external-data files referenced
/// relatively from `model.onnx`; the TEXTUAL tower is one self-contained
/// `model.onnx`. CoreML load failed on the visual tower with an external-data
/// path error; this probe confirms whether CoreML loads the SINGLE-FILE
/// textual tower (which would pin the failure to external data, not CoreML).
#[test]
#[ignore = "spike probe; run with --ignored --nocapture on macOS"]
fn coreml_spike_textual_singlefile_loads() {
    let (_, txt, tok) = clip_paths();
    if !txt.exists() {
        eprintln!("skipping: DFN5B textual snapshot absent");
        return;
    }
    // SAFETY: single-threaded test setup.
    unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    let (vis, _, _) = clip_paths();
    let r = OrtEmbedder::clip(CLIP_MODEL_ID, &vis, &txt, &tok, 1024);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    match &r {
        Ok(_) => {
            println!("[coreml-spike] CoreML loaded BOTH towers (incl. visual external-data) OK")
        }
        Err(e) => {
            println!("[coreml-spike] CoreML load error (expected on visual external-data): {e}")
        }
    }
}

/// Probe: does the CoreML external-data failure clear if onnxruntime resolves
/// the external data from the CURRENT DIRECTORY (the documented workaround for
/// external-data models is a bare filename + cwd at the model dir)? If CoreML
/// loads the visual tower this way, the failure is a path-resolution quirk we
/// can engineer around; if it still fails, external data is fundamentally
/// unsupported by this CoreML EP build and an inlined re-export is required.
#[test]
#[ignore = "spike probe; run with --ignored --nocapture on macOS"]
fn coreml_spike_visual_from_cwd() {
    let dir = models_dir().join("visual");
    if !dir.join("model.onnx").exists() {
        eprintln!("skipping: visual snapshot absent");
        return;
    }
    let (_, txt, tok) = clip_paths();
    // SAFETY: single-threaded test setup.
    unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    let prev = std::env::current_dir().ok();
    std::env::set_current_dir(&dir).expect("cd visual");
    let r = OrtEmbedder::clip(CLIP_MODEL_ID, Path::new("model.onnx"), &txt, &tok, 1024);
    if let Some(p) = prev {
        let _ = std::env::set_current_dir(p);
    }
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    match &r {
        Ok(_) => println!(
            "[coreml-spike] CoreML visual tower loaded from cwd (external-data workaround WORKS)"
        ),
        Err(e) => println!("[coreml-spike] CoreML visual tower STILL fails from cwd: {e}"),
    }
}

/// Probe: prove the CoreML EP is FUNCTIONAL on a SINGLE-FILE model (rules out
/// "CoreML is just broken"). The textual tower is one self-contained 1.4GB
/// `model.onnx` (no external data). We load it as BOTH towers: a successful
/// load proves CoreML compiles + registers; we do not run inference (the image
/// graph's input name would differ), only confirm the session builds.
#[test]
#[ignore = "spike probe; run with --ignored --nocapture on macOS"]
fn coreml_spike_singlefile_session_builds() {
    let (_, txt, tok) = clip_paths();
    if !txt.exists() {
        eprintln!("skipping: textual snapshot absent");
        return;
    }
    // SAFETY: single-threaded test setup.
    unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    // textual as both towers: single-file, so external data cannot be the
    // failure mode here. Tests the CoreML compile/register path in isolation.
    let r = OrtEmbedder::clip(CLIP_MODEL_ID, &txt, &txt, &tok, 1024);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    match &r {
        Ok(_) => println!(
            "[coreml-spike] CoreML built a session for the SINGLE-FILE tower OK (EP is functional)"
        ),
        Err(e) => println!("[coreml-spike] CoreML FAILED even on a single-file model: {e}"),
    }
}

/// Q2/Q3: CPU vs CoreML images/sec AND embedding cosine accuracy. Prints a
/// table; asserts only soft sanity (mean cosine high enough that CoreML is not
/// returning garbage). The ship/no-ship call is made from the printed numbers
/// in docs/SPIKE-COREML.md, not from a pass/fail here.
#[test]
#[ignore = "spike measurement; run with --ignored --nocapture on macOS"]
fn coreml_spike_clip_image_cpu_vs_coreml() {
    let (vis, _, _) = clip_paths();
    if !vis.exists() {
        eprintln!(
            "skipping: DFN5B visual snapshot absent at {}",
            vis.display()
        );
        return;
    }
    let imgs = load_sample(SAMPLE_N + WARMUP);
    if imgs.len() < WARMUP + 5 {
        eprintln!(
            "skipping: only {} COCO images decoded from {} (need >= {})",
            imgs.len(),
            coco_images_dir().display(),
            WARMUP + 5
        );
        return;
    }
    let (warm, timed) = imgs.split_at(WARMUP);
    println!(
        "[coreml-spike] timing {} images/EP ({} warmup), edge {}x{}",
        timed.len(),
        warm.len(),
        CLIP_EDGE,
        CLIP_EDGE
    );

    // --- CPU EP (the shipped default) ---
    let cpu = build_clip(false);
    let _ = embed_all(&cpu, warm);
    let t0 = Instant::now();
    let cpu_vecs = embed_all(&cpu, timed);
    let cpu_secs = t0.elapsed().as_secs_f64();
    let cpu_ips = timed.len() as f64 / cpu_secs;
    println!(
        "[coreml-spike] CPU    : {:.2} img/s  ({:.1} img/min, {:.3}s for {})",
        cpu_ips,
        cpu_ips * 60.0,
        cpu_secs,
        timed.len()
    );

    // --- CoreML EP (CPUAndNeuralEngine, MLProgram) ---
    // The DFN5B visual tower ships as external-data (397 sibling weight files);
    // the CoreML EP in this onnxruntime build mis-resolves those paths and the
    // session fails to load (see the `coreml_spike_visual_from_cwd` probe and
    // docs/SPIKE-COREML.md). Report that cleanly rather than panicking, so the
    // CPU baseline above is still captured.
    let ml = match build_clip_fallible(true) {
        Ok(e) => e,
        Err(e) => {
            println!(
                "[coreml-spike] CoreML could NOT load the visual tower (external-data path issue): {e}"
            );
            println!(
                "[coreml-spike] => no CoreML img/s on the bottleneck tower without an inlined-weights re-export; CPU stands at {:.2} img/s",
                cpu_ips
            );
            return;
        }
    };
    let _ = embed_all(&ml, warm);
    let t1 = Instant::now();
    let ml_vecs = embed_all(&ml, timed);
    let ml_secs = t1.elapsed().as_secs_f64();
    let ml_ips = timed.len() as f64 / ml_secs;
    println!(
        "[coreml-spike] CoreML : {:.2} img/s  ({:.1} img/min, {:.3}s for {})",
        ml_ips,
        ml_ips * 60.0,
        ml_secs,
        timed.len()
    );
    println!(
        "[coreml-spike] speedup (CoreML / CPU): {:.2}x",
        ml_ips / cpu_ips
    );

    // --- Accuracy: cosine(CPU vec, CoreML vec) per image ---
    let cosines: Vec<f32> = cpu_vecs
        .iter()
        .zip(&ml_vecs)
        .map(|(a, b)| cos(a, b))
        .collect();
    let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
    let min = cosines.iter().cloned().fold(f32::INFINITY, f32::min);
    let below_999 = cosines.iter().filter(|&&c| c < 0.999).count();
    println!(
        "[coreml-spike] embedding cosine CPU-vs-CoreML: mean {:.6}, min {:.6}, {} of {} below 0.999",
        mean,
        min,
        below_999,
        cosines.len()
    );

    // Soft sanity only: a wildly different embedding space (mean cosine far
    // from 1) means CoreML changed the math enough to corrupt retrieval — a
    // hard no-go that should fail loudly even in a spike.
    assert!(
        mean > 0.9,
        "CoreML embeddings diverged from CPU (mean cosine {mean:.4}); see docs/SPIKE-COREML.md"
    );
}

// --------------------------------------------------------------------------
// FP16 follow-up (docs/SPIKE-COREML.md "FP16 follow-up"). The int8 spike found
// CoreML cannot load the external-data visual tower and falls back to CPU on
// int8 ops. This measures the hypothesized fix: a single-file FP16 re-export of
// the SAME DFN5B graph (same I/O names/shapes the connector feeds). It loads the
// FP16 visual tower CPU-vs-CoreML and reports img/s + the CoreML-vs-CPU cosine.
// --------------------------------------------------------------------------

/// FP16 tower paths (the `-fp16` staged dir). Same filenames as int8.
fn clip_paths_fp16() -> (PathBuf, PathBuf, PathBuf) {
    let dir = models_dir_fp16();
    // The FP16 dir's tokenizer is copied from the int8 textual dir; either works.
    (
        dir.join("visual/model.onnx"),
        dir.join("textual/model.onnx"),
        dir.join("textual/tokenizer.json"),
    )
}

/// Build the FP16 CLIP embedder with CoreML on/off via the shipped env knob.
fn build_clip_fp16_fallible(
    coreml: bool,
) -> Result<OrtEmbedder, photoproof_connectors::ConnectorError> {
    if coreml {
        // SAFETY: single-threaded test setup; no other thread reads env here.
        unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    } else {
        unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    }
    let (vis, txt, tok) = clip_paths_fp16();
    let r = OrtEmbedder::clip(CLIP_MODEL_ID, &vis, &txt, &tok, 1024);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    r
}

/// THE PAYOFF: does the single-file FP16 visual tower LOAD on CoreML (the int8
/// external-data one did not), and is CoreML faster/accurate? Loads the FP16
/// visual tower CPU vs CoreML, times image embedding over N COCO images, and
/// reports CoreML-vs-CPU cosine. Prints the table; the ship/no-ship verdict is
/// written from these numbers in docs/SPIKE-COREML.md, not asserted here. Only a
/// soft accuracy gate is enforced (CoreML must not corrupt the embedding space).
#[test]
#[ignore = "spike measurement; needs the staged -fp16 dir + COCO images; run with --ignored --nocapture on macOS"]
fn coreml_spike_fp16_clip_image_cpu_vs_coreml() {
    let (vis, _, _) = clip_paths_fp16();
    if !vis.exists() {
        eprintln!(
            "skipping: FP16 visual tower absent at {} (run the FP16 conversion first, see docs/SPIKE-COREML.md)",
            vis.display()
        );
        return;
    }
    let imgs = load_sample(SAMPLE_N + WARMUP);
    if imgs.len() < WARMUP + 5 {
        eprintln!(
            "skipping: only {} COCO images decoded from {} (need >= {})",
            imgs.len(),
            coco_images_dir().display(),
            WARMUP + 5
        );
        return;
    }
    let (warm, timed) = imgs.split_at(WARMUP);
    println!(
        "[coreml-spike fp16] timing {} images/EP ({} warmup), edge {}x{}",
        timed.len(),
        warm.len(),
        CLIP_EDGE,
        CLIP_EDGE
    );

    // --- CPU EP on the FP16 tower ---
    let cpu = build_clip_fp16_fallible(false).expect("load FP16 visual (CPU)");
    let _ = embed_all(&cpu, warm);
    let t0 = Instant::now();
    let cpu_vecs = embed_all(&cpu, timed);
    let cpu_secs = t0.elapsed().as_secs_f64();
    let cpu_ips = timed.len() as f64 / cpu_secs;
    println!(
        "[coreml-spike fp16] CPU    : {:.2} img/s  ({:.1} img/min, {:.3}s for {})",
        cpu_ips,
        cpu_ips * 60.0,
        cpu_secs,
        timed.len()
    );

    // --- CoreML EP on the FP16 tower (the int8 one could not even load) ---
    let t_load = Instant::now();
    let ml = match build_clip_fp16_fallible(true) {
        Ok(e) => {
            println!(
                "[coreml-spike fp16] CoreML LOADED the FP16 visual tower in {:.1}s (int8 external-data tower could NOT)",
                t_load.elapsed().as_secs_f64()
            );
            e
        }
        Err(e) => {
            println!("[coreml-spike fp16] CoreML still FAILS to load the FP16 tower: {e}");
            println!(
                "[coreml-spike fp16] => FP16 did not unblock CoreML; CPU stands at {cpu_ips:.2} img/s"
            );
            return;
        }
    };
    let _ = embed_all(&ml, warm);
    let t1 = Instant::now();
    let ml_vecs = embed_all(&ml, timed);
    let ml_secs = t1.elapsed().as_secs_f64();
    let ml_ips = timed.len() as f64 / ml_secs;
    println!(
        "[coreml-spike fp16] CoreML : {:.2} img/s  ({:.1} img/min, {:.3}s for {})",
        ml_ips,
        ml_ips * 60.0,
        ml_secs,
        timed.len()
    );
    println!(
        "[coreml-spike fp16] speedup (CoreML / CPU): {:.2}x",
        ml_ips / cpu_ips
    );

    // --- Accuracy: cosine(CPU vec, CoreML vec) per image ---
    let cosines: Vec<f32> = cpu_vecs
        .iter()
        .zip(&ml_vecs)
        .map(|(a, b)| cos(a, b))
        .collect();
    let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
    let min = cosines.iter().cloned().fold(f32::INFINITY, f32::min);
    let below_999 = cosines.iter().filter(|&&c| c < 0.999).count();
    println!(
        "[coreml-spike fp16] embedding cosine CPU-vs-CoreML: mean {:.6}, min {:.6}, {} of {} below 0.999",
        mean,
        min,
        below_999,
        cosines.len()
    );

    assert!(
        mean > 0.9,
        "CoreML FP16 embeddings diverged from CPU (mean cosine {mean:.4}); see docs/SPIKE-COREML.md"
    );
}
