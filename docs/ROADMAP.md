# ROADMAP - what is left to build

The planned-but-not-yet-built census, organized by milestone. Derived from
`docs/STATUS.md` (the capability ledger), `docs/BACKLOG.md` (open items),
`docs/PLAN-PERF.md` (the perf plan), `docs/RUNTIME-MATRIX.md` (the EP/accelerator
wiring), and the `spec/` milestone tags. Where this disagrees with a spec, the spec
wins. This is a navigational view, not a commitment of order - the build loop pulls
items into packets.

Generated June 14 2026. Cross-checked against the specs: no surprise unbuilt
features, no major doc drift.

## The spine: where we are

M1 (library) and M2a (pencil) are essentially shipped; M2b (voice) runs end to end
and is in tuning; **M3 (retrieval + collections) is the big open milestone** - the
real ML wiring (LLM query-parse, caption + summary generation, the GPU execution
providers) all lives here. M4 (time) and M5 (partner) are seats reserved. The Tier-0
floor (typed notes + grease pencil + FTS5) is always a complete product underneath.

---

## M1 - library (mostly shipped; remaining tails)

- At-idle WAL checkpoint scheduling (shutdown path built; idle caller missing) - `STATUS` partial, `spec/EVENTS §5.1`
- User-facing backup guidance + "export includes overflow + sessions" messaging - `STATUS` not-built, `spec/SIDECARS §8.3`
- Overflow-only count in the export UI - `STATUS` not-built
- APFS case-only rename relinking (the known-red `s02_2`) - awaiting case-sensitivity ruling
- Cloud-sync root advisory (Dropbox/OneDrive/iCloud one-time warning) - detection exists, UI does not
- CI pipeline (GitHub Actions standing gate + OS-matrix sidecar byte-compare + nightly full-scale)
- Real-hardware budget checks (grid 60 fps, search <100 ms, 50k-RAW real ingest) - `FOUNDER-CHECKLIST`

## M1.5 - deep image decode (scheduled concept)

- Full RAW decode 1:1 pass (rawler demosaic, on-demand develop-to-cache) - PLAN written `docs/PLAN-RAW-DECODE.md`
- HEIC/HEIF 1:1 previews (libheif) - HEIC ingests today, preview deferred to this pass
- Preview-policy settings (build/keep toggles, LrC-style expiration)
- Progressive import tails: pre-identity cards, building-shimmer, low-res EXIF-IFD1 tier (live counts already landed)

## M2a - pencil (shipped; conditional polish)

- One-euro live-stroke smoothing - add only if real-pen dogfood shows wobble (`spec/CAPTURE §8.3` MAY)
- Pencil pressure/feel verification on real hardware (Wacom Windows-Ink, Linux stylus)

## M2b - voice (runs end to end; tuning + UI)

- Voice endpoint tuning: ASR tail-truncation fix (B74, the 560 ms lookahead pin-swap), rule2/merge-policy feel
- Audiobook WER stress run on the founder machine (scorer landed; needs model + corpus)
- Settings Microphone section: device picker, input level meter, enable checkbox (stub today)
- Mic silence watchdog (device armed but delivering no frames)
- Nemotron 3.5 upgrade watch (trigger: sherpa-onnx Rust crate ships 3.5 support)
- Recorded, not designed (K17): voice-command retraction ("strike that"), audio-retention opt-in

## M3 - retrieval + collections (THE big milestone)

The ML brain lights up here. Grouped by theme.

### ML generation passes (the unbuilt brain)
- **LLM query-parse wiring** - schema + tests exist (mock); wire the real `llama-server` slot (today `None`)
- **LLM image captioning** - `PassName::Caption` is a registered stub with no runner; additive retrieval fuel
- **Derived summary generation** - per-image rolling / per-folder / session; storage + FTS + S3 search all wired, GENERATION is not
  - + the June 14 decision: surface summaries as VISIBLE "system" journal entries, deletable (`BACKLOG` June 14 thread)
- **Sentiment scoring writer** - storage exists, no scorer; gates M4 trajectory queries
- **Reranker stage 3b** - seam + mock trait exist, not wired into fusion; default-OFF, awaiting the §12 eval
- **Retrieval eval harness** - built and unit-tested; awaiting the founder's real golden query set to gate weights

### Execution providers (the GPU wiring) - see `docs/RUNTIME-MATRIX.md`
- FP16 single-file CLIP export + CoreML - **VALIDATED June 14: 8.77x over CPU, ship-with-FP16** (`docs/SPIKE-COREML.md`)
- CoreML code wiring (cache dir + fp16 model spec) - **LANDED June 14** (`ort_embedder.rs`/`model_specs.rs`); env-knob path now compiles once, fp16 id buildable
- CoreML CLIP - FLIPPED on the M1 Pro dev machine (eval held). Ship-to-users (founder/infra): host the fp16 model + manifest entry (SHAs in spike doc); graduate the env knob to a config field
- Text-embed on CoreML - REJECTED (spiked, measured 0.48-0.64x slower; the transformer graph does not partition to the ANE). Stays int8/CPU - its best path. `docs/SPIKE-COREML-TEXT.md`
- CUDA EP for the `ort` embedders (Ryzen/5080) - CLIP + maybe text-embed (CUDA takes the whole graph, unlike CoreML); measure on the 5080
- CUDA EP for the `ort` embedders (the NVIDIA "Margo" desktop)
- DirectML EP option (Windows GPUs without CUDA) - not yet evaluated
- Supervisor auto-detect: extend llama.cpp's hardware-pick + graceful fallback to the embedders
- Per-model capture-pause relaxation once embedders are on GPU (`BACKLOG` June 14 thread)

### Search / UI
- NL-parse chip rendering + collection-name resolution (depends on the parse wiring)
- Context assembler (the 5-layer read-scope feeding summaries + the partner)
- Vector-search page-cache prewarm (exists, never called in the shipped app)
- Search-as-scope contextual sidebar - M3 design decision (collection-view context vs full-canvas)
- Tuning made user-visible (weights / RRF beta / endpoint rules as configurable, not invisible constants)

### Collections
- Collection-note composer UI (storage/merge landed; the composer is missing)
- Collection-level rollups from member notes (FUEL tier invisible; NUDGE tier pending a founder call)
- Autosuggest collections (quietly, from co-annotation / repeated phrases / time+folder / repeated queries)
- Group-by-volume in the Folders tab (deferred from the roots design round)

### Journal (June 14 founder thread)
- Type/source chips on journal entries (drawing / voice / written / system-summary)
- Visible, deletable "system" summaries (the K14 bend, above)

### Visualization (M3+)
- Topic-graph v3: wire the real Gemma connector into the `suggest_topics_llm` seam
- Full-library LOD-threshold calibration against a real scale spike

## M4 - time

- Look bottom-edge stroke time-scrubber (seat reserved)
- Journal timeline rendering upgrade
- Library-wide event timeline (sessions as spans, events as marks - a query + render problem)
- Sentiment trajectory queries + trajectories as an alternate grid lens (depends on the sentiment writer)
- Compare module (4th view mode) - architecture ready on the `viewMode` axis; design round on note-scope / 2-up vs N-up / persist vs gesture
- Heatmap x graph synthesis ("hot topics" + "neglected clusters") - once both primitives land
- Stroke-aware retrieval: gesture-semantic intent (circle/X/arrow) + region-conditioned CLIP (the circled crop)

## M5 - partner (cloud, reserved)

- Right-edge dockable partner panel (summon key, Tab lights-out, per-conversation consent)
- Anthropic cloud LLM adapter (ChatRequest -> Messages API; config parses, no impl yet)
- OS-keychain resolution (macOS Keychain / Windows Credential Manager / Linux Secret Service)

## Performance (P-series, `docs/PLAN-PERF.md`)

Landed: P1 preview resize (3.66x), P3 graph Worker, P4 PPG demosaic. Rejected: P5
(CLIP-preprocess parity failed). Remaining:
- P2 CoreML EP (the embedding bottleneck) - see the EP wiring under M3
- P6 WebGL graph render (gated: only if the Worker alone misses smoothness; Sonoma/Safari 17+)
- P7 off-thread thumbnail decode (optional; only if scroll-decode jank is measured)
- P8 ingest pass pipelining (deferred; preview->embed is a hard dependency)
- P9 USearch HNSW (scale-triggered; brute-force is correct under ~100k images)

## Interop / export

- Tag generation + export INTO image files (writes real IPTC/XMP metadata) - opt-in, warned; the June 14 posture change (`BACKLOG`)
- XMP keyword export to a sidecar for Lightroom/C1 (post-M3, `spec/SIDECARS §14`)
- Foreign edit-sidecar reading (portable crop/orientation/rating from Adobe/darktable) - pragmatic-middle design round
- First-class export-folder review path (done work as exported JPEG/TIFF with the edit baked in)

## Platform / deployment

- Cross-platform launch: macOS + Windows + Linux (`memory: deployment-accelerator-plan`)
- Per-platform accelerators: CoreML (Mac), CUDA (NVIDIA), DirectML (Windows), Vulkan (later, needs a non-`ort` runtime)
- Model-landscape survey (recurring quarterly seam-by-seam SOTA check)

## Cross-cutting / quality

- Full metrics suite across every pipeline stage (when feature-complete)
- Em-dash CI grep-gate (the sweep landed; the guard against creep did not)
- Stronger storage/backup messaging beyond the welcome card

---

The normative truth is in `spec/`; the live build state is in `docs/STATUS.md`; the
founder-quoted rationale for each item is in `docs/BACKLOG.md`. This file is the map.
