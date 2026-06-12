# spec/SIDECARS.md — Sidecar Format, Write Path, Merge, Export & Rebuild

Status: normative for M1. Closes SPEC-GAPS A3 (propagation), C2, C3.
Related: `spec/EVENTS.md` (canonical event serialization and fold rules), `spec/LIBRARY.md` (volume identity, relink, offline state), `spec/CAPTURE.md` (when events are created), `spec/RUNTIME.md`, `spec/UI.md`.

---

## 1. Principles

1. **Sidecars are the canonical truth; SQLite is a rebuildable index.** Every durability and integrity decision below follows from that ordering.
2. **One format, three roles.** The identical per-image JSON document serves as (a) the adjacent sidecar beside the image, (b) the overflow-store entry for unwritable volumes, and (c) the unit of export. One parser, one writer, one schema. Export output *is* the rebuild input.
3. **Merge = set-union by event id.** Any two copies of a journal merge by union; nothing else is ever needed. Redaction wins over union.
4. **Deterministic bytes.** A serialized sidecar is a pure function of `(schema_version, journal content)`. Two machines holding the same journal write byte-identical files — no write timestamps, writer identity, or other nondeterminism in the file. This makes diffs clean, sync trivial, and the round-trip test exact.
5. **Never destroy user data.** The writer never overwrites a file it cannot positively identify as a Photoproof sidecar, never deletes an adjacent sidecar (except the rename cases in §2.2 and §10.3), and never drops fields it does not understand.

## 2. Placement and naming

### 2.1 Adjacent sidecar filename

For an image file named `F` (the full filename including its extension), the adjacent sidecar is `F.photoproof.json` in the same directory: `IMG_4471.arw` → `IMG_4471.arw.photoproof.json`; `scan-002.tiff` → `scan-002.tiff.photoproof.json`; extensionless `frame17` → `frame17.photoproof.json`. The prefix is the image filename **byte-for-byte as it exists on disk**; the suffix `.photoproof.json` is always written lowercase.

### 2.2 Case handling

- Writing: prefix preserves the image filename's case exactly; suffix lowercase.
- Scanning: a file qualifies as a sidecar if its name ends with `.photoproof.json` compared **case-insensitively** (defensive against FAT/exFAT case mangling on archive drives).
- Adjacency match: prefix compared byte-exact on case-sensitive volumes, case-insensitively on case-insensitive volumes (volume case sensitivity per `spec/LIBRARY.md`).
- A case-only rename of the image is a relink; the sidecar is renamed to match on its next write (write new name, verify parse, delete old name).

Adjacency is only a *placement* convention. Binding is always by the embedded content hash (§10); a sidecar sitting beside the wrong image never attaches to it.

### 2.3 Collision with user files

If the target sidecar path exists and the existing file is **not** a parseable Photoproof sidecar (missing/wrong `format` marker, §4), the app MUST NOT overwrite or rename it. The image's journal routes to the overflow store (§8), a warning surfaces once per file in the integrity report, and the condition is re-checked at each reconciliation scan.

## 3. File format

A sidecar is a single UTF-8 JSON document. Top-level object:

| Key | Type | Req | Description |
|---|---|---|---|
| `format` | string | yes | Literal `"photoproof-sidecar"`. Positive identification marker; a file without it is not a sidecar (§2.3). |
| `schema_version` | integer | yes | Starts at `1`. Bump policy in §5. |
| `image` | object \| null | yes | Identity + re-match snapshot (§3.1). `null` only in session journals (§7.2). |
| `sessions` | object | yes | Denormalized metadata for every session referenced by `events` (§3.2). |
| `events` | array | yes | Complete event history targeting this image, ordered by event id ascending (§3.3). |

Unknown top-level keys are preserved (§5.2).

### 3.1 `image` object

| Key | Type | Req | Description |
|---|---|---|---|
| `hash` | string | yes | BLAKE3-256 of the image file bytes, 64 lowercase hex chars. The identity. |
| `filename` | string | yes | Image filename at last write — a snapshot for human re-matching, not identity. |
| `byte_size` | integer | yes | Image file size in bytes at hash time (cheap pre-filter when re-matching a separated sidecar). |

The snapshot is advisory: hash always wins. If filename/byte_size drift from reality (rename, same bytes), the snapshot is refreshed on the next rewrite.

### 3.2 `sessions` object

Map from session ULID → minimal denormalized session metadata, containing exactly the sessions referenced by at least one event in this file. This is what makes a sidecar readable standalone — a future reader can group and date entries without any database. Per-session value:

| Key | Type | Req | Description |
|---|---|---|---|
| `started_at` | string | yes | Session start, UTC RFC 3339. |
| `app_version` | string | yes | App version that ran the session (provenance for format archaeology). |

Deliberately **not** included: `ended_at` (sessions close on a 30-minute idle boundary, usually after the sidecar was already written; including it would force a rewrite at session close and break determinism mid-session — session end is derivable from the last event's `ts`), hostnames, machine ids.

### 3.3 `events` array

Each element is one annotation event in its canonical serialized form. **The normative field-level definition of every event kind lives in `spec/EVENTS.md`**; this spec is normative only about which events appear in which file, ordering, the redaction marker, the `targets` array, and unknown-field handling. The field table below is a plausible serialization for illustration and MUST be reconciled against EVENTS.md in the coordinator's consistency pass.

Inclusion rule: a sidecar for image `H` contains **every** event whose *effective* target set includes `H` (EVENTS §2.3) — including retracted events, and the revision/retraction/redaction events that modify them. Meta-events store zero targets and reference their target via `target_event`; because an event always travels with all meta-events that alter it, effective targets resolve within the file with no external lookup.

Ordering: ascending by event id (ULID lexicographic). Because ULIDs are monotonic within a process (kernel), this equals append order for locally-created events; after a cross-machine merge it is the deterministic canonical order. Readers MUST NOT re-sort by `ts`.

Illustrative common fields (normative form defined in `spec/EVENTS.md`):

| Field | Type | Notes |
|---|---|---|
| `id` | string | ULID, 26 chars. |
| `v` | integer | Event schema version, currently `1`. |
| `session_id` | string | Session ULID; key into `sessions`. |
| `ts` | string | UTC RFC 3339, millisecond precision. |
| `source` | string | `voice` \| `typed` \| `pencil` \| `system`. |
| `kind` | string | `remark` \| `rating` \| `stroke` \| `revision` \| `retraction` \| `redaction`. |
| `targets` | array | Plain array of 64-hex hashes, order = selection position — the event's **complete** target list, including hashes other than this file's image (§6). Empty = session-level (§7.2) and all meta-events (revision/retraction/redaction store zero targets; placement follows their target's effective targets, EVENTS §2.3). |
| `text` | string | Utterance/note text (`remark`/`revision`). Absent on strokes, ratings, meta-events, and scrubbed events. |
| `payload` | object | Kind-specific, EVENTS §3: voice remark `{ "conf_pm": <int 0..1000>, "dur_ms": <int> }`; rating `{ "value": <int 0..5> }`; stroke `{ "base_w": <int>, "orientation": <EXIF 1–8>, "points": [[x, y, p, t], …], "tool": "pencil" }` with integer ten-thousandth coords (−2500..12500), per-mille pressure, ms offsets. Removed by redaction. |
| `target_event` | string | Target event id on `revision` / `retraction` / `redaction`. |
| `linked_event` | string | Cross-modal stroke↔utterance link, carried by the later-committed event (EVENTS §3.3). |
| `redacted_by` | string | Id of the redaction event that scrubbed this one (§3.4). |

Embeddings, summaries, sentiment, captions, and all other derived values are **never** present in a sidecar (kernel). A writer holding such data has a bug; a reader encountering unknown fields preserves them (§5.2).

### 3.4 The redaction marker

A redacted event remains in the array as a scrubbed husk — the one sanctioned violation of append-only:

- Retained: `id`, `v`, `kind`, `session_id`, `source`, `targets`, `ts`, and structural refs (`target_event`, `linked_event`).
- Removed entirely: `text`, `payload`, and every other content-bearing field, **including unknown fields** (unknown fields might be content; redaction supremacy beats preservation).
- Added: `"redacted_by": "<redaction event id>"` — the redaction event itself also appears in the array (it mirrors wherever its target mirrors), carrying the when.

A retraction is different and ordinary: the original event stays intact and a separate `kind:"retraction"` event references it via `target_event`.

### 3.5 Canonical serialization (byte determinism)

Writers MUST emit:

1. UTF-8, no BOM. Non-ASCII characters raw (no `\uXXXX` beyond JSON-mandatory escapes: `"`, `\`, control characters).
2. LF line endings; the file ends with exactly one trailing LF.
3. Pretty-printed, 2-space indent (sidecars live in users' folders and their diffs; one-line JSON is hostile). One exception for signal-to-noise: an array whose elements are all numbers (e.g. a stroke point tuple) is emitted compact on one line, `[4120, 3880, 620, 0]`, with `", "` separators.
4. **Object keys sorted ascending by UTF-8 byte order at every nesting level**, unknown fields included (they sort in naturally).
5. Arrays: `events` sorted by `id` ascending; `targets` in position (selection) order as stored; stroke `points` in capture order. No other array reordering.
6. Optional/absent fields are **omitted**, never written as `null` (sole exception: top-level `image: null` in session journals, §7.2).
7. Numbers: **integers only**, without fraction or exponent; canonical event JSON contains no floats (EVENTS §4.1) — coordinates, pressure, and confidence are quantized integers. No NaN/Infinity anywhere.
8. Event objects are EVENTS §4 canonical events re-laid-out by these rules: identical fields, identical key order, identical values — only whitespace differs. EVENTS' compact single-line form is the normal form for cross-sidecar byte comparison and dedupe.

Result: identical journal content → identical bytes, on every platform, forever within a schema_version. The round-trip acceptance test (§13) depends on this.

### 3.6 Complete example

`IMG_4471.arw.photoproof.json` — a typed multi-target remark, a rating later retracted, a voice remark with low ASR confidence, a stroke linked to it, its revision, the retraction, and a redacted event with its redaction. (Event field shapes are EVENTS §4 canonical.)

```json
{
  "events": [
    {
      "id": "01JXF8M3ABCDEFGH23456789QR",
      "kind": "remark",
      "session_id": "01JXF8M2QK5T7VWXYZ0123456R",
      "source": "typed",
      "targets": [
        "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d",
        "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"
      ],
      "text": "These two are the bookends of the harbor sequence.",
      "ts": "2026-06-09T14:02:11.482Z",
      "v": 1
    },
    {
      "id": "01JXF8N7P2Q4R6S8T0V1W3X5Y7",
      "kind": "rating",
      "payload": {
        "value": 3
      },
      "session_id": "01JXF8M2QK5T7VWXYZ0123456R",
      "source": "typed",
      "targets": [
        "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d"
      ],
      "ts": "2026-06-09T14:02:40.019Z",
      "v": 1
    },
    {
      "id": "01JXJ9A1B2C3D4E5F6G7H8J9K0",
      "kind": "remark",
      "payload": {
        "conf_pm": 740,
        "dur_ms": 3260
      },
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "voice",
      "targets": [
        "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d"
      ],
      "text": "the muddy light is what holds this one together",
      "ts": "2026-07-14T09:30:05.110Z",
      "v": 1
    },
    {
      "id": "01JXJ9B4C5D6E7F8G9H0J1K2M3",
      "kind": "stroke",
      "linked_event": "01JXJ9A1B2C3D4E5F6G7H8J9K0",
      "payload": {
        "base_w": 40,
        "orientation": 1,
        "points": [
          [4120, 3880, 620, 0],
          [4600, 3520, 710, 14],
          [4710, 4300, 550, 29],
          [4090, 4410, 400, 46]
        ],
        "tool": "pencil"
      },
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "pencil",
      "targets": [
        "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d"
      ],
      "ts": "2026-07-14T09:30:07.902Z",
      "v": 1
    },
    {
      "id": "01JXJ9C7D8E9F0G1H2J3K4M5N6",
      "kind": "revision",
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "typed",
      "target_event": "01JXJ9A1B2C3D4E5F6G7H8J9K0",
      "targets": [],
      "text": "the moody light is what holds this one together",
      "ts": "2026-07-14T09:31:42.560Z",
      "v": 1
    },
    {
      "id": "01JXJ9D0E1F2G3H4J5K6M7N8P9",
      "kind": "retraction",
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "system",
      "target_event": "01JXF8N7P2Q4R6S8T0V1W3X5Y7",
      "targets": [],
      "ts": "2026-07-14T09:33:10.004Z",
      "v": 1
    },
    {
      "id": "01JXJ9E3F4G5H6J7K8M9N0P1Q2",
      "kind": "remark",
      "redacted_by": "01JXJ9F6G7H8J9K0M1N2P3Q4R5",
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "voice",
      "targets": [
        "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d"
      ],
      "ts": "2026-07-14T09:35:02.770Z",
      "v": 1
    },
    {
      "id": "01JXJ9F6G7H8J9K0M1N2P3Q4R5",
      "kind": "redaction",
      "session_id": "01JXJ99XYZ0123456789ABCDEF",
      "source": "system",
      "target_event": "01JXJ9E3F4G5H6J7K8M9N0P1Q2",
      "targets": [],
      "ts": "2026-07-14T09:40:55.231Z",
      "v": 1
    }
  ],
  "format": "photoproof-sidecar",
  "image": {
    "byte_size": 25431808,
    "filename": "IMG_4471.arw",
    "hash": "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d"
  },
  "schema_version": 1,
  "sessions": {
    "01JXF8M2QK5T7VWXYZ0123456R": {
      "app_version": "0.3.1",
      "started_at": "2026-06-09T13:58:00.000Z"
    },
    "01JXJ99XYZ0123456789ABCDEF": {
      "app_version": "0.4.0",
      "started_at": "2026-07-14T09:25:13.000Z"
    }
  }
}
```

(Yes, `events` sorts before `format` — lexicographic key order everywhere is worth more than a pretty header.)

## 4. Reading: validation

A reader accepts a file as a sidecar iff: valid UTF-8 JSON; top-level object; `format == "photoproof-sidecar"`; `schema_version` a positive integer; `image` an object with a 64-lowercase-hex `hash` (or `null` in a session journal); `events` an array. Per-event validation is per `spec/EVENTS.md`; an event that fails validation is preserved verbatim as an opaque blob (kept on rewrite, not indexed, surfaced in the integrity report) — a malformed *event* never poisons the rest of the file.

## 5. Forward / backward compatibility

### 5.1 `schema_version` bump policy

- **Additive changes never bump.** New optional fields, new event kinds, and new enum values ship within version 1, relying on unknown-field/unknown-kind preservation. This is expected to cover nearly all evolution.
- **Bump only for breaking changes:** removing/renaming a required field, changing a field's meaning or type, or changing canonical serialization rules. A bump is a project-level event requiring a written migration in `spec/DECISIONS.md`.

### 5.2 Unknown-field preservation

A reader MUST retain every field it does not recognize, at every nesting level, and a writer MUST re-emit them byte-equivalently (modulo canonical key sorting) on rewrite. Implementation note: deserialize into structs carrying a `#[serde(flatten)] BTreeMap<String, serde_json::Value>` catch-all; the unknown map round-trips through SQLite alongside the event row so a rewrite from the index loses nothing. Unknown event *kinds* are likewise preserved end-to-end: stored, re-emitted, not indexed, not displayed (or shown as "unsupported entry — made by a newer version") — never dropped. Sole exception: redaction scrubs unknown fields on the redacted event (§3.4).

### 5.3 A v1 reader meeting a v2 file

Because bumps are breaking by definition, an older reader MUST NOT guess:

1. The file is **opaque and inviolable**: never rewritten, never renamed, never imported (semantics may have changed under the same field names).
2. The user is notified once ("this library contains journals written by a newer Photoproof; upgrade to read them"), with the count in the integrity report.
3. New events captured for that image route to the overflow store (§8) under the current reader's schema version. Union-merge reconciles the two files when an upgraded app next sees them.

A v2 reader MUST read all prior versions, and upgrades a file to the current version only when it has a reason to rewrite it; rewriting always emits the writer's current schema version.

## 6. Multi-target events

An event targeting N images is serialized **in full** (complete `targets` list) into **all N** sidecars. Consequences:

- Any single sidecar suffices to reconstruct the event and its complete target set, including targets whose images/sidecars are lost.
- Rebuild dedupes by event id: N copies fold to one `annotation_events` row plus N `event_targets` rows (hash + position from the `targets` array).
- Retracting/revising/redacting a multi-target event dirties all N sidecars (§11 for the redaction case).
- Copies of the same event MUST be byte-identical across sidecars (they are, by §3.5 determinism). Divergent copies are a conflict (§10.2).

## 7. Where every event lives

### 7.1 Image-targeted events

In the adjacent sidecar of each target, or in the overflow store when that image's volume is unwritable (§8). Per (event, image), one of the two locations is the live write target at any time; merge makes any duplication harmless.

### 7.2 Session journals (events with zero targets)

Session-level remarks target no image, so no image sidecar can carry them — but sidecars are the truth, so they need a file. They live in **session journal** files in app data:

```
<app-data>/photoproof/journal/sessions/<session-ulid>.photoproof.json
```

Identical envelope and rules, with `image: null`, the `sessions` map containing that one session, and `events` holding only that session's zero-target events (sorted by id). Session journals are written by the same debounced writer, are included in every export (§12), and are consumed by rebuild. A session with no zero-target events gets no file. Like the overflow store, they live in app data and are covered by the backup caveat in §8.3.

## 8. Overflow store (read-only / unwritable volumes)

### 8.1 Layout

When the adjacent location is unwritable (read-only volume, EROFS/EACCES, no-write network share, §2.3 collision), the sidecar is written — **byte-identical format, including the real `image.filename`** — to a content-hash-keyed fan-out inside app data:

```
<app-data>/photoproof/journal/overflow/<h[0..2]>/<h[2..4]>/<full-hash>.photoproof.json
```

e.g. `overflow/7c/9e/7c9e1d3f…b1c3d.photoproof.json`. Two-level fan-out (256 × 256) keeps directories small at 100k+ images. The filename is the hash because adjacency is meaningless here; the embedded `image` block still records the original filename and size for re-matching.

### 8.2 Migration when a volume becomes writable

At volume mount and at every reconciliation scan, for each overflow entry whose image now resolves to a writable path: (1) union-merge the overflow entry with any existing adjacent sidecar (§10); (2) write the merged result to the adjacent location (atomic, §9.2); (3) read back and re-validate the adjacent file (parse + hash match); (4) only then delete the overflow entry. A failure at any step leaves the overflow entry in place; the operation is idempotent and retries next scan. The reverse transition (volume turns read-only) copies nothing: the overflow path simply becomes the live write target; the stale adjacent sidecar remains valid (a subset) and reconciles by union whenever the volume is writable again.

### 8.3 The honesty caveat (normative, user-facing)

For images on unwritable volumes — and for session journals — the canonical truth lives in the app data directory, not beside the images. **The "sidecars beside your photos are the truth" promise degrades to "the truth is in Photoproof's app data" for exactly these cases.** Therefore: the export (§12) always includes the entire overflow store and all session journals; the export UI states how many journals exist only in overflow; and the backup guidance (docs + first run) says plainly that backing up image folders alone does not capture overflow journals — export, or back up app data too.

## 9. Write path

### 9.1 Debounce and flush triggers

Events commit to SQLite (WAL) first — that append is the moment of capture — then the sidecar writer mirrors them out, coalescing dirty state per image:

- **Debounce window: 2 s** of write-quiet per image, with a **hard cap of 5 s** from the first unflushed event for that image (this is the "sidecar within 5 s of capture" promise; FEATURES.md `[M1]`).
- **Immediate flush (bypass debounce):** redaction (§11), session end, app shutdown (writer drains before exit), explicit export, overflow→adjacent migration.
- Burst behavior: a 90-minute talking cull rewrites a touched image's sidecar at most every 2 s, not per event. If measured write amplification hurts spinning archive drives, the debounce may be tuned upward per volume class, but the 5 s cap is contractual.

### 9.2 Atomic write

Sidecars are always replaced whole, never appended or patched in place. The writer is
**mtime-stable**: before writing, compare the serialized bytes against the existing file and
**skip the write entirely when identical** — no temp file, no rename, no mtime touch. (§3.5
determinism makes the comparison exact; this closes cloud-sync churn loops where a sync
service touching the file would otherwise ping-pong with our writer, each side re-triggering
the other forever. See LIBRARY.md §5.2.)

1. Serialize the complete canonical document (§3.5).
2. If the target exists and its bytes equal the serialization, **stop** — the write is
   skipped (mtime-stable no-op).
3. Write to a temp file in the **same directory**: `<sidecar-name>.tmp-<8-hex-random>`.
4. Flush + fsync the temp file.
5. Atomically rename over the target — POSIX `rename(2)` then fsync the parent directory; Windows `ReplaceFileW` / `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
6. On any failure: delete the temp file, leave the target untouched.

Invariant: at every instant the target path holds either the previous complete sidecar or the new complete sidecar. `kill -9` at any point cannot corrupt an existing sidecar (acceptance §13.3).

### 9.3 Partial-write recovery

Orphaned `*.photoproof.json.tmp-*` files (crash between steps 3–5) are ignored by all scanners and deleted by the writer when older than one hour or when the same target is next written. They are never parsed and never merged.

### 9.4 Write failures, retry, durability window

- **Permanent for this location** (EROFS, EACCES, read-only mount, §2.3 collision): route to overflow immediately; mark the location unwritable in the library index.
- **Transient** (EBUSY, ENOSPC, network drop, volume vanished mid-write): the image stays in the persistent dirty queue — `sidecar_dirty(image_hash, first_dirty_at, attempts, last_error)` in SQLite — with backoff 1 s, 5 s, 30 s, 5 min, then once per reconciliation scan. ENOSPC additionally raises a user-visible warning (truth cannot land anywhere).
- Volume offline before flush: the dirty entry persists; flush happens at mount.

Durability window, stated honestly: an event is at risk only between its SQLite commit and its sidecar flush (≤ 5 s normally; longer if the volume is unreachable, during which the committed DB row and dirty queue carry it). Losing the DB *and* the sidecar inside that window loses the event. The dirty queue is index data; after a rebuild-from-sidecars it is empty by construction (DB == sidecars).

### 9.5 When the writer fires beyond capture

Reconciliation scans also dirty an image when: the adjacent sidecar is missing or stale relative to the index (user deletion, restored old backup); the filename snapshot is stale after a relink; a redaction-registry id (§11) appears unscrubbed in the file; or a merge (§10) produced a superset. The writer is the single chokepoint: nothing else ever writes sidecar bytes.

## 10. Merge and re-matching

### 10.1 Union by event id, redaction supremacy

Reading any sidecar — at reconciliation, rebuild, overflow migration, or when the user drops in files from a backup or a second machine — is always the same operation:

1. Validate (§4); bind by embedded hash, never by adjacency.
2. Per event: if the id is **unknown**, import it (row + targets + FTS/derived per EVENTS.md fold rules). If **known**, the copies should be byte-identical; keep the existing row.
3. **Redaction registry check (supremacy):** the index maintains `redactions(event_id, redacted_at)`, populated from every redaction marker ever seen (and rebuilt from markers during rebuild). If an incoming event id is in the registry but the incoming copy carries content, the content is discarded unread — never indexed, never displayed, never embedded — and the source file is marked dirty for a scrubbed rewrite. A redaction marker arriving for a locally-intact event triggers local redaction (§11). **Reading a sidecar never resurrects redacted content.**
4. If the union produced a superset of the file just read, mark it dirty so it converges to the canonical superset bytes.

Conflicting *copies of the same sidecar* (adjacent + overflow, two paths holding byte-identical images, a restored backup) merge by running the above on each; all copies then converge to identical canonical bytes.

### 10.2 Same event id, different content

Should be impossible (events are immutable). If observed and neither copy is a redaction marker: keep the copy whose canonical serialization is byte-wise lesser (deterministic), preserve the loser verbatim in the integrity report, and flag the pair as a corruption warning. If either copy is a redaction marker, the marker wins unconditionally.

### 10.3 Re-matching a separated sidecar

| Situation | Behavior |
|---|---|
| Sidecar found, image absent (hash resolves to no known path) | Import the journal (union). The image exists in the index as *missing*: searchable, journal intact, cached thumbnail shown if one exists, else a placeholder. The `image.filename`/`byte_size` snapshot is shown to help the user hunt for the file; when any file with that hash is later ingested anywhere, everything reattaches automatically. The orphan sidecar file is left in place untouched. |
| Image found, no sidecar, but the index has a journal (user deleted it, or restored an image-only backup) | Reconciliation marks the image dirty; the writer regenerates the sidecar from the index. |
| Image + stale sidecar (file is a subset of the index) | Union (no-op), rewrite to superset. |
| Image + foreign/newer sidecar (file has events the index lacks — second machine, restored backup) | Union imports them; rewrite converges the file. No clocks compared, no "newer wins" — union is the whole algorithm. |
| Adjacent sidecar whose embedded hash ≠ the adjacent image's hash (user shuffled files) | Never bind by adjacency. (a) Import the sidecar's journal under its embedded hash; if that hash resolves to a writable path elsewhere, write it adjacent there, else it lives in overflow. (b) The slot is then rewritten for the image actually present, if it has a journal; the mismatched file is replaced only after step (a) verifiably preserved its content. Nothing is lost; both journals end up keyed correctly. |
| `byte_size`/`filename` snapshot disagrees but hash matches | Hash wins; snapshot refreshed on next rewrite. |
| Parse failure (corrupt sidecar) | Rename aside to `<name>.photoproof.json.corrupt-<UTC compact ts>` (preserved for manual inspection, never deleted), write a fresh sidecar from the index if a journal exists, report. |

## 11. Redaction propagation

Redaction is the one sanctioned violation of append-only. When the user redacts event `E` (targets `H1..Hn`), in order:

1. **Index scrub (synchronous):** scrub the `annotation_events` row to the husk of §3.4; purge FTS entries, vectors referencing `E`, and derived text; insert into the redaction registry; delete retained audio if any exists (per CAPTURE.md, normally none survives finalization).
2. **Durable scrubbed record (synchronous):** write the scrubbed sidecar to the **overflow store** for every target `Hi` whose adjacent location is not immediately writable. A durable redaction marker therefore exists on the app-data volume *before* the UI reports success, regardless of which archive drives are plugged in.
3. **Adjacent rewrite (immediate, bypasses debounce):** rewrite every reachable adjacent sidecar of `H1..Hn` (and the session journal, if `E` was session-level) now.
4. **Queue for offline/unwritable volumes:** for each unreachable copy, enqueue `redaction_queue(event_id, image_hash, volume_id, queued_at)` in SQLite. On volume mount and at each reconciliation scan, drain: rewrite, verify by re-read (the event must appear only as a marker), then dequeue.
5. **Backstop beyond the queue:** the queue is an accelerator, not the invariant. The invariant is §10.1 step 3 — *any* scan that reads unscrubbed content for a registry id rewrites that file. Even if the queue is lost with the DB, any surviving marker (step 2 made one durable) re-seeds the registry at rebuild, and reconciliation re-scrubs stragglers.

**The redaction guarantee, stated honestly (this text, or equivalent, appears in user-facing docs):** Redaction permanently removes the content from Photoproof's database, search indexes, vectors, and from every sidecar file the app can reach, now or whenever an offline volume next mounts. It cannot scrub copies made outside the app: backups of your image folders, sidecars copied to other drives or machines the app has never seen, or cloud-sync snapshots. Photoproof-aware machines scrub on their next merge (the marker propagates and wins); foreign copies are beyond reach. The redacted event's existence — id, timestamp, kind, and which images it targeted — remains visible; only its content is destroyed.

## 12. Export and rebuild

### 12.1 Export layout

One-click full export produces a directory (default `photoproof-export-<UTC compact timestamp>/`):

```
photoproof-export-20260609T193000Z/
├── manifest.photoproof.json
├── sidecars/<h[0..2]>/<h[2..4]>/<full-hash>.photoproof.json   # one per annotated image
└── sessions/<session-ulid>.photoproof.json                    # session journals
```

Exported sidecars are serialized **from the index** in canonical form — the index is, by the merge invariant, the union of every journal the app has ever read, which is what makes export complete even while archive volumes are offline — and are hash-named like overflow entries. Only annotated images export (no journal → no file). An image whose only truth is in overflow exports identically to any other: one format, three roles.

### 12.2 Manifest schema

`manifest.photoproof.json`:

| Key | Type | Description |
|---|---|---|
| `format` | string | `"photoproof-export-manifest"`. |
| `schema_version` | integer | Manifest schema version, starts at 1 (versioned independently of the sidecar schema). |
| `sidecar_schema_version` | integer | Schema version of the exported sidecars. |
| `app_version` | string | Exporting app version. |
| `exported_at` | string | UTC RFC 3339. (The manifest is metadata, not journal truth — it is allowed this nondeterminism.) |
| `counts` | object | `{ "images": n, "events": n, "sessions": n, "redactions": n }`. |
| `images` | array | Per image: `{ "hash", "filenames": [..], "byte_size", "event_count", "sidecar": "<relative path>" }`. |
| `session_journals` | array | `{ "session": <ulid>, "path": "<relative path>" }`. |
| `volumes` | array | `{ "id", "label", "last_seen" }` per LIBRARY.md volume identity — context for humans re-locating images, never identity. |

Example (abridged):

```json
{
  "app_version": "0.4.2",
  "counts": { "events": 18342, "images": 4117, "redactions": 3, "sessions": 412 },
  "exported_at": "2026-06-09T19:30:00Z",
  "format": "photoproof-export-manifest",
  "images": [
    {
      "byte_size": 25431808,
      "event_count": 7,
      "filenames": ["IMG_4471.arw"],
      "hash": "7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d",
      "sidecar": "sidecars/7c/9e/7c9e1d3f5a2b4c6d8e0f1a3b5c7d9e2f4a6b8c0d1e3f5a7b9c2d4e6f8a0b1c3d.photoproof.json"
    }
  ],
  "schema_version": 1,
  "session_journals": [
    { "path": "sessions/01JXJ99XYZ0123456789ABCDEF.photoproof.json", "session": "01JXJ99XYZ0123456789ABCDEF" }
  ],
  "sidecar_schema_version": 1,
  "volumes": [
    { "id": "vol-9f2a", "label": "Archive 2019-2026", "last_seen": "2026-06-01T10:11:12Z" }
  ]
}
```

### 12.3 Rebuild-from-sidecars

Rebuild accepts either an export directory or the live world (watched roots + overflow + session journals). The manifest, when present, is **advisory** — a cross-check, never a requirement; rebuild works from bare sidecars found by scan.

1. **Scan:** enumerate `*.photoproof.json` (case-insensitive suffix), skipping `*.tmp-*` orphans.
2. **Parse & validate** (§4). Failures are quarantined and reported; nothing aborts the run.
3. **Pass 1 — redaction registry:** collect every redaction marker from every file first.
4. **Pass 2 — union:** group by embedded hash; union events by id across all copies with redaction supremacy (§10.1) and the §10.2 conflict rule; collect event rows, `event_targets` (from each event's `targets`), sessions (from the denormalized maps; identical session ids across files must agree — mismatches are reported, first-seen wins), and images (hash + snapshot).
5. **Insert (ingestion discipline, normative):** after the union pass completes, **sort the full event set ascending by event id** before insertion — monotonic ULID keys make every insert a right-edge B-tree append instead of a rebalancing storm ([andersmurphy: UUID primary keys in SQLite](https://andersmurphy.com/2026/06/05/the-perils-of-uuid-primary-keys-in-sqlite.html)). Insert in transactions of **~10k events**. The same discipline applies to large merges — see `spec/EVENTS.md`.
6. **Derived (single pass, after the union):** populate FTS from folded text (revisions applied, retracted excluded, redactions absent — fold per EVENTS.md) and the derived tables in **one pass after the union completes** — never interleaved per-file. Vectors/summaries are queued for background re-derivation (RUNTIME/RETRIEVAL); they are never in sidecars. Finish with `ANALYZE` + FTS `optimize` + `wal_checkpoint(TRUNCATE)`.
7. **Integrity report (always produced):** files scanned/parsed/failed (paths + errors), events imported, duplicate copies deduped, redactions enforced (incl. files dirtied for re-scrub), id conflicts (§10.2), opaque newer-version files (§5.3), unknown kinds preserved, and manifest discrepancies when a manifest was present (missing/extra files, count mismatches).
8. Post-rebuild reconciliation rewrites any sidecar now a subset of the union (convergence).

## 13. Acceptance criteria

1. **Round-trip:** ingest → annotate (incl. a multi-target remark, a revision, a retraction, a redaction, a stroke) → delete SQLite + caches → rebuild from sidecars → the folded journal is **byte-identical** (event rows, targets, fold results); a fresh export is byte-identical to one taken before deletion, manifest `exported_at` excepted.
2. **Latency:** a typed note's adjacent sidecar is on disk, valid, and containing the event ≤ 5 s after capture, under sustained note bursts.
3. **Crash safety:** `kill -9` injected at arbitrary points in the §9.2 sequence (fault-injection harness), repeated 1000×: the target path always parses and equals either the old or the new complete document; orphan temps are cleaned on the next run.
4. **Redaction propagation:** redact an event targeting an image on an unmounted volume → a scrubbed overflow record exists before the UI confirms; mount the volume → the adjacent sidecar is scrubbed and the queue drains. Then restore a pre-redaction copy of that sidecar from "backup" → merge imports none of the content, FTS/vector search find nothing, and the file is rewritten scrubbed.
5. **Unknown-field preservation:** inject unknown fields at top level, in `image`, in `sessions` values, and in events (plus one whole unknown `kind`) → rewrite preserves all of them in canonical sorted position; a v2-marked file is never modified or imported, and is reported.
6. **Determinism:** the same journal serialized repeatedly, after rebuild, and on a second platform → byte-identical files (hash-compare in CI on Linux/macOS/Windows runners).
7. **Multi-target dedupe:** one event targeting 3 images appears in 3 sidecars; rebuild yields exactly one event row and 3 ordered target rows.
8. **Collision safety:** a pre-existing user file named `IMG_0001.jpg.photoproof.json` that is not a sidecar is never modified; the journal lands in overflow; the report says so.
9. **Overflow migration:** annotate images on a read-only volume → remount writable → adjacent sidecars appear with full history, overflow entries are gone, and a second migration run is a no-op.
10. **Re-match:** every row of the §10.3 table has a test, including the swapped-image/adjacent-mismatch case ending with both journals intact under their correct hashes.
11. **Mtime-stable writer:** flushing an image whose sidecar already holds the identical canonical bytes performs no write — file mtime and inode/file-id unchanged, no temp file created (asserted via instrumentation); a one-event change still writes normally. Repeated reconciliation passes over a converged library leave every sidecar's mtime untouched.

## 14. XMP keyword export (post-M3 — design space reserved, not specified)

A one-way, lossy, **derived** export for Lightroom/Capture One interop: selected journal-derived keywords (e.g. the rating fold, collection names) written to standard XMP. Constraints already decided: export-only (XMP is never a rebuild input and never truth); the app never writes into image files — XMP sidecars only; and because darktable/Lightroom also own `.xmp` sidecar files, the design must resolve coexistence with foreign XMP (a separate namespace at minimum, possibly a distinct file) before implementation. Everything else — keyword vocabulary, sync cadence, collision policy — is deliberately unspecified until post-M3.

---

*End of spec. Field-level event serialization defers to `spec/EVENTS.md`; volume identity and writability detection defer to `spec/LIBRARY.md`.*
