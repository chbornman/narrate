# PhotoProof Architecture Contracts

> Companion to `docs/STATE-MACHINE.md` (the MAP). This is the **target** — the
> seams and disciplines that stop core functions from stepping on each other and
> let us swap pieces (models, views, decoders) as the field moves. Written as a
> design to react to, not a finished spec. Spec still wins; this graduates into
> spec deltas + packets as pieces land.

## Why this exists

The state-machine audit found the root pattern: **5 orthogonal state axes** (view
mode, grid scope, ingest pass, embedder slot, vector space) that keep interacting,
because the layers between them have **no explicit contracts**. Symptoms we hit
this session — the visualizer thrashing on a 1.5s poll, a new photo not cleanly
reaching the graph, a model swap silently missing images, `visual_ready` meaning
something different than anyone assumed — are all the same disease: **implicit
seams**. A magic number buried in logic is the same disease at the smallest
scale: an un-named, un-located coupling you can't tune or swap safely.

Three principles:

1. **Layered seams, explicit contracts.** A layer talks to the next only through
   a named, typed contract. No reaching across (a view polling runtime status to
   guess at data; a number duplicated in three places).
2. **Data-driven views.** Views react to **versioned data-change notifications**,
   never to polls or ad-hoc reactive ticks.
3. **Swappable pieces.** Models/embedders/decoders sit behind traits; one vector
   space per model; swapping is a clean register + re-embed, not a rewrite.
4. **No magic numbers.** Every threshold, interval, limit, dimension, or feel
   value is a **named** constant or a tuning field, in **one** home, with a WHY.

---

## Seam 1 — Library → View data-change contract (the missing connecting tissue)

### Today (the gap)
The library emits `ingest-progress` / `previews-changed` / `roots-changed`; the
**grid** subscribes. The **visualizer does NOT** (`STATE-MACHINE.md §3`). It
refreshes on its own triggers (mount, scope/topic/alpha change) **plus a 1.5s
self-heal poll** that re-fetches affinities and re-runs the layout. There is no
"the data for scope X changed → refresh" signal. Each view invents its own
staleness story (the grid's 2s relist, the visualizer's poll, the coarse
`invalidateScopedGraphs` cache-drop).

> **Interim narrowing landed (`260eeb0`, `9f6de6c`).** The poll's two worst
> failure modes have been patched at the symptom: it no longer tight-loops
> (~45/sec) on a mid-embedding space, and it no longer thrashes a 1.5s beat on a
> Ready-but-empty-scope join — it now polls **only while the embedder state is
> `building`**. This is a band-aid, not the contract: a new photo still doesn't
> reach the graph cleanly (it waits for the next user-driven recompute), and the
> poll still exists. **Seam 1 below retires the poll entirely** and is the
> principled fix. See `STATE-MACHINE.md §6b` for the full post-mortem.

### The contract
A single, typed, **versioned** change notification the library owns and every
view consumes the same way.

```
DataChange  (backend -> all windows, coalesced)
  kind:     ImagesChanged | VectorsChanged | JournalChanged | RootsChanged | CollectionsChanged
  scopeRef: { rootId?, collectionId?, space? }      // what slice changed
  version:  u64                                      // monotonic per (slice)
```

- **Emitter:** the ingest pump / library publishes on the existing bus when it
  commits a meaningful change (images added, vectors written for a space,
  journal mutated). Coalesced on the same cadence as `ingest-progress`.
- **Versions:** a small `DataVersions` snapshot the frontend holds — e.g.
  `{ images: u64, vectors: Map<space, u64>, journal: u64 }` — bumped on commit.
  A view records the version it last rendered against.
- **Subscriber rule:** a view **re-fetches when, and only when, the version of a
  slice it depends on advances** — then debounces (one refresh, not N).
- **The visualizer** drops the 1.5s self-heal poll entirely: it recomputes when
  the `vectors[activeSpace]` version (intersected with its scope) advances, and
  shows a calm "embedding…" state otherwise. No timer, no thrash.

### Migration (incremental, non-breaking)
1. Add `DataVersions` to the `ingest-progress` / `runtime-status` payloads
   (additive) — backend already commits these; just expose the counters.
2. Migrate the **visualizer** first (it has the worst symptom): subscribe to the
   version, delete `retryWhenEmbeddersReady`'s polling.
3. Generalize: grid + inspector move from their bespoke throttles to the same
   version handshake. Retire the ad-hoc `INGEST_RELIST_MS` / cache-drop hacks.

### Decisions to make
- **Granularity:** per-(scope×space) version vs a coarse `{images, vectors,
  journal}` triple. Recommend the coarse triple first (cheap, covers 90%),
  refine to per-space if false-refreshes show up.
- **Transport:** fold versions into the existing events vs a new `data-changed`
  event. Recommend folding in (fewer channels).

---

## Seam 2 — Model / embedder swap contract

### Today
Half-there and good: the `Embedder` trait is the seam (`OrtEmbedder` behind it;
CoreML/CUDA/CPU EP selection internal), and there is **one vector space per
`model_id`** (`vectors.model_id`). Gaps (`STATE-MACHINE.md §5/§6c`):
`repend_passes_for_model` re-pends only **`done`** rows (skipped HEIC /
annotation-less left behind); a swap with an unchanged `model_id` re-embeds
nothing; retire-before-loaded once dropped the live space (fixed:
verify-before-retire).

### The contract
Swapping a model = **register a space + re-embed, retire only when safe**:
- A model is `{ id, role, EP-selection, vector space }`. Selecting it makes its
  space the **active** space for that role.
- **Re-embed contract:** activating a space re-pends **every** image's embed pass
  for that space (not just `done` — include `skipped`-for-transient and missing),
  so no image is silently left in the old space.
- **Retire contract:** the old space is dropped only once the new space is
  `Ready` AND producing vectors (verify-before-retire — already landed).
- **View contract:** views read the **active** space via Seam 1's version; a swap
  is just "active space changed → version bumps → views refresh."

This is exactly your "try and swap pieces as new models release": drop in a
model, it gets a space, the library re-embeds into it cleanly, views follow.

---

## Seam 3 — Constants & config discipline (NO magic numbers)

Every number lives in exactly **one** of two homes, by what KIND of number it is:

| Kind | Home | Examples | Rebuild to change? |
|---|---|---|---|
| **Feel / behavior the founder tunes** | `tuning.toml` (+ `tuning.rs` typed loader, runtime-tunable) | force `repulsion`/`ring_radius`/`anchor_repulsion`, anneal floor, voice VAD timings, heatmap weights, poll cadences | No (relaunch picks up) |
| **Structural invariants / protocol / versions / dims** | a **constants module** per layer (`tuning.rs` consts; a NEW `apps/desktop/src/lib/constants.ts` for the frontend) | `GENERATOR_VERSION`, `EMBED_BATCH`, `RUNTIME_PUMP_TICK`, `READINESS_POLL_MS`, canvas font sizes, the embed dim (read from `model_id`, never hardcoded) | Yes (it's an invariant) |

**The rule:** no bare numeric literal in logic (except `0`/`1`/identity). Every
threshold / interval / limit / dimension / feel value is a named const or a
tuning field, **with a WHY comment**, in exactly one place.

### Current state + the gaps to close (a real sweep)
- ✅ `tuning.toml` exists (`[graph]`/`[voice]`/`[heatmap]`); `forcegraph.ts` has
  `REST_ENERGY_PER_BODY` / `REHEAT_START` / `ANNEAL_FLOOR` consts.
- ❌ **No frontend constants file** — numbers scatter across components/logic.
- ❌ **Duplicated defaults**: `forceConfig()` does `t?.repulsion ?? 800` — the
  `800` shadows the `tuning.toml` value, so a tune can silently disagree with the
  fallback. Defaults must come from ONE place (the typed tuning loader supplies
  them; no inline `?? literal`).
- ❌ **Bare literals in hot paths**: `width=800`/`height=600`, `settleCount > 30`,
  `heat <= 1.0001`, `"10px"/"11px"` canvas fonts, `shadowBlur=12`, `sy - 14`,
  the visualizer's `1.5s` / `40 tries`, the `60`-char label clamp, etc.
- ⚠ **No hardcoded `384`** for the CLIP dim — it's model-dependent; read it from
  the loaded space (`STATE-MACHINE.md §6a` contradiction note).

### Plan
1. Create `apps/desktop/src/lib/constants.ts` (structural) + ensure every tuning
   default flows from `tuning.rs`/the typed loader (kill inline `?? literal`).
2. Sweep the hot paths (TopicGraph, forcegraph, pump, embedding) — move literals
   to a named const or a tuning field; add WHY comments.
3. A lint/gate idea: a check that flags bare multi-digit literals in `logic/` +
   the connectors/library hot paths (allowlist 0/1, indices). Optional, later.

---

## Incremental rollout (so we never refactor blind)

1. **Seam 1 visualizer proof** — `DataVersions` in the ingest event; visualizer
   subscribes, self-heal poll deleted. Fixes the thrash + the "new photo" gap in
   one move, and is the smallest end-to-end demonstration of the contract.
2. **Constants sweep** — `constants.ts` + tuning-default consolidation + the
   hot-path literal sweep. Cheap, high-clarity, unblocks safe tuning/swapping.
3. **Seam 1 generalized** — grid + inspector onto the version handshake; retire
   the bespoke throttles.
4. **Seam 2 re-embed contract** — fix `repend_passes_for_model` to cover all
   rows; formalize active-space switch → version bump.

Each step is independently shippable and gated. The MAP (`STATE-MACHINE.md`) is
the reference; this doc is the destination.
