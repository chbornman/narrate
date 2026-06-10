# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis).

## Next polish round (small, founder-requested)

- [ ] **Zoom centering + pan clamp** (coordinator applies at the P4.2b
  merge): when the scaled image fits the viewport on an axis, center on
  that axis; while it overflows, clamp panning so edges never detach from
  the viewport. One pure `clampOffsets()` in zoom.ts + unit tests.
  (Founder, dogfood round 1.)

- [ ] **Journal entries show sibling targets** — a multi-target note's entry
  carries a quiet "+N others" affordance (targets are already in the event;
  the journal DTO needs them surfaced). (Founder, dogfood round 1.)
- [ ] **Select images from note** — from a journal entry, select the event's
  full target set in the grid (jump home + select). The vision statement as
  a verb; also an M3 workflow entry point (select → file into project).
- [ ] **Backend `journal-changed` event** carrying affected hashes, so open
  surfaces refresh without frontend-triggered reloads — required before
  M2b voice (events will land without UI actions); the M1 writers are
  covered by direct refresh hooks (`ecf6e26`).

## M1.5 (scheduled concept, not yet a packet)

- [ ] Full RAW decode backfill pass (rawler/libheif worker; queue already
  knows the pass kind) — unlocks HEIC previews + RAW 1:1 zoom.
- [ ] Preview-policy settings (which previews to build/keep; LrC-style
  "build 1:1 on demand, discard after N days" knobs) — founder asked for
  exposure of these as toggles eventually.

## Milestone-attached extras (build with their milestone)

- **M2a (pencil)**: pencil toolbar from the reserved P/E/V band; overlay
  cycle key; stroke rendering in journal entries (stub exists).
- **M2b (voice)**: mic indicator segment (seat reserved); hold-to-talk
  duality; journal-changed event (above) becomes load-bearing.
- **M3 (retrieval/projects)**: rail source-list grows projects + saved
  searches; drag-selection-to-rail filing; query-residue indicator segment
  with one-key clear; chip-creation UI (parser-driven); select-from-note ↔
  project filing workflow chain.
- **M4 (time)**: Look bottom-edge stroke scrubber (seat reserved); journal
  timeline rendering upgrade; trajectories as an alternate grid lens.
- **M5 (partner)**: right-edge dockable panel sharing the inspector slot;
  summon key reserved; obeys Tab lights-out unconditionally.

## Decided, awaiting founder appetite

- [ ] Full interface themes (light chrome + grays) — token architecture
  ready; surround-luminance shipped in P4.2 (D6).
- [ ] Configurable external editor (D4 revisit).
- [ ] Type-to-jump filename in grid (Search covers it meanwhile).
- [ ] Burst/HDR-bracket stacks beyond RAW+JPEG.
- [ ] GPS map view; histogram in Look (needs decode-pipeline access).
- [ ] Very-large grid cells served by display previews (>512px targets).
- [ ] CI pipeline (GitHub Actions: standing gate + OS-matrix sidecar
  byte-compare + nightly full-scale `#[ignore]` lane).

## Recorded, not designed (K17 — unchanged)

Future fine-tuning of a small LLM for app tasks; voice-command retraction;
audio-retention opt-in; multi-machine sync as a product feature.

## Won't build (UI-FEATURESET §8 + D3 — kept here so they stay decided)

Color labels / pick-reject flags · metadata editing · image editing ·
import/copy/move workflows · in-app deletion (D3) · multi-window/tabs ·
auto-hide chrome · keyword taxonomies (projects are intent groupings with
evented membership — "tags with time" — never hierarchical vocabularies).
