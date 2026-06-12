# Spec Gap Analysis & Spec Completion Plan

A hole-poking pass over SCOPE.md and M1-BUILD-PLAN.md, June 2026. Verdict: the
*architecture* is sound, but the spec is not implementation-complete. The gaps
cluster in exactly the places that are expensive to change later — on-disk
formats and event semantics — plus two product-level tensions nobody has
resolved. This doc inventories the holes, then defines the spec documents that
must exist (and be internally consistent) before code.

Guiding rule for "completely done": **freeze what is expensive to change,
iterate what is cheap to change.** Event semantics, sidecar formats, identity,
and merge rules get specified to 100% — they end up in users' file trees and
can never be casually migrated. UI ergonomics and retrieval ranking are
cheap to change and should get acceptance criteria, not pixel-level spec;
spec-freezing those would be false precision.

---

## A. Schema-level holes (the data model cannot express the product as drawn)

### A1. Multi-image annotations cannot be represented
`annotation_events.image_hash` is a single nullable column, but multi-select
("speaking about: 3 images") is a core flow named in the scope doc itself. As
drawn, a remark about exactly 3 images is representable only as (a) a
session-level event — losing the binding — or (b) three duplicated rows —
breaking single-source-of-truth (tombstoning one of three copies?).
**Fix:** events get a separate `event_targets` join table (event_id →
image_hash, ordered). An event targets 0 images (session-level), 1, or N.
Sidecar mirroring writes the event into every targeted image's sidecar;
rebuild dedupes by event id (ULIDs make this trivial).

### A2. `embedding_id` FK points the wrong way
The scope doc says model swaps are "a background re-index job, not a migration
crisis," with old vectors queryable until the new index completes. But
`annotation_events.embedding_id → vectors` allows exactly one embedding per
event. **Fix:** invert it — `vectors(event_id, model_id, dims, vec)`; an event
has zero-to-many embeddings. Embeddings are derived data and do not belong in
the canonical event row at all (they also must not leak into sidecars).

### A3. Redaction is self-contradictory
"Redaction = tombstone event" coexists with "rows are never updated or
deleted" and with plaintext sidecars beside the user's images. A tombstone
that leaves the original utterance readable in a JSON file is not redaction —
and an accidental capture (a phone call, another person in the room) is a
*when*, not an *if*, for an always-on mic workflow. **Decision needed, not
deferral:** distinguish *retraction* (tombstone: hidden from UI/search/context
assembly, content preserved — the "changed my mind" case) from *redaction*
(content physically scrubbed: the event row keeps id/ts/kind for log
continuity, `text` is overwritten with a redaction marker, sidecar rewritten,
FTS/vector entries purged, audio deleted if retained). Redaction is the one
sanctioned violation of append-only, and the spec must say so in exactly those
words, including its propagation to sidecars on offline volumes (queued until
the volume mounts).

### A4. No fold rules — "current state" is undefined
Append-only logs need defined fold functions to answer simple questions. What
is an image's *current rating* (last rating event wins?)? What does the UI
show when a `revision` corrects an earlier remark? What does FTS index — the
original, the revision, or both? Every event `kind` needs: its payload schema,
its fold behavior (how it affects derived current state), its search/indexing
behavior, and its sidecar representation. Right now `kind` is an enum of
seven words. This is the single largest spec gap.

### A5. `source` vs `kind` overlap
`source=markup` and `kind=stroke` are redundant; `source=system` is
unspecified (what system events exist? ingest? relink? model re-pass?).
Tighten: `source` = how it entered (voice/typed/pencil/system), `kind` =
what it means; define the valid matrix.

## B. Capture semantics (the race conditions are product decisions)

### B1. The utterance↔selection binding race
Streaming ASR finalizes a segment ~0.5–2s after the words were spoken. Users
click to the next image *while talking about the previous one*. If events bind
to "current selection" at transcript-arrival time, the journal silently
attributes words to the wrong image — and an ambiguous write scope "pollutes
the journal, and the journal is the product" (scope doc's own words).
**Rule needed:** an utterance binds to the selection snapshot at
*utterance start* (VAD speech-onset), not at transcript arrival, with a
specified grace window; the UI shows which image(s) a still-streaming
utterance is bound to. This rule must be in the spec before M2 is designed.

### B2. Session lifecycle is undefined
"Session" appears throughout (session_id, session-level remarks, session
summaries, recency trail) but is never defined. When does one start/end —
app launch? mic toggle? idle timeout? Can a session span folders? Is a
typed-notes-only browse a session? Define it once; everything in M2–M4 hangs
off it. (Proposal: session = app-run segmented by >N-minutes idle; mic
toggles do not create sessions, they occur within one.)

### B3. Transcript correction flow
ASR will mishear ("moody" → "muddy"), and the photographer's own words are
the primary retrieval signal — uncorrectable transcripts poison retrieval.
`kind=revision` exists but is unspecified. Spec: a revision event references
the target event, carries corrected text; display and FTS/embedding index the
*folded* (corrected) text; the original remains in the log; sidecars carry
both.

### B4. Audio retention — undecided, must be decided
Keep raw audio or discard post-transcription? Affects privacy story, storage
(hours per cull session), redaction (A3), and correction (B3 — re-listening
is the only way to fix a transcript you don't remember saying). Proposal:
v1 discards audio once the segment is finalized + N seconds; retention is a
visible setting later. Whatever the answer, it must be written down.

### B5. Grease pencil — thin everywhere it touches disk
Underspecified, in order of how expensive each is to get wrong:
- **Coordinate space vs orientation:** "normalized image coords" — relative
  to the stored pixels or the EXIF-rotated display orientation? (Must be:
  display orientation, with the orientation value recorded in the stroke
  event; otherwise marks rotate out from under the user when a tool
  rewrites orientation metadata.)
- **Event granularity:** one stroke = one event (pen-down→pen-up), strongly
  preferred — a circle drawn in three strokes is three events sharing
  session + linked utterance.
- **Eraser/undo:** undo before commit = local, never logged; after commit,
  erasing a stroke = tombstone (retraction) of the stroke event. There is no
  partial-stroke erase in v1.
- **Tools:** v1 = one red grease pencil, period. Pressure recorded when
  available, rendering may ignore it initially.
- **Stroke↔utterance linking:** "an utterance in progress" — define the
  window (stroke links to any utterance whose VAD span overlaps the stroke's
  time span; else nearest within ±N seconds; else unlinked).

## C. Identity, sidecars, library (the part users carry for 20 years)

### C1. Same hash, multiple paths — and RAW+JPEG pairs
Content-addressing makes byte-identical copies *one image* (correct, but the
browser must render that sensibly: one journal, N locations). The sharper
hole: cameras shooting RAW+JPEG produce two files, two hashes, one frame —
and photographers think in frames. Lightroom solved this with stacking. V1
stance must be explicit: no stacking; RAW and JPEG are separate images with
separate journals; a pairing heuristic (same basename, same capture time,
same folder) is a recorded *future* feature so its absence is a decision,
not an oversight.

### C2. Sidecar merge rules — the spec is one sentence away from a superpower
Two machines, a restored backup, a sidecar newer than the DB: all the same
problem. Because events are immutable and ULID-keyed, the event log is a
grow-only set and **merge = set-union by event id** — order-independent,
conflict-free, no clocks compared. The spec should state this as a core
invariant ("any two copies of a journal merge by union; nothing else is ever
needed"), because it quietly makes multi-machine sync, backup restore, and
re-found sidecars all the same trivial code path. Corollary to specify:
redaction (A3) must win over union — a redacted event re-imported from a
stale sidecar must not resurrect; the redaction marker must be the thing
that propagates. Tombstones already do this for retraction.

### C3. The overflow store is canonical truth hiding in app data
Sidecars for unwritable volumes live in app data — meaning for archive-drive
users, the "sidecars are the truth" promise quietly degrades to "the truth is
in our app data dir." Spec: the overflow store uses the *identical* per-image
sidecar format (so it is the same parser and the same export), and the
permanent export feature is defined now: export = the complete set of
per-image sidecar JSONs + a manifest, which is also exactly what
rebuild-from-sidecars consumes. One format, three roles (adjacent sidecar,
overflow entry, export).

### C4. Volume identity is hand-waved
"Volumes are tracked as their own entity" — by what key? Mount points and
drive letters are unstable; filesystem UUIDs behave differently across
macOS/Windows/Linux and across exFAT archive drives. Spec the identity
recipe per-platform plus the fallback (a `.photoproof-volume` marker file
with a generated id at the volume root — ugly, reliable, and portable).

### C5. Clock discipline
ULIDs and `ts` come from the wall clock; culls happen on planes and across
DST. Spec: timestamps are UTC; ULID generation is monotonic within a
process (the `ulid` crate supports this); event *order* within a session is
the log order, never re-sorted by ts.

## D. Ingest & rendering

### D1. Embedded previews can take RAW decode off M1's critical path
The build plan budgets for rawler + libraw fallback in M1. But nearly every
RAW contains a camera-generated full-size JPEG preview, extractable in
milliseconds without demosaicing. **M1 should ship on embedded previews**
(extract → orient → cache), with full RAW decode demoted to a versioned
backfill pass for files with missing/tiny previews. This removes M1's
largest technical risk, makes 50k-image first-run dramatically faster, and
photographers culling JPEG previews is literally what tethered/camera
workflows already do. Full-decode quality matters for the *loupe*, not the
journal — schedule it accordingly (M1.5/M2).

### D2. Color & orientation stance
Spec one sentence each: previews are converted to sRGB at cache time
(wide-gamut display correctness deferred, recorded as a known limitation);
EXIF orientation is applied at cache time and the cached preview is always
display-oriented (this also simplifies B5 coordinates).

### D3. Staged ingest, stated as such
The scope doc's pipeline (hash → decode → thumb → embed → caption) reads as
one pipeline, but embed/caption are M3. The spec should define ingest as
versioned, independent *passes* over the library (hash/preview/EXIF in M1;
embedding and caption passes added in M3 as backfills), each recording
`(pass, version, model_id)` per image — which the doc already implies with
caption versioning; make it the explicit architecture of ingest, because
it is also the answer to "what happens when models improve."

## E. Retrieval & the AI seam (the user-asked "end-to-end" gap)

### E1. Nobody parses the natural-language query
M3 promises "pull up the images I was considering for that quieter,
melancholic series" — that requires decomposing NL into structured filters
(date, collection, rating) + semantic query text. Embedding the raw sentence
and hoping is not a plan; "that series I shelved last winter" has temporal
and collection clauses embeddings won't honor. Spec the query pipeline:
local LLM (same llama.cpp seam) parses the query into a typed filter AST +
semantic remainder → candidate generation across the four signals → rank
fusion (name the algorithm: RRF, k=60, it's fine) → group hits by image →
present. Free tier includes the local LLM by M3, so there is no "free tier
has no parser" problem — but state it, and spec the fallback when the parse
fails (treat whole query as FTS + vector).

### E2. Results must show *why* — in the photographer's own words
The retrieval identity is "matches against your own words," so every hit
must surface the matching quote (with date and session context), not just a
thumbnail grid. This is the difference between the product thesis and a
worse Lightroom search, and it constrains the index design (store
event-level provenance with every vector/FTS hit — which A2's
vectors-reference-events direction gives for free). Acceptance criterion,
not afterthought.

### E3. Embedding granularity
Embed per-event? Per rolling summary? Long monologues chunked how? Proposal
to spec: embed (a) each text-bearing event, chunked at ~512 tokens with
overlap, and (b) each per-image rolling summary, as distinct `VecKind`s;
fusion may weight them differently. Cheap to revise (re-index), but the
initial recipe should be written down.

### E4. Visible summaries contradict "not an AI product" — pick a lane
The positioning section promises AI invisible-as-a-B-tree; M2 ships "session
summaries" generated by an LLM. If users *read* model-written summaries of
their own creative sessions, that is precisely the "machine narrating your
art" experience the positioning forswears — the first sloppy
hallucinated summary torches trust with this exact audience. Decision
required: (recommended) summaries are *retrieval fuel only* — context
assembly, search, recency trail — never rendered as prose in the UI; the
user-facing journal is always verbatim their own words and marks. If a
visible digest is ever wanted, it's quotes-only (extractive), no generation.
Write the decision into SCOPE.md positioning *and* the M2 spec.

### E5. The model runtime is the biggest unbuilt system in the plan
"Bundle or manage a llama.cpp server child process" is one line of the scope
doc and several weeks of unglamorous engineering: weight distribution
(who downloads 3–16 GB? from where? resumable? licensed how?), first-run
hardware detection and tier selection, VRAM arbitration with Capture One
*and* with our own ASR (the scope doc already admits contention), crash/
restart supervision, port management, model updates, and the
below-hardware-floor experience (graceful "typed notes + FTS only" mode —
which M1 conveniently already is). This deserves its own spec and an
**early technical spike** (during M1, in parallel) so its surprises arrive
before M2 depends on it, not during.

## F. Reading the journal (the forgotten half of the UX)

The scope doc specifies capture and retrieval exhaustively and the *reading*
experience almost not at all — but "it remembers" is only believable if
remembering is visible. Needs spec (acceptance-criteria level, per the
guiding rule): the per-image journal panel (chronological, sessions
delimited, strokes shown inline at their timestamps, retracted items
hidden, revisions folded), the session-history view, and the overlay
time-scrub even in its M2 minimal form (all-strokes-on/off toggle first,
scrubbing M4). Performance budgets belong here too: grid scroll at 60fps on
20k-item folders, <100ms search-as-you-type, <5s from "note typed" to
"sidecar on disk" (the promise the M1 plan already makes).

---

## The spec set to write before implementation

Six documents; each gap above is assigned. "Done" = another engineer could
implement from the spec alone without asking a product question.

| Spec | Covers | Gaps closed |
|---|---|---|
| `spec/EVENTS.md` | Event model: every `kind`'s payload, fold, index, and sidecar behavior; multi-image targets; retraction vs redaction; revisions; ULID/clock rules; merge=union invariant | A1–A5, B3, C2, C5 |
| `spec/SIDECARS.md` | The JSON schema itself (literal, versioned, with examples); placement; overflow store; export-=-sidecars+manifest; rebuild & dedupe; redaction propagation | A3, C2, C3 |
| `spec/LIBRARY.md` | Identity & hashing; volumes & identity recipe; relink; offline; RAW+JPEG stance; embedded-preview ingest; staged passes; color/orientation | C1, C4, D1–D3 |
| `spec/CAPTURE.md` | Session lifecycle; write-scope binding rule (utterance-start snapshot); typed path; voice pipeline; audio retention; the complete grease-pencil spec | B1–B5 |
| `spec/RETRIEVAL.md` | Index recipes; query pipeline (LLM parse → AST + semantic → RRF); provenance/"show the quote"; embedding granularity; context-assembly budgets; summaries-as-fuel-only | E1–E4 |
| `spec/RUNTIME.md` | Model runtime: weight distribution, hardware tiers & floor behavior, llama.cpp supervision, VRAM arbitration, ASR sidecar lifecycle | E5 |

Plus `spec/DECISIONS.md` — a short ADR log so resolved questions stay resolved.

UI mockups/flows for the journal panel and search results (F) ride along as
acceptance criteria inside CAPTURE/RETRIEVAL rather than a frozen UI spec.

## Revised implementation order

```
Phase 0  SPEC        Write the six specs above; reconcile SCOPE.md with them
                     (schema fixes A1/A2 land in SCOPE.md at the same time).

Phase 1  M1 SPINE    As planned, with two changes:
                     • ingest uses embedded previews (D1); rawler full decode
                       becomes a backfill pass, not a blocker
                     • schema implements EVENTS.md (targets table, vectors
                       table direction, redaction) from day one
         ── parallel spike: llama.cpp/ASR process-manager prototype (E5),
            throwaway code, findings feed spec/RUNTIME.md before M2 design
            hardens.

Phase 2  M2a PENCIL  Grease pencil + journal reading panel. Pure events +
                     canvas; zero AI dependencies; stroke↔utterance linking
                     fields exist but link to nothing yet. Ships the full
                     "mark and it remembers" loop for dogfooding.

Phase 3  M2b VOICE   ASR streaming, binding rule from CAPTURE.md, audio
                     policy, transcript correction UI. Background summaries
                     begin here but as retrieval fuel only (E4).

Phase 4  M3 RETRIEVE Embedding passes (staged-ingest backfill), hybrid
                     search + query parse + quote-provenance results,
                     collections store.

Phase 5  M4 TIME     As scoped (trajectories, scrubbing) — gated on E4/
                     sentiment-quality evaluation from M3 dogfooding.

Phase 6  M5 PARTNER  As scoped.
```

Why this order: M2a before voice decouples the two untested input methods —
the pencil has zero ASR/model risk and is itself a hedge per the scope doc's
own open question #1, so it should not sit behind voice in the queue. The
runtime spike runs early because E5 is the highest-variance engineering in
the plan and currently has the least ink. Everything downstream of M3 is
already correctly sequenced.

## Holes deliberately left open (acknowledged, not blocking)

- Multi-machine sync as a *product feature* (C2 makes it cheap later; not v1).
- Editing-app awareness (already deferred in SCOPE.md).
- Stacking/pairing UX for RAW+JPEG (C1 records the stance).
- Wide-gamut color management (D2 records the limitation).
- Final name (placeholder discipline already tracked).
