# spec/CAPTURE.md — Sessions, Write Scope, and the Capture Pipelines

Status: Draft 1 for implementation. Closes gaps B1–B5 of `docs/SPEC-GAPS.md`.

This spec owns capture **semantics, timing, state machines, and data flow**.
Boundaries: UI owns surfaces, layout, interaction patterns (this spec defines
the data contracts UI renders); RUNTIME owns the ASR process and models (this
spec consumes a `Transcriber` stream and states its requirements); EVENTS owns
the event-row schema and fold rules (this spec defines capture-side payloads
and the moments events are minted). MUST/SHOULD/MAY are RFC 2119-normative.

## 1. Clocks

- **Wall clock**: UTC, RFC 3339 in JSON, used for event `ts`; never used for
  ordering or binding (kernel: order within a session = append order).
- **Capture clock**: host monotonic clock (`std::time::Instant`). All binding,
  linking, and idle decisions happen on the capture clock and are only
  *recorded* as wall-clock timestamps.
- **Stream clock**: ASR audio-stream position, ms from stream start; anchored
  to the capture clock at **first audio buffer submission** (`anchor_mono`);
  segment times convert as `t_mono = anchor_mono + stream_offset_ms`.
- **Error budget**: VAD-onset detection latency + clock conversion error MUST
  stay under **250 ms** combined; selection changes are human-paced, so this
  keeps binding correct (§13.1 enforces it).

## 2. Session lifecycle (closes B2)

A **session** is a contiguous period of app use; sessions are automatic.

### 2.1 Activity

**Activity** is any of: keyboard/pointer input to an app window (keydown,
drag, wheel, click, pen contact); selection or view-mode change (grid ↔
single-image, image navigation); mic arm/disarm; VAD speech activity (any
`SpeechStart`/`Partial`/`Final` while armed); journal-panel actions;
typed-note submission and rating keystrokes. **Not activity**: app focus
alone, passive hover, background ingest/backfill, sidecar-writer or
model-runtime activity.

### 2.2 State machine

```
            launch                          activity after ≥30 min idle
  ┌──────┐ ───────► ┌────────┐  ─────────────────────────────────────┐
  │ None │          │  Open  │ ◄────────────────────────────────────┐│
  └──────┘ ◄─────── └────────┘                                      ││
            clean      │  ▲                                         ││
            quit       │  └── any activity (refreshes idle timer)   ││
            (close)    ▼                                            ││
                  ┌─────────┐   close processing (§2.5)             ││
                  │ Closing │ ───────────────────────► closed ──────┘│
                  └─────────┘           new session opened ──────────┘
```

- Exactly one session is **Open** while the app runs; `session_id` is a ULID
  minted at session start; all events minted while open carry it.
- **Idle boundary**: when activity occurs and `now_mono − last_activity_mono ≥
  30 min`, the open session closes *before* the triggering activity is
  processed, with `ended_at` = wall-clock time of the **last** activity (not
  `now`); a new session opens and the triggering activity belongs to it. No
  timer fires during idle — closure is lazy (next activity, or quit), so a
  session's span never includes dead air at its tail.
- Speech is activity, so an idle boundary never bisects an in-flight
  utterance. A typed-only browse is a session; sessions span folders and view
  modes freely; mic toggles occur within sessions, never create them.

### 2.3 Session records

Sessions are **implicit in the event log** (the set of events sharing a
`session_id`; start = first event's `ts`). The SQLite index keeps a
rebuildable bookkeeping row — `(session_id, started_at, ended_at NULL while
open, closed_clean, close_processing_done)` — index-only, never in sidecars.

### 2.4 Crash recovery

On launch, before opening a new session: a `sessions` row with `ended_at IS
NULL` means the previous process died with the session open. The app MUST set
`ended_at` = ts of that session's last event (or `started_at` if none) and
`closed_clean = false`, then enqueue close processing (§2.5). Recovery mints
no events; a recovered session is indistinguishable from a cleanly closed one.

### 2.5 Session-close processing (hook points, in order)

1. **Capture drain**: mic disarms if armed; Transcriber stream ends; wait up
   to **5 s** for trailing `Final`s (they mint events into the *closing*
   session); after 5 s, in-flight utterances are abandoned (§6.5).
2. **Sidecar flush**: sidecar writer flushes pending events (SIDECARS spec).
3. **Close processors**: an ordered, registered list runs asynchronously —
   empty in M1/M2a; M2b+ registers session summary, per-image rolling-summary
   updates, sentiment scoring, annotation embedding — all *retrieval fuel
   only*, never user-facing prose (kernel). Processors are idempotent and
   resumable; if the app quits first, next launch re-enqueues
   (`close_processing_done = false`). Crash-recovered sessions use the same
   processors.

Steps 1–2 block quit (capped at 5 s + sidecar budget); step 3 never blocks.

## 3. Write scope model

The write scope is **always exactly one of**: `single` (one image), `multi`
(explicit multi-selection, N ≥ 2), or `session` (zero image targets). Scope
derives mechanically from selection/view state; the user never sets it.

| UI state | Scope |
|---|---|
| Single-image view | `single` — the viewed image |
| Grid, selection of 1 | `single` — the selected image |
| Grid, selection of N ≥ 2 | `multi` — selected images, in selection order |
| Grid, no selection | `session` — zero targets |
| Search results | same rules over result selection |

Multi target order = selection order, recorded as `event_targets.position`
(EVENTS spec). Scope changes are **instantaneous** for typed notes and ratings
(bind at submit/keystroke time); voice binds by VAD onset (§5); strokes always
bind to the viewed image (§8). Entering single-image view from a
multi-selection narrows scope to the viewed image; returning to the grid
restores the selection-derived scope.

### 3.1 Scope snapshots and the scope ring buffer

The capture layer maintains the current scope snapshot:

```rust
struct ScopeSnapshot {
    kind: ScopeKind,            // Single | Multi | Session
    targets: Vec<ContentHash>,  // ordered; empty for Session
    captured_at_mono: Instant,  // capture clock
    captured_at: DateTime<Utc>, // diagnostics only
}
```

Every scope change pushes the new snapshot into a **ring buffer**: capacity
**1024 entries or 120 s**, whichever fills first (covers any selection burst
and far exceeds the longest onset→final gap plus the §1 budget); in-memory,
per-process, never persisted. **Lookup**: `scope_at(t_mono)` = the snapshot
with the greatest `captured_at_mono ≤ t_mono`; if `t_mono` predates the oldest
entry (should be impossible), use the oldest and log a debug-panel warning.

## 4. Typed pipeline

UI provides a minimal input (UI owns placement/keys; recommendation: Enter =
submit, Shift+Enter = newline). **Submit** mints exactly one event:
`kind=remark`, `source=typed`, `text` = input verbatim, multi-line allowed.
**No markdown or other processing in v1** — stored byte-for-byte (the app MAY
trim one trailing newline, nothing else); empty/whitespace-only submissions
mint nothing. Binding: the **current scope snapshot at submit time** — no
ring-buffer lookup; typed input has no latency to compensate. Append → sidecar
writer notified → indicator pulse (§11). The typed path has zero model
dependencies and MUST work identically in degraded mode.

## 5. THE BINDING RULE — utterances bind at VAD onset (closes B1)

> **An utterance binds to the write-scope snapshot at utterance start — VAD
> speech onset — not at transcript arrival.**

Streaming ASR finalizes 0.5–2 s after the words; users click to the next image
while still talking about the previous one. Binding at onset attributes the
words to what the photographer was looking at when they started saying them;
binding at arrival attributes them to the wrong image.

### 5.1 Mechanism

On `SpeechStart { t_start_ms }` (silero-vad, in-process on the cpal stream —
§6.2): convert `t_start_ms` → `t_mono` (§1), call `scope_at(t_mono)` (§3.1),
and hold that snapshot for the utterance. On `Final { segment_id, text,
t_start, t_end, confidence? }`, the held snapshot's targets become the
event's targets (a `session` snapshot yields a session-level event, zero
targets).

**The VAD onset is authoritative for binding.** The `Final`'s `t_start` (ASR
token timestamp) is a **cross-check only**: transducer token times are
systematically late — RNN-T emission delay
([FastEmit](https://arxiv.org/abs/2010.11148)) — and may not exist at all for
the Nemotron export (RUNTIME's spike tests this). When present and it
disagrees with the VAD onset across a scope change by more than the §1
budget, the disagreement is logged to the debug panel; binding is **never**
silently re-decided from token times.

### 5.2 Grace window: none

**N = 0. Onset wins; no grace window in v1.** Speech starting 50 ms after a
selection change binds to the *new* scope. A grace window trades one
misattribution for another and adds an untunable knob; the onset rule is one
sentence — "it writes down what you say about what you're looking at."
Revisit only on dogfooding evidence (recorded as a future tunable, default 0).

### 5.3 Multi-segment utterances across a scope change

The ASR's endpointing segments continuous speech (§6.3). **Each final segment
binds independently by its own onset.** Stated deliberately: one continuous
monologue CAN split across two images — "…lovely gesture here" (image A),
arrow-key to B mid-breath, "—but this one has better light" (image B). This is
**correct**: words spoken while looking at B are about B. The journal records
judgments about frames, not paragraphs; session order preserves the monologue
for any reader of the session view.

### 5.4 UI contract for streaming utterances

While an utterance is in flight (`SpeechStart` → `Final`), the indicator MUST
show **the scope it is bound to** (`scope_at(onset)`), even if
the live selection has changed. The contract (§11) carries both
`current_scope` and `streaming_utterance.bound_scope` so the UI can render
"selection is now B, but what you're saying lands on A". The distinction is
mandatory; the rendering is UI's.

## 6. Voice pipeline

### 6.1 Mic toggle semantics

The mic control is a **toggle: arm / disarm** — not push-to-talk. Armed =
continuously listening; the ASR's VAD decides what is speech. Push-to-talk is
a recorded **future settings option** (same pipeline, app-gated audio feed).

### 6.2 Audio capture chain and Transcriber requirements

- **Capture is Rust-side via `cpal`** in photoproof-core — not webview/Tauri
  audio: no IPC hop, sample-accurate stream anchoring, audio never in the JS
  heap. Capture opens the default input device, downmixes to mono, resamples
  to **16 kHz f32**, pushes to the Transcriber.
- Required `Transcriber` stream interface (RUNTIME implements):
  - `push_audio(frames)` — 16 kHz mono f32.
  - Emits per utterance, in order:
    - `SpeechStart { t_start_ms }` — VAD onset; detection latency ≤ **300 ms**
      after true onset; `t_start_ms` is the *estimated onset*, not detection
      time.
    - `Partial { segment_id, text, t_start_ms, t_now_ms }` — zero or more;
      `segment_id` stable from first partial through final.
    - `Final { segment_id, text, t_start_ms, t_end_ms, confidence }` — exactly
      one per segment; `t_start_ms` authoritative (Nemotron streaming provides
      segment timing); `confidence` ∈ [0,1].
  - `end_stream()` — flush; trailing `Final`s may follow, then `Closed`.
    `Error { fatal }` — fatal = ASR process gone (§6.6). All times are
    stream-clock ms offsets (§1); the Transcriber MUST NOT re-emit or renumber
    segments.

### 6.3 Segmentation ownership

**The ASR's endpointing is authoritative.** Capture performs no VAD, silence
detection, or re-segmentation. One ASR final segment = one utterance.

### 6.4 Mic state machine

```
 ┌──────────┐ toggle  ┌────────┐ ready   ┌────────────┐
 │ Disarmed │ ──────► │ Arming │ ──────► │ Armed·Idle │ ◄────────────┐
 └──────────┘         └────────┘         └────────────┘              │
   ▲   ▲                 │ device/ASR fail   │ SpeechStart           │ last Final,
   │   │                 ▼                   ▼                       │ no speech
   │   │            Disarmed(error)   ┌────────────────┐            │ in flight
   │   └──────────────────────────────│ Armed·Speaking │ ───────────┘
   │        toggle, quit, or          └────────────────┘
   │        fatal ASR error (§6.6)     (≥1 utterance in flight)
```

`Arming`: open cpal stream, confirm ASR readiness with RUNTIME (which may
spawn/wake the ASR child), anchor the stream clock; failure →
`Disarmed(error)`, quiet notification. `Armed·Speaking` holds while any
utterance is in flight (a new `SpeechStart` may arrive while a prior segment
finalizes). Disarm: stop pushing audio, `end_stream()`, accept trailing
`Final`s up to **5 s** (they mint events normally — their onsets predate the
disarm), then drop the stream and zero the audio ring buffer (§7).

### 6.5 Utterance lifecycle

```
 SpeechStart        Partial*           Final              append
 ──────────► Streaming ──────► Streaming ───► Finalized ─────────► Committed
                 │                                              (event exists)
                 │ fatal ASR error / 5 s drain timeout
                 ▼
             Abandoned (nothing persisted; debug-panel note only)
```

- **Partials are never persisted.** They exist in memory solely for the
  dev-build debug panel and the indicator's "speaking" affordance; the
  indicator MUST NOT display partial text (kernel: no live transcript pane),
  the debug panel MAY.
- **Commit**: each `Final` mints exactly one event — `kind=remark`,
  `source=voice`, `text` = final text verbatim, bound per §5, with a capture
  payload (field placement per EVENTS spec):

  ```json
  { "asr": {
      "model_id": "nemotron-3.5-asr-streaming-0.6b",
      "confidence": 0.91,
      "speech_started_at": "2026-06-09T17:42:03.120Z",
      "speech_ended_at":   "2026-06-09T17:42:07.480Z" } }
  ```

  The VAD span (wall clock) is durable and sidecar-visible — stroke linking
  (§9) requires it.
- **No merging in v1.** Consecutive finals are NOT merged, regardless of gap
  or scope equality. One final = one event: simpler, preserves per-segment
  confidence/spans, and reversible later (merging can become a fold/display
  policy; the reverse migration would be impossible).
- Empty/whitespace-only finals mint nothing.

### 6.6 Error states

If the ASR process dies mid-session (fatal `Error`, or RUNTIME reports the
child gone): `Streaming` utterances are **Abandoned** — partials discarded,
nothing minted, a debug-panel entry records the loss; the mic **auto-disarms**
to `Disarmed(error)`, the ring buffer is zeroed, and the user is **notified
quietly** — the indicator's degraded state (§11), no modal, no toast storm;
re-arming retries via RUNTIME supervision. Typed notes, ratings, and the
pencil are unaffected. Degraded mode (below hardware floor / models absent) is
this state permanently: the mic control exists but arming fails quietly.

## 7. Audio policy (closes B4)

- **No audio is ever written to disk in v1** — not as files, not in SQLite,
  sidecars, or app-controlled crash dumps. Capture audio lives only in an
  **in-memory ring buffer**, **60 s** at 16 kHz mono f32 (~3.8 MB).
- A segment's audio is **discard-eligible at finalization + 5 s** (safety
  window for immediate ASR retry; "discard" = overwrite eligibility, not a
  deletion job). On disarm, quit, or fatal ASR error the whole buffer is
  zeroed immediately.
- Per-segment **ASR confidence is stored on the event** (§6.5) — the only
  durable residue of the audio.
- **Future setting (recorded, not designed): audio retention opt-in.** Would
  require: per-event clips keyed by event id in app data (never beside
  images), a retention-duration setting, redaction extended to delete clips
  (A3 anticipates this), an export stance (clips NOT in the sidecar export),
  and a privacy disclosure. Until then: the audio is gone seconds after you
  spoke.

## 8. Grease pencil (closes B5)

Available **only in single-image view**. One stroke = pen-down → pen-up = one
event: `kind=stroke`, `source=pencil`, bound to the **viewed image** (always
`single`; the scope ring buffer is not consulted).

### 8.1 Coordinate space and mapping contract

- Coordinates are normalized **(x, y) ∈ display-oriented image space** (EXIF
  orientation applied — same as the cached preview, per LIBRARY/D2):
  `x = px / W_display`, `y = py / H_display`, origin top-left, y down.
- The event records `orientation`: the EXIF orientation value (1–8) applied at
  draw time, so a tool later rewriting orientation metadata cannot rotate
  marks out from under the user (renderers detect mismatch and compensate;
  EVENTS/LIBRARY own that fold).
- **Pan/zoom mapping**: the single-image view maintains a view transform `T`
  (image px → screen px; uniform scale `s` + translation; no rotation in v1).
  Each pointer sample maps `p_img = T⁻¹(p_screen)`, then normalizes. Contract:
  a stroke drawn at any zoom/pan, stored, and re-rendered at any other
  zoom/pan MUST land on the same image pixels (tolerance ≤ 1 source-image
  pixel; §13.3). Rendering applies the current `T` to stored points; on-screen
  width = image-space width × `s` (marks zoom with the image, like grease
  pencil on film).
- Points MAY extend past the frame (circling an edge subject): stored values
  clamp to **[−0.25, 1.25]** (encoded as integers −2500..12500, §8.2);
  renderers clip to the visible overlay.

### 8.2 Stroke payload schema

Stored in the event's `payload`; **the canonical encoding is normative in
EVENTS §3.3** (canonical JSON is integer-only — no floats). Shape:

```json
{
  "base_w": 40,
  "orientation": 1,
  "points": [[4312, 2210, 1000, 0], [4330, 2204, 820, 9], [4391, 2188, 770, 17]],
  "tool": "pencil"
}
```

- `tool` — `"pencil"` is the **only** tool id in v1 (the pencil is red; color
  is a property of the tool, not the payload). `orientation` — EXIF
  orientation at draw time (§8.1).
- `base_w` — stroke base width in **ten-thousandths of the display-oriented
  long edge**; default **40** (0.4 % of long edge); recorded per stroke so a
  future width control needs no schema change.
- `points` — `[x, y, p, t]` tuples in capture order, ≥ 1, ≤ 8192. `x`,`y`:
  integer ten-thousandths of the display-oriented extent (§8.1), range
  −2500..12500. `p`: pressure per-mille 0..1000; **1000 when the device
  reports none** (mouse, basic touch). `t`: integer ms offsets from pen-down
  (`t[0] = 0`, non-decreasing), for future time-scrubbing (M4). The event's
  `ts` is the **pen-up (commit) time**; pen-down = `ts − t_last` (no separate
  start field).
- **Width model**: rendered width `w(i) = base_w × (0.4 + 0.6 × p[i]/1000)` —
  no pressure (p = 1000) renders constant base width; pressure pens thin to
  40 % at zero. Renderers interpolate width along the path.

### 8.3 Input, sampling, smoothing

Pointer events — mouse, pen (pressure where the platform provides it), touch —
share one path; use coalesced/high-frequency samples where available. **Raw
points are stored unsmoothed**; capture-side reduction is limited to dropping
consecutive samples closer than **0.5 screen px** (jitter dedupe, lossless at
display resolution). Smoothing is **render-only**: centripetal Catmull-Rom
through stored points (a one-euro filter MAY also smooth the live in-progress
stroke for feel). Stored data is the witness; rendering taste changes freely.

### 8.4 Stroke lifecycle and commit threshold

```
 pen-down        samples        pen-up
 ───────► Drawing ──────► Drawing ──┬─────► Committed (event minted)
                                    │ below threshold or pointer-cancel
                                    └─────► Discarded (no event)
```

- A stroke is **not an event until pen-up**; pointer-cancel (palm rejection,
  window loss) discards it, nothing logged.
- **Commit threshold**: discard iff total path length < **0.003** (normalized
  long-edge units) **and** duration < **100 ms** — a deliberate press-and-hold
  dot commits, a fleeting accidental tap does not.
- On commit: event minted, indicator pulses, stroke pushed onto the undo stack
  (§8.5), link resolution runs (§9).

### 8.5 Undo (retraction-based, never deletion)

- Before pen-up there is nothing to undo (the stroke doesn't exist yet). After
  commit, **Ctrl+Z** (Cmd+Z on macOS) mints a **retraction (tombstone) event**
  targeting the most recent non-retracted stroke on the undo stack; the stroke
  event is preserved, folded out (kernel). Undo is never deletion.
- **Undo stack**: depth **10**, session-scoped, in-memory per process (cleared
  at session close and restart); contains only strokes authored this session
  by this process. Empty stack → Ctrl+Z is a no-op in the pencil layer.
- **No redo in v1** — the user re-draws (recorded as future polish). Older
  strokes (prior sessions) are retracted via the journal panel or eraser, not
  Ctrl+Z.

### 8.6 Eraser

v1 eraser = **tap-to-retract a whole stroke**; no partial erase. Hit test: map
the tap to image space (§8.1); eligible if min distance from tap to the
stroke's polyline ≤ `max(0.01 normalized long-edge units, 12 screen px / s)`
(`s` = zoom scale) — at least a 12-px target at any zoom. Among eligible
strokes the **most recent** (latest event id) wins — topmost in render order.
Erase mints a retraction event; redacting a stroke (geometry scrubbed) is a
journal-panel flow (§12.3), not the eraser.

## 9. Stroke ↔ utterance linking

### 9.1 The rule

For stroke S (span = pen-down … pen-up, i.e. `ts − t.last` … `ts`) and utterance U
(VAD span = `speech_started_at` … `speech_ended_at`), candidates being
committed events **within the same session** only:

1. **Overlap**: spans overlap → link (several candidates: greatest overlap
   duration wins).
2. **Nearest fallback**: else, nearest candidate with span-gap ≤ **10 s**, *in
   the same scope context*: the utterance's bound target set must contain the
   stroke's image (session-level utterances never link via fallback).
3. Else **unlinked** — normal and permanent; silence while marking is common.

### 9.2 Who carries the link (append-only consequence)

Events are immutable, and the transcript usually finalizes *after* pen-up — a
late link cannot be written onto the earlier event. Therefore:

> **The link lives on the LATER-committed event only, as `linked_event`
> pointing backward.** Link resolution runs exactly once per event, at its
> commit, over already-committed events. Folded views (EVENTS spec) MUST
> traverse `linked_event` in **both** directions: "linked" is symmetric,
> realized as a one-way stored pointer — a direct consequence of append-only
> that EVENTS must honor.

- **In-flight suppression**: at stroke commit, if an utterance is currently
  streaming (`SpeechStart` seen, no `Final`) and its span-so-far overlaps the
  stroke, the stroke commits **unlinked** — the utterance carries the link
  when it commits, and the stroke is kept from grabbing a wrong
  nearest-fallback partner while its true partner is still in the air.
- Each event has **at most one outgoing** `linked_event`; an event MAY receive
  multiple incoming links (three quick circles during one sentence → three
  strokes linking back); folds render the connected component. An abandoned
  utterance (§6.5) was never committed and can never be linked.

## 10. Ratings

During culling, keys **0–5** mint `kind=rating`, `source=typed`, value 0–5,
bound to the **current scope at keystroke time** (instantaneous, like typed
notes). **0 explicitly clears** (a rating event with value 0; fold = last
rating wins — EVENTS owns fold and display). **Multi-select applies to all** —
confirmed, desired: with N selected, one keystroke mints **one event targeting
all N** (not N events), matching every culling tool photographers know and
keeping "I rated these together" recoverable from the log. Session scope (no
selection): rating keys do nothing. Display comes from the fold (EVENTS).

## 11. Write-scope indicator — data contract

UI renders the small persistent indicator; capture feeds it, pushing on change:

```rust
struct IndicatorState {
    current_scope: ScopeView,          // what a typed note would bind to NOW
    mic: MicState,                     // Disarmed | Arming | ArmedIdle
                                       //   | ArmedSpeaking | DisarmedError
    streaming_utterance: Option<StreamingView>,
    degraded: DegradedFlags,           // { asr_unavailable: bool, .. }
}
struct ScopeView {
    kind: ScopeKind,                   // Single | Multi | Session
    count: usize,
    preview_hashes: Vec<ContentHash>,  // first ≤3, for thumbnails
}
struct StreamingView {
    bound_scope: ScopeView,            // scope_at(onset) — §5.4
    started_at: DateTime<Utc>,
}
```

Plus a transient **pulse signal** `IndicatorPulse { event_kind }` on every
event commit (remark, rating, stroke, revision, retraction); fire-and-forget,
and **no text content ever rides the indicator channel** (no live transcript —
kernel). Required renderable distinctions (UI chooses how): current scope
(count + thumbnails); mic armed vs not; **a still-streaming utterance bound to
a scope differing from the current scope**; the degraded "mic intent on but
ASR down" state (`DisarmedError` + `asr_unavailable`), quiet and non-modal.

## 12. Corrections, retraction, redaction (capture-side semantics)

### 12.1 Transcript correction (closes B3)

- From the journal panel (UI owns the surface), the user edits a voice
  remark's text; submit mints `kind=revision`, `source=typed`, referencing the
  target event and carrying the **full corrected text** (not a diff). **No
  length or diff restrictions.** Typed remarks are correctable identically.
- **Folded text** — the latest revision in the chain — is what display, FTS,
  and embeddings use (kernel; EVENTS owns the fold); the original stays in the
  log and sidecars. **Revisions of revisions**: a new revision references the
  *original* target event; the chain folds to the latest by append order
  (capture always submits against the original event id underlying the folded
  text it displayed).
- Correction MUST trigger FTS re-index and embedding refresh of the folded
  text (RETRIEVAL owns mechanics; the hook fires here).

### 12.2 Retraction

v1 retraction ("strike that") is a **journal-panel action** plus the pencil
flows of §8.5/§8.6: a tombstone event referencing the target; content
preserved, folded out of UI/search/context. **Voice-command retraction**
(spoken "strike that") is a recorded future feature — it needs
command-vs-content disambiguation v1 does not attempt; until then the phrase
simply lands in the journal, which is itself honest marginalia.

### 12.3 Redaction

- Explicit **two-step confirm** (UI owns the dialog; the second step MUST name
  the consequence: content removed everywhere, unrecoverable).
- Semantics (the one sanctioned append-only violation — normative in EVENTS
  §7): `text` and `payload` are **removed entirely** and `redacted_by` marks
  the act — for voice remarks that scrubs the ASR capture payload (confidence,
  duration) with the text; for strokes it scrubs the entire geometry
  (`points`, `base_w`, the lot). Preserved: event id, `v`, `ts`, `session_id`,
  kind, source, targets, `target_event`, `linked_event` — structure survives,
  content dies.
- Propagation trigger (capture fires; SIDECARS/RETRIEVAL own mechanics):
  rewrite the sidecar of **every targeted image** (queued until offline
  volumes mount); purge the event's FTS rows and vectors; when the future
  audio-retention setting exists, delete any retained clip. Redaction wins
  over any merge (kernel): the redaction marker is what propagates. Redacting
  an event does not redact linked events; the confirm dialog SHOULD surface
  them so the user can redact both.

## 13. Acceptance criteria

1. **Binding under rapid selection change (scripted).** With a Transcriber
   stub: segment 1 onset at T; selection A→B at T+800 ms; segment 1 finalizes
   T+2000 ms; segment 2 onset T+1100 ms, finalizes T+3000 ms. Segment 1
   targets A, segment 2 targets B — regardless of finalization times. A
   selection change 50 ms before an onset binds the new scope (no grace).
2. **No audio on disk.** A full armed session (speech, finals, disarm, quit)
   leaves zero audio bytes in app data, library tree, SQLite, sidecars, or
   temp dirs (filesystem snapshot diff + format scan).
3. **Stroke round-trip fidelity.** Draw one stroke at 100 % zoom, one at
   400 % panned to a corner; persist; restart; re-render at unrelated zoom/pan
   states. Every rendered point lies within 1 source-image pixel of the
   original image-space path; `p[]`/`t[]` round-trip exactly; payload
   validates against §8.2.
4. **Undo = retraction.** Draw 3 strokes; Ctrl+Z twice → two tombstones
   appended, zero rows mutated/deleted; the strokes vanish from overlay and
   fold but remain in log and sidecar; rebuild-from-sidecars reproduces the
   same folded state.
5. **Correction folding visible in search.** Voice remark "muddy light",
   corrected to "moody light": FTS "moody" returns the image with the
   corrected quote as provenance; "muddy" returns nothing; the journal panel
   shows corrected text with the original in history.
6. **Session boundaries.** 29-min gap → same session; 31-min gap → next
   activity opens a new session (first event carries the new session_id), old
   session's `ended_at` = its last activity time. Kill the process mid-session
   → next launch sets `ended_at` = last event ts, enqueues close processing
   once.
7. **ASR death.** Kill the ASR child mid-utterance: no event minted from the
   partial, mic auto-disarms, indicator shows degraded state, and a typed note
   submitted immediately after lands normally.
8. **Linking.** (a) Stroke inside an utterance's VAD span whose final lands
   after pen-up → the utterance carries `linked_event` → stroke. (b) Stroke
   4 s after an utterance ends, same image → the stroke carries the backward
   link. (c) 11 s after → unlinked. (d) Stroke near a session-scoped
   utterance, no overlap → unlinked (scope gate).
9. **Multi-select rating.** Select 5 images, press 3 → one rating event with
   5 ordered targets; each of the 5 sidecars contains it; rebuild dedupes to
   one event.

## 14. Recorded-future ledger (deferred on purpose)

Push-to-talk as a settings option (§6.1) · onset grace window as a tunable,
default 0 (§5.2) · audio retention opt-in (§7) · voice-command retraction
(§12.2) · segment merging as a display/fold policy (§6.5) · redo for pencil
undo (§8.5) · additional pencil tools/colors/widths and partial erase (§8.6).
