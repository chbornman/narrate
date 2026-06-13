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

## Open decisions for the founder
- Anchor layout: topics on a ring (stable, readable) vs. topics themselves
  force-placed (organic but jumpier)? (lean ring for v1.)
- Default α (looks vs said) — 50/50 start?
- RESOLVED: collection-first to get it working, THEN run it on the full library
  to surface the scale issues empirically before designing LOD (founder, June
  13 2026).
