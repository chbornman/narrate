# DESIGN: Unified view modes (scope × viewMode)

Status: accepted (founder, June 13 2026). Implementation contract for collapsing
the three center views onto one orthogonal axis.

## The model

Two orthogonal pieces of state:

- **`scope`** the noun: the existing `gridScope` discriminated union
  (`folder | collection | query | similar | topic`). Already clean. UNCHANGED by
  this work.
- **`viewMode`** the lens: `"grid" | "visualizer" | "look"` (OPEN, extensible —
  `compare` is a future member). Replaces the old `surface: "grid" | "look"`
  enum PLUS the bolted-on `graphOpen: boolean` overlay.

Plus one **shared active image** (`activeHash`) that SURVIVES a view switch:
toggling grid → visualizer → look keeps you on the same photo.

Why: today the visualizer is an overlay boolean threaded through ~20 call sites
(`goHome`, `openFolder`, `reportScope`, `actionContext`, tests…). Adding a 4th
view (compare) under that shape would mean another boolean threaded everywhere.
With `viewMode` as one axis, a new view is ~5 additive edits (see the litmus at
the end).

### Type + location
`export type ViewMode = "grid" | "visualizer" | "look"` lives in
`lib/actions/types.ts` (next to `ActionContext`), imported by keymap / scope /
escape / menus. `app.svelte.ts` holds `viewMode = $state<ViewMode>("grid")`.

### App.svelte rendering
One `{#if}/{:else if}` chain keyed on `viewMode` (grid / visualizer / look),
replacing the old `{#if surface==="grid"}…{:else}<LookSurface/>` plus the
separate `{#if graphOpen}<TopicGraph/>` overlay. The visualizer becomes a PEER
view (renders instead of the grid, not over it). TopicGraph already fills its
container, so nothing moves visually.

### Shared active image
`get activeHash` is the single funnel (inspector / membership / dwell / scope all
read it):
```
switch (viewMode) {
  case "look":       return look.currentHash;
  case "visualizer": return viewSelection;   // renamed from graphSelection
  case "grid":       return grid.activeHash;
}
```
Per-view cursors stay the source of truth for navigation (grid `sel.focus`, look
`index`, visualizer `viewSelection`). On a VIEW SWITCH, the target cursor is
SEEDED from the current `activeHash` so the photo carries across. `graphSelection`
is renamed `viewSelection` (no longer graph-specific; a future compare view could
reuse it). Its `null` semantics (neutral session scope) are preserved.

## Scope reconciliation (R6) — seed-from-active, neutral when none

The earlier "opening the Visualizer neutralizes the scope so dictation never hits
a stale image" decision is reconciled with "keep the same photo across a switch"
as follows:

- Opening the visualizer **seeds `viewSelection` from `activeHash`** — that image
  is the one you were just on (grid focus / look current), NOT stale. Scope =
  `[activeImage]`, so dictation/rating continue on that photo.
- If `activeHash` is `null` (fresh scope, nothing focused), the visualizer opens
  **neutral** (`viewSelection = null`, scope `[]`) — no stale target, dictation
  becomes a session note.

This honors both intents. `scopeTargets` for the visualizer returns
`viewSelection != null ? [viewSelection] : []`.

## Transition table

Trigger → viewMode (from→to) · scope · activeHash

- `openFolder`/`openCollection`/`openTopic` (rail): look→**grid**, grid→grid,
  visualizer→**visualizer** (persists, re-points at the new scope via
  `graphScope()`). New scope; grid focus resets.
- `runQueryScope` commit: stays current mode. Query scope; focus reset.
- `find-similar` (`runSimilarScope`): look→**grid**, else→grid. Similar scope.
- **`g`** (`go-grid`/`goHome`): *→**grid**. Clears derived scope to source; same
  image stays active.
- **`l`** (`toggle-graph`): grid→**visualizer**, visualizer→**grid**,
  look→**visualizer**. Scope seeded from activeHash. Photo preserved.
- **Enter** (`open-look`): grid→**look** on focused cell; visualizer→**look** on
  `viewSelection` (via `openFromGraph`, no teardown).
- **Esc** in look: look→**grid** (leave-look), same image active.
- **Esc** in visualizer with selection: deselect (`viewSelection=null`), stay.
- **Esc** in visualizer no selection: →**grid** (`leaveVisualizer` seeds grid
  focus from the departing `viewSelection`).
- `scopeToTopic` (anchor click): visualizer→**grid** + semantic query scope.

Tricky cases resolved:
- (i) Rail scope select persists grid/visualizer, drops look→grid. Falls out for
  free once `leaveLook`/scope-feeders gate on `viewMode==="look"` and no longer
  force the visualizer closed.
- (ii) `repointScope`/`returnToSource` (scope-swap under Look) key on `gridScope`,
  not surface — untouched.
- (iii) `openFromGraph` becomes `await openLook(hash); viewSelection = null` (no
  `closeGraph`-then-open flash through grid; `openLook` sets `viewMode="look"`
  directly).
- (iv) Esc ladder: visualizer Esc stays component-local in TopicGraph
  (deselect-first, then `leaveVisualizer`); look/grid Esc via `escape.ts`.

## Regression guards
- **graphstore persist/restore**: mount/unmount happens on the same
  enter/leave-visualizer transitions; keying `(scope, topic-set)` UNCHANGED.
- **dwell/heat**: `dwellRefocus` gets a visualizer arm (attribute single-image
  dwell to `viewSelection`, or null when neutral). Heat keys on scope hashes.
- **inspector follow**: via `activeHash` getter + every transition ending in
  `reportScope` (keep that invariant).
- **rate() session guard**: visualizer-with-null-selection → scope `[]` → session
  → rate no-ops. `rate.enabled` becomes `viewMode==="look" || hasSelection`.
- **keymap scope eligibility**: `scopeEligible` grid/look arms gate on
  `viewMode`. Visualizer keys (Esc/Enter) stay component-local (as today). Nuance:
  grid-scoped keys (arrows, select-all) become INELIGIBLE in visualizer — more
  correct than today (arrows shouldn't move grid focus under the graph).
- **ActionContext**: `surface` field → `viewMode`. `keymap.ts` LegacyContextKeys
  and all fixtures updated.
- **advance.ts**: keep its `"grid"|"look"` field; caller passes
  `viewMode==="look" ? "look" : "grid"`.

## Compare-mode litmus (proves locality)
Adding a future `compare` touches exactly: (1) the `ViewMode` union (one token);
(2) one `App.svelte` render arm; (3) `app.svelte.ts` — one `activeHash` arm, one
`dwellRefocus` arm, an `enterCompare`/`leaveCompare` pair on the
`openVisualizer`/`leaveVisualizer` template, and a trigger; (4) `scope.ts` only if
compare has its own write-scope rule (else falls through to grid selection); (5) a
`CompareSurface.svelte`. No new boolean, no edits to `goHome`/`openFolder`/
`reportScope`/`actionContext`/four tests. That locality IS the win.
