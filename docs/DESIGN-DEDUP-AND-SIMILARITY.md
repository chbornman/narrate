# DESIGN: Similarity grouping + duplication-tolerance (the "hide-for-variety" lens)

Status: **exploratory / wide-think** (founder, June 17 2026). Not yet a packet.
Grounded in a deep-research pass on SOTA dedup (24 cited claims adversarially
verified, 1 refuted; sources at the end). Spec wins; this is a design to react to.

## The spark

With **zero topics**, the visualizer already groups by visual similarity — the
founder's `photoproof_test_set` screenshot shows near-dup bursts stacking tightly,
and B&W / silhouette / family shots pooling apart. That clustering is **emergent**
from the CLIP-cosine k-NN neighbor springs (`graph_neighbors` → `knn_within`,
`ppvec.rs:1030`); the union-find we built for soft topics
(`synthesis.ts unnamedClusters:367`) already carves that graph into clumps. **The
signal is built. This is about surfacing it** as two opt-in controls.

## The one distinction that organizes everything

The research is unambiguous: **"is this the SAME photo" and "do these LOOK alike"
are different questions needing different tools** — a speed-vs-robustness trade,
not one method dominating (MDPI Electronics 2025; Meta SSCD). PhotoProof should
run **three tiers**, cheapest-first:

| Tier | Question | Tool | Status |
|---|---|---|---|
| 0 — exact | byte-identical | **BLAKE3** content hash | ✅ built (K13) |
| 1 — same photo (re-save/resize/light edit) | precise near-dup | **perceptual hash** (dHash/pHash) + Hamming | ❌ the one small add |
| 2 — looks alike / same scene/session | semantic group | **CLIP cosine k-NN** + union-find | ✅ ~built |

Crops/rotations beyond a few degrees fall *between* 1 and 2: perceptual hashes
miss them (mirroring is the single most disruptive transform — it "scatters
scores as if unrelated", DFRWS 2023), but CLIP catches them. So **CLIP is also our
crop/rotation-robust fallback** — we likely do *not* need a heavyweight local-
feature tier (ORB/AKAZE + geometric verification) unless precise crop-dedup proves
necessary (open question).

## Tier 1 — perceptual hash (the precise "same photo" cull)

- **Algorithm:** **dHash (gradient) or pHash (DCT)**, NOT aHash. Both are robust
  to upscaling and low-quality JPEG because they downscale during preprocessing
  (DFRWS 2023). 64-bit hash, compared by **Hamming distance**.
- **Rust:** `img_hash` / `image_hasher` (qarmin fork) implements aHash/dHash/pHash/
  Blockhash with `hash1.dist(&hash2)` (popcount of XOR). Pure Rust, no C/C++.
- **Where it's computed:** during the **preview pass** — we already decode each
  image to a preview, so hashing a 32×32 downscale is nearly free, CPU-only, no
  GPU. Store as a **derived, rebuildable `u64`** column in SQLite (an index, not
  truth — same status as vectors/previews).
- **Threshold — MUST be empirically calibrated.** The research's one *refuted*
  claim is instructive: do NOT assume a textbook normal distribution of Hamming
  distances. Tune the cutoff on PhotoProof's own libraries. Starting point from
  practice: inter-image distance centers near 0.5 (coin-flip per bit) for a good
  discriminator; "same photo" lives at a small Hamming radius (≈ ≤6–10 / 64).
- **What it powers:** a **"Duplicates" detector** — near-dup **stacks** (collapse
  like RAW+JPEG already do; folds in the open "Burst/HDR-bracket stacks" backlog
  item) and a **cull review** ("142 near-dups in 38 groups; keep the sharpest").

## Tier 2 — CLIP cosine (the "looks alike" grouping, already built)

Reuse `knn_within` + `unnamedClusters`. The research backs this as the *robust*
tier: CNN/CLIP embeddings beat all four classical hashes across near-dups and
geometric transforms (MDPI 2025; SSCD 67% vs PDQ 25% recall@99% on geometric
transforms). This is what the screenshot is already doing — no new model.

## The duplication-tolerance slider (hide redundancy → surface variety)

The founder's idea is **not a delete tool** — it **hides** images similar enough
(often same-session) so fewer, more *varied* results show. This maps cleanly onto
established **diverse-subset / representative-selection** algorithms that run
**on top of the CLIP k-NN graph we already maintain**:

- **(a) Facility-location / k-medoid** — `f(X)=Σ_i max_{j∈X} s_ij`, "cover every
  image with a representative" (≈ k-medoid clustering); its naive O(n²) is
  approximated on a **nearest-neighbor graph** — directly consumable from
  `knn_within` (arXiv 1805.11191). **This is the recommended core.**
- **(b) Max-sum diversification / MMR** — a single **λ knob** trading quality vs
  diversity (arXiv 1203.6397) — the natural mapping for *one slider*.
- **(c) Disparity-min / farthest-point (k-center)** — greedy 2-approx, maximizes
  the minimum pairwise distance — "maximally distinct subset" (Gonzalez 1985).

**UX template (verified from real tools):** dupeGuru and digiKam both expose a
single **0–100% similarity threshold** ("99% = very similar but allow a bit of
fuzz"; lowering it excludes exact copies and burst/series shots). That 0–100%
control is a direct template for our slider. (dupeGuru's own engine, for the
record, is a 15×15 grid of per-tile average colors summed into a color-diff score
— cruder than a perceptual hash; we can do better.)

**How one slider behaves across two distance spaces** (the real design knot):
recommended split — the **slider drives the CLIP "diversify" view** (cosine space:
tolerance → similarity cutoff → facility-location collapses each cluster to a
representative; representative = highest-rated / sharpest / medoid). The
**perceptual-hash tier is a separate "find exact duplicates" mode** (Hamming
space), surfaced as stacks + cull, not on the same slider. Keep them distinct so
each control behaves intuitively. (A unified single-slider mapping is an open
question below.)

## Bursts / "same moment"

Fuse **EXIF `capture_ts` proximity** with visual similarity — the digiKam /
Lightroom-stacking / Apple-Google-Photos pattern. We have `capture_ts`. A
**"best of burst" auto-pick** (keep the sharpest/eyes-open) is feasible later: a
learned per-frame goodness ranker (ECCV 2018, MS Research) matched users' top
choice 64% / top-3 86%, at 0.47 MB / 13 ms on a phone — but it's a 2018 model with
self-reported numbers; a simple sharpness/variance-of-Laplacian heuristic is the
cheap v1.

## Indexing / scale (don't over-build)

PhotoProof's libraries are **tens of thousands**, occasionally more. At that scale:
- **Tier 1 Hamming search:** a **BK-tree or even linear scan is adequate**;
  multi-index hashing (MIH — exact k-NN, 300–1000× over linear scan, proven to 1B
  codes) is there if we ever need it, but its sub-linear guarantee assumes
  near-uniform codes and is overkill for us now.
- **Tier 2 CLIP ANN:** we already query PPVEC; if it needs to scale, pure-Rust
  **`hnsw_rs`** (native cosine, ~15k q/s @ 0.99 recall on SIFT1M) or
  `instant-distance` drop in without C/C++.

## Architecture fit (the PhotoProof-specific part)

- **Derived, rebuildable:** the perceptual-hash `u64` joins vectors/previews as a
  derived index (computed in the preview pass; survives a DB wipe via rebuild).
- **Sidecar = truth:** *detection* is derived; **keep/cull/hide decisions are
  truth → sidecar events**. Hiding is a non-destructive view filter; an actual
  cull is an event the user can walk away from.
- **Scope axis:** both controls drop onto the existing orthogonal scope/lens axis
  (a "Duplicates" scope + a "Diversify" toggle with the slider), composing with
  grid and visualizer. **Opt-in** — it's destructive-adjacent.
- **Reuse:** Tier 2 + the slider are ~80% existing code (`knn_within`,
  `unnamedClusters`); only Tier 1's `u64` + a greedy facility-location pass are new.

## Recommended phasing (each shippable)

1. **Perceptual hash at ingest** (`img_hash` dHash → `u64` column) + a **near-dup
   "Duplicates" scope** (BK-tree/linear Hamming, threshold calibrated on real
   libraries) showing stacks. Highest precision, smallest blast radius.
2. **Duplication-tolerance "Diversify" slider** over CLIP cosine: greedy
   facility-location on `knn_within` → hide non-representatives. Mostly reuse.
3. **Burst grouping** = capture_ts + similarity → stacks; then a sharpness-based
   best-of-burst pick.
4. **Best-of-burst learned ranker** (only if the heuristic underwhelms).

## Open questions (from the research + ours)

1. **Exact Hamming threshold** on *our* photo data (the normality assumption was
   refuted — calibrate empirically, don't assume).
2. **Tier boundary for crops/rotations:** is CLIP cosine enough, or add an
   ORB/AKAZE local-feature tier for crop-robust "same photo"? (Probably CLIP is
   enough; verify on real crops.)
3. **One slider, two spaces:** keep Hamming-dedup and cosine-diversify as separate
   controls (recommended), or unify into one mapping? And which diversity
   objective — facility-location vs MMR-λ vs disparity-min — feels best?
4. **MIH vs BK-tree vs linear** at our real scale (linear is likely fine).

## Adjacent use cases the same machinery serves (medium confidence)

The same two-tier stack (exact + perceptual hash for "same photo"; CLIP ANN for
"same scene/derivative") is exactly what these domains use — worth knowing the
engine generalizes:
- **ML training-set de-duplication / dataset decontamination** (submodular
  representative selection is literally motivated by "learning from less data").
- **Copyright / reverse-image / stolen-photo matching** (Meta SSCD was built for
  copy detection + abuse).
- **Content-versioning** — find every edit/export of one original (the precise
  near-dup tier with tolerance loose enough to survive re-encode).
- **Backup/storage dedup** and **near-dup spam/abuse detection.**

## Sources (verified)

- MDPI Electronics 2025/2026 (hash-vs-CNN benchmark) — https://www.mdpi.com/2079-9292/15/7/1493
- Meta SSCD copy detection — https://arxiv.org/abs/2202.10261
- ACM perceptual-hashing survey 10.1145/3727880 — https://dl.acm.org/doi/10.1145/3727880
- McKeown & Russell, perceptual-hash robustness (DFRWS/FSI 2023) — https://www.sciencedirect.com/science/article/pii/S2666281723000100
- dupeGuru picture-mode algorithm — https://dupeguru.voltaicideas.net/help/en/scan.html
- digiKam similarity (Haar/Fast Multi-Resolution Querying) — https://docs.digikam.org/en/left_sidebar/similarity_view.html
- Multi-index hashing (Norouzi et al., TPAMI 2014) — https://ar5iv.labs.arxiv.org/html/1307.2982
- `img_hash` (qarmin) — https://github.com/qarmin/img_hash · `image_hasher` — https://docs.rs/image_hasher/
- `hnsw_rs` — https://github.com/jean-pierreBoth/hnswlib-rs · `instant-distance` — https://github.com/djc/instant-distance
- Submodular diversity (facility-location, disparity-min) — https://arxiv.org/pdf/1805.11191
- Max-sum diversification / MMR — https://arxiv.org/pdf/1203.6397
- Real-time burst best-frame (ECCV 2018, MS Research) — https://arxiv.org/pdf/1803.07212

*Research caveat: the flagship MDPI benchmark URL 403'd but is corroborated by
SSCD + long-standing consensus; benchmark throughput numbers are hardware-
dependent; the burst model is 2018 + self-reported; DPP was named but not
independently verified here. Treat thresholds as starting points to calibrate.*
