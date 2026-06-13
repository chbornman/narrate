# DESIGN — Topics, Autosuggest, and the Topic→Collection bake

Founder-directed, June 13 2026. The unifying model behind the semantic graph,
autosuggest collections, and the topics sidebar tab. They are ONE system.

## The relationship (ratified)
- A **topic** is *continuous, computed, fuzzy*: a phrase embedded into the
  vector spaces, with a similarity SCORE for every image. A lens. Nothing is
  "in" it; images are near or far. It shifts as embeddings/notes evolve.
- A **collection** is *discrete, curated, durable*: explicit EVENTED membership
  (in/out, `added_ts`/`removed_ts`), authored intent, its own notes. A decision.

They are NOT the same. A topic is the **generative** layer; a collection is the
**committed** layer. The bridge between them is a one-way BAKE (founder: "one-way
for now"), exactly like RAW → develop: a topic + a threshold = a live selection;
commit it = a durable collection that then decouples and is yours to hand-edit.
The topic stays fuzzy; the collection is the snapshot. (Re-sync-from-topic is a
possible advanced toggle later; v1 is a clean one-way bake, recording "born from
topic X @ threshold T" as provenance only.)

## The four things this unifies
1. **Autosuggest** — the system proposes topics by clustering (graph v2's
   `cluster_topics`) plus other signals.
2. **The graph** — those topics as force-anchors you explore.
3. **The slider** — a topic becomes a live selection (drag threshold → the
   images scoring above it glow in the graph).
4. **Collections** — commit the glowing selection; it bakes into an evented
   collection.

So autosuggest is NOT a separate feature: the graph IS the autosuggest UI.

## Topics sidebar tab (founder, June 13 2026)
A THIRD rail tab next to Folders and Collections: **Topics**. It lists the SAME
topic set as the graph view — manually created topics + autosuggested ones —
and, when one is selected, shows its images in the normal GRID view in RANKED
order (highest affinity first). So the graph and the topics tab are two views of
one topic set: the graph is spatial/exploratory, the tab is a ranked grid (the
familiar surface for actually working the images).

- **Manual topics persist** (a saved phrase, like a saved search): a small
  `topics` table `( id, phrase, space?, created_ts )`. Editable/removable.
- **Autosuggested topics are computed** (cluster labels + signal candidates),
  surfaced alongside the manual ones, ranked by cluster tightness / size.
- A topic's "images" are ALWAYS computed affinity (no stored membership — that
  is precisely what distinguishes a topic from a collection). Selecting a topic
  in the tab scopes the grid to its ranked images (reusing the gridScope
  machinery, a `topic` scope variant like `similar`/`query`).

## The slider-to-collection (the bake gesture)
In the graph (and the topics tab): select a topic → a **threshold slider**
appears → images with blended affinity above the threshold HIGHLIGHT (glow /
halo in the graph; a selection in the grid) → **"Make a collection"** bakes the
current selection into an evented collection (`create_collection_from_selection`,
provenance recorded). Visually: the "nearby" set lighting up as you drag is the
signature moment.

## Autosuggest backend plan (build after graph v2; reuses its engine)
- **Core**: `cluster_topics` (CLIP for visual coherence + annotation/summary for
  note-grounded coherence) → candidate topics with member sets + per-image
  scores. (Built in v2.)
- **Additional candidate signals** (BACKLOG autosuggest item): co-annotation in
  one session (images marked together), repeated phrases across notes, time +
  folder affinity. Each yields candidate groupings.
- `suggest_collections(scope)` → `[{ label, members[], scores, source }]` —
  quiet, never auto-creates (K14: machine never authors content into the store;
  it proposes, the human commits).
- `create_collection_from_selection(hashes, name)` — the bake; records the
  origin topic + threshold as provenance, then an independent evented collection.
- Manual-topic CRUD commands for the `topics` table.

## Phasing
1. Topic scope + Topics sidebar tab (ranked grid view) over manual topics +
   v2 cluster suggestions.
2. The slider-to-collection bake (graph + tab) + `create_collection_from_selection`.
3. The extra autosuggest signals (co-annotation, repeated-phrase, time/folder).

## Why this is on-thesis
Collections are the core of helping the photographer think/plan/organize
(founder). Topics are the machine's *proposals*; the bake keeps the human as the
author of every durable decision. The system surfaces structure (clusters,
affinity, the synthesis overlay's overlooked-bodies-of-work); the photographer
decides what becomes a collection.
