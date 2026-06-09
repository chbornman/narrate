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

## Open questions deliberately left to the founder

- **Q1.** Final product name ("Photoproof" is a placeholder; sidecar suffix hardens into user data at M1 ship — decide before then).
- **Q2.** EVENTS §12 carries two founder-level questions: (a) multi-target events expose sibling-image *hashes* in shared sidecars — recommendation: accept (non-reversible, and stripping breaks dedupe); (b) do redacted events show as "[redacted]" stubs in the journal panel, or vanish without trace? Both supported; privacy-philosophy call.
- **Q3.** Frontend framework (Svelte vs. React) — low stakes, decide at workspace creation.
- **Q4.** M3 gates: sentiment quality evaluation; dedicated-text-embedder benchmark vs. alternatives during dogfooding.
