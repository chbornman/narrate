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
- **R6.** Projects: separate store, evented membership (interval rows), portable via `projects.photoproof.json` with union-merge; project metadata is the system's one named last-writer-wins exception.
- **R7.** Sentiment stored (int −2..+2) but consumed by nothing until the M3 quality evaluation passes.
- **R8.** Context assembler: budget caps 40/15/10/10/25 % (selection/recency/folder/projects/retrieval), unspent rolls forward; emits layer-tagged blocks, never final prompts.

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

## Open questions deliberately left to the founder

- **Q1.** Final product name ("Photoproof" is a placeholder; sidecar suffix hardens into user data at M1 ship — decide before then).
- ~~**Q2.** EVENTS §12 journal-semantics questions~~ — **RESOLVED (founder, June 2026)**: (a) sibling-image hashes in shared sidecars accepted; (b) redacted events render as "[redacted]" stubs. Specs approved for implementation as of this date.
- ~~**Q3.** Frontend framework~~ — **RESOLVED (founder, June 2026): Svelte** (Tauri 2 + Svelte 5; lighter runtime in a webview, fits the quiet-UI philosophy).
- **Q4.** M3 gates: sentiment quality evaluation; dedicated-text-embedder benchmark vs. alternatives during dogfooding.
