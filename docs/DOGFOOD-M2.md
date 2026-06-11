# DOGFOOD-M2 — the founder-machine checklist (M2a pencil + M2b voice)

P5.1 shipped the grease pencil with geometry and persistence property-tested
(CAPTURE §13.3/§13.4 traced in the suite). Everything below is the
eyes-and-hands half that needs your real hardware. Same drill as
DOGFOOD-M1: a checklist, not archaeology.

## 1. Pencil feel (CAPTURE §8, UI §4.4)

- [ ] `B` toggles pencil mode in Look; the red-dot cursor plus the
  indicator ✎ segment are the ONLY announcements — no toolbar, no palette,
  zero added chrome (UI §4.6 eyes-only acceptance row).
- [ ] Draw at speed: the live stroke tracks the pen with no visible lag;
  the committed stroke (render-time Catmull-Rom) matches what your hand
  did. The optional one-euro live filter (§8.3 MAY) was deliberately
  skipped — if the live stroke wobbles on real pen hardware, that is the
  recorded fix (BACKLOG, M2a).
- [ ] Wheel-zoom mid-stroke and after: marks zoom with the image like
  grease pencil on film; strokes land on the same image pixels at any
  zoom/pan (the geometry half is property-tested; this is the visual half).
- [ ] `Space`+drag pans while pencil is on — no dropped first-pan-sample
  feel (the overlay yields pointer-events on Space). Plain drag still pans
  with pencil off. A clean Space tap at fit does NOT close Look while
  pencil is on (U14).
- [ ] `O` toggles the tracing-paper overlay; toggling it off while in
  pencil mode exits pencil mode; toggling back on, strokes return exactly
  as drawn at any zoom (UI §4.6 row).
- [ ] Pencil mode persists across ←/→ within Look and resets to off on
  return to Grid.

## 2. Pressure (UI §4.4 — progressive enhancement, not a baseline)

- [ ] Windows pen hardware: p[] varies — strokes thin toward 40 % width at
  zero pressure. Wacom driver: the "Use Windows Ink" toggle must be ON
  (the named support-doc item).
- [ ] macOS: constant base width (pressure does not reach WKWebView — the
  spec'd norm there, not a bug).
- [ ] Linux stylus (your machine): TBD by design — record what reaches the
  webview; constant width is the acceptable fallback.

## 3. Eraser & undo (CAPTURE §8.5–§8.6)

- [ ] Hold `E` in pencil mode: hollow-circle cursor; a tap retracts the
  whole topmost stroke. Radius feels right at high zoom AND the 12-px
  floor works zoomed far out; on dense overlapping marks the latest wins.
- [ ] The stylus eraser end (if your pen has one) erases without holding E.
- [ ] `Ctrl+Z`: retracts newest-first, depth 10, this-session only; empty
  stack does nothing (and does not eat Ctrl+Z in text inputs); the journal
  panel shows the tombstones; strokes never reappear after undo.
- [ ] `Ctrl+Z` mid-stroke (pen still down) cancels the in-progress mark
  silently — nothing lands in the journal.

## 4. Journal panel (UI §8)

- [ ] Stroke rows render micro-previews over the thumbnail; legible at row
  width (stroke widths get thin on small thumbs — a render-only floor is
  in place).
- [ ] Clicking a stroke row while in Look flashes that stroke on the
  overlay (~700 ms).

## 5. Budgets (UI §13)

- [ ] Pen-up → indicator pulse < 50 ms (one IPC command + one event emit —
  needs on-target measurement).
- [ ] Overlay redraw while wheel-zooming an image with MANY strokes stays
  smooth (full repaint per frame; fine in reasoning, unmeasured on floor
  hardware).
- [ ] Cursor visibility: the red dot over bright/red-heavy images; the
  eraser ring legibility.

## 6. Voice capture (P6.1 engine — feel verifiable once P6.2 supervision and P6.3 real models land)

The engine is mock-verified end-to-end (CAPTURE §13.1/2/5/6/7/8 traced);
everything below needs a real mic, the real ASR child, or your eyes.

- [ ] **Binding feel (the B1 rule, real mic)**: arm, speak while arrowing
  through images mid-sentence — each remark must land on the image you
  were LOOKING AT when you started the words. The debug panel's Capture
  feed logs "token-time cross-check" entries only when ASR-vs-VAD onset
  disagreement exceeds 250 ms across a scope change: frequent entries =
  silero detection latency eating the §1 budget on this machine (the
  spike's ONSET_ERROR_BUDGET_MS is the constant to tune against).
- [ ] **Indicator legibility at 24 px**: five mic states (absent / dim
  disarmed / solid armed / breathing while speaking / struck-muted
  degraded) plus the streaming tether — verify the tether reads as "words
  land on the earlier selection" without explanation, the 2.4 s
  opacity-only breathing feels quiet, and the degraded glyph's one-line
  hover shows. The mic glyph stays absent until P6.2 reports ASR ready.
- [ ] **Disarm honesty (macOS especially)**: speak, disarm immediately —
  the trailing sentence still lands within ~5 s, then the OS mic indicator
  (orange dot) dies with the stream (closed, never paused).
- [ ] **ASR kill drill (§13.7, once P6.2 lands)**: kill the ASR child
  mid-sentence — only the mic glyph changes (muted), nothing minted from
  the partial, and a typed note lands instantly.
- [ ] **B41 stroke ends**: the terminal pen-up sample adds one
  near-duplicate end point per stroke — confirm no visible hook/blob at
  stroke ends at 400 % zoom; press-and-hold dots still commit, accidental
  taps still vanish.
- [ ] **Journal link marks**: circle a detail while talking about it —
  both the stroke row and the remark row show the quiet linked mark;
  clicking the stroke row still flashes the overlay stroke.

## 7. Runtime (P6.2 — surfaces verifiable NOW on this machine; real children with P6.3)

- [ ] **Consent card**: launch with a fresh app-data dir and one watched
  root — the quiet card appears (bottom-right, above the indicator) with
  the live byte sum, per-model license links, and Accept-license buttons
  gating Download; "Later" re-offers only from settings; "Never" is
  remembered. Skipping changes nothing about journaling.
- [ ] **Settings → Models** (this machine detects Tier 1 — see the
  checklist's tier-gate item): offered rows with not-downloaded states,
  license links, Restart runtime + Re-detect hardware actions. Pressing
  Download against the unpinned manifest surfaces the TLS-deferred error
  in the row (expected until P6.3 pins artifacts and picks the TLS
  client) — quiet, no dialog.
- [ ] **Single instance**: launch a second app instance — it focuses the
  first and exits; the second's debug Runtime tab (if reached) reports
  instance_lock_held = false.
- [ ] **Debug panel → Runtime tab**: plan/tier/lock/orphan-sweep/download
  lines + the full status snapshot render; supervisor state histories and
  scheduler decisions join it when real children exist (P6.3).
- [ ] **macOS shutdown mechanics** (when you target a Mac): normal quit
  leaves no children; Activity Monitor force-quit relies on the
  children.json net at next launch; kinfo_proc start-time matching.
- [ ] **Windows** (first Windows build): Job Object KILL_ON_JOB_CLOSE and
  the DXGI DedicatedVideoMemory probe are code-complete behind cfg but
  have NEVER compiled — compile-check and run them before trusting them.
- [ ] **sherpa wire contract** (P6.3): the WS client assumes result JSON
  carries segment/start_time/is_final and optional ys_probs — confirm the
  pinned sherpa-onnx build's exact field shapes, and whether token
  timestamps exist (RUNTIME §3.2 open item; B49's cross-check wants them).
