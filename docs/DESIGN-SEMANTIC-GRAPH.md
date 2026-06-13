# DESIGN — Semantic topic-graph (force-directed library lens)

Draft for founder ratification, June 13 2026. Founder-directed in conversation.
A new spatial NAVIGATION lens: images arranged in a force-directed graph,
pulled toward named TOPIC anchors by how strongly they relate to each topic.
Not a black-box t-SNE blob: the anchors are interpretable topics, so the space
has meaning.

## The core mechanic (the engine already exists)
"More like this" (landed `3ea6f2f`) proved the primitive: rank every image by
similarity to a reference vector. The graph generalizes the anchor from an
IMAGE to a TOPIC PHRASE.

- **Topic anchor nodes** sit at fixed-ish positions (e.g. around a circle).
- **Image nodes** are each pulled toward every topic by a force ∝ their
  similarity to that topic, plus mutual repulsion between images.
- An image relating to two topics floats BETWEEN them; clusters form around
  topics; bridge-images sit in the gaps. The layout IS a semantic map.

## Topics — three sources, all wanted (staged)
Founder: all three, with nice "add topic" UI and SUGGESTED topics to click.
1. **v1 — manual seed.** Type/add a topic phrase; we embed + pull. Plus a
   suggestion chip-rail to click: cheap suggestions first = frequent note
   n-grams + collection names. Fully buildable now.
2. **v2 — cluster auto-labels.** Cluster the embeddings; label each cluster by
   its nearest note phrase. Suggestions get smarter, "connected to notes."
3. **v3 — LLM topic suggestion.** Once Gemma is wired, extract N themes from the
   scope's notes/summaries as suggested topics. Richest; waits on the LLM.

## Looks vs. what-you-said — a blend slider (founder preferred over a toggle)
Each topic phrase is embedded in BOTH spaces:
- **CLIP text tower** → compare to each image's `image_clip` vector (what it
  LOOKS like).
- **Text embedder** → compare to each image's `annotation_chunk` / `summary`
  vector (what you SAID about it).

Pull strength toward a topic = `α·cos(visual) + (1-α)·cos(annotation)`, where
**α is a 0-100% slider** (default ~50%). This reuses the same blend instinct as
the hybrid fusion β and the S1/S3/S4 weights — one tuning model across search
and the graph. (A toggle is the degenerate α∈{0,1}; the slider is barely more
work and much nicer, so: build the slider.)

## Scale (founder: yes full library, but watch scaling)
- **Collection / folder scope → live physics.** Dozens-to-hundreds of nodes run
  a real force sim smoothly (d3-force-class or a small custom sim).
- **Full library (10k+) → LOD.** Options to prototype + measure: cluster into
  representative super-nodes that expand on zoom; or run the sim once and cache
  positions; or aggregate into per-topic density fields. Decide empirically —
  prototype on a collection first, profile, then pick the library strategy.

## Interaction (it's a navigation surface)
Click a topic → scope the grid to it. Click an image → open in Look. Drag to
explore. Ties into the backlog's "trajectories as an alternate grid lens" and
"region-conditioned visual embeddings."

## Backend additions needed (founder okayed adding backend)
- Embed an arbitrary topic phrase in BOTH the CLIP-text and text-embedder spaces
  (both embedders exist).
- Score all in-scope images vs a topic in each space (reuse the `find_similar` /
  `VectorStore::search` machinery; here the query is a topic embedding).
- Enumerate scope images + their vectors (collection members / folder).
- (v2) a clustering pass over the in-scope embeddings + nearest-phrase labeling.
- (v3) an LLM topic-extraction call once Gemma lands.
The force LAYOUT itself is frontend.

## Phasing
1. **v1**: manual-seed topics + cheap suggestions (note n-grams, collection
   names) + the looks/said blend slider + live force layout on a COLLECTION.
   Click-topic-to-scope, click-image-to-open. Profile it.
   **Then deliberately point v1 at the FULL LIBRARY (founder wants to feel the
   scale issues early)** — even unoptimized, to see where the force sim / vector
   scan / render falls over. That measurement, not a guess, picks the v2 LOD
   strategy. Expect it to struggle at 10k+; that's the point of the spike.
2. **v2**: cluster auto-topic labels; full-library LOD strategy from the v1
   profiling.
3. **v3**: LLM topic suggestion (gated on Gemma being wired).

## v2 — built (June 13 2026)
- **Cluster auto-labels.** `cluster_topics(scope, k?, space?)` runs a small,
  deterministic k-means (farthest-first seeding by index, fixed iteration order
  — no RNG, so labels are reproducible and testable) over the in-scope image
  vectors. Clusters the ANNOTATION space (`image_summary`) by default since the
  labels are note-grounded; CLIP is optional. k = `clamp(round(sqrt(n/2)),
  cluster_k_min, cluster_k_max)` unless passed. Each cluster is LABELED by the
  most representative salient n-gram in its members' notes (reusing v1's
  `mine_ngrams` miner; most frequent, then longer phrase, then alphabetical),
  with a generic `Group N` fallback when a cluster has no notes. Returns
  `[{ label, size, centroid_affinity }]` feeding the suggestion rail as smarter,
  note-grounded auto-topics above the v1 n-gram chips. Empty/un-embedded scope
  returns empty, never errors. Reads STORED vectors (no embed pass): the model
  id comes from the active embedder when loaded, else any stored row.
- **Full-library LOD.** Past `graph.lod_threshold` nodes (default 1500) the
  frontend AGGREGATES images into SUPER-NODES (binned by dominant topic; a
  super-node's mass = member count, position = members' affinity-weighted
  centroid). The pure force sim weights repulsion by the product of masses and
  divides each node's acceleration by its own mass, so an aggregate of N images
  behaves like the cluster it replaces — and a single image (mass 1) recovers
  the v1 integrator exactly. A super-node EXPANDS into its members on click or
  on zoom past `LOD_ZOOM_EXPAND`, and COLLAPSES on zoom-out; the sim runs over
  the current (mixed) node set, within the budget the v1 spike measured. The v1
  banner now reads "LOD active (showing N clusters of M images)" instead of the
  scale-spike warning, keeping the node-count + scan-time telemetry.
  **FOUNDER-REVIEW:** the 1500 default is a placeholder picked just above v1's
  ~1200-node strain banner. Reconcile it with the REAL full-library scale-spike
  numbers once the founder profiles the spike (DESIGN's whole premise: the
  measurement, not a guess, picks the LOD threshold).
- **v3 LLM seam (scaffold only).** `suggest_topics_llm(scope)` exists end-to-end
  (command + IPC + a hidden rail) but the Gemma connector is NOT wired (mocked in
  M1), so it always returns the explicit `Unavailable` state and the UI shows the
  cluster + n-gram suggestions meanwhile. The `TopicLlm` trait + `WiredTopicLlm`
  placeholder mark the seam; `// TODO(v3): wire when the LLM connector lands`.
  LLM suggestions appear on the rail ONLY when the connector becomes real.

## New tuning knobs (`[graph]`)
- `cluster_k_min` (2) / `cluster_k_max` (12): k-means k bounds. Range [1, 64].
- `lod_threshold` (1500): node count past which LOD aggregates. Range [50, 1e6].

## Open decisions for the founder
- Anchor layout: topics on a ring (stable, readable) vs. topics themselves
  force-placed (organic but jumpier)? (lean ring for v1.)
- Default α (looks vs said) — 50/50 start?
- RESOLVED: collection-first to get it working, THEN run it on the full library
  to surface the scale issues empirically before designing LOD (founder, June
  13 2026).
- OPEN (v2): reconcile `graph.lod_threshold` (placeholder 1500) with the real
  full-library scale-spike numbers once profiled.
