# PLAN — Seam 1 (data-version) + the sim-interaction state-machine pass

> **✅ LANDED June 17 2026** (`32251af` backend + `b883dd3` frontend; see
> `docs/LANDED.md`). Part A (visualizer) and Part B both shipped. What remains is
> the **generalize** tail — grid + inspector onto the version handshake (needs the
> `images`/`journal` counters) — tracked in `docs/BACKLOG.md`. This doc is kept as
> the scoping record; the acceptance criteria below were met (gate green: clippy +
> svelte-check 0/0 + 1111 vitest + em-dash).

> The principled packet that ends the visualizer whack-a-mole. Companion to
> `docs/ARCHITECTURE-CONTRACTS.md` (the target) and `docs/STATE-MACHINE.md` (the
> map). Scoped against the live tree on `main`; every anchor verified. Spec wins.

## Why now

Three visualizer fixes landed in a row this session (`260eeb0`, `9f6de6c`,
`c8087d9`; `docs/LANDED.md`), all the same class — the interaction/refresh state
machine (heat ↔ settle ↔ poll ↔ recompute) tripping over implicit invariants we
keep discovering by tripping them. The remaining `expandSuper` jitter
(`STATE-MACHINE.md §6b` mech 2) is the next one waiting. Rather than patch it and
wait for the next symptom, this packet replaces the leaky mechanism (a poll) with
a contract (a version) and makes the sim invariants explicit + tested.

Two independently shippable parts. Part A is the bigger architectural win; Part B
is smaller and absorbs the open `expandSuper` fix. Recommended order: **A then
B**, but they don't depend on each other.

---

## Part A — Seam 1: library→view vectors data-version (retire the poll)

### The contract (minimal first cut)
Per `ARCHITECTURE-CONTRACTS.md` Seam 1, start with the **coarse counter**, refine
to per-space only if false-refreshes show up:

```
vectors_version: u64   // monotonic, bumped once per committed vector write
```

A view records the `vectors_version` it last rendered against; it re-fetches
**when, and only when, that version advances** (debounced to one refresh). No
timer. An empty scope-join (§6a — a HEIC folder, images not in the active space)
never bumps the counter, so the visualizer stays calm instead of thrashing.

> **Why coarse, not per-(scope×space):** the chokepoint (below) knows the
> `VecSpace`, so per-space is *available*. But the coarse counter covers ~90% (any
> vector landing while the graph is open is almost always for the active space
> mid-embed), is one `u64`, and can't desync. Refine to `Map<space, u64>` only if
> a multi-space session shows false refreshes. Decision recorded; default coarse.

> **Why in-memory is enough:** the counter only needs monotonicity *within a
> process run* — every view re-fetches on mount anyway, so a restart resetting it
> to 0 is harmless (mount already reads fresh). No schema column, no migration. An
> `AtomicU64` on the store is the whole mechanism.

### Implementation

1. **Add the counter at the commit chokepoint.**
   `PpvecStore::upsert_with_meta` (`crates/photoproof-core/src/retrieval/ppvec.rs:300-453`)
   is the single write gate for *all* vectors (image CLIP via
   `embedding.rs:523`, text chunks via `embedding.rs:318`). Add an
   `AtomicU64` to the store; `fetch_add(1, Relaxed)` at the end of the critical
   section (after :452, on success). Expose `vectors_version(&self) -> u64`.
   *No magic number; the counter is the invariant (Seam 3 discipline).*

2. **Ride it on `ingest-progress`** (the carrier that already fires during the
   embedding drain). Add `vectors_version: u64` to `IngestStatus`
   (`apps/desktop/src-tauri/src/dto.rs:182-210`), populated in `ingest_status()`
   (`pump.rs:168-187`) from the store. The existing `PartialEq` change-detection
   emit gate (`pump.rs:349-361`) then emits whenever the version differs — exactly
   the "vectors changed → notify" signal, free, on the existing 400ms cadence.
   - *Mirror it into the boot/echo path too* so a view mounting mid-session gets a
     baseline (the frontend already fetches an initial `IngestStatus`).

3. **Frontend: hold the version, drop the poll.**
   - `onIngestProgress` (`apps/desktop/src/lib/state/app.svelte.ts:856`) records
     `vectorsVersion` into shell/app state (one field).
   - In `TopicGraph.svelte`: when `vectorsVersion` advances **and** the graph is
     open, schedule a debounced `recompute()` (reuse the existing affinity-cache
     evict + `recompute()` path). A `$effect` keyed on the version is the Svelte-5
     idiom and matches the existing `alpha`/`topicStrength` effects (`:2333/:2347`).
   - **Delete the self-heal poll**: `retryWhenEmbeddersReady`,
     `READINESS_POLL_MS`, `READINESS_MAX_TRIES`, `readinessTries`,
     `readinessTimer`, the `recompute(selfHeal)` budget param
     (`TopicGraph.svelte:522-589, 650, 706, 2284-2285`). The version handshake
     replaces all of it. Keep the calm "embedding…" banner, now driven by embedder
     slot state (`status.clip.state === "building"`) only — no timer behind it.

### Acceptance (Part A)
- Add a folder, open the visualizer mid-embed → graph fills in as vectors land,
  with **zero** `topic_affinities` calls on an empty timer (verify in the log /
  via a counter). Version-driven calls only.
- Open the visualizer on a HEIC-only / un-embedded scope → calm "no signal", **no
  polling**, no beat.
- `grep` confirms `retryWhenEmbeddersReady` and the READINESS_* constants are gone.
- (Stretch, deferred to the Seam 1 *generalize* step) grid + inspector move onto
  the same `vectors_version` / future `images_version` handshake, retiring
  `INGEST_RELIST_MS`. **Out of scope for this packet** — visualizer-only proof first.

---

## Part B — Sim-interaction state-machine pass (make invariants explicit + tested)

### The one load-bearing invariant
**Every node-set re-seed must be paired with `reheat()` before `restartLoop()`.**
A re-seed jumps node positions (and zeroes vx/vy); if heat is still ≈1 from a
prior settled layout, the heat-tied anneal clamp (`forcegraph.ts:494`) pins motion
to the floor → visible jitter / "frozen" feel. The recompute path honors this; the
drag path now honors it (`c8087d9`); **`expandSuper` does not** (the open bug).

### Implementation

1. **Audit every re-seed site for the reheat pairing.** Known node-set rebuilds:
   - `expandSuper` (`TopicGraph.svelte:1856-1863`) — **MISSING reheat** (the bug).
   - the expand/collapse handler `expandSuperNode(...)` at `:2039` → `restartLoop` `:2045` — **verify** it reheats; fix if not.
   - the topic-attraction re-seed at `:761/:765` — **reference (correct):** reheats before restart.
   Add `reheat()` before `restartLoop()` wherever missing.

2. **Make the invariant un-skippable, not just fixed.** Funnel re-seeds through a
   single helper so the pairing can't be forgotten again:
   ```
   function reseedAndRestart(nextNodes) {
     nodes = nextNodes; nodeCount = nodes.length;
     staticDirty = true;   // node set changed → worker mirror must resync
     reheat();             // INVARIANT: a re-seed always reheats (else clamp-frozen jitter)
     restartLoop();
   }
   ```
   Route `expandSuper`, the collapse/expand handler, and the topic re-seed through
   it. One home for the invariant (Seam 3 discipline: the rule lives in one place
   with a WHY).

3. **Tests** (vitest; the sim's pure pieces in `forcegraph.ts` / `layout.ts` are
   already unit-testable):
   - **re-seed reheats:** after a simulated `reseedAndRestart`, `heat` is at the
     reheat start value, not the cooled ≈1 (guards the `expandSuper` regression).
   - **drag holds awake:** `isSettled(energy)` returns `false` while
     `dragging`/`draggingAnchor` is set, regardless of energy (guards `c8087d9`).
   - **recompute debounced:** N version bumps within the debounce window →
     exactly one `recompute()` (guards Part A's refresh path).
   - **poll is gone:** a `building` embedder still surfaces vectors via the version
     handshake; assert no `setTimeout`-based readiness retry remains.

4. **Constants discipline (Seam 3, folded in):** while in `TopicGraph.svelte` /
   `forcegraph.ts`, move the bare literals the audit flagged
   (`width=800`/`height=600`, `settleCount > 30`, `heat <= 1.0001`, the `"10px"`/
   `"11px"` canvas fonts, `shadowBlur=12`, `MOVE_THRESHOLD` already named, the now-
   deleted `1.5s`/`40`) into a frontend `constants.ts` or a `tuning.toml` field per
   their KIND (`ARCHITECTURE-CONTRACTS.md` Seam 3 table), each with a WHY. Keep this
   scoped to the files this packet already touches — not a repo-wide sweep.

### Acceptance (Part B)
- Click a LOD super-node → members separate and **settle smoothly** (no sub-pixel
  jitter / "freeze"). The §6b mech-2 bug is closed; flip it to ✅ in
  `STATE-MACHINE.md` and move the BACKLOG item to `LANDED.md`.
- All four sim-invariant tests pass; `make fmt && make test && make lint` green.
- No new bare literal introduced; touched literals are named with a WHY.

---

## Sequencing & gating
- **A and B are independent** — either can land first. Recommended A→B so the
  version handshake exists before the constants sweep deletes the poll constants.
- Each part is its own commit (or two: backend counter+event, then frontend
  consume+delete-poll). Gate every commit with `make fmt && make test && make lint`
  (`docs/BUILD-LOOP.md`).
- On landing: update `STATE-MACHINE.md §6b` (mech 1 → "poll retired", mech 2 → ✅),
  `ARCHITECTURE-CONTRACTS.md` Seam 1 (Today → done; tick the rollout list), and
  move the two BACKLOG items to `LANDED.md` with hashes.

## Explicitly out of scope (so this stays a packet, not a rewrite)
- Generalizing Seam 1 to grid + inspector (the `images_version` / `journal_version`
  handshake, retiring `INGEST_RELIST_MS`) — a follow-up packet once the visualizer
  proof holds.
- Seam 2 (model/embedder re-embed contract) and the repo-wide constants sweep —
  separate backlog items.
- Per-(scope×space) version granularity — only if coarse shows false refreshes.
