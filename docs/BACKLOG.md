# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis). Shipped items move to LANDED.md verbatim — only open
work lives here.

## Dogfood round 4 (founder, June 12 2026 evening — second live session)

- [ ] **Search ranking is rank-flat: any note outranks a perfect CLIP
  match** (founder, THE headline bug): "ANY saved note in the image
  journal is outranking even perfect semantic visual clip search."
  ROOT CAUSE FOUND (`search/hybrid.rs` FusionWeights): weighted RRF
  with k=60. S2 (note keyword FTS) and S1 (note own-words vectors) are
  weight 1.0; S4 (image_clip visual) is weight 0.5. Because RRF scores
  by RANK not similarity — score = weight / (60 + rank) — an image
  ranked #1 by a weak note keyword hit scores 1.0/61 = 0.0164 and a
  PERFECT CLIP visual match ranked #1 scores 0.5/61 = 0.0082, so the
  note ALWAYS wins regardless of how strong the visual match is or how
  weak the note hit. The 0.5 CLIP weight (B69: "protected by WEIGHT not
  exclusion") was a spec default explicitly flagged as "data not
  findings, the §12 golden-set eval is the named gate." This is that
  gate arriving via dogfood. Two moves, likely both: (a) re-weight —
  CLIP visual should not sit at half a note's vote when the query is
  visual; consider raising S4 or making weights query-shaped (a
  visually-descriptive query leans S4, a "what did I say about…" query
  leans S1/S2); (b) RRF's rank-flatness is itself the deeper culprit —
  a near-miss and a perfect match at the same rank score identically;
  consider a similarity-aware fusion or a score-floor so a high-cosine
  CLIP hit can't be buried under a tangential keyword brush. PAIRS WITH
  the search-as-scope UI overhaul the founder asked to start now (see
  "Lighting up M3" + the search-scope riff) — the relevance-sort and
  per-signal toggles from that design make the weighting VISIBLE and
  tunable by the user, not just an invisible constant. (Founder, June
  12 2026.)
- [x] **Backend logs to a file** — landed `6c1f44b`: fresh
  file per `tauri dev` launch (founder preferred over rotating) at
  `<app_data>/logs/photoproof.log`, installed in `lib.rs::install_logging`
  (console + truncate-on-start file sharing one env filter). Recorded
  in CLAUDE.md as the first-class debug surface. NOT done: folding the
  stray `eprintln!`s into tracing; surfacing the path in settings.
  ORIGINAL ASK:
  (founder asked; also: the
  assistant can't see runtime behavior without it): `lib.rs` installs
  a `tracing_subscriber::fmt()` to STDERR only (`info` default,
  `photoproof_core/desktop=debug`), plus scattered `eprintln!`s
  (mic.rs, pump.rs, state.rs, embedders.rs). Nothing persists, so a
  crash/jank is unreviewable after the fact. Add a file layer
  (`tracing-appender` non-blocking rolling appender) writing to the
  app-data dir (e.g. `<app>/logs/photoproof.log`, daily roll, keep N);
  keep the stderr layer for `tauri dev`. Fold the stray `eprintln!`s
  into `tracing` while there so one sink captures everything. Surface
  the log path in the debug panel / settings for "reveal in Finder."
  (Founder, June 12 2026.)
- [ ] **"154 RAWs left to decode" reads as stuck — it's an UNBUILT pass,
  not a stall** (founder: "154 raws left to decode that seem stuck").
  DIAGNOSED: `ingest_passes` has 154 `full-raw-decode` rows in state
  `pending`, `attempts=0`, no error — they were enqueued and NEVER
  claimed, because `ingest::claim_next` drains only `Exif` + `Preview`;
  `full-raw-decode` is M1.5 and has NO worker yet ("stay pending in the
  queue by design"). So nothing is broken — but the UI advertises a
  count of work that will never move until M1.5 ships, which reads as a
  hang. Fix is honesty, not a decoder (unless M1.5 graduates now): stop
  surfacing pending counts for passes that have no worker, or label
  them "available in a future version," not "left to decode." (Same
  root cause as the DNG item below.) (Founder, June 12 2026.)
- [ ] **DNG (and other RAW) never loads a 1:1 preview** (founder:
  "Embedded preview — full decode pending… a dng file never loads
  1-to-1 preview"). SAME ROOT CAUSE as the stuck-RAW item: the 1:1
  view needs a full demosaic, which IS the `full-raw-decode` M1.5 pass
  — unbuilt, never claimed, so "full decode pending" is permanent. The
  embedded preview (the in-RAW JPEG) loads; the true 1:1 cannot until
  the decode pass exists (`preview.rs` already enqueues it at backfill
  priority and notes the CR3 HDR-PQ / chained-JPEG ladder it would
  feed). DECISION NEEDED: graduate the M1.5 full-RAW-decode pass now
  (rawler demosaic → 1:1 artifact), or make the UI stop promising a 1:1
  that won't arrive. For DNG specifically, verify rawler's DNG path and
  whether a larger embedded preview exists to show meanwhile. (Founder,
  June 12 2026.)
- [x] **Add-to-collection from the grid offers "New collection…"** — landed `589a0fd`: new `new-collection-add` thumb seat (available even at zero collections), captures targets synchronously, reuses the rail's inline name input (one create UX), runs create-then-add in order; blank name leaves nothing empty.
  ORIGINAL ASK:
  (founder: "if I right click on image(s) in grid, I want to add to a
  collection even if none exists / add to new collection"). Today the
  thumb context menu's add-to-collection only lists EXISTING collections
  (`collectionRows` over the current set); with zero collections there's
  no path, and you can't mint one from the selection. Add a "New
  collection…" item to the add-to-collection submenu that creates the
  collection AND adds the current selection in one evented step (the
  rail already has an inline "New collection…" creator —
  `SourceRail.svelte` — reuse its create path, then chain
  add-to-collection). This is also the natural feeder for the
  autosuggest/encourage-collecting thesis. (Founder, June 12 2026.)
- [ ] **Grid right-click submenus are janky** (founder: "submenus don't
  stick out the side, don't always open/close smoothly"). The whole
  context menu is `ContextMenuHost.svelte` (a 1 KB stub) — submenus
  (add-to-collection, surround, etc.) don't flyout to the side and
  open/close unreliably. Needs a real submenu implementation: side
  flyout with edge-aware flipping (open left when the right edge is
  near the viewport), hover-intent open/close with a small close delay
  so diagonal travel into the submenu doesn't dismiss it, keyboard
  arrows. Likely wants a small reusable Menu primitive rather than
  more ad-hoc positioning. (Founder, June 12 2026.)
- [ ] **T cell-info should grow the cell, not overlay the image; info at
  the TOP** (founder). Today the cell-info row (`cellinfo.ts` cycled by
  T) is `position: absolute` over the bottom of the thumbnail
  (`Thumb.svelte` ~234), covering the image. Founder wants: when info
  is shown, the cell EXTENDS DOWNWARD to make room (image stays fully
  visible, info sits in its own strip below — or per the founder, info
  at the TOP of the cell). Touches the grid layout math (cell height
  becomes image + info-strip when active) and the gridlayout row-height
  calc, not just Thumb CSS. (Founder, June 12 2026.)
- [ ] **No em-dashes in UI copy** (founder, emphatic: "no emdashes in the
  UI!!!"). Sweep user-VISIBLE strings (EmptyState lines, button labels,
  settings copy, station/indicator text, welcome/consent cards,
  tooltips) and replace `—` with " - " or a rephrase. ~408 `—` occur in
  the frontend but MOST are code comments — target only rendered text;
  do not touch comments or this backlog. Consider a tiny lint (grep gate
  in CI over .svelte template regions / string literals) so they don't
  creep back. (Founder, June 12 2026.)

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
- [ ] **Stronger storage story beyond the welcome card** — the residue of
  the welcome-card item: hash-keyed sidecar recovery sweep,
  case-insensitive-filesystem rename semantics (APFS: a case-only rename
  isn't a rename; s02_2 fails on macOS today), import-time warnings on
  risky volumes. (Founder, dogfood round 3, June 2026.)

- [ ] **Full metrics suite across every pipeline stage** — when the product is feature-complete, instrument each step (ingest passes, hash/preview throughput, search latency, fold cost, capture/binding latencies, overlay render, IPC round-trips) into one coherent metrics surface (debug panel growing into a perf dashboard); founder wants "blazing fast" to be measured, not vibes. (Founder, June 2026.)

## M1.5 (scheduled concept, not yet a packet)

- [ ] Full RAW decode backfill pass (rawler/libheif worker; queue already
  knows the pass kind) — unlocks HEIC previews + RAW 1:1 zoom.
- [ ] Preview-policy settings (which previews to build/keep; LrC-style
  "build 1:1 on demand, discard after N days" knobs) — founder asked for
  exposure of these as toggles eventually.

## Milestone-attached extras (build with their milestone)

- **M2a (pencil) — P5.1 SHIPPED** (`1e06f1e`): B/E/O keys, overlay, undo/eraser, journal stroke micro-previews. The toolbar idea is ruled out for good — zero-chrome wins (U14); the old P/E/V band is retired. Review-sourced polish landed (LANDED.md) except:
- [ ] Pencil: one-euro live-stroke filter (CAPTURE §8.3 MAY) — add only if real-pen dogfood shows live wobble. (P5.1, DOGFOOD-M2.)
- **M2b (voice) — P6.1 engine (`9a5eece`) + P6.2 runtime (`fd0adc8`) SHIPPED**: sessions/scope ring/VAD-onset binding/voice pipeline/corrections/linking, mock/stub-verified (supervisor, downloads incl. byte-zero license gate, tiers, scheduler, consent card, OpenAI-compatible + sherpa-WS clients); M-key mic row still reserved — un-reserving needs the real arm path (P6.3). All eight P6.1→P6.2 wiring obligations closed by P6.2 (the items live in LANDED.md).
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
