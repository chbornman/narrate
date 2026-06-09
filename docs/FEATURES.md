# Photoproof — Feature Set

The complete v1-era feature inventory, milestone-tagged. This is the contract
the specs in `spec/` elaborate. UI philosophy throughout: **the main flow is
quiet** — browse, look, talk, mark. Capture happens ambiently; the user never
watches notes being made and never edits metadata visually. The only capture
feedback is a small persistent indicator. Development builds get a debug side
panel; release builds do not contain it.

## Library

- Watched root folders: register/remove roots; everything beneath is indexed automatically `[M1]`
- Live filesystem watcher while running + reconciliation scan at launch and on schedule `[M1]`
- Content-addressed identity (BLAKE3): moves/renames relink automatically; annotations follow the pixels `[M1]`
- Resumable, idempotent ingest: 50k+ images, interruption-safe, restart-safe `[M1]`
- Fast first run via embedded RAW preview extraction; full RAW decode (rawler) as a background backfill pass `[M1 / M1.5]`
- EXIF subset capture (date, camera, lens, exposure) — read-only; the app never writes file metadata `[M1]`
- Thumbnail + preview cache, display-oriented (EXIF rotation applied), sRGB `[M1]`
- Offline volumes are first-class: disconnected badge, cached thumbnails, journal intact and searchable `[M1]`
- Byte-identical duplicates resolve to one image with multiple known locations `[M1]`
- RAW+JPEG pairs are two images in v1 (no stacking; pairing heuristic is a recorded future feature)

## Capture — the journal

- Typed quick notes bound to the current selection `[M1]`
- Multi-image annotation: one remark targeting all N selected images `[M1]`
- Session-level remarks (no image target) `[M1]`
- Keyboard ratings during culling — journal events, never file metadata `[M1]`
- Voice capture: mic toggle, streaming local ASR; utterances bind to the selection snapshot at utterance start `[M2b]`
- Grease pencil on the single-image view: red strokes, pressure-aware where hardware allows, one event per stroke `[M2a]`
- Stroke↔utterance linking when made in the same moment `[M2b]`
- Append-only event log: nothing is ever silently edited or lost `[M1]`
- Retraction ("strike that" — hidden but preserved) and true redaction (content scrubbed from DB, sidecars, indexes) `[M1]`
- Transcript correction: revision events; corrected text is what's shown and indexed, original stays in the log `[M2b]`
- Sessions are automatic and idle-segmented; there is no manual session management `[M1]`

## Main-flow UI (quiet by design)

- Three surfaces: thumbnail grid → single-image view → search. That is the whole app. `[M1]`
- Capture is ambient: no live transcript pane, no note-composer chrome beyond a minimal input, no metadata editing UI anywhere `[M1]`
- Persistent small write-scope indicator ("● 3 images") that pulses as notes, strokes, and metadata updates land `[M1]`
- Keyboard-first culling navigation `[M1]`
- Grease-pencil overlay toggle — tracing paper on/off `[M2a]`
- On-demand journal panel (per image / per session): verbatim history, correct, retract, redact. Closed by default; never part of the main chrome `[M2a]`

## Persistence & portability

- `.photoproof.json` sidecar beside every annotated image, written within seconds of capture `[M1]`
- Overflow store (identical format) for read-only/unwritable volumes `[M1]`
- SQLite is a rebuildable index; full rebuild-from-sidecars command `[M1]`
- Merge = union by event id: restored backups, second machines, and re-found sidecars simply merge; redactions always win `[M1]`
- One-click full library export: complete sidecar set + manifest (identical to rebuild input) `[M1]`
- XMP keyword export for Lightroom/Capture One interop `[post-M3]`

## Search & retrieval

- Instant keyword search (FTS5) over notes and transcripts, search-as-you-type `[M1]`
- Structured filters: date, camera/lens, folder, rating `[M1]`; project membership `[M3]`
- Semantic search over the photographer's own words (annotation embeddings primary; image embeddings as fallback for the un-annotated) `[M3]`
- Natural-language queries parsed locally into filters + semantic search `[M3]`
- Every result shows *why* it matched: the user's own quote or mark, dated, with session context `[M3]`
- Projects / intent memory: named collections with evolving notes and membership `[M3]`
- Temporal queries: sentiment trajectories, "images I've come around on" `[M4]`
- Per-image timeline and stroke time-scrubbing `[M4]`

## Invisible AI infrastructure

- Small-models-first defaults: Nemotron 0.6B streaming ASR for STT; the larger OpenCLIP preset from Immich's supported set (ViT-H-14-378 / DFN5B, 1024-dim) for visual embeddings; a small dedicated text-embedding model (Qwen3-Embedding-0.6B-class) for annotation text — the primary retrieval signal (CLIP text towers cap at 77 tokens and can't carry it); a small modern LLM (Gemma 4 E4B-class or a small Qwen 3.6 variant) for summaries/parsing. Larger models are optional upgrades behind the same traits, never the baseline `[M2b/M3]`
- Future consideration (recorded, not designed): fine-tuning a small LLM for the app's specific tasks (summarization, sentiment, query parsing)
- Managed local model runtime: llama.cpp child process behind the OpenAI-compatible seam; the app never links inference `[M2b]`
- Managed local streaming-ASR process `[M2b]`
- Model download manager: resumable, hardware-tier aware `[M2b]`
- Hardware detection with a graceful degraded mode — typed notes + pencil + FTS work on any machine, no models required `[M1 by construction]`
- Background summaries and sentiment scoring as **retrieval fuel only** — never rendered as prose, scores, or tags in the user-facing product `[M2b/M3]`
- VRAM-polite scheduling: background passes yield to live sessions and the user's editing apps `[M3]`

## Partner tier (paid) `[M5]`

- Opt-in cloud conversation (Claude) grounded in the journal
- Explicit, per-conversation consent; the free tier never touches the network

## Development-only

- Debug side panel, compile-time feature flag, absent from release builds: live event feed, write-scope and session state, ASR segment timing and confidence, ingest queue depth and pass status, sidecar-writer activity, model-runtime health, raw retrieval scores `[M1, grows with each milestone]`

## Explicit non-features

- No image editing and no edit intent — editing stays in Capture One / darktable / Lightroom
- No metadata editing UI; EXIF is read-only, ratings live in the journal, the app never writes into image files
- No visible AI prose, AI scores, or auto-tags anywhere in the user-facing product
- No accounts and no network access in the free tier
- No manual session or sidecar management — both are automatic
