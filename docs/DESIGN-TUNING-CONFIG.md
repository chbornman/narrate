# DESIGN — Centralized tuning configuration

Founder-directed, June 13 2026: "we probably want a set of configuration files
for our entire app tuning, not spread randomly across our implementation files."

Today the tunable weights live as scattered `const`s: `FusionWeights` defaults +
`RRF_K` + `SIM_BLEND_BETA` in `hybrid.rs`, `SEARCH_DEBOUNCE_MS` in
`GridHeader.svelte`, `MIN_QUERY_CHARS` in `app.svelte.ts`, `DISPLAY_EDGE` /
`EMBEDDED_ACCEPT_EDGE` / aspect tolerance in `preview.rs`, voice rules in
`pp-asr-server`. The heatmap and graph are about to add a dozen more. They need
ONE home. `docs/tuning.html` becomes a rendered VIEW of that config.

## Principle
Every arbitrary ranking / recommendation factor is defined in a typed config
surface with code defaults, **overridable from a file without recompiling** (so
the founder can tune and re-feel without a build). Implementation files READ the
config; they no longer own the numbers.

## Shape — two typed configs (tuning spans both layers) + a file

### Core (Rust): `crates/photoproof-core/src/tuning.rs`
A serde `Tuning` struct, nested by domain, `#[serde(default)]` on every field so
a partial file merges cleanly over the code defaults:

```
Tuning {
  search:  SearchTuning  { fusion: FusionWeights, rrf_k, beta, min_query_chars, ... }
  heatmap: HeatmapTuning { w_dwell, w_events, w_strokes, dwell_look, dwell_grid, dwell_cap_ms, recency_decay }
  graph:   GraphTuning   { alpha, attraction, repulsion, anchor_layout }
  preview: PreviewTuning { embedded_accept_edge, display_edge, aspect_tolerance }
  voice:   VoiceTuning   { rule2_ms, ... }   // or leave with pp-asr-server's own config
}
```

- `impl Default for Tuning` holds the CURRENT live values (moved out of the
  scattered consts; the old consts either move here or re-export from here so
  there is ONE definition).
- `Tuning::load(app_data) -> Tuning`: read `<app-data>/tuning.toml` if present,
  merge over defaults; absent file = pure defaults. Validate ranges; a bad value
  is rejected with a logged warning and the default kept (never a silent bad
  number).
- Held as a process-global initialized at startup (a `OnceLock<Tuning>` or
  threaded through the library handle). The per-search weight OVERRIDES (the
  Phase-3 user toggles) compose on TOP of `tuning.search.fusion`.
- The **eval harness loads the same `Tuning`** so a sweep tunes the real config,
  and a winning config is just a `tuning.toml` you keep.

### Frontend: `apps/desktop/src/lib/tuning.ts`
One module for UI-only knobs (`SEARCH_DEBOUNCE_MS`, graph-slider default, dwell
tier rates if tracked client-side, etc.). For values the BACKEND owns that the
UI also needs, expose them through a `get_tuning` command (read the backend
config) rather than duplicating - single source.

### The file: `tuning.toml`
RECOMMEND one well-sectioned file for v1 (`[search] [heatmap] [graph] [preview]
[voice]`) - a "set" of grouped config that's easy to see whole - rather than a
`tuning/` dir of many files. Splittable later if it grows. A committed
`tuning.default.toml` in the repo documents the defaults; the live one sits in
app-data and is git-ignored (it's user state).

## Sequencing (matters for merge safety)
1. **After fuzzy merges** (it's editing `hybrid.rs` + the search bar right now -
   a config refactor of those files would collide).
2. **Build the config system by consolidating EXISTING scattered consts first**
   (search weights, RRF_K, beta, min-chars, preview edges, debounce), updating
   references + pointing `tuning.html` at `tuning.toml`. No behavior change -
   the defaults equal today's values; this is a pure "give them one home" move,
   verified by the gate.
3. **Then heatmap + graph land their tuning IN the config from day one** (their
   design docs already name the knobs) - so the new features never reintroduce
   the scatter.

## Open decisions for the founder
- One sectioned `tuning.toml` (recommended) vs. a `tuning/` directory of files?
- Runtime file-load + restart is the v1 (the point is "edit and re-feel"); a
  live hot-reload (no restart) and an in-app tuning PANEL with sliders are v2.
- Anything that should stay a hard constant (a real budget/contract, e.g. the
  <100ms search budget, geometry tolerances) vs. a tunable - I'd keep
  budgets/contracts as fixed consts and only pull genuine JUDGMENT knobs into
  the config. (tuning.html marks these "fixed" vs "owned by eval/founder".)
