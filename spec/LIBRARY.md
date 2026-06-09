# spec/LIBRARY.md — Library Identity, Volumes, Ingest, and Rendering Pipeline

Status: v1 normative. Closes SPEC-GAPS C1, C4 (plus C5-adjacent path edges), D1, D2, D3.
Siblings: EVENTS, SIDECARS, CAPTURE, RETRIEVAL, RUNTIME, UI. Kernel terms are assumed, not
restated. This document is the **index side** of the system: every table here is rebuildable
from the filesystem plus the sidecar set. The event log (EVENTS.md) and sidecars
(SIDECARS.md) are truth; nothing in this spec is.

## 1. Image identity

### 1.1 The hash

Identity = **BLAKE3-256 of the complete file bytes**, 64 lowercase hex characters — the
`image_hash` everywhere: DB, sidecars, event targets, cache filenames, manifests. Hashing is
over the file as stored (RAW container, JPEG, …); no pixel-level or metadata-stripped hashing
in v1. Images are known by hash, never by path; paths are claims about where a hash's bytes
were last observed (§3).

### 1.2 Hashing strategy (performance)

Files ≥ 1 MiB are memory-mapped (`memmap2`) and hashed with `blake3`'s rayon parallel mode;
smaller files use a plain buffered read (mmap costs more than it saves there). The hash queue
processes files in **ascending size order** so the grid populates quickly — small JPEGs
become browsable while 80 MB RAWs are in flight. File-level parallelism: up to
`min(physical_cores, 8)` concurrent files; BLAKE3-internal rayon only for files ≥ 64 MiB
(avoids oversubscription). Hashing is disk-bound: required ≥ 1 GB/s sustained aggregate on
NVMe (BLAKE3 exceeds 5 GB/s/core; falling short is a pipeline bug). End-to-end budget: §12;
slow-volume behavior: §12.1.

### 1.3 Bytes changed in place = new image

A re-export overwriting `IMG_4471.jpg` is **a new image identity**; Photoproof never
guesses whether an overwrite is "the same picture." When bytes at a known path change (§7):

1. New bytes hashed → new `images` row (if unseen) + M1 passes enqueued.
2. The old image's path row there → `stale` (reason `superseded`); a new `active` row binds
   the path to the new hash.
3. The old image row, journal, previews, and sidecar content are **untouched** — no events
   move, copy, or migrate. The old journal stays reachable via the old hash's record: through
   search (FTS/semantic hits still resolve to it) and the old image's journal panel.
4. The old image's availability typically becomes `missing` (§8) unless other paths remain.

UI-facing concept (consumed by UI.md): **dormant prior version** — the single-image view for
the new hash MAY show a one-line affordance ("a previous file here has journal entries —
view"), derived entirely from stale `superseded` path rows at the same `(volume_id,
rel_path)`. No merge, no automatic anything. Defense: any auto-migration heuristic will
eventually attach years of reflection to the wrong pixels, and the journal is the product; a
wrong non-migration costs one click, a wrong migration silently corrupts truth. A
user-invoked "migrate journal" action is a recorded **non-feature** (candidate post-M3).

## 2. Images and duplicates

Byte-identical copies = **one image, N path rows**: one journal, one preview set, one
sidecar *content* (fan-out across duplicate locations is SIDECARS.md's jurisdiction). The
`images` row carries the EXIF subset (§9.6), pixel geometry, and format — all derived, all
rebuildable. No "delete image" operation exists in v1: files vanishing make an image
`missing`; journals are never deleted (redaction, per EVENTS.md, scrubs events, not images).

### 2.1 RAW+JPEG pairs (v1 stance)

RAW+JPEG = **two images, two journals, no stacking**. Recorded future feature (non-feature
now): pairing heuristic — same basename stem + `DateTimeOriginal` (±1 s) + folder — shown as
a stack with a shared journal *view*, never merged event logs. Its absence in v1 is a
decision, not an oversight.

## 3. Paths

A path row asserts: "these bytes were observed at `(volume_id, rel_path)` with this size and mtime."

- `rel_path` is relative to the **volume root**, UTF-8, `/`-separated on all platforms.
  Non-UTF-8 filenames are skipped and logged (lossy storage would break relinking).
  Comparisons are byte-wise; a case-only rename is an ordinary rename (relink, §7.4).
- Symlinks are **not followed**. Hard links are fine: same bytes, two paths, one image.
- States: `active` and `stale` (`stale_reason` ∈ `moved | deleted | superseded |
  root-removed`). Stale rows are kept forever — the breadcrumbs for dormant-prior-version
  lookup (§1.3) and "last seen at" UI copy.

### 3.1 Current-best-path resolution

When the UI needs one path (title bar, reveal-in-Finder, folder attribution), resolve among
`active` rows by, in order: (1) **online** volume beats offline; (2) under an **active
watched root** beats outside; (3) **writable** beats read-only; (4) most recent
`last_verified_at`; (5) lexicographically smallest `(volume_id, rel_path)` as deterministic
tiebreak. If no row survives filter 1, return the best *offline* path plus the availability
state (§8) so the UI can badge it.

## 4. Volumes

The volume is the unit of online/offline, read-only detection, and path anchoring. Mount
points and drive letters are unstable; volume identity must not be.

### 4.1 Identity recipe (precedence, highest first)

1. **Marker file** `.photoproof-volume` at the volume root:
   `{ "schema_version": 1, "volume_ulid": "<ULID>", "created_at": "<RFC3339>",
   "created_by": "photoproof/<version>" }`. If present and parseable, its ULID **is** the
   identity, overriding platform ids — the only identity that survives a drive moving between
   OSes, USB enclosures, and exFAT's unstable serials.
2. **Platform-native id**: macOS — volume UUID via `DADiskCopyDescription`
   (`kDADiskDescriptionVolumeUUIDKey`, diskutil's "Volume UUID"; `statfs.f_fsid` is *not*
   used — unstable across mounts). Windows — volume serial from `GetVolumeInformationW`.
   Linux — filesystem UUID by matching the mount source device against
   `/dev/disk/by-uuid/*`; `statvfs.f_fsid` only as a corroborating signal.
3. **Heuristic fingerprint** (last resort): `blake3(fs_type | label | capacity_bytes)`
   truncated to 16 hex chars, `platform_kind = 'heuristic'`, flagged low-confidence in the
   debug panel.

Marker policy (decision): written **automatically on first ingest of any writable volume**
hosting a watched root — no prompt. A 200-byte dotfile, documented in user docs beside the
sidecar explanation, exactly as invasive as the sidecars the product is built on. Unwritable
volume → levels 2–3 now, marker written opportunistically if it ever mounts writable; never
written to the filesystem root of system/boot volumes (platform ids are reliable there).
Conflicts: marker matches volume A but platform id matches volume B (cloned drive) →
**marker wins**, B's platform id cleared, warning logged; two mounted volumes with the same
marker ULID (full clone) → the second registers as a *new* volume with a fresh marker,
debug-panel warning.

### 4.2 Volume state machine

```
offline ──(mount detected / startup probe: identity resolved)──▶ online
offline ◀──(unmount event / mount point gone on probe)────────── online
```

Exactly two states; `read_only` is an attribute refreshed on each online transition.
**online**: update `mount_point` (remount under a different mount point — `/Volumes/Archive
1`, `E:`→`F:` — is the normal case; nothing is keyed on mount point, so remount
reconciliation is just this field update), set `last_seen_at`, refresh `read_only` (§4.3),
start watchers, schedule a reconciliation scan (§7.3). **offline**: stop watchers; path rows
untouched (availability derivation handles the rest, §8); sidecar writes queue per
SIDECARS.md. Detection: DiskArbitration (macOS), `WM_DEVICECHANGE` (Windows), `/proc/mounts`
poll at 5 s (Linux), plus a probe at startup and before any reconciliation.

### 4.3 Read-only detection

On each online transition: check the mount's read-only flag (`statfs`/`statvfs`,
`FILE_READ_ONLY_VOLUME`), then **verify with a probe** — create-and-delete a temp file in
the watched root (flags lie on network mounts). Store in `volumes.read_only`. Consequence
(normative for SIDECARS.md): sidecars for images whose only active paths sit on read-only
volumes route to the overflow store; a later writable mount triggers flush-to-adjacent.

## 5. Watched roots

- A root = `(volume_id, rel_path)`. Registration resolves the chosen directory to volume +
  relative path, writes the marker if applicable (§4.1), enqueues an initial scan.
- **Nested roots are forbidden** (decision): registering inside or above an existing active
  root is rejected; the UI offers "replace existing root" as the only escape hatch.
- **Removal** sets `roots.state = 'removed'` and marks its path rows `stale`
  (`root-removed`). Nothing else: journals kept, events never deleted, previews retained,
  search still works (availability → `missing` unless other paths exist). Re-registering an
  overlapping root later relinks everything by hash via the reconciliation fast path.

### 5.1 Exclusion rules

Skipped during walks and ignored by the watcher: hidden entries (leading `.`, files and
directories — covers `.photoproof-volume`, `.DS_Store`, `.dtrash`, `.git`);
`*.photoproof.json` sidecars; known tool/cache dirs, case-insensitive — `@eaDir`, `__MACOSX`,
`*.lrdata`, `*.cocatalogdb`, Capture One session subfolders (`Cache`, `Proxies`,
`Thumbnails`), `Lightroom Catalog*`, `.thumbnails`, `$RECYCLE.BIN`, `System Volume
Information`, `lost+found`, `node_modules`; files off the format allowlist (§9.1) or > 2 GiB
(sanity ceiling, logged); the app's own data dir if a root ever contains it. Ships as a
built-in constant; user-editable exclusions are post-v1.

## 6. Library-side SQLite DDL

Migrations in `photoproof-core/src/db/`. WAL mode; timestamps RFC 3339 UTC TEXT. Normative:
column additions are free, semantic changes require a spec revision.

```sql
CREATE TABLE volumes (
  volume_id       TEXT PRIMARY KEY,             -- app-assigned ULID
  marker_ulid     TEXT UNIQUE,                  -- from .photoproof-volume; NULL if none
  platform_id     TEXT,                         -- platform-native id; NULL if unavailable
  platform_kind   TEXT CHECK (platform_kind IN ('macos-uuid','win-serial','linux-fsuuid','heuristic')),
  label TEXT, fs_type TEXT, capacity_bytes INTEGER,
  read_only       INTEGER NOT NULL DEFAULT 0,
  state           TEXT NOT NULL DEFAULT 'offline' CHECK (state IN ('online','offline')),
  mount_point     TEXT,                         -- current; NULL when offline
  first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
);

CREATE TABLE roots (
  root_id       TEXT PRIMARY KEY,               -- ULID
  volume_id     TEXT NOT NULL REFERENCES volumes(volume_id),
  rel_path      TEXT NOT NULL,                  -- '' = volume root; '/'-separated UTF-8
  display_name  TEXT,
  state         TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','removed')),
  created_at TEXT NOT NULL, removed_at TEXT,
  UNIQUE (volume_id, rel_path)
);

CREATE TABLE images (
  image_hash        TEXT PRIMARY KEY,           -- blake3-256, 64 lowercase hex
  byte_size         INTEGER NOT NULL,
  format            TEXT NOT NULL,              -- 'jpeg','png','tiff','webp','heic','raw'
  raw_subtype       TEXT,                       -- 'cr3','nef',…; NULL for non-RAW
  pixel_width INTEGER, pixel_height INTEGER,    -- as stored (pre-orientation)
  exif_orientation  INTEGER NOT NULL DEFAULT 1, -- 1..8; 1 if absent
  -- EXIF subset (§9.6): nullable, read-only, rebuildable
  capture_ts TEXT,                              -- RFC3339 UTC; NULL if undated
  capture_tz_offset TEXT,                       -- '+02:00' etc.
  camera_make TEXT, camera_model TEXT, lens_model TEXT,
  focal_length_mm REAL, iso INTEGER, f_number REAL,
  exposure_time TEXT, gps_lat REAL, gps_lon REAL,  -- exposure as written, e.g. '1/250'
  first_ingested_at TEXT NOT NULL
);

CREATE TABLE paths (
  path_id          TEXT PRIMARY KEY,            -- ULID
  image_hash       TEXT NOT NULL REFERENCES images(image_hash),
  volume_id        TEXT NOT NULL REFERENCES volumes(volume_id),
  root_id          TEXT REFERENCES roots(root_id),
  rel_path         TEXT NOT NULL,
  size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,  -- mtime: ns since epoch, fs precision
  state            TEXT NOT NULL CHECK (state IN ('active','stale')),
  stale_reason     TEXT CHECK (stale_reason IN ('moved','deleted','superseded','root-removed')),
  first_seen_at TEXT NOT NULL, last_verified_at TEXT NOT NULL, stale_since TEXT
);
CREATE UNIQUE INDEX paths_active_loc ON paths(volume_id, rel_path) WHERE state = 'active';
CREATE INDEX paths_by_image ON paths(image_hash, state);
CREATE INDEX paths_stale_loc ON paths(volume_id, rel_path) WHERE state = 'stale';

CREATE TABLE ingest_passes (
  image_hash    TEXT NOT NULL REFERENCES images(image_hash),
  pass_name     TEXT NOT NULL,                  -- registry, §10.1
  pass_version  INTEGER NOT NULL,
  model_id      TEXT,                           -- NULL for non-model passes
  state         TEXT NOT NULL CHECK (state IN ('pending','running','done','error','skipped')),
  priority      INTEGER NOT NULL DEFAULT 2,     -- §10.3
  attempts      INTEGER NOT NULL DEFAULT 0,
  error         TEXT,                           -- last error; NULL unless 'error'
  enqueued_at TEXT NOT NULL, started_at TEXT, completed_at TEXT,
  PRIMARY KEY (image_hash, pass_name, pass_version)
);
CREATE INDEX ingest_queue ON ingest_passes(state, priority, enqueued_at)
  WHERE state = 'pending';

CREATE TABLE preview_artifacts (
  image_hash         TEXT NOT NULL REFERENCES images(image_hash),
  kind               TEXT NOT NULL CHECK (kind IN ('thumb','display')),
  source             TEXT NOT NULL CHECK (source IN ('embedded','full-decode','original')),
  width INTEGER NOT NULL, height INTEGER NOT NULL,  -- display-oriented dimensions
  bytes INTEGER NOT NULL, format TEXT NOT NULL DEFAULT 'webp',
  needs_full_decode  INTEGER NOT NULL DEFAULT 0,    -- §9.3 threshold miss
  generator_version  INTEGER NOT NULL,              -- bump → regeneration (§9.8)
  generated_at       TEXT NOT NULL,
  PRIMARY KEY (image_hash, kind)
);
```

No separate queue table: **the queue is the set of `pending` rows in `ingest_passes`** —
resumable and idempotent by construction (§10.4).

## 7. Watcher and reconciliation

### 7.1 Live watcher

`notify` crate, recursive watch per active root on an online volume. **Debounce**: per-path
500 ms quiescence; bursts coalesce to one evaluation; a file whose size is still changing
(mid-copy from a card reader) is re-checked every 2 s until size+mtime are stable across two
checks. Paired rename events are handled directly as relinks; unpaired removes/creates fall
through to §7.2, which handles moves identically — pairing is an optimization, not
correctness. Watcher overflow, or an unsupported backend (some network filesystems),
degrades that root to **polled mode**: a §7.3 scan every 10 minutes, noted in the debug panel.

### 7.2 Event handling per path (single algorithm)

For any stable path event (create/modify/rename-to):

1. `stat`. Missing → remove handling (step R). Excluded (§5.1) → ignore.
2. Look up the `active` path row at `(volume_id, rel_path)`:
   - **exists, size+mtime match** → touch `last_verified_at`; done.
   - **exists, size or mtime differ** → re-hash. Same hash → update size/mtime. New hash →
     in-place change protocol (§1.3).
   - **no row** → hash the file. Hash known → **relink**: insert active path row. Hash
     unknown → new image: `images` row + M1 pass enqueues, one transaction (§10.4).

R. **Remove**: mark the active row `stale` (`deleted`); if a create relinks the same hash
elsewhere within 10 s, flip the reason to `moved`. A move never re-ingests — that is the point
of content addressing.

### 7.3 Startup / scheduled reconciliation scan

Runs at launch for every root on an online volume, on every volume online-transition, and on
a 6-hour timer while running. Per root:

1. Load all `active` path rows into a map `rel_path → (path_id, size, mtime_ns)`.
2. Walk the filesystem (exclusions applied). Per file: known + size & mtime match (**2 s
   mtime tolerance on FAT/exFAT**, exact elsewhere) → mark seen, nothing beyond the `stat` —
   this fast path must cover ≥ 99% of a stable library; known + mismatch → re-hash → §7.2
   logic; unknown → hash → relink or new-image per §7.2.
3. Loaded rows not seen → `stale` (`deleted`); batch move-correlation: a new path whose
   hash matches an image that just lost a path is a move (`stale_reason = 'moved'`).
4. Batch-update `last_verified_at` for fast-path rows (one transaction per few thousand rows
   — per-row writes would dominate the scan).

**Re-hash is required exactly when**: (a) the path is new to the index, (b) size differs,
(c) mtime differs beyond fs tolerance, (d) an explicit verify command (debug panel) or a
sidecar hash mismatch (SIDECARS.md) demands it. Never otherwise — a 50k-file no-change scan
does zero hashing.

### 7.4 Relink invariant

Relink = insert/activate a path row for an existing hash. It never touches `images`,
events, previews, embeddings, or sidecar *content* (placement may react, per SIDECARS.md).
Annotations follow the pixels with no copying — nothing was ever keyed on path.

## 8. Image availability (derived, never stored)

Computed from path rows + volume state at query time (never a stored column that can rot):

| State | Definition | UI consequence (UI.md) |
|---|---|---|
| `available` | ≥ 1 active path on an online volume | normal display |
| `offline` | active paths exist, all on offline volumes | cached previews + disconnected badge |
| `missing` | no active paths (all stale) | cached previews + missing badge |

Search, the journal panel, retrieval, and export are **fully functional in all three
states**: previews are cached (§9), events live in the DB/sidecar set, the read path never
touches originals. Badge payload for non-available images: best path (§3.1, including
offline rows), volume `label` and `last_seen_at`, and `stale_since` for missing.

## 9. Preview pipeline

### 9.1 Format allowlist

Ingested extensions (case-insensitive): `jpg jpeg png tif tiff webp heic heif dng cr2 cr3
crw nef nrw arw raf orf ori rw2 pef srw x3f 3fr fff iiq mos mrw kdc dcr sr2 srf erf`. GIF,
BMP, PSD, and video are out of scope for v1 (recorded; PSD and video are likely first
additions).

### 9.2 Generated artifacts

Two artifacts per image, both **display-oriented** (§9.7) and **sRGB**:

| Artifact | Spec | Use |
|---|---|---|
| `thumb` | WebP, longest edge 512 px, quality 75 | grid |
| `display` | WebP, longest edge 2560 px, quality 87 | single-image view; stroke canvas substrate |

Never upscale: a source (or best embedded preview) smaller than the target edge yields a
native-size artifact. Strokes render over `display`; a full-resolution loupe is post-v1 (the
journal needs the proof, not the print).

### 9.3 RAW path: embedded previews first (M1 primary)

M1 extracts the **largest embedded JPEG preview** from the RAW container via rawler's
metadata-only parse — no demosaic, milliseconds per file. Expectations (guidance): CR2/CR3,
NEF, modern ARW, RW2, ORF, DNG, PEF generally embed full-resolution previews; older Sony ARW
~1616×1080; some RAF and compressed modes reduced.

**Acceptability threshold (decision): embedded preview longest edge ≥ 2048 px.** At or above:
it sources both artifacts, `source = 'embedded'`, done. Below: still generate both artifacts
from it (a small preview beats a placeholder), set `needs_full_decode = 1`, enqueue
`full-raw-decode`. No embedded preview at all: UI placeholder, `full-raw-decode` enqueued at
elevated backfill priority.

### 9.4 Full RAW decode (backfill pass, M1.5)

`full-raw-decode` (rawler; libraw FFI fallback deferred until a real format gap appears)
regenerates **both** artifacts for flagged images, sets `source = 'full-decode'`, clears the
flag. A backfill (§10.3), never an M1 ingest blocker.

### 9.5 Non-RAW handling

JPEG / PNG / TIFF / WebP: decode the original (`image` crate), generate both artifacts,
`source = 'original'`; multi-page TIFF uses page 0 only. **HEIC/HEIF (decision)**: ingested
in M1 — hashed, EXIF extracted — but preview generation is **deferred to the
`full-raw-decode` backfill**, which decodes HEIC via `libheif` (embedded HEIF thumbnails are
~320 px, below threshold); placeholder until then. Rationale: iPhone shooters are real, but
a native libheif dependency must not block the M1 spine; the pass architecture makes "later"
cheap.

### 9.6 EXIF subset (the exact fields)

Extracted at ingest (`kamadak-exif` for non-RAW; rawler metadata for RAW), stored on
`images`, read-only always — the app never writes file metadata: `DateTimeOriginal` +
`OffsetTimeOriginal` → `capture_ts` (UTC, offset kept in `capture_tz_offset`; fallback
`CreateDate`, else file mtime with `capture_ts` NULL); `Make`; `Model`; `LensModel`;
`FocalLength`; `ISOSpeedRatings`; `FNumber`; `ExposureTime`; `Orientation`; pixel
dimensions; `GPSLatitude`/`GPSLongitude` (nullable); `ColorSpace` + embedded ICC presence
(consumed by §9.7, not stored beyond what conversion needs).

### 9.7 Orientation & color contract (load-bearing)

- **EXIF orientation is applied at cache time; cached artifacts are always
  display-oriented.** Normative contract for CAPTURE/UI: *the pixel grid the UI displays and
  the normalized coordinate space strokes are recorded in are the same space — the
  display-oriented image.* The applied orientation (1–8) is stored on
  `images.exif_orientation` and recorded in each stroke event per the kernel, so a tool
  later rewriting orientation metadata is detectable rather than silently rotating marks out
  from under the user. A rotated portrait's preview and a stroke on it must agree everywhere,
  after every rebuild (§13).
- **sRGB at cache time**: embedded ICC profile → convert to sRGB during artifact generation
  (qcms-class CMS crate); no profile → assume sRGB; EXIF `ColorSpace = AdobeRGB` without ICC
  → convert from built-in AdobeRGB primaries. Wide-gamut display output (P3 tagging, monitor
  profiles) is a **known v1 limitation**, recorded here.

### 9.8 Cache layout, eviction, regeneration

- Location: `<app_data>/previews/<h[0..2]>/<h[2..4]>/<hash>-thumb.webp` / `<hash>-disp.webp`
  (two-level hex fan-out; ≤ ~1k files/dir at 50k images × 2). Metadata in `preview_artifacts`.
- Atomic writes: temp file in the same directory + rename; a crash mid-write leaves the old
  artifact or none, never a torn file, and the non-`done` pass row re-runs.
- **Eviction (decision): never, in v1.** 50k images ≈ 50k × (~40 KB + ~700 KB) ≈ 35–40 GB
  worst case — real but acceptable for this audience. Cache size is reported in settings and
  the debug panel; a "clear preview cache" command re-enqueues preview passes. LRU: post-v1.
- **Regeneration** only when: `generator_version` (compile-time constant covering encoder,
  sizes, color pipeline) is bumped → re-enqueue all preview passes at backfill priority; or
  `full-raw-decode` upgrades a flagged image; or the manual clear. Orientation or ICC changes
  MUST bump `generator_version` — stroke agreement depends on it.

## 10. Ingest as versioned passes

Ingest is a set of independent, versioned **passes** over the image population, each
recorded per image in `ingest_passes`. "What happens when models improve" = a new
`pass_version` (or `model_id`) enqueued as a backfill; old rows remain as history.

### 10.1 Pass registry

| pass_name | Milestone | model_id | Work |
|---|---|---|---|
| `hash` | M1 | — | identity; its row is written `done` atomically with the `images` insert (exists for uniform progress reporting) |
| `exif` | M1 | — | §9.6 subset → `images` columns |
| `preview` | M1 | — | §9.2–9.5 artifacts (embedded/original route) |
| `full-raw-decode` | M1.5 | — | §9.4 backfill for flagged RAW + HEIC |
| `image-embedding` | M3 | yes | OpenCLIP vectors per RETRIEVAL.md; vectors reference images/events, never event rows or sidecars |
| `caption` | M3 | yes | VLM caption, retrieval fuel only per kernel |

`pass_version` starts at 1 per pass. `skipped` marks structurally inapplicable work (e.g.
`full-raw-decode` on a plain JPEG) so progress math stays honest.

### 10.2 Pass state machine

```
(absent) ──enqueue──▶ pending ──worker──▶ running ──success──▶ done
              ▲                              │
              └──── retry (§10.5) ── error ◀─┘ failure (attempts exhausted: stays error)
```

`skipped` is written directly at enqueue time for inapplicable work. On **startup, every
`running` row reverts to `pending`** — no leases, no heartbeats; a single process owns the
DB. With idempotent work units (§10.4) this is the entire crash-recovery story.

### 10.3 Priority and concurrency

Priority (lower = sooner), stored per row: **P0** M1 passes for newly discovered files
(live watcher — the user is probably looking at that folder); **P1** M1 passes from
reconciliation/initial scans; **P2** backfills (`full-raw-decode`, regeneration); **P3**
GPU/model backfills (`image-embedding`, `caption`).

Dequeue order: `(priority, enqueued_at)`; within the hash pass, ascending file size (§1.2).
Concurrency: one dispatcher (tokio task) feeding worker pools — **CPU pool**
`min(physical_cores, 8)` for hash/exif/preview; **decode pool** `max(2, physical_cores / 2)`
for `full-raw-decode` (demosaic is memory-hungry); **GPU passes** concurrency 1 via the
connector seam, **pausing immediately when a live session needs ASR/LLM/VRAM** (the
dispatcher subscribes to RUNTIME.md's yield signal and stops dispatching P3 until released);
a **single DB writer** task receives results over a channel — workers never touch SQLite.

### 10.4 Resumability, idempotency, interrupt safety

The queue is the `pending` rows; restart resumes it by construction. Every pass is
idempotent: `preview` overwrites artifacts atomically (§9.8), `exif` overwrites columns,
`hash` on known bytes is a no-op upsert. **kill -9 mid-run resumes with no duplicates and no
misses**; worst case, one work unit is redone. New-image insertion (`images` row + `hash`
done-row + sibling enqueues) is one transaction — no window where an image lacks its queue
entries.

### 10.5 Errors and retry

Failure records `error` (message + stable error-code prefix) and increments `attempts`.
Transient errors (I/O, volume offline mid-read, model process restart): re-`pending` with
backoff 1 min then 10 min; after 3 attempts → `error`. `error` rows auto-retry only on app
restart and on the 6-hour reconciliation tick (`attempts` persists; after 10 lifetime
attempts, manual retry only). Permanent errors (corrupt file, unsupported variant) stay
visible in the debug panel with counts. A volume going offline mid-pass is an ordinary
transient — the retry waits out the remount.

### 10.6 Progress reporting

Core exposes per-pass counters — `(pass_name, pass_version) → {pending, running, done,
error, skipped}` — via query plus a push stream (debounced to 4 Hz) for the dev-build debug
panel: queue depth, files in flight, throughput (files/s; MB/s for hash), ETA, error list.
Release builds keep the core API (feeding one quiet "indexing N remaining" line in
settings); the panel itself is compile-time stripped per the kernel.

## 11. Offline operation summary

Unplugging an archive drive must cost nothing but pixels-on-demand: volume → `offline`;
availability → `offline`; views serve cached artifacts with badge data; journal capture
against offline images is **fully allowed** (events target hashes; sidecar writes queue per
SIDECARS.md); search is unaffected — indexes never touch originals; pending passes
fail-transient and wait; on remount at any mount point: identity match (§4.1) → online →
watcher up → reconciliation scan → queued sidecar flush.

## 12. Performance budgets

Reference: 8 physical cores, 32 GB RAM, NVMe ≥ 2 GB/s, internal drive; 50k images, ~1.5 TB (mixed RAW+JPEG). **The ≤ 90-min first-ingest budget applies to internal NVMe explicitly**; slow volumes get §12.1, not a miracle.

| Budget | Target |
|---|---|
| First-run M1 ingest (hash + exif + preview, embedded route) — internal NVMe | ≤ 90 min end-to-end; hash phase ≥ 1 GB/s sustained (~25 min for 1.5 TB) |
| Grid usability during first run | first 1,000 thumbs within 60 s of start (ascending-size ordering makes this achievable) |
| Watcher latency, steady state | new stable file → thumb in grid ≤ 5 s (p95) |
| Startup reconciliation, 50k files, no changes | ≤ 10 s (stat-only fast path, zero hashing) |
| Reconciliation after a 1k-file move | ≤ 30 s incl. re-hash of moved files |
| Grid scroll | previews served from disk cache, no decode-on-scroll; supports UI.md's 60 fps virtualized grid |

### 12.1 Slow volumes (normative)

On volumes measured below ~200 MB/s sustained read, the budget **scales with throughput** —
a 120 MB/s spinning-disk USB archive is ≈ 3.5 h/TB of hashing, full stop — and previews
trail hashing visibly (the hash queue saturates the disk; the embedded-preview pass reads
behind it). The UI requirement on such volumes is **honest progress framing**, not speed: the
ingest hairline plus debug-panel detail (MB/s, ETA derived from measured throughput), never a
fast-path identity tier that papers over the wait.

**Considered and rejected — provisional quick-hash identity tier** (the digiKam pattern:
MD5 of first+last 100 KB + size, [digikam-users](https://mail.kde.org/pipermail/digikam-users/2013-June/017748.html);
the czkawka pattern: size → 2 KB prehash → full hash, [workflow](https://deepwiki.com/qarmin/czkawka/4.1-tool-types-and-workflow)):
a provisional identity that later upgrades to the full BLAKE3 hash would make slow first
ingests feel fast, but two-state identity is unacceptable complexity for the journal's
integrity model — every event target, sidecar binding, and cache key would need a "which
identity?" answer, and digiKam's tier has known in-place-edit blind spots. One identity, one
hash, honest progress.

## 13. Acceptance criteria

Each is a test in `photoproof-core/tests/` (headless, real temp filesystems) unless marked manual:

1. **Relink, running**: move/rename files under a watched root while running → path rows
   updated, same `image_hash`, journal intact, zero new `images` rows, zero extra hashing.
2. **Relink, stopped**: same mutation with the app stopped → startup reconciliation produces
   the identical end state as (1).
3. **Interrupt/resume**: `kill -9` during a 10k-file ingest at random points (incl.
   mid-transaction, mid-preview-write) → restart yields exactly one `images` row per unique
   hash, every pass `done`, no torn cache files, no misses (vs. an independent walk).
4. **Duplicates**: 500 byte-identical copies → one `images` row, N active path rows;
   annotating via any path lands in the one journal; best-path follows §3.1 deterministically.
5. **In-place overwrite**: re-export over a JPEG → new image + passes; old image keeps its
   journal; old path `stale/superseded`; dormant-prior-version lookup finds the old hash.
6. **Volume remount, new mount point**: ingest at mount A, remount at B (marker/platform id
   unchanged) → `online`, `mount_point = B`, all paths resolve, no relink storm, no re-hash.
7. **Marker precedence**: platform id changed (new enclosure), marker intact → same volume
   identity; warning logged.
8. **Offline browsing** (manual + automated state check): unplug a volume → images report
   `offline`, previews render, search returns them, typed notes attach, sidecar writes queue;
   remount → flush with no duplicate events (merge=union).
9. **Root removal**: remove → events untouched, paths `stale/root-removed`, availability
   `missing`; re-register → full recovery via fast path, zero re-hashing of unchanged files.
10. **Orientation correctness**: an EXIF-orientation-6 portrait → cached artifacts upright;
    a stroke drawn at the subject's eye (normalized display-oriented coords) re-renders at
    the eye after preview regeneration and after rebuild-from-sidecars. Repeat for 3 and 8.
11. **Threshold routing**: a RAW with a 1616×1080 embedded preview → small artifacts,
    `needs_full_decode = 1`, queued `full-raw-decode`; after backfill, full-quality, flag clear.
12. **Reconciliation budget**: 50k-path fixture, no changes → ≤ 10 s, zero hash invocations
    (asserted via instrumentation).
13. **Exclusions**: sidecars, `.photoproof-volume`, hidden dirs, and listed cache dirs never
    produce `images` rows.

## 14. Recorded non-features (v1)

RAW+JPEG stacking and the pairing heuristic (§2.1); journal migration across an in-place
overwrite (§1.3), manual or automatic; user-editable exclusion patterns; symlink following;
preview cache eviction; full-resolution loupe decode; wide-gamut display output (§9.7);
GIF/BMP/PSD/video ingestion; libraw FFI fallback (until rawler hits a real format gap);
provisional quick-hash identity tier (rejected outright, not deferred — §12.1).
