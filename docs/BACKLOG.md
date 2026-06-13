# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis). Shipped items move to LANDED.md verbatim — only open
work lives here.

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
- [ ] **B summons the overlay** (founder, June 2026): pressing B in Look
  with the tracing-paper overlay hidden currently does nothing - it
  should show the overlay AND enter pencil mode in one keystroke; a
  bound key must never be dead.
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
- [ ] **Drag photos OUT of the app** — from the grid or from Look, click-
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
auto-hide chrome · keyword taxonomies (collections are intent groupings with
evented membership — "tags with time" — never hierarchical vocabularies).
