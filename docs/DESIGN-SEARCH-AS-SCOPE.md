# DESIGN — Search-as-Scope UI Overhaul (M3)

PhotoProof desktop · Svelte 5 + Tauri · drafted for founder ratification · 2026-06-12

This is a design round. No code has been written. All file:line citations are
from a read-only pass of `main`. The build is HELD until the founder ratifies
the open decisions (D1–D6, below).

---

## 0. The one-sentence problem

Today the query and the grid are **two different surfaces with two different
selection systems**. The vision: they become **one** — the query is just the
grid's scope, the same way a folder or a collection already is.

The backend already speaks this language. The frontend does not — yet.

---

## 1. Research findings (current state, cited)

### 1.1 Search today is a separate overlay-then-canvas destination
- Entry is `/` or `Cmd+F` (`actions/defs/global.ts:85-91`) → `openSearch()`
  (`state/app.svelte.ts:530-546`), which **resets query/chips/results/selection
  on every open** (540-544) — the opposite of "always-visible."
- The whole experience is `SearchOverlay.svelte` with a derived two-stage model
  (`:49-54`): `entry` = floating input over a scrim; `canvas` = a full-bleed
  opaque contact sheet (`:189-191`). The moment results exist, the grid you came
  from is **replaced**, not scoped.
- The query has its **own selection state** (`searchSel`/`searchFocus`/
  `resultHashes`, `app.svelte.ts:114-118`), parallel to `grid.sel`, reconciled
  only at `openLook` via a `fromSearch` branch (467-492).
- Debounce 50ms (`SearchOverlay.svelte:41`), `MIN_QUERY_CHARS=2`.
- The **query-residue indicator** the M3 backlog references does NOT exist yet.
- Chips render the executed `Filter[]` (`:133-144`); NL chip parser is M3-future.

### 1.2 The backend fusion — already user-facing-ready
`crates/photoproof-core/src/search/hybrid.rs`:
- `FusionWeights` (100-134) is already a struct of per-signal floats:
  `s1` (note own-words vectors), `s2` (note keyword FTS), `s3_each` (derived
  prose), `s4` (image CLIP). Defaults post-B75: 1.0 / 1.0 / 0.5 / 1.0.
- `SIM_BLEND_BETA = 0.5` (86), documented as a default, not a finding.
- Both thread through `HybridOptions { weights, ... }` into `run()` — per-search
  weight variation already works (`retrieval_hybrid.rs:347-354` proves it).
- Signals `SignalId` (`search/mod.rs:305-311`) map 1:1 to S1/S2/S3/S4.
- Per-result provenance already crosses IPC: `DebugScores { per_signal, fused }`
  (`types/search.ts:75-78`) — the data the toggles want to SHOW is already wired.
- IPC: one `search(query, filters)` command (`search_wire.rs:36-78`) that
  **hard-codes `HybridOptions::default()`** (71) — weights/β reachable but
  currently invisible constants.
- <100ms budget is real and tested (`search_latency.rs`, >1M rows); cancellation
  on keystroke is wired (`search()` calls `interrupt()`).

### 1.3 The grid receives its set three ways — none is a query
`grid.units` ← `sortItems(grid.rawItems)`; `rawItems` filled by `setItems`
(`grid.svelte.ts:131`), called from exactly: `openFolder` (`listFolder`),
`openCollection` (`listCollectionMembers`), `refreshItems`. Mutually exclusive
modes arbitrated by `collectionId` (null = folder mode) + a `gridLoad` token.
**A query is NOT one of these modes** — results live in `results` and render in
the overlay. This is the crux to fix.

### 1.4 Where a bar could dock
`.main` = `[rail][center][inspector]`; `.center` = `[canvas][filmstrip]`. Right
edge reserved for inspector/journal/partner (M5). The natural dock for an
always-visible bar is the **grid header** (`GridHeader.svelte`, a 30px strip:
folder-name · sort ▾ · thumb-slider) — it already owns "what is this grid."

---

## 2. The design proposal

### 2.1 Core move: the query is a grid scope
A **third grid mode** next to folder and collection. Generalize the two-mode
`collectionId` arbitration into a `gridScope` union:
```
type GridScope =
  | { kind: "folder"; rootId; folder }
  | { kind: "collection"; id }
  | { kind: "query"; query; chips; within?: GridScope }   // NEW
```
When a query commits, `setItems` is fed from the **search result hashes**
instead of `listFolder`/`listCollectionMembers`. The grid renders results
**in place** — same cells, same `grid.sel`, same marquee, same `openLook`, same
context menu, same add-to-collection. **One selection system.** Retire
`searchSel`/`searchFocus`/`resultHashes`, the `fromSearch` branch, and the
overlay canvas stage. Default sort in query mode = a new `relevance` mode that
preserves the backend's fused order.

### 2.2 The always-visible bar — docked in the grid header
```
┌──────────────────────────────────────────────────────────────────────────┐
│ ▍Harbor 2024            [ 🔎  fog over the ridge________ ]  ~ ⚙   sort▾  ⬚⬚ │  header
├──────────────────────────────────────────────────────────────────────────┤
│  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐                   │
│  │////│ │    │ │////│ │    │ │////│ │    │ │////│ │    │   the GRID itself  │
│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘   is the result   │
│  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐   set — query is  │
│  │    │ │////│ │    │ │////│ │    │ │////│ │    │ │////│   a SCOPE, not a   │
│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘   destination     │
└──────────────────────────────────────────────────────────────────────────┘
   ▍ = scope residue (folder/collection you are searching within)
   ~ = fuzzy quiet-toggle (off by default, dim until armed)
   ⚙ = signal-tuning popover trigger (per-signal toggles; 2.5)
```
On focus-with-text the header grows a thin detail row:
```
│   within: Harbor 2024   [date: 2024 x]      lexical · Enter for semantic │
```
`/` and `Cmd+F` focus the bar (no overlay). First `Escape` clears the query
scope and returns the grid to its underlying folder/collection (the residue
tells you which); second `Escape` blurs. The `within:` residue IS the M3
"query-residue indicator with one-key clear."

### 2.3 Live-lexical / commit-semantic (the <100ms guardrail)
- **LIVE-LEXICAL (every keystroke, 50ms debounce):** the keyword-only FTS path
  (`run_search_keyword`) — the M1 path the latency test pins <100ms. No embedder,
  no CLIP, no LLM parse. The grid re-scopes live as you type. The floor.
- **COMMIT-SEMANTIC (on Enter):** the full hybrid rig (S1/S3/S4 vectors + S2 +
  fusion). Expensive, paid only on commit, never on the keystroke budget.
The detail row shows the mode. Re-typing drops back to lexical until next Enter.
Mechanism: add `mode: "lexical" | "semantic"` to the `search` command so the
frontend can FORCE the keyword lane even when embedders are warm (otherwise a
warm machine pays vector latency per keystroke).

### 2.4 The relevance sort mode
Add `relevance` to `SortMode` (`logic/sort.ts`). In query mode it is the default
and means "preserve the backend's fused order" (pass-through in `sortItems`).
Unavailable in folder/collection mode. Sort ▾ still composes with the query
scope — re-sorting by date/filename re-orders the same result hashes.

### 2.5 Per-signal toggles — making B75 weights visible without taxing the quick path
A ⚙ popover (opened on demand, never on the keystroke path):
```
┌─ Ranking signals ───────────────────────────┐
│  [x] S1  Your words (notes)      |####|  1.0 │
│  [x] S2  Note keywords           |####|  1.0 │
│  [x] S3  Derived descriptions    |##|    0.5 │
│  [x] S4  Visual match (CLIP)     |####|  1.0 │
│  ─────────────────────────────────────────── │
│  similarity tilt (beta)          |##|    0.5 │
│  [ Reset to defaults ]                        │
└───────────────────────────────────────────── ┘
```
Checkbox = include (weight 0 when off); slider = the `FusionWeights` field; β =
a per-search override of `SIM_BLEND_BETA`. Wiring is small because the plumbing
exists: add an optional `weights`+`beta` payload to the `search` command; build
`HybridOptions` from it (replacing `::default()` at `search_wire.rs:71`); promote
`SIM_BLEND_BETA` from a const to a `HybridOptions` field. Show, don't just tune:
gate `DebugScores.per_signal` (already on the wire) behind the popover being open
so each cell can show its per-signal contribution. Guardrail: the popover is
**semantic-lane only** — S1/S3/S4 don't exist on the lexical keystroke path, so
tuning weights can never touch the <100ms budget. Default-closed, default-weights.

### 2.6 The fuzzy quiet-toggle
A dim `~` glyph. Off by default. When armed, typo-tolerant fuzzy over metadata
(camera/lens/filename), per the backlog: never default-on, never outranks exact
matches, never blocks FTS. Rides as a flag; exact-match runs first and
unconditionally, fuzzy is additive widening. Backend FTS-fuzzy is its own packet.

---

## 3. DECISIONS — RATIFIED (founder, June 12 2026)

All six ratified AS RECOMMENDED below. Phase 1 (query-as-scope + always-visible
bar + relevance sort, lexical-only) is cleared to build once the current
RAW/UI work lands.

**D1 — Search-as-sidebar-for-collections vs query-as-grid-scope.**
*Recommend:* No separate sidebar. Query-as-scope subsumes it — a query always
scopes *within* the current source, shown by the `within:` residue. Right edge
stays reserved for inspector/journal/partner. *Decide: ratify subsumption, or
keep a collection sidebar on the table?*

**D2 — Do per-signal toggles change weights live, or just on/off?**
*Recommend:* Both, tiered. Ship on/off checkboxes first (the legible, low-risk
slice that answers "make B75 visible"). Continuous sliders + β as a second tier
behind a "show advanced" disclosure — free-floating weights are what the §12
golden-set eval is meant to own. *Decide: continuous sliders in v1, or eval-gated?*

**D3 — Where does the bar dock?**
*Recommend:* Grid header, inline (it already owns what/how-ordered). Rejected: a
dedicated top-of-canvas bar (steals vertical space permanently). *Decide:
inline-in-header, or its own row?*

**D4 — Commit replaces the grid, or layers over it?**
*Recommend:* Replace (re-scope) — that's the thesis; residue + one-key clear is
the safety. *Decide: confirm replace-in-place, vs. a softer "highlight matches
within the existing grid" for small folders?*

**D5 — Persistence of query scope across navigation.**
*Recommend:* Per-grid-source and ephemeral — switching folders/collections clears
it; returning does not restore it. Saved searches (a separate backlog item) are
the durable form. *Decide: ratify ephemeral, or should the query survive folder
switches?*

**D6 — Lexical-vs-semantic default on warm-embedder machines.**
*Recommend:* Always lexical as-you-type; semantic only on Enter — even when
embedders are warm — to protect the keystroke budget. Requires the explicit
`mode` arg on the `search` command. *Decide: accept explicit-mode plumbing, or
let Enter be an implicit "upgrade if embedders ready"?*

---

## 4. Phased implementation plan (each phase carries the <100ms guardrail)

**Phase 1 — Query-as-scope + always-visible bar + relevance sort (ships first).**
Lexical-only; no per-signal UI. Generalize `collectionId` → `gridScope` union;
add a `runQueryScope()` `setItems` feeder (guarded by `gridLoad`); retire
`searchSel`/`searchFocus`/`resultHashes` + the `fromSearch` branch + the overlay
canvas stage. Bar in `GridHeader.svelte`; rewire `/`+`Cmd+F` to focus it; rewire
Escape to clear-query-scope; add the `within:` residue. Add `relevance` to
`SortMode`. Backend: add `mode` to the `search` command (default preserves
today). Guardrail: as-you-type runs lexical only (the path `search_latency.rs`
pins <100ms); extend that test for the `mode` arg.

**Phase 2 — Live-lexical/commit-semantic split + detail-row status.** Mostly
frontend state on Phase 1's `mode` arg. Add a test that as-you-type only ever
calls `search` with `mode:"lexical"`.

**Phase 3 — Per-signal toggles (on/off) + B75 legibility.** ⚙ popover; `weights`
payload on the command; `HybridOptions` built from it; promote β to a field;
surface `per_signal` when the popover is open. Semantic-lane-only by construction.

**Phase 4 — Continuous sliders + β (eval-gated per D2) + fuzzy quiet-toggle.**
Sliders behind "advanced"; `~` toggle + command flag (backend FTS-fuzzy is its
own packet). Fuzzy is additive metadata-only widening after exact FTS.

---

## Critical files (for the eventual build)
- `apps/desktop/src/lib/state/app.svelte.ts` — `collectionId` → `gridScope`
  union, `setItems` feeders, search-state retirement, Escape/scope wiring
  (92-93, 114-118, 331-429, 467-492, 530-570, 926-978)
- `apps/desktop/src/lib/components/grid/GridHeader.svelte` — the bar's dock
  (+ `GridSurface.svelte:31-33` chrome-hidden gate)
- `apps/desktop/src/lib/components/search/SearchOverlay.svelte` — retired/reduced;
  chip + debounce + result logic migrates into the header bar
- `apps/desktop/src-tauri/src/search_wire.rs` — `run_search` (36-78): `mode` lane
  selection; `HybridOptions::default()` (71) → built-from-payload
- `crates/photoproof-core/src/search/hybrid.rs` — `FusionWeights` (100-134),
  `SIM_BLEND_BETA` (86, 797): promote β to a field; weights are already per-call
- supporting: `logic/sort.ts` (`relevance`), `commands/search.rs` +
  `ipc/commands.ts` (new `mode`/`weights`/`beta` args),
  `tests/search_latency.rs` (the <100ms guardrail to extend per phase)
