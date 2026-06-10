# DOGFOOD-M2 — the founder-machine checklist (M2a pencil; M2b voice will extend this)

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
