# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis).

## Next polish round (small, founder-requested)

- [ ] **Pair targets vs "+N others"** — a journal entry on a collapsed
  RAW+JPEG pair targets both members (DECISIONS 4), so the sibling mark
  reads "+1 other" even though no OTHER image is involved — misleading.
  Decide the right indication: suppress the mark when every extra target
  is the inspected image's own pair member ("● 2" already says it), or a
  distinct quiet register for pair-mate targets. (Founder, dogfood
  round 3, June 2026.)
- [ ] **"Rebuild previews…" on the rail folder menu** — a recovery/
  maintenance verb SEPARATE from Rescan (different semantics: Rescan
  reconciles files↔index and enqueues missing passes; Rebuild re-enqueues
  the preview pass for everything under the root, the generator_version
  machinery's manual trigger). Becomes more load-bearing with M1.5
  preview-policy knobs. (Founder, dogfood round 3, June 2026.)
- [ ] **First-run welcome card: how your data is stored** — a plain-words
  disclaimer on first launch ("don't show again" toggle, persisted like
  other prefs): sidecars are FILENAME-SPECIFIC (`.pp.json` beside the
  image — rename outside the app while it isn't watching and the link
  depends on the §7 relink heuristics), the index is rebuildable but the
  journal sidecars ARE the data, what lives where. Beyond the card, think
  about making the storage story itself stronger — e.g. hash-keyed
  sidecar recovery sweep, case-insensitive-filesystem rename semantics
  (APFS: a case-only rename isn't a rename; s02_2 fails on macOS today),
  import-time warnings on risky volumes. (Founder, dogfood round 3,
  June 2026.)
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
- [x] **Roots changes propagate live across windows** — landed `6dab0f6`
  (batch-1 rail cluster): `add_root`/`remove_root` emit `roots-changed`
  (the `settings-changed` pattern); App listens → `refreshRoots()`.
  (Founder, dogfood round 2.)
- [x] **Add watched folder from the rail, one button click** — landed `6dab0f6`: "Add folder…" footer button + rail-folder context-menu `add-root` row, both opening the picker directly. (Founder, dogfood rounds 1+2.)

- [x] **Compose entries from the journal panel** — landed `506d81a` (batch-1 journal cluster): inline composer in the Journal tab (quiet textarea + rating binding; its focus joins the Esc text-edit layers). (Founder, dogfood round 2.)
- [x] **Journal entries show sibling targets** — landed `506d81a`: "+N
  others" quiet mark (`siblingTargetsLabel`), targets surfaced on the
  journal DTO. (Founder, dogfood round 1.)
- [x] **Select images from note** — landed `506d81a`: `select-journal-targets`
  row affordance + journal-row seat (jump home + select the entry's full
  target set). Availability: every entry kind except redacted stubs (B59).
- [x] **Backend `journal-changed` event** — landed `506d81a`: carries
  affected hashes; journal panel, grid badges, and the Look overlay
  refresh off it (the indicator pulse is pure feedback again).

- [x] **RAW 1:1 via the embedded full-res JPEG** — landed `1cbf7ad`
  (batch-1 raw cluster): `/embedded` route serves the RAW's embedded JPEG
  at native size with the preview's exact §9.3.1 orientation policy
  (strokes stay put at deep zoom); ladder is /original → /embedded →
  preview stands. True decoded 1:1 stays M1.5.
- [x] **Esc keeps the inspector on Look→Grid** — landed `506d81a`: the
  inspector layer peels AFTER Look→Grid (returning to the grid keeps the
  panel on the still-active image). Multi-select display resolved by B60:
  anchor image + quiet "N selected" (`64b220e`).
- [x] **Filmstrip pushes, doesn't overlay** — landed `ca5c9a7` (batch-1
  look cluster): the filmstrip moves the Look viewport up rather than
  covering it (deliberately opposite the rail's I1 overlay convention —
  Look's canvas is the one surface where covered pixels matter).
  (Founder, June 2026.)

- [ ] **Full metrics suite across every pipeline stage** — when the product is feature-complete, instrument each step (ingest passes, hash/preview throughput, search latency, fold cost, capture/binding latencies, overlay render, IPC round-trips) into one coherent metrics surface (debug panel growing into a perf dashboard); founder wants "blazing fast" to be measured, not vibes. (Founder, June 2026.)

## M1.5 (scheduled concept, not yet a packet)

- [ ] Full RAW decode backfill pass (rawler/libheif worker; queue already
  knows the pass kind) — unlocks HEIC previews + RAW 1:1 zoom.
- [ ] Preview-policy settings (which previews to build/keep; LrC-style
  "build 1:1 on demand, discard after N days" knobs) — founder asked for
  exposure of these as toggles eventually.

## Milestone-attached extras (build with their milestone)

- **M2a (pencil) — P5.1 SHIPPED** (`1e06f1e`): B/E/O keys, overlay, undo/eraser, journal stroke micro-previews. The toolbar idea is ruled out for good — zero-chrome wins (U14); the old P/E/V band is retired. Review-sourced polish below:
- [x] Pencil: jitter-dedupe baseline recomputed on transform change (wheel-zoom mid-stroke) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: button-0 gate evaluated before eraser intent — middle/right-click with E held no longer erases or pre-empts the look-backdrop menu — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: PencilOverlay consumes the shared ui.look spaceHeld slice (eraserHeld precedent); the one tracker lives in LookStage behind stageOwnsRawKeys (+ the Space-at-fit close fix, `ffbd515`) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: "Undo stroke" row on the look-backdrop seat (enabled: pencilUndoable) replaces the keyboard-only exemption — landed `ca5c9a7`. (P5.1 review.)
- [ ] Pencil: one-euro live-stroke filter (CAPTURE §8.3 MAY) — add only if real-pen dogfood shows live wobble. (P5.1, DOGFOOD-M2.)
- [x] Pencil: terminal pen-up sample (dedupe-exempt) to make ts − t_last exact for held dots — founder-resolved, landed with P6.1 (B41). 
- **M2b (voice) — P6.1 engine (`9a5eece`) + P6.2 runtime (`fd0adc8`) SHIPPED**: sessions/scope ring/VAD-onset binding/voice pipeline/corrections/linking, mock/stub-verified (supervisor, downloads incl. byte-zero license gate, tiers, scheduler, consent card, OpenAI-compatible + sherpa-WS clients); M-key mic row still reserved — un-reserving needs the real arm path (P6.3). All eight P6.1→P6.2 wiring obligations below closed by P6.2:
- [x] P6.2: reconcile the two ASR-readiness ctx flags — asrReady (hardcoded false) vs the live asrUnavailable — when supervision lands. (P6.1 review.)
- [x] P6.2: session rotation must re-point an attached CaptureEngine at the newly opened session (shell attaches NoCapture today; currently an undocumented caller burden). (P6.1 review.)
- [x] P6.2: move AudioFeed out of photoproof-connectors' mock namespace — the production engine imports its audio inlet from mock:: (plumbing, not mock behavior). (P6.1 review.)
- [x] P6.2: the shell's real bounded 5 s drain wait at quit (the engine enforces the deadline on its clock; the pump loop owns the blocking wait). (P6.1, B52.)
- [x] P6.2: drain deadline only bites on Poll::Pending — ready finals past the cap still mint and a never-pending stream defeats it; harden against the real stream. (P6.1 review.)
- [x] P6.2: cfg-gate partial text out of the release debug-note ring (§6.5 makes partials dev-build debug territory; today the bounded in-memory ring holds text in all build configs). (P6.1 review.)
- [x] P6.2: pin §6.4's "ArmedSpeaking holds while any utterance is in flight" with a test — a guard-removal mutant currently survives. (P6.1 review.)
- [x] P6.2: close processors run synchronously inline on the close/quit path — fine while the registry is empty, but §2.5 says step 3 never blocks; move onto the pump before real processors register. (P6.1 review.)
- [ ] M2b: hold-to-talk duality; journal-changed event (above) becomes load-bearing.
- **M3 (retrieval/projects)**: rail source-list grows projects + saved
  searches; drag-selection-to-rail filing; query-residue indicator segment
  with one-key clear; chip-creation UI (parser-driven); select-from-note ↔
  project filing workflow chain.
- **M3 north star (founder)**: ONE unified retrieval system across all
  surfaces — toggles, filters, and sorting modes power users can configure
  precisely, over an excellent zero-config default where a quick search
  just pops the right image. Power-user depth must never tax the quick
  path (the <100 ms as-you-type budget and quiet defaults are the floor).
- **Stroke-aware retrieval (founder + design, pre-M3)**: strokes are
  already searchable via has_strokes (built), the stroke↔utterance link
  (K9 — words spoken while drawing find the stroke; provenance carries
  linked_stroke), and stroke provenance in results. NEW: (a) gesture
  semantics — classify stroke geometry (circle/X/underline/arrow) into
  searchable intent ("images I X'd out"); raw points are stored, pure
  downstream consumer. (b) region-conditioned visual embeddings — embed
  the CIRCLED CROP, not the frame: visual search conditioned on where the
  photographer's attention went. Both M3+/M4 candidates.
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

- [ ] **Sidebar design pass** — both sidebars (rail/sources, inspector) deserve a deliberate future design round (layout, affordances, what lives where) once M3's source-list growth and the collection-view question land; for now only baseline functionality matters. (Founder, dogfood round 2.)

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
