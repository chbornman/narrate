# DESIGN — Attention / engagement heatmap

Draft for founder ratification, June 13 2026. Founder-directed in conversation.
NOT a gaze tracker: it measures where you put INTENT (what you opened, marked,
said), capped and local. No recordings, no surveillance.

## What "attention" is here
An **engagement-intensity** score per image, normalized across the current
scope (collection / folder), from two families of signal:

1. **Dwell (primary, NEW capture)** — time an image was your focus, **capped at
   60 s per focus episode** (founder: keeps lunch-break walk-aways from skewing
   it). Accumulated per image.
2. **Annotation counts (secondary factors)** — how much you marked it up:
   - remark count (voice + typed), rating touches, **stroke COUNT** (a *small*
     factor — founder explicitly DROPPED stroke "effort"/duration as too
     complicated and not tracking true attention; a bare count is enough).
   - Available today from `image_journal_stats` (`event_count`, `has_text`,
     `has_strokes`, `last_ts`); stroke-count needs a cheap addition.

`intensity = w_dwell·dwell_capped + w_events·event_count + w_strokes·stroke_count`,
**recency-weighted by default** (recent attention burns hotter) unless the UI
**"All-time"** toggle is on (then flat, no decay). Weights are data we can tune;
dwell leads.

## The new backend bit: dwell capture
We do NOT store dwell in the annotation event log — the journal is the user's
OWN words/marks (K14). Dwell is machine-observed telemetry, so it lives in a
SEPARATE per-image accumulator table, local-only:

```
image_dwell ( image_hash TEXT PRIMARY KEY, dwell_ms INTEGER, focus_count INTEGER, last_ts TEXT )
```

**A "focus episode"** = a continuous stretch where an image is in focus. Dwell
is **TIERED by how strong the focus is** (founder, June 13 2026):
- **Look-open = full weight (1.0x).** Opening an image in the single-image view
  is the strongest "I am focusing on THIS."
- **Grid selection = far less.** Clicking an image in the grid, OR multi-
  selecting, DOES count, but at a small fraction (e.g. ~0.1-0.2x) of the
  Look rate. For a multi-select, the (reduced) accrual is attributed to each
  selected image (so a 10-image marquee doesn't crown any single one).
- The episode ends on: leaving Look / deselecting / switching, **window blur**
  (app backgrounded), or a short idle (no input for ~N s). On end, add
  `min(tier_rate · elapsed, 60_000) ms` to each focused image's `dwell_ms` and
  bump `focus_count`. The 60s cap is per episode per image.
- Window-blur pause + the 60s cap together handle the walk-away case.

This is the only genuinely new capture in the app; it's light, debounced, and
never leaves the machine. The tier rates (Look vs grid) are tunable data.

## Rendering
- **Grid heat-tint** — a warm glow / corner heat-bar on each cell scaled by
  normalized intensity in the current scope. A toggle (like the T cell-info
  cycle) shows/hides it; off by default.
- **"Sort by attention"** — a new sort mode (composes with the search/collection
  scope we already have).
- **"All-time" toggle (founder, June 13 2026)** — the recency control as a simple
  UI switch, not a slider: DEFAULT = recency-weighted intensity ("what am I
  working on NOW" - recent dwell + annotation count for more); toggled ON = flat
  all-time intensity ("what mattered most ever"). Persisted like the heat toggle.
- Siblings (later, different views, noted not built): a TEMPORAL heatmap
  (sessions-as-spans + events-as-marks, the M4 timeline) and an IN-IMAGE stroke
  density map (strokes carry x,y points) — both real but separate features.

## Phasing
1. **Dwell capture + `image_dwell` table + stroke-count** (backend) — the new
   telemetry, behind the cap/blur rules.
2. **Intensity query** per scope (composite, normalized) + **grid heat-tint**
   toggle + **sort-by-attention**.
3. Tuning pass on the weights against the founder's real library.

## Open decisions for the founder
- RESOLVED: dwell is tiered — Look-open full weight, grid select/multiselect far
  less (founder, June 13 2026).
- RESOLVED: recency is a UI "All-time" toggle — default recency-weighted, ON =
  flat all-time (founder, June 13 2026).
- Default weights + tier rates (dwell-led; stroke-count small; grid ~0.1-0.2x of
  Look) — tune against the real library later.
- Is dwell telemetry something to ever expose/reset in settings? (privacy hygiene
  even though it's local — probably yes, a "clear attention data" button.)
