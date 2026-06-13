# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis).

## Next polish round (small, founder-requested)

- [ ] **Voice chunking tuning** — first live run (June 2026) works end to
  end ("it is making finals and saving notes"), but utterance
  segmentation needs a deliberate tuning round against real dictation.
  The knobs, all in one place so the round is empirical, not archaeology:
  (a) server-side endpoint rules in `pp-asr-server` — rule2 1.2 s
  trailing silence after decoded speech (the main "when does a sentence
  end" feel), rule1 2.4 s, rule3 20 s max utterance; (b) the engine's
  `TRAILING_SHIP_MS` 3 s ship window (must stay > the rules it feeds);
  (c) silero hang `HANG_WINDOWS` 15 x 32 ms = 480 ms (gate flap vs
  intra-sentence pauses) and ENTER/EXIT 0.5/0.35 thresholds; (d)
  `asr.chunk_ms` config (160 ms default — latency vs throughput).
  Consider whether consecutive finals within a short gap on the SAME
  scope should merge into one journal entry (a capture-policy question,
  not a knob). THE TOOL EXISTS: `pp_voice_bench` (synth + run modes, all
  knobs as flags, --json for sweeps) — first sweeps bracket rule2
  between 0.6 (over-splits intra-sentence pauses) and 1.2 (merges 0.8 s
  thought-pauses); real tuning needs founder dictation clips (drop wavs
  in gitignored test-corpora/voice/). The harness's first catch — the
  engine's FIFO onset-association binding text to the WRONG onset when
  VAD and ASR disagree on segment count — is FIXED (B72: proximity
  association + merged-onset retirement + one stream clock,
  `8c2393b`/`6739de9`); the tuning round itself remains open. (Founder,
  first voice dogfood, June 2026.)
  TUNING ROUND 1 FINDINGS (June 12, founder-corpus-driven): cold-start
  first-word chop FIXED (engine pre-roll PRE_ROLL_MS 400, `cec8604`,
  verified on the corpus). Endpoint-tail truncation ("actually incred",
  "Kee[per]") is INVARIANT to rule2 (1.2/1.5/2.0), feed pacing
  (realtime vs fast), wire chunk size (50/160 ms), and pre-roll length
  - while flush-minted finals (disarm/Done path) always come back
  COMPLETE and raw ungated feeds through the SAME server emit full
  tails. Conclusion: something in the gated stream's content around
  the tail; NEXT FORENSIC: a --dump-shipped tee in pp_voice_bench
  (write exactly what the engine shipped to a wav; raw-feed that wav
  back - splits engine-content from server-behavior in one move).
  Mumble-zone mid-word dropouts ("fogens") are invariant to exit/hang
  knobs - likely model-level on quiet speech; quantify with the
  audiobook WER harness (below). pp-asr-server has an endpoint-grace
  mechanism (--endpoint-grace-ms + energy early-out) defaulted OFF:
  the corpus showed deferred resets clip the next word's start when
  pauses run short.
  RE-PRIORITIZED BY B74 (June 12): the truncation class root-caused to
  the export's baked-in lookahead (docs/SPIKE-ASR35.md) - the 560 ms pin
  swap supersedes further old-model pipeline forensics (dump-shipped tee
  et al now low-priority); chunking FEEL tuning (rule2, merge policy)
  remains live and applies to any model.

- [ ] **Roots and subfolders: the long-practice design round** (founder,
  June 2026): today the model is a FLAT list of watched roots, each with
  a navigable subfolder tree in the rail (folder_tree); fine at 3 roots,
  unexamined at 30. Questions for a deliberate round: nested or
  overlapping roots (adding a folder inside an existing root - refuse,
  merge, or alias?); whether the Folders tab should group roots (by
  volume? by year-shaped naming?); deep-tree ergonomics (lazy loading,
  filtering, jump-to-folder); root lifecycle (a root that moved volumes;
  archiving a root without losing its journal); and how the
  collections-first philosophy shapes how much folder UI we even want.
  Pairs with the sidebar design pass already logged under founder
  appetite. (Founder, June 2026.)
- [x] **B summons the overlay** — landed `c13f09b`: the key was dead
  twice over (the `pencil-pen` def gated on `overlayVisible`, and
  `togglePencil()` refused while hidden); now B with hidden paper shows
  the overlay AND arms the pencil in one keystroke (show-and-arm),
  visible-overlay toggling byte-for-byte unchanged. (Founder, June
  2026.)
- [ ] **Model-landscape survey** (founder, June 2026 - periodic): the
  toolchain is modular by seam, so every block deserves a recurring
  look at the leading alternatives: ASR, VAD, LLM, image embedder, text
  embedder, reranker. docs/MODELS.md is the living matrix; refresh it
  quarterly or when a release moves the frontier (the Nemotron 3.5 day
  proved the swap evaluation costs an afternoon).
- [ ] **Nemotron 3.5 upgrade watch** (B74): trigger = sherpa-onnx Rust
  crate release with 3.5 support (runtime landed in their master June
  12; official exports live at csukuangfj2/...-2026-06-11). Then: pin
  the 560 ms int8 export, wire the per-stream language option, rerun
  the voice corpus + Alice WER STREAMED, spike-style latency/RSS
  numbers. Brings native punctuation/capitalization + 40 locales.
- [ ] **Audiobook WER stress harness** (founder idea, June 2026): run a
  LONG known-transcript recording through the full pipeline - a LibriVox
  public-domain audiobook chapter (librivox.org) with its Project
  Gutenberg text. Gives three things the cards cannot: (a) word-error
  rate at scale, separating MODEL accuracy from PIPELINE truncation
  (score raw feed vs gated feed against the same transcript); (b)
  endurance - memory and drift over an hour of armed decode; (c) a
  fixed public corpus any machine reproduces. Recipe: fetch one chapter
  (solo reader, clean recording), afconvert to 16 kHz mono PCM16 into
  gitignored test-corpora/voice-long/, align the Gutenberg chapter
  text, add a WER scorer (sidecar script or a pp_voice_bench --expect
  upgrade). CORPUS FETCHED June 12: test-corpora/voice-long/ holds Alice
  ch1 (LibriVox v8 solo, 64+128 kbps -> 16 kHz wavs) + the exact
  Gutenberg transcript + caveats README; the scorer is the remaining
  piece. (Founder, June 2026.)
- [x] **Mid-ingest scroll stability** — landed: the scroll anchor pins
  the IMAGE (hash) across re-lists — when a re-sort moves it, the
  viewport follows it to its new offset (B64 applied to scroll); and
  scroll-focus-into-view keys on `focusNav` (bumped only by
  setSelection, the user-driven path), so a refresh's silent focus
  remap never yanks the viewport. (Founder, dogfood round 3, June
  2026.)
- [ ] **Import progressively: cards before hashes, previews in tiers** —
  big-folder import should SHOW something immediately: (a) discovery
  pass lists filenames and paints placeholder cards before hashing
  completes (needs a pre-identity card state — today an image exists
  only once hashed, K1; the card would carry the path until its hash
  arrives and the card re-keys), (b) a quiet per-card indicator while
  the preview builds (the previewReady placeholder is the seam — give it
  a subtle building shimmer instead of dead gray), (c) consider a
  low-res-first tier: a tiny embedded thumbnail (EXIF IFD1 ~160px) is
  readable in milliseconds even over SMB — paint it blurred-up, replace
  with the real 512px artifact when the preview pass lands. Performance
  work should be DRIVEN by pp-bench numbers (scripts/bench.sh), not
  vibes. (Founder, dogfood round 3, June 2026.)
  FRESH-INSTANCE DOGFOOD (founder, June 12, 2026) sharpened two more
  edges of the same flow — BOTH LANDED `d066fe8`: (d) instant scanning
  state — `ingestExpecting` optimistic bridge set synchronously on
  add-root/drop/rescan, cleared by the first real ingest event; the
  walk itself now reads as running (root cause was structural:
  scan_root walked the entire tree before any pass row existed, so
  `running` was false for the whole walk); (e) live discovered count —
  a per-file atomic counter on ScanOptions rides the existing
  ingest-progress channel; the empty state reads "Indexing — N
  photographs found so far…". Items (a)–(c) above (pre-identity cards,
  shimmer, low-res tier) remain open. The whole shebang remains the
  goal: add folder → instant "scanning" → live count → cards appear →
  previews fill in.
- [x] **The What's-Happening Station (indicator 2.0)** — landed
  `de9f126` (merge-fixed: the mic seat resolves `mic-press` arg
  "toggle", the def the M→Space move owns): pure StationModel
  (logic/station.ts) over existing state, collapsed icon row with one
  breathe driver, hover-expand via the indicator Popover (read-only
  body; icons are the only click targets), info seat pins via new
  `toggle-station-detail` row, pop-chips generalize the note pop to
  mic arm/disarm/"Captured". Founder manual pass pending: pulse/hover
  feel, chip stacking. Original riff: (founder, June
  12, 2026 — "Do you see what I mean?" riff, captured verbatim in
  spirit): evolve the bottom-right capture indicator into the app-wide
  LIVING STATUS ORGAN. Same corner, bigger presence. Two states:
  COLLAPSED = a quiet icon row (mic, magnifying-glass search,
  background-tasks/info dot, the note pencil), pulsing gently when
  something is happening; HOVER = the capsule expands large with real
  context (ingest/digest progress with counts, background task list,
  current scope, streaming utterance), shrinking back to icons on
  leave — counts move INTO the hover, off the always-on chrome.
  Events POP from the station: note creation already does (founder:
  "which is cool" — that's the signature move, keep it), mic
  arm/disarm and push-to-talk evidence join it, and searches could
  pop from there too (pairs with the search-as-scope direction).
  Each icon is a clickable seat with the expected verb: mic =
  toggle (the M tap twin), magnifier = focus search, info = expand
  the tasks view. Existing rulings carry forward: lights-out
  exemption (DECISIONS U5), scope-segment → inspector bridge, the
  note-input summon. This is likely WHERE the digest-visibility
  surface below lives — design the two together. (Founder, June
  2026.)
- [ ] **Digest visibility: a design round for "what is my library
  doing?"** (founder, fresh-instance dogfood, June 12, 2026): while a
  new folder digests, the only signal is the word "digest" in the
  header bar. A new folder kicks off a whole pipeline of background
  work — discovery walk, hashing, sidecar adoption, preview builds,
  embedding passes (CLIP + text once M3 lights up), and any model
  downloads those need — and the user has no way to see where the
  library IS in that pipeline, what remains, or what the app is waiting
  on. Needs a deliberate UX round, not another one-word status: a
  per-stage progress surface (counts done/total per pass), an
  at-a-glance "library is settled / library is working" state, and an
  answer for where it lives — LEADING CANDIDATE: the What's-Happening
  Station above (founder, June 12: hover-expanded task detail there,
  not always-on counts). Subsumes the header word as the COLLAPSED form of something
  expandable. Related: the progressive-import item above (the grid's
  half of the same story) and the model-download progress item below
  (same disease: real work invisible or misreported). (Founder, June
  2026.)
- [x] **Voice notes save a leading space** — landed `6ee8554`:
  `on_final` mints `seg.text.trim()` (edges only; interior spacing
  verbatim — §6.5 protects words from paraphrase, not BPE tokenizer
  plumbing); acceptance test pins " Slow  down " → "Slow  down".
  NOT taken: normalizing the handful of existing test-note rows
  (append-only journal; they're tonight's throwaway dictation).
  Original report (founder, June 12, 2026):
  every voice remark in the journal starts with a literal " " —
  CONFIRMED IN THE STORE, not a render artifact (sqlite:
  `[ Slow down]`, `[ We've got time left to be lazy]` …; typed notes
  unaffected). Root cause shape: BPE-style ASR tokens carry the
  word-boundary space, so an utterance's first token decodes as
  " Slow", and the engine mints the final without trimming. Fix at the
  final-minting boundary in the capture engine (trim leading/trailing
  whitespace before the journal event exists — whitespace is not "the
  user's words", K14 is safe); decide whether to also normalize the
  nine-and-counting existing rows (journal events are append-only —
  if normalization is wrong, a display-time trim for legacy rows is
  the honest fallback). Check the sidecar snapshots carry the same
  bytes. (Founder, June 12, 2026.)
- [x] **Desktop platform conventions pass** — landed `a0cac41` (audit
  found NO native menu existed): macOS menu bar App/File/Edit/View/
  Window with standard roles (Edit roles = ⌘C/⌘V in WKWebView fields;
  predefined Quit still exits through the sidecar-flush path), custom
  rows routed through the one action registry via a `menu-action`
  event (the menu is a fourth rendering of the action table); UI-scale
  zoom ⌘=/⌘−/⌘0 on a 0.8–1.5 ladder via webview setZoom, persisted
  (`pp.uiZoom`), distinct from Look's plain-key image zoom; keymap now
  forfeits ctrl+meta chords to the menu layer (⌃⌘F fullscreen no
  longer starved by ⌘F search). Founder manual smoke test pending
  (menus/zoom/Edit-paste/window verbs). Original ask:
  (Founder, June 12, 2026):
  all the things long-lived desktop apps just DO, audited and wired for
  macOS first: (a) UI-scale zoom on Cmd+= / Cmd+− / Cmd+0-to-reset —
  the webview zoom convention every Tauri/Electron app inherits (note:
  distinct from the existing image zoom in Look; UI zoom scales the
  chrome) — persist the chosen scale; (b) the window-management row:
  Cmd+W close window, Cmd+M minimize, Cmd+H hide (these come free with
  a proper native menu bar — audit ours for the standard App/File/Edit/
  View/Window menus and make sure every in-app action with a key also
  appears in a menu, which is also what makes them discoverable and
  remappable in System Settings); (c) Cmd+, opens settings (verify the
  existing open-settings binding uses it); (d) Edit-menu basics working
  in every text field (cut/copy/paste/select-all/undo); (e) sweep for
  the rest: double-click titlebar to zoom, full-screen Cmd+Ctrl+F (a
  toggle-fullscreen action exists — check the binding), text-field
  focus outlines. One pass, one checklist, so the app feels NATIVE,
  not webby. (Founder, June 2026.)
- [x] **Click feedback pass: every action acknowledges the click** —
  landed `d8a8658`: one global `button:active` rule (filter+transform,
  chosen because component-scoped background overrides would swallow a
  background-based press) gives every real button a pressed state; new
  `AckFlash`/`AckButton` primitives (copyflash idiom) give
  fire-and-forget verbs a truthful momentary done-label — Restart
  runtime ("Restarted") and Re-detect hardware ("Re-detected") adopted.
  Non-button clickables audited and deliberately left alone (selection
  surfaces already self-signal). (Founder, fresh-instance dogfood,
  June 12, 2026.)
- [x] **M key = push-to-talk on hold, mic toggle on click** — landed
  `2fbe2c9`: pure hold machine (`logic/michold.ts`, time-as-parameter),
  press arms immediately from disarmed (both gestures want sound from
  the keydown), release <250 ms = tap (arm stands), ≥250 ms = PTT
  (explicit disarm ships through the normal drain); from armed, tap
  disarms and hold is deliberately inert (an absent-minded hold never
  tears down a deliberately armed mic). Intents are explicit
  arm/disarm via a new idempotent `set_mic` command — never blind
  toggle. Auto-repeat absorbed; window blur resolves a gesture-opened
  mic, leaves a pre-armed one alone. (Founder, June 12, 2026.)
  SUPERSEDED same night (founder: "like a Zoom call"): the mic moves
  to SPACE — tap toggles, hold is push-to-talk; M is freed back to
  the reserved pool; Space's old verbs displaced (open-Look keeps
  Enter, Look-close keeps Esc, zoomed hold-Space pan dies — drag-pan
  remains). LANDED `e486023`; the hold machine itself was unchanged,
  and §11 input suppression already covered Space (the rule keys on
  "the chord can type", not "single letter").
- [x] **Model download progress must be model-cumulative** — landed
  `ab1369a`: core's download loop carries a `base` accumulator so every
  DownloadProgress event is model-cumulative (the per-file completion
  event is what advances the row through DFN5B's ~290 sub-coalescing
  shards); enqueue seeds from `downloaded_bytes(model)` (statted before
  the host lock) so a resume opens at its true bytes; the dead `last`
  fold deleted. Original diagnosis (founder, fresh-instance dogfood,
  June 12, 2026 — caused two separate "it didn't resume / stuck at
  zero" impressions in one evening while downloads were in fact
  healthy; founder's actual bar: "look and feel modern"): two
  compounding display defects on the settings model rows. (a) `DownloadProgress` bus events carry the CURRENT FILE's
  bytes (core download.rs publish sites), but the row divides by the
  whole model's total (runtime.rs status ~336) — DFN5B is 400 files,
  ~290 of them tiny shards, so the displayed number sits at ~0% while
  gigabytes land verified on disk. (b) clicking Download seeds
  `state.downloads` with `(0, total_bytes)` (enqueue_downloads ~525), so
  a resume of a 1 GB part file FIRST displays "0 bytes" — reads as
  progress thrown away. Fix shape: publish cumulative model bytes from
  core's per-model loop (it knows the model), or seed/fold in the
  manager's `downloaded_bytes(model)` baseline host-side; the discarded
  `last` fold in run_download (~597–623, `let _ = last;`) is a vestige
  of the same seam. One number, one meaning: bytes of THIS MODEL on
  disk over its manifest total.
- [x] **Auto-retry interrupted model downloads** — landed `ab1369a`:
  `run_download` retries the `Interrupted` class ONLY, 4 more attempts
  at 2/5/15/30 s backoff (sliced sleeps against the stop latch so quit
  mid-backoff returns within a beat), row stays "downloading" with a
  `retry_hint` ("connection interrupted — retrying (attempt 2 of 5)")
  until exhaustion; checksum/license/HTTP errors still fail fast.
  NOT taken (still open if wanted): resume-on-launch for models with a
  part file + recorded acceptance. (Founder, fresh-instance dogfood,
  June 12, 2026 — interruptions hit 3× in one evening.) — from the grid or from Look, click-
  drag an image out of the window and drop it into Finder/another app as
  the ORIGINAL file (a native OS file drag carrying absolute paths — the
  D4 reveal/open-with class of OS integration, not an in-app file verb;
  D3 stands: the library never moves or deletes its own files, the drop
  target copies). Implementation pointers: Tauri needs a native start-
  drag (HTML5 dragstart cannot carry real files out of a webview) —
  tauri-plugin-drag (CrabNebula) or NSDraggingSession/NSFilePromise via
  the window handle on macOS. Sub-questions to decide at build time:
  a multi-select drag carries the whole selection; does a collapsed
  RAW+JPEG pair drag both members or the display member (lean: both —
  the pair is one image to the user, and a half-exported pair is the
  kind of silent data loss the welcome card warns about); offline-volume
  images can't drag (no readable path) — quiet refusal, no toast spam.
  (Founder, dogfood round 3, June 2026.)
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
- [ ] **Stronger storage story beyond the welcome card** — the residue of
  the welcome-card item: hash-keyed sidecar recovery sweep,
  case-insensitive-filesystem rename semantics (APFS: a case-only rename
  isn't a rename; s02_2 fails on macOS today), import-time warnings on
  risky volumes. (Founder, dogfood round 3, June 2026.)
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
- **M3 (retrieval/collections)**: rail source-list grows collections + saved
  searches; drag-selection-to-rail filing; query-residue indicator segment
  with one-key clear; chip-creation UI (parser-driven); select-from-note ↔
  collection filing workflow chain.
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
  <100 ms FTS path. **M3 design decision to make**: when collections become
  browsable grids ("collection view"), does search turn contextual — e.g.
  a right sidebar scoped to the collection — instead of the full-canvas
  destination? (Tension: the right edge is reserved for journal/partner;
  founder suspects he'll want search-as-sidebar there. Decide at M3 design
  time, not before.) Full-canvas search stands until then.
- **M4 (time)**: Look bottom-edge stroke scrubber (seat reserved); journal
  timeline rendering upgrade; trajectories as an alternate grid lens.
  - **Library-wide event timeline** (founder, June 2026): a view of WHEN
    annotation activity happened across ALL folders — every event is
    db-stored with ts + session, so this is a query + rendering problem,
    no new capture machinery: sessions as spans, events as marks, click
    lands on the image/journal. Natural M4 fit (it IS the time milestone);
    consider it the journal-timeline upgrade's library-level sibling.
- **M5 (partner)**: right-edge dockable panel sharing the inspector slot;
  summon key reserved; obeys Tab lights-out unconditionally.

## Lighting up M3 (the semantic-search chain, in order)

- [x] **Embedder bake-off (MacBook half)** — DONE June 12 2026 (B73,
  docs/SPIKE-P7-EMBED.md): text = EmbeddingGemma-300m q8 (chosen),
  Qwen3-Embedding-0.6B int8 alternative; image = DFN5B confirmed
  (founder call + feasibility numbers + eye-verified zero-shot). All
  SHAs pinned in the report; integration traps recorded.
- [ ] **Real embedder connector + backfill packet**: implement the
  Embedder seam against the pinned models (RUNTIME process or in-process
  ort, per spike findings), let the existing P7.1 embedding passes chew
  through the library, flip STATUS.md's mock-only retrieval rows live.
- [ ] **Spike session 2, desktop half** (needs the RTX 5080 machine):
  tier-2 throughput calibration, CUDA posture, the full RUNTIME 12.4
  concurrency matrix.
- [ ] **Golden-query retrieval eval** (post-dogfood, M3 quality gate):
  founder-built query set over his real annotated library; settles S4
  always-on weight (B69) and the reranker go/no-go.

## Collections (B71 — the M3 curation thread)

- [x] **Rail: Folders vs Collections tabs — first slice** — landed
  `98e3cb5`/`d92bd29` (Phase 7): peer tabs in the rail, collection list
  with create + click-to-view (grid shows current members), add/remove
  membership on the image context menu, welcome copy reframed
  (collections are the point; folders are mechanical). REMAINING for the
  design round: the full encouragement UX and autosuggest (below).
  (Founder, June 2026.)
- [ ] **Collection-note composer (UI slice)**: the storage, merge rules,
  and commands (add_collection_note / collection_notes) landed with
  P7.3 - collections carry their own append-only notes, a deliberately
  separate kind from image journal events (about the grouping's intent,
  not any image). Missing: the composer - a notes area when viewing a
  collection in the rail tab, possibly a "note the collection" verb
  while its grid is open. (Founder, June 2026.)
- [ ] **Collection-level rollups from member notes (LLM)** - founder
  idea, June 2026; posture split to respect K14 ("machine prose is
  retrieval fuel only; the journal preserves YOURS"): (a) FUEL TIER,
  uncontroversial: LLM-derived collection summaries, invisible,
  search/context only - "find that melancholy series" works without
  visible machine prose; (b) NUDGE TIER: surface quiet observations
  ("seven of twelve notes here mention fog") that invite the USER to
  write the collection note - machine notices, human authors; ties into
  the encourage-collecting principle and autosuggest below. AVOIDED by
  recommendation: machine-drafted notes entering the store as content,
  even behind an accept button - search provenance would quote words the
  photographer never said. FOUNDER CALL pending on whether (b) ever
  graduates toward drafting.
- [ ] **Autosuggest collections** (founder, June 2026): the app should
  NATURALLY encourage collecting — that is the point of gathering all
  this disparate context. Beyond manual creation, propose collections
  quietly from signals the app already has: images co-annotated in one
  session, repeated phrases across voice/typed notes, time+folder
  affinity, search queries the user runs repeatedly. Surface as a quiet
  suggestion (never a modal); accepting one creates the collection with
  evented membership. Needs a design round — record signals first,
  suggest later is a legitimate v1 (the membership tables make late
  suggestions retroactively useful).

## Decided, awaiting founder appetite

- [x] **Layout architecture design round: canvas-centered, everything
  resizable** — landed `c12a90c`: one `Panel` primitive (drag-resize
  with pointer capture, double-click-resets, min/max clamps, sizes
  persisted globally under pp.panel.*), canvas-centered shell (flex
  [rail][center 1fr][inspector], center = [canvas][filmstrip] so the
  filmstrip is canvas-width by construction), Tab snapshot-restores
  exactly what was open (DECISIONS exemptions preserved), F total in
  both surfaces (the "works sometimes" gate was scope:"look"), rail
  resize root cause was an $effect that re-read size every drag frame
  and snapped back. Founder manual pass pending: drag feel, filmstrip
  width tracking, traffic-light lockstep. FIRST DOGFOOD FIX `8e24911`:
  the launch filmstrip rendered a fixed 17-neighbor window ("only loads
  17 images… doesn't fill the width") — now a virtual horizontal list
  over the whole order, selected photo centered with the founder's
  override rule (manual scroll holds until the next selection snaps
  back). (Supersedes the narrower "sidebar design pass" from
  dogfood round 2; founder, fresh-instance dogfood, June 12, 2026):
  rethink the BASE LAYERS of the app layout. The principle: the canvas
  (grid/Look) is the center section ALWAYS, regardless of which
  top/bottom/left/right bars are open; every bar is a peer panel with
  the same contract. Concretely from tonight's annoyances: (a) the left
  rail can't be click-drag resized — all four edges' panels should be
  drag-resizable; (b) the left rail and right inspector have visibly
  different UX (affordances, headers, toggle behavior) — one panel
  system, two instances; (c) the filmstrip doesn't extend the full app
  width (and shouldn't depend on what else is open — see the canvas
  principle); (d) F sometimes opens the filmstrip and sometimes doesn't
  — find the contextual gate (or focus dependence) and either make it
  total or make the WHY visible; (e) interaction contract: each panel
  gets its individual toggle, AND the Tab global hide-everything stays
  (it works today and feels right). Layout state (sizes, open/closed)
  persists. This is an architecture round first (the panel/dock layer),
  then reconcile the existing rail/inspector/filmstrip into it.
  (Founder, June 2026.) FOUNDER CALLS (June 12, 2026 — build in
  flight): filmstrip spans the CANVAS width (bottom of the center
  column, dynamic as side panels toggle); panel sizes persist
  GLOBALLY (one size per panel, not per-surface); Tab lights-out
  restores WHAT WAS OPEN (snapshot at hide); F toggles the filmstrip
  in BOTH grid and Look.

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
auto-hide chrome · keyword taxonomies (collections are intent groupings with
evented membership — "tags with time" — never hierarchical vocabularies).
