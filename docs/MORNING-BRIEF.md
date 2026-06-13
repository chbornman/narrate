# MORNING BRIEF — June 13 2026, after the autonomous night

15 items landed + pushed overnight (see `docs/LANDED.md`, `origin/main`). One
crisis (full disk) hit and was recovered; prevention is in place (prune each
worktree right after its merge + a `df` preflight per wave). This brief covers
what's LEFT, with my recommended shape for each so you can steer instead of
reverse-engineering my choices.

## Currently building
- **"More like this"** — visual-similarity search off the stored CLIP vectors,
  surfaced as a new grid scope through the search-as-scope machinery.

## Search line — one slice left
- **Phase 4: fuzzy quiet-toggle** — a `~` glyph in the bar, off by default,
  typo-tolerant matching over metadata (camera/lens/filename), ADDITIVE after
  exact FTS, never on the <100ms keystroke path. Backend FTS-fuzzy (trigram or
  edit-distance over metadata columns) is the real work. RECOMMEND: build it.
  (The continuous weight SLIDERS from Phase 4 stay eval-gated per your D2 — they
  wait on real numbers from the eval harness, so I'm deliberately NOT building
  them yet.)

## Nice-to-haves I recommend building (well-scoped, on-thesis)
- **Compare / A-B view** — N images side-by-side in Look to pick between takes /
  brackets. Reviewing finished work IS choosing between takes; this is the
  missing verb. Bigger (a view mode), but high value.
- **Focus loupe in Look** — a zoomed sharpness inspector, now that we have a
  true full-res decode. Moderate, Look-local.
- (ALREADY EXISTS, scratch from the list: the camera-JPEG vs develop toggle is
  the existing `R` key RAW/JPEG flip in Look.)
- Lower priority, larger: session replay · attention heatmap over a collection ·
  contact-sheet/gallery export · keyboard-free pen+voice review mode · voice as
  a navigation verb.

## Design rounds — my recommendation per item (your call before I build big ones)
- **Digest visibility** — RECOMMEND: expand the What's-Happening Station on
  hover to show per-pass progress (done/total per stage), collapsed = a single
  "settled / working" glyph. Small, self-contained slice; I can build it.
- **Autosuggest collections** — RECOMMEND: "record signals first" (co-annotation
  in a session, repeated phrases, time+folder affinity) into a signals table NOW;
  surface suggestions later. Backend slice is safe to build; the suggestion UX is
  a later design pass. (Avoid machine-drafted notes entering the store, per K14.)
- **Collection rollups (LLM)** — RECOMMEND: FUEL TIER only first (invisible
  LLM summaries for retrieval/context). NUDGE TIER ("7 of 12 notes mention fog")
  as a quiet suggestion later. Do NOT let machine prose enter the journal as
  content. YOUR CALL: does drafting ever graduate? (I recommend no.)
- **Preview-policy settings** — RECOMMEND: LrC-style toggles — which previews to
  keep, and "discard full-decode 1:1 artifacts after N days" (they're big on
  disk; on-demand build already landed with RAW decode). Moderate; buildable.
- **Roots & subfolders redesign** — BIGGER / subjective. RECOMMEND: keep the flat
  roots model; refuse nested/overlapping root adds (alias into the existing root);
  lazy-load deep trees; optional grouping by volume. But this reshapes the rail,
  so I'd rather you ratify the shape before I build it.
- **Full interface themes (light chrome)** — BIGGER / subjective. The token
  architecture is ready, but a light theme is a lot of visual QA across every
  surface. RECOMMEND you eyeball a direction before I invest a build here.

## What's genuinely blocked on you / the world
- Spike session 2 (needs the RTX 5080 box) · Nemotron 3.5 (waiting on a
  sherpa-onnx crate release) · the golden-query eval RESULTS (harness is built;
  needs your real query set at `test-corpora/retrieval/golden.json`) · the
  audiobook WER NUMBER (scorer built; needs the model + corpus run on your box).

## How I'll proceed unless you redirect
Keep building the well-scoped items one/two at a time (more-like-this → fuzzy →
focus loupe → compare view → digest visibility → autosuggest signals →
preview-policy), pruning worktrees as I go. Holding **roots redesign** and
**light themes** for your direction (too subjective to build blind without
risking rework). Ping me to reprioritize any of it.
