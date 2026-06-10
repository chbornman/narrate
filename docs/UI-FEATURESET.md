# UI Featureset — the desktop-conventions agreement

> We are not an everything program. We are THE BEST WAY to review and
> mentally process your images — so that externally processing your work
> makes connecting disparate images into cohesive themes part of the joy of
> looking at your library, instead of a painful metadata search for the one
> photo that made you feel a certain way. — founder, 2026-06-10

Normative addendum to spec/UI.md (where UI.md is silent or amended here,
this file wins). Grounded in a 7-app convention study (Lightroom Classic,
Capture One, darktable, digiKam, Photo Mechanic, Apple Photos,
FastRawViewer); the synthesis lives in the research workflow record and its
key findings are inlined. The quiet philosophy (K16, I1–I6) stands; these
conventions are friction removal from the reviewing state, not feature
creep. **Filter for every line: does it serve reviewing/processing, or
managing? Managing is off-thesis.**

Tags: **[MVP]** P4.2 must ship it · **[nice]** ship if cheap in-packet ·
**[post-M1]** decided, deferred. Founder decisions D1–D7 recorded below.

## 0. The contract (sacred, never violated again by any packet)

- **[MVP] Esc is sacred**: always back exactly one layer, always reaches
  Grid, never quits, always exits text-input focus first. (Every reference
  app that broke this generated chronic bug reports.)
- **[MVP] `G` = go to Grid from anywhere** (universal "go home").
- **[MVP] Symmetric open/close**: Enter, Space, and double-click open Look;
  Space and Esc close it (no LrC-style Space-in-but-not-out asymmetry).
- **[MVP] One window forever**; every key means the same thing in Grid,
  Look, and overlays.
- **[MVP] Modes are visible**: any sticky state (auto-advance, future
  pencil/mic) shows in the indicator strip. No invisible modes.
- **[MVP] Tab = lights-out**: hides ALL chrome instantly (rail, inspector,
  filmstrip, titlebar accessories) — the quiet philosophy as a keystroke,
  and the contract that keeps all future chrome honest (everything added
  later must vanish under Tab). Rail-only toggle moves to `\`. *(Amends the
  current Tab=rail binding — D5.)*

## 1. Grid

- **[MVP] Fixed uniform cells, aspect-fit thumbnails, never square-crop**
  (unanimous across all 7 apps; justified/masonry rejected). The size
  slider sets a *target*; actual cell width snaps to container ÷ integer
  column count so rows always fill the width exactly. Recomputed on resize
  and sidebar open/close.
- **[MVP] Ctrl+wheel = thumbnail size** (synced with slider, `-`/`=` kept);
  plain wheel only scrolls.
- **[MVP] Marquee selection** from empty gutter space (drag on a thumb
  never marquees) — the most-requested missing feature of the pro tools.
  Ctrl held = additive.
- **[MVP] Standard modifier clicks**: click replace · Ctrl+click toggle ·
  Shift+click range. Ctrl+A/Ctrl+Shift+A all/none.
- **[MVP] Active vs selected** (LrC "most-selected" model): the focused
  image is visually distinct from the selected set. Look, the inspector,
  and `R` member-flip act on the *active* image. **Write scope is
  unchanged** (CAPTURE §3: notes/ratings target the full selection — the
  indicator's "speaking about N" remains the truth).
- **[MVP] Badges are display-only, hover-quiet**; `T` cycles cell info
  none → minimal → annotated-state. No clickable badges (LrC's regret);
  the stack chevron (§5) is an expand *control*, not a badge.
- **[MVP]** Scroll position preserved across Look round-trips and folder
  revisits; Home/End, PageUp/PageDown.
- **[nice]** Type-to-jump is NOT built (Search covers it) — revisit only on
  ask. **[post-M1]**

## 2. Look

- **[MVP] Continuous wheel zoom-to-cursor** (the demand-majority: #1 zoom
  complaint in LrC and C1 is its absence) + **fix the existing
  zoom-anchor bug** (vertical axis drifts; keep the cursor point invariant
  on both axes; unit-test the transform incl. letterboxed edges).
- **[MVP] `Z` = Fit ↔ 100% anchored at pointer**; Ctrl+0 fit, Ctrl+1 100%;
  double-click = zoom toggle at cursor; drag pans when zoomed; **Space =
  hold-to-pan while zoomed** (must exist before the pencil claims the
  pointer — M2a prerequisite).
- **[MVP] Zoom state persists across ←/→** within a Look session (punch in
  at 1:1, arrow through to compare — its reset is a chronic complaint in
  digiKam/Apple). Default entry = Fit.
- **[MVP] Navigation set = entry selection**: entering Look with a
  multi-selection cycles within it; single-image entry cycles the folder.
- **[MVP]** Filmstrip toggleable (`F`), **default hidden**; the indicator
  carries "n of m" position. The Look bottom edge stays otherwise
  unclaimed — the M4 stroke-scrubber shares it as an alternate mode.
- **[nice]** Preload ±1 neighbor display preview; transient zoom-% readout.

## 3. Panels — the spatial grammar (left = sources, right = truth about
this image, bottom = the set)

- **[MVP] Left rail = generalized source list**, architected now for M3:
  folders today; projects and saved searches join as siblings later (every
  reference app puts collections on the left). Resizable, persisted,
  `\` toggles, push-not-overlay.
- **[MVP] Right inspector** (push, resizable, persisted, hidden by
  default): `I` opens Metadata tab, `J` opens Journal tab, Esc closes
  first. The right edge is RESERVED for per-image truth — journal,
  metadata, and (M5) the partner panel share it; nothing else may claim it.
  - **Metadata tab [MVP]**, read-only (K16 stands): capture time, gear,
    exposure, ISO, dims, orientation, GPS text, file name/path/size,
    content hash (copyable), preview source + backfill state. From the db's
    EXIF subset; no new parsing.
  - **Journal tab [MVP]** — the UI.md journal panel pulled forward (D2):
    chronological folded entries (a vertical timeline from day one — M4's
    per-image timeline becomes a rendering upgrade, not a new surface),
    revision folding with "edited" affordance, retracted behind a toggle,
    redacted stubs, retract + redact flows; the redaction dialog is the
    app's one modal with the required copy; redaction-done is one of the
    three sanctioned toasts.
- **[MVP]** No auto-hide hover fly-outs, ever (universally disabled in LrC).

## 4. Annotate-and-advance (the heartbeat)

- **[MVP] Auto-advance**: `A` toggles; ON = after a rating key (and after
  note submit when entered from Look), advance to the next image in the
  navigation set. Shown as an indicator segment (visible mode rule).
  Default OFF, persisted. (The single most-praised culling flow in the
  corpus; its absence is darktable/digiKam/Apple's top complaint.)
- **[MVP]** Rating keys stay bare 0–5 (never chorded — apps that chorded
  per-image verbs spawned community remap projects). 0 = explicit clear
  (C6 stands).
- **[MVP] Indicator becomes a segmented status strip** (M2b-proofing,
  cheap now): scope ("● N" / "● session") · n-of-m in Look · auto-advance ·
  reserved mic segment. Recording state will live here, never in a toast.

## 5. RAW+JPEG stacks (D1)

- **[MVP]** Auto-pair by basename + folder; one cell with a pair chevron;
  **live, reversible** collapse/expand per pair AND globally (import-time-
  only pairing is LrC's most-documented regret). Collapsed preview = JPEG.
- **[MVP]** Annotating a collapsed stack targets **both hashes** (one
  multi-target event) — this kills the invisible-sidecar failure (a note
  left on the JPEG must not vanish when viewing the RAW). Expanded members
  annotate individually. Data model untouched (K13: two images).
- **[MVP]** In Look, `R` flips the displayed member (FRV convention).

## 6. Discoverability & OS integration

- **[MVP] Right-click context menus mirroring every keyboard verb** (for a
  no-tour app this is the discoverability story): thumb/selection → Open ·
  Rate ▸ · Stack collapse/expand · Metadata · Journal · Show in file
  manager · Copy file path · Open with default app (D4); gutter → Select
  all/none · Sort ▸ · Size ▸; rail folder → Open · Show in file manager ·
  Rescan.
- **[MVP] `?` / F1 = keyboard-map overlay** (the full key table as a
  dismissable sheet — no tour, no coach marks; I5 stands).
- **[MVP]** Empty states say the next action; tooltips with key hints on
  the few chrome controls.
- **[MVP]** Drag a folder onto the window → register-root confirmation.
- **[MVP]** Window geometry persisted; F11 fullscreen.
- **D3: no deletion in v1** — the app never deletes/trashes files; "Show in
  file manager" covers it; the watcher reconciles; journals go dormant.

## 7. Viewing comfort (D6 — amends I5 "dark only")

- **[MVP] Surround luminance setting**: chrome stays dark (quiet stands);
  the image surround (Grid + Look backdrop) gets: black · dark gray ·
  middle gray · light gray · white. Right-click on the backdrop also sets
  it (LrC convention). Journal-dot/selection/focus tokens get per-surround
  contrast tuning; pencil-red reservation holds everywhere.
- **[post-M1] Full interface themes** (light chrome + grays): the token
  architecture already permits it; build only on founder ask.

## 8. Anti-pattern guardrails (adopted as standing rules)

Single click zone per cell · no chorded hot-path verbs · no auto-hide
panels · no import-time-only decisions (everything pairing/grouping is
live-reversible) · filters must read as "show only" and never resemble
annotation controls (M3 rule) · no multi-window · zoom never resets per
image · defaults ARE the fast path (configurability is the escape hatch,
not the prerequisite) · destructive ambiguity impossible (we have no
destructive file verbs at all).

## 9. Future seats reserved (so nothing re-architects)

- Bare-letter band `P`/`E`/`V` reserved for M2a pencil tools; overlay
  cycle key reserved; wheel-zoom stays live while a tool is active.
- Indicator mic segment + one bare key reserved for M2b (press-toggle /
  hold-momentary).
- Rail = source list (M3 projects/saved-searches); active-query residue
  shows in the indicator with one-key clear; drag-selection-to-rail files
  into projects (enabled by marquee + drag split).
- Look bottom edge = filmstrip/scrubber alternates (M4); right edge =
  journal/partner (M5); both obey Tab.

## 10. Founder decisions

- **D1** stacks: display-level pairing; collapsed annotations target both
  hashes; expand = individual. **D2** journal panel ships in P4.2; P5.1 =
  pencil only. **D3** no deletion in v1. **D4** OS integration = reveal +
  copy path + open-default (no configurable editor yet). **D5** Tab =
  lights-out, `\` = rail. **D6** surround luminance in MVP; full themes
  post-M1. **D7** auto-advance default OFF. *(All confirmed by founder,
  2026-06-10.)*

## Acceptance

Every [MVP] item lands in packet **P4.2** with logic under test (zoom
transform, layout math, stack grouping/targeting, selection cycling, keymap
dispatch incl. new contract keys, journal display states) or named in
DOGFOOD-M1.md §visual. Standing gates unchanged. P5.1 (pencil) follows.
