# SPIKE-COREML-TEXT -- CoreML/FP16 for the EmbeddingGemma TEXT embedder (June 14, 2026)

The TEXT-embedder mirror of the CLIP FP16 CoreML spike (`docs/SPIKE-COREML.md`).
Goal: MEASURE honestly whether the EmbeddingGemma-300m text tower runs FASTER on
CoreML/FP16 than on the shipped int8/CPU path on this M1 Pro Mac. Text-embed is
small, short-input, and VARIABLE sequence length -- awkward for CoreML -- so this
was NOT assumed to win. It does not.

Machine: Apple Silicon M1 Pro, 10 cores (8 perf + 2 eff), unified memory.
`ort = 2.0.0-rc.12`. Models: EmbeddingGemma-300m int8 (`model_quantized.onnx`,
the shipped default) and a single-file FP16 re-export (this spike), at the
app-data `models/` dir.

## VERDICT: DON'T-SHIP. int8/CPU is the right option for this seam. CoreML LOSES.

CoreML/FP16 is **slower than int8/CPU for the text embedder, under every shape
strategy tried**, AND its heavily-partitioned fp16 path is numerically unstable
(emits NaN on this graph). The int8/CPU path the connector already ships is the
best option here. Concretely, on 200 short texts (realistic 5-60 token note
chunks + queries), mean-pooled, dim 768:

| EP / strategy | embeds/sec | vs CPU int8 | accuracy | notes |
|---|---|---|---|---|
| **CPU int8 (shipped default)** | **20.7** | 1.00x (reference) | reference | the path every stored PPVEC vector was embedded under |
| CPU fp16 (same single file) | 21.8 | 1.05x | -- | fp16 on CPU is ~CPU int8; quantization buys little here |
| CoreML fp16, DYNAMIC shapes | 13.2 | **0.64x (slower)** | NaN-prone (see below) | the connector's actual CoreML path |
| CoreML fp16, STATIC shapes (pad 64) | 10.0 | **0.48x (slower)** | NaN-prone | padding made it WORSE, not better |

Three independent reasons CoreML loses on this seam, all measured:

1. **CoreML can only take ~3% of the graph.** CoreML's own GetCapability dump:
   `number of nodes supported by CoreML: 48 number of nodes in the graph: 1767
   number of partitions supported by CoreML: 24`. EmbeddingGemma's body is fused
   `MultiHeadAttention` / `SimplifiedLayerNormalization` (RMSNorm) /
   `RotaryEmbedding` / `Gather` / dynamic-shape ops that the CoreML EP does not
   implement, so it runs ~1719 of 1767 nodes on CPU anyway -- and splits the
   remainder into **24 partitions**, paying a CPU<->ANE hand-off at each
   boundary. The net is "mostly CPU, plus partition overhead", i.e. slower than
   plain CPU. (Contrast CLIP's visual tower, a conv/matmul stack CoreML takes
   whole -- there it won 8.8x. This text graph is the opposite case.)

2. **The dynamic-shape question -- neither answer helps.** EmbeddingGemma has a
   variable sequence length. We tested both paths on raw `ort` sessions:
   - DYNAMIC / native shapes (`with_static_input_shapes(false)`): 13.2 embeds/s.
     CoreML does NOT pathologically recompile per unique length here -- it just
     runs the 24-partition mostly-CPU plan, which is slower than CPU.
   - STATIC shapes (`with_static_input_shapes(true)`, every input padded to 64):
     10.0 embeds/s -- SLOWER STILL. The corpus averages **16.6 real tokens**, so
     padding to 64 wastes **74% of the positions** on dead compute; that waste
     dwarfs any compile-once benefit. (Short, variable text is exactly the
     workload static shapes punish.)

   So the dynamic-shape worry resolves to: dynamic doesn't recompile-storm, and
   static doesn't rescue throughput -- both lose to CPU.

3. **The fp16 CoreML path is NaN-prone on this graph.** Through the SHIPPED
   `OrtEmbedder::text` CoreML path (which sets `with_subgraphs(true)`, inherited
   from the CLIP recipe), the partitioned fp16 execution emitted NaN vectors on
   the 200-text corpus. A NaN-free CoreML result is only obtainable on some
   inputs, not reliably across the corpus. (RMSNorm's sum-of-squares and the
   attention masking overflow fp16 on the partition boundaries; the official
   `model_fp16.onnx` keeps RMSNorm in fp32 on CPU, but CoreML re-partitions and
   re-introduces the instability.) Even setting aside speed, this alone is a
   no-ship: the embedding space would be corrupted.

Even in the best case for CoreML (subgraphs off, the inputs that stay finite),
the cosine vs CPU int8 is ~0.997 -- i.e. accuracy is NOT the blocker; **speed and
stability are**. There is simply no shape strategy where CoreML beats int8/CPU
for this small, short, variable-length text tower.

Bonus context: the text embedder is **not the ingest bottleneck** -- that is the
CLIP visual tower (~18 img/min CPU, the seam CoreML/FP16 actually fixes 8.8x in
`docs/SPIKE-COREML.md`). At ~21 embeds/s, int8/CPU text-embed is already a
non-issue. Spending a CoreML compile + a re-embed + a second vector space to make
a non-bottleneck ~35% SLOWER would be strictly negative.

## The conversion recipe (deltas vs the CLIP FP16 recipe)

To get a CoreML-loadable FP16 model you need an FP16 export with weights INLINED
into a single self-contained `model.onnx` (the form that clears CoreML's
external-data path mis-resolution -- same blocker the CLIP int8 export hit). You
CANNOT upscale the shipped int8 to fp16 (fake precision); start from FP32.

Source: `onnx-community/embeddinggemma-300m-ONNX` on HuggingFace. It ships both
`onnx/model.onnx` (FP32, ~1.23 GB external data) and `onnx/model_fp16.onnx`
(FP16, ~617 MB external data). Env: a Python 3.12 venv with `onnx==1.21`,
`onnxconverter_common`, `onnxruntime==1.26`, `huggingface_hub`, `tokenizers`,
`numpy`.

Two deltas from the CLIP recipe surfaced and matter:

1. **Gemma overflows fp16 if you naively convert FP32.** The CLIP recipe's
   `convert_float_to_float16(keep_io_types=False)` + retarget-all-`Cast(to=FLOAT)`
   produced a model that loaded but returned **NaN**: EmbeddingGemma's
   `SimplifiedLayerNormalization` (RMSNorm) sum-of-squares overflows fp16's
   range. The fix that the model authors already baked into the repo's
   `model_fp16.onnx` is to keep RMSNorm in FP32 (an `op_block_list`), while the
   MatMul body goes fp16. Trying to reproduce that by hand with the converter's
   `op_block_list=[MultiHeadAttention, SimplifiedLayerNormalization, ...]` ALSO
   broke load, because the converter mishandles the boundary casts around the
   `com.microsoft` fused ops (`Type Error ... bound to different types`). So:

   **Use the repo's pre-validated `onnx/model_fp16.onnx` and just INLINE it.** It
   already has the right f32 I/O (int64 `input_ids`/`attention_mask` in, f32
   `last_hidden_state` + `sentence_embedding` out -- exactly what the connector's
   `run_text` feeds and `try_extract_tensor::<f32>()` reads), is NaN-free on CPU,
   and only needs external-data inlining for CoreML:

   ```python
   import onnx
   m = onnx.load("onnx/model_fp16.onnx")            # resolves the .onnx_data
   onnx.save_model(m, "model.onnx", save_as_external_data=False)  # single file
   ```

   Output: one self-contained `model.onnx` ~618 MB (half the FP32 size), no
   `.onnx_data` sidecar. Staged at `models/embeddinggemma-300m-fp16/` mirroring
   the q8 layout (`onnx/model.onnx` + `tokenizer.json`). NOT committed.

2. **No hand-wrapping of f32 I/O is needed (unlike CLIP).** The CLIP visual tower
   has an f32 image input that needed a `Cast(f32->f16)` wrapper. EmbeddingGemma's
   inputs are int64 token ids/mask (they stay int; CoreML and CPU both take ints
   natively) and the repo's `model_fp16.onnx` already keeps the two outputs f32.
   So the connector's `run_text` works unchanged against this single file.

## Validation (FP16 lossless vs FP32? YES)

FP16 vs FP32 reference, both ORT CPU EP, the connector's exact text pipeline
(doc prompt `title: none | text: `, `add_special_tokens=True` -> `<bos>..<eos>`,
mean-pool the f32 last hidden state, L2-normalize), on the spike's 4 paraphrase
pairs + unrelated foils (`ort_embedder.rs::real_model_tests::PARAPHRASES/UNRELATED`):

| metric | value |
|---|---|
| mean cosine FP16-vs-FP32 (12 texts) | **1.000000** |
| min cosine FP16-vs-FP32 | **1.000000** |
| FP16 paraphrase margin (mean) | **+0.314** (per-pair `[0.379, 0.257, 0.232, 0.388]`) |
| FP32 paraphrase margin (mean) | +0.314 |

The FP16 model is numerically identical to FP32 on CPU (margin matches the
spike's ~+0.310 / `+0.314` measured floor, comfortably past the +0.05 sanity
bar). The instability is introduced ONLY by CoreML's re-partitioned fp16
execution (reason #3 above), not by the FP16 weights themselves.

## Accuracy: int8/CPU vs fp16/CoreML

On the inputs where the CoreML fp16 path stayed finite, cosine(CPU-int8 vec,
CoreML-fp16 vec) ≈ **0.997** (mean), min ~0.997 -- retrieval-safe. So if CoreML
were faster and stable, the space change would be acceptable. It is neither.
Accuracy is not what kills this; throughput and NaN are.

## How to reproduce (this machine)

```bash
# 1. Confirm the linked onnxruntime carries the CoreML EP:
cargo test -p photoproof-connectors --test coreml_spike_text \
    text_coreml_provider_available -- --ignored --nocapture

# 2. End-to-end CPU(int8) vs CoreML(fp16) embeds/sec + cosine, via the SHIPPED
#    OrtEmbedder::text path (CoreML toggled by PHOTOPROOF_ORT_COREML):
cargo test --release -p photoproof-connectors --test coreml_spike_text \
    text_cpu_int8_vs_coreml_fp16 -- --ignored --nocapture

# 3. The dynamic-vs-static shape finding (raw ort sessions, the static-shapes
#    flag the connector does not expose, + with_profile_compute_plan):
cargo test --release -p photoproof-connectors --test coreml_spike_text \
    text_coreml_dynamic_vs_static_shapes -- --ignored --nocapture
```

The harness lives at `crates/photoproof-connectors/tests/coreml_spike_text.rs`
(all `#[ignore]`, measurements not gates; they skip cleanly without the local
int8 snapshot + the staged single-file `-fp16` dir). The FP16 model is not
committed; rebuild it with the recipe above.

## Constraint check

- **No production model-selection wired:** this is a measurement spike only.
  `OrtEmbedder::text` still passes `coreml=false` (the text embedder stays CPU
  structurally, as documented in `ort_embedder.rs`); nothing in `model_specs`
  selects an `embeddinggemma-...-fp16` id. The verdict (DON'T-SHIP) is the reason
  to leave it that way.
- **CPU default byte-identical / no model files committed / gate green:**
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test -p photoproof-connectors` all pass (the new spike tests are
  `#[ignore]`). No em-dashes in the new code/doc strings.
