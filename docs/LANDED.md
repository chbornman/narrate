# LANDED — shipped from the backlog

The archive half of BACKLOG.md: every `[x]` item moves here verbatim once it
ships, commit hashes, root causes, and founder context intact — this is the
de facto changelog of backlog-sourced work. Open work stays in BACKLOG.md;
this file only grows. Organized by era, newest first; older entries keep
their original wording.

## June 12 2026 — the dogfood waves (rounds 1–3: wave2 polish, batch-1 clusters)

- [x] **Mid-ingest scroll stability** — landed: the scroll anchor pins
  the IMAGE (hash) across re-lists — when a re-sort moves it, the
  viewport follows it to its new offset (B64 applied to scroll); and
  scroll-focus-into-view keys on `focusNav` (bumped only by
  setSelection, the user-driven path), so a refresh's silent focus
  remap never yanks the viewport. (Founder, dogfood round 3, June
  2026.)

- [x] **Pair targets vs "+N others"** — landed `wave2/polish` (B61:
  suppress, the stack badge already says it): `siblingTargetsLabel`
  gains the inspected image's pair-mate and never counts it — the mark
  shows only for genuinely DIFFERENT images; `GridSlice.pairMateOf`
  resolves the mate (collapsed alt or expanded partner cell), JournalTab
  threads it down. (Founder, dogfood round 3, June 2026.)
- [x] **"Rebuild previews…" on the rail folder menu** — landed `8755af1`:
  `Library::rebuild_previews(root_id)` re-pends the preview pass for every
  image with an active path under the root, fresh budget, backfill
  priority (the generator_version machinery's manual trigger; regeneration
  overwrites idempotently, §9.8); rail-folder seat
  row right after Rescan. Becomes more load-bearing with M1.5
  preview-policy knobs. (Founder, dogfood round 3, June 2026.)
- [x] **First-run welcome card: how your data is stored** — landed
  `8755af1`: WelcomeCard modal on launch — sidecars
  (`.photoproof.json`, SIDECARS §2.1) live beside images, ARE the data,
  and are filename-specific (outside-the-app renames lean on the §7
  relink heuristics); the index is rebuildable. "Don't show again"
  toggle (default ON) via prefs.ts; escape layer 1; redaction-modal
  frame/focus pattern. (Founder, dogfood round 3, June 2026.)
- [x] **Header shows background jobs** — landed: `IngestStatus` now
  carries a per-pass-kind `passes` breakdown (pending+running, versions
  summed — pure surfacing of `pass_counters` over the existing
  `ingest-progress` channel), and the titlebar shows one dim word
  ("digesting") while ANY kind has queued work; count + kind live in
  the hover title ("Still digesting — hashing 12 · building previews
  480"), never a progress bar. Ingest, preview rebuilds, doctor
  re-pends, and the M3 embedding/caption backfills all flow through
  `ingest_passes`, so the register covers every background job by
  construction (logic/jobs.ts maps queue names to reviewer words;
  unknown passes surface verbatim). The §7.5 indicator hairline keeps
  the fraction. (Founder, June 2026.)
- [x] **Copy actions confirm themselves** — landed: ONE register, the
  icon-to-check flash (toasts stay spec-capped at three triggers,
  UI §7.5/R5, so the confirmation lives AT the affordance).
  `primitives/copyflash.svelte.ts` is the shared seam: every copy
  affordance writes through `copyToClipboard(key, text)` (the one
  webview-fallback clipboard path now) and renders a brief Lucide check
  while `copyFlash.key` matches — truthfully, only after the write
  landed. Applied everywhere copy exists today: the Metadata tab's
  hash/path glyphs flash to a check; the thumb menu's "Copy file path"
  row (def-level `copyConfirm` flag → row `flashKey`) shows the check
  and holds the menu open ~900 ms so the confirmation has a seat.
  Future copy verbs join by setting `copyConfirm` on their def.
  (Founder, dogfood round 3, June 2026.)

- [x] **Library doctor / self-check pass** — v1 landed `8755af1`:
  `Library::doctor()` re-pends done preview passes whose
  artifacts are missing on disk, COUNTS orphaned stale path rows (no
  deletion — conservative by charter), sweeps stranded preview temp
  files; runs on the maintenance tick and as the debug panel's [dev]
  doctor; `info!`s the report when nonzero. v2 candidates remain:
  half-ingested RAW+JPEG pairs (one member's passes dead) → re-enqueue
  the laggard; marker/identity drift → report; stale-orphan sweep. Born
  from dogfood round 3's mangled-folder session: the offline-defer fix
  (`l13_08`) removes the biggest poison source, but mangled states will
  keep happening and the library should HEAL, not just avoid. (Founder,
  June 2026.)
- [x] **Grid: recycled `<img>` can flash the previous image's pixels** —
  landed `wave2/polish`: both loaded-marking paths in Thumb (the
  complete-check effect and onload) now prove via `currentSrc` that the
  element holds THIS hash's bitmap (`srcHash` in ipc/urls.ts) — a
  recycled img stays at the opacity-0 placeholder until the new hash's
  first load; stale complete/naturalWidth and in-flight load events for
  the previous occupant can no longer re-mark it. (P5.1-polish review
  residual, June 2026.)
- [x] **Zoom centering + pan clamp** — landed `652c839` (clampOffsets in
  carryOver; per-axis centering + edge clamp). (Founder, dogfood round 1.)
- [x] **Search entry as overlay, results as canvas** — landed
  (wave2/search): `/` floats the input over a dimmed, pointer-inert
  scrim (visual only — Esc remains the one return path, Sheet's scrim
  contract stands); results expand to the full canvas as they arrive,
  zero-results stays a quiet line in the panel; the contact-sheet
  contracts (selection/write-scope/Look, return point, Esc layers) are
  unchanged. (Founder, dogfood round 2.)
- [x] **Adopt Lucide icons** (`@lucide/svelte`) — landed (wave2/lucide):
  ad-hoc glyphs (🔍 from the spec mockup, sort ▾, ⏏, ×, chevrons, titlebar
  buttons) replaced with the Lucide stroke set, sized per-site (12–16 px)
  and toned via the existing tokens (icons inherit currentColor). Lucide
  ships no eject, so the offline-volume ⏏ became Unplug. UI.md §5 mockup
  emoji is illustrative, not normative. (Founder, dogfood round 2.)
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

## June 12 2026 — lighting up M3

- [x] **Embedder bake-off (MacBook half)** — DONE June 12 2026 (B73,
  docs/SPIKE-P7-EMBED.md): text = EmbeddingGemma-300m q8 (chosen),
  Qwen3-Embedding-0.6B int8 alternative; image = DFN5B confirmed
  (founder call + feasibility numbers + eye-verified zero-shot). All
  SHAs pinned in the report; integration traps recorded.

- [x] **Rail: Folders vs Collections tabs — first slice** — landed
  `98e3cb5`/`d92bd29` (Phase 7): peer tabs in the rail, collection list
  with create + click-to-view (grid shows current members), add/remove
  membership on the image context menu, welcome copy reframed
  (collections are the point; folders are mechanical). REMAINING for the
  design round: the full encouragement UX and autosuggest (below).
  (Founder, June 2026.)

## M2b voice — the P6.1 → P6.2 wiring obligations

All eight closed by P6.2 runtime (`fd0adc8`); recorded at P6.1 review, retired
as a set:

- [x] P6.2: reconcile the two ASR-readiness ctx flags — asrReady (hardcoded false) vs the live asrUnavailable — when supervision lands. (P6.1 review.)
- [x] P6.2: session rotation must re-point an attached CaptureEngine at the newly opened session (shell attaches NoCapture today; currently an undocumented caller burden). (P6.1 review.)
- [x] P6.2: move AudioFeed out of photoproof-connectors' mock namespace — the production engine imports its audio inlet from mock:: (plumbing, not mock behavior). (P6.1 review.)
- [x] P6.2: the shell's real bounded 5 s drain wait at quit (the engine enforces the deadline on its clock; the pump loop owns the blocking wait). (P6.1, B52.)
- [x] P6.2: drain deadline only bites on Poll::Pending — ready finals past the cap still mint and a never-pending stream defeats it; harden against the real stream. (P6.1 review.)
- [x] P6.2: cfg-gate partial text out of the release debug-note ring (§6.5 makes partials dev-build debug territory; today the bounded in-memory ring holds text in all build configs). (P6.1 review.)
- [x] P6.2: pin §6.4's "ArmedSpeaking holds while any utterance is in flight" with a test — a guard-removal mutant currently survives. (P6.1 review.)
- [x] P6.2: close processors run synchronously inline on the close/quit path — fine while the registry is empty, but §2.5 says step 3 never blocks; move onto the pump before real processors register. (P6.1 review.)

## M2a pencil — P5.1 review polish (P5.1 shipped `1e06f1e`)

- [x] Pencil: jitter-dedupe baseline recomputed on transform change (wheel-zoom mid-stroke) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: button-0 gate evaluated before eraser intent — middle/right-click with E held no longer erases or pre-empts the look-backdrop menu — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: PencilOverlay consumes the shared ui.look spaceHeld slice (eraserHeld precedent); the one tracker lives in LookStage behind stageOwnsRawKeys (+ the Space-at-fit close fix, `ffbd515`) — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: "Undo stroke" row on the look-backdrop seat (enabled: pencilUndoable) replaces the keyboard-only exemption — landed `ca5c9a7`. (P5.1 review.)
- [x] Pencil: terminal pen-up sample (dedupe-exempt) to make ts − t_last exact for held dots — founder-resolved, landed with P6.1 (B41). 
