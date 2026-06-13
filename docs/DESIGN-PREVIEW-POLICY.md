# DESIGN — Preview / 1:1-cache policy

Founder-directed, June 13 2026 ("set up the suggested ideas in settings").
How we manage the disk cost of previews and full-res RAW develops, shaped by how
people actually USE the app: they point at folders of finished work and REVIEW —
opening many RAWs (each building a big full-res 1:1 to disk), annotating,
collecting. They do not want to babysit storage. So the policy is **automatic,
with one knob** — managing is off-thesis.

## The tiers
- **Thumb + display previews** (small): always built on ingest, always kept.
- **Full-res 1:1 artifacts** (big, built on-demand by RAW decode, cached to
  disk): the only tier that needs a policy.

## Policy (proposed shape — ratified for settings)
- **A single cache budget** governs the 1:1 artifacts: "Keep 1:1 previews until
  the cache exceeds [N GB], then evict least-recently-VIEWED." Default generous
  (e.g. 20 GB). Predictable, matches how real disk pressure works.
- **Eviction is always safe**: a discarded 1:1 re-derives on next view, and
  strokes live in display-oriented VECTOR coords (not the cached artifact), so
  nothing is ever lost by evicting. Eviction can be aggressive.
- **A visible cache-size readout** (Settings, and/or the Station hover) + a
  manual **"Clear 1:1 cache"** and **"Clear all previews"** button.
- (Alternative offered, deferred): an AGE window ("discard 1:1 after N days
  unused", LrC's model). Less predictable than a size cap; can be exposed later
  as an option. v1 = size budget.

## Settings section (what to build)
- `[Previews]` section in Settings:
  - **1:1 preview cache budget**: a size input (GB) with the LRU-evict behavior.
  - **Cache size readout**: current 1:1 cache size on disk.
  - **Clear 1:1 cache** / **Clear all previews** buttons (with the safe-to-clear
    note).
- Backend: track 1:1 artifact LRU (last-viewed ts + size) and an evict pass that
  trims to the budget; a `preview_cache_stats()` + `clear_preview_cache(kind)`.
  The full-res artifacts are disk-only today (no `preview_artifacts` row), so the
  evictor walks the `previews/<...>-full.*` files by mtime/atime + size.

## Why this shape
The user reviews; they don't manage. A size cap with safe LRU eviction means the
disk never surprises them and they never have to think about it — but the one
knob + the clear buttons are there for the power user who wants control. Tier
choices (which previews to build) stay fixed; only the 1:1 retention is exposed,
because that is the only tier whose disk cost is large and user-variable.
