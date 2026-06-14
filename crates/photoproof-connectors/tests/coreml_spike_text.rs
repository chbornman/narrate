//! CoreML FP16 SPIKE for the TEXT embedder (docs/SPIKE-COREML-TEXT.md).
//!
//! Mirrors the CLIP CoreML spike (`coreml_spike.rs`) but for the EmbeddingGemma
//! TEXT tower: does the FP16/CoreML path embed SHORT, VARIABLE-LENGTH text
//! FASTER than the shipped int8/CPU path on Apple Silicon? Text-embed is small,
//! short-input, and variable-length - awkward for CoreML, which prefers static
//! shapes - so this is NOT assumed to win; it is MEASURED.
//!
//! All tests are `#[ignore]`: they need the local int8 snapshot (shipped) and a
//! single-file FP16 re-export staged at `models/embeddinggemma-300m-fp16/`
//! (the conversion is in docs/SPIKE-COREML-TEXT.md; it is NOT committed). They
//! SKIP CLEANLY (return early, no failure) when a model is absent, so the suite
//! stays green on machines without the snapshots. They are MEASUREMENTS, not
//! gates: the ship/no-ship verdict is written from the printed numbers in
//! docs/SPIKE-COREML-TEXT.md, not asserted here.
//!
//! Run on this machine with:
//!
//!   # Does the linked onnxruntime even carry the CoreML EP?
//!   cargo test -p photoproof-connectors --test coreml_spike_text \
//!       text_coreml_provider_available -- --ignored --nocapture
//!
//!   # End-to-end CPU(int8) vs CoreML(fp16) embeds/sec + cosine, via the SHIPPED
//!   # OrtEmbedder::text path (CoreML toggled by the same PHOTOPROOF_ORT_COREML
//!   # env knob the production code reads):
//!   cargo test --release -p photoproof-connectors --test coreml_spike_text \
//!       text_cpu_int8_vs_coreml_fp16 -- --ignored --nocapture
//!
//!   # The dynamic-vs-static shape question on raw ort sessions (does CoreML
//!   # recompile per unique length, or run dynamic, or fall back to CPU?):
//!   cargo test --release -p photoproof-connectors --test coreml_spike_text \
//!       text_coreml_dynamic_vs_static_shapes -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use photoproof_connectors::{Embedder, OrtEmbedder, TextRecipe};
use tokenizers::Tokenizer;

/// The shipped int8 EmbeddingGemma id + its app-data layout (mirrors model_specs).
const INT8_MODEL_ID: &str = "embeddinggemma-300m-q8";
/// The FP16 single-file re-export staged alongside the int8 (NOT committed; the
/// conversion recipe is in docs/SPIKE-COREML-TEXT.md). `onnx/model.onnx` is the
/// inlined-weights single file (no `.onnx_data`) that CoreML can load.
const FP16_MODEL_ID: &str = "embeddinggemma-300m-fp16";
const DIMS: usize = 768;
const INTRA_OP_THREADS: usize = 4;

/// EmbeddingGemma document-side prompt (matches the connector's
/// GEMMA_DOCUMENT_PROMPT). The prompt is part of the embedding space.
const DOC_PROMPT: &str = "title: none | text: ";

/// How many short texts to embed per EP. Big enough to average out per-embed
/// jitter; the corpus is realistic note-chunk + query lengths (5-60 tokens).
const SAMPLE_N: usize = 200;
/// Untimed warmups before each timed sweep: CoreML compiles/specializes on the
/// FIRST run(s); timing those would charge the spike a one-time compile a long
/// ingest never re-pays.
const WARMUP: usize = 5;
/// The fixed padded length for the static-shape CoreML experiment. EmbeddingGemma
/// note chunks are short; 64 covers the corpus with little waste. (The model's
/// max is far higher; 64 is the static-shape test length, not a model limit.)
const STATIC_PAD_LEN: usize = 64;

fn models_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home).join("Library/Application Support/com.photoproof.desktop/models")
}

fn int8_paths() -> (PathBuf, PathBuf) {
    let d = models_dir().join(INT8_MODEL_ID);
    (
        d.join("onnx/model_quantized.onnx"),
        d.join("tokenizer.json"),
    )
}

fn fp16_paths() -> (PathBuf, PathBuf) {
    let d = models_dir().join(FP16_MODEL_ID);
    (d.join("onnx/model.onnx"), d.join("tokenizer.json"))
}

/// A realistic short-text corpus: photography journal note chunks + search
/// queries, the kind the EmbeddingGemma seam actually embeds. Lengths span the
/// 5-60 token range (variable, which is the whole point of the dynamic-shape
/// question). Repeated to reach SAMPLE_N without inventing filler.
fn corpus(n: usize) -> Vec<String> {
    const SEEDS: [&str; 25] = [
        "harbor at dusk",
        "the focus missed her eyes, too soft",
        "print this big, the brick texture carries it",
        "flat light, nothing happening",
        "the cab's yellow pops against the gray street",
        "eyes are soft, focus error",
        "enlarge it - the wall texture is the strength",
        "dull lighting, lifeless frame",
        "yellow taxi against a gray scene, the color works",
        "the ferry wake makes a leading line",
        "the dog mid-leap, peak action, sharp",
        "banding in the sky from a cheap filter",
        "golden hour rim light on the climber's shoulder",
        "underexposed by two stops, recover in raw",
        "the strongest frame from the harbor series at first light",
        "reject - motion blur on the hands ruins it",
        "keep this one, the catchlight makes the portrait",
        "wide environmental shot, subject small in the frame intentionally",
        "the reflection in the puddle doubles the neon sign",
        "grain is heavy but it suits the night street mood",
        "crop tighter, the empty left third adds nothing",
        "backlit translucent leaf, the veins glow",
        "a candid laugh, slightly soft but the moment carries it",
        "long exposure smooths the waterfall into silk",
        "the color cast from the tungsten lamp needs a white balance fix",
    ];
    (0..n).map(|i| SEEDS[i % SEEDS.len()].to_string()).collect()
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    // Both are L2-normalized by the embedder, so cosine is the dot product.
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn mean_min(xs: &[f32]) -> (f32, f32) {
    let mean = xs.iter().sum::<f32>() / xs.len() as f32;
    let min = xs.iter().cloned().fold(f32::INFINITY, f32::min);
    (mean, min)
}

// ---------------------------------------------------------------------------
// Q1: does the linked onnxruntime even carry the CoreML EP? (same as CLIP)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spike measurement; run with --ignored --nocapture on macOS"]
fn text_coreml_provider_available() {
    use ort::ep::CoreML;
    use ort::ep::ExecutionProvider;
    let available = CoreML::default()
        .is_available()
        .expect("query available providers");
    println!("[coreml-text] CoreML EP present in linked onnxruntime: {available}");
    println!(
        "[coreml-text] target_vendor=apple, supported_by_platform: {}",
        cfg!(target_vendor = "apple")
    );
    assert!(
        available,
        "the linked onnxruntime does NOT carry the CoreML EP - see docs/SPIKE-COREML-TEXT.md"
    );
}

// ---------------------------------------------------------------------------
// Q2/Q3: end-to-end CPU(int8) vs CoreML(fp16) embeds/sec + cosine, via the
// SHIPPED OrtEmbedder::text path. CoreML is toggled by the same env knob the
// production build_session reads, so this measures the exact path a flag-flipped
// operator would get (not a private back door). The FP16 single-file model is
// what CoreML loads; the int8 external-data model is the CPU default.
// ---------------------------------------------------------------------------

/// Build the int8 text embedder on the CPU EP (the shipped default).
fn build_int8_cpu() -> Option<OrtEmbedder> {
    let (model, tok) = int8_paths();
    if !model.exists() || !tok.exists() {
        eprintln!(
            "skipping: int8 EmbeddingGemma snapshot absent at {}",
            model.display()
        );
        return None;
    }
    // SAFETY: single-threaded test setup; no other thread reads env here.
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    Some(
        OrtEmbedder::text(INT8_MODEL_ID, TextRecipe::MeanPooled, &model, &tok, DIMS)
            .expect("load int8 EmbeddingGemma (CPU)"),
    )
}

/// Build the FP16 text embedder on the CoreML EP (env knob forces CoreML in
/// build_session even for the text path, which structurally stays CPU). Returns
/// the load error instead of panicking: CoreML may reject the model or fall back.
fn build_fp16_coreml() -> Option<Result<OrtEmbedder, photoproof_connectors::ConnectorError>> {
    let (model, tok) = fp16_paths();
    if !model.exists() || !tok.exists() {
        eprintln!(
            "skipping: FP16 EmbeddingGemma single-file absent at {} (run the conversion in docs/SPIKE-COREML-TEXT.md)",
            model.display()
        );
        return None;
    }
    // SAFETY: single-threaded test setup.
    unsafe { std::env::set_var("PHOTOPROOF_ORT_COREML", "1") };
    let r = OrtEmbedder::text(FP16_MODEL_ID, TextRecipe::MeanPooled, &model, &tok, DIMS);
    unsafe { std::env::remove_var("PHOTOPROOF_ORT_COREML") };
    Some(r)
}

fn embed_all(e: &OrtEmbedder, texts: &[String]) -> Vec<Vec<f32>> {
    texts
        .iter()
        .map(|t| {
            pollster::block_on(e.embed_text(t))
                .expect("embed_text")
                .vector
        })
        .collect()
}

#[test]
#[ignore = "spike measurement; needs the staged -fp16 dir; run with --ignored --nocapture on macOS"]
fn text_cpu_int8_vs_coreml_fp16() {
    let Some(cpu) = build_int8_cpu() else { return };
    let texts = corpus(SAMPLE_N + WARMUP);
    let (warm, timed) = texts.split_at(WARMUP);
    println!(
        "[coreml-text] timing {} short texts/EP ({} warmup), doc-prompted, mean-pooled, dim {}",
        timed.len(),
        warm.len(),
        DIMS
    );

    // --- CPU EP (int8, the shipped default) ---
    let _ = embed_all(&cpu, warm);
    let t0 = Instant::now();
    let cpu_vecs = embed_all(&cpu, timed);
    let cpu_secs = t0.elapsed().as_secs_f64();
    let cpu_eps = timed.len() as f64 / cpu_secs;
    println!(
        "[coreml-text] CPU int8 : {:.1} embeds/s  ({:.3}s for {})",
        cpu_eps,
        cpu_secs,
        timed.len()
    );

    // --- CoreML EP (fp16 single file) ---
    let t_load = Instant::now();
    let ml = match build_fp16_coreml() {
        None => return, // fp16 model absent: CPU number stands, skip cleanly.
        Some(Ok(e)) => {
            println!(
                "[coreml-text] CoreML LOADED the FP16 text tower in {:.1}s",
                t_load.elapsed().as_secs_f64()
            );
            e
        }
        Some(Err(e)) => {
            println!("[coreml-text] CoreML FAILED to load the FP16 text tower: {e}");
            println!("[coreml-text] => CPU int8 stands at {cpu_eps:.1} embeds/s");
            return;
        }
    };
    let _ = embed_all(&ml, warm);
    let t1 = Instant::now();
    let ml_vecs = embed_all(&ml, timed);
    let ml_secs = t1.elapsed().as_secs_f64();
    let ml_eps = timed.len() as f64 / ml_secs;
    println!(
        "[coreml-text] CoreML fp16: {:.1} embeds/s  ({:.3}s for {})",
        ml_eps,
        ml_secs,
        timed.len()
    );
    println!(
        "[coreml-text] speedup (CoreML / CPU): {:.2}x  ({})",
        ml_eps / cpu_eps,
        if ml_eps > cpu_eps {
            "CoreML faster"
        } else {
            "CPU faster"
        }
    );

    // --- Accuracy: cosine(CPU-int8 vec, CoreML-fp16 vec) per text ---
    // CoreML partitions this graph heavily (only ~48 of ~1767 nodes run on the
    // ANE; the rest fall back to CPU across ~24 partitions - see
    // docs/SPIKE-COREML-TEXT.md). That partitioned fp16 path can emit a NaN
    // vector on the odd input, so we COUNT and EXCLUDE NaN rather than panic
    // (this is a measurement, not a gate) and gate softly on the finite rows.
    let mut cosines = Vec::new();
    let mut nan_rows = 0usize;
    for (a, b) in cpu_vecs.iter().zip(&ml_vecs) {
        if b.iter().any(|x| x.is_nan()) {
            nan_rows += 1;
            continue;
        }
        cosines.push(cos(a, b));
    }
    let (mean, min) = if cosines.is_empty() {
        (f32::NAN, f32::NAN)
    } else {
        mean_min(&cosines)
    };
    let below_99 = cosines.iter().filter(|&&c| c < 0.99).count();
    println!(
        "[coreml-text] cosine CPU-int8 vs CoreML-fp16 (finite rows): mean {:.6}, min {:.6}, {} of {} below 0.99; {} NaN row(s) excluded",
        mean,
        min,
        below_99,
        cosines.len(),
        nan_rows
    );
    // Soft sanity only on the FINITE rows: int8 and fp16 are different precisions
    // of the same model, so the floor is looser than the CLIP fp16-vs-fp16 case.
    // A mean far from 1 means the spaces diverged enough to matter for retrieval.
    if !cosines.is_empty() {
        assert!(
            mean > 0.95,
            "CoreML-fp16 vs CPU-int8 diverged (mean cosine {mean:.4}); see docs/SPIKE-COREML-TEXT.md"
        );
    }
}

// ---------------------------------------------------------------------------
// THE DYNAMIC-SHAPE QUESTION (the crux for text-embed). EmbeddingGemma has a
// VARIABLE sequence length. On RAW ort sessions (so we can flip the CoreML
// static-shapes flag the connector does not expose), compare:
//   (a) DYNAMIC / native shapes  - CoreML may run dynamic, recompile per unique
//       length, or partition the dynamic-shape nodes to CPU.
//   (b) STATIC shapes - pad EVERY input to STATIC_PAD_LEN and set
//       with_static_input_shapes(true) so CoreML compiles ONCE for one shape.
// Reports embeds/sec for each and the padding-waste cost. with_profile_compute_plan
// is on so the per-op CPU<->ANE partition is dumped to stderr (read it to see if
// CoreML actually took the graph or fell back to CPU).
// ---------------------------------------------------------------------------

/// Build a raw CoreML session for the FP16 model with a chosen static-shapes
/// setting and compute-plan profiling on. Cache dir co-located so the first
/// compile is paid once. Returns the load error rather than panicking.
fn build_raw_coreml_session(
    model: &std::path::Path,
    static_shapes: bool,
    cache_dir: &std::path::Path,
) -> Result<Session, String> {
    use ort::ep::CoreML;
    use ort::ep::coreml::{ComputeUnits, ModelFormat};
    let _ = std::fs::create_dir_all(cache_dir);
    let ep = CoreML::default()
        .with_compute_units(ComputeUnits::CPUAndNeuralEngine)
        .with_model_format(ModelFormat::MLProgram)
        .with_subgraphs(true)
        .with_static_input_shapes(static_shapes)
        .with_profile_compute_plan(true)
        .with_model_cache_dir(cache_dir.to_string_lossy().to_string());
    Session::builder()
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .with_intra_threads(INTRA_OP_THREADS)
        .map_err(|e| e.to_string())?
        .with_execution_providers([ep.build().error_on_failure()])
        .map_err(|e| e.to_string())?
        .commit_from_file(model)
        .map_err(|e| e.to_string())
}

/// Tokenize doc-prompted text to int64 ids + mask (mirror the connector's
/// MeanPooled path: add_special_tokens=true wraps <bos>..<eos>). `pad_to`, when
/// Some, right-pads ids with 0 and mask with 0 to a fixed length (the static
/// shape); attention_mask 0 means the pad positions are ignored by the model.
fn tokenize(tok: &Tokenizer, text: &str, pad_to: Option<usize>) -> (Vec<i64>, Vec<i64>) {
    let prompted = format!("{DOC_PROMPT}{text}");
    let enc = tok.encode(prompted, true).expect("tokenize");
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i64::from(i)).collect();
    if let Some(n) = pad_to {
        if ids.len() > n {
            ids.truncate(n); // corpus is short; truncation is a non-event here
        }
        let mut mask = vec![1i64; ids.len()];
        ids.resize(n, 0);
        mask.resize(n, 0);
        (ids, mask)
    } else {
        let mask = vec![1i64; ids.len()];
        (ids, mask)
    }
}

/// Run one raw session over the texts, returning embeds/sec and the L2-normalized
/// mean-pooled vectors (so callers can cosine them against the connector's).
/// `pad_to` selects dynamic (None) vs static (Some(len)) feeds.
fn run_raw(
    session: &mut Session,
    tok: &Tokenizer,
    texts: &[String],
    pad_to: Option<usize>,
) -> (f64, Vec<Vec<f32>>) {
    let t0 = Instant::now();
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        let (ids, mask) = tokenize(tok, text, pad_to);
        let seq = ids.len();
        let id_t = Tensor::from_array(([1usize, seq], ids)).expect("ids tensor");
        let mask_t = Tensor::from_array(([1usize, seq], mask.clone())).expect("mask tensor");
        let outputs = session
            .run(ort::inputs! { "input_ids" => id_t, "attention_mask" => mask_t })
            .expect("session run");
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().expect("extract f32");
        let hid = *shape.last().unwrap() as usize;
        // Mean-pool over ONLY the real (mask=1) positions so static padding does
        // not change the vector (the connector mean-pools all positions, but its
        // inputs are unpadded; with mask-aware pooling the static and dynamic
        // vectors match, isolating the SHAPE effect from a pooling difference).
        let mut v = vec![0f32; hid];
        let mut real = 0usize;
        for (t, m) in mask.iter().enumerate() {
            if *m == 0 {
                continue;
            }
            real += 1;
            for (o, x) in v.iter_mut().zip(&data[t * hid..(t + 1) * hid]) {
                *o += *x;
            }
        }
        if real > 0 {
            let inv = 1.0 / real as f32;
            for o in &mut v {
                *o *= inv;
            }
        }
        let norm = v
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum::<f64>()
            .sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x = (*x as f64 / norm) as f32;
            }
        }
        out.push(v);
    }
    (texts.len() as f64 / t0.elapsed().as_secs_f64(), out)
}

#[test]
#[ignore = "spike measurement; needs the staged -fp16 dir; run with --ignored --nocapture on macOS"]
fn text_coreml_dynamic_vs_static_shapes() {
    let (model, tok_path) = fp16_paths();
    if !model.exists() || !tok_path.exists() {
        eprintln!(
            "skipping: FP16 EmbeddingGemma single-file absent at {} (see docs/SPIKE-COREML-TEXT.md)",
            model.display()
        );
        return;
    }
    let tok = Tokenizer::from_file(&tok_path).expect("tokenizer");
    let texts = corpus(SAMPLE_N + WARMUP);
    let (warm, timed) = texts.split_at(WARMUP);
    let base = model.parent().unwrap();
    println!(
        "[coreml-text shapes] {} texts timed ({} warmup); static pad len = {}",
        timed.len(),
        warm.len(),
        STATIC_PAD_LEN
    );

    // (a) DYNAMIC / native shapes. CoreML's with_static_input_shapes(false)
    //     (the default): does it run dynamic, recompile per length, or fall back?
    println!("[coreml-text shapes] --- (a) DYNAMIC shapes (with_static_input_shapes=false) ---");
    let dyn_cache = base.join(".coreml-cache-dyn");
    match build_raw_coreml_session(&model, false, &dyn_cache) {
        Ok(mut s) => {
            let _ = run_raw(&mut s, &tok, warm, None); // pay first-compile in warmup
            let (eps, _) = run_raw(&mut s, &tok, timed, None);
            println!(
                "[coreml-text shapes] DYNAMIC CoreML : {eps:.1} embeds/s (native variable length)"
            );
        }
        Err(e) => println!("[coreml-text shapes] DYNAMIC CoreML load FAILED: {e}"),
    }

    // (b) STATIC shapes. Pad every input to STATIC_PAD_LEN and require static
    //     shapes so CoreML compiles ONCE for the single [1, PAD] shape.
    println!(
        "[coreml-text shapes] --- (b) STATIC shapes, padded to {} (with_static_input_shapes=true) ---",
        STATIC_PAD_LEN
    );
    let stat_cache = base.join(".coreml-cache-static");
    match build_raw_coreml_session(&model, true, &stat_cache) {
        Ok(mut s) => {
            let _ = run_raw(&mut s, &tok, warm, Some(STATIC_PAD_LEN));
            let (eps, _) = run_raw(&mut s, &tok, timed, Some(STATIC_PAD_LEN));
            println!(
                "[coreml-text shapes] STATIC CoreML  : {eps:.1} embeds/s (padded to {})",
                STATIC_PAD_LEN
            );
        }
        Err(e) => println!("[coreml-text shapes] STATIC CoreML load FAILED: {e}"),
    }

    // Padding-waste reference: average real tokens vs the padded length, so the
    // doc can quote the wasted compute fraction of the static path.
    let mut total_real = 0usize;
    for t in timed {
        let (_, mask) = tokenize(&tok, t, Some(STATIC_PAD_LEN));
        total_real += mask.iter().filter(|&&m| m == 1).count();
    }
    let avg_real = total_real as f64 / timed.len() as f64;
    println!(
        "[coreml-text shapes] padding waste: avg {:.1} real tokens of {} padded = {:.0}% wasted positions",
        avg_real,
        STATIC_PAD_LEN,
        100.0 * (1.0 - avg_real / STATIC_PAD_LEN as f64)
    );

    // CPU(fp16) raw baseline for the same dynamic feeds, so the shapes test is
    // self-contained (CoreML rows are only meaningful against a CPU row).
    println!("[coreml-text shapes] --- CPU baseline on the SAME fp16 model (dynamic) ---");
    let cpu = Session::builder()
        .unwrap()
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .unwrap()
        .with_intra_threads(INTRA_OP_THREADS)
        .unwrap()
        .commit_from_file(&model)
        .expect("cpu fp16 session");
    let mut cpu = cpu;
    let _ = run_raw(&mut cpu, &tok, warm, None);
    let (cpu_eps, _) = run_raw(&mut cpu, &tok, timed, None);
    println!("[coreml-text shapes] CPU fp16 (dynamic): {cpu_eps:.1} embeds/s");
}
