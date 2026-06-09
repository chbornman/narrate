# Research: Annotation Storage & FTS (SQLite event store)

Validation of spec/EVENTS.md, spec/SIDECARS.md (write path), and
spec/RETRIEVAL.md §1 against 2024–2026 practice. Scale assumptions: 50k
images, 1–2M events over 20 years (~500k–1M remark roots), text corpus
≤ 100 MB, single local user, human-paced writes.

## Verdicts

**Append-only `annotation_events` + `event_targets` join, fold in app code — VALIDATED.**
The canonical SQL event-store shape: events table + `(stream_id, event_id)`
index for "all events for entity X in order" ([SQLite Forum: Event Sourcing with SQLite](https://www.sqliteforum.com/p/event-sourcing-with-sqlite),
[sql-event-store reference design](https://github.com/mattbishop/sql-event-store)).
`idx_targets_image(image_hash, event_id)` on a WITHOUT ROWID table is a pure
covering range scan ([sqlite.org WITHOUT ROWID](https://sqlite.org/withoutrowid.html)).
Practitioners report replaying 10,000 events for one aggregate in ~50 ms over
a network DB ([patchlevel: The Performance Factor in Event Sourcing](https://patchlevel.de/blog/the-performance-factor-in-event-sourcing));
our typical 5–200 events/image is two orders of magnitude below that.

**ULID TEXT primary key — VALIDATED; monotonicity matters.** A June 2026
10M-row benchmark shows random (UUID4) text keys degrade inserts 7–16× from
B-tree rebalancing, while time-ordered keys sustain ~1.3M inserts/sec, near
integer-key speed ([andersmurphy: The perils of UUID primary keys in SQLite](https://andersmurphy.com/2026/06/05/the-perils-of-uuid-primary-keys-in-sqlite.html)).
Monotonic ULIDs are in the good category.

**FTS5 one-row-per-chain-root, app-managed transactional maintenance (no
triggers) — VALIDATED**, and the right call: the SQLite forum documents FTS5
desync/corruption from subtly wrong triggers ([Corrupt FTS5 table after declaring triggers](https://sqlite.org/forum/info/da59bf102d7a7951740bd01c4942b1119512a86bfa1b11d4f762056c8eb7fc4e),
[FTS5 rows not searchable](https://sqlite.org/forum/info/db41a553368f4d4a));
the docs state external-content consistency is the user's problem
([sqlite.org/fts5.html](https://sqlite.org/fts5.html)). Fold rules are
unexpressible in triggers anyway. **However: EVENTS §5.4 (plain content-ful
`event_fts` + `fts_map`, prefix '2 3') and RETRIEVAL §1.1 (external-content
`events_fts`, prefix '2 3 4', 'rebuild' fast path) specified two different
constructions — resolved in favor of EVENTS' plain content-ful table** (the
indexed unit, folded text, exists nowhere as a real column; snippet() needs
stored text; one construction, one name).

**unicode61 + prefix indexes over trigram — VALIDATED.** Trigram matches
nothing under 3 chars ([sqlite.org/fts5.html](https://sqlite.org/fts5.html)),
breaking ≥2-char search-as-you-type; has non-Latin issues
([chroma #1073](https://github.com/chroma-core/chroma/issues/1073)); larger
index; no real typo tolerance. detail=full required for snippet(); index ≈
45% of text → ~50 MB worst case. Trivial.

**foreign_keys=OFF — VALIDATED** (merge semantics genuinely require it; OFF
is SQLite's default; event-store practice agrees —
[patchlevel](https://patchlevel.de/blog/the-performance-factor-in-event-sourcing)).
**STRICT + CHECKs — VALIDATED** (per-row CPU, immeasurable at human rates).
**secure_delete=ON always — VALIDATED** ([antonz.org: Secure delete in SQLite](https://antonz.org/sqlite-secure-delete/));
keeping it always-on is correct (pages freed by ordinary churn before a
future redaction could retain plaintext); do NOT downgrade to
`secure_delete=fast` (skips freelist scrubbing).

**WAL + synchronous=NORMAL — VALIDATED** — the universal recommendation
([phiresky's tuning guide](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/),
[Database School: recommended pragmas](https://databaseschool.com/articles/sqlite-recommended-pragmas)).
Real desktop risk: long-lived read transactions block checkpointing → WAL
grows unbounded → all reads slow ([sqlite.org/wal.html](https://sqlite.org/wal.html)).

**image_ratings derived table — VALIDATED and necessary**; the rating filter
chip is an indexed WHERE instead of a 50k-image fold. The same logic
justifies exactly one more flags table (image_journal_stats) — and no
general materialized current-state projection.

## Hot paths

- **(a) Journal panel** (5–200 typical, 5k worst): FAST if N+1 is avoided —
  covering range scan + warm B-tree probes; 200 probes ≪ 1 ms, 5k =
  single-digit ms. The only slow mechanism is per-event retracted()/chain()
  queries.
- **(b) Search-as-you-type, 1M events**: FAST; two traps — snippet() must be
  evaluated post-LIMIT ([sql.js-httpvfs #10](https://github.com/phiresky/sql.js-httpvfs/issues/10)),
  and FTS virtual-table joins without ANALYZE have a documented 170 s → 0.26 s
  (650×) planner failure ([sqlite.org forum: JOINs with FTS5 virtual tables are very slow](https://sqlite.org/forum/info/509bdbe534f58f20)).
- **(c) Grid badges, 20k images**: FAST at the right granularity — one
  set-at-once query or the stats table; never per-image probes.
- **(d) Folder rating fold**: FAST — already materialized (image_ratings).
- **(e) Rebuild, 50k sidecars**: minutes, dominated by file I/O + JSON
  parsing, not SQLite (batched inserts sustain high-hundreds-of-k rows/sec
  for time-ordered text keys — [andersmurphy](https://andersmurphy.com/2026/06/05/the-perils-of-uuid-primary-keys-in-sqlite.html)).

## Real-world shapes

- **Lightroom**: append-only develop history = 50%+ of a 4 GB catalog; pain
  is bloat, not query algorithmics ([Points in Focus](https://www.pointsinfocus.com/learning/digital-darkroom/the-lightroom-catalog-and-develop-history-states/));
  ours is human words, 10–100× smaller per image.
- **digiKam**: SQLite default; their own docs say WAL-mode SQLite on SSD is
  fine past 100k items ([digiKam manual](https://docs.digikam.org/en/setup_application/database_settings.html)).
- **Joplin**: the FTS path is the fast one; bloat pain came from sync
  bookkeeping, which our no-machine-events rule precludes
  ([Joplin search docs](https://joplinapp.org/help/apps/search/)).

## Amendments (all applied to specs)

1. **S1** One FTS construction: EVENTS' plain content-ful table wins;
   RETRIEVAL §1.1/§4/§11 updated (row unit = live remark chain root via
   fts_map; 'rebuild' path replaced by wipe+refold+optimize).
2. **S2** Journal fold pinned to ≤ 3 batched queries (targets scan → events
   IN → meta-closure to fixpoint); per-event queries forbidden.
3. **S3** M1 search SQL: `WITH hits AS MATERIALIZED (… MATCH … ORDER BY rank
   LIMIT 500)` then join fts_map/event_targets/filters; snippet bounded to
   the LIMITed set.
4. **S4** `image_journal_stats(image_hash PK, event_count, has_strokes,
   last_ts)` derived table — grid badges, HasStrokes chip, RRF tie-break.
5. **S5** Pragmas: cache_size -65536 (64 MiB), mmap_size 256 MiB, temp_store
   MEMORY, busy_timeout 5000; `PRAGMA optimize` on close; ANALYZE after
   rebuild/large merge; checkpoint(TRUNCATE) at idle/shutdown; no held-open
   read statements; one writer + read pool ([tokio-rusqlite](https://docs.rs/tokio-rusqlite/latest/tokio_rusqlite/)).
6. **S6** prefix='2 3' (drop 4 — each prefix length is a full extra posting
   index); scheduled FTS `optimize` after rebuild/large merges.
7. **S7** idx_events_target → (target_event, kind) partial; drop
   idx_events_kind (low selectivity); partial index for redactions; rule: JSON
   filtering via virtual generated columns, never json_extract in hot WHEREs
   ([antonz.org](https://antonz.org/json-virtual-columns/)).
8. **S8** Rebuild discipline: union sorted ascending by id (right-edge
   appends), ~10k-event transactions, FTS/derived in a single pass after the
   union, finish with ANALYZE + FTS optimize + checkpoint(TRUNCATE).

## Non-issues

DB size at 1–2M rows (likely 200–500 MB; hundreds-of-GB SQLite DBs serve
40 ms queries when indexed — [example](https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0)) ·
Lightroom-style bloat · fold-on-read at the 5k worst case · TEXT ULID keys ·
secure_delete/STRICT overhead at human rates · FTS index size · rebuild time ·
digiKam MySQL folklore · WAL itself (hazards neutralized by S5).
