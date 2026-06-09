# RETRIEVAL.md — Indexing, Search, Ranking, Context Assembly, Derived Views, Projects

Status: Draft 1, June 2026. Closes SPEC-GAPS E1–E4, plus the read-scope context
model and the project/intent store.

Boundaries (normative): `EVENTS.md` owns the event log, fold rules, and the
`event_fts`/`fts_map` DDL and construction (the single normative FTS design —
EVENTS §5.4); this spec owns **what** gets indexed and when, the
`vectors` table in full, query processing, ranking, the result data contract,
context assembly, derived views, and the project store. `RUNTIME.md` owns model
serving; this spec consumes the `Embedder` and `LanguageModel` traits.
`UI.md` owns presentation; it renders the result contract in §5.4 verbatim.
`LIBRARY.md` owns backfill-pass mechanics; this spec defines what the embedding
and reindex passes must produce.

Retrieval identity (the product thesis, restated as a requirement): **search
matches the photographer's own words about their work.** Annotation-text
embeddings are the primary signal; image semantics are a fallback; every
result must carry a human-readable explanation of why it matched (§6).

---

## 1. Index recipes

### 1.1 FTS5 — what is indexed

Two FTS5 virtual tables. `event_fts` is owned in full — name, DDL,
construction — by EVENTS §5.4, **the single normative FTS design**; this spec
defers to it and adds only the content rules. `summaries_fts` is owned here.

| Table | Row unit | Content | Keying |
|---|---|---|---|
| `event_fts` | one row per **live remark chain root** | the root's **effective (folded) text**, single `body` column | `fts_map(root_event_id, fts_rowid)` (EVENTS §5.4) |
| `summaries_fts` | one row per derived summary | summary text (§9) | `text`, `summary_id UNINDEXED` |

`event_fts` is a **plain content-ful** table: the folded text exists nowhere
as a real column elsewhere, and `snippet()` needs stored text — so there is no
external-content option and no `event_id UNINDEXED` column; the id mapping
lives in `fts_map`.

**Indexable events.** A remark root has a row in `event_fts` iff all of:

- `kind` is `remark` (typed or voice). `rating` events are structured data,
  never FTS. `stroke` events have no text of their own (they become searchable
  through a linked utterance's text — EVENTS.md linking rules). `revision`
  events are not indexed as themselves; they change the *target's* folded text.
- The event is not retracted (tombstoned) and not redacted.
- The text indexed is the **folded text** per EVENTS.md: latest revision
  replaces the original; the original never appears in any index.

Session-level remarks (zero image targets) **are** indexed; they surface in
the separate `session_hits` list of the result contract (§5.4), never attributed
to an image.

**Why a separate summaries table** (decision): summaries are derived rows,
not events; a second column in `event_fts` would force a fake event row per
summary and entangle event retraction logic with derived-data lifecycle. A
separate table is its own down-weighted ranked list in fusion (§5.3). A
summary hit may *rank* an image but is never shown as provenance text (§6).

**Tokenizer (normative):**

```sql
tokenize = "unicode61 remove_diacritics 2"
prefix   = '2 3'
-- detail=full (default): required for snippet() and phrase queries
```

`unicode61` with `remove_diacritics 2` so "Sebastião" matches "sebastiao".
No custom `tokenchars`: "f/2.8" tokenizes as `f`,`2`,`8` — acceptable, since
exposure data is queried through structured filters, not prose. Prefix indexes
at 2/3 chars make search-as-you-type prefix matching (§4) index-backed; no
4-char index — each prefix length is a full extra posting index, and ≥4-char
prefixes are served acceptably by the main term index.

**Index maintenance** (all inside the same transaction as the triggering event
append/scrub; `event_fts` mechanics are EVENTS §6.3's — root rows keyed
through `fts_map`):

| Trigger | `event_fts` action (EVENTS §6.3) | `vectors` action (§1.2) |
|---|---|---|
| New indexable remark R | insert R's root row (folded text) | enqueue embed (M3+) |
| Revision in root R's chain | update R's row to the new effective text | mark R's chunk rows `deleted=1`; enqueue re-embed |
| Retraction of root R | delete R's row | mark R's chunk rows `deleted=1` |
| Retraction of a revision of R | recompute R's effective text (update, or delete if R no longer live) | mark R's chunk rows `deleted=1`; enqueue re-embed |
| Redaction of chain root R | delete R's row (same transaction as content scrub) | mark deleted **and zero the flat-file rows** (§1.3); delete the chain's sentiment rows; delete derived summaries whose `inputs` include any chain member (§9.5) |
| Un-retract (if EVENTS.md permits) | reinsert | re-embed |

Acceptance: a retracted or redacted event is absent from all search results
the moment the triggering call returns (§13).

### 1.2 Vectors — DDL (normative, owned by this spec)

```sql
CREATE TABLE vectors (
  id           INTEGER PRIMARY KEY,                -- rowid
  vec_kind     TEXT    NOT NULL
               CHECK (vec_kind IN ('annotation_chunk','image_summary','image_clip')),
  model_id     TEXT    NOT NULL,                   -- Embedder::model_id()
  dims         INTEGER NOT NULL,                   -- Embedder::dimensions()
  event_id     TEXT,                               -- ULID; set iff annotation_chunk
  image_hash   TEXT,                               -- blake3 hex; set iff image_summary/image_clip
  chunk_index  INTEGER NOT NULL DEFAULT 0,         -- 0..n within one event's text
  char_start   INTEGER,                            -- char offsets (Unicode scalars) into the
  char_end     INTEGER,                            --   folded text, for quote extraction
  file_row     INTEGER NOT NULL,                   -- row index in the flat file for (vec_kind, model_id)
  inputs_hash  TEXT    NOT NULL,                   -- blake3 of embedded text (or preview bytes) + the
                                                   --   context-prefix scheme version (§2) + instruct
                                                   --   template version (§3); staleness check
  created_ts   TEXT    NOT NULL,                   -- RFC 3339 UTC
  deleted      INTEGER NOT NULL DEFAULT 0,         -- awaiting compaction; search skips
  CHECK ((event_id IS NULL) <> (image_hash IS NULL))
);
CREATE UNIQUE INDEX vectors_chunk
  ON vectors(vec_kind, model_id, event_id, chunk_index) WHERE event_id IS NOT NULL;
CREATE UNIQUE INDEX vectors_image
  ON vectors(vec_kind, model_id, image_hash) WHERE image_hash IS NOT NULL;
CREATE UNIQUE INDEX vectors_row ON vectors(vec_kind, model_id, file_row);
```

`vec_kind` semantics:

- `annotation_chunk` — one chunk of one event's folded text (§2), keyed by
  `event_id`; image association resolves through `event_targets` at query
  time, so a multi-image event's chunk can rank all N targets (correct — the
  remark is about all of them).
- `image_summary` — embedding of the per-image rolling summary (§9.1); one
  live row per (image, model).
- `image_clip` — image embedding of the preview pixels (CLIP image tower);
  one live row per (image, model).

**Query history is NOT stored in v1** — no `query` vec_kind, no search-log
table. Deliberate privacy bias: the journal records what the photographer says
about their work, not what they were looking for. Recorded as a future opt-in.

Embeddings never appear in event rows or sidecars (kernel). `vectors` rows are
derived, disposable, and rebuildable from the event log + previews.

### 1.3 Flat-file vector storage

One file per `(vec_kind, model_id)` pair under app data:

```
appdata/vectors/{vec_kind}.{model_id_sanitized}.ppvec
```

**Format (PPVEC v2):** header — magic `PPVEC\x02`, `dims: u32 LE`,
`dtype: u8` (`0` = f32, `1` = int8), reserved padding to alignment — followed,
for int8 files, by the quantization parameters: **per-dimension** `scale:
f32[dims]` and `offset: f32[dims]`, frozen at file creation from a calibration
sample of the space; every stored vector and every query is quantized with
these same parameters. Then row-major rows of `dims` entries each. Vectors are
**L2-normalized before quantization**, so cosine similarity = (de-scaled) dot
product.

**Stored encoding default: int8 scalar quantization at MRL-truncated 512
dims.** f32 exists only transiently at embed time; nothing f32 touches disk.
Cost: ~1–2% retrieval quality for an **8× reduction** in scan bytes and
storage versus f32-1024d
([HF embedding-quantization](https://huggingface.co/blog/embedding-quantization));
Matryoshka truncation 1024→512 is independently ~1–2%. The runtime spike
(RUNTIME.md) decides 512 vs 1024 with a small eval on the §12 golden set.

- **Read:** memory-mapped; brute-force top-k over all non-deleted rows.
  **Multithreaded SIMD scanning is a requirement, not an optimization.** The
  honest latency model is bandwidth, not "single-digit ms": `scan_time ≈
  scan_bytes / ~30 GB/s` effective desktop memory bandwidth
  ([hardware analysis](https://apxml.com/courses/advanced-vector-search-llms/chapter-2-optimizing-vector-search-performance/hardware-acceleration-considerations)).
  At a p95 < 50 ms budget:

  | Encoding | Bytes/vec | Vectors @ 50 ms |
  |---|---|---|
  | f32 1024d | 4 KB | ≈ 375k |
  | **int8 512d (default)** | 512 B | ≈ 3M |

  **Prewarm:** on app start (or first search), sequentially touch the active
  spaces' files to pull them into the OS page cache — otherwise the first
  scan runs at disk speed, not memory speed. If prewarm is skipped, the
  cold-start hit must be documented and the first query excluded from the
  latency budget.
- **Append:** at file end; write + fsync the file first, then commit the
  SQLite row (an orphaned file row is unreachable garbage, cleaned by
  compaction; the reverse order would be a dangling pointer).
- **Delete:** logical — `deleted=1` in SQLite — **and for redaction the file
  row is additionally zeroed in place** (the scrub must be physical, per
  EVENTS.md). Redaction zeroing semantics are unchanged by the int8 encoding:
  the stored int8 row bytes are zeroed.
- **Compact:** when deleted rows exceed 20% of a file or 10,000 (whichever
  first): rewrite dropping dead rows, remap `file_row` in one transaction.
  Runs in the background-pass scheduler (LIBRARY.md).
- **Scale escape hatch:** all access goes through `VectorStore`. The swap
  trigger is **bytes scanned per space > ~1.5 GB** (the 50 ms budget at
  ~30 GB/s — ≈ 3M int8-512d vectors), not a row count; past it, swap in
  usearch (or sqlite-vec once stable) behind the same trait; SQLite metadata
  rows are unchanged. Not built in v1.

```rust
pub trait VectorStore {
    fn upsert(&self, key: VecKey, v: &Embedding) -> Result<()>;
    fn search(&self, query: &Embedding, space: VecSpace, k: usize) -> Result<Vec<VecHit>>;
    fn mark_deleted(&self, key: VecKey) -> Result<()>;
    fn scrub(&self, key: VecKey) -> Result<()>;          // physical zero (redaction)
    fn compact(&self, space: VecSpace) -> Result<()>;
}
pub struct VecSpace { pub vec_kind: VecKind, pub model_id: String }
pub struct VecHit   { pub vector_id: i64, pub score: f32 } // joins back to `vectors`
```

(Refines the SCOPE.md trait sketch, which keyed `upsert` by `ContentHash`
only; annotation chunks are keyed by event. SCOPE.md states signatures evolve.)

## 2. Chunking

Per text-bearing event, over the **folded** text:

- ≤ 512 tokens → exactly one chunk, `chunk_index=0`, offsets spanning the
  whole text. **The overwhelmingly common case** — most utterances are a
  sentence or two.
- Longer monologues → ~512-token chunks with **64-token overlap**; boundaries
  snap backward to the nearest sentence end within the window's final 64
  tokens when one exists, else hard-split.
- Token counting: the embedder's tokenizer when the connector exposes one,
  else `tokens ≈ whitespace_words × 1.3`. 512 is a target, not a contract.
- Each chunk records `char_start`/`char_end` so provenance can quote the
  exact matched span (§6).
- A revision re-folds the text → all chunks invalidated and re-embedded;
  offsets always refer to the *current* folded text.
- **Tiny-chunk context prefix (normative):** chunks under ~2 sentences are
  embedded with a deterministic metadata prefix prepended **at embed time
  only** — capture/annotation date, folder name, and active project name if
  any (e.g. `[2026-01-14 · 2026/iceland · Quiet Hours] `). One-sentence texts
  benefit measurably from added context at embed time
  ([One Word Is Not Enough](https://arxiv.org/html/2512.06744)). The prefix
  never enters FTS, the stored folded text, or provenance: quotes still come
  from the **bare** folded text via `char_start`/`char_end`. The prefix scheme
  version enters `inputs_hash` (§1.2). The prefix MUST be deterministic — no
  generated text — so rebuild byte-equality (§13.8) is preserved.

## 3. What is embedded, by pass

Embedding is a versioned backfill pass (LIBRARY.md mechanics), M3:

1. `annotation_chunk`: every indexable event (§1.1 rules), chunked per §2,
   via `Embedder::embed_text`.
2. `image_summary`: each per-image rolling summary on (re)generation (§9.1).
3. `image_clip`: each image's cached preview via `Embedder::embed_image`.

The spaces split across **two embedders** (RUNTIME §3 owns serving): the
**text embedder** — a small dedicated text-embedding model — produces
`annotation_chunk` and `image_summary` vectors via `Embedder::embed_text`
(CLIP-class text towers cap at 77 tokens and are trained for image–text
alignment, not text–text similarity; they cannot carry the product's primary
signal). The **CLIP embedder** (OpenCLIP ViT-H-14/DFN5B-class preset) produces
`image_clip` vectors via `Embedder::embed_image`, and embeds short *queries*
with its text tower for S4 visual matching. Each space records its own
`model_id` as always.

**Instruction prefixes (normative).** The text embedder
(Qwen3-Embedding-class) is instruction-aware and the two sides are
asymmetric: **documents are embedded bare** (no prefix beyond §2's tiny-chunk
rule — the Qwen3 convention), **queries are embedded with the instruct
template**, literally:

```
Instruct: Given a photographer's search, retrieve their journal notes about matching images
Query: {q}
```

Skipping the query instruction costs **1–5%** retrieval quality per the model
card ([Qwen3-Embedding-0.6B](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)).
The prefix template version is an `inputs_hash` input (§1.2), so a template
change invalidates and re-embeds rather than silently mixing recipes. (CLIP
queries take the bare text — no instruct template on the CLIP text tower.)

## 4. M1 search — FTS5 + structured filters (ships first; specified fully)

M1 search is the v1 product: no models, no vectors, no parse LLM.

**Input:** a query string plus zero or more UI filter chips (date range,
camera, lens, folder/root, rating, has-strokes, source, online/offline).
Chips construct `Filter` values (§5.1) directly — the same AST the M3 parser
emits, so M1 filter execution *is* the M3 code path. The has-strokes chip
(`Filter::HasStrokes`) compiles to an indexed WHERE on
`image_journal_stats.has_strokes` (EVENTS §5.3) — never a stroke-event fold
at query time; the rating chip likewise reads `image_ratings`.

**Query construction:** split on whitespace; drop empty tokens; escape each
token by doubling internal `"`; all tokens but the last become quoted exact
terms, the last becomes a prefix term:

```
input:  melanch         → MATCH '"melanch"*'
input:  fog barn        → MATCH '"fog" "barn"*'
```

Implicit AND (FTS5 default). No user-facing boolean syntax in v1; `OR`,
`NEAR`, `-` are treated as plain tokens (quoting neutralizes them).

**Execution (one SQL statement, materialize-first, target <100 ms end to end):**

```sql
WITH hits AS MATERIALIZED (
  SELECT f.rowid AS fts_rowid,
         bm25(event_fts) AS s,
         snippet(event_fts, 0, '⟦', '⟧', '…', 12) AS snip
  FROM event_fts f
  WHERE event_fts MATCH :q
  ORDER BY s
  LIMIT 500
)
SELECT m.root_event_id, t.image_hash, h.s, h.snip
FROM hits h
JOIN fts_map m       ON m.fts_rowid = h.fts_rowid
JOIN event_targets t ON t.event_id  = m.root_event_id
WHERE <filter WHERE clauses, joined through event_targets / images / paths>;
```

The shape is normative, not stylistic:

- The MATERIALIZED CTE resolves the MATCH **before** any join — joining an
  FTS5 virtual table directly has a documented planner failure mode of
  170 s → 0.26 s (**650×**) depending on join order
  ([sqlite.org forum](https://sqlite.org/forum/info/509bdbe534f58f20)).
- `snippet()` is evaluated only over the LIMITed hit set, never per candidate
  row pre-LIMIT ([sql.js-httpvfs #10](https://github.com/phiresky/sql.js-httpvfs/issues/10)).
- It matches EVENTS' schema: `event_fts(body)` keyed through
  `fts_map(root_event_id, fts_rowid)` — there is no `event_id` column on the
  FTS table to join on.
- The outer joins still depend on fresh statistics: `ANALYZE` after rebuild
  and large merges (EVENTS §5.1) is a correctness-of-performance dependency
  of this statement.

Filters are **hard constraints** (WHERE), never ranking inputs. Image-scoped
filters (camera, date-captured, rating, folder) apply to the images joined
through `event_targets`; an event targeting 3 images can survive for one
target and be filtered for another.

**Grouping:** hits group by image; an image's score is its best (lowest)
bm25 among its hits; images order by that score. Each image carries the
snippet of its best-matching event. Session-level hits go to `session_hits`.
Empty query + filters = filter-only browse: results ordered by capture date
descending, provenance `FilterOnly`.

**Search-as-you-type:** re-query on every keystroke once the string is ≥ 2
chars, with a 100 ms debounce; in-flight queries cancelled on new input
(`sqlite3_interrupt`). Budget: **p95 < 100 ms** from keystroke (post-debounce)
to result rows delivered to the UI, on the reference 50k-image library. The
prefix indexes (§1.1) exist for this.

**Highlighting:** FTS5 `snippet()` with the sentinel delimiters `⟦`/`⟧`
(above), mapped to `<mark>` by the UI; never raw HTML through IPC.

## 5. M3 query pipeline

Four stages: parse → candidate generation → fusion → results. If the typed
query is *only* filter chips + keywords (no NL parse requested, or models
unavailable), stages run with an empty/fallback parse — M1 behavior is the
degenerate case of this pipeline, not a separate system.

### 5.1 Stage 1 — NL parse: filter AST + semantic remainder

The local LLM (Gemma 4 E4B-class via the llama.cpp seam; RUNTIME.md) parses
the query into typed filters and a semantic remainder.

**The AST (normative Rust, lives in `photoproof-core::search`):**

```rust
pub struct ParsedQuery {
    pub filters:  Vec<Filter>,        // hard WHERE constraints
    pub semantic: Option<String>,     // remainder for embedding search
    pub keywords: Option<String>,     // remainder for FTS; not in the LLM JSON — set = semantic post-parse
    pub visual:   bool,               // query asks about image content, not words about it
    pub dropped:  Vec<DroppedClause>, // validation rejects, for the debug panel
    pub fallback: bool,               // true if parse failed/timed out (§5.1 fallback)
}

pub enum Filter {
    Date      { field: DateField, range: DateRange },
    Camera    (StringMatch),
    Lens      (StringMatch),
    Folder    (PathMatch),            // subtree of a watched root
    Root      (String),               // watched-root name
    Rating    (Comparison),           // folded current rating, 0..=5
    Project   (ProjectRef),           // resolved against the project store, §10
    Volume    (VolumeFilter),
    HasStrokes(bool),
    Source    (Vec<EventSource>),     // voice | typed | pencil | system
    Kind      (Vec<EventKind>),       // remark | rating | stroke | ...
}

pub enum DateField { Captured, Annotated }   // EXIF capture ts vs. event ts; default Captured

pub enum DateRange {
    Absolute { start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>> }, // half-open [start, end)
    Relative (RelativeRange),                // resolved against `now` at execution time
}
pub enum RelativeRange {
    LastDays(u32), LastWeeks(u32), LastMonths(u32), LastYears(u32),
    Season { season: Season, years_ago: u32 },  // "last winter" = Winter, years_ago: 1
    Month  { month: u8, year: Option<i32> },     // "in March", "March 2024"
    Year   (i32),
}
pub enum Season { Spring, Summer, Autumn, Winter } // N-hemisphere months; Winter spans the year boundary (Dec 1 – Mar 1)

pub enum StringMatch { Exact(String), Contains(String) }    // case-insensitive
pub enum PathMatch   { Subtree(String), NameContains(String) }
pub enum Comparison  { Eq(u8), Gte(u8), Lte(u8), Between(u8, u8) }

pub struct ProjectRef { pub raw: String, pub resolved: Option<Ulid> } // §10.3 fuzzy resolution
pub enum VolumeFilter { Online, Offline, Named(String) }

pub struct DroppedClause { pub raw: serde_json::Value, pub reason: String }
```

**LLM output JSON schema** (compact notation; the full JSONSchema derives
mechanically from it and is enforced via llama.cpp grammar / `json_schema`
response format, `temperature 0`). Top level — all three keys required:

```jsonc
{ "filters": [ Filter… ], "semantic": string|null, "visual": bool }

// Filter variants, discriminated by "type":
{ "type":"date", "field":"captured"|"annotated",            // exactly one of:
  "absolute": { "start": rfc3339|null, "end": rfc3339|null },
  "relative": { "unit":"days"|"weeks"|"months"|"years"|"season"|"month"|"year",
                "n": int, "season":"spring"|"summer"|"autumn"|"winter",
                "month": 1-12, "year": int } }               // unused keys omitted
{ "type":"camera",      "value": string }
{ "type":"lens",        "value": string }
{ "type":"folder",      "value": string }
{ "type":"rating",      "op":"eq"|"gte"|"lte", "value": 0-5 }
{ "type":"project",     "name": string }
{ "type":"volume",      "value": "online"|"offline" }
{ "type":"has_strokes", "value": bool }
{ "type":"source",      "values": ["voice"|"typed"|"pencil", …] }
```

**Prompt strategy (sketch — wording iterable, structure normative):**

- System: role ("translate a photographer's search into filters + remainder");
  today's date + timezone; "anything not expressible as a filter goes into
  `semantic` *verbatim, preserving the user's wording*"; "set `visual: true`
  only when the query describes what is IN the picture (objects, colors,
  composition), not what the photographer said or felt about it"; "emit only
  fields you are certain of; omit, never guess."
- Grounding lists, kept small: project names with status (§10), distinct
  camera/lens strings from EXIF (≤ 40 each, most-used first), root names.
- 4–6 few-shot pairs covering: relative season date, project fuzzy name,
  rating + camera combo, pure-semantic (empty filters), explicitly visual.
- User message: the raw query string.

**Validation (hallucination firewall):** deserialize with
`deny_unknown_fields` per variant; each filter validates independently.
Unknown `type`, out-of-range rating, unparseable date, camera/lens matching
nothing in the EXIF vocabulary (case-insensitive contains), project with no
fuzzy match ≥ threshold (§10.3) → that clause is **dropped**, recorded in
`ParsedQuery::dropped`, visible with its reason in the dev-build debug panel.
A dropped clause never fails the query. If everything drops and `semantic` is
null → fallback.

**Latency budget & fallback:** the parse must complete in **< 1.5 s** on the
small local LLM. On timeout, model unavailable (degraded mode), or wholly
undeserializable JSON: **fallback = the whole raw query as both FTS string
and embedding text, zero filters**, `fallback: true` (debug panel shows it;
any user-facing hint is UI.md's call). Search must never error because the
parser did.

### 5.2 Stage 2 — candidate generation (four signals)

All signals run against the **filtered universe**: structured filters compile
to SQL WHERE constraints applied to each signal's candidate join. Filters
filter; they never rank.

| # | Signal | Source | Ranked list of |
|---|---|---|---|
| S1 | `annotation_chunk` vectors — **primary** | embed(`semantic`) vs. `annotation_chunk` space, k=200 chunks | images (via event_targets), best chunk per image |
| S2 | FTS5 `event_fts` | `keywords` via §4 query construction, limit 500 | images, best bm25 per image |
| S3 | `summaries_fts` + `image_summary` vectors | same query texts vs. summary indexes | images |
| S4 | `image_clip` vectors | embed(`semantic`) vs. `image_clip` space, k=200 | images |

**S4 activation rule (normative):** the image_clip list participates iff
`semantic` is non-empty AND (`visual == true` OR the union of S1+S2 candidates
after filtering is < 10 images). Rationale: clip matching is the fallback for
the un-annotated and for explicitly visual asks; letting it always vote
dilutes the own-words identity on every query. When S4 contributed a result's
best evidence, provenance says so honestly (§6).

The semantic remainder is embedded **twice at most**: once by the text
embedder (serves S1 and the `image_summary` half of S3 — same space, same
`model_id`) and, only when S4 activates, once by the OpenCLIP text tower
(queries are short; 77 tokens is ample for a query even though it cannot
hold a 512-token chunk).

### 5.3 Stage 3 — fusion: weighted RRF, k = 60

**Image-level aggregation before fusion (decision: max per signal).** Within
each signal an image's representative score is the **max** over its hits
(best chunk / event / summary); images rank within the signal by that.
Defense: *sum* rewards verbosity — ten weakly relevant rambles would outrank
one dead-on sentence, and journal density reflects habit, not relevance. The
promise is "your best matching words come back": a max, not an integral.
(Cheaply revisable; ranking is deliberately not frozen, per SPEC-GAPS.)

**Fusion formula:**

```
score(img) = Σ over signals s where img appears:   w_s / (k + rank_s(img))
k = 60
w: S1 annotation_chunk vectors          = 1.0
   S2 event_fts (FTS5)                  = 1.0
   S3 summaries  (both sub-lists)       = 0.5
   S4 image_clip                        = 0.5  (when active, §5.2)
```

`rank_s` is 1-based within signal s. S3's two sub-lists (FTS and vector over
summaries) each contribute at w=0.5 — summaries are derived prose and must
never outvote the photographer's actual words.

**Tie-breaking:** equal fused scores order by `image_journal_stats.last_ts`
(EVENTS §5.3 — the materialized recency of the image's last live annotation
event; an indexed read, never a `max(ts)` fold at query time), most recent
first; then by `image_hash` for determinism.

Top 100 fused images proceed to results.

**Optional stage 3b — cross-encoder reranking (M3+, default OFF).** Behind a
config flag, a `Reranker` may reorder the head of the fused list:

```rust
pub trait Reranker {
    /// Reorders `candidates` by query relevance; returns the new order.
    fn rerank(&self, query: &str, candidates: &[CandidateText]) -> Result<Vec<usize>>;
}
```

- **Candidates:** the top **20–30** fused-union images' best-evidence texts
  (the §5.4 best-quote spans — 1–3 sentences each, far cheaper than benchmark
  passages).
- **Target models:** Qwen3-Reranker-0.6B or bge-reranker-v2-m3, ONNX int8 on
  CPU (≈ 130–400 ms for a batch this size).
- **Expected gain:** +5–15 nDCG@10, typical for a cross-encoder over a hybrid
  pipeline.
- **Activation gate:** stays OFF until the §12 eval demonstrates a measurable
  gap on the golden set **and** the reranked path stays inside the 1–2 s
  result budget. The fused order is the fallback whenever the reranker is
  off, unavailable, or over budget — search never blocks on it.

### 5.4 Stage 4 — result contract (normative; UI renders this, never raw rows)

```rust
pub struct SearchResults {
    pub query:        QueryEcho,            // raw string, ParsedQuery (incl. dropped, fallback)
    pub images:       Vec<ImageResult>,     // fused order
    pub session_hits: Vec<SessionHit>,      // session-level remark matches, separate list
}

pub struct ImageResult {
    pub image_hash:        ContentHash,
    pub preview:           PreviewRef,                  // cache key, LIBRARY.md
    pub score:             f32,                         // fused RRF score
    pub provenance:        Provenance,                  // §7 — REQUIRED, never absent
    pub last_annotated_ts: Option<DateTime<Utc>>,
    pub debug:             Option<DebugScores>,         // dev builds only
}

pub enum Provenance {
    Quote {                                  // the BEST matching span of the user's own words
        event_id:   Ulid,
        session_id: Ulid,
        ts:         DateTime<Utc>,
        source:     EventSource,             // voice | typed
        text:       String,                  // exact folded-text span (chunk or snippet window)
        char_start: u32, char_end: u32,      // span within the event's folded text
        highlights: Vec<(u32, u32)>,         // matched-term ranges within `text` (FTS hits)
        linked_stroke: Option<Ulid>,         // stroke event drawn with these words, if any
    },
    Stroke {                                 // image matched via has_strokes / stroke-only evidence
        event_id: Ulid, session_id: Ulid, ts: DateTime<Utc>,
    },
    VisualMatch,                             // image_clip evidence only — labeled honestly, NO fake quote
    FilterOnly,                              // pure structured-filter query
}

pub struct SessionHit { pub session_id: Ulid, pub quote: /* Provenance::Quote fields */ }

pub struct DebugScores {                     // dev-build debug panel only
    pub per_signal: Vec<(SignalId, Option<u32 /*rank*/>, f32 /*raw score*/)>,
    pub fused:      f32,
}
```

**Best-quote selection:** the image's top-ranked hit from the
highest-weighted signal that produced one: S1's best chunk (quote = the
chunk's span) → else S2's best event (quote = snippet window with
highlights) → else S3 ranked it: quote = the best *event-level* evidence
found by re-running the query against that image's own events; if none
exists, the summary hit is **discarded as provenance** and the next signal
is consulted → else S4 → `VisualMatch`. Summary text itself is never quoted
(it is not the user's words — E4).

## 6. Provenance — non-negotiable (E2)

**Acceptance criterion: no result row without an explanation the user can
read.** FTS hit → the snippet with matched terms highlighted, plus event
date, source, session. Vector hit → the chunk's exact folded-text span, same
metadata — the span is the user's verbatim words; the embedding only *chose*
it. `image_clip` hit → labeled **"visual match"**, explicitly and honestly:
no generated caption, no paraphrase, no fake quote. Filter-only /
stroke-only → `FilterOnly` / `Stroke` (UI.md renders "matches your filters" /
shows the stroke). Redacted/retracted text cannot appear because it is out of
every index (§1.1) — and as defense in depth the quote extractor reads folded
text at render time, so a stale index row cannot resurrect scrubbed bytes.

## 7. Worked examples (normative walkthroughs — tests reproduce these artifacts)

### 7.1 M3: "pull up the images I was considering for that quieter, melancholic series last winter"

**Stage 1 — parse** (LLM, < 1.5 s). Project store contains
`Quiet Hours (active)`, `Harbor Nights (shelved)`. Model emits:

```json
{ "filters": [
    { "type": "date", "field": "annotated",
      "relative": { "unit": "season", "season": "winter", "n": 1 } },
    { "type": "project", "name": "quieter melancholic series" } ],
  "semantic": "quieter, melancholic series I was considering",
  "visual": false }
```

Validation: date OK ("last winter", `field: annotated` because the query is
about when the *considering* happened) → resolves (run June 2026) to
`[2025-12-01, 2026-03-01)`. Project fuzzy match: "quieter melancholic series"
vs. {Quiet Hours, Harbor Nights} → best similarity 0.41 < 0.80 threshold →
clause **dropped**, `dropped: [{reason: "no project ≥ 0.80: best 'Quiet
Hours' 0.41"}]` (debug panel). Resulting `ParsedQuery`: one Date filter,
semantic remainder as above, `visual: false`.

**Stage 2 — candidates** (filter = events in the winter window, applied as
WHERE on every join). S4 inactive (`visual=false`, S1∪S2 ≥ 10). Suppose:

- S1 (annotation_chunk): img **A** (chunk of event `01HV…Q3`, span
  "something quieter in these three… almost mournful, could anchor the slow
  series"), img **B**, img **C** — ranks 1, 2, 3.
- S2 (event_fts on `"quieter," "melancholic" "series" …` — yields events
  containing "quieter"/"series"): img **B** rank 1, img **A** rank 2.
- S3 (summaries): img **A** rank 1, img **C** rank 2.

**Stage 3 — fusion** (k=60, weights 1.0 / 1.0 / 0.5):

```
A = 1.0/(60+1) + 1.0/(60+2) + 0.7/(60+1)   = 0.016393 + 0.016129 + 0.011475 = 0.043997
B = 1.0/(60+2) + 1.0/(60+1)                = 0.016129 + 0.016393            = 0.032522
C = 1.0/(60+3) + 0.7/(60+2)                = 0.015873 + 0.011290            = 0.027163
```

\* In this example S3 fuses as a single combined sub-list at an effective
weight of 0.7 to keep the arithmetic small; implementations fuse the two S3
sub-lists at 0.5 each per §5.3. Final order: **A, B, C**.

**Stage 4 — results.** `ImageResult` for A: provenance =
`Quote { event_id: 01HV…Q3, ts: 2026-01-14T21:08:11Z, source: voice, text:
"something quieter in these three… almost mournful, could anchor the slow
series", char_start: 0, char_end: 88, … }`. Debug panel (dev build) shows the
three per-signal ranks, the fused 0.043997, and the dropped project clause.

### 7.2 M1: typing `fog ba` with filter chip `rating ≥ 3`

- Keystroke "…ba" lands; 100 ms debounce elapses; prior query interrupted.
- Query construction: `MATCH '"fog" "ba"*'` (prefix index `prefix='2 3'`
  serves the 2-char prefix).
- SQL: §4 statement + `WHERE folded_rating >= 3` joined through
  `event_targets` → `images`.
- Hits: event `01HT…8M` ("the fog swallowing the barn, keep this one") on
  image **X** (bm25 −7.2), event `01HT…2C` ("fog bank ate the whole ridge")
  on images **Y, Z** (multi-target, bm25 −5.9). Y has rating 2 → Y filtered
  out; the same event still surfaces Z (rating 4).
- Grouping: X (best bm25 −7.2) then Z (−5.9). Snippets:
  X → `the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one`.
- Result: two `ImageResult`s, provenance `Quote` with highlight ranges from
  the snippet sentinels; `session_hits` empty; total time budget < 100 ms.

## 8. Read-scope context assembly (the concentric model, SCOPE.md)

One reusable component, `ContextAssembler` in `photoproof-core`, used by
**every** LLM call that needs library context: M2b summaries, M3 query-time
disambiguation (if ever), M5 partner. It fills a token budget through the five
concentric layers, in order:

```
B_total = model_ctx − output_reserve(1024) − prompt_overhead(512)
caps:  selection 40% · recency 15% · folder 10% · projects 10% · retrieval 25%
```

Fill order is layer 1 → 5; **unspent tokens from each layer roll forward** to
the remaining layers' caps pro-rata (in practice the elastic layer is
retrieval, which can always consume more pulls).

| Layer | Draws from | Form | Truncation rule when over its cap |
|---|---|---|---|
| 1. Selection | full **folded** transcripts + stroke descriptors of selected images, via event log | verbatim, oldest→newest, per-image headers (hash short-form, filename, capture date) | cap split evenly across selected images (unused share redistributed); within an image keep **newest events verbatim**, replace the truncated older span with that image's rolling summary + an explicit `[older notes summarized]` marker |
| 2. Recency trail | last **15** viewed images this session (session = 30-min-idle rule) | each image's rolling summary (§9.1), 1–2 sentences | drop oldest-viewed first |
| 3. Current folder | the folder rollup (§9.2) for the folder of the primary selection | one rollup block | hard-truncate tail |
| 4. Active projects | project store: projects with `status=active` | name + description + last 5 notes each | drop least-recently-updated projects first, then oldest notes |
| 5. Retrieval pulls | the §5 pipeline, queried with the caller-supplied question/topic | top-K results' provenance quotes + image identifiers | take results in fused order until budget exhausted |

Token counting uses the serving model's tokenizer when RUNTIME.md exposes it,
else the §2 heuristic. The assembler emits a structured `AssembledContext`
(layer-tagged blocks) so callers can place layers in their own prompts; it
never formats final prompts itself. Retracted/redacted content is invisible to
the assembler by construction (it reads folded state only).

**Lost-in-the-middle ordering (normative for callers):** models attend best
to the beginning and end of a long context and worst to its middle — the
effect persists in current long-context models
([Liu et al.](https://arxiv.org/abs/2307.03172)). Callers therefore place
retrieval blocks in relevance order with the **strongest evidence at the
start AND end** of the assembled context and the weakest in the middle. The
assembler hands back blocks in fused relevance order; the start/end placement
is the caller's obligation, stated here because every caller shares it.

## 9. Derived views — retrieval fuel ONLY (E4)

Hard rule, restated from the kernel: summaries and sentiment are **never
rendered as prose, scores, or tags in the user-facing product**. They exist to
make search and context assembly work. All derived rows carry provenance and
are disposable: deleting every table in this section loses nothing the system
cannot regenerate.

Common storage:

```sql
CREATE TABLE derived_summaries (
  id           TEXT PRIMARY KEY,              -- ULID
  scope        TEXT NOT NULL CHECK (scope IN ('image','folder','session')),
  scope_key    TEXT NOT NULL,                 -- image_hash | folder path | session_id
  text         TEXT NOT NULL,
  model_id     TEXT NOT NULL,
  prompt_ver   INTEGER NOT NULL,
  inputs_hash  TEXT NOT NULL,                 -- blake3 over: sorted input event ids + each folded-text hash + prompt_ver
  generated_ts TEXT NOT NULL,
  UNIQUE (scope, scope_key, model_id)         -- one live row per scope+model
);
CREATE TABLE sentiment_scores (
  event_id   TEXT PRIMARY KEY,                -- one score per text event
  score      INTEGER NOT NULL CHECK (score BETWEEN -2 AND 2),
  model_id   TEXT NOT NULL,
  prompt_ver INTEGER NOT NULL,
  scored_ts  TEXT NOT NULL
);
```

`inputs_hash` is the cache key: regeneration is skipped when recomputed
inputs hash to the same value; any input change (new event, revision,
retraction, redaction, prompt bump, model swap) changes it.

### 9.1 Per-image rolling summary

- **Regenerated when:** ≥ 5 new indexable events accumulated for the image
  since last generation, OR at session close for every image annotated in the
  session — whichever first. Runs in the background-pass scheduler
  (VRAM-polite, LIBRARY/RUNTIME).
- **Prompt contract (sketch):** input = previous summary (if any) + the new
  folded events with dates and sources. Instructions: ≤ 120 words; preserve
  the photographer's own vocabulary and phrases wherever possible
  (extractive-leaning); record judgments and intents *as theirs* ("she keeps
  returning to…", "marked as the anchor for…"); note trajectory if stance
  visibly changed; no aesthetic opinions of the model's own; plain text.
- On write: row upserted, `summaries_fts` row replaced, `image_summary`
  vector re-embedded (delete + append, §1.3).

### 9.2 Per-folder rollup

Same contract over the folder's images' rolling summaries (not raw events) +
folder-level facts (shoot date span, count annotated). Regenerated when > 20%
of member images' summaries changed since last build, lazily on first use.

### 9.3 Session summary

At session close (30-min idle boundary): input = the session's folded events
in log order with their target images. Used by the recency trail and context
assembly. Same table, `scope='session'`.

### 9.4 Sentiment (experimental — gated)

Per indexable text event, the local LLM scores the photographer's stance
toward the targeted image(s) on **−2 (strong negative) … +2 (strong
positive)**, integer, 0 = neutral/unclear. Stored per the DDL above with
`model_id` + `prompt_ver`. **Explicitly experimental pending the M3
evaluation flagged in SPEC-GAPS (open question 3): trajectories built on noisy
scores are worse than none.** Until that evaluation passes, sentiment rows are
written but consumed by nothing.

Trajectory queries are **M4**; the reserved query shape is plain SQL over
scored events ordered by time:

```sql
SELECT t.image_hash, e.ts, s.score
FROM sentiment_scores s
JOIN annotation_events e ON e.id = s.event_id
JOIN event_targets t     ON t.event_id = e.id
WHERE t.image_hash = :img AND e.retracted = 0
ORDER BY e.ts;          -- "came around on": early avg < 0, late avg > 0, etc.
```

### 9.5 Redaction propagation into derived rows

Redacting event E deletes E's sentiment row and **deletes** (not lazily
invalidates) every `derived_summaries` row whose input set included E — the
summary text may paraphrase scrubbed content. Affected summaries regenerate in
the background afterward. `summaries_fts` and `image_summary` vectors follow
their summary rows in the same transaction.

## 10. Project / intent store

### 10.1 Tables

```sql
CREATE TABLE projects (
  id          TEXT PRIMARY KEY,               -- ULID
  name        TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  status      TEXT NOT NULL CHECK (status IN ('active','shelved','done')),
  created_ts  TEXT NOT NULL,                  -- RFC 3339 UTC
  updated_ts  TEXT NOT NULL
);
CREATE TABLE project_notes (                  -- append-only, like the event log
  id         TEXT PRIMARY KEY,                -- ULID
  project_id TEXT NOT NULL REFERENCES projects(id),
  ts         TEXT NOT NULL,
  text       TEXT NOT NULL
);
CREATE TABLE project_members (                -- membership is evented, not destructive
  project_id TEXT NOT NULL REFERENCES projects(id),
  image_hash TEXT NOT NULL,
  added_ts   TEXT NOT NULL,
  removed_ts TEXT,                            -- NULL = currently a member
  PRIMARY KEY (project_id, image_hash, added_ts)
);
```

Removing an image sets `removed_ts` on the open interval row; re-adding
inserts a new row with a new `added_ts`. Full membership history is therefore
queryable ("was in the series last winter, dropped in spring") — required for
M4 temporal questions. `project_notes` rows are never edited or deleted.

**Project notes are not FTS-indexed in v1** — they are not image events
(`event_fts` would be a category error) and not derived prose
(`summaries_fts` would launder user truth through the fuel tier). They reach
the model through context assembly (layer 4) and reach search through
project-filter resolution; v1 does not full-text-rank projects as results.
A dedicated `projects_fts` table is recorded as a candidate v1.x addition if
dogfooding wants "find that project."

### 10.2 Portability — projects are user truth (sidecar-equivalent required)

Projects fail the "SQLite is a rebuildable index" test unless mirrored.
Normative: a single export file in app data, mirrored by the same debounced
writer that maintains sidecars (SIDECARS.md mechanics):

```
appdata/projects.photoproof.json
{
  "version": 1,
  "projects": [ {
      "id": "01HV…", "name": "Quiet Hours", "description": "…",
      "status": "active",
      "created_ts": "2026-01-02T19:00:00Z", "updated_ts": "2026-05-30T22:10:00Z",
      "notes":   [ { "id": "01HV…", "ts": "…", "text": "…" } ],
      "members": [ { "image_hash": "ab12…", "added_ts": "…", "removed_ts": null } ]
  } ]
}
```

- Included in the one-click full export beside the sidecar set + manifest;
  consumed by rebuild-from-sidecars. "You can walk away with everything"
  includes your projects.
- **Merge rules (union family, mirroring C2):** projects merge by project
  `id`; `notes` merge by set-union on note id (append-only ⇒ conflict-free);
  `members` merge by union on `(project_id, image_hash, added_ts)`, and for a
  matching key a non-null `removed_ts` beats null (a recorded removal never
  un-happens by merging a stale copy); two different non-null `removed_ts`
  resolve to the earlier. Project metadata (name/description/status) is the
  one mutable surface: **last-writer-wins by `updated_ts`** — accepted as a
  pragmatic exception to pure union, confined to three display fields.
- Image redaction does not touch membership rows (they contain only hashes);
  if an image's events are redacted it simply has no quotable journal.

### 10.3 Projects in search and context

- **Filter:** `Filter::Project` constrains results to current members
  (`removed_ts IS NULL`) of the resolved project, joined through
  `project_members`.
- **Fuzzy name resolution (parse-time):** the parser's emitted `name` is
  matched against all project names — every status — by normalized
  Jaro-Winkler similarity, **threshold 0.80**; ties broken by status
  (`active` > `shelved` > `done`) then `updated_ts`. Below threshold → clause
  dropped with debug visibility (§5.1). The grounding list in the parse
  prompt makes the LLM emit near-exact names in the common case.
- **Context assembly:** active projects are layer 4 (§8).

## 11. Reindexing & model swap

- **Embedder swap:** a new `(vec_kind, model_id)` space = new flat file +
  new `vectors` rows, built by a versioned backfill pass (mechanics:
  LIBRARY.md). The old space remains the **active query space** until
  cutover. **Cutover rule:** the new space becomes active when backfill
  coverage ≥ 99.5% of live indexable units for that vec_kind (stragglers — 
  offline volumes' previews, mid-flight events — finish after cutover). Config
  records `active_model_id` per vec_kind; the old space's file is deleted
  after a 7-day grace (instant rollback window). The two **text** vec_kinds
  (`annotation_chunk`, `image_summary`) cut over together — they must share a
  space so one query embedding serves both; `image_clip` belongs to the CLIP
  embedder and cuts over independently.
- **Query-parse / summary model swap:** `model_id` + `prompt_ver` on every
  derived row (§9) make this a lazy regeneration, not a migration: rows with
  stale `model_id` are regenerated by the background pass in
  recently-annotated-first order; old rows serve until replaced.
- **FTS rebuild command** (CLI + dev menu): wipe both FTS tables; re-fold
  every live remark chain root and live summary; **bulk reinsert** (event
  rows keyed by their existing `fts_map` rowids); finish with
  `INSERT INTO event_fts(event_fts) VALUES('optimize')` (and the equivalent
  for `summaries_fts`). There is no external-content `'rebuild'` fast path —
  `event_fts` is plain content-ful (EVENTS §5.4), and the folded text it
  indexes exists nowhere as a column to rebuild from. Required after any
  change to fold rules or tokenizer config (a tokenizer change is a schema
  version bump).
- Rebuild-from-sidecars (M1 integrity test) implies: sidecars + previews can
  reconstruct *every* table in this spec. Nothing here is truth.

## 12. Retrieval evaluation — the gate for every ranking knob

A **golden query set of ~50–100 query → expected-images pairs**, accumulated
from dogfooding (real queries against a real library, expected result sets
hand-marked), kept as repo fixtures. The harness runs the §5 pipeline and
reports **recall@20** and **nDCG@10** — per signal (S1–S4 individually) and
for the fused order.

This eval is the named gate for:

- **RRF weight tuning** — the §5.3 weights are defaults, not findings;
- **the S4 activation threshold** — §5.2's `< 10 images` constant;
- **a possible convex-combination fusion upgrade** — a tuned convex
  combination of normalized scores beats RRF given even a handful of labeled
  queries ([Bruch et al.](https://arxiv.org/abs/2210.11934)); revisit once
  the golden set exists;
- **the reranker go/no-go** (§5.3 stage 3b);
- **the 512-vs-1024 MRL truncation decision** (§1.3, runtime spike).

Once the harness exists, no ranking-affecting change ships without
before/after numbers on it.

## 13. Acceptance criteria

1. **M1 latency:** search-as-you-type p95 < 100 ms (keystroke post-debounce →
   result rows) on the reference 50k-image / journal-bearing library; grid
   interaction stays at 60 fps while querying.
2. **Provenance:** every `ImageResult` carries a `Provenance` that renders to
   human-readable text — a verbatim quote with date/session/source, a stroke
   reference, an honest "visual match", or "matches your filters". A result
   with none of these is a bug, not a degraded state. Summary text never
   appears as provenance.
3. **Parse fallback:** with the LLM stopped (or responding > 1.5 s), any NL
   query still returns results via whole-query FTS + vector with zero filters,
   `fallback=true` visible in the debug panel; no user-facing error.
4. **Hallucination firewall:** a parse emitting an unknown filter type, an
   invalid rating, or an unresolvable project/camera name drops exactly those
   clauses (visible with reasons in the debug panel) and executes the rest.
5. **Retraction/redaction:** the instant the retraction/redaction call
   returns, the affected text is absent from FTS results, vector results,
   provenance quotes, context assembly, and (for redaction) the flat-file
   bytes are zeroed and dependent derived summaries are deleted.
6. **Worked examples:** integration tests reproduce §7.1 and §7.2 — the parse
   JSON (under a stubbed `LanguageModel`), the dropped-clause record, the
   per-signal ranked lists, the fused scores to 6 decimal places
   (0.043997 / 0.032522 / 0.027163), the result ordering, and the exact
   provenance spans.
7. **Multi-target correctness:** one event targeting N images can surface all
   N, and per-image filters exclude individual targets without suppressing the
   event's other targets (§7.2's Y/Z case is the test).
8. **Rebuildability:** dropping `vectors`, both FTS tables,
   `derived_summaries`, and `sentiment_scores`, then running the rebuild
   passes, restores search behavior byte-for-byte equal provenance output.
9. **Projects round-trip:** delete SQLite; rebuild from sidecars +
   `projects.photoproof.json`; projects, notes, and full membership history
   (including closed intervals) are intact. Merging a stale copy of the file
   never un-removes a member and never loses a note.
10. **Fuel-only invariant:** no release-build UI surface renders
    `derived_summaries.text` or `sentiment_scores.score` (compile-time: the
    Tauri command layer exposes no API returning them outside dev builds).
11. **M1 query plan:** the §4 statement's `EXPLAIN QUERY PLAN` shows the
    MATERIALIZED hits CTE resolved before any join to `fts_map`/
    `event_targets`, and `snippet()` is evaluated for at most the LIMITed hit
    set (plan-shape test, guarding the 650× FTS-join planner failure).
12. **PPVEC v2 round-trip:** an int8/MRL-512 space written, closed, reopened,
    and scanned returns top-k identical (within quantization tolerance) to
    the transient-f32 reference path; the header round-trips
    dtype/dims/scale/offset exactly; redaction zeroes the stored int8 row
    bytes (byte-scan test, mirroring EVENTS I8).
13. **Eval harness exists:** the §12 golden-set harness runs against fixture
    data and reports recall@20 and nDCG@10 per signal and fused; the §5.3
    reranker stays OFF unless those numbers justify it within budget.
