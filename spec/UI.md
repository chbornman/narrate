# spec/UI.md — Surfaces, Interaction, and the Quiet Main Flow

Status: Draft 1, June 2026. Normative for the frontend. Closes gap F (SPEC-GAPS.md) plus the founder's UI directives in FEATURES.md.

Boundaries with sibling specs: **CAPTURE** owns capture semantics, timing, and state machines (this spec renders their contracts); **RETRIEVAL** owns the search result data contract (this spec renders it); **RUNTIME** owns first-run/model lifecycle states (this spec renders them); **EVENTS** owns event semantics (retraction/redaction/revision, folds). Where this document describes captured data it is descriptive, not authoritative.

Per SPEC-GAPS guidance, this spec freezes **surfaces, navigation, and the keyboard map** (concrete) and specifies everything else as acceptance criteria and interaction contracts — exact spacing, fonts, and animation curves are implementation latitude.

Key notation: `Cmd/Ctrl` means Cmd on macOS, Ctrl on Windows/Linux.

---

## 1. Philosophy (normative, not decoration)

**The main flow is quiet.** The photographer browses, looks, talks, and marks. Capture is ambient: the user never watches notes being made and never edits metadata visually. The photographs are the interface; chrome exists only where its absence would cause an error of attribution or trust.

Consequences, stated as rules:

- **R1.** There is NO live transcript pane, anywhere, ever. Speech lands in the journal invisibly; the only feedback is the indicator pulse.
- **R2.** There is NO metadata editing UI. EXIF is read-only display; ratings are journal events entered by keyboard with no visible widget.
- **R3.** No AI prose, scores, or tags are ever rendered. Summaries and sentiment are retrieval fuel only. The journal shows verbatim user words and marks; search results quote the user, never paraphrase.
- **R4.** No dashboard, no home screen, no onboarding tour, no progress nags. Features light up silently as services become ready.
- **R5.** Modal dialogs: exactly one exists in the app (redaction confirm, §8.4). Toasts: exactly three triggers (§7.5). Everything else is inline, ambient, or absent.
- **R6.** Every action is reachable by keyboard except drawing strokes.

---

## 2. Application shell

### 2.1 Surfaces

Three surfaces. That is the whole app.

1. **Grid** — virtualized thumbnail browser over watched roots.
2. **Look** — single-image view, edge-to-edge. ("Look" is the canonical name; never "detail view," "loupe," or "viewer" in code or copy.)
3. **Search** — one input, results as a grid with provenance.

Auxiliary, non-surface elements: the capture indicator (persistent), the typed-note input (transient), the journal panel (slide-over), the settings window (separate small window), the first-run screens (one-time), and the debug panel (dev builds only).

### 2.2 Navigation model (keyboard-first)

```
            Enter (focused thumb)
   ┌─────┐ ───────────────────────▶ ┌──────┐
   │GRID │                          │ LOOK │
   └─────┘ ◀─────────────────────── └──────┘
      ▲          Escape                 ▲
      │                                 │ Enter (focused result)
      │  Escape (returns to            ┌┴───────┐
      └────── prior surface) ───────── │ SEARCH │ ◀── "/" or Cmd/Ctrl+F
                                       └────────┘     from anywhere
```

- `Enter` in Grid opens the focused image in Look. `Enter` on a Search result opens it in Look.
- `Escape` is strictly "step back one layer," in this order: (1) close any open transient (note input, sort menu, journal panel, indicator popover, debug panel), (2) leave Search back to the surface it was invoked from, (3) leave Look back to Grid with the same image focused, (4) in Grid with a selection, clear the selection. `Escape` never quits the app.
- `/` or `Cmd/Ctrl+F` enters Search from any surface, cursor in the input.
- There is no browser-style history; the model is a two-level stack (Grid ⇄ Look) with Search as an overlay surface that remembers its return point.

### 2.3 Window chrome

- Single main window. Native title bar minimal/hidden where the platform allows (Tauri custom titlebar on Windows/Linux; `titleBarStyle: overlay` on macOS). Window title: current folder name in Grid, image filename in Look, "Search" in Search. No menus beyond the platform-mandated app menu (macOS) carrying only About / Settings / Quit / standard Edit roles.
- Dark theme only in v1 (§12). The background is near-black; the photos are the brightest thing on screen by design.

### 2.4 Settings window — the entire enumeration

One modest window (`Cmd/Ctrl+,`), four sections, nothing else in v1:

1. **Watched folders** — list of roots with online/offline state; add / remove. Removing a root warns inline (one sentence: journals and sidecars are untouched; the images leave the index) — not a modal.
2. **Microphone** — input device picker, input level meter (the only live audio UI in the app), mic-enabled checkbox. Section hidden until ASR is installed (RUNTIME).
3. **Models** — renders RUNTIME's contract: detected hardware tier; per-model rows (name, size, state: not-downloaded / downloading with resumable progress / ready / failed), download/pause/remove actions; the degraded-mode explainer (§9.1 copy rules apply).
4. **Export** — "Export library journal…" (sidecar set + manifest, per SIDECARS), destination picker, last-export timestamp shown inline; and a secondary "Rebuild index from sidecars…" maintenance action with inline (not modal) confirm.

Explicitly absent: appearance/theme, keyboard remapping, per-folder options, cache tuning, telemetry (none exists), accounts (none exist).

---

## 3. The Grid

### 3.1 Layout

```
┌────────────────────────────────────────────────────────────────┐
│ ▍2026-04 Iceland                                sort ▾  ▢▢▢ ─┊ │  ← header: folder name,
│┌──────┐                                                        │     sort, thumb-size slider
││rail  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐            │
││(auto │  │    │ │  ● │ │    │ │    │ │ ⏏  │ │    │            │  ● has-journal dot
││hides)│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘            │  ⏏ offline-volume badge
││      │  ┌────┐ ┌────┐ ┌╔══╗┐ ┌╔══╗┐ ┌────┐ ┌────┐            │  ╔╗ selected
││Active│  │    │ │    │ │║  ║│ │║ ●║│ │    │ │    │            │
││ Work │  └────┘ └────┘ └╚══╝┘ └╚══╝┘ └────┘ └────┘            │
││Archiv│  ┌────┐ ┌────┐ ┌────┐ …                               │
│└──────┘  └────┘ └────┘ └────┘                                 │
│                                                       ● 2  🎙 │  ← indicator (§7)
└────────────────────────────────────────────────────────────────┘
```

### 3.2 Folder navigation — the rail

- A **collapsible minimal left rail** lists watched roots and their folder trees. Folders are shoots; the rail is the only folder UI.
- **Auto-hide:** hidden by default; slides in when the pointer rests at the left edge (~150 ms dwell) or when toggled with `Tab`, and hides again on folder selection or pointer leave. A small pin affordance keeps it open; pinned state persists.
- Rail rows show folder name and, for offline volumes, the ⏏ badge. No counts, no context menus in v1.
- With the rail open: `↑/↓` move folder focus, `→/←` expand/collapse, `Enter` opens the folder in the Grid and hides the rail (unless pinned).

### 3.3 Virtualization & performance

- The grid is windowed/virtualized: only visible rows (+1 screen of overscan) are mounted. Thumbnails load from the preview cache (display-oriented sRGB, per LIBRARY) into a neutral placeholder; no layout shift on load.
- **Thumbnail delivery (normative):** thumbnails and Look images are served over a Tauri **custom URI scheme** (async `register_uri_scheme_protocol`) reading from the preview cache, with HTTP cache headers so the webview's own cache does the rest. Image bytes **never** cross `invoke`/IPC and are never base64-encoded — maintainer guidance is unambiguous that protocol serving beats IPC for assets ([tauri discussions #7145](https://github.com/orgs/tauri-apps/discussions/7145), [#5690](https://github.com/orgs/tauri-apps/discussions/5690)). The virtualizer **recycles `img` elements**; no blob/object URLs for thumbnails.
- **Budget: 60 fps sustained scroll on a 20k-item folder** on the hardware-floor machine. Thumbnail decode happens off the main thread; scrolling never blocks on IO.
- During live ingest the grid populates incrementally (new items appear in sort position) without breaking scroll position or 60 fps.

### 3.4 Selection model (selection = write scope, per CAPTURE)

- `Click` selects one. `Shift+Click` range-extends. `Cmd/Ctrl+Click` toggles. `Cmd/Ctrl+A` selects all in the current folder. `Escape` clears.
- Keyboard: arrows move focus; `Shift+arrows` extend selection; `Space` toggles selection on the focused item.
- Selection renders as a thin light border + slight lift — no checkmarks, no count chips on thumbnails. The count lives in the indicator ("● 3").
- The selection IS the write scope. The UI performs no scope logic of its own: it reports selection changes to the core and renders the scope the core echoes back (CAPTURE owns snapshot/binding semantics).

### 3.5 Badges — the complete set

Exactly two badges may appear on a thumbnail:

1. **Has-journal dot** — a small dull-red dot, bottom-right of the thumb. The only grid evidence that annotations exist. No count.
2. **Offline-volume badge** — small ⏏/disconnected glyph, top-right, on images whose only known paths are on offline volumes.

Explicitly forbidden on thumbnails: rating stars, color labels, label chips, filename overlays, EXIF overlays, stroke previews. Quiet.

### 3.6 Sort & zoom

- Sort menu (header, key `S`): **Capture date (default, newest first)**, Capture date oldest-first, Filename A–Z, Date added (ingest time). That is the v1 set. Sort persists per folder.
- Thumbnail size: header slider plus `-`/`=` keys, 4 steps (~96–320 px cells). Persists globally.

### 3.7 Grid acceptance criteria

- [ ] 20k-item folder scrolls at 60 fps end-to-end on floor hardware.
- [ ] Webview process memory stays below **800 MB** after scrolling a 20k-item grid end-to-end **twice** — guards the WebView2 asset memory-release class of bug ([tauri #2952](https://github.com/tauri-apps/tauri/issues/2952)).
- [ ] Rail is absent from layout until summoned; summoning/hiding never reflows the grid (overlay, not push).
- [ ] Rating keys 0–5 with a selection commit events with **no visual change to any thumbnail** — indicator pulse only.
- [ ] A thumbnail shows at most the two permitted badges; nothing else.
- [ ] Multi-select of 500 images then typing a note produces one event targeting 500 hashes (scope renders "● 500"); UI stays responsive.
- [ ] Moved/renamed files relink without thumbnails flickering or reordering beyond their new sort position.

---

## 4. Look (single-image view)

### 4.1 Layout

```
┌────────────────────────────────────────────────────────────────┐
│                                                                │
│                ┌────────────────────────────┐                  │
│                │        IMAGE  (edge-to-    │  ← no chrome over the
│                │        edge at fit; pans   │    image, ever
│                │        and zooms freely)   │                  │
│                │      ◜~~ red stroke ~~◝    │  ← overlay (tracing
│                │                            │    paper), O toggles
│                └────────────────────────────┘                  │
│  [filmstrip — hidden by default, F toggles]            ● 1 🎙 │
└────────────────────────────────────────────────────────────────┘
```

- The image fills the surface edge-to-edge at "fit." Background is the near-black canvas; no borders, no info bar, no filename overlay.
- **Filmstrip:** off by default; `F` toggles a single-row strip along the bottom (height ≤ 88 px) for context while sequencing. State persists. The filmstrip shows the same two badges as the Grid, nothing more.

### 4.2 Pan / zoom map

- Scroll wheel / pinch: zoom toward cursor. Trackpad two-finger pan when zoomed.
- Drag pans when the pencil is OFF. With pencil ON, hold `Space`+drag to pan.
- `Z` toggles fit ⇄ 100%; `+`/`-` step zoom; `Cmd/Ctrl+0` fit.
- **Budget: image swap < 150 ms from cache** on `←`/`→` (preview-cache hit; progressive sharpen afterward is fine).

### 4.3 Prev/next

`←`/`→` move through the current Grid order (or the Search result order if Look was entered from Search). Navigation **never** interrupts the mic or the session: a still-streaming utterance stays bound to its utterance-start snapshot per CAPTURE; the UI's only job is to keep reporting view/selection changes and to render the streaming-scope state in the indicator (§7.3).

### 4.4 The grease pencil — zero-chrome drawing

CAPTURE owns stroke semantics (one pen-down→pen-up = one event, normalized display-oriented coordinates, utterance linking). This spec owns the interaction:

- **Activation:** `B` toggles pencil mode (sticky toggle, not hold-key — photographers draw multiple strokes per thought; a held key cramps the drawing hand). Pencil mode persists across prev/next within Look and resets to off when returning to Grid.
- **Cursor:** in pencil mode the cursor is a small red dot (the pencil tip); otherwise the platform default/grab cursor. The dot is the entire mode announcement — **no toolbar, no palette, no tool options appear.** One red pencil is the whole tool set in v1 (CAPTURE / SPEC-GAPS B5). The pencil is a mode, not a palette.
- **Eraser:** hold `E` (or use the stylus eraser end) while in pencil mode; cursor becomes a small hollow circle; clicking/tapping a stroke retracts that whole stroke event (tombstone, per EVENTS). No partial-stroke erase. Pre-commit undo (`Cmd/Ctrl+Z` during the same pen-down) is local and never logged.
- **Pressure is progressive enhancement, not a baseline.** Expected on Windows pen hardware, where WebView2 receives pressure via Windows Ink (support-doc item: the Wacom driver's "Use Windows Ink" toggle must be on — [Wacom support](https://support.wacom.com/hc/en-us/articles/1500006343962-Why-is-my-pen-pressure-not-working)). **Not expected in WKWebView on macOS** — macOS tablets post mouse events with tablet subtype data ([Wacom dev docs](https://developer-docs.wacom.com/docs/icbt/macos/ns-events/ns-events-basics/)) and there is no positive evidence pressure reaches the webview; strokes render at constant `base_w` there (CAPTURE §8.2's no-pressure rule is the macOS norm). Recorded future option: a native NSEvent tablet-pressure passthrough **plugin** feeding the overlay — a plugin, not a webview fix.
- **Overlay toggle:** `O` toggles the tracing-paper overlay (all strokes shown/hidden). Toggling the overlay off while in pencil mode also exits pencil mode (you cannot draw on paper you cannot see). Overlay state persists per app-run; defaults to on.
- Stroke commit (pen-up) pulses the indicator. Nothing else happens visibly — the stroke simply remains on the tracing paper.

### 4.5 Ratings in Look

`0`–`5` commit a rating event for the viewed image (or the current selection if Look was entered with a multi-selection — CAPTURE defines scope). **Explicitly: no star overlay, badge, or HUD appears on or near the image. The indicator pulse is the entire feedback.** `0` clears (rating fold per EVENTS: last rating wins).

### 4.6 Look acceptance criteria

- [ ] `←`/`→` image swap < 150 ms from cache; mic state and any in-flight utterance binding are unaffected (validated via debug panel Capture tab).
- [ ] Entering pencil mode adds zero pixels of chrome; the cursor change is the only signal.
- [ ] A stroke drawn while speaking shows, in the journal later, the stroke linked to the utterance (rendering per §8; linking per CAPTURE).
- [ ] `O` off → strokes invisible and pencil unavailable; `O` on → strokes return exactly as drawn at any zoom (vector overlay, normalized coords).
- [ ] Pressing `3` shows nothing on the image; the indicator pulses once; a rating event exists in the journal.

---

## 5. Search

### 5.1 Layout & behavior

```
┌────────────────────────────────────────────────────────────────┐
│   🔍  quieter melancholic series_                              │
│       [date: last winter ×] [project: Harbor ×]   ← chips (M3) │
│                                                                │
│   ┌────┐  "this one has that stillness I keep                  │
│   │    │   coming back to"             — 12 Jan 2026           │
│   └────┘                                                       │
│   ┌────┐  "too literal for the series, shelve it"              │
│   │    │                               — 3 Nov 2025            │
│   └────┘                                                ● 0 🎙 │
└────────────────────────────────────────────────────────────────┘
```

- **One input field.** Focused on entry; no search button.
- **Search-as-you-type** (M1: FTS5; M3: hybrid per RETRIEVAL). Budget: results render **< 100 ms** after keystroke (debounce ≤ 50 ms allowed within that budget) for FTS; hybrid results may stream in after.
- **Results = image grid with provenance.** Each result renders the data RETRIEVAL provides: thumbnail + the matching quote (the user's own words, verbatim, match-highlighted) + date, beneath/beside the thumb at default size, on hover at small sizes. The quote is never paraphrased or summarized (R3).
- **Filter chips (M3):** when RETRIEVAL's parse extracts structured filters, they render as removable chips under the input. `×` or `Backspace` on an empty input removes the last chip; removing a chip re-runs the query with that clause returned to plain text. No faceted sidebar, no filter-builder UI.
- Navigation: arrows move result focus; `Enter` opens Look (result order becomes the prev/next order); `Escape` returns to the invoking surface. Selection within results works like the Grid and is a valid write scope.

### 5.2 Empty & zero-result states

- Empty query: blank canvas below the input. No trending, no recents, no suggestions. Quiet.
- Zero results: the single dimmed centered line `Nothing in your journal matches.` No suggestions, no "did you mean," no tips.

### 5.3 Search acceptance criteria

- [ ] FTS results update within 100 ms of each keystroke at 50k images.
- [ ] Every rendered result includes a verbatim quote and date from RETRIEVAL's provenance contract; a result with no text provenance (pure image-embedding hit, M3) renders with no quote rather than a generated one.
- [ ] `Enter` on a result lands in Look on that image; `←`/`→` then walk the result list.
- [ ] No UI element ever displays a relevance score, signal name, or ranking explanation (debug panel only).

---

## 6. Typed note input

- Summoned by `N` from any surface (and by clicking the indicator). A small floating input appears **anchored above the indicator** (bottom-right), ~420 px wide, with the current scope echoed as dimmed placeholder text ("note on 3 images").
- Multi-line via `Shift+Enter`. `Enter` submits: the input vanishes immediately; the indicator pulses when the event commits. `Escape` cancels and vanishes; non-empty drafts are discarded without prompt (notes are short; a confirm would be louder than the loss).
- Never persistent chrome: it exists only between summon and submit/cancel. No notes list, composer history, or formatting.
- Acceptance: [ ] summon→type→Enter→vanish leaves no visual residue except one indicator pulse; [ ] the event targets the scope shown at summon time (binding per CAPTURE); [ ] note-to-pulse < 50 ms.

---

## 7. The capture indicator

The single ambient capture-feedback element. Bottom-right corner, all surfaces, ~24 px tall, always present after first-run.

### 7.1 Anatomy

```
       ┌──────────────────────┐
       │  ▁▁▂▁  ● 3   🎙      │      ▁▂ ingest hairline (only while ingest runs)
       └──────────────────────┘
            scope ──┘    └── mic glyph (only when ASR installed)
```

### 7.2 Write-scope states

- `● 1` — one image selected/viewed. `● 3` — three selected. `● session` — no selection; words land session-level. `● 0` never renders; no-selection is always `● session` (per CAPTURE's scope rules).
- **Hover:** a small popover shows micro-thumbnails of the scoped images (up to 8, then "+N"), confirming attribution without opening anything.
- **Click:** opens the typed-note input (§6).

### 7.3 Mic & streaming states (rendering CAPTURE's contract)

- Mic glyph absent until ASR is ready (RUNTIME); it appears silently.
- Disarmed: dimmed glyph. Armed: solid glyph with a faint slow breathing animation while VAD detects speech.
- **Armed hover popover** carries the one-line privacy claim: "Listening — audio is transcribed on this device and never written to disk." Nothing more. Note the macOS interplay: the system orange mic dot burns for the **entire armed session** ([Apple](https://support.apple.com/en-us/118449)); the app's armed state must always agree with it — CAPTURE closes (never pauses) the audio stream on disarm, so glyph and dot cannot disagree.
- **Streaming-utterance scope:** while an utterance that started under a *previous* scope is still streaming, the indicator shows that bound scope with a tether, e.g. `● 1 ⇠ 🎙` — "the words in flight belong to that earlier snapshot," even though the user has clicked onward. It reverts to the live scope when the segment finalizes. This renders CAPTURE's utterance-start binding rule; the indicator never re-binds anything itself.
- **ASR degraded/down:** the glyph swaps to a muted-mic glyph (struck through), dimmed. Hover popover explains in one line ("Voice capture unavailable — typed notes and pencil still work."). **Never a modal, never a toast, never a banner.**

### 7.4 Pulse

A single ~300 ms brightness pulse of the scope dot on each committed event: typed note, finalized utterance, stroke commit, rating, revision/retraction/redaction, and sidecar/metadata writes where they carry their own commit signal. Pulses queue visually (rapid events = rapid distinct pulses, coalesced above ~5/s). Coalescing applies on the wire too: the pulse stream must stay low-rate and payload-light — high-frequency Tauri event emission has a documented leak history ([tauri #852](https://github.com/tauri-apps/tauri/issues/852)). **Budget: input action → pulse < 50 ms** for locally-originated events.

### 7.5 Ingest line & toasts

- While ingest runs, a 2 px hairline progress bar sits inside the indicator capsule. Hover shows "Indexing — 12,402 of 48,377". Full detail lives in the debug panel only. No percentage text, no dialog.
- **Toasts may appear in exactly three cases**, bottom-right above the indicator, auto-dismiss 5 s: (1) retraction committed — "Retracted" with an Undo action (undo = un-retract per EVENTS); (2) redaction completed — "Redacted from journal, sidecars, and indexes" (appending "— 1 offline sidecar pending" when applicable); (3) a pending offline-volume redaction later completing on mount. Nothing else in the application may toast.

### 7.6 Indicator acceptance criteria

- [ ] Indicator visible on Grid, Look, and Search at all times post first-run; never obscures image pixels in Look at fit zoom.
- [ ] Scope text matches CAPTURE's echoed scope within one frame of a selection change.
- [ ] Speak, click next image mid-sentence: indicator shows the tethered previous scope until finalization (verifiable in the debug Capture tab), then reverts.
- [ ] Killing the ASR process mid-session changes only the mic glyph; no modal, toast, or banner appears.

---

## 8. The journal panel (on-demand reading)

### 8.1 Summon & frame

- `J` toggles a right-side slide-over panel, ~380 px wide, over the current surface. In Look: the viewed image's journal. In Grid with a single selection: that image's. In Grid with multi/no selection: the current session's journal.
- Closed by default, **never** default-open, never remembered-open across launches. The panel overlays; it does not reflow the surface. `Escape` or `J` closes it.

### 8.2 Contents — verbatim, chronological

```
┌— Journal — IMG_4471 ——————————┐
│ ── Session · 4 Jun 2026 ──    │   session dividers
│ 14:02  "the hand is the whole │   voice remark (verbatim)
│         picture"              │
│ 14:02  [stroke ◜◯◝ thumb]     │   stroke = micro-preview of the stroke
│         linked ↑              │   over a small thumbnail, at its timestamp
│ 14:09  rating set to 4        │
│ ── Session · 12 Jan 2026 ──   │
│ 09:31  "too dark in the       │
│         corners" · edited ▸   │   revision-folded; expand shows original
│        [show 1 retracted]     │   retracted hidden behind toggle
└───────────────────────────────┘
```

- Strict chronological order (log order per EVENTS), newest session first, grouped under session dividers (date, time range).
- Remarks (voice and typed) render identically — verbatim text, time. Source shows only as a subtle glyph on hover. No confidence scores.
- Strokes render as micro-previews: the stroke path drawn over a small thumbnail, at the stroke's timestamp; linked utterances show a subtle link mark. Clicking a stroke row flashes that stroke on the Look overlay (when in Look).
- Revisions: the folded (corrected) text is displayed; a subtle "edited" affordance expands to the original, dimmed and labeled.
- Retracted items hide behind a per-session "show retracted" toggle; shown, they render struck-through and dimmed.
- Redacted items render as a single dimmed "redacted" line at their timestamp — content gone, continuity preserved (per EVENTS).
- **No AI summaries, rollups, sentiment, or tags appear here, ever.** Verbatim user words and marks only (R3).

### 8.3 Actions (the only place capture is edited)

Hovering a row reveals three quiet actions:

- **Correct** — inline edit of the text; commit emits a revision event (EVENTS); the row re-renders folded with the "edited" affordance.
- **Retract** — immediate tombstone; the row disappears behind the toggle; a toast with Undo appears (§7.5).
- **Redact…** — opens the app's one modal (§8.4).

### 8.4 The redaction dialog — copy requirements

Two-step confirm, the single heavyweight dialog in the app. Required content (wording is copy-editable; the claims are not):

1. The verbatim content about to be redacted, quoted.
2. What redaction DOES: permanently scrubs the text from the database, all search indexes and vectors, and every sidecar file it appears in; the event's id and timestamp remain as a "redacted" marker for log continuity; a redaction can never be undone or merged back from old sidecars (redaction wins over union, per EVENTS).
3. What redaction does NOT do: it cannot scrub copies exported or backed up outside Photoproof; and sidecars on currently-offline volumes are scrubbed only when that volume next connects (until then the text still exists on that disk) — listed explicitly with the affected volume names when applicable.
4. Step two: a type-to-confirm or hold-to-confirm action labeled "Redact permanently." Default-focused button is Cancel.

### 8.5 Journal panel acceptance criteria

- [ ] `J` in Look opens the image's full history < 100 ms at 1k events.
- [ ] An image with zero events opens an empty panel reading only "Nothing yet." — no prompt to add notes.
- [ ] Correct → revision: list shows corrected text; expanding shows the original; FTS finds the corrected text, not the original (EVENTS).
- [ ] Retracted items invisible until toggled; redacted items show only the marker line.
- [ ] No element in the panel renders model-generated text.

---

## 9. First-run & runtime states (rendering RUNTIME's contract)

### 9.1 First launch

1. The window opens directly to an empty Grid with one centered, dimmed line — **"Add a folder of photographs."** — and an Add Folder button (also reachable via `Tab` → rail). No tour, no carousel, no sample library.
2. Picking a root starts ingest immediately; thumbnails stream into the Grid live as the hash/preview pass completes each image. The user can browse, select, rate, and type notes during ingest (M1 is fully functional with zero models).
3. **Model consent screen** — shown once, after the first root is added, as a one-time panel, not a modal gate. Contents per RUNTIME: detected hardware tier; one decision — "Download" (sizes per model, total shown) or "Not now." **Copy must state loudly:** skipping changes nothing about journaling — typed notes, the pencil, ratings, and keyword search are fully functional without any models; voice capture and semantic search light up later if models are added (Settings → Models).
4. Thereafter, features appear silently as services become ready — the mic glyph simply appears in the indicator when ASR is up (R4). No "ready!" notification of any kind. Ingest progress lives as the indicator hairline (§7.5) + full detail in the debug panel.

### 9.2 Degraded / below-floor mode

Identical UI minus the mic glyph and minus semantic results. No banners, no nags, no upsell. Degraded mode is named only in Settings → Models and the consent screen.

### 9.3 Acceptance criteria

- [ ] Cold first run to "browsing my photos" requires exactly one decision (pick a folder); the model decision is skippable and deferred without consequence to journaling.
- [ ] During a 50k ingest, grid interaction and typed capture meet their budgets (§13).
- [ ] ASR becoming ready mid-session adds the mic glyph with no other UI change or notification.

---

## 10. The debug side panel (dev builds only)

### 10.1 Build mechanics (expectation, normative)

- Gated by a Cargo feature `debug-panel` on the Tauri crate; the feature's Tauri commands/event emitters do not exist in release binaries (`#[cfg(feature = "debug-panel")]`).
- The frontend panel code is excluded from release bundles at build time (compile-time define, e.g. `import.meta.env.PHOTOPROOF_DEBUG`, with dead-code elimination). CI asserts release bundles contain no debug-panel modules or strings, and that invoking a debug command against a release binary fails as unknown.

### 10.2 Behavior

- `F12` toggles a right-side, full-height panel (~480 px), monospace, dense. Overlays like the journal panel; the two may coexist.
- **Read-only** except explicitly-marked dev actions, each rendered as a bordered `[dev]` button: force sidecar flush, force rescan (per root), kill/restart a model process. No other mutations.

### 10.3 Sections (tabs)

1. **Events** — live tail of `annotation_events` as committed: raw payload JSON, targets, source/kind, and the fold result each event produced (e.g. "rating: 3→4"). Filter by kind; click to copy.
2. **Capture** — THE tool for validating the binding rule: ring buffer of write-scope snapshots with timestamps; VAD onset/offset and segment timing; partial transcripts as they stream; each binding decision as it is made (utterance id → snapshot id → target hashes), including grace-window outcomes; stroke↔utterance link decisions.
3. **Ingest** — queue depth, per-pass states (hash/preview/EXIF/backfills) with versions, items/sec throughput, recent errors with paths.
4. **Sidecars** — writer queue length, last N writes (path, latency), debounce state, overflow-store activity, pending offline-volume redactions.
5. **Runtime** — managed process table (ASR, llama.cpp): state, pid, port, health-check latency, VRAM estimate, model versions, recent request latencies (p50/p95).
6. **Search** — for the last query: raw per-signal scores, RRF inputs and fused ranks, and the parse AST (M3) or the fallback path taken.

### 10.4 Acceptance criteria

- [ ] Release build: no `F12` handler, no debug commands registered, no panel code in the bundle (CI-enforced).
- [ ] Dev build: speak-while-navigating scenarios are fully explainable from the Capture tab alone (snapshot → binding → event).

---

## 11. Keyboard map — single source of truth

Every shortcut in the app. Anything not listed here does not exist.

| Key | Context | Action |
|---|---|---|
| `Enter` | Grid / Search result | Open focused image in Look |
| `Escape` | Global | Step back one layer (§2.2 order); never quits |
| `/` or `Cmd/Ctrl+F` | Global | Enter Search |
| `N` | Global | Summon typed-note input |
| `J` | Global | Toggle journal panel (image or session, §8.1) |
| `M` | Global (ASR ready) | Toggle mic armed/disarmed |
| `0`–`5` | Grid (selection) / Look | Commit rating event (silent; pulse only) |
| `Cmd/Ctrl+,` | Global | Open Settings window |
| `Cmd/Ctrl+Q` | Global | Quit |
| `F12` | Global (dev builds only) | Toggle debug panel |
| `Tab` | Grid | Toggle folder rail |
| `↑↓←→` | Grid | Move focus · in rail: navigate folders |
| `Shift+arrows` | Grid | Extend selection |
| `Space` | Grid | Toggle selection on focused item |
| `Cmd/Ctrl+A` | Grid | Select all in folder |
| `S` | Grid | Open sort menu |
| `-` / `=` | Grid | Thumbnail size down / up |
| `←` / `→` | Look | Previous / next image (session-safe per CAPTURE) |
| `Z` | Look | Toggle zoom fit ⇄ 100% |
| `+` / `-` | Look | Zoom in / out |
| `Cmd/Ctrl+0` | Look | Zoom to fit |
| `Space` (hold) + drag | Look (pencil on) | Pan |
| `B` | Look | Toggle grease-pencil mode |
| `E` (hold) | Look (pencil on) | Eraser — click a stroke to retract it |
| `Cmd/Ctrl+Z` | Look (during pen-down) | Cancel in-progress stroke (local, unlogged) |
| `O` | Look | Toggle tracing-paper overlay |
| `F` | Look | Toggle filmstrip |
| `Shift+Enter` | Note input | New line |
| `Enter` | Note input | Submit and vanish |
| `Backspace` (empty input) | Search (M3) | Remove last filter chip |

Single-letter shortcuts are suppressed while any text input is focused. No user remapping in v1.

---

## 12. Cross-cutting: theme, type, accessibility

- **Dark theme only in v1.** No light theme, no toggle, no `prefers-color-scheme` response. Rationale: photographers evaluate images against dark surrounds; one theme keeps the surface honest and the QA matrix small. Recorded as a deliberate limitation.
- **Color discipline:** near-black UI (#0e0e0e-class), low-contrast gray chrome, white text where needed. **The red grease pencil is the only saturated color in the entire UI** — the has-journal dot uses a dulled version of the same red; everything else is achromatic.
- **Typography:** one UI face (system stack acceptable), two sizes plus the journal's reading size; monospace only in the debug panel. No display typography.
- **Accessibility baseline (v1):** keyboard completeness per §11 (every action except drawing strokes); visible focus ring on all focusables; the journal panel and search results are semantic, screen-reader-readable text — the user's own words must never be locked in a canvas. Full screen-reader support for grid culling and drawing is an **honest deferral**, recorded as a known limitation, not silently skipped.

---

## 13. Performance budgets (normative)

| Interaction | Budget | Condition |
|---|---|---|
| Grid scroll | 60 fps sustained | 20k-item folder, floor hardware |
| Look image swap (`←`/`→`) | < 150 ms to displayed image | preview-cache hit |
| Search-as-you-type (FTS) | < 100 ms keystroke→results | 50k-image library |
| Typed note → indicator pulse | < 50 ms | local commit path |
| Stroke pen-up → pulse | < 50 ms | local commit path |
| Journal panel open | < 100 ms | 1k events on image |
| Rail summon/hide | < 100 ms, no grid reflow | always |
| Surface transitions (Grid⇄Look⇄Search) | < 100 ms perceived | always |

Budgets are acceptance criteria for M1 sign-off; regressions are bugs.
