# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis).

## Next polish round (small, founder-requested)

- [x] **Zoom centering + pan clamp** — landed `652c839` (clampOffsets in
  carryOver; per-axis centering + edge clamp). (Founder, dogfood round 1.)
- [ ] **Search entry as overlay, results as canvas** — `/` opens a floating
  input over the DIMMED current surface (context stays visible; honest
  "overlay" per I1); results expand to the full canvas as they arrive
  (results stay a contact sheet — selection/write-scope/Look behavior
  unchanged, UI §5 stands). (Founder, dogfood round 2.)
- [ ] **Adopt Lucide icons** (`@lucide/svelte`) — replace ad-hoc glyphs
  (🔍 from the spec mockup, sort ▾, ⏏, ×, chevron, titlebar buttons) with
  a consistent stroke set, sized/toned via tokens. UI.md §5 mockup emoji is
  illustrative, not normative. (Founder, dogfood round 2.)
- [ ] **Roots changes propagate live across windows** — adding/removing a
  folder in Settings must appear in the main-window rail instantly:
  `add_root`/`remove_root` emit a `roots-changed` event (same pattern as
  P4.2b's `settings-changed`); App listens → `refreshRoots()`. Same class
  as the journal-staleness fix (`ecf6e26`). (Founder, dogfood round 2.)
- [ ] **Add watched folder from the main window** — currently reachable via
  drag-a-folder-onto-the-window, Settings (Ctrl+,), and the first-run
  screen, but the rail itself has no [+] affordance; add a quiet "Add
  folder…" rail footer row + gutter/rail context-menu entry dispatching the
  same registered action. (Founder, dogfood round 1.)

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
- **M3 additions (founder, dogfood round 2)**: free-text/fuzzy matching
  over metadata fields (camera/lens/filename — typo-tolerant) as a QUIET
  TOGGLE: never default-on, never outranks exact matches, never blocks the
  <100 ms FTS path. **M3 design decision to make**: when projects become
  browsable grids ("collection view"), does search turn contextual — e.g.
  a right sidebar scoped to the collection — instead of the full-canvas
  destination? (Tension: the right edge is reserved for journal/partner;
  founder suspects he'll want search-as-sidebar there. Decide at M3 design
  time, not before.) Full-canvas search stands until then.
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
