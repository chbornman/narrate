# DECISIONS.md — Architecture Decision Log

Condensed record of decisions made during spec drafting (June 2026), so
resolved questions stay resolved. Organized: kernel (set before fan-out),
per-spec headline decisions, and the consistency-pass resolutions that
reconciled the seven parallel drafts. Specs are normative for their details;
this file is the index of *why*.

## Kernel (set before spec fan-out)

- **K1.** Image identity = BLAKE3-256 of file bytes, lowercase hex; never paths.
- **K2.** Event ids = ULID, minted monotonically at capture onset; all timestamps UTC; **log order = ULID order**, never re-sorted by `ts`.
- **K3.** One append-only log (`annotation_events`) + `event_targets` join for 0..N image targets. Derived data (embeddings, summaries) never in event rows or sidecars; `vectors` reference events.
- **K4.** *Retraction* = tombstone (content preserved, folded out). *Redaction* = content physically scrubbed — the one sanctioned append-only violation; redaction beats any merge.
- **K5.** *Revision* = corrected text referencing its target; folded (corrected) text is what UI/FTS/embeddings use.
- **K6.** **Merge = set-union by event id** — the one sync/backup/restore primitive.
- **K7.** Session = contiguous app use, 30-min idle boundary; automatic, never manual.
- **K8.** Utterances bind to the **selection snapshot at utterance start** (VAD onset), not transcript arrival.
- **K9.** One stroke (pen-down→pen-up) = one event; stroke↔utterance link by span overlap, else nearest ≤ 10 s.
- **K10.** Audio is never persisted; discarded after segment finalization; per-event ASR confidence kept.
- **K11.** Sidecars (`.photoproof.json`) beside images are truth; SQLite is a rebuildable index; export = sidecar set + manifest = rebuild input.
- **K12.** Ingest = independent versioned passes; M1 ships on **embedded RAW previews**, full decode is a backfill. Previews cached display-oriented + sRGB.
- **K13.** Byte-identical copies = one image, N paths; RAW+JPEG = two images in v1 (no stacking).
- **K14.** Retrieval = hybrid (annotation text primary, FTS5, image embeddings fallback, hard filters), RRF fusion; every result carries provenance (the user's own quote, dated). Summaries/sentiment are **retrieval fuel only — never user-facing prose**.
- **K15.** Small models first: Nemotron 0.6B streaming ASR; Immich-class OpenCLIP for visual; small Gemma 4 / Qwen 3.6 LLM. Managed local processes; the app never links LLM/ASR inference. Below the floor = degraded mode, which is exactly M1 and fully functional.
- **K16.** UI: quiet main flow (Grid → Look → Search); ambient capture; no live transcript; no metadata editing; small persistent scope indicator; on-demand journal panel; dev-only debug side panel (compile-time flag).
- **K17.** Recorded, not designed: future fine-tuning of a small LLM for app tasks; voice-command retraction; audio-retention opt-in; RAW+JPEG pairing; multi-machine sync as a product feature.

## EVENTS (spec/EVENTS.md)

- **E1.** Canonical JSON: sorted keys, **integers only — no floats, no nulls**, closed field set per `v`. Coordinates/pressure/confidence are quantized ints (float formatting is the cross-language canonicalization trap).
- **E2.** `text` is a top-level field; all other kind-specific data in `payload`. Six kinds exactly: remark, rating, stroke, revision, retraction, redaction. No machine bookkeeping in the journal (ingest/relink records live in LIBRARY tables).
- **E3.** Meta-events (revision/retraction/redaction) store zero targets; *effective targets* resolve through `target_event`, so every sidecar is self-sufficient.
- **E4.** Revision chains resolve to a root; greatest-ULID live revision wins. Retraction-of-retraction unsupported (re-state instead). Rating fold = last live rating; `value:0` ≠ unrated.
- **E5.** Redaction mechanics: scrub exactly `text`+`payload` (→ absent), add `redacted_by`; redaction registry (itself a fold) enforces merge supremacy; chain closure scrubs a whole revision chain; `secure_delete=ON` for physical hygiene.
- **E6.** FTS maintained transactionally in application code (triggers can't express folds); one FTS row per chain root. SQLite `foreign_keys=OFF` deliberately — dangling refs are inert-by-design for merge.
- **E7.** Sessions table mutation is limited to `ended_ts`; `device_id` per install; session merge = union + max(ended_ts).

## SIDECARS (spec/SIDECARS.md)

- **S1.** Canonical sidecar serialization: UTF-8/LF, 2-space pretty-print, lexicographic keys at every level, numeric arrays compact, absent-fields-omitted; file bytes are a pure function of journal content (no write timestamp).
- **S2.** `format: "photoproof-sidecar"` literal marker; image snapshot = filename + byte_size, advisory only — hash always wins.
- **S3.** Debounce 2 s quiet / 5 s cap; immediate flush on redaction, session end, shutdown, export. Atomic temp+rename with fsync discipline; corrupt files renamed aside, never deleted.
- **S4.** **Session journals**: zero-target events get `sessions/<ulid>.photoproof.json` in app data — sidecars-are-truth has no gaps. Export always includes overflow + session journals, and says so.
- **S5.** Redaction propagation: synchronous index scrub + durable scrubbed overflow record *before* UI confirmation; offline volumes via SQLite-backed queue drained at mount; honest guarantee statement (copies made outside the app are out of reach).
- **S6.** schema_version bumps only on breaking changes; unknown fields preserved on rewrite (except under redaction, which scrubs them — supremacy beats preservation); v1 readers treat v2 files as opaque and inviolable.
- **S7.** Same-id-different-content merge conflict: redacted copy wins; else deterministic byte-order winner, loser logged in the integrity report.

## LIBRARY (spec/LIBRARY.md)

- **L1.** In-place file overwrite = **new image identity**; the old journal stays on the old hash (dormant, surfaced via stale path rows); no auto-migration.
- **L2.** Volume identity: platform recipe + `.photoproof-volume` ULID marker file (written automatically on first ingest of writable volumes; marker beats platform ids); read-only detection by probe, not flags.
- **L3.** Watcher: 500 ms debounce, 2 s stability check for mid-copy files, move correlation 10 s, polled fallback; reconciliation = size+mtime fast path, re-hash only on evidence; 6-hour scheduled rescans.
- **L4.** The queue *is* `ingest_passes` rows; `running→pending` on startup is crash recovery. Priorities: watcher finds > scans > decode backfills > GPU backfills.
- **L5.** Previews: thumb 512 px / display 2560 px WebP; embedded-preview acceptance threshold ≥ 2048 px (else flag full-decode backfill); HEIC deferred to the backfill (keeps libheif off the M1 spine); cache never evicts in v1.
- **L6.** EXIF subset fixed (capture time+offset, gear, exposure, orientation, dims, GPS); ICC→sRGB at cache time; `generator_version` bumps are load-bearing (stroke coordinates depend on display orientation).
- **L7.** Budgets: 50k/1.5 TB library ≤ 90 min M1 ingest (8-core NVMe), first 1k thumbs ≤ 60 s, watcher-to-grid ≤ 5 s p95.

## CAPTURE (spec/CAPTURE.md)

- **C1.** Binding mechanism: in-memory ring buffer of timestamped scope snapshots; the final segment's `t_start` is authoritative; **zero grace window** (onset wins); a monologue spanning a selection change legitimately splits across images.
- **C2.** Audio: Rust-side cpal, mono 16 kHz, 60 s in-memory ring only, zeroed on disarm; partials never persisted, even on ASR failure.
- **C3.** No segment merging in v1 (one final = one event) — merging later is a fold/display policy; un-merging stored events would be impossible.
- **C4.** Pencil: raw points stored unsmoothed (render-time Catmull-Rom); commit threshold discards only sub-0.003-extent *and* sub-100 ms accidents; undo = retraction, depth 10, this-session only; eraser = whole-stroke retract by hit test.
- **C5.** **The link lives on the later-committed event only**, pointing backward; folds traverse both directions; a stroke overlapping a still-streaming utterance commits unlinked (the utterance carries the link).
- **C6.** Ratings: keyboard 0–5; multi-select rates all selected; 0 = explicit clear, distinct from unrated.

## RETRIEVAL (spec/RETRIEVAL.md)

- **R1.** Vectors: flat memory-mapped f32 files per (vec_kind, model_id), L2-normalized rows, logical delete + zeroing on redaction, compaction thresholds; brute-force cosine until ~500k, then swap behind `VectorStore`.
- **R2.** Weighted RRF (k=60): annotation vectors 1.0, FTS 1.0, summaries 0.5, clip 0.5 — the user's words always outvote derived/visual signals. Image aggregation = max per signal. Filters filter; they never rank.
- **R3.** image_clip participates only when the query is explicitly visual or own-words recall is thin (< 10 images) — protects the own-words identity.
- **R4.** Provenance ladder: chunk span → FTS snippet → re-resolved event quote; summary text is never quoted; visual matches labeled honestly; session-level hits never attributed to images.
- **R5.** Query parse: temperature 0, grammar-constrained JSON, grounding lists, per-clause drop-don't-fail; < 1.5 s or fall back to whole-query FTS+vector.
- **R6.** Collections (B71; earlier drafts said projects): separate store, evented membership (interval rows), portable via `collections.photoproof.json` with union-merge; collection metadata is the system's one named last-writer-wins exception.
- **R7.** Sentiment stored (int −2..+2) but consumed by nothing until the M3 quality evaluation passes.
- **R8.** Context assembler: budget caps 40/15/10/10/25 % (selection/recency/folder/collections/retrieval), unspent rolls forward; emits layer-tagged blocks, never final prompts.

## RUNTIME (spec/RUNTIME.md)

- **U1.** Exactly two managed external processes: llama-server (LLM, Q4_K_M, two priority lanes) + sherpa-onnx ASR server. No Python, no Docker, localhost-only random ports, children die with parent, single-instance lock.
- **U2.** ASR runs **CPU-only by design** — makes live-mic vs. LLM VRAM contention structurally impossible. English 0.6B is the v1 default (verified desktop path); multilingual Nemotron 3.5 export is the M2b spike's headline deliverable.
- **U3.** The embedders are the one sanctioned in-process exception (`ort`/ONNX Runtime): deterministic, fixed-shape, stateless, background-path only; a crash can never lose an annotation. CPU execution provider default until the spike validates GPU.
- **U4.** Tiers: 0 = < 8 GB → no models, full journal; 1 = 8–12 GB / AS 16 GB → E4B + ASR + embedders (~9.5 GB download); 2 = ≥ 16 GB → quality LLM optional. User override always.
- **U5.** Weights: HF download, SHA-256-pinned manifest compiled into the app; license acceptance gated per model; GC only after verify + reindex complete; orphans surfaced, never auto-deleted.
- **U6.** Supervision: health-gated readiness (features light up silently), backoff restarts capped then quiet `Failed`, mid-call crash retries exactly once.

## UI (spec/UI.md)

- **I1.** Single-image view named **"Look"**. Escape = strict back-one-layer; Search is an overlay remembering its return point.
- **I2.** Pencil = sticky toggle `B` (drawing hands cramp on hold); `O` toggles the overlay; eraser = hold `E`; red-dot cursor is the entire mode announcement.
- **I3.** Indicator: bottom-right capsule; `● session` when nothing selected; streaming-utterance tether state; pulse coalescing; ingest as a 2 px hairline. **Exactly three toast triggers** (retract-undo, redaction done, offline redaction completed); nothing else may toast.
- **I4.** Journal panel `J`: verbatim only, revision folding with "edited" affordance, retracted behind a toggle, redaction dialog is the app's one modal (copy must name offline latency + external-copy limits).
- **I5.** Dark theme only; the saturated pencil red is reserved for the pencil; has-journal dot is a dulled red. No onboarding tour; settings = exactly four sections.
- **I6.** Debug panel `F12`: cargo feature + frontend define, CI asserts release bundles are clean; read-only except force-flush / force-rescan / restart-process.

## Consistency-pass resolutions (post fan-out)

- **X1. Stroke payload encoding** (EVENTS × CAPTURE): canonical form is EVENTS' integer `[x, y, p, t]` tuples — x/y in **ten-thousandths of the display-oriented extent, range −2500..12500** (CAPTURE's overshoot clamp, integer-encoded); p per-mille 0..1000 with **1000 = device reports no pressure** (renders nominal width); `base_w` int ten-thousandths (default 40) added; `started_at` dropped (pen-down = `ts − t_last`); tool id is `"pencil"` (color belongs to the tool).
- **X2. Link directionality** (EVENTS × CAPTURE): CAPTURE's rule adopted — `linked_event` is carried by the **later-committed** event (stroke *or* voice remark), backward-pointing; EVENTS' CHECK and field table widened accordingly; folds traverse both directions. The circle-first-speak-after case demanded it.
- **X3. Two embedders** (RUNTIME × RETRIEVAL): CLIP text towers (77 tokens, image-aligned) cannot carry 512-token annotation chunks — the primary signal. Resolution: a small dedicated **text-embedding model** (Qwen3-Embedding-0.6B-class, in-process ort) owns `annotation_chunk` + `image_summary`; **OpenCLIP DFN5B** owns `image_clip` + short S4 query embeddings. Text vec_kinds cut over together on reindex; clip independently. Both flagged independently by RUNTIME and RETRIEVAL; kernel's single-embedder assumption corrected.
- **X4. vec_kind naming**: unified on RETRIEVAL's `annotation_chunk` / `image_summary` / `image_clip` (EVENTS' `event_text` renamed).
- **X5. Session journals** (SIDECARS extension): accepted — zero-target events get per-session journal files; EVENTS §2.3 already delegates their home to SIDECARS.
- **X6. Sidecar event shape**: SIDECARS' illustrative table and example rewritten to EVENTS §4 canonical fields (`session_id`, plain `targets` array, `payload`, `target_event`, `linked_event`, `redacted_by`, `v`); sidecar layout = canonical events re-indented (EVENTS' compact form is the dedupe normal form); meta-events store zero targets per E3.
- **X7. Legacy docs reconciled**: SCOPE.md (Draft 4) and M1-BUILD-PLAN.md updated — schema sketch replaced, "redaction = tombstone" corrected to retraction/redaction split, `series_ref`/`tombstone`/`markup`/`stroke_data`/SigLIP references removed; both now defer to `spec/` as normative.

## Research-pass resolutions (June 2026 best-practices review)

Four cited research reports (`docs/research/`) validated the spec set;
~30 amendments applied. Disposition table in `docs/BEST-PRACTICES-REVIEW.md`.
The decisions that changed:

- **P1. Vector storage = int8 scalar quantization at MRL-512 dims** (PPVEC v2,
  dtype in header). The f32/1024d flat-file math broke the 50 ms budget at the
  2M-event ceiling (8 GB scan ≈ 270 ms); int8+512d covers ~3M vectors at
  ~35 ms. Swap trigger restated in bytes-scanned (~1.5 GB/space), not rows.
- **P2. silero-vad fronts the ASR** (in-process ort, ~1 MB): supplies speech
  onset for the binding rule (sherpa-onnx emits no SpeechStart; transducer
  token times are systematically late), silence gating, and the "speaking"
  affordance. ASR endpointing keeps segmentation authority (VAD-only
  endpointing measurably hurts accuracy).
- **P3. One FTS construction**: EVENTS' plain content-ful `event_fts` +
  `fts_map` is normative; RETRIEVAL's external-content variant removed;
  prefix='2 3'; M1 search SQL materializes MATCH hits before joining and
  bounds snippet() to the LIMITed page (documented 650× planner trap).
- **P4. Journal folds pinned to ≤3 batched queries** — N+1 fold queries
  forbidden by spec; it is the only mechanism by which the hot path gets slow.
- **P5. One more derived table**: `image_journal_stats` (event_count,
  has_strokes, last_ts) for grid badges, filter chips, RRF tie-break.
  Confirmed: NO general materialized current-state projection — fold-on-read
  holds to the 5k-events/image worst case.
- **P6. Embedding hygiene normative**: Qwen3 instruction prefix on queries
  (1–5% free win), deterministic context prefixes on tiny chunks at embed
  time only; both versioned into inputs_hash.
- **P7. Optional `Reranker` stage** (Qwen3-Reranker-0.6B / bge-reranker-v2-m3
  class, top 20–30, CPU) added behind config, M3+, gated on **P8**.
- **P8. Golden-query eval harness** (~50–100 pairs, recall@20 / nDCG@10) is
  the gate for all deferred ranking decisions (RRF weights, S4 threshold,
  convex-combination fusion, reranker).
- **P9. Embedded-preview orientation verification** + per-format fixtures;
  stroke-substrate regeneration invariant explicit (correctness, not perf —
  previews are inconsistently pre-rotated across camera makers).
- **P10. FAT/exFAT uniform clock-shift detection** (DST moves every mtime by
  exactly 1 h — without detection, one transition = full re-hash storm).
- **P11. Cloud-sync awareness**: placeholder/dataless files never hashed
  (forces hydration); sync-root advisory; mtime-stable sidecar writes.
- **P12. Ingest budget honestly re-scoped to internal NVMe**; slow volumes
  scale with throughput, previews trail hashing visibly. Provisional
  quick-hash identity tier considered and REJECTED (two-state identity vs
  journal integrity).
- **P13. llama-server corrections**: `--ctx-size` divides across slots
  (16384/2 = 8192 per lane; VRAM re-budgeted); background lane MUST stream
  (disconnect-cancel only works for streaming); /health timeout under load =
  Busy not Lost (waitpid is ground truth).
- **P14. sherpa-onnx wire contract corrected** (float32 frames + "Done");
  confidence = exp(mean token log-prob), optional, uncalibrated; spike tests
  Nemotron token-timestamp availability and may replace the demo-grade
  websocket server with a thin Rust-crate wrapper child process.
- **P15. cpal discipline**: open default device config + resample in-app;
  watchdog re-arm on stream death; stream CLOSED (not paused) on disarm so
  the OS mic dot always agrees with app state; Bluetooth-HFP quality advisory.
- **P16. Tauri delivery normative**: thumbnails/Look images via custom URI
  scheme only — image bytes never cross IPC, never base64; webview memory
  bound added to acceptance; img-element recycling.
- **P17. Pen pressure = progressive enhancement**: expected on Windows
  (WebView2/Windows Ink), NOT expected in macOS WKWebView — constant base_w
  there; native NSEvent pressure passthrough recorded as the future fix.
- **P18. SQLite operations**: 64 MiB page cache, 256 MiB mmap, busy_timeout,
  PRAGMA optimize on close, ANALYZE after rebuild/merge, checkpoint(TRUNCATE)
  at idle, no held-open read statements, one writer + read pool; rebuild
  inserts id-sorted in ~10k batches with FTS/derived after the union.
- **P19. EmbeddingGemma-308M added to the spike** as the half-cost text-
  embedder candidate; Moonshine/Kyutai STT on the ASR watch list behind the
  trait.

## Build-pass resolutions (implementation, June 2026)

Ambiguities found while implementing, resolved per the integrity invariants
and flagged here per the build loop. Spec text stands; these record the
chosen readings.

- **B1 (P1.1).** FTS5 ignores `PRAGMA secure_delete`; the I8 byte-scan
  demands `INSERT INTO event_fts(event_fts, rank) VALUES('secure-delete', 1)`
  at table creation — adopted as the FTS analogue of EVENTS §5.1.
- **B2 (P1.1).** `redact()` carries no session parameter; redaction events
  take the latest open session, else the latest session, else the target's own.
- **B3 (P1.1).** Reverse `linked_event` traversal resolves within the fetched
  fold closure (K8 guarantees the linker targets the same image), keeping the
  §10.1 plan at exactly 3 queries; no new index.
- **B4 (P1.1).** `image_journal_stats`: `event_count` = non-retracted content
  events (scrubbed stubs count); `has_strokes` = non-retracted *and*
  non-scrubbed strokes; `last_ts` = ts of the greatest-id live event.
- **B5 (P1.1).** Session union is a separate primitive (`merge_sessions`);
  `merge()` stays events-only.
- **B6 (P1.1).** `rebuild_derived()` preserves `sidecar_dirty` (durable queue
  with ack history) and live-root vectors (RETRIEVAL owns re-embedding);
  deletes dead-root vectors.
- **B7 (P1.1).** Duplicate redactions of one target: registry keeps the min
  redaction id deterministically; the victim's `redacted_by` is first-learned
  and immutable (scrub-only trigger).
- **B8 (P1.1).** Fold timing asserted at 10 ms in release (measured 7.9 ms),
  100 ms debug allowance; query count: 3 SELECTs norm, +1 per revision-chain
  fixpoint round (§10.1's own wording).
- **B9 (P1.1).** Append requires resolvable revision/retraction targets;
  dangling refs arise only via merge (inert-by-design). Sessions need not
  pre-exist at append.
- **B10 (P1.2).** VAD trait shape: push-mode `process_frame`/`reset`/
  `sample_rate`, events `SpeechStart{onset}`/`SpeechEnd{end}`, per-frame
  gate; `Send` not `Sync` (lives on the capture thread).
- **B11 (P1.2).** `VecKey` = space + unit (`event_id`+`chunk_index` |
  `image_hash`) per RETRIEVAL §1.2's unique indexes; dedicated
  `VectorStoreError` instead of the supervision-shaped `ConnectorError`.
- **B12 (P1.2).** Reranker returns candidate indices and gains `model_id()`;
  the text embedder's unsupported `embed_image` maps to `Backend{status:501}`.
- **B13 (P1.2).** Config: non-empty `api_key_ref` must match
  `keychain:<service>/<account>` else hard parse error; `chunk_ms` ∈
  {80, 160, 560, 1120} enforced at parse; unknown keys warn, never fail.
- **B14 (P1.2).** Native `async fn` in traits kept per RUNTIME §4: traits are
  not dyn-compatible and futures carry no `Send` bound — later packets use
  static/generic dispatch.
- **B15 (P2.2).** Cloud placeholders get a skipped `ingest_passes` row keyed
  by a deterministic sentinel hash `blake3("photoproof-placeholder\0" +
  volume + "\0" + rel_path)` (placeholders have no content hash by
  definition); the sentinel clears on hydration.
- **B16 (P2.2).** Re-registering a removed root revives the removed row
  (roots are identified by location under `UNIQUE(volume_id, rel_path)`),
  consistent with §5's relink-everything-on-re-register.
- **B17 (P2.2).** §13.1's "zero extra hashing" applies to the paired-rename
  path; an unpaired remove+create hashes once to prove content identity
  (§12 explicitly budgets re-hash on moves). Both asserted in tests.
- **B18 (P2.2).** The `image` crate's TIFF decoder doesn't surface IFD
  orientation; the preview pass falls back to the §9.6 orientation stored by
  the EXIF pass (kamadak-exif reads it).
- **B19 (P2.2).** Capture time without `OffsetTimeOriginal` is stored as
  wall time + `Z` with `capture_tz_offset` NULL.
- **B20 (P2.2).** Hash pool sizes from `available_parallelism` (logical),
  capped at 8. `ingest_passes` gains a `not_before` column (§10.5 backoff).
  §10.3's async dispatcher/GPU-yield is shell wiring: the packet ships queue
  semantics, synchronous `process_queue`, and `maintenance_tick` /
  `on_system_resume` / `probe_volumes` hooks for the shell's scheduler.
- **B21 (audit fix).** Redaction closure on dangling/cyclic chains (where
  §6.1's `root()` is undefined): anchor at the highest locally reachable
  ancestor and scrub every local revision resolving to it — the
  privacy-conservative closure; reachable *ancestors* of the target are
  scrubbed too.
- **B22 (audit fix).** `append()` of a registry-condemned id is rejected
  (`AppendError::CondemnedId`) rather than inserted scrubbed: local append
  of condemned content is producer corruption, unlike merge, where the
  scrubbed-form insert is correct union semantics.
- **B23 (audit fix).** A blocked post-commit `wal_checkpoint(TRUNCATE)`
  surfaces as `Err(StoreError::CheckpointBlocked)` after bounded retry —
  the write is already durable; `maintain()` completes the hygiene. Silent
  swallowing (what the audit flagged) was rejected.
- **B24 (P2.1).** Events carrying unknown extra fields (EVENTS §4.1(8) vs
  SIDECARS §5.2): preserved verbatim as opaque entries (byte-equivalent
  rewrite, sorted by id, redactable via husk) but NOT indexed — EVENTS'
  closed field set is normative for event shape (X6); preservation and
  redaction supremacy both hold.
- **B25 (P2.1).** Unparseable file at a sidecar slot: renamed aside as
  corrupt ONLY if it positively self-identifies as ours (format-marker
  bytes); otherwise it is a §2.3 collision and is never touched.
- **B26 (P2.1).** §10.3(a) swapped-images rehoming is literally unbounded
  mutual recursion; rehoming goes through the overflow store (verified
  durable) with same-scan migration to adjacent — identical end state,
  one-scan convergence.
- **B27 (P2.1).** Manifest `counts.sessions` = distinct session ids over
  events; `filenames` = the snapshot filename (extensible via the
  `VolumeInfo` seam).
- **B28 (P2.1).** Sessions learned only from sidecars get
  `device_id = 32×'0'` (sidecars deliberately carry no machine ids, §3.2).
- **B29 (P2.1).** Volume identity crosses the packet boundary through the
  `ImageLocator` trait (`Writable`/`Unwritable{image_path}`/`Offline`) +
  `VolumeInfo` — owned by sidecars, implemented by the library layer.
- **B30 (P3.1).** The executed §4 statement orders by FTS5 `rank` (default
  bm25) instead of the spec's `ORDER BY s` alias form: the alias is not
  consumed by xBestIndex → temp B-tree → `snippet()` evaluated per candidate
  pre-LIMIT (measured 20,000 calls vs 500) — the exact failure §4 itself
  forbids. Ordering identical; everything else verbatim.
- **B31 (P3.1, spec erratum).** §7.2's X-before-Z ordering is unreproducible
  under real FTS5 bm25 with the spec's own texts (equal tf, X longer, IDF
  clamped → length normalization always favors Z). Tests assert the engine's
  true ordering [Z, X] and every other §7.2 artifact exactly. RETRIEVAL §7.2
  should swap the expected order when next edited.
- **B32 (P3.1).** Session hits resolve in the same statement via LEFT-joined
  `event_targets` (re-running MATCH doubled the dominant cost); image-scoped
  chips use the inner-join form and suppress session hits (R4). Prefix-term
  highlights trim to the typed prefix; diacritic-folded matches keep the
  whole-token highlight. Provenance text is re-read from truth tables, never
  the FTS index.
- **B33 (P3.1).** Filter minutiae: camera/lens ASCII-case-insensitive;
  `Folder::Subtree` = volume-relative segment prefix; `NameContains` matches
  directories only; M3-only filters (`Project`, `Kind`) hard-error rather
  than silently drop; the core result contract carries no serde — the shell
  maps to its own DTOs (`search_wire`).
- **B34 (P3.2).** Grid thumbnails render exactly two badges (UI §3.5);
  rating data ships in `GridItem` but is never rendered on thumbnails. The
  has-journal dot requires remark/stroke evidence — rating-only journals
  don't light it (UI §3.7 over the B4 event_count); a derived `has_text`
  stats column is the clean fix → P4.1.
- **B35 (P3.2).** The typed-note transient cancels on any scope change, making
  UI §6's summon-time scope and CAPTURE §4's submit-time binding identical by
  construction. Look always narrows scope to the viewed image (CAPTURE owns
  scope; UI §4.5's parenthetical loses).
- **B36 (P3.2).** M1 ships chip rendering/removal but NO chip-creation
  affordance (no parser, no manual builder specced — quiet wins); 50 ms
  search debounce (UI normative over RETRIEVAL's 100 ms), ≥2 chars; the
  model-consent screen waits for P6.2's `runtime_status` contract;
  Settings' "rebuild index" = in-process union-merge + `rebuild_derived()`,
  the fresh-database restore stays an offline path.
- **B37 (P4.1, closes B34).** `image_journal_stats.has_text` = any live AND
  non-scrubbed remark (symmetric with B4's `has_strokes`; a redacted stub is
  not words-evidence). The grid's has-journal dot = `has_text || has_strokes`.
- **B38 (P4.1).** `has_text` was added to the v1 DDL in place — no deployed
  databases exist pre-dogfood. After dogfooding begins, schema changes
  require real migration steps.
- **B39 (P4.1).** Product-facing reads all go through core APIs
  (`journal_stats`, `list_folder`, `folder_tree`, `open_sessions`); the
  shell's raw-row reads survive only behind the `debug-panel` feature
  (diagnostics rendering raw rows). `Library` implements `ImageLocator`
  (B29's home); `SidecarEngine::new_shared(Arc<EventStore>, …)` added, the
  borrowed `new` retained to avoid churning the sidecars suite.

## UI build decisions (P4.2, June 2026)

The featureset/architecture amendments recorded by UI-ARCHITECTURE §11,
landed by the P4.2 build (INTEGRATION). Featureset = docs/UI-FEATURESET.md;
where it amends spec/UI.md, the featureset wins.

- **U1.** Space opens AND closes Look (featureset §0 symmetric open/close
  supersedes UI.md §3.4's Space-toggles-selection); keyboard
  selection-toggle moves to **Ctrl+Space**. Not a chorded hot-path verb:
  selection-toggle is not a per-image annotation verb, so the §8
  no-chorded-verbs guardrail stands.
- **U2.** `Tab` is consumed globally for lights-out (D5); webview Tab
  focus-traversal is forfeited in the main window only (Settings keeps it).
  A11y note: arrows/Enter/Esc remain the keyboard path; recorded as a
  deliberate trade against UI.md §12 keyboard completeness.
- **U3.** The rail and inspector are **push** panels (no auto-hide, no
  dwell, no pin) — supersedes UI.md §3.7 "overlay, not push" and §8.1's
  slide-over journal panel. Inspector width persists; openness does not.
  The §3.7 "summoning never reflows the grid" acceptance line is replaced
  by integer-column re-snap during panel resize (perf item in DOGFOOD).
- **U4.** Collapsed-stack write events target both members as ONE ordered
  multi-target list, display member (JPEG) first, then RAW
  (`event_targets.position` — CAPTURE §3). One cell, two targets: the
  indicator truthfully reads "● 2".
- **U5.** The capture indicator and an open note input are EXEMPT from Tab
  lights-out (the indicator is capture-state truth; modes must stay
  visible — coordinator ruling; founder sign-off requested at P4.2 gate).
- **U6.** Surround luminance (D6) surfaces ONLY via the gutter and
  Look-backdrop right-click seats + the persisted pref; the Settings §2.4
  enumeration stays closed (no appearance section).
- **U7.** Sheet instances are enumerated: cheatsheet and drop-confirm.
  A new Sheet instance is a spec change.
- **U9 (P4.2b).** Settings gains one row inside Watched folders ("Stacked
  pairs show: JPEG|RAW"); persisted in the backend settings store (NOT
  localStorage — it must cross the Settings→main window seam live via the
  `settings-changed` event).
- **U10 (P4.2b).** The indicator's scope segment click = `open-inspector`
  (journal) toggle; note-summon moves to the capsule remainder. The
  indicator remains a status strip.
- **U11 (P4.2b).** U4's collapsed-pair target order is *display-member
  first* — RAW-first when the preference is RAW (`event_targets.position`
  consumers see the preference-dependent order; tested).
- **U12 (P4.2b).** The P16 protocol gains `/original/<hash>`: stored-format
  allowlist (jpeg/png/webp), online-only, immutable cache headers, uniform
  404 refusal. "Actual" (100%) zoom stays preview-relative even when the
  original renders (it draws in the preview's layout box so the canonical
  zoom session carries exactly); true 1:1-of-original is an M1.5 question.
- **U13 (P4.2b + coordinator).** ActionDef gains optional `checked(ctx)`
  (menu toggle state stays defs-only). Expanded stack members link via a
  ±2 px inward nudge + bridging underline (sanctioned micro-deviation from
  strict column alignment). Zoom transforms normalize through
  `clampOffsets` in `carryOver`: per-axis centering when the scaled image
  fits, edge clamping while it overflows — corner anchors pin to edges by
  design.
- **U8 (INTEGRATION amendment).** The drag-folder drop-confirm joins the
  Esc order as layer 2 (after the redaction modal, before the context
  menu): Esc is now a **13-layer** order. The Sheet contract promises Esc
  dismisses, and Esc routes only through logic/escape.ts.
- **U9 (INTEGRATION).** Window geometry persists via
  `tauri-plugin-window-state` (settings window denylisted — it stays the
  one modest window). `tauri-plugin-opener` was NOT adopted: commands/os.rs
  ships tested xdg-open-class spawns; swapping the launcher in later
  touches no command surface. Wayland restore-drift is a named DOGFOOD
  §visual check, with a manual save/restore fallback path reserved in
  commands/app.rs if the plugin misbehaves.
- **U10.** The retract-toast Undo performs **RE-STATE** (a new event
  carrying the folded content into the current session) — never an
  un-retraction; retraction-of-retraction stays forbidden (E4).
  Double-Undo is idempotent (the backend declines non-retracted targets).

## P5.1 pencil decisions (June 2026)

Build-pass readings (continuing the B series) and one UI amendment
(continuing the U series), recorded by the P5.1 grease-pencil packet.

- **U14 (P5.1).** Spec keys win over the P4.2 reserved band: **B** = sticky
  pencil toggle, **hold-E** = eraser, **O** = the single overlay toggle (UI
  §4.4/§11). `pencil-visibility` (V) and its Action kind are removed — the
  "union extended, never narrowed" rule does not protect a never-dispatched
  reserved kind replaced by its own packet; the `pencil-pen` /
  `pencil-eraser` / `cycle-overlay` ids are kept. New look-scope
  `pencil-undo` (Ctrl+Z) row, enabled only when pencil work exists so the
  layer never swallows the chord (keyboard-only via named exemption; the
  journal panel's Retract row is the pointer path). Space-at-fit no longer
  closes Look while pencil is ON (§11's pencil-on Space row beats U1's
  symmetry; closing mid-mark would also destroy the mode).
  UI-ARCHITECTURE §8's anticipated pencil toolbar stays unbuilt and the
  `look-toolbar` seat reserved/empty — §4.4's zero-chrome mandate wins;
  pointer reachability is satisfied by Pencil/Overlay rows on the
  look-backdrop menu.
- **B40 (P5.1).** Over-8192-point strokes: §8.2 bounds the count but not
  the overflow behavior. Capture decimates the in-flight buffer by stride 2
  (first/last samples always kept) and continues — tail truncation rejected
  because silently losing the END of a gesture is the worse integrity
  violation. Triggers only after ~65 s of continuous 125 Hz drawing; below
  the bound C4's raw-unsmoothed rule holds.
- **B41 (P5.1).** §8.4's commit-threshold duration = pen-down → pen-UP wall
  time (not t_last): the only reading under which a motionless
  press-and-hold dot (one sample, t = [0]) can commit. Consequence of X1's
  schema: a held dot's dwell is unrecoverable from the payload, and
  `ts − t_last` reconstructs the span only to the last move sample —
  P6.1's §9.1 span math must reckon with under-spanned dots (recorded fix
  if linking needs it: a terminal pen-up sample, dedupe-exempt).
  **RESOLVED (founder, June 2026): the terminal pen-up sample lands with
  P6.1** — the pointer-up position/time is recorded as the stroke's final
  sample, dedupe-exempt (more faithful to the hand, not less; one
  near-duplicate point per stroke), making `ts − t_last` exact before the
  §9.1 resolver is built on it.
- **B42 (P5.1).** `add_stroke` bounds `base_w` to 1..10000 (core leaves it
  unbounded; a stroke wider than the entire long edge is rejected as
  hostile input). The spec default 40 is unaffected.
- **B43 (P5.1).** Pencil retractions (Ctrl+Z undo, eraser) pulse the
  indicator but do NOT toast: UI §7.5's retraction toast is the
  journal-panel Retract flow (§8.3); a toast Undo (U10 re-state) on pencil
  undo would be de-facto redo, which §8.5 forbids in v1.
- **B44 (P5.1).** Journal-panel stroke micro-previews render as stored,
  without §8.1 orientation-mismatch compensation (rows don't fetch current
  image metadata); the Look overlay compensates where the current display
  orientation is known. Drift occurs only if an external tool rewrites
  EXIF orientation post-capture.
- **B45 (P5.1).** The eraser fires on pointer-DOWN (§8.6's "click/tap" read
  as press — snappier, with no drag semantics to wait out); the stylus
  eraser end is detected via PointerEvent button 5 / buttons bit 32.

## P6.1 capture decisions (June 2026)

Build-pass readings recorded by the P6.1 capture-engine packet
(mock-verified by design, per BUILD-LOOP's honest-scope table).

- **B46 (P6.1).** CAPTURE §6.5's asr capture payload realized in EVENTS'
  closed integer field set: `speech_started_at` ≡ the event's `ts`
  (utterance id + ts are minted at VAD onset), `speech_ended_at` ≡
  `ts + dur_ms`, `confidence` → `conf_pm`. `model_id` is NOT representable
  in the v1 field set (storing it would break §4.1 rule-8 byte-exact
  round-trips) and stays debug-panel territory. **EVENTS §3.1 erratum:
  `conf_pm` is OPTIONAL** — omitted entirely when the model exposes no
  token log-probs (CAPTURE §6.5 governs; never null), per §4.1 rule 6.
- **B47 (P6.1).** `dur_ms` = the **VAD span** (onset → VAD speech end; the
  segment's reported end as fallback) — EVENTS §3.1's "(VAD onset →
  finalization)" parenthetical loses to CAPTURE §9.1's linking span:
  finalization-based spans over-span by ASR emission latency and would
  corrupt overlap linking, and §3.1 itself says `dur_ms` feeds linking.
- **B48 (P6.1).** Session bookkeeping (`closed_clean`,
  `close_processing_done` — CAPTURE §2.3) lives in a capture-owned,
  index-only `capture_session_state` table (schema v6): the EVENTS §5.2
  sessions table stays byte-identical and §9's "ended_ts is the single
  permitted UPDATE" letter holds. The bookkeeping is intentionally
  non-rebuildable.
- **B49 (P6.1).** Token-time cross-check inputs: P1.2's TranscriptSegment
  carries one onset (the VAD-onset echo) and no separate ASR token
  t_start; capture-side VAD SpeechStarts associate to ASR utterance ids
  FIFO in stream order, and the segment's own onset serves as the §5.1
  cross-check — >250 ms disagreement across a scope change logs to the
  debug panel, never rebinds. A genuine independent token-time input waits
  on the Transcriber trait growing one (the P6.3 spike informs whether the
  Nemotron export can supply it).
- **B50 (P6.1).** §9.2's in-flight suppression over "span-so-far
  (onset..now)" reduces to: ANY utterance in flight at pen-up suppresses
  (a span ending at now always touches a stroke committing at now) —
  implemented as derived, derivation in a comment, and the
  wrong-fallback-partner case is pinned by test.
- **B51 (P6.1).** §5.4 with multiple simultaneously in-flight utterances
  (spec silent on plurality): the indicator's `streaming_utterance` shows
  the MOST-RECENT onset's bound scope — the one being spoken.
- **B52 (P6.1).** §2.5's "steps 1–2 block quit (capped)" splits across
  layers: the core engine never sleeps — disarm sets the 5 s drain
  deadline on the capture clock and pump() enforces it, force-abandoning
  stragglers so trailing finals can never mint into a LATER session; the
  real bounded blocking wait at quit belongs to the shell/P6.2 pump
  thread. Also recorded: engine-minted ids share the store's single Minter
  (new `EventStore::mint_at`) so I14 process-wide id monotonicity holds
  across capture-minted and store-minted events.

## P6.2 runtime decisions (June 2026)

Build-pass readings recorded by the P6.2 runtime-supervision packet
(stub-verified by design; real binaries, pins, and TLS arrive with P6.3).

- **B53 (P6.2).** RUNTIME §8.1's Restarting(n) backoff exponent resets to 1
  after each Ready (a consecutive-failure exponent), while the 5-attempt
  budget stays a pure rolling 10-minute window ACROSS Ready periods — the
  reading most consistent with flap protection (a flapping child exhausts
  its budget even though each crash follows a Ready; a one-off crash after
  a long healthy run restarts at 1 s). Both halves pinned by tests, the
  exponent-reset by a mutant the review killed.
- **B54 (P6.2).** §9's "interactive queue depth 1" on overflow: a newer
  interactive submission displaces a still-QUEUED one, which completes as
  Cancelled (search-as-you-type supersedes itself); a RUNNING interactive
  call is never cancelled.
- **B55 (P6.2).** The manifest schema gains an explicit per-file
  `revision` field (§5.1's example embeds the revision in prose; the
  schema needs it as data). All SHA-256/revision pins ship as explicit
  `UNPINNED-P6.3` placeholders and verification FAILS CLOSED until the
  spike pins real artifacts. Likewise the download transport: `hf:` URLs
  resolve to https and the localhost-grade client refuses them
  (TlsUnsupported) — choosing the TLS client (ureq/rustls vs reqwest) is a
  P6.3 decision; the manager is fully verified over plain HTTP against the
  stub server.
- **B56 (P6.2).** The supervisor's Downloading/DownloadFailed states are
  fed by a WeightsGate from the download manager (a separate component per
  §5) rather than being supervisor-internal; the §8.1 diagram's states are
  preserved verbatim in the machine. License-not-yet-accepted is NOT a
  failure row — the gate simply hasn't opened, and settings shows the
  acceptance affordance instead of an error.
- **B57 (P6.2, amends B52).** CAPTURE §2.5 step 3 hardened from "never
  blocks" to "never runs inline": session close only ENQUEUES
  (close_processing_done = 0) and the sidecar pump tick drains the queue.
  And the drain-deadline boundary semantics changed deliberately (the
  backlog item's requested reading): a trailing final that becomes ready
  only AFTER the 5 s cap is abandoned — previously it could still mint if
  the poll happened to return Ready past the deadline.
- **B58 (P6.2).** §5.2's "background priority" downloads are realized as a
  dedicated worker thread plus the pacer throttle seam (no OS
  thread-priority call), and one-file-at-a-time is enforced ACROSS models
  (the review caught the consent path fanning out one thread per model —
  now a single serialized queue). The per-model "pause" action is deferred:
  resume-from-part makes pause equivalent to stop.

## Batch-1 polish decisions (June 2026)

Founder rulings from the batch-1 polish round (journal/look/rail/raw
clusters), recorded at merge.

- **B59 (founder).** The journal rows' "Select in grid" affordance
  (select-from-note) rides EVERY entry kind with targets except redacted
  stubs: ratings and retracted rows keep Select — their targets are real
  and useful to jump to — while a stub offers no verbs at all, even when
  the DTO still carries the event's targets (redaction means the entry is
  gone; affordances on the stub read wrong). Availability is pure row
  logic (rowActions).
- **B60 (founder).** Multi-select inspector display: the panel keeps
  showing ONE image's truth — the anchor the inspector already follows —
  plus a quiet "N selected" line (grid only; Look narrows scope by
  construction). N counts the stack-expanded write-scope targets, the
  same N a rating keystroke mints against and the scope ring echoes. No
  aggregate/combined panel; revisit at the M3 sidebar design pass if
  dogfood demands it.

## Wave 2 polish decisions (June 2026)

Founder rulings from the wave-2 polish round, recorded at merge.

- **B61 (founder).** The journal row's "+N others" sibling mark never
  counts the inspected image's own RAW/JPEG pair member: an entry minted
  against a collapsed pair targets BOTH members (DECISIONS 4), but the
  mate is the same picture — the stack badge already says "2", and
  "+1 other" must mean a genuinely DIFFERENT image. The mark is
  suppressed entirely when every extra target is the inspected image's
  pair-mate. The pure row logic keeps the ruling testable
  (`siblingTargetsLabel(targets, inspectedHash, pairMateHash)`); the
  grid slice resolves the mate through the unit hosting the inspected
  hash (collapsed alt or expanded partner cell — collapse state must not
  change what counts as a different image) and JournalTab threads it
  down. (Founder, dogfood round 3, June 2026.)

## Dogfood round 3 resolutions (June 2026, recorded at the wave-2 merge)

The first founder-machine (macOS) session. Build-pass rulings from the
fix packets it produced; the macOS portability finds themselves (libc
kinfo_proc, FSEvents symlink-resolved paths, the stub volume probe) are
ledger rows, not decisions.

- **B62 (amends §10.5).** An offline volume is never evidence about a
  FILE: a pass claimed while no online active path exists DEFERS — re-
  pends with the long transient backoff and gives the claim's attempt
  back — instead of burning lifetime attempts (a flapping volume killed
  two-thirds of a folder's passes at the cap before this). The
  restart/6-hour retry rescues `volume-offline` error rows REGARDLESS of
  attempts with a fresh budget (heals pre-fix databases), and a volume's
  online transition clears the defer backoff on pending rows too —
  replugging must not wait out a 10-minute `not_before`.
- **B63 (LIBRARY §4.1).** Level-3 heuristic volume matching is
  implemented (it was specified but missing — heuristic-identified
  volumes could never re-match and flapped offline), and ambiguity
  refuses to guess: two indistinguishable candidate mounts leave the
  volume OFFLINE. Misbinding is worse than waiting for a marker. macOS
  identity is the volume UUID via getattrlist (no DiskArbitration
  dependency); a firmlinked path (e.g. /Users on the Data volume)
  reports "/" as its reachable mount so rel-path math holds while the
  identity stays the Data volume's (the sealed snapshot's UUID churns
  with OS updates).
- **B64 (UI).** Grid identity travels by HASH across every async or
  re-derivation boundary: items refreshes remap focus/selection through
  the image hash (an exif pass filling captureTs re-sorts capture-desc
  mid-session), pointer handlers carry the rendered unit's hash and
  resolve indexes at event time, and the chevron toggles the pair
  hosting that hash at execute time. Index-based identity across the
  applySelection IPC await is how clicking one image toggled another's
  pair. Keyboard/menu verbs stay focus-based — their subject IS the
  focus at perform time.
- **B65 (architecture).** Metrics live in a crate-level
  `photoproof_core::metrics` module: lock-free cumulative StageStat
  counters (the Prometheus model — rates fall out of snapshot diffing;
  no reset method by design), `record()` as the stable seam (histograms
  can grow inside without touching call sites), ingest's
  PipelineMetrics as the first tenant. Logging is the `tracing` facade
  in core with the ONE subscriber installed by the desktop shell
  (env-filtered; quiet info default).

## P6.3 spike decisions (June 2026, session 1 — Apple Silicon Tier-1 floor)

Measured grounds in docs/SPIKE-P6.3.md; throwaway harness in spike-p6.3/.

- **B66 (resolves B55's open item).** Download-manager TLS client:
  **ureq + rustls**. The manager is one serialized blocking worker (B58)
  doing resumable GETs — synchronous fits the pump model, rustls avoids
  the platform-OpenSSL matrix, reqwest would drag a tokio runtime into a
  crate that deliberately has none.
- **B67 (resolves §3.2's serving-shape question).** P2 ships as a TINY
  RUST-CRATE WRAPPER CHILD we own, not the vendored sherpa websocket
  server: the vendored server (v1.13.2) mints finals that DROP text its
  own partials already decoded (reproduced; --reset-encoder irrelevant) —
  CAPTURE mints events from finals, so lost words disqualify. Same
  process boundary (invariant 1.1), same wire contract. Allied recipe
  facts the wrapper must honor: ONNX intra-op threads EXPLICITLY ≥4
  (default 1 falls behind real time), ~0.8 s silence tail before flush,
  silero v5's 64-sample context-prepend, and llama-server gets
  `--reasoning-budget 0` (Gemma 4 E4B thinks otherwise — constrained
  output never reaches content).

- **B68 (founder RAM directive).** Default P1 model: **Gemma 4 E2B QAT
  q4_0** (official Google quant), not E4B — half the footprint
  (4.0/4.3 GB vs 5.3/6.7), double the speed (71 vs 35 tok/s), identical
  50/50 schema validity, and the interactive parse fits §9's 2 s budget
  (1.69 s vs 2.93). E4B remains a config-selectable Tier-2+ option.
  Qwen3.5-2B: no official GGUF; the community export crashes the pinned
  llama.cpp at load — out for v1. Load-on-demand P1 (3.5 s Ready) is the
  sanctioned idle-footprint lever: ~1.7 GB resident without the LLM.

- **B69 (founder, June 2026 — retrieval stays additive).** Machine
  signals are never RETIRED by journal coverage: captions/summaries (S3)
  already always vote at w=0.5; the founder rules that **image_clip (S4)
  participates on every semantic query too**, not only via §5.2's
  activation gate (visual:true or <10 candidates). Own-words identity is
  protected by WEIGHT, not exclusion — S4's always-on weight (and whether
  the gate survives as a latency optimization only) is settled by the
  golden-query eval at M3 build time, where §5.3 already declares ranking
  deliberately revisable. Annotating an image must only ever ADD ways to
  find it.

- **B70 (founder, June 2026 — the name stands).** "Photoproof" is the
  product name for now (Q1 resolved). The `.photoproof.json` sidecar
  suffix and `.photoproof-volume` markers are sanctioned to harden into
  real user data as dogfooding proceeds; if a rename ever comes, it is a
  sidecar-format migration with compatibility reads, not a find/replace.

- **B71 (founder, June 2026 — "projects" are COLLECTIONS).** The
  RETRIEVAL §10 project/intent store is renamed **collections** before
  anything is built (P7.3 had zero code; the rename is free now).
  Founder framing, now canonical: collections are the "tags not folders"
  answer to filesystem organization — evented membership over moving
  files, the natural home for the disparate context the app gathers; the
  app should ENCOURAGE collecting (see BACKLOG "autosuggest
  collections"). Mechanics unchanged: evented membership with history,
  append-only notes, status, the portability file
  (`collections.photoproof.json`) with C2-family union merge, fuzzy name
  resolution in search, context-assembly layer 4. Spec text, table
  names, parser grammar (`"type":"collection"`), and file names all
  rename with it.

## Open questions deliberately left to the founder
- ~~**Q2.** EVENTS §12 journal-semantics questions~~ — **RESOLVED (founder, June 2026)**: (a) sibling-image hashes in shared sidecars accepted; (b) redacted events render as "[redacted]" stubs. Specs approved for implementation as of this date.
- ~~**Q3.** Frontend framework~~ — **RESOLVED (founder, June 2026): Svelte** (Tauri 2 + Svelte 5; lighter runtime in a webview, fits the quiet-UI philosophy).
- **Q4.** M3 gates: sentiment quality evaluation; dedicated-text-embedder benchmark vs. alternatives during dogfooding.
