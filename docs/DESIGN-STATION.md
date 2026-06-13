# DESIGN — What's-Happening Station 2.0 (the bottom-right pill)

Founder-directed, June 13 2026 ("station update sounds perfect"). An upgrade to
the existing What's-Happening Station (`logic/station.ts`, landed `de9f126`) into
the app's living status organ: always-present core, transient pulsing icons when
work happens, a colored border that reflects the active action, and a
first-class "you're missing a model" surface.

## Collapsed pill (the always-visible form)
- **Core icons, always present** (never move): mic · search · background-tasks ·
  pencil.
- **Transient icons** fade in + gently pulse ONLY while their thing is live, then
  retire: ingest/digest (with a **count badge + a progress arc**), embedding
  pass, model-download — or a **model-MISSING** warning icon.
- **A colored border = the most salient active state**, by PRIORITY (highest
  wins when several are live):
  1. **mic armed → red** (recording / push-to-talk)
  2. **error / missing model → amber**
  3. **background work (ingest / digest / embed) → a cool "working" hue**
  4. **idle → no border** (neutral)
- **Larger collapsed footprint** than today so the border + transient icons read
  at a glance. Gentle breathe while anything is active; the existing
  note-creation "pop" stays.

## Hover (the expanded organ)
Expands to the full detail (it already hover-expands; enrich it):
- per-pass progress with **done/total counts** (ingest, hash, preview, embedding)
- the background task list + current scope + streaming utterance
- **MISSING-MODEL prompts, first-class**: "Semantic search needs the CLIP model ·
  Download (1.2 GB)" with the download action inline. A missing model silently
  breaks a feature today; the Station becomes where that surfaces and resolves.

## State model (extends `station.ts`)
- A set of "organs", each with a state (`idle | active | pulsing`) and optional
  progress (done/total) + a label, derived from existing app state (ingest
  progress channel, embedding queue, mic `capture_live`, model registry).
- A pure **border-color resolver**: maps the live organ set → the winning border
  color via the priority order above. Unit-testable; no new capture machinery
  (everything is already evented — it is a query + rendering problem).
- Model-missing comes from the model registry (which models a feature needs vs
  which are present); surfaced as an organ + the amber border + the hover prompt.

## Why
This single organ subsumes three open needs: digest visibility (counts/progress),
model-download visibility (what's missing + a fix), and live action color (mic =
red). It honors the lights-out exemption (DECISIONS U5) and the
note-pop signature. It is the COLLAPSED form of the digest-visibility surface the
backlog asked for — expandable, not always-on counts.
