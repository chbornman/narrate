# PhotoProof State Machine — Top-Down Map

> **Why this doc exists.** Stop whack-a-mole debugging. When a bug shows up
> ("thumb won't load", "visualizer froze", "graph says no signal but I have
> vectors"), don't poke the symptom — find the state the system is *actually*
> in and reason forward from this map. Every claim here is anchored to
> `file:line`. Where the source maps disagreed, the contradiction is called out
> in **⚠ CONTRADICTION** callouts rather than smoothed over.

**Audience:** engineers. **Scope:** the whole desktop app — folder → scan →
ingest passes → previews → embeddings → vector spaces → search / visualizer.

---

## 0. One-screen overview — the full pipeline

```
                              USER ADDS A FOLDER
                                     │
            ┌────────────────────────▼─────────────────────────┐
            │  add_root  (commands/library.rs:83-150)           │
            │   • register_root → mint root_id                  │
            │   • spawn 'pp-initial-scan' thread                │
            │   • start filesystem watcher                      │
            │   • emit 'roots-changed' → ALL windows            │
            └───────────┬───────────────────────┬──────────────┘
                        │                        │
              SCAN THREAD                  FRONTEND
        (library/scan.rs:150-498)   onRootsChanged → openFolder(first)
        walk → stat → BLAKE3 hash         (app.svelte.ts:650-677)
                        │                        │
        new_image_tx (mod.rs:1570)        listFolder → setItems → grid
          hash  → DONE                          │  (gray placeholder thumbs)
          exif  → PENDING                       │
          preview → PENDING|SKIPPED             │
                        │                        │
        ┌───────────────▼──────────────────────────────────────┐
        │  INGEST PUMP  (pump.rs:258-377) — loop @ ~500ms idle  │
        │    process_queue(64) ──► exif, preview drain          │
        │    decode_drain(2)   ──► on-demand RAW develop        │
        │    drain_embeddings(8)─► CLIP + text embed @ GPU prio │
        │  emits 'ingest-progress' (400ms coalesced)            │
        │  emits 'previews-changed' (on preview landing)        │
        └───────┬───────────────────────────┬──────────────────┘
                │                            │
       VECTOR STORE (ppvec)          FRONTEND (App.svelte:210-257)
   image_clip vec[≈384/512]      onIngestProgress → throttled refreshItems(2s)
   text vec[≈768] per chunk      onPreviewsChanged → bump ?p= → thumb heals
                │                            │
        ┌───────▼────────────────────────────────────────────┐
        │  EMBEDDER HOST  (embedders.rs) — Slot per role      │
        │    Idle → Building{model} → Ready{Arc<Embedder>}    │
        │                          ↘ Failed{model,msg}        │
        │  converge_* @ 2s, generation gate discards stale    │
        └───────┬─────────────────────────────────────────────┘
                │
        ┌───────▼─────────────────────────────────────────────┐
        │  SEARCH / VISUALIZER (read side)                     │
        │   • search(): lexical (<100ms) ⇄ semantic (Enter)   │
        │   • find_similar(): kNN over image_clip space        │
        │   • topic_affinities(): blend CLIP+text by alpha     │
        │       → visual_ready / annotation_ready flags        │
        │   • TopicGraph force sim: anchors + image nodes      │
        └─────────────────────────────────────────────────────┘
```

**The five axes that matter.** Hold these separate in your head; most "spooky"
bugs are two axes interacting:

| Axis | Lives where | States |
|---|---|---|
| **View mode** | `shell` / `app.svelte.ts` | grid ⇄ visualizer ⇄ look (one orthogonal axis, DESIGN-VIEW-MODES.md) |
| **Grid scope** | `grid.svelte.ts` | folder / collection / query / similar / topic (derived scopes sit *under* any view) |
| **Ingest pass** | `ingest.rs` DB rows | pending / running / done / error / skipped (per image × pass) |
| **Embedder slot** | `embedders.rs` | Idle / Building / Ready / Failed (+ generation gate) |
| **Vector space** | `ppvec.rs` | per (VecKind, model_id) — reconciled at startup |

---

## 1. Frontend: action → effect

> **Diagram:** the user-journey state machine (viewMode × scope × search-lane,
> with capture/escape) is drawn as `diagrams.html` #8 — the visual companion to
> the action→effect tables below. Source for both: `DESIGN-VIEW-MODES.md`.

The dispatch spine is uniform: **every** input path (keystroke, native menu,
chrome click, context-menu row) funnels through `resolveAction(id, ctx, arg?)`
→ `ui.perform(action)`. One sink, zero per-surface verbs.

```
keystroke ─┐
menu item ─┤
chrome btn ─┼─► resolveAction(id, ctx, arg) ─► def.available && def.enabled
ctx-menu  ─┘        (registry.ts)                 │ gates
                                                  ▼
                                          ui.perform(action)
                                          (app.svelte.ts:1981+)
                                                  │
                                       ┌──────────┴──────────┐
                                       ▼                     ▼
                                  state mutation         ipc.invoke(...)
                                  (grid/look/shell/        (commands.ts)
                                   inspector slices)
```

### 1a. Navigation & scope

| User action | IPC / state effect | Anchor |
|---|---|---|
| Rail folder row click | `openFolder()` → `listFolder` → `setItems` | defs/rail.ts; app.svelte.ts |
| Rail rescan row | `ipc.rescanRoot(rootId)` + optimistic `ingestExpecting=true` | commands.ts |
| Grid cell click | `applySelection({order,focus,anchor})` → `setSelection` → `reportScope` | GridSurface.svelte |
| Grid cell dbl-click / Enter | `openLook(hash)` → `navigationSet` → `look.open` | defs/grid.ts; looknav.ts |
| `G` (go home) | leaveVisualizer / leaveLook / clearQueryScope → `reportScope` | defs/global.ts |
| `L` (toggle visualizer) | openVisualizer (seeds `viewSelection` from activeHash) / leaveVisualizer | defs/global.ts |
| `Esc` | 15-layer escape stack router | logic/escape.ts |

### 1b. Search bar (lane machine: none → lexical → semantic)

| User action | Effect | Anchor |
|---|---|---|
| Type a char | `runQueryScope('lexical', transition)` <100ms; `search(...,'lexical',fuzzy)` → `setItems` | logic/searchmode.ts (`nextLane`) |
| Enter | `runQueryScope('semantic')` full hybrid; `search(...,'semantic',{weights,includeDebug})` | search.rs:36-129 |
| Esc #1 (query live) | `clearQueryScope()` → `returnToSource()` → `refreshItems` | escape.ts |
| Esc #2 (focused) | `barFocused=false` | — |
| ⚙ signal checkbox | `setSignal` → if semantic live: re-run `runQueryScope('semantic')` | defs/search.ts |
| Fuzzy `~` toggle | lexical-only; semantic never re-runs (budget rule) | searchmode.ts |

> **Lane invariant:** `weights`+`includeDebug` are **semantic-only**; `fuzzy`
> is **lexical-only**. Every keystroke calls `searcher.interrupt()` — stale
> results are discarded (search.rs).

### 1c. Capture (note / rating / pencil / mic)

| User action | Effect | Anchor |
|---|---|---|
| `N` summon note | `shell.summonNote()` — **scope snapshot at summon time** | NoteInput.svelte; logic/note.ts |
| Note Enter | `note.submit` → `ipc.addNote(text)` → refreshItems → advanceAfter | capture.rs:216-233 |
| Rating key 0-5 | `ipc.setRating(value)` (scope targets, stack-expanded) → advanceAfter | capture.rs:216-233 |
| Pencil pen-up | `ipc.addStroke(hash, payload)` — **always single viewed image** | capture.rs:287-397 |
| Pencil Ctrl+Z | session-rotation check → `ipc.retractEvent(topId)` | look.svelte.ts |
| Space (mic PTT) | two-gesture machine `micDown/micUp/micBlur`; PTT uses idempotent `set_mic` | logic/michold.ts; capture.rs |

### 1d. Visualizer-specific (TopicGraph.svelte)

| User action | Effect | Anchor |
|---|---|---|
| Single-click image node | `selectGraphNode(hash)` → `viewSelection=hash` (SELECTS, not opens) | TopicGraph.svelte:1983 |
| Dbl-click image node | `openFromGraph(hash)` → openLook, `viewSelection=null` | TopicGraph.svelte |
| Click super-node (LOD) | `expandSuper()` → **`reseedAndRestart()` (reheats; jitter fixed `b883dd3`, §6b)** | TopicGraph.svelte |
| Drag node / anchor | `pointermove` writes x/y + **`reheat()`**; drag holds sim awake (§6b) | TopicGraph.svelte:1897-1910 |
| `alpha` slider | `$effect` → **recompute()** (refetch affinities, reheat, restart) | TopicGraph.svelte:2333 |
| `topicStrength` slider | `$effect` → reheat + restartLoop, **NO refetch** (force balance only) | TopicGraph.svelte:2347 |
| Topic anchor click | `selectTopicForBake(idx)` → glow set | TopicGraph.svelte:2296 |
| "Make a collection" bake | `createCollectionFromTopic(phrase,scope,threshold,name,alpha?)` | topics.rs:48-423 |

---

## 2. Backend state machines (ASCII)

### 2a. Ingest pass lifecycle (per image × pass)

Source: `library/ingest.rs:18-462`. **All transitions under the DB write lock.**

```
                    enqueue (scan_root / view-trigger / backfill)
                      INSERT ... ON CONFLICT DO NOTHING
                                     │
                  hash→DONE atomically;  exif/preview→
                                     │
              ┌──────────────────────▼──────────────────────┐
              │                  PENDING                     │◄──────────┐
              │  (claimable iff not_before<=now AND, for     │           │
              │   file-read passes, an ONLINE active path)   │           │
              └──────────────────────┬──────────────────────┘           │
                       claim_next (attempts += 1, started_at := now)     │
                                     ▼                                   │
              ┌──────────────────────────────────────────────┐          │
              │                   RUNNING                     │          │
              └───┬───────────┬────────────┬─────────────┬───┘          │
       mark_done  │   mark_failed          │  mark_failed │ mark_skipped │
                  ▼   (transient,          ▼  (terminal   ▼ (intentional │
              ┌──────┐ attempts<3)     ┌───────┐ OR ≥3) ┌─────────┐      │
              │ DONE │   ─backoff──────│ retry │────────│  ERROR  │      │
              └──────┘  [60s,600s],    └───┬───┘        └────┬────┘  ┌──────────┐
              (model_id    attempts-=1     │ (back to PENDING)│      │ SKIPPED  │
               recorded on  REFUND)        └──────────────────┘      └────┬─────┘
               embed passes)                                              │
                                                                          │
   ── RECOVERY paths (all → PENDING) ──────────────────────────────────────
     • startup crash recovery:  RUNNING → PENDING (mod.rs:195)
     • volume online (probe):   ERROR/PENDING(volume-offline) → PENDING (mod.rs:757)
     • maintenance tick (6h):   ERROR(attempts<10 OR volume-offline) → PENDING
     • re-pend (doctor / generator bump / model swap):
          {DONE,ERROR,SKIPPED} → PENDING, attempts:=0  (running rows left alone)
          priority := MIN(old,new) for pending  (never demote watcher P0)
```

**Offline deferral** (`defer_offline`, ingest.rs:150): file-read pass at offline
volume → PENDING, `not_before := now+600s`, **attempts -= 1 (never counts
against lifetime cap)**. Embedding passes are *unaffected* — they read cached
preview/text, not the original file. This asymmetry is load-bearing and a
footgun (§6).

### 2b. Embedder Slot + generation gate (per role: text / clip)

Source: `embedders.rs:44-413`. Convergence runs every 2s from `state.rs`.

```
                         converge_text / converge_clip  (@2s)
                                     │
          plan=NotConfigured ───────┤────── plan=Run{model}, unknown model
                  │                 │                      │
                  ▼                 │                      ▼
            ┌─────────┐             │               ┌──────────────┐
            │  IDLE   │             │               │ FAILED{model}│ (no auto-retry;
            └─────────┘             │               └──────────────┘  waits for plan
                  │  plan=Run{known}, slot.planned_model() != target    /model change)
                  │  bump gen + spawn build thread (off-lock)
                  ▼
        ┌──────────────────┐   build runs seconds (visual tower ~10s)
        │ BUILDING{model}  │   off-lock, serialized by build_lock
        └────────┬─────────┘
                 │ land_build(): read gen BEFORE and AFTER slot lock
        ┌────────┴───────────────────────────────┐
        │ gen == my_gen ?                         │
        │   YES → overwrite slot (Ready|Failed)   │
        │   NO  → DISCARD ("superseded before     │
        │          land"), never touch slot       │
        └────────┬───────────────────────────────┘
                 ▼
        ┌──────────────────────────────┐
        │ READY{model, Arc<Embedder>}  │  clip_ready()/text_ready() == true
        └──────────────────────────────┘

  generation bumped on: every transition away from current slot, AND shutdown()
  shutdown(): latch stopped=true + bump both gens → apply() becomes no-op
              (prevents a fresh multi-GB build dispatch during quit teardown)
```

> **The slot lands with NO bus event.** `Embedder.build()` completes on a
> background thread; the slot flips Ready/Failed *off-bus*. The **only** thing
> that tells the UI is the runtime pump's idle-tick `readiness_fp` change
> detection (pump.rs:627-643). If that tick wedges, the UI shows "building"
> forever — the "embedder-loading-that-never-finishes" bug. See §6a.

### 2c. Vector-space reconcile (startup doctor)

Source: `ppvec.rs:668-794`, `runtime.rs:342-374`. STATE-INTEGRITY-AUDIT.md.

```
            active_vector_models()  →  {VecKind → model_id}
            (runtime.rs:342 — ONLY embedders that are LOADED/ready==true,
             NOT merely named by plan = "VERIFY-BEFORE-RETIRE")
                                │
                                ▼
            reconcile_spaces(active) fixes 3 silent failures:
            ┌─────────────────────────────────────────────────────────┐
            │ 1. active model file MISSING  → delete rows, repend       │
            │ 2. SUPERSEDED model (old id exists, active id for same    │
            │    kind exists AND is populated) → retire old space       │
            │       GUARD: active_model.is_some() && active_rows > 0     │
            │       (never drop the only copy of the library's vectors) │
            │ 3. ORPHAN .ppvec file (no row points to it) → remove bytes│
            └─────────────────────────────────────────────────────────┘
```

---

## 3. Event plumbing: emitter → cadence → listener → re-render

All backend → frontend events are **snapshot broadcasts to ALL windows**
(`handle.emit`, not window-targeted). Frontend listeners are wired in
`App.svelte:210-257`.

| Event | Emitter / cadence-gating | Frontend listener | What re-renders |
|---|---|---|---|
| `ingest-progress` | pump.rs:258-377; **400ms** PROGRESS_INTERVAL + PartialEq change + `rate_quantum` gate | `onIngestProgress` (app.svelte.ts:501) | clears `ingestExpecting`; throttled `refreshItems` (2s); unthrottled on running→idle edge |
| `runtime-status` | pump.rs:628-694; `recv_timeout(500ms)` coalesce burst + **idle-tick on `readiness_fp` change** | `shell.onRuntimeStatus` (shell.svelte.ts:300+) | Station (download border, blocked reasons), `asrReady` gates mic glyph |
| `previews-changed` | ingest drain + on-demand decode drain (both!) | `grid.onPreviewsChanged` | bump `?p=` query param → thumb replaces 404/placeholder |
| `journal-changed` | every mint (note/rating/stroke), no dedup | `onJournalChanged(hashes)` | look `strokesVersion++`, inspector `load(hash)`, grid `refreshItems` if affected |
| `roots-changed` | add/remove/archive root | `onRootsChanged` | invalidate scoped graphs, applyRootsSnapshot, fallback openFolder(first) |
| `collections-changed` | every collection mutation, serialized by STATIC EMIT_ORDER mutex (collections.rs:68) | `onCollectionsChanged` | replace list, refreshItems if collection view, refreshActiveMemberships |
| `settings-changed` | Settings window; broadcast | `applySettings` | grid `stackDisplay`, re-pair stacks, reportScope |
| `indicator-state` | mic readiness | `shell.onIndicatorState` | mic/streaming/asrUnavailable indicator |
| `indicator-pulse` | per journal mutation, coalesce 200ms | `shell.onPulse` | indicator pulse animation |

> **Coalescing mismatch (by design, kept in sync):** ingest pump emits every
> **400ms** but the shell polls every **2s** (`INGEST_RELIST_MS`). A very fast
> ingest can emit + drain between polls, briefly leaving the grid stale until
> the next scheduled poll. The constants share a source (app.svelte.ts:90).

---

## 4. Worked example: "Add a new folder", end to end

In order — this is the canonical trace to walk when ingest "feels stuck."

1. **Click "Add Folder…"** → `addRootFromPicker()` (app.svelte.ts:683) → OS
   dialog → `ipc.addRoot(path)`.
2. **`add_root`** (library.rs:83): `app.touch()` → `register_root` (overlap
   check, mint root_id) → spawn `pp-initial-scan` thread → `start_watcher` →
   `emit('roots-changed')` to all windows.
3. **`onRootsChanged`** (app.svelte.ts:650): if folder scope and `rootId==null`
   and roots exist → `openFolder(roots[0], '')` → `listFolder` → `setItems`.
   Grid paints with **gray placeholder thumbs** (preview pass still pending).
4. **Scan thread** (scan.rs:150): WalkDir → stat → `known.get(rel)` →
   fast-path / mismatch / unknown. Unknown files → parallel BLAKE3 (batch 64).
   - hash exists → `relink_tx`; hash new → `new_image_tx` (mod.rs:1570):
     INSERT image row; `hash→DONE`; `exif→PENDING`; `preview→PENDING|SKIPPED`
     (HEIC deferred), at `PRIORITY_SCAN`.
5. **Ingest pump** (pump.rs:258) wakes (~500ms idle): `process_queue(64)`
   claims exif/preview pending → running → done. On preview landing, pump
   collects hashes → `emit('previews-changed')`.
6. **`onPreviewsChanged`** → bump `?p=` → thumbnail heals from
   `photoproof://` cache URL. **`onIngestProgress`** (every 400ms) →
   throttled `refreshItems` (2s) → grid fills in.
7. **Embedding drain** (pump.rs:433): once `process_queue==0` and mic
   disarmed → `enqueue_embedding_backfill` (every image, ON CONFLICT DO
   NOTHING) → `process_embedding_queue`:
   - `ImageEmbedding`: load bytes → `encode_image` → CLIP vec → `vectors.write(hash, ImageClip)`.
   - `TextEmbedding`: fold events → chunk → `encode_text` → text vec per chunk.
   - At `PRIORITY_GPU`. New vectors are immediately queryable by
     `find_similar` / `topic_affinities`.
8. **running → idle edge**: `onIngestProgress(running=false)` →
   **unthrottled** `refreshItems` for the final accurate count.
9. **Open visualizer** (`L`): TopicGraph mounts → `topic_affinities(scope,
   topics, alpha)` (graph.rs:110) → `enumerate_scope` → score each image ×
   topic → `AffinityReport{visual_ready, annotation_ready}`. Module caches
   (affinity / neighbor / thumbs / graphState) survive unmount.

---

## 5. The model-swap / vector-space migration path (read before touching models)

```
config changes CLIP fp32 → fp16
        │
        ▼
plan() recomputes (runtime.rs:274)        embedders.converge_clip (@2s)
        │                                      bumps gen, builds fp16 slot
        ▼                                      │
repend_passes_for_model(ImageEmbedding,   land_build → Ready{fp16}  (or Failed)
  new_model_id)  (ingest.rs:385)               │
  DONE rows where model_id != fp16 → PENDING   ▼
  attempts:=0                            active_vector_models now reports fp16
        │                                  IFF clip_ready()==true (LOADED, not named)
        ▼                                      │
process_embedding_queue re-embeds @ fp16       ▼
  vectors.write(hash, ImageClip, fp16)   reconcile_spaces retires fp32 space
                                           ONLY IF fp16 space populated (rows>0)
```

**Migration gotchas (all real, all in the maps):**

- `repend_passes_for_model` only re-pends **DONE** rows. **SKIPPED** rows
  (preview-deferred HEIC, annotation-less images) are left alone → a model
  swap silently misses them until the skipped pass re-runs. No auto-recovery
  (embedders map footgun; ingest map footgun).
- **Model swap with no model_id change** (e.g. weights file replaced but id
  reused) leaves DONE rows untouched → no re-embed. User must `rebuild_index`
  / rescan. (Add-folder map footgun.)
- **`active_vector_models` reports LOADED, not NAMED.** If config names fp16
  but fp16 *fails to build*, fp32 stays live and is never retired (the
  superseded guard needs an fp16 space to exist first). Misnamed "active" is
  the footgun. (embedders map.)
- **inputs_hash includes GENERATOR_VERSION.** A preview-generator bump with no
  CLIP change still re-embeds (pixels identical) because the staleness hash
  shifts. Wasted GPU. (ingest map.)
- **Half-finished re-embed never retires the old space** (guard wants
  `active_rows>0`, but if active has 1 row and superseded has 10k, old space
  lingers; disk grows until compaction + manual action). (embedders map.)

---

## 6. FOOTGUNS — consolidated

The three the prompt specifically asked about are **6a, 6b, 6c**. The rest are
grouped by subsystem.

### 6a. ⭐ Why `visual_ready=false` while embedder is Ready AND vectors exist

This is the headline confusion. **All three can be simultaneously true:**
CLIP slot = `Ready`, 158 `image_clip` vectors on disk, **and**
`visual_ready=false`.

`topic_affinities` computes readiness as *"did ANY topic emit a non-empty score
map?"*:

```
topic.rs:175-181
  let mut visual_ready = false;
  for each topic:
      (visual, annotation) = score_topic(topic, scope, vectors, text, clip);
      visual_ready |= !visual.is_empty();   // ← line 181
```

`score_topic` → `embed_then_score` → `vectors.score_images(query, ImageClip
space, scope_hashes)` (ppvec.rs:816). `score_images` does a **critical join**:

```
score_images(query, VecSpace{ImageClip, model_id}, scope_hashes):
  1. read all vectors for (ImageClip, model_id, deleted=0)
  2. FILTER to image_hash IN (scope_hashes)        ← the join that can empty out
  3. return HashMap{hash → cosine}                 → empty if intersection empty
```

So `visual_ready` is true **only if** `clip_ready()` **AND** the stored
vectors' `model_id` matches the active model **AND** at least one stored vector
belongs to **this scope's** image set. If the scope's hashes and the vector
table's hashes have **empty intersection** (different library, archived subset,
a folder whose images were never embedded), `score_images` returns empty,
`visual_ready` stays false — even though CLIP is Ready and 158 vectors exist
*for other images*.

**Semantically correct** ("no visual signal for *this scope*"), but it presents
as "ready embedder + vectors exist, yet visual_ready=false." When debugging:
check the **scope × stored-hash intersection**, not just embedder readiness or
total vector count. (embedders map; topic.rs:161-213; ppvec.rs:816-899.)

> **⚠ CONTRADICTION (vector dim).** The maps disagree on the CLIP vector
> length: the embedder/vector-space map and find_similar trace say `fp32[384]`;
> the add-folder map's vector-store note also says `vector[384]`; but the
> Look-zoom/embedding trace elsewhere implies a CLIP image tower of different
> width and text `[768]`. The dim is model-dependent (DFN5B vs others). **Do
> not hardcode 384** — read it from the loaded space's `model_id`. The
> readiness logic above does not depend on the dim, only on the join.

### 6b. ⭐ Visualizer self-heal poll + the "click → everything moves → freezes" bug

> **STATUS (updated June 17 2026 — all three RESOLVED).** This whole section is
> the interaction/refresh state machine the doc was written to systematize; the
> three mechanisms below were the symptoms, and all three are now fixed at the
> root. Kept here as the post-mortem map (the code anchors are historical):
> - ✅ **Self-heal poll — RETIRED** (`32251af` + `b883dd3`, Seam 1). The whole
>   poll (`retryWhenEmbeddersReady` + `READINESS_*`) is **deleted**. The
>   visualizer now refreshes when the vector store's `vectorsVersion` advances
>   (rides `ingest-progress`), so there is no timer to thrash: no data advance →
>   no work; a ready-but-empty scope stays calm. Interim narrowings `260eeb0` /
>   `9f6de6c` are superseded by this. (`ARCHITECTURE-CONTRACTS.md` Seam 1.)
> - ✅ **Drag freeze — FIXED** (`c8087d9`). Heat cooled mid-drag, `isSettled`
>   tripped, the loop stopped. Now a drag is never "at rest" and `pointermove`
>   reheats. The rest predicate is now the pure, unit-tested `isAtRest`
>   (`forcegraph.ts`); `b883dd3` extracted it.
> - ✅ **`expandSuper` re-seed jitter — FIXED** (`b883dd3`). Every node-set
>   re-seed now funnels through `reseedAndRestart()`, which **always reheats**
>   before `restartLoop` (the same gap in both `applyLodZoomTransition` branches
>   was closed too). Mechanism 2 below is the post-mortem.

**Three coupled mechanisms** (all now resolved — historical detail follows).

**(1) Affinity self-heal poll** (was `retryWhenEmbeddersReady`).
**[RETIRED — `32251af` + `b883dd3`, Seam 1.]** The poll is gone entirely. For the
record, its failure arc: the original code recomputed *immediately* whenever
`clipReady` was true → ~45/sec tight loop over a mid-embedding space (`260eeb0`);
the interim fix treated any non-idle/non-failed state as "coming", so a Ready
embedder over an empty **scope-join** (§6a — a HEIC folder, images not in the
active space) beat `recompute()` on a 1.5s timer for its full 60s budget every
visit (`9f6de6c`); narrowing it to `building`-only helped but was still a timer.
The data-version contract removes the mechanism: the store bumps a monotonic
`vectorsVersion` on every committed write, it rides `ingest-progress`, and the
visualizer re-fetches **only when that version advances past the one it rendered
against** while a half it needs is still missing (throttled). Empty scope → no
write for it → no advance → no work. This is the Seam 1 proof.

**(2) The click-jitter / re-seed bug. [FIXED — `b883dd3`.]** Clicking a **LOD
super-node** called `expandSuper()`, which historically did
(TopicGraph.svelte, pre-fix):

```
expandSuper(node):            // PRE-FIX (b883dd3) — the bug
  expandSuperNode()   // rebuild nodes[] : members spiral-seeded around
                      // the super-node's CURRENT (x,y); vx/vy zeroed
  staticDirty = true
  restartLoop()       // ← NO reheat() — the jitter cause
```

At that moment **heat was still ≈1.0** (steady state from the prior settled
layout). The annealing clamp is **heat-tied** (now the pure `annealedMaxStep`,
`forcegraph.ts`: `anneal = clamp01((heat-1)/(REHEAT_START-1))`), so at heat≈1 the
per-step displacement clamp is pinned at `ANNEAL_FLOOR (0.5px)`. The
freshly-separated members felt **large mutual repulsion** (they just jumped
apart) but the clamp damped it to sub-pixel steps — visible *jitter* over ~10-30
frames as the layout oozed apart. User saw: **"click an image → everything moves
a bit → freezes."**

**The fix (`b883dd3`):** every node-set re-seed now funnels through
`reseedAndRestart()`, which **always `reheat()`s before `restartLoop()`** — so the
displaced members settle under hot forces instead of crawling at the floor.
`expandSuper` and **both** `applyLodZoomTransition` branches (expand + collapse,
which had the same gap) route through it. The premise is unit-tested:
`annealedMaxStep(1, …)` returns the floor (cooled = crawl) and
`annealedMaxStep(REHEAT_START, …)` returns the full step (reheat = free) — see
`forcegraph-restseed.test.ts`. The invariant now lives in **one** helper so it
can't be forgotten on the next re-seed site.

> **✅ CONTRADICTION RESOLVED (jitter mechanism).** Two earlier source maps
> disagreed on the cause: *clamp-frozen* repulsion (heat≈1 → floor clamp →
> sub-perceptual but visible jitter, **too-cold**) vs restart "with ACTIVE heat"
> leaving "large velocity vectors" (**too-hot**). The **too-cold / clamp-frozen**
> reading was confirmed: the `c8087d9` drag-freeze fix demonstrated the exact
> same heat-cooling dynamic on `isSettled`, `vx/vy` are zeroed on re-seed (no
> inherited velocity, so "too-hot" was never possible), and the fix
> (reheat-before-restart) resolved the jitter in practice.

**Related visualizer footguns:**

- **graphState per-scope persistence** can land a layout from a *prior* scope
  if the key collides (open collection A, close, reopen A reuses old layout
  even if images changed via ingest in between). (Frontend + add-folder maps.)
- **`topicStrength` slider snaps** if the backend tuning file changed after
  mount and was never reloaded — slider jumps from stale default to new value
  with no interpolation. (Frontend map.)
- **`selectedTopic` off-by-one**: it's an array index into reverse-sorted
  topics. Remove an *earlier* topic and later indices shift down; a `$effect`
  clears a now-invalid index *next frame*, but the in-flight frame glows the
  wrong topic. (Frontend map.)
- **Expanded members have no neighbor edges** (`expandSuperNode` sets no
  neighbors) → they scatter under repulsion alone until next recompute; feels
  less cohesive than a full-detail view. (Map 6.)
- **Unnamed-cluster ghosts vanish in LOD** (`unnamedClusters()` only runs
  `lodActive=false`) → Overlooked mode shows no soft topics on large
  aggregated libraries. (Map 6.)
- **Module caches never evict** (affinity/neighbor/graphState are module-level)
  → unbounded RAM across many scope/topic combos in a long session. (Map 6.)

### 6c. Ingest / volume / model-swap gotchas

- **Volume-offline asymmetry**: file-read passes idle at the SQL claim gate;
  embedding passes keep draining (cached artifacts). A flapping volume can park
  a pass in perpetual defer/re-pend **bypassing `MAX_LIFETIME_ATTEMPTS`**
  (offline always refunds attempts). By design, but invisible to the user.
- **Generator-version downgrade strands files**: an older app re-pends newer
  v3 rows as stale but never outputs v3; v3 files orphan on disk until a
  current-version develop for that hash sweeps them.
- **`mark_done` running-guard vs re-pend race**: serialized by SQLite single
  writer; events-engine re-pend lands first so `mark_done` sees PENDING and
  no-ops. Don't reorder.
- **HEIC decode failure**: preview ends in **error** (not skipped) →
  image-embedding stays **pending forever** because the skip-detect only checks
  the `skipped` state. (ingest map.)
- **Cancel granularity**: cancel checked at drain entry + per-item, but a
  100-item bounded channel can finish all 100 after cancel before the next
  claim re-checks the flag. (ingest map.)

### 6d. Capture / session / scope footguns

- **Session rotation is LAZY** (`app.touch()`): rotates at *next activity*, not
  at the 30m boundary. A note/stroke committed mid-rotation can bind to the
  **new** session. Pencil undo stack is **cleared on rotation** → after 30m
  idle, Ctrl+Z is a silent no-op until a new stroke lands. (Capture maps.)
- **Note/rating scope snapshot at keystroke time**: change the selection while
  composing → targets reflect the **old** selection. Panel composer uses an
  explicit hash to avoid this. (Capture maps.)
- **Stroke is ALWAYS the viewed image**, never the scope ring — confusing when
  composing a multi-select note and drawing at the same time. (Capture maps.)
- **`toggleMic` flips blindly** → can re-arm after a device-failure disarm.
  PTT always uses idempotent `set_mic(false)`; the M-key blind toggle is the
  hazard. (IPC map.)

### 6e. Events / plumbing footguns

- **Embedder silent landing** (the §6a/§2b twin): slot lands with no bus event;
  the runtime pump idle-tick `readiness_fp` is the only catcher. Wedged tick →
  UI stuck on "building." (Events map; pump.rs:638-643.)
- **Runtime initial-state dark**: a boot error in `onRuntimeStatus` is silently
  swallowed; if no bus event ever fires (supervisors wedged / plan
  NotConfigured), runtime stays null and the UI is dark with **no indication
  why** (the "silent-dark" incident). (Events map; supervisors.rs:53-62.)
- **`ingestExpecting` strands on silent failure**: set on add_root/rescan
  click, cleared on first ingest-progress event. If a rescan instantly fails
  (path deleted) no event ever arrives → empty grid shows "Indexing" forever
  until refresh/restart. (Frontend + events maps.)
- **Settings/roots/collections broadcast to ALL windows** with no source-window
  tag → a window can't tell it caused its own re-render. (Events map.)
- **DownloadProgress is MODEL-cumulative**, not per-file — apply per-file
  semantics and a 400-file manifest shows ~0%. (Events map.)
- **EMIT_ORDER static mutex** serializes ALL collection mutations globally →
  throughput O(1) under concurrency. (IPC map.)

### 6f. Grid / scope race footguns

- **`gridLoad` monotone token**: only the latest `setItems` feeder wins; two
  async scopes landing out-of-order briefly show wrong items before the latest
  load corrects. (Frontend map.)
- **Query-below-threshold returns to source without clearing the bar text** →
  bar input desyncs from grid scope until commit or explicit clear. (Frontend.)
- **`graphScope()` falls back to `{kind:'library'}`** if the source folder was
  removed → visualizer silently computes over the **whole library** instead of
  erroring. (Add-folder map.)
- **Heat-tint signature** (length + endpoints) misses a **middle removal** → a
  removed image's intensity stays stale in the map (kept items still render
  honestly). (Frontend map.)

---

## 7. Where to look first (debug index)

| Symptom | Most likely state | Start here |
|---|---|---|
| Graph "no signal" but vectors exist | scope × stored-hash empty join (§6a) | topic.rs:181, ppvec.rs:816 |
| Visualizer stuck "loading" | embedder slot `building` (real wait); refresh is now `vectorsVersion`-driven, no poll (§6a/§6b, Seam 1) | embedders.rs:367, TopicGraph.svelte (`vectorsVersion` $effect) |
| Click super-node → jitter (FIXED `b883dd3`) | was: expandSuper re-seed without reheat (§6b mech 2); now `reseedAndRestart` | TopicGraph.svelte, forcegraph.ts `annealedMaxStep` |
| Click+drag node → freeze (FIXED `c8087d9`) | was: `isSettled` cooled mid-drag, loop stopped | TopicGraph.svelte:1010, 1902 |
| Grid stuck "Indexing" | `ingestExpecting` stranded on silent rescan failure | app.svelte.ts:501; pump.rs:258 |
| UI dark, no models | runtime never emitted (silent-dark) | pump.rs:628; supervisors.rs:53 |
| Thumbs stale after cache clear | missing `previews-changed` emit | app.rs (clear_preview_cache) |
| Model swap didn't re-embed | model_id unchanged OR rows were SKIPPED | ingest.rs:385 |
| Wrong items flash in grid | `gridLoad` stale-load race | grid.svelte.ts |

---

*Sources: 7 subsystem maps (frontend actions, IPC surface, event plumbing,
ingest lifecycle, embedder/vector-space, TopicGraph visualizer, add-folder
trace). Anchors verified against the live tree on the `main` branch. Spec wins
over this doc; this doc wins over intuition.*
