# Research: RAG & Vector Retrieval

Validation of spec/RETRIEVAL.md and spec/RUNTIME.md §3.3 against 2024–2026
state of the art.

## Is this a proper RAG system?

**Yes — in places ahead of common practice.** What RETRIEVAL.md specifies is
a hybrid retrieval system (the product) plus a RAG context layer (§8) for LLM
calls — the right decomposition, since the product's primary output is
images, not generated text.

| Best-practice component | Photoproof | Status |
|---|---|---|
| Hybrid sparse+dense retrieval | FTS5 BM25 + text vectors + CLIP | ✅ |
| Rank fusion | weighted RRF k=60 | ✅ standard default ([Digital Applied 2026](https://www.digitalapplied.com/blog/hybrid-search-bm25-vector-reranking-reference-2026), [MongoDB](https://www.mongodb.com/resources/products/capabilities/hybrid-search)) |
| Metadata filtering | filter AST via constrained LLM parse | ✅ self-query pattern, done more rigorously |
| Cross-encoder reranking | was absent | ⚠️ now an optional eval-gated stage (amendment) |
| Provenance | mandatory verbatim quote | ✅ stronger than most production RAG |
| Eval harness | was absent | ⚠️ added (amendment) |
| Embedding prefixes | was unspecified | ⚠️ now normative (amendment) |
| Query rewriting (HyDE/multi-query) | absent | ✅ correctly absent |

## Verdicts

- **Hybrid + weighted RRF k=60 — VALIDATED.** RRF's rank-only fusion
  sidesteps BM25/cosine score incompatibility. Nuance: a tuned convex
  combination beats RRF with only a handful of labeled queries
  ([Bruch et al., TOIS](https://arxiv.org/abs/2210.11934), [Pinecone summary](https://www.pinecone.io/research/an-analysis-of-fusion-functions-for-hybrid-retrieval/)) —
  revisit after the eval harness exists.
- **No reranker — was the one real gap.** +5–15 nDCG@10 typical
  ([Local AI Master](https://localaimaster.com/blog/reranking-cross-encoders-guide),
  [BSWEN 2026](https://docs.bswen.com/blog/2026-02-25-best-reranker-models/));
  Qwen3-Reranker-0.6B ≈ 380 ms/query CPU ([production writeup](https://medium.com/@oliversmithth852/building-a-production-rag-system-qwen3-embeddings-reranking-and-vector-database-insights-9c114c5f9da8)),
  bge-reranker-v2-m3 ≈ 130 ms/16-pair batch CPU; our candidates are 1–3
  sentences, so cheaper than benchmarks. Sweet spot: rerank top 20–30.
- **Chunking 512/64 sentence-snapped — VALIDATED** (256–512 is the general
  sweet spot — [NVIDIA chunking study](https://developer.nvidia.com/blog/finding-the-best-chunking-strategy-for-accurate-ai-responses/));
  ≥95% of our docs are single-chunk utterances. The real risk is inverse:
  **tiny one-sentence texts** benefit measurably from added context at embed
  time ([One Word Is Not Enough](https://arxiv.org/html/2512.06744)) and
  asymmetric query/document prefixes are mandatory hygiene
  ([nomic card](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5),
  [e5 discussion](https://huggingface.co/intfloat/multilingual-e5-large/discussions/34)).
- **Qwen3-Embedding-0.6B — VALIDATED** (strongest open model in class:
  80.83 MTEB retrieval, 32k ctx, MRL 32–1024d, instruction-aware —
  [card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B),
  [paper](https://arxiv.org/pdf/2506.05176)). **Card warns: omitting the
  query instruction costs 1–5%.** Lighter credible swap:
  EmbeddingGemma-308M ([HF blog](https://huggingface.co/blog/embeddinggemma));
  bge-m3 comparable but heavier; nomic weaker
  ([Milvus 2026 comparison](https://milvus.io/blog/choose-embedding-model-rag-2026.md)).
  Matryoshka truncation 1024→512 ≈ 1–2% cost
  ([HF quantization blog](https://huggingface.co/blog/embedding-quantization)).
- **Flat-file mmap f32 brute force — VALIDATED at v1 scale, numbers were
  optimistic.** Scan is memory-bandwidth-bound (effective ~30 GB/s desktop —
  [apxml](https://apxml.com/courses/advanced-vector-search-llms/chapter-2-optimizing-vector-search-performance/hardware-acceleration-considerations),
  [BigData Boutique](https://bigdataboutique.com/blog/scaling-vector-search-performance-from-millions-to-billions-8d50a1));
  p95 < 50 ms ⇒ scan ≤ ~1.5 GB:

  | Encoding | Bytes/vec (1024d) | N for p95<50ms | 2M ceiling? |
  |---|---|---|---|
  | f32 1024d (old spec) | 4 KB | ~375k | ✗ (~270 ms) |
  | int8 1024d | 1 KB | ~1.5M | borderline |
  | **int8 + MRL 512d** | 512 B | ~3M | ✓ (~35 ms) |

  int8 ≈ 1–1.5% recall cost, perpendicular to MRL
  ([HF](https://huggingface.co/blog/embedding-quantization)). Store choice
  validated: sqlite-vec still pre-v1 alpha with a 2025 maintenance stall
  ([issue #226](https://github.com/asg017/sqlite-vec/issues/226)); LanceDB
  Rust API unstable ([docs.rs](https://docs.rs/lancedb/latest/lancedb/));
  usearch is the mature ANN swap target. Owning the format keeps the
  redaction zeroing guarantee.
- **Max-per-signal image aggregation — VALIDATED** (MaxP beats SumP; sum has
  documented verbosity bias — [Zhang et al., ECIR 2021](https://cs.uwaterloo.ca/~jimmylin/publications/ZhangXinyu_etal_ECIR2021.pdf),
  [PARADE](https://arxiv.org/pdf/2008.09093)).
- **LLM query parse — VALIDATED**; **HyDE/multi-query correctly omitted**
  (25–60% latency tax on small local LLMs; hallucination-prone in fact-bound
  personal corpora; query vocabulary ≈ document vocabulary here —
  [EmergentMind](https://www.emergentmind.com/topics/hypothetical-document-embeddings-hyde),
  [production analysis](https://medium.com/@mudassar.hakim/retrieval-is-the-bottleneck-hyde-query-expansion-and-multi-query-rag-explained-for-production-c1842bed7f8a)).
- **Concentric context assembler — VALIDATED + LITM addition** (lost-in-the-
  middle persists in 2025 even at 128k — [Liu et al.](https://arxiv.org/abs/2307.03172),
  [ICLR 2025](https://proceedings.iclr.cc/paper_files/paper/2025/file/5df5b1f121c915d8bdd00db6aac20827-Paper-Conference.pdf)).

## Amendments (all applied)

- **R1** Instruction prefixes normative (queries instructed, documents bare,
  template version in inputs_hash).
- **R2** PPVEC v2: dtype field; int8 + MRL-512 stored default; spike decides
  truncation with a small eval; redaction zeroing unchanged.
- **R3** Latency math corrected; multithreaded SIMD scan required; swap
  trigger restated in bytes-scanned (~1.5 GB/space); mmap prewarm note.
- **R4** Optional `Reranker` trait (Qwen3-Reranker-0.6B / bge-reranker-v2-m3,
  ONNX int8 CPU, top 20–30), M3+, gated on eval.
- **R5** Deterministic context prefix for tiny chunks at embed time only
  (provenance still quotes bare folded text; scheme version in inputs_hash;
  no generated context — preserves rebuild byte-equality).
- **R6** Golden query set (~50–100 pairs) + recall@20 / nDCG@10 per signal
  and fused — unblocks RRF weights, S4 threshold, convex-combination
  upgrade, reranker go/no-go.
- **R7** LITM ordering guidance in §8 (top evidence at start and end).
- **R8** Benchmark EmbeddingGemma-308M in the runtime spike.

## Non-issues

ANN/HNSW/vector DB servers (brute force + R2 covers the ceiling; ANN matters
past ~3–5M) · HyDE/query expansion · learned fusion/LTR (no training data) ·
GraphRAG/ColBERT/SPLADE (wrong corpus shape) · embedding throughput (bounded
backfill) · FTS5 at 2M short rows · RRF staleness (it isn't).
