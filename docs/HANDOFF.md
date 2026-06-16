# Handoff — visualizer + state-integrity work (June 2026)

A working handoff for the next agent. Spec still wins (`spec/`); permanent open
work belongs in `docs/BACKLOG.md`. This is the session-context the backlog lines
don't carry. Everything described as DONE is committed + pushed to `origin/main`.

## Where things stand

The semantic topic-graph **visualizer** was reworked end to end, and a batch of
**state-integrity / self-heal** robustness fixes landed (from
`docs/STATE-INTEGRITY-AUDIT.md`). Working tree is clean and pushed.

### Visualizer — the arc (all landed)
1. **Stage 1** — bounded the force sim so it settles: per-step displacement
   clamp + scale-invariant (per-body) settle test; LOD threshold 1500→700.
2. **Stage 2 engine** — `apps/desktop/src/lib/logic/layout.ts`
   `computeStaticLayout` (closed-form affinity-weighted-centroid + declump).
   Briefly swapped IN as the only layout (commit `f2eba9c`) then **reverted**
   (`7255462`) because the founder wants the live force feel, not a static map.
   The engine still exists and is used as the **seed** for the sim.
3. **Semantic forces** — `graph_neighbors(scope, k=6)` command computes a sparse
   CLIP+note k-NN graph (`PpvecStore::knn_within`, `ppvec.rs:1005`); the sim adds
   a rest-length spring along those edges so alike photos draw together.
4. **Annealing** — the dense k-NN graph is frustrated and churned forever ("big
   blob spinning"); fixed with rest-length springs + a heat-tied clamp that cools
   motion to a floor, so it always settles. `forcegraph.ts` (`ANNEAL_FLOOR`,
   `HEAT_COOL=0.95`, `neighborAttraction`, `neighborRestLength`).
5. **Rebalance + live knobs** — `graph.attraction` 0.02→0.08 (stronger topic
   pull), `graph.neighbor_attraction` 0.03, `graph.neighbor_rest_length` 40, all
   exposed in `GraphTuning` so they are tunable in `tuning.toml` without a
   rebuild (relaunch picks them up).
6. **Topic-strength slider** — live `attraction` control in the graph header
   ("loose ↔ topics"); up = images snap onto topics (unnamed clumps dissolve),
   down = natural clusters re-form.
7. **Overlooked → unnamed clusters ("soft topics")** — unified into the existing
   Overlooked lens rather than a separate feature. `synthesis.ts`
   `unnamedClusters(nodes, topicCount)` detects coherent clumps of images with no
   named topic (union-find over the k-NN edges among low-affinity nodes);
   TopicGraph lists them in the Overlooked readout + glows their members.

### State-integrity / robustness (all landed earlier)
Startup doctor (vector-space reconcile + preview reconcile at launch), offline-
volume **warn + pause** (station shows "Paused — <drive> offline"), WAL redaction
recovery at open/shutdown, schema/generator version guards, **embedder bypass**
(unhostable fp16 CLIP falls back to an installed compatible model), theme/surround
cross-window sync, and the visualizer **self-heal** (recompute affinity when the
embedder finishes loading). Audit doc: `docs/STATE-INTEGRITY-AUDIT.md`.

## Tasks ahead (prioritized)

### 1. Visualizer: dogfood the balance, then soft-topic v2
- **Dogfood first.** The founder should restart `tauri dev`, use the topic-
  strength slider + `tuning.toml` (`[graph] attraction / neighbor_attraction /
  neighbor_rest_length / repulsion`), and report the feel. Tune the defaults from
  there. The architecture is settled; this is numbers.
- **Soft-topic v2 (deferred follow-up).** Detection + readout + glow exist;
  remaining = render **ghost anchors** at each `unnamedClusters` centroid in
  Overlooked mode and **promote-to-topic** on click. Labeling decision (founder):
  **notes first, unlabeled otherwise** — label a cluster by its most
  representative member note phrase when notes exist, else an unlabeled dot.
  Promote needs a name (note-phrase or a new-topic input). Detector:
  `synthesis.ts:367`.

### 2. Removed-folder reconciliation (confirmed bug, robustness)
When a root is removed, its images are orphaned (non-destructive by design) but:
- they **still appear in Library scope** — `Library::image_hashes()`
  (`library/mod.rs:2364`) selects all images with **no active-path filter**;
- they **keep consuming ingest work** — `remove_root` (`library/mod.rs:1026`)
  marks paths stale but does **not** cancel pending passes, so the app re-embeds
  + re-previews deleted folders (founder saw ~414 ghost images doing this).
Fix: filter the Library scope to images with an active path; cancel/skip orphaned
images' pending passes on `remove_root`; have the startup doctor heal already-
orphaned images; invalidate the visualizer affinity cache on a root change (the
view-swap workaround the founder hit). The images themselves stay (relink).

### 3. Self-heal refinements (robustness)
- **Verify-before-retire.** The doctor retires a superseded vector space based on
  the config-named active model (`RuntimeHost::active_vector_models`,
  `runtime.rs:342` → `PpvecStore::reconcile_spaces`, `ppvec.rs:663`). It should
  only retire once that model is actually LOADED and producing vectors — it once
  dropped the dfn5b space because config said fp16 while fp16 wasn't loadable.
- **Skip-already-correct embeddings.** The embedding drain re-embeds on re-pend
  even when a valid vector already exists; it should skip when `inputs_hash`
  matches, to avoid redundant full re-embeds (the 414-image re-embed the founder
  hit after "rebuild all previews").

### 4. fp16 CLIP hosting (backlog, ops)
The fp16 CLIP is a NOMINAL/unhostable manifest entry (the immich-app
`local-fp16-convert` revision 404s). It was regenerated on **margo**, staged
locally, and registered in `installed.json`; the embedder-bypass covers fresh
machines for now. To make it downloadable: host the 3 files (sitting on margo at
`~/fp16-convert/dfn5b-fp16/`), then re-pin the fp16 entry in
`crates/photoproof-core/src/runtime/manifest.rs` with the real repo + revision +
SHAs. SHAs already computed: visual `06554df3…`, textual `8617a89a…`, tokenizer
`6d9109cc…`. margo scratch (~6 GB at `~/fp16-convert/`) can be cleaned or kept
for the upload.

## Operational caveats
- **Restart `tauri dev`** to run the latest binary (visualizer + tuning changes).
- Runtime data lives at `~/Library/Application Support/com.photoproof.desktop/`
  (`photoproof.db`, `models/`, `vectors/`, `previews/`, `logs/photoproof.log`).
- Gate before any commit: `cd apps/desktop && npm run check && npx vitest run &&
  npm run check:emdash`, plus `cargo fmt --all --check && cargo clippy
  --workspace --all-targets && cargo test --workspace`. Known pre-existing
  failure to ignore: `s02_2_case_only_rename_relinks_sidecar`.
