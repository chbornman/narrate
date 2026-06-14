//! NVIDIA (CUDA / TensorRT) measurement harness for the FP16 CLIP visual tower.
//! Built + run ONLY on the RTX 5080 machine (margo):
//!
//!   cargo test -p photoproof-connectors --features cuda --test cuda_spike \
//!       -- --ignored --nocapture
//!
//! Swap `--features cuda` for `--features tensorrt` once the TensorRT libs are
//! installed (then the EP ladder is TensorRT-FP16 -> CUDA -> CPU).
//!
//! Mirrors `coreml_spike`: loads the SAME single-file FP16 visual tower on CPU
//! (forced) and on the GPU EP ladder, times image embedding over N COCO images,
//! and reports GPU-vs-CPU img/s + the embedding cosine. Skips cleanly when the
//! fp16 model or the COCO sample is absent. Whole file gated behind `cuda`.
#![cfg(feature = "cuda")]

use std::path::{Path, PathBuf};
use std::time::Instant;

use photoproof_connectors::{DecodedImage, Embedder, OrtEmbedder};

/// The fp16 model id - the `-fp16` suffix makes `select_clip_accel` pick the
/// NVIDIA EP ladder (the production gating, exercised end to end here).
const FP16_ID: &str = "ViT-H-14-378-quickgelu__dfn5b-fp16";
const CLIP_EDGE: u32 = 378;
const SAMPLE_N: usize = 60;
const WARMUP: usize = 3;

/// FP16 model dir. Override with `PP_FP16_MODEL_DIR`; defaults to the margo scp
/// target `~/models/ViT-H-14-378-quickgelu__dfn5b-fp16`.
fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PP_FP16_MODEL_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").expect("HOME"))
        .join("models/ViT-H-14-378-quickgelu__dfn5b-fp16")
}

/// COCO sample dir. Override with `COCO_IMAGES_DIR`; defaults to `~/coco-sample`.
fn coco_images_dir() -> PathBuf {
    if let Ok(p) = std::env::var("COCO_IMAGES_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").expect("HOME")).join("coco-sample")
}

fn clip_paths() -> (PathBuf, PathBuf, PathBuf) {
    let d = model_dir();
    (
        d.join("visual/model.onnx"),
        d.join("textual/model.onnx"),
        d.join("textual/tokenizer.json"),
    )
}

/// Replicate core's CLIP geometry: resize-shortest-side + center-crop to 378.
fn decode_clip_square(path: &Path) -> Option<DecodedImage> {
    let img = image::open(path).ok()?;
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    if w == 0 || h == 0 {
        return None;
    }
    let scale = CLIP_EDGE as f32 / w.min(h) as f32;
    let nw = (w as f32 * scale).round().max(CLIP_EDGE as f32) as u32;
    let nh = (h as f32 * scale).round().max(CLIP_EDGE as f32) as u32;
    let resized = image::imageops::resize(&rgb, nw, nh, image::imageops::FilterType::CatmullRom);
    let (x0, y0) = ((nw - CLIP_EDGE) / 2, (nh - CLIP_EDGE) / 2);
    let cropped = image::imageops::crop_imm(&resized, x0, y0, CLIP_EDGE, CLIP_EDGE).to_image();
    Some(DecodedImage {
        width: CLIP_EDGE,
        height: CLIP_EDGE,
        rgb8: cropped.into_raw(),
    })
}

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
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Build the fp16 CLIP. `force_cpu` sets the force-CPU env knob (the baseline
/// arm); otherwise `select_clip_accel` picks the NVIDIA ladder for the `-fp16` id.
fn build(force_cpu: bool) -> Result<OrtEmbedder, photoproof_connectors::ConnectorError> {
    // SAFETY: single-threaded test setup; no other thread reads env here.
    if force_cpu {
        unsafe { std::env::set_var("PHOTOPROOF_ORT_FORCE_CPU", "1") };
    } else {
        unsafe { std::env::remove_var("PHOTOPROOF_ORT_FORCE_CPU") };
    }
    let (v, t, tok) = clip_paths();
    let r = OrtEmbedder::clip(FP16_ID, &v, &t, &tok, 1024);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_FORCE_CPU") };
    r
}

/// THE PAYOFF: img/s for the FP16 visual tower on CPU vs the NVIDIA GPU ladder,
/// plus the GPU-vs-CPU embedding cosine. Prints the table; the verdict is read
/// from the numbers (a soft accuracy gate only).
#[test]
#[ignore = "nvidia measurement; run on the RTX 5080 with --features cuda (or tensorrt) -- --ignored --nocapture"]
fn cuda_spike_clip_image_cpu_vs_gpu() {
    let (vis, _, _) = clip_paths();
    if !vis.exists() {
        eprintln!("skipping: FP16 visual tower absent at {}", vis.display());
        return;
    }
    let imgs = load_sample(SAMPLE_N + WARMUP);
    if imgs.len() < WARMUP + 5 {
        eprintln!(
            "skipping: only {} COCO images decoded from {}",
            imgs.len(),
            coco_images_dir().display()
        );
        return;
    }
    let (warm, timed) = imgs.split_at(WARMUP);
    println!(
        "[cuda-spike] timing {} images/EP ({} warmup), edge {CLIP_EDGE}",
        timed.len(),
        warm.len()
    );

    // --- CPU EP on the FP16 tower (forced) ---
    let cpu = build(true).expect("load FP16 visual (CPU)");
    let _ = embed_all(&cpu, warm);
    let t0 = Instant::now();
    let cpu_vecs = embed_all(&cpu, timed);
    let cpu_ips = timed.len() as f64 / t0.elapsed().as_secs_f64();
    println!("[cuda-spike] CPU(fp16): {cpu_ips:.2} img/s ({:.1} img/min)", cpu_ips * 60.0);

    // --- NVIDIA EP ladder (TensorRT/CUDA) on the FP16 tower ---
    let t_load = Instant::now();
    let gpu = match build(false) {
        Ok(e) => {
            println!(
                "[cuda-spike] NVIDIA ladder loaded in {:.1}s (TensorRT engine build / CUDA init)",
                t_load.elapsed().as_secs_f64()
            );
            e
        }
        Err(e) => {
            println!("[cuda-spike] NVIDIA load FAILED: {e}");
            println!("[cuda-spike] => CPU stands at {cpu_ips:.2} img/s");
            return;
        }
    };
    let _ = embed_all(&gpu, warm);
    let t1 = Instant::now();
    let gpu_vecs = embed_all(&gpu, timed);
    let gpu_ips = timed.len() as f64 / t1.elapsed().as_secs_f64();
    println!("[cuda-spike] GPU: {gpu_ips:.2} img/s ({:.1} img/min)", gpu_ips * 60.0);
    println!("[cuda-spike] speedup (GPU / CPU): {:.2}x", gpu_ips / cpu_ips);

    // --- Accuracy: cosine(CPU vec, GPU vec) per image ---
    let cosines: Vec<f32> = cpu_vecs.iter().zip(&gpu_vecs).map(|(a, b)| cos(a, b)).collect();
    let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
    let min = cosines.iter().cloned().fold(f32::INFINITY, f32::min);
    let below = cosines.iter().filter(|&&c| c < 0.999).count();
    println!(
        "[cuda-spike] cosine CPU-vs-GPU: mean {mean:.6}, min {min:.6}, {below} of {} below 0.999",
        cosines.len()
    );
    assert!(mean > 0.9, "GPU embeddings diverged from CPU (mean cosine {mean:.4})");
}
