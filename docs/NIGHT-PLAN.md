# NIGHT PLAN — autonomous session, June 12-13 2026

Founder went to bed and asked me to keep working through the backlog, push
freely, and surface what's buildable tonight vs what needs them. This is the
triage + the live worklist. Status updates land here and in `docs/LANDED.md`.

## ✅ Landed + pushed tonight (origin/main @ `5ce9f56` and onward)

- **Full RAW decode Phase 1** (`6d7c4fb`) — on-demand neutral develop, full-
  sensor-res deep-zoom, the 154-stuck-rows bug dissolved. (LANDED.md has the
  full entry + review follow-ups.)
- **Context-menu side-flyout submenus** (`91bfa15`).
- **T cell-info grows the cell** (`d541854`).
- Plan corrected (rawler `todo!()` API hazards) + search-as-scope design doc
  written, all six decisions ratified (`docs/DESIGN-SEARCH-AS-SCOPE.md`).

## 🟢 Building now / queued (autonomous — design-complete, low-risk)

These I can build, gate, and push without waking anyone. Reviewed before merge.

1. **Search-as-scope Phase 1** — query as a third grid scope, always-visible
   header bar, relevance sort, lexical/semantic `mode` arg. (IN FLIGHT.)
2. **Audiobook WER scorer** — `pp_voice_bench --expect` + WER core + tests;
   corpus already fetched. (IN FLIGHT.)
3. **Em-dash creep gate** — `scripts/check-no-emdash.sh` + a gate target.
   (IN FLIGHT.)
4. **Type-to-jump filename in grid** — decided; "Search covers it meanwhile."
   Small, clear behavior. (QUEUED — after search Phase 1 merges, to avoid grid-
   state conflicts.)
5. **Configurable external editor (D4)** — open-in-editor command + a pref +
   a context-menu seat. (QUEUED.)
6. **Collection-note composer (UI slice)** — storage/commands already landed
   with P7.3; just the composer in the collection's rail tab. (QUEUED.)
7. **Progressive-import shimmer** — item (b): a subtle "building" shimmer on
   the previewReady placeholder instead of dead gray. (QUEUED.)
8. **Search Phase 2** — explicit live-lexical/commit-semantic split + the
   detail-row status. (QUEUED — after Phase 1.)
9. **Search Phase 3** — per-signal toggles (on/off) surfacing the B75 weights;
   the backend plumbing already exists. (QUEUED — after Phase 2.)

## 🧑 Needs you (design round or founder call — I will NOT auto-build these)

- **Foreign-edit sidecars / exports-folder review** — the portable-subset
  (crop/orientation/rating) approach needs a design round; the exports-folder
  path needs a scoping call. (In tension with neutral develop.)
- **Roots & subfolders design round** — nested/overlapping roots, grouping,
  deep-tree ergonomics, root lifecycle.
- **Digest visibility design round** — the per-stage "what is my library doing"
  surface (leading candidate: the What's-Happening Station hover).
- **Collection-level rollups (LLM)** — fuel tier vs nudge tier vs drafting;
  founder call pending on whether drafting ever graduates.
- **Autosuggest collections** — design round ("record signals first" is a
  legit v1; surfacing later).
- **Full interface themes (light chrome)** — token architecture ready; needs
  founder appetite.
- **Preview-policy settings** (LrC-style build/keep/discard 1:1 knobs) — some
  design.
- **Histogram in Look / GPS map view** — now feasible with the decode pipeline,
  but placement/toggle needs a design call.
- **Voice chunking FEEL tuning** (rule2 / merge policy) — needs founder
  dictation clips and ear; empirical but judgment-driven.

## ⛔ Blocked (external — can't do tonight)

- **Spike session 2, desktop half** — needs the RTX 5080 machine.
- **Nemotron 3.5 upgrade** — waiting on a sherpa-onnx Rust crate release.
- **Golden-query retrieval eval** — needs the founder-built query set over the
  real annotated library.
- **Real embedder connector + backfill** — wants the spike-2 findings first.

## 💡 Nice-to-haves we haven't built (new ideas, on-thesis: REVIEW done work)

Brainstorm for founder consideration — none auto-built; logged here so they
aren't lost. All fit the "review/gather context, don't edit/manage" thesis.

1. **Compare / A-B view** — two (or N) images side-by-side in Look to pick
   between near-duplicates or bracket variants. Reviewing finished work IS
   choosing between takes; this is the missing verb. (Pairs with burst stacks.)
2. **"More like this" from an image** — one-click reverse-image search off the
   CLIP embedding we already compute. The strongest retrieval payoff for almost
   no new machinery (the vector is already in the store).
3. **Camera-JPEG vs neutral-develop toggle** — now that BOTH the embedded
   camera render and our neutral develop exist for a RAW, a key to flip between
   them is a genuine reviewing aid (judge the camera's rendering).
4. **Focus loupe in Look** — a zoomed sharpness inspector (now that we have a
   real full-res decode), for checking critical focus on finished selects.
5. **Voice as a navigation/retrieval verb** — the mic is already armed for
   notes; let it also drive ("show the foggy ones", "next", "circle this").
   Voice-scoped search closes the loop with search-as-scope.
6. **Session replay** — replay an annotation session as a timeline (strokes +
   voice notes in capture order) — "review my review." Natural M4 sibling to
   the library event timeline.
7. **Attention heatmap over a collection** — which images drew the most
   strokes/notes/dwell; "where did my eye go." Builds on stroke-aware retrieval.
8. **Contact-sheet / web-gallery export** — share a collection-as-reviewed with
   a client or collaborator (read-only, edit baked into the chosen exports).
   The app reviews done work; sharing the review is the natural outward step.
9. **"Show everything I X'd out"** — the gesture-semantics idea (classify
   circle/X/underline/arrow) cashed out as a one-tap reject/flag smart view.
10. **Keyboard-free review mode** — full-screen lights-out + mic-armed +
    pencil, tuned for a tablet/pen review session away from the keyboard.
