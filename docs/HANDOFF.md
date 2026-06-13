# HANDOFF — June 12 2026, end of dogfood round 4

Where things stand at the close of a long build session, and what to pick up
next. Pairs with `CLAUDE.md` (resource map) and `docs/BACKLOG.md` (open work).

Main is at `689fa1c`, pushed, clean. All agent worktrees removed.

## The one thing to do first

**Restart `cargo tauri dev`.** A pile of this session's work is Rust-side
(menus, mic, logging, search fusion) and only activates on a fresh Rust
process. That restart also: builds `pp-asr-server` so voice works, opens a
fresh `~/Library/Application Support/com.photoproof.desktop/logs/photoproof.log`
(now the first-class debug surface — read it after any jank), and picks up
every UI change below.

Then **re-dogfood semantic search** — the ranking fix is the thing to feel.

## What landed this session (all on main, pushed)

Two build waves + inline fixes. Highlights:

- **Search ranking fixed (B75, `0907fe7`)** — was rank-flat (any note buried
  any visual match). Now similarity-aware RRF: dense signals tilt by
  `w·(1/(k+rank))·(1+β·cos)` (β=0.5), CLIP at note parity. Spec deviation in
  `spec/DECISIONS.md` B75 + `spec/RETRIEVAL.md` §5.3. Weights/β stay data the
  §12 eval owns.
- **Space is the mic** (tap=toggle, hold=push-to-talk); M freed. Filmstrip
  rewritten (virtual scroll, selected photo centered). Panel system
  (drag-resizable rail/inspector/filmstrip, canvas-centered, Tab
  snapshot-restore, F in both surfaces). What's-Happening Station (the
  bottom-right status organ). Native macOS menus + ⌘=/−/0 UI zoom. Click
  feedback everywhere. ASR "binary missing" surfacing. Ingest honesty
  (instant scanning + live count). Voice-note leading-space trim.
- **This evening's dogfood-4 fixes:** new-collection-from-grid, fresh
  per-launch file logging, em-dash sweep (no em-dashes in UI copy — it's a
  rule now), search ranking (above).
- **Docs system:** `docs/index.html` (landing), `docs/features.html`
  (cascading built-tree), `docs/LANDED.md` (the shipped-work changelog),
  discipline codified in `BUILD-LOOP.md`. Landed backlog items carry commit
  hashes.

Full landed history with hashes: `docs/LANDED.md` and `docs/BACKLOG.md`
(items marked `[x]`).

## Next up — RAW full decode (decision-complete, READY TO BUILD)

`docs/PLAN-RAW-DECODE.md` is the spec. The founder hit "DNG never loads 1:1 /
154 RAWs stuck decoding"; root cause was an unbuilt M1.5 pass. All decisions
are made:

- **No new dependency** — rawler 0.7.2 already exposes WB coeffs, cam→XYZ
  matrix, CFA, levels. We write the develop math only
  (black/scale→WB→demosaic→matrix→sRGB→gamma, darktable's order).
- **"1:1" = full sensor resolution** (deep-zoom like LR/darktable 100%).
- **Quality = typical neutral decode** (bilinear demosaic fine; "just need
  real resolution"). Not editing — reviewing.
- **Memory = Lightroom's model:** develop once → cache full-res artifact to
  disk → serve zoom from cache. One develop in flight; tiled-demosaic
  fallback on low RAM.
- **ON-DEMAND, not eager** (founder, explicit): do NOT develop every RAW on
  ingest. **Remove** the eager `full-raw-decode` enqueue in the preview pass
  (that made the 154 stuck rows). Add a **view-time trigger** in Look that
  develops at top interactive priority when a RAW's full-decode artifact
  isn't cached, with a "developing…" state; optionally pre-warm next/prev in
  the filmstrip.

Build shape (one focused agent, not parallel — it's one coherent pipeline):
new `raw_develop` module in `photoproof-core`, the on-demand trigger + Look
"developing…" UX, disk-cache wiring. Review the develop math carefully before
merge (like the search fusion). Phase 1 = neutral full-sensor decode on
zoom. Start this RESTED — a from-scratch demosaic pipeline deserves it.

## Other open dogfood-4 items (smaller)

- **Grid right-click submenu jank** — `ContextMenuHost.svelte` is a 1KB stub;
  needs a real side-flyout (edge-flip, hover-intent, keyboard).
- **T cell-info layout** — grow the cell downward instead of overlaying the
  image; info at the top. Touches grid row-height math, not just CSS.
- **Foreign edit sidecars / review-exports** — own thread (don't block RAW
  decode). Honest take in the plan: first-class an exports-folder review
  (cheap, already works); sidecar = portable subset only (crop/orientation/
  rating), faithful XMP render is out of scope.

## The bigger arc (founder-sequenced)

1. Ranking fix ✅ → **2. search-as-scope UI overhaul** (query as a grid
scope, always-visible search bar, live-lexical/commit-semantic, relevance
sort + per-signal toggles that make B75's weights user-visible). This is the
next big design-and-build after RAW decode, and the founder already chose its
shape. 3. Then §12 retrieval-quality eval, spike session 2 (RTX/Windows),
M1.5/Phase 8 scheduling.

## Gotchas

- Known failing test `s02_2_case_only_rename_relinks_sidecar` (APFS
  case-rename) is pre-existing — don't chase it.
- Worktree-agent flow: agents sometimes finish without committing; commit in
  their worktree, then merge to main and re-run the gate on the merged tree.
- Verify gate: `cargo fmt --check` / `clippy -D warnings` / `cargo test`;
  frontend `npx svelte-check --fail-on-warnings` + `vitest` from `apps/desktop`.
