# PLAN P7.4 — Embedder wiring (pins, in-process ort connector, scheduling)

Status: implementation plan, June 12 2026. Contract for the build lanes —
decisions here are MADE; lanes execute, they do not re-decide. Inputs:
docs/SPIKE-P7-EMBED.md (B73 winners + pins + traps), spec/RUNTIME.md
section 3.3 (in-process ort posture, two embedder instances), RETRIEVAL
section 3 (vec_kind routing), DECISIONS B69/B73.

## Outcome

The app downloads the pinned embedder models on consent, runs them
IN-PROCESS via ort (no new child processes — RUNTIME 3.3 is normative:
"deterministic, fixed-shape, stateless"), the P7.1 embedding passes fill
PPVEC in the background, and hybrid search goes semantic with provenance.
STATUS.md's mock-only retrieval rows flip to built-tested (live flip is
founder dogfood).

## Verified ground truth (do not re-derive; cite when touching)

- **Downloads flatten paths — REAL BUG this packet must fix first.**
  `crates/photoproof-core/src/runtime/download.rs:216,251`:
  `dest = dir.join(f.file_name())` where `file_name()` is the basename
  (`manifest.rs:70`). DFN5B ships `visual/model.onnx` AND
  `textual/model.onnx` (collision), and ort resolves the visual tower's
  ~100 external-data files RELATIVE to model.onnx — the subdirectory
  layout is load-bearing.
- **The embedding pipeline is built and idle.**
  `crates/photoproof-core/src/library/embedding.rs:37` —
  `EmbeddingRig { text: Option<&TE>, clip: Option<&CE>, vectors }`;
  `None` slots leave pass rows pending with zero errors.
  `process_embedding_queue` works under tests; NOTHING in apps/desktop
  schedules it. Search hard-codes the keyword-only rig
  (`crates/photoproof-core/src/search/`).
- **Plans exist for embedders**: `runtime/plan.rs:144` computes
  `clip_embedder`/`text_embedder` from `config.embedder.model` /
  `config.embedder.text.model`; nothing consumes those plans in the
  shell today. In-process means NO SupervisorHost slots — consumption is
  session construction, not spawning.
- **PPVEC sets dimensions from the first vector per space; mismatch is a
  hard error.** EmbeddingGemma = 768, DFN5B = 1024 — vec_kind routing
  per RETRIEVAL 3: `annotation_chunk` + `image_summary` -> text
  embedder; `image_clip` -> CLIP embedder.
- **Connector trait**: `photoproof_connectors::Embedder` +
  `DecodedImage` — core decodes pixels, the connector embeds. Mock in
  `src/mock/embedder.rs` stays the test driver for everything
  deterministic.

## Decisions (made now)

1. **Path-preserving downloads**: dest becomes
   `models_dir/<model_id>/<file.path>` (subdirs created). `file_name()`
   remains for display only. The ASR/LLM entries are flat files, so
   their on-disk layout is unchanged — but audit every consumer that
   joins paths (`supervisors.rs llama_spec/asr_spec`,
   `runtime/launch.rs`, install checks in `download.rs:216` region) and
   point them at `path`, not basename. Existing part-file resume and
   verify logic must keep working with nested dests.
2. **Manifest pins** (SHAs + sizes in docs/SPIKE-P7-EMBED.md; local
   copies under /Users/bornman/spike-p7-embed/models/ for verification
   and for computing the DFN5B external-data enumeration):
   - `embeddinggemma-300m-q8` — NEW entry, role "text-embedder", the
     B73 DEFAULT. Gemma license (`acceptance_required: true`, same
     terms URL as the LLM; a separate model id needs its own acceptance
     record — that is existing behavior, keep it).
   - `qwen3-embedding-0.6b-int8` — pinned as the configured ALTERNATIVE
     (role "text-embedder-alt" or tiers it is offered at — follow the
     existing llm-alt precedent in manifest.rs). Apache-2.0.
   - `ViT-H-14-378-quickgelu__dfn5b` — pinned with the FULL
     external-data enumeration: every file under visual/ and textual/
     except rknpu/armnn variants. Generate the entries by hashing the
     local snapshot (a build-time script or a generated include —
     whichever stays reviewable; the manifest must remain a pure
     compiled literal in spirit: no network, no runtime discovery).
     License: apple-ascl (Immich repo).
   - Config default `embedder.text.model` -> `embeddinggemma-300m-q8`;
     spec/RUNTIME.md section 3 text-embedder sentence gains "(B73:
     EmbeddingGemma-300m default; Qwen3-0.6B alternative)".
   - Tests that assert embedders are unpinned (runtime.rs
     `unpinned_models_surface_as_pending...`) UPDATE to assert the new
     truth (pinned, offered, downloadable); manifest byte-sum tests
     update.
3. **One `OrtEmbedder` connector** (crates/photoproof-connectors, new
   module beside silero.rs, same ort version): parameterized by a model
   recipe enum, not three structs —
   - `TextMeanPooled` (EmbeddingGemma): mean pooling over last hidden
     state; document prompts "task: search result | query: " /
     "title: none | text: " (constants with WHY); tokenizer.json via the
     `tokenizers` crate (new dependency — pin it).
   - `TextLastToken` (Qwen3): last-token pooling, EOS appended,
     instruction prefix on the query side.
   - `ClipImage` + `ClipText` (DFN5B): two sessions, image side takes
     `DecodedImage` already preprocessed BY CORE to 378x378 CHW
     normalized (the preprocess constants from visual/preprocess_cfg
     live in core where decode happens — connector stays pixel-format
     dumb; mirror the spike's clip_bench.py preprocessing exactly).
   - THE TRAPS (spike report, both verified live): onnx-community text
     exports demand `past_key_values.*` inputs — feed zero-length
     caches shaped from session metadata; CLIP textual export is
     batch=1 fixed — loop queries singly.
   - CPU EP only, 4 intra-op threads (spike posture). GPU EP stays
     gated on spike session 2 (RUNTIME line ~513) — leave a device knob
     UNBUILT, just a comment.
   - L2-normalize all outputs (PPVEC cosine assumption; spike scripts
     normalized).
   - Tests: deterministic unit tests for prompt assembly, KV-feed
     shaping, pooling math (tiny hand-built tensors where possible);
     `#[ignore]` real-model tests against the local snapshot paths
     (gate: skip cleanly when files absent) proving load + dims +
     paraphrase-margin sanity, mirroring the spike numbers.
4. **Shell integration — an EmbedderHost, not a supervisor**
   (apps/desktop/src-tauri): owns `Option<Arc<OrtEmbedder>>` per role;
   converges on the runtime plan like apply_supervisor_plan does (same
   2 s converge loop): plan says Run + model installed -> build
   sessions on a background thread (load is seconds; NEVER on the
   command thread); config/plan change -> drop and rebuild. Readiness =
   sessions constructed; surface as `clip_ready`/`text_embedder_ready`
   in RuntimeStatus (additive DTO fields; settings rows show
   running/idle state text). A native load failure marks the host
   degraded-with-error (debug panel visible), never crashes the app —
   RUNTIME 3.3's defense rests on this isolation being honest.
5. **Scheduling** (the missing piece STATUS calls out): the ingest pump
   (`apps/desktop/src-tauri/src/pump.rs`) gains the embedding drain:
   when embedders are ready AND the regular ingest queue is idle (the
   politeness rule — embeddings are the lowest backfill priority, L4
   ordering), call `ensure_embedding_rows` + `process_embedding_queue`
   with a bounded batch; reuse the existing coalesced progress wiring so
   the header background-jobs indicator picks it up for free
   (pass_counters already counts these passes). Capture-live throttling:
   embedding batches PAUSE while `capture_live` is set (same posture as
   downloads — the mic owns the machine).
6. **Search goes hybrid for real**: where the shell builds the Searcher
   rig, replace the hard-coded keyword-only rig with a rig fed from the
   EmbedderHost when ready (query-time text embedding; S4 CLIP query
   embedding per B69 always-votes). Degraded posture unchanged: no
   embedders -> keyword-only, zero behavioral change for today's users.
7. **Out of scope** (recorded, not built): GPU EP, reranker, golden-query
   eval (post-dogfood), embedding progress UI beyond the existing jobs
   indicator, any new settings surface beyond model rows already shown.

## Lanes (sequential, each gated; Opus implementers, default-model review)

- **L1 path-preserving downloads** — decision 1. Files:
  core/runtime/download.rs (+ tests), manifest.rs docs, supervisors.rs,
  launch.rs audit. New tests: nested-path round-trip with two same-basename
  files; resume of a nested part file; existing suites green.
- **L2 pins + config + spec touch** — decision 2. Files: manifest.rs,
  connectors config.rs, spec/RUNTIME.md (one sentence), runtime.rs tests.
  Verification step: recompute every SHA from the local snapshot rather
  than trusting the report (the report is the cross-check).
- **L3 OrtEmbedder connector** — decision 3. Files: connectors (new
  module, Cargo.toml tokenizers dep), core image-preprocess helper next
  to the existing decode pipeline. Heaviest lane; budget accordingly.
- **L4 EmbedderHost + scheduling + search rig** — decisions 4-6. Files:
  src-tauri (new embedders.rs, runtime.rs, pump.rs, dto.rs, state.rs),
  core search rig construction. Frontend: only RuntimeStatus row text if
  trivially additive.
- **L5 e2e + ledger** — an `#[ignore]` end-to-end test (real models:
  ingest 3 corpus images + synthetic notes -> run passes -> hybrid query
  returns the semantically-right image with provenance); run it once on
  this machine and record output in the ledger row; STATUS.md row flips;
  BUILD-LOOP row; FOUNDER-CHECKLIST: consent size grows (~4.6 GB more at
  tier 1), embeddinggemma license acceptance appears, backfill
  expectations (2.96 s/image on laptop CPU - idle-hours or desktop).

Every lane: the standing gates (fmt, clippy zero, workspace tests with
only s02_2 red, svelte-check/vitest if frontend touched), commit per lane,
no push until the final gate.
