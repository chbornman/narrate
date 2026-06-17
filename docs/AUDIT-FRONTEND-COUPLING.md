# Frontend Coupling Audit — implicit-seam / staleness bug class

> **Why this doc exists.** The visualizer just shed three bugs of one shape:
> state axes interacting through un-named couplings, ad-hoc staleness/refresh
> polls, and scope snapshots taken at the wrong moment (`STATE-MACHINE.md §6b`,
> `ARCHITECTURE-CONTRACTS.md` Seam 1). This is a top-down sweep of the rest of
> the desktop frontend for the SAME disease. Every finding is anchored to
> `file:line` against the live `main` tree, marked **CONFIRMED** (traced) or
> **SUSPECTED** (needs a runtime check), with the seam the fix should restore.
> ⚠ marks a contradiction with the map or a place I am uncertain.
>
> **Read first:** `STATE-MACHINE.md §1/§3/§6d/§6f`, `ARCHITECTURE-CONTRACTS.md`
> Seam 1 + Seam 3. **Scope:** READ-ONLY audit; no code touched.

**The five axes** (most bugs are two interacting):
`viewMode` (grid/visualizer/look) × `gridScope` (folder/collection/query/similar/topic)
× `searchLane` (none→lexical→semantic) × `capture` (note/rating/pencil/mic) × the escape stack.

**Verdict up front.** The dispatch spine (`resolveAction → ui.perform`), the
escape ladder, the lane machine, and the note snapshot machine are clean and
well-sealed. The coupling debt is concentrated in **three places**: (1) the
`vectorsVersion` data-version contract is wired to the visualizer ONLY — the
grid and inspector still run bespoke throttles and membership-test refreshes
(the explicit "still open" of Seam 1); (2) the visualizer's module caches key on
`scope×topics` but NOT on `alpha`/`fullLibrary`/data-version, so a stale layout
can land on reopen; (3) the `ingestExpecting` optimistic flag can strand on a
silent ingest no-op. None are crashes; all are the visualizer bug class one ring
out.

---

## A. Library → View refresh contract (Seam 1, the connecting tissue)

### A1. Grid + inspector never got the `vectorsVersion` handshake — they still invent staleness  ⚠ KNOWN-OPEN
**Couples:** `gridScope`/`ingest pass` ↔ the grid's refresh, through an **ad-hoc
2s wall-clock throttle** and a **journal membership test**, not through a
versioned data-change notification.

**Confirmed.** `vectorsVersion` is consumed in exactly ONE place — the
visualizer (`TopicGraph.svelte:2386` `const v = ui.shell.ingest.vectorsVersion`,
recorded at `:678`). The DTO carries it (`types/dto.ts:149-150`) and the shell
holds it (`shell.svelte.ts:159`). The grid's refresh is still the bespoke
machinery the contract says to retire:
- `onIngestProgress` (`app.svelte.ts:856-873`) re-lists on a **2s wall-clock
  throttle** (`lastIngestRefresh`, `INGEST_RELIST_MS`, `:867`) plus an
  unthrottled running→idle edge (`:870`), AND
- `App.svelte:261-268` runs a **second** `setInterval(INGEST_RELIST_MS)` that
  *also* calls `refreshItems()` while `ingest.running` — two independent timers
  driving the same relist (the doc's "each view invents its own staleness
  story"), AND
- `onJournalChanged` (`app.svelte.ts:1781`) relists if `grid.items.some(affected)`.

**How a user trips it.** `STATE-MACHINE.md §3` already names the coalescing
mismatch: a very fast ingest emits + drains between the 2s polls, leaving the
grid stale until the next scheduled poll. The visualizer no longer has this
because it rides the version; the grid still does.

**Severity:** med. **Fix direction:** `ARCHITECTURE-CONTRACTS.md` Seam 1
migration step 3 — add `images`/`journal` counters next to `vectors`, move the
grid + inspector onto the version handshake, delete the `App.svelte` interval
and the `lastIngestRefresh` throttle. One refresh policy, versioned, not two
timers + a membership test.

### A2. `ingestExpecting` strands on a silent ingest no-op  — CONFIRMED, the §6e footgun, verified
**Couples:** the `capture/add-root` optimistic flag ↔ the `ingest pass` event
stream, with **no monotone token and no timeout** — a classic optimistic flag
that can strand.

**Confirmed.** Set in three places — `addRootFromPicker` (`:696`), `confirmDrop`
(`:1500`), and `rescan-root` (`:2277`) — and cleared on the FIRST
`ingest-progress` event (`onIngestProgress:861`, unconditional, before the
`rootId===null` guard at `:864`). The clear depends on an event *arriving*. The
catch blocks only fire on an **IPC-level** throw (`:701`, `:1516`, `:2282`).

**How a user trips it.** Rescan a root whose path was deleted out from under the
app, or whose scan finds zero changes and emits no `ingest-progress`:
`ipc.rescanRoot` returns Ok (no throw → `:2282` catch never runs), but no status
event ever lands → `ingestExpecting` stays `true` → an empty grid reads
"Indexing - photographs appear as they are found." (`App.svelte:347`) forever,
until an unrelated refresh or restart. `shell.svelte.ts:161-170` claims "cleared
by the FIRST real status event … a stale flag cannot strand" — true only if an
event is guaranteed to follow, which the silent-no-op case violates. ⚠ The
slice comment and `STATE-MACHINE.md §6e` disagree on whether this can strand;
the code confirms §6e is right.

**Severity:** med. **Fix direction:** give the optimistic flag an owner with a
deadline — clear it on the same versioned `ImagesChanged`/`scanning` signal it
is bridging to, with a watchdog timeout that stands it down if no scan signal
arrives within N seconds (a named constant, Seam 3). Same shape as the
visualizer's "calm embedding… state otherwise."

---

## B. Visualizer (verify the fixes + the caches the map flagged)

### B0. The three fixed bugs — VERIFIED LANDED
- **Self-heal poll retired.** No `retryWhenEmbeddersReady` / `READINESS_*` /
  affinity `setInterval` survives. Refresh is the `vectorsVersion` `$effect`
  (`TopicGraph.svelte:2384-2393`), guarded `if (v === lastVectorsVersion) return`
  and `if (visualReady && annotationReady) return` (only re-fetch when a half is
  still missing), throttled by `VECTOR_REFRESH_THROTTLE_MS` (`:530`), cleared on
  unmount. This is the Seam 1 proof and it holds. CONFIRMED.
- **`reseedAndRestart` reheat-before-restart.** `expandSuper` (`:1843`) and
  **both** `applyLodZoomTransition` branches funnel through `reseedAndRestart`
  which always `reheat()` then `restartLoop()`. CONFIRMED.
- **Drag reheat.** `pointermove` reheats; sim held awake mid-drag. CONFIRMED.

### B1. Module caches key on `scope×topics` but omit `alpha`, `fullLibrary`, and the data-version → stale layout on reopen  — SUSPECTED
**Couples:** `gridScope`/the `fullLibrary` toggle/`alpha` ↔ the persisted graph
layout, through a cache key that **does not name** alpha or fullLibrary — a
scope snapshot keyed at the wrong granularity (the §6b "graphState per-scope
persistence can land a layout from a prior scope" footgun, now anchored).

**Suspected (needs a runtime check).** `graphStateKey(scope, topics)`
(`logic/graphstore.ts`) and the affinity key
(`logic/affinitycache.ts` `affinityKey(topics, scope, alpha)`) differ: the
affinity cache **does** include alpha (`a=${alpha.toFixed(3)}`) but the graph
**state/layout** snapshot key is `scopeKey|topics` with **no alpha and no
fullLibrary**. `scope()` (`TopicGraph.svelte:476`) returns `{kind:'library'}`
whenever `fullLibrary` is on, so:
- a folder view and a library view at the same topics produce DIFFERENT scope
  keys (library vs folder) — OK; but
- closing and reopening the SAME scope+topics at a **different alpha** restores
  the saved layout (stored alpha ignored by the key), and
- neither key carries `vectorsVersion`, so a layout snapshotted while a scope
  was mid-embed is restored verbatim even though new vectors landed in between
  (the cache has no generation tracking — the exact shape of the poll bug, just
  on the persistence side instead of the refresh side).

**How a user trips it.** Open scope A, let it settle, leave; ingest adds images
to A; reopen A with the same topics — the old layout (missing the new nodes)
restores from cache; only the `vectorsVersion` effect's *missing-half* guard
re-fetches, and if the scope already had a non-empty visual half it reads
`visualReady && annotationReady → return` and never refreshes.

**Severity:** med. **Fix direction:** fold the rendered `vectorsVersion` (and
alpha/fullLibrary if they affect layout) into the snapshot key, OR validate a
restored snapshot's node-set against the live affinity hash set before trusting
it. This is Seam 1 applied to the cache layer: a snapshot is only valid for the
data-version it was built against.

### B2. Neighbor cache keyed on `scopeKey` only — index validity unguarded across scopes  — SUSPECTED
**Couples:** `gridScope` ↔ the k-NN neighbor edge indices (array offsets into
`nodes`), through a cache keyed on scope but with no check that the cached
graph's indices match the current node-set.

**Suspected.** `neighborCache.set(scopeKey(sc), graph)` keys only on scope; the
neighbor entries are offsets into the node array. If a scope's image set changed
(ingest added/removed images) but the scope key is identical, restored neighbor
indices can point past the new `nodes` array. In practice the affinity
recompute usually rebuilds nodes together, so this may be latent; flagged for a
runtime check on the rapid open→ingest→reopen-same-scope path.

**Severity:** low. **Fix direction:** same as B1 — key (or validate) the
neighbor cache against the data-version / node-set it was built for.

### B3. `selectedTopic` off-by-one on a mid-array topic removal  — CONFIRMED (the §6b footgun, anchored)
**Couples:** the bake panel's `selectedTopic` index ↔ the `topics` array order;
the guard only catches **out-of-bounds**, not a **shift**.

**Confirmed.** `topics = ui.topics.map(t=>t.phrase).reverse()` — `selectedTopic`
is an index into that reversed array. The clamp `$effect` only fires when
`selectedTopic >= topics.length`. Remove an EARLIER topic and later indices
shift down by one while still in-bounds: the panel keeps `selectedTopic` but it
now names a different topic (`bakeName`, the glow, the baked phrase all read the
wrong row for the in-flight frame). This is exactly the §6b "selectedTopic
off-by-one" footgun, now anchored.

**Severity:** low. **Fix direction:** track the selected topic by **phrase/id**,
not by array index — then a reorder/removal can't silently retarget it. (The
same "identity not index" discipline `grid.fixupSelection` already uses for
focus by hash.)

### B4. `graphScope()` falls back to `{kind:'library'}` for a bare query/similar scope  — CONFIRMED (the §6f footgun, anchored)
**Couples:** `gridScope=query|similar` ↔ the visualizer scope, silently widening
to the WHOLE library instead of erroring or scoping to the result set.

**Confirmed.** `graphScope()` (`app.svelte.ts:1328-1336`) returns
`{kind:'library'}` when the source resolves to neither collection nor folder.
Open the visualizer from a query/similar scope whose `within` doesn't resolve to
a folder/collection and the graph computes over the entire library — the
deliberate scale spike happening by accident. ⚠ The agent thought the type
invariant makes this dead code; the `within` is folder-or-collection by type,
but a query over a removed folder (the §6f "source folder removed" case) reaches
the fallback. Worth a runtime confirm of which scopes actually hit `:1335`.

**Severity:** low-med. **Fix direction:** make the query/similar→graph transition
explicit — scope to the result hash-set, or refuse with a calm message, never
silently fall through to library.

### B5. Magic numbers in the hot path (Seam 3 debt)  — CONFIRMED
Quick-press window `250ms` hardcoded twice (`TopicGraph.svelte:1930, 1958`);
`DBLCLICK_MS`/`FIELD_THROTTLE_MS`/`LOD_*` are named but canvas geometry/font
literals are bare. `ARCHITECTURE-CONTRACTS.md` Seam 3 already owns this sweep
(`constants.ts` + tuning consolidation). **Severity:** low. Listed for
completeness; it is the smallest-scale version of the same disease.

---

## C. Capture / scope-snapshot (the §6d axis)

### C0. Note + mic snapshot machines — VERIFIED CLEAN
The two scope-snapshot machines are correct. `logic/note.ts` snapshots scope at
**summon** (`summon`) and `onScopeChanged` **cancels** the open note on any scope
change (`shell.svelte.ts:301-314`), so summon-time ≡ submit-time by
construction. The mic machine (`logic/michold.ts`) resolves to explicit
`arm`/`disarm` intents, never a blind toggle, and `micBlur`/`micUp` always
return to idle so a hold can't wedge. CONFIRMED — these are the *good* pattern.

### C1. Pencil undo stack rides lazy session rotation — two clearers, one race  — SUSPECTED (the §6d footgun)
**Couples:** the `capture` pencil-undo stack ↔ the **lazy** session-rotation
boundary, cleared from two independent paths.

**Suspected.** The undo stack clears on session change via TWO routes:
`onStrokeCommitted` (`look.svelte.ts:196-203`, clears when a stroke lands in a
new `sessionId`) and `syncUndoSession` (`:216-221`, clears when
`reportActivity`'s echoed session id differs). Session rotation is **lazy**
(`app.touch()` at next activity, `STATE-MACHINE.md §6d`), and the activity echo
is throttled to once/minute (`App.svelte:193 ACTIVITY_REPORT_THROTTLE_MS`).
After 30m idle, the first `Ctrl+Z` (before any activity touch fires) operates on
a stack belonging to a session the backend already rotated away from →
`retractEvent(topId)` targets a stale event id. By-design per the map, but the
two-clearer split + the 60s throttle widen the stale window.

**Severity:** low (matches §6d). **Fix direction:** make session-rotation a
versioned signal the look slice subscribes to once (single clearer), rather than
two opportunistic clears racing a throttled activity echo.

### C2. `reportScope` reloads the inspector against a possibly-moved `activeHash` mid-await  — SUSPECTED
**Couples:** focus moves ↔ the inspector load, through `activeHash` read AFTER an
`await ipc.setScope`.

**Suspected.** `reportScope` (`app.svelte.ts:564-577`) awaits `setScope`, then
reads `this.activeHash` and calls `inspector.load(active)`. If focus moved during
the await, `active` is the new focus. This is **mostly sealed** by the
inspector's own `#loadSeq` monotone guard (`inspector.svelte.ts:46,63,74,79`) and
the `refreshActiveMemberships` `membershipsHash` guard (`app.svelte.ts:609-621`)
— both well-built. The residual risk is multiple `reportScope` calls in flight
each loading a different hash; the `#loadSeq` guard makes only the last land, so
this is likely benign. Flagged so a reviewer confirms the guard covers every
`reportScope` caller.

**Severity:** low. **Fix direction:** none needed if the `#loadSeq` guard is the
single sink (it appears to be); document it as the seam.

---

## D. Search lane (the §1b / §6f axis)

### D0. Lane machine + gridLoad fence — VERIFIED CLEAN
`logic/searchmode.ts` `nextLane` is pure/total; `runQueryScope`
(`app.svelte.ts:1009-1083`) keeps `weights`/`includeDebug` semantic-only
(`:1050-1058`), `fuzzy` lexical-only (`:1063`), and fences BOTH awaits
(`ipc.search` `:1066`, `ipc.listImages` `:1080`) with the `gridLoad` monotone
token (`:1064`). `resultDebug` is rebuilt or emptied each commit (`:1070-1075`)
so a lexical re-list can't leave stale debug. This is the correct stale-load
guard — the §6f "`gridLoad` monotone token" working as intended. CONFIRMED.

### D1. Query-below-threshold returns to source without clearing bar text  — CONFIRMED (the §6f footgun, anchored)
**Couples:** the `searchLane` indicator ↔ the bar input text ↔ `gridScope`, which
desync when a query drops below `MIN_QUERY_CHARS`.

**Confirmed.** `runQueryScope` (`:1011-1022`): below threshold with no chips it
sets `searchLane = 'none'` and `returnToSource()` but **deliberately does not
clear `this.query`** (comment `:1013-1016` — the input is `bind:value`'d and the
user is mid-type). So the bar shows residual text while the grid is back on its
source scope and the lane reads "none" — the §6f "bar input desyncs from grid
scope until commit or explicit clear." Intentional (don't erase under the
typist), but it IS a cross-seam desync the user can observe.

**Severity:** low (by design). **Fix direction:** none if intentional; if the
desync confuses, render the bar's "not a scope" state explicitly (a dimmed
residue affordance) rather than leaving live-looking text over a source scope.

---

## E. Events / plumbing (the §3 / §6e axis)

### E1. Broadcast-to-all-windows with no source tag → self-render  — CONFIRMED (the §6e footgun)
**Confirmed.** Every backend→frontend event is `handle.emit` to ALL windows
(`STATE-MACHINE.md §3`); `App.svelte:228-251` listeners (`settings-changed`,
`roots-changed`, `collections-changed`) carry no source-window tag, so a window
can't tell it caused its own re-render. `onRootsChanged` (`app.svelte.ts:657`)
re-runs `invalidateScopedGraphs` + `applyRootsSnapshot` + a possible
`openFolder` on every broadcast including its own. **Severity:** low. **Fix
direction:** a source-window id on the `DataChange` envelope (Seam 1's typed
notification is the natural home) so a window can skip its own echo.

### E2. `invalidateScopedGraphs` runs BEFORE `applyRootsSnapshot`  — SUSPECTED
**Suspected.** `onRootsChanged` (`:664-667`) invalidates each removed root's
graphs by diffing `this.roots` (still the OLD snapshot) against incoming, THEN
applies the snapshot. If `graphScope()` were read in that window it would see a
removed root. The ordering is actually correct here (diff old-vs-new must happen
before overwrite), so this is likely fine; flagged only because cache-invalidate
vs state-apply ordering is exactly where the visualizer cache bugs lived.
**Severity:** low. **Fix direction:** none if the diff-before-apply is the
intent (it reads correct); covered by B1's "validate snapshot against
data-version" anyway.

---

## PRIORITIZED PACKET (dependency-ordered, each independently shippable)

Each restores a named seam; later items get cheaper once earlier ones land.

1. **P1 — Stand `ingestExpecting` down on a signal + watchdog (A2).** Highest
   user-visible payoff, smallest blast radius, no new infra. Clear the flag on
   the `scanning`/`ImagesChanged` signal it bridges to, plus a named-constant
   watchdog timeout. *Restores:* the optimistic-flag seam (a flag owned by the
   event it predicts, not an open-ended bet). **Severity addressed:** med.

2. **P2 — Add `images` + `journal` versions and move the grid + inspector onto
   the handshake (A1).** The explicit "still open" of `ARCHITECTURE-CONTRACTS.md`
   Seam 1 step 3. Delete the `App.svelte:261` interval and the
   `lastIngestRefresh`/`INGEST_RELIST_MS` throttle and the `onJournalChanged`
   membership-test relist; the grid re-fetches when its slice's version advances.
   *Restores:* Seam 1 generalized — one versioned refresh policy, zero bespoke
   timers. **Unblocks P3** (the cache can then key on the data-version). **Sev:** med.

3. **P3 — Key the visualizer module caches on the data-version (+ alpha /
   fullLibrary where they affect layout) (B1, B2).** Once P2 exposes per-slice
   versions, fold the rendered version into `graphStateKey` / the neighbor +
   affinity keys, or validate a restored snapshot against the live node-set.
   *Restores:* the cache layer to Seam 1 — a snapshot is valid only for the
   data-version it was built against (the persistence-side twin of the poll fix).
   **Sev:** med.

4. **P4 — Track `selectedTopic` by phrase/id, not array index (B3).** Tiny,
   independent, "identity not index" — the discipline `fixupSelection` already
   uses for grid focus. *Restores:* the bake-panel selection seam. **Sev:** low.

5. **P5 — Make the query/similar → visualizer scope transition explicit (B4).**
   Scope to the result set or refuse calmly; never silently fall through to
   `{kind:'library'}`. *Restores:* the scope-resolution seam. **Sev:** low-med.

6. **P6 — Single-clearer session rotation for the pencil undo stack (C1).**
   Subscribe the look slice to one versioned rotation signal instead of two
   opportunistic clears racing a throttled echo. Cheaper once P2's version
   plumbing exists. *Restores:* the capture-session seam. **Sev:** low.

7. **P7 — Source-window tag on the event envelope (E1)** and the **Seam 3
   constants sweep (B5)** — both low, both naturally fold into the Seam 1 typed
   `DataChange` envelope (P2) and the existing `constants.ts` plan. *Restores:*
   the no-self-render seam and the no-magic-numbers seam. **Sev:** low.

---

*Anchors verified against `main` on 2026-06-17. CONFIRMED = traced in source;
SUSPECTED = needs a runtime check (open→ingest→reopen, silent-rescan, 30m-idle
Ctrl+Z). Spec wins; `STATE-MACHINE.md` is the map, `ARCHITECTURE-CONTRACTS.md`
is the destination, this doc is the punch-list between them.*
