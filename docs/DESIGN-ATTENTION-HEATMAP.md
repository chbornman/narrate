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

`intensity = w_dwell·dwell_capped + w_events·event_count + w_strokes·stroke_count`
(+ optional recency decay). Weights are data we can tune; dwell leads.

## The new backend bit: dwell capture
We do NOT store dwell in the annotation event log — the journal is the user's
OWN words/marks (K14). Dwell is machine-observed telemetry, so it lives in a
SEPARATE per-image accumulator table, local-only:

```
image_dwell ( image_hash TEXT PRIMARY KEY, dwell_ms INTEGER, focus_count INTEGER, last_ts TEXT )
```

**A "focus episode"** = a continuous stretch where one image is the primary
subject. RECOMMENDED definition (founder to confirm):
- **Look-open is the strong signal** — opening an image in the single-image view
  is "I am focusing on THIS." Accrue while it's the open image.
- **Optionally** the single focused/centered image in the grid (weaker).
- The episode ends on: leaving Look, switching to another image, **window blur**
  (app backgrounded), or a short idle (no input for ~N s). On end, add
  `min(elapsed, 60_000) ms` to that image's `dwell_ms`, bump `focus_count`.
- Window-blur pause + the 60 s cap together handle the walk-away case.

This is the only genuinely new capture in the app; it's light, debounced, and
never leaves the machine.

OPEN: does grid-single-focus count, or Look-open only? (Look-only is cleaner and
less noisy; I lean Look-only for v1.)

## Rendering
- **Grid heat-tint** — a warm glow / corner heat-bar on each cell scaled by
  normalized intensity in the current scope. A toggle (like the T cell-info
  cycle) shows/hides it; off by default.
- **"Sort by attention"** — a new sort mode (composes with the search/collection
  scope we already have).
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
- Grid-focus dwell, or Look-open only? (lean Look-only)
- Default weights (dwell-led; stroke-count small) — tune later.
- Is dwell telemetry something to ever expose/reset in settings? (privacy hygiene
  even though it's local — probably yes, a "clear attention data" button.)
