# EVENTS.md — The Photoproof Event Model

**Status:** Normative. Foundation spec; SIDECARS, LIBRARY, CAPTURE, RETRIEVAL, RUNTIME, and UI are written against it.
**Closes gaps:** A1–A5, B3, C2, C5 (`docs/SPEC-GAPS.md`).
**Owns:** event identity; the `annotation_events` log + `event_targets` join; per-kind payload/fold/index/sidecar semantics; revision/retraction/redaction; the merge invariant; session *storage*; the event-store API.
**Does not own:** sidecar file format (SIDECARS), session lifecycle and capture binding (CAPTURE), embedding/chunking recipes and search (RETRIEVAL), image identity beyond the hash definition (LIBRARY), journal-panel ergonomics (UI).

---

## 1. Identity primitives

### 1.1 ContentHash

- BLAKE3-256 of the image file's bytes exactly as on disk — no normalization, no metadata stripping, no partial hashing.
- Wire/storage form: **64 lowercase hex characters**, in `event_targets.image_hash`, canonical event JSON, and sidecars. Uppercase/mixed input is rejected, not silently normalized (a silent normalizer hides corrupted producers).
- Images are known by hash, never by path. No path ever appears in an event row or in canonical event JSON.

### 1.2 EventId (ULID)

- 26 characters, Crockford base32, **uppercase**: 48-bit UTC milliseconds + 80-bit randomness.
- Generation is **monotonic within a process** (the `ulid` crate's monotonic mode): each issued id is strictly greater than the last, by incrementing the random component within a millisecond and clamping the timestamp component to `max(last_issued_ms, now_ms)` if the wall clock regresses.
- Ids may be **minted before the event row is inserted**. CAPTURE mints an utterance's id at VAD speech-onset; the row lands only at ASR finalization. Minting fixes both id and `ts` at onset, so ULID order reflects capture order even when finalization lags by seconds.
- Cross-machine ULID collisions are negligible; uniqueness is enforced by `PRIMARY KEY` and the merge rule (§8): same id ⇒ same event.

### 1.3 Clock discipline; log order vs timestamp order

- All timestamps everywhere are **UTC**, serialized as RFC 3339 with exactly millisecond precision and `Z`: `2026-06-09T14:23:05.123Z`. Never local time, never any other offset.
- `ts` = wall-clock UTC at id-mint time, i.e. at capture onset (VAD onset for voice, pen-down for strokes, submit for typed notes), not at row insertion.
- **Log order is ULID order.** Canonical display/replay order within a session is ascending event id. Ids are minted monotonically at capture onset, so ULID order *is* append order in the journal sense. Events are **never re-sorted by `ts`**: `ts` may be non-monotonic (clock regression, NTP step) while ids increase strictly. `ts` is testimony; `id` is order.
- The ms encoded inside a ULID may differ from `ts` (clamping). `ts` is authoritative for "when"; `id` for "in what order".
- If the wall clock regresses > 60 s within a process lifetime, log a warning; do nothing else (culls happen on planes).

---

## 2. The event record

### 2.1 Logical fields

| field          | type                 | presence                                           |
|----------------|----------------------|----------------------------------------------------|
| `id`           | EventId (ULID)       | always                                             |
| `v`            | integer              | always; event schema version, currently `1`        |
| `session_id`   | ULID                 | always                                             |
| `ts`           | RFC 3339 UTC ms      | always                                             |
| `source`       | enum (§2.2)          | always                                             |
| `kind`         | enum (§2.2)          | always                                             |
| `targets`      | array of ContentHash | always (may be empty); ordered                     |
| `text`         | UTF-8 string         | `remark`/`revision` only; removed by redaction     |
| `payload`      | JSON object          | kind-specific (§3); removed by redaction           |
| `target_event` | EventId              | `revision`/`retraction`/`redaction` only, required |
| `linked_event` | EventId              | `stroke` or voice `remark`, optional — the cross-modal link, carried by the **later-committed** event (§3.3, CAPTURE §9) |
| `redacted_by`  | EventId              | present iff content has been scrubbed              |

### 2.2 `source` × `kind` matrix

`source` = how the event entered. `kind` = what it means.
`source ∈ {voice, typed, pencil, system}`; `kind ∈ {remark, rating, stroke, revision, retraction, redaction}`.

| kind \ source | voice | typed | pencil | system | notes |
|---------------|:-----:|:-----:|:------:|:------:|-------|
| `remark`      |  ✓    |  ✓    |   —    |   —    | voice = ASR utterance; typed = quick note |
| `rating`      |  —    |  ✓    |   —    |   —    | keyboard rating; spoken ratings = recorded future feature, not v1 |
| `stroke`      |  —    |  —    |   ✓    |   —    | one pen-down→pen-up = one event |
| `revision`    |  —    |  ✓    |   —    |   —    | correction typed in the journal panel |
| `retraction`  |  ✓    |  —    |   —    |   ✓    | `system` = journal-panel retract button (M1); `voice` = "strike that" (M2b) |
| `redaction`   |  —    |  —    |   —    |   ✓    | always a deliberate journal-panel act |

`source=system` means "entered via an app affordance that is neither free text, voice, nor pencil" — the user clicked a button. It does **not** mean machine-generated content; no machine-generated content ever enters the journal (§3.7). Anything outside the matrix is invalid: `append()` rejects it, `merge()` quarantines it. The matrix is also a `CHECK` constraint (§5.2).

### 2.3 Targeting and effective targets

Targets live in `event_targets` (and the `targets` JSON array), never in the event row. Allowed counts:

| kind        | targets | meaning |
|-------------|---------|---------|
| `remark`    | 0..N    | 0 = session-level remark; N = one remark about N selected images |
| `rating`    | 1..N    | rate the selection; fold is still per-image |
| `stroke`    | exactly 1 | the viewed image |
| `revision`, `retraction`, `redaction` | 0 (stored) | applies wherever the target applies |

**Effective targets** (drives sidecar placement and dirty-tracking):

```
effective_targets(e):
    if e.kind in {remark, rating, stroke}: return targets(e)
    t = get(e.target_event)
    return effective_targets(t) if t exists else []      # dangling ⇒ []
```

A meta-event (revision/retraction/redaction) is mirrored into **every sidecar its target is mirrored into**, so a sidecar is self-sufficient: an event travels with all meta-events that alter it. An event with N effective targets appears in N sidecars; on rebuild, copies **dedupe by event id** (byte-identical canonical JSON; copies with the same id that differ structurally are corruption — keep the first, log an integrity warning, except a scrubbed copy always beats an unscrubbed one, §8).

Events with **zero** effective targets (session-level remarks, dangling meta-events) have no adjacent-sidecar home. The store exposes them grouped per session (§10); SIDECARS assigns them a home in the overflow store and export manifest. They are journal truth and MUST be included in export/rebuild.

`event_targets.position` records selection order at capture (0-based) and equals the `targets` array order. Preserved verbatim; no v1 semantics beyond stable round-tripping.

---

## 3. Event kinds

### 3.1 `remark`

One utterance or one typed note — the atom of the journal.

- **Fields:** `text` (required, non-empty after trim). For `source=voice`, `payload = {"conf_pm": int 0..1000, "dur_ms": int ≥ 0}` — per-segment ASR confidence (per-mille) and utterance duration (VAD onset → finalization); a voice remark MAY carry `linked_event` referencing an earlier stroke (the later-committed event carries the link, CAPTURE §9). `payload` omitted for typed.
- **Fold:** folded entry shows the **effective text** — the latest live revision in the chain, else the remark's own text (§6.1). Hidden entirely if retracted; redaction stub if scrubbed.
- **Indexing:** FTS5 over effective text, one row per remark keyed by chain-root id (§5.4). Embeddings: `vec_kind='annotation_chunk'` vectors reference the root id, embed effective text, chunked ~512 tokens (RETRIEVAL owns the chunker). `dur_ms` feeds stroke↔utterance overlap linking (CAPTURE owns the algorithm).
- **Sidecar:** canonical JSON, verbatim. **UI default:** visible everywhere journal context appears.

### 3.2 `rating`

- **Fields:** `payload = {"value": int 0..5}` — photographer-convention stars. `value: 0` is an **explicit zero** ("considered, no stars"), distinct from never-rated (no live rating event).
- **Fold:** **current rating per image = `value` of the greatest-id (ULID order) non-retracted rating event targeting that image.** Retracting the latest resurfaces the previous; retracting all returns to never-rated. Re-rating just appends — history is the point.
- **Indexing:** no FTS, no embeddings; folded into derived `image_ratings` (§5.3) for structured filters.
- **Sidecar:** verbatim. **UI default:** current stars on the image; each rating event a journal-panel timeline row.
- **Redaction:** not permitted (no scrubable content); use retraction.

### 3.3 `stroke`

One grease-pencil gesture, pen-down to pen-up.

- **Fields:** `payload`:

  ```
  {
    "base_w": int,                  // stroke base width, ten-thousandths of the
                                    // display-oriented long edge; default 40 (= 0.4%)
    "orientation": int 1..8,        // EXIF orientation value applied to produce the
                                    // display-oriented frame the coords refer to
    "points": [[x, y, p, t], ...],  // ≥ 1 point, ≤ 8192 (capture downsamples)
    "tool": "pencil"                // only v1 value; future tools extend this enum
  }
  ```

  `x`,`y`: int, ten-thousandths of the **display-oriented** image width/height (left→right, top→bottom): `round(u * 10000)` where `u` is the normalized position. Range **−2500..12500** — strokes may overshoot the frame while circling an edge subject (CAPTURE §8.1 clamps to [−0.25, 1.25]); renderers clip to the visible overlay. `p`: pressure per-mille, int 0..1000; devices that report no pressure record **1000** (renders at nominal width under CAPTURE's width model `w = base_w × (0.4 + 0.6·p/1000)`). `t`: int ms since pen-down, first point 0, non-decreasing; the event's `ts` is pen-up (commit), so pen-down = `ts − t_last`. Integer quantization is deliberate: canonical JSON has no floats (§4.1); 1/10000 of image extent is sub-pixel at any preview size.
- `linked_event` (top-level, optional): the cross-modal stroke↔utterance link. **Carried by the later-committed event only**, pointing backward — a stroke linking to an earlier utterance, or a voice remark linking to an earlier stroke (CAPTURE §9 owns resolution: VAD-span overlap, else nearest within ±10 s, else absent). Folds and journal views MUST traverse `linked_event` in both directions; "linked" is symmetric even though storage is one-directional. Dangling links are inert until the partner arrives (§8).
- **Fold:** visible on the overlay unless retracted (erase = retraction of the stroke event) or scrubbed. No partial-stroke erase in v1.
- **Indexing:** no FTS/embeddings of its own; searchable *through* the linked utterance ("the one where I circled the hand" hits the utterance text; UI highlights the linked stroke).
- **Sidecar:** verbatim. **UI default:** rendered on the toggleable overlay; inline at its timestamp in the journal panel.

### 3.4 `revision`

Correction of the photographer's recorded words; ASR will mishear, and the journal must be correctable without editing history.

- **Fields:** `text` (corrected full text, required); `target_event` (required) — a `remark` or another `revision` (chains). Other target kinds rejected at append.
- **Chain resolution:** `root(e)` follows `target_event` hops until a non-revision event. All revisions resolving to root R form R's chain. **Effective text of R** = `text` of the **greatest-id** live (non-retracted, non-scrubbed) revision in the chain, else R's own text. Revision-of-revision needs no special case: both resolve to the same root, latest id wins. A revision whose chain has a missing hop is **inert** — not displayed, not indexed — and activates automatically when the missing ancestor arrives via merge.
- **Fold:** never a standalone entry; replaces the displayed/indexed text of its root. Retracting a revision removes that correction (previous live revision or the original resurfaces). Retracting the *root* hides the whole entry, revisions included.
- **Indexing:** on append, the root's FTS row is updated to the new effective text and the root's `annotation_chunk` vectors are invalidated for re-embedding. The original text stays in the log and sidecars — it is simply not indexed.
- **Sidecar:** verbatim, mirrored to the root's sidecars (§2.3) — sidecars carry original *and* correction.
- **UI default:** journal panel shows effective text with a "corrected" affordance that expands the chain; not a separate timeline entry.

### 3.5 `retraction`

The tombstone: "strike that." Hidden, preserved.

- **Fields:** `target_event` (required); no `text`, no `payload`. Valid targets: `remark`, `rating`, `stroke`, `revision`.
- **Fold:** the target folds out — hidden from the journal UI by default, removed from FTS, vectors deleted, excluded from context assembly and rating folds. Event row, target row, and both sidecar entries fully preserved.
- **Retraction of a retraction: not supported in v1.** `append()` rejects it; to bring something back, **re-state it** (a new remark/rating/stroke). Rationale: un-retract doubles the fold-rule surface for a case re-stating covers, and "I said it again" is truer journal semantics than "I un-struck it." Duplicate retractions of one target (merge can produce them) are valid and idempotent. Retracting a redaction is also invalid.
- **Indexing:** removes (or recomputes, for revision targets) the target's FTS row/vectors; never indexed itself.
- **Sidecar:** verbatim, mirrored to the target's sidecars. **UI default:** retracted entries hidden (a "show retracted" toggle is the UI spec's call; the fold exposes the flag).

### 3.6 `redaction`

**The one sanctioned violation of append-only.** Content is physically destroyed; the *fact* of the event survives. Mechanics in §7.

- **Fields:** `target_event` (required); no `text`/`payload`. Valid targets: `remark`, `revision`, `stroke`. Invalid: `rating` (nothing to scrub — retract instead), `retraction`, `redaction`. There is no redaction of a redaction: a redaction event holds nothing but the target id, and that id must survive — it *is* the registry entry that makes redaction supremacy work (§7–8).
- **Chain closure:** redacting any member of a revision chain scrubs the **entire chain** (root + all revisions) — corrected copies of sensitive content are still sensitive. `redact()` emits one redaction event per chain member in one transaction. Redacting a stroke scrubs just that stroke; a linked utterance is separate content, redacted separately (UI may offer both in one gesture; the store sees two `redact()` calls).
- **Fold:** scrubbed target appears as an inert stub (§7); never indexed.
- **Sidecar:** the redaction event itself is mirrored verbatim to the target's sidecars; the target appears only in scrubbed form (§4.3 D).
- **UI default:** redaction events are not timeline entries; the scrubbed target shows as a "[redacted]" stub (Open Question Q2).

### 3.7 No machine-bookkeeping kinds — and why

There are **no** ingest/relink/model-pass event kinds. The journal records human judgment, nothing else. Machine bookkeeping (file discovered, hashed, relinked, preview cached, embedding pass v3 done) lives in LIBRARY-owned, rebuildable tables (`images`, `paths`, `ingest_passes`, `volumes`) and is **never mirrored to sidecars**. Defense:

1. **Sidecars are the user's archive.** A relink marker would embed machine state (and paths!) into a file that must stay meaningful for twenty years.
2. **Merge purity.** Journal merge is set-union of immutable human facts (§8). Bookkeeping is neither immutable nor worth unioning — two machines legitimately disagree about paths.
3. **Volume.** A 50k-image ingest would drown the journal in system rows.

Consequently `source=system` appears only on `retraction`/`redaction` (button-mediated user acts), and the journal has exactly six kinds.

### 3.8 Summary

| kind       | text | payload                   | target_event | linked_event | targets | FTS              | vectors           | sidecar            | UI default           |
|------------|------|---------------------------|--------------|--------------|---------|------------------|-------------------|--------------------|----------------------|
| remark     | ✓    | voice: conf_pm, dur_ms    | —            | voice: opt.  | 0..N    | effective text   | annotation_chunk (root) | verbatim           | visible              |
| rating     | —    | value 0..5                | —            | —            | 1..N    | —                | —                 | verbatim           | stars + timeline row |
| stroke     | —    | base_w, orientation, points, tool | —    | optional     | 1       | — (via link)     | — (via link)      | verbatim           | overlay + timeline   |
| revision   | ✓    | —                         | required     | —            | 0       | updates root row | invalidates root  | verbatim, w/root   | folded into root     |
| retraction | —    | —                         | required     | —            | 0       | removes target   | removes target    | verbatim, w/target | hides target         |
| redaction  | —    | —                         | required     | —            | 0       | purges target    | purges target     | verbatim, w/target | target shows stub    |

---

## 4. Canonical JSON serialization

This is **the** serialization: sidecars embed events in exactly this form, and the round-trip invariant (I5) demands byte-identical re-serialization. The SIDECARS spec depends on these bytes.

### 4.1 Rules

1. UTF-8, no BOM. One event = one JSON object, no trailing newline (containers add their own separators).
2. Compact: no whitespace outside string values.
3. Object members sorted by key, ascending byte order (keys are ASCII). Applies recursively (`payload` too).
4. Strings: escape only `"` → `\"`, `\` → `\\`, and control chars U+0000–U+001F (`\b \f \n \r \t` where applicable, else `\u00xx`, lowercase hex). Everything else, including non-ASCII, is raw UTF-8 — never `\uXXXX`-escaped.
5. **Numbers are integers only.** Base 10, no leading zeros, no `-0`, no exponent/fraction. **No floats anywhere** — coordinates, pressure, and confidence are quantized integers precisely because float formatting is the classic cross-language canonicalization trap.
6. `null` never appears; absent optional fields are **omitted entirely**.
7. Arrays preserve order (`targets` = position order; `points` = capture order).
8. The field set is closed per `v`. Parsers MUST reject unknown top-level or payload keys for `v:1`; forward compatibility = bump `v`, never tolerate extras (extras break byte-identical round-trips).

### 4.2 Top-level fields in canonical (sorted) order

`id`, `kind`, `linked_event`?, `payload`?, `redacted_by`?, `session_id`, `source`, `target_event`?, `targets`, `text`?, `ts`, `v` — `targets` always present, possibly `[]`.

### 4.3 Literal examples

Image A = `b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5`, image B = `4d1f8a2b9c0e3657d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6`.

**A. Typed remark targeting two images:**

```json
{"id":"01JZ5C4R2GW8Q1T9M3N7P5XKDA","kind":"remark","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"typed","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5","4d1f8a2b9c0e3657d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6"],"text":"these two are the spine of the quiet sequence","ts":"2026-06-09T14:23:05.123Z","v":1}
```

**B. Voice remark, then a revision correcting "muddy" → "moody":**

```json
{"id":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","kind":"remark","payload":{"conf_pm":912,"dur_ms":4210},"session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"voice","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"text":"keep this one it has that muddy light I keep chasing","ts":"2026-06-09T14:24:11.480Z","v":1}
```

```json
{"id":"01JZ5C7T8HV2X4B6D8F0G2J4KC","kind":"revision","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"typed","target_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","targets":[],"text":"keep this one it has that moody light I keep chasing","ts":"2026-06-09T14:26:02.007Z","v":1}
```

Fold: effective text of `…C2D` is the revision's text; its FTS row now says "moody"; "muddy" remains in the log and in image A's sidecar. A later revision `01JZ5D02M9N3P5Q7R9S1T3V5WB` targeting either `…C2D` *or* `…J4KC` resolves to the same root and, having the greater id, wins the fold.

**C. Stroke linked to that utterance** (circle around a hand; 4 points shown — real strokes carry tens to hundreds):

```json
{"id":"01JZ5C5SXK3M7P9Q1R3S5T7V9W","kind":"stroke","linked_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","payload":{"base_w":40,"orientation":1,"points":[[3326,4593,1000,0],[3654,4337,1000,16],[3983,4591,1000,33],[3328,4601,1000,610]],"tool":"pencil"},"session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"pencil","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"ts":"2026-06-09T14:24:12.950Z","v":1}
```

**D. Redaction pair** — the redaction event, and the scrubbed form of B's voice remark as it appears in the DB and every sidecar afterwards:

```json
{"id":"01JZ6A0QZ3W5X7Y9Z1B3C5D7E9","kind":"redaction","session_id":"01JZ6A0PR4S6T8V0W2X4Y6Z8AB","source":"system","target_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","targets":[],"ts":"2026-06-14T09:02:44.310Z","v":1}
```

```json
{"id":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","kind":"remark","redacted_by":"01JZ6A0QZ3W5X7Y9Z1B3C5D7E9","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"voice","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"ts":"2026-06-09T14:24:11.480Z","v":1}
```

What survives scrubbing: `id`, `kind`, `session_id`, `source`, `targets`, `ts`, `v`, and structural refs (`target_event`/`linked_event`). `text` and `payload` are not blanked but **absent**; `redacted_by` marks the act. (B's revision is scrubbed in the same transaction — chain closure, §3.6 — with its own redaction event.)

---

## 5. SQLite schema

### 5.1 Connection pragmas

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA secure_delete = ON;     -- required: redaction must overwrite freed bytes (§7)
PRAGMA foreign_keys = OFF;     -- deliberate, see below
PRAGMA cache_size = -65536;    -- 64 MiB page cache
PRAGMA mmap_size = 268435456;  -- 256 MiB memory-mapped I/O
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;    -- ms; writer + read pool share one file
```

`page_size` stays at the 4096 default — no measured win justifies a migration. Foreign keys stay OFF because merge legitimately inserts events whose `target_event`/`linked_event`/`session_id` referent has not arrived yet (a sidecar found before its sibling; a stroke referencing a minted utterance id). Dangling references are **inert, never invalid** (§6.1, §8); referential integrity is a fold-level concept here, not a storage constraint.

**Operational rules (normative):**

- `PRAGMA optimize` on every connection close, and periodically on long-lived connections.
- Full `ANALYZE` after rebuild-from-sidecars or any large merge — the FTS5 join planner depends on fresh stats (RETRIEVAL §4).
- `PRAGMA wal_checkpoint(TRUNCATE)` at app idle and at shutdown.
- Long-lived UI statements MUST NOT hold read transactions open: an open reader blocks checkpointing and the WAL grows without bound, slowing every read ([sqlite.org/wal.html](https://sqlite.org/wal.html)). Step or reset statements promptly; never keep a cursor alive across UI idle time.
- `EventStore` opens **one dedicated writer connection plus a small read pool** (the [tokio-rusqlite](https://docs.rs/tokio-rusqlite/latest/tokio_rusqlite/)/async-sqlite pattern); all writes serialize through the writer.

### 5.2 Truth tables (the journal — rebuildable only from sidecars)

```sql
CREATE TABLE annotation_events (
  id            TEXT PRIMARY KEY CHECK (length(id) = 26),
  v             INTEGER NOT NULL DEFAULT 1,
  session_id    TEXT NOT NULL CHECK (length(session_id) = 26),
  ts            TEXT NOT NULL
                  CHECK (ts GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T' ||
                               '[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
  source        TEXT NOT NULL CHECK (source IN ('voice','typed','pencil','system')),
  kind          TEXT NOT NULL CHECK (kind IN
                  ('remark','rating','stroke','revision','retraction','redaction')),
  text          TEXT,
  payload       TEXT,               -- canonical JSON object (§4), kind-specific
  target_event  TEXT CHECK (target_event IS NULL OR length(target_event) = 26),
  linked_event  TEXT CHECK (linked_event IS NULL OR length(linked_event) = 26),
  redacted_by   TEXT CHECK (redacted_by IS NULL OR length(redacted_by) = 26),

  -- source × kind matrix (§2.2)
  CHECK ( (kind = 'remark'     AND source IN ('voice','typed'))
       OR (kind = 'rating'     AND source = 'typed')
       OR (kind = 'stroke'     AND source = 'pencil')
       OR (kind = 'revision'   AND source = 'typed')
       OR (kind = 'retraction' AND source IN ('voice','system'))
       OR (kind = 'redaction'  AND source = 'system') ),

  -- structural shape per kind
  CHECK ( (target_event IS NOT NULL) = (kind IN ('revision','retraction','redaction')) ),
  CHECK ( linked_event IS NULL OR kind IN ('stroke','remark') ),
  CHECK ( kind IN ('remark','revision') OR text IS NULL ),
  CHECK ( kind NOT IN ('retraction','redaction')
          OR (text IS NULL AND payload IS NULL AND redacted_by IS NULL) ),
  -- content present unless scrubbed
  CHECK ( kind NOT IN ('remark','revision') OR text IS NOT NULL OR redacted_by IS NOT NULL ),
  CHECK ( kind NOT IN ('rating','stroke') OR payload IS NOT NULL OR redacted_by IS NOT NULL ),
  CHECK ( kind <> 'rating' OR redacted_by IS NOT NULL
          OR CAST(json_extract(payload,'$.value') AS INTEGER) BETWEEN 0 AND 5 )
) STRICT;

CREATE INDEX idx_events_session ON annotation_events(session_id, id);
-- (target_event, kind): retracted(id) checks and the meta-closure's kind dispatch
-- (§6.1, §10.1) are answered from the index alone, no row fetch.
CREATE INDEX idx_events_target  ON annotation_events(target_event, kind) WHERE target_event IS NOT NULL;
-- No index on kind alone: six values, too low-selectivity to earn one. The redaction
-- registry rebuild gets a partial index instead:
CREATE INDEX idx_events_redactions ON annotation_events(id) WHERE kind = 'redaction';

CREATE TABLE event_targets (
  event_id    TEXT    NOT NULL CHECK (length(event_id) = 26),
  image_hash  TEXT    NOT NULL
                CHECK (length(image_hash) = 64 AND image_hash NOT GLOB '*[^0-9a-f]*'),
  position    INTEGER NOT NULL CHECK (position >= 0),
  PRIMARY KEY (event_id, image_hash),
  UNIQUE (event_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_targets_image ON event_targets(image_hash, event_id);

CREATE TABLE sessions (             -- storage here; lifecycle in CAPTURE (§9)
  id            TEXT PRIMARY KEY CHECK (length(id) = 26),
  started_ts    TEXT NOT NULL,
  ended_ts      TEXT,               -- NULL while open
  app_version   TEXT NOT NULL,      -- semver of the writing build
  device_id     TEXT NOT NULL,      -- 32 lowercase hex; random per install
  root_context  TEXT                -- JSON written by CAPTURE; see §9
) STRICT;
```

**Payload-filtering rule (normative):** any future filtering on `payload` fields is done through a **virtual generated column plus an index on it** — never `json_extract()` in a hot WHERE clause, which forces a full-table JSON parse per query ([JSON virtual columns](https://antonz.org/json-virtual-columns/)). The §5.2 CHECK constraints may use `json_extract` (they run once, at insert); query paths may not.

### 5.3 Derived tables (rebuildable from the truth tables; never in sidecars)

```sql
-- Redaction registry: O(1) supremacy lookup. Pure index over kind='redaction' rows.
CREATE TABLE redactions (
  event_id      TEXT PRIMARY KEY,   -- the scrubbed/condemned event
  redaction_id  TEXT NOT NULL,      -- the redaction event recording the act
  ts            TEXT NOT NULL       -- redaction event's ts
) STRICT, WITHOUT ROWID;

-- Rating fold, for structured filters.
CREATE TABLE image_ratings (
  image_hash  TEXT PRIMARY KEY,
  rating      INTEGER NOT NULL CHECK (rating BETWEEN 0 AND 5),
  event_id    TEXT NOT NULL,        -- the winning rating event
  ts          TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Journal-stats fold: one row per image with any live journal presence.
-- Consumers: grid has-journal badges, the HasStrokes filter chip, and RRF
-- recency tie-breaking (RETRIEVAL §4, §5.3) — each an indexed lookup instead
-- of a per-image fold. Maintained in the same append/redact/merge transactions
-- as the other derived tables (§6.3), rebuilt by rebuild_derived(), covered by
-- the derived=fold invariant (I7).
CREATE TABLE image_journal_stats (
  image_hash   TEXT PRIMARY KEY,
  event_count  INTEGER NOT NULL,    -- live (non-retracted) events targeting the image
  has_strokes  INTEGER NOT NULL,    -- 0/1: any live stroke
  last_ts      TEXT NOT NULL        -- ts of the most recent live event
) STRICT, WITHOUT ROWID;

-- Durable dirty set consumed by the sidecar writer (SIDECARS owns consumption).
-- Survives restart; rows removed only after a successful sidecar write. 'redaction'
-- rows persisting while a volume is offline ARE the queued propagation of §7.
-- 'redaction' outranks and overwrites other reasons for a hash, never vice versa.
CREATE TABLE sidecar_dirty (
  image_hash  TEXT PRIMARY KEY,
  reason      TEXT NOT NULL CHECK (reason IN ('append','fold','redaction')),
  since_ts    TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Vectors: REFERENCE DESIGN ONLY — RETRIEVAL owns details. Normative: vectors
-- reference events/images (never the reverse), carry model_id + dims, and are NEVER
-- serialized into event rows or sidecars.
CREATE TABLE vectors (
  vec_id      INTEGER PRIMARY KEY,
  vec_kind    TEXT NOT NULL CHECK (vec_kind IN ('annotation_chunk','image_summary','image_clip')),
  event_id    TEXT,                 -- chain-root event id, for annotation_chunk
  image_hash  TEXT,                 -- for image_summary / image
  chunk_idx   INTEGER NOT NULL DEFAULT 0,
  model_id    TEXT NOT NULL,
  dims        INTEGER NOT NULL,
  vec         BLOB NOT NULL,        -- f32 little-endian, dims entries
  created_ts  TEXT NOT NULL,
  CHECK ((event_id IS NULL) <> (image_hash IS NULL))
) STRICT;
CREATE INDEX idx_vectors_event ON vectors(event_id)   WHERE event_id IS NOT NULL;
CREATE INDEX idx_vectors_image ON vectors(image_hash) WHERE image_hash IS NOT NULL;
```

### 5.4 FTS5

```sql
-- rowid mapping: one FTS row per chain ROOT (remark), stable across revisions.
CREATE TABLE fts_map (
  fts_rowid      INTEGER PRIMARY KEY AUTOINCREMENT,
  root_event_id  TEXT NOT NULL UNIQUE
) STRICT;

CREATE VIRTUAL TABLE event_fts USING fts5(
  body,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'                    -- search-as-you-type
);
```

`event_fts` rows are keyed by `fts_map.fts_rowid` and contain the **effective text** of one live remark root. **This plain content-ful construction — stored text, no external-content option, no UNINDEXED columns, id mapping in `fts_map` — is THE single normative FTS design; RETRIEVAL defers to it.** Maintenance is **not** done with SQL triggers: fold rules (chain resolution, retraction fallback) are application logic triggers cannot express. Instead `photoproof-core` performs FTS maintenance **in the same SQLite transaction as the event insert** (§6.3). A hit's `root_event_id` joins through `event_targets` to images — provenance ("the matching quote, dated") is free.

After rebuild-from-sidecars and after any large merge, run `INSERT INTO event_fts(event_fts) VALUES('optimize')`: each transaction writes its own small index b-trees, so the FTS index fragments and query cost grows until the segments are merged.

### 5.5 Append-only enforcement (defense in depth)

The Rust API exposes no update/delete path; the DB enforces it anyway:

```sql
CREATE TRIGGER trg_events_no_delete BEFORE DELETE ON annotation_events
BEGIN SELECT RAISE(ABORT, 'annotation_events is append-only'); END;

-- The sole permitted mutation: the redaction scrub (§7).
CREATE TRIGGER trg_events_scrub_only BEFORE UPDATE ON annotation_events
WHEN NOT (  OLD.redacted_by IS NULL AND NEW.redacted_by IS NOT NULL
        AND NEW.text IS NULL AND NEW.payload IS NULL
        AND NEW.id = OLD.id AND NEW.v = OLD.v
        AND NEW.session_id = OLD.session_id AND NEW.ts = OLD.ts
        AND NEW.source = OLD.source AND NEW.kind = OLD.kind
        AND NEW.target_event IS OLD.target_event
        AND NEW.linked_event IS OLD.linked_event )
BEGIN SELECT RAISE(ABORT, 'only the redaction scrub may update annotation_events'); END;

CREATE TRIGGER trg_targets_no_update BEFORE UPDATE ON event_targets
BEGIN SELECT RAISE(ABORT, 'event_targets is append-only'); END;
CREATE TRIGGER trg_targets_no_delete BEFORE DELETE ON event_targets
BEGIN SELECT RAISE(ABORT, 'event_targets is append-only'); END;
```

(`rebuild-from-sidecars` recreates the database file from scratch; it never deletes rows.)

---

## 6. Fold rules, precisely

### 6.1 Definitions

```
get(id)            -> the event row, or None
scrubbed(e)        := e.redacted_by != NULL  (equivalently e.id ∈ redactions)
retracted(id)      := ∃ retraction r: r.target_event == id          # never scrubbed (§3.6)
root(e):
    seen = {}
    while e.kind == 'revision':
        if e.id in seen: return None            # cycle: corrupt data, chain inert (I10)
        seen += e.id
        t = get(e.target_event)
        if t is None: return None               # dangling: inert until merge heals it
        e = t
    return e

chain(R)           := {R} ∪ { v : v.kind=='revision' and root(v) == R }
live_revisions(R)  := { v ∈ chain(R) : v.kind=='revision',
                        not retracted(v.id), not scrubbed(v) }
effective_text(R)  := text of max-id member of live_revisions(R),
                      else R.text   (undefined if scrubbed(R) — stub instead)
```

### 6.2 The folded journal of image H

Input: all events `e` with `H ∈ effective_targets(e)`, ascending id. Output:

- `remark` R: skip if `retracted(R.id)`. If `scrubbed(R)` → emit a **redacted stub** (id, ts, kind, source; no content). Else emit `effective_text(R)` plus a `corrected: bool` flag.
- `rating`: skip if retracted; emit (value, ts). `current_rating(H)` = value of the max-id live rating targeting H, else none.
- `stroke` S: skip if retracted; scrubbed → stub (journal panel only; nothing on the overlay). Else emit path payload + resolved `linked_event`.
- `revision`/`retraction`/`redaction`: never standalone entries; they act on their targets as above.

The **indexable set** (FTS + `annotation_chunk` embeddings) = effective texts of live, unscrubbed remark roots. Retracted and scrubbed content are excluded from FTS, vectors, and context assembly identically; they differ only in what remains in the log (everything vs. structure-only).

### 6.3 FTS/derived maintenance (same transaction as the insert)

| operation | derived effect |
|---|---|
| append remark (targets T) | create `fts_map` row; `INSERT INTO event_fts(rowid, body)`; mark T dirty(`append`) |
| append rating | recompute `image_ratings` for its targets; dirty(`append`) |
| append stroke | mark its target dirty(`append`) |
| append revision (root R) | `UPDATE event_fts SET body = effective_text(R) WHERE rowid = map(R)`; delete `vectors` for R (re-embed queued); mark `effective_targets` dirty(`fold`) |
| append retraction of remark R | `DELETE FROM event_fts WHERE rowid = map(R)`; delete vectors for R; dirty(`fold`) |
| append retraction of revision in chain R | recompute: root still live → UPDATE fts body, else no-op; invalidate vectors; dirty(`fold`) |
| append retraction of rating / stroke | recompute `image_ratings` / nothing; dirty(`fold`) |
| redact (§7) | DELETE fts row for chain root; delete vectors; dirty(`redaction`) |
| merge (§8) | batch inserts first, then recompute derived state for affected roots/images |

Every operation above additionally upserts `image_journal_stats` (§5.3) for the affected images — `event_count`/`has_strokes`/`last_ts` recomputed from the live fold — in the same transaction, exactly as `image_ratings` is.

`fts_map` rows are never deleted (stable rowids); only `event_fts` rows come and go. All derived tables are disposable: `rebuild_derived()` recomputes them from `annotation_events` + `event_targets` alone and MUST equal incremental maintenance (I7).

---

## 7. Redaction mechanics

An accidental capture (a phone call, a third party in the room) is a *when*, not an *if*. Redaction destroys content while preserving the shape of history. `redact(target_id)` — all steps in **one SQLite transaction** (events, registry, FTS, vectors, dirty queue share the database, so atomicity is total):

1. **Validate.** Target exists locally, kind ∈ {remark, revision, stroke}, not already scrubbed.
2. **Chain closure.** `C = chain(root(target))` for remark/revision; `C = {target}` for a stroke. Already-scrubbed members skipped (idempotence).
3. **Record the act.** For each `c ∈ C` ascending: mint redaction event `R_c` (`kind='redaction'`, `source='system'`, `target_event=c.id`, `targets=[]`, fresh id/ts, current session) and insert it. The acts are history and travel in sidecars.
4. **Registry.** `INSERT OR IGNORE INTO redactions(event_id, redaction_id, ts)` per pair.
5. **Scrub in place.** Per `c`: `UPDATE annotation_events SET text = NULL, payload = NULL, redacted_by = R_c.id WHERE id = c.id` — the only UPDATE the §5.5 trigger admits. Columns overwritten: exactly `text`, `payload` (→ NULL) and `redacted_by` (→ the marker). Everything else — id, v, session_id, ts, source, kind, target_event, linked_event, target rows — survives, so log continuity, ordering, and stroke↔utterance structure stay intact.
6. **Purge indexes.** `DELETE FROM event_fts WHERE rowid = map(root)`; `DELETE FROM vectors WHERE event_id IN C`. (`image_ratings` never involved — ratings can't be redacted.)
7. **Queue propagation.** Upsert `sidecar_dirty(hash, 'redaction', now)` for every hash in `effective_targets(root)`. The sidecar writer (SIDECARS) rewrites each sidecar with scrubbed forms + redaction events. For images on **offline volumes** the row persists until the volume mounts — the durable dirty table *is* the propagation queue; `reason='redaction'` rows are first-priority and must never be coalesced away by later writes.
8. **Physical hygiene.** `secure_delete=ON` (§5.1) zeroes freed page bytes; after commit run `PRAGMA wal_checkpoint(TRUNCATE)` so scrubbed plaintext doesn't linger in the WAL.
9. **Audio:** none retained (kernel: discarded at segment finalization). If a retention setting ever ships, redaction MUST also delete retained audio for the scrubbed utterances — recorded here so the future feature inherits the obligation.

**Redaction supremacy** (the registry's purpose): no merge, rebuild, or import path may write `text`/`payload` for an event id present in `redactions`; every insert path checks the registry first (§8). A stale sidecar restored from a 2024 backup can re-introduce the event *row* — scrubbed — but never the content. The registry itself is derived: it is exactly the fold of `kind='redaction'` events, which live in the log and in sidecars, so supremacy survives `rebuild-from-sidecars` too.

---

## 8. Merge — the one synchronization primitive

Rebuild-from-sidecars, backup restore, second machines, re-found sidecars, overflow import: all the same operation.

**Invariant: journal merge is set-union by event id, order-independent, with redaction supremacy.** Post-merge database state is a pure function of the union of event sets — commutative, associative, idempotent. Two copies of a journal merge by union; nothing else is ever needed. No clock comparison, no conflict resolution.

```
merge(incoming: [Event]) -> MergeReport            # one transaction
  # 0. Validate each incoming event: canonical-parse + matrix/shape rules (§2–3).
  #    Invalid events are quarantined (listed in the report), never silently dropped,
  #    never inserted.

  # 1. Redactions first — supremacy in force before content lands.
  for r in incoming where r.kind == 'redaction':
      if get(r.id) is None: insert r
      INSERT OR IGNORE INTO redactions(r.target_event, r.id, r.ts)

  # 2. Scrub local victims of newly learned redactions.
  for r in those redactions:
      v = get(r.target_event)
      if v exists and not scrubbed(v): scrub v in place (steps 5–8 of §7)

  # 3. Union the rest (ascending id; order is cosmetic — the result is order-free).
  for e in incoming where e.kind != 'redaction':
      if e.id ∈ redactions: e = scrubbed_form(e)        # supremacy on arrival
      match get(e.id):
        None        -> insert e + its event_targets rows
        Some(local) ->
            if scrubbed(local) and not scrubbed(e): keep local       # supremacy
            elif scrubbed(e) and not scrubbed(local): scrub local    # learn it
            else: skip (set semantics); if structural fields differ:
                  log integrity warning, local wins — same id MUST mean same
                  event, so a mismatch is corruption upstream

  # 4. Recompute derived state for every affected chain root and image: event_fts
  #    bodies, image_ratings, vector invalidation, sidecar_dirty marks.
  #    Recomputation (not incremental patching) is what makes the result independent
  #    of arrival order — an old revision arriving after a newer one cannot regress
  #    the FTS body, because the fold recomputes from the full set.

  # 5. Sessions: union by id. Same id on both sides: immutable fields (started_ts,
  #    app_version, device_id, root_context) keep local on mismatch + warn;
  #    ended_ts := max(non-NULL), NULL only if both NULL.
```

Dangling references after a merge (a revision whose ancestor's sidecar sits on an unplugged drive) are inert per §6.1 and activate automatically in the merge that supplies the missing event — no repair pass. Duplicate retractions and duplicate redaction events union harmlessly.

`MergeReport`: `{ inserted, duplicates, scrubbed_on_arrival, newly_scrubbed, quarantined: Vec<(offset, reason)>, integrity_warnings: Vec<EventId> }`.

**Batching discipline for rebuild and large merges (normative):** sort the incoming union **ascending by event id before insert** — monotonic ULIDs then append at the B-tree's right edge instead of rebalancing it; commit in transactions of **~10k events**; populate `event_fts` and the derived tables in a **single pass after the union completes** (step 4 above), never interleaved per-event; finish with `ANALYZE`, `INSERT INTO event_fts(event_fts) VALUES('optimize')`, and `PRAGMA wal_checkpoint(TRUNCATE)` (§5.1, §5.4). The result is identical either way (I6); the discipline is purely about not turning a 50k-sidecar rebuild into a B-tree-rebalancing exercise.

---

## 9. Sessions (storage)

A session = contiguous app use; a new session begins after 30 minutes idle. CAPTURE owns detection, idle rules, and mic toggles (which occur *within* sessions, never creating them). EVENTS owns the row:

- `id`: ULID minted at session start; every event carries it.
- `started_ts`/`ended_ts`: RFC 3339 UTC ms. `ended_ts` NULL while open, set once on close — the single permitted UPDATE on `sessions` (bookkeeping, not journal truth; the store API exposes no other mutation). A session left open by a crash is closed on next launch with `ended_ts :=` ts of its last event, else `started_ts`.
- `app_version`: semver of the writing build (forensics for format regressions).
- `device_id`: 32 lowercase hex, random, minted once per install, kept in app config. Distinguishes machines in a merged multi-device journal.
- `root_context`: opaque JSON written by CAPTURE describing where the user was working; reference shape `{"roots":["<library root id>", …],"focus_path":"2026/iceland"}`. LIBRARY owns root identity. May be NULL. The only path-like data anywhere near the journal, and it is session bookkeeping, not image truth.

Sessions referenced by exported events are included in the export manifest (SIDECARS owns placement). Merge rule: §8 step 5.

---

## 10. Event-store API surface (photoproof-core)

Signatures are the contract; no update/delete API exists.

```rust
pub struct EventStore { /* one dedicated writer connection + small read pool (§5.1) */ }

/// Id + timestamp fixed at capture onset; insertion may happen later (§1.2).
pub struct Minted { pub id: EventId, pub ts: UtcMillis }

pub enum EventDraft {
    Remark     { source: RemarkSource,          // Voice { conf_pm: u16, dur_ms: u32,
                                                //         linked_event: Option<EventId> } | Typed
                 text: String,
                 targets: Vec<ContentHash> },   // empty = session-level
    Rating     { value: u8,                     // 0..=5
                 targets: Vec<ContentHash> },   // non-empty
    Stroke     { payload: StrokePayload,        // base_w, orientation, points, tool (§3.3)
                 target: ContentHash,
                 linked_event: Option<EventId> },   // backward link to an earlier utterance
    Revision   { target: EventId, text: String },
    Retraction { target: EventId, source: RetractionSource },  // Voice | System
    // No Redaction draft: redaction is not an append; it goes through redact().
}

impl EventStore {
    /// Monotonic ULID + UTC ts, for pre-allocation at capture onset.
    pub fn mint(&self) -> Minted;

    /// Validates (matrix, target counts, target-kind rules), inserts the event and its
    /// event_targets rows, performs fold maintenance (§6.3) — one transaction.
    pub fn append(&self, session: &SessionId, draft: EventDraft,
                  minted: Option<Minted>) -> Result<Event, AppendError>;

    /// Folded views (§6.2): what the UI and context assembly consume.
    pub fn folded_journal(&self, image: &ContentHash) -> Result<Vec<JournalEntry>>;
    pub fn folded_session(&self, session: &SessionId) -> Result<Vec<JournalEntry>>;
    pub fn current_rating(&self, image: &ContentHash) -> Result<Option<u8>>;

    /// Raw reads: verbatim history, retracted included, scrubbed in scrubbed form.
    pub fn raw_event(&self, id: &EventId) -> Result<Option<Event>>;
    /// Effective-target closure for one image, ascending id — the sidecar feed.
    pub fn events_for_image(&self, image: &ContentHash) -> Result<Vec<Event>>;
    /// Zero-effective-target events per session — overflow/export feed (§2.3).
    pub fn sessionlevel_events(&self, session: &SessionId) -> Result<Vec<Event>>;

    /// §7. Returns the redaction event ids minted (one per chain member).
    pub fn redact(&self, target: &EventId) -> Result<Vec<EventId>, RedactError>;

    /// §8. The primitive for rebuild, restore, second machines, re-found sidecars.
    pub fn merge(&self, incoming: &[Event]) -> Result<MergeReport>;

    /// §4 — the bytes sidecars embed. Round-trip is byte-exact (I5).
    pub fn canonical_json(event: &Event) -> Vec<u8>;
    pub fn parse_canonical(bytes: &[u8]) -> Result<Event, CanonicalParseError>;

    /// Sidecar-writer interface over the durable dirty set (§5.3). Rows removed via
    /// ack only after a successful sidecar write.
    pub fn dirty_images(&self) -> Result<Vec<DirtyImage>>;
    pub fn ack_dirty(&self, image: &ContentHash, upto_ts: UtcMillis) -> Result<()>;

    /// Sessions (storage only; lifecycle in CAPTURE).
    pub fn open_session(&self, ctx: SessionContext) -> Result<SessionId>;
    pub fn close_session(&self, id: &SessionId, ended: UtcMillis) -> Result<()>;

    /// Drop + recompute all derived tables (§5.3–5.4) from truth tables.
    pub fn rebuild_derived(&self) -> Result<()>;
}
```

### 10.1 Folded-read query plan (normative)

`folded_journal(image)` and `events_for_image(image)` execute **at most three batched queries**:

1. **Covering range scan** on `idx_targets_image(image_hash, event_id)` → the event ids targeting the image.
2. **One batched** `SELECT … FROM annotation_events WHERE id IN (…)` for those ids.
3. **Meta-closure:** `SELECT … FROM annotation_events WHERE target_event IN (…)` — revisions, retractions, redactions of the fetched set — iterated to fixpoint (each round is one batched query; chains are shallow, so one round is the norm), answered from `idx_events_target(target_event, kind)`.

Per-event `retracted()`/`chain()` queries are **FORBIDDEN** — N+1 probes are the only mechanism that makes the 5k-event worst case slow; the batched plan keeps it at single-digit milliseconds. The fold itself (§6) runs in application memory over the fetched set.

---

## 11. Integrity invariants (testable; extends the M1 plan's list)

Each is a test in `photoproof-core/tests`. I1–I5 restate the M1 plan's list with the retraction/redaction split made exact; I6+ are new.

- **I1 Append-only.** No API mutates or deletes an event; the §5.5 triggers abort any UPDATE/DELETE on `annotation_events`/`event_targets` other than the redaction scrub.
- **I2 Scrub is minimal.** `redact()` changes exactly `text`, `payload`, `redacted_by`; every other column is byte-identical before/after (property test).
- **I3 Round-trip.** ingest → annotate (every kind) → delete SQLite → rebuild from sidecars → identical event set, identical folds, byte-identical canonical JSON per event.
- **I4 Relink / sidecar re-match** — as in the M1 plan (LIBRARY/SIDECARS own mechanics; events bind by hash, so the journal is untouched).
- **I5 Canonical stability.** `canonical_json(parse_canonical(b)) == b` for all valid `b`; no floats, no nulls, sorted keys.
- **I6 Merge is a set.** Random event sets split into random overlapping subsets applied in random orders yield identical final state (truth + derived). Merging anything twice is a no-op.
- **I7 Derived = fold.** After any operation sequence, `rebuild_derived()` changes nothing: incremental maintenance equals from-scratch recomputation.
- **I8 Redaction supremacy.** Redact on machine A; merge a stale sidecar (or full backup) containing the original content: content stays scrubbed, and with `secure_delete=ON` + WAL truncate the plaintext appears nowhere in the DB or WAL files (byte-scan test).
- **I9 Rating fold.** Current rating = max-id live rating per image under any interleaving of ratings and retractions, on any subset of multi-image targets.
- **I10 Folds terminate.** Chain resolution terminates on cycles (visited set); dangling/cyclic chains are inert; a merge supplying the missing ancestor activates the chain with no repair step.
- **I11 Dedupe.** An event targeting N images, rebuilt from its N sidecars, yields exactly one row and N target rows.
- **I12 Retraction folds out, fully.** A retracted event appears in `raw_*` reads and sidecars, and in none of: folded journal (default), FTS, vectors, rating fold. Retraction-of-retraction is rejected at append and quarantined at merge.
- **I13 Order discipline.** Journal order = ascending ULID; events with regressed `ts` (clock skew) reorder nothing; no code path sorts by `ts`.
- **I14 Monotonic mint.** Ids minted by one process strictly increase, including across a simulated wall-clock regression.
- **I15 Scrubbed shape.** A scrubbed event retains id/v/session/ts/source/kind/targets/target_event/linked_event; its canonical JSON omits `text`/`payload` and carries `redacted_by`.
- **I16 Fold reads are batched.** `folded_journal`/`events_for_image` issue at most 3 batched queries per §10.1 (a query-count assertion in tests fails on any per-event `retracted()`/`chain()` probe), and the 5k-event worst-case fold stays under 10 ms on the reference machine.

---

## 12. Open questions for the founder

Only decisions that are genuinely his:

- **Q1 — Cross-image hash visibility in sidecars.** A multi-target event's canonical JSON lists *all* its target hashes, so a sidecar handed to a client reveals opaque BLAKE3 hashes of sibling images (not their pixels, names, or paths). **Recommendation: accept** — hashes are non-reversible, and stripping `targets` per-sidecar would break byte-identical dedupe and rebuild fidelity (I3, I11). Decide; SIDECARS documents it either way.
- **Q2 — Redacted stubs in the journal panel.** Default specified here: a scrubbed event shows as an inert "[redacted]" stub (honest about the log's shape). The alternative — hide stubs entirely so redaction leaves no visible trace — is a privacy-philosophy call, not an engineering one. Storage and fold support both; UI implements whichever he picks.

Everything else in this document is decided.
