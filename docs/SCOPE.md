# Photoproof
### The digital contact sheet with a grease pencil
*Scope & Architecture Document — Draft 4, June 2026*
*(The name stands for now - DECISIONS B70. Formerly "Darkroom Notes," briefly "Daido"; the repo is `narrate` for historical reasons.)*

> **Normative specs live in `spec/`** (EVENTS, SIDECARS, LIBRARY, CAPTURE, RETRIEVAL, RUNTIME, UI; decisions in `spec/DECISIONS.md`). This document is the vision and architecture overview; where it and a spec disagree, the spec wins.

---

## The Problem

A photographer with a serious practice accumulates tens of thousands of images, and with them, tens of thousands of judgments: why this frame and not that one, what a series is reaching for, which images keep pulling them back. None of that survives. Catalog tools store ratings and keywords — the thinnest possible residue of creative thinking. The actual reasoning lives in the photographer's head, and across years it decays, contradicts itself, and gets lost. When it's time to assemble a collection, sequence a show, or decide what to print, the photographer is reconstructing their own taste from scratch.

AI photo search doesn't solve this. Lightroom, Excire, Peakto, and Apple Photos can all find "dog on beach" now. Semantic image search is a commodity. What no tool captures is *intent over time*: what the photographer thought about an image at the cull, at the edit, six months later, and how that relationship changed.

## The Product

**The digital contact sheet with a grease pencil.** For a century, photographers worked their images on contact sheets: loupe in one hand, red grease pencil in the other — circling frames, slashing rejects, scrawling notes in the margins, and *talking through the work* with themselves or an editor. That practice produced exactly the artifact this product resurrects: a physical record of judgment, layered over time, bound to the frames themselves. Magnum's marked-up contact sheets are studied today precisely because they preserve the *thinking*, not just the pictures. Digital workflows kept the images and threw away the marginalia.

Photoproof is a desktop application that restores that practice. You browse your library, select an image or a group, and you work the sheet: **talk** (out loud or typed) and **mark** — a red grease-pencil tool for circling, striking through, underlining a gesture in the frame, drawing the crop you're imagining. Every utterance and every stroke is transcribed/captured, timestamped, and bound to the image as an append-only journal. Over time the library stops being a pile of files and becomes a longitudinal record of a creative practice.

Retrieval is conversational and intent-aware. Not "find photos with fog" but "pull up the images I was considering for that quieter, more melancholic series" — answered from the photographer's own words about their own work, accumulated across months of sessions.

The core loop: **browse → select → speak & mark → it remembers.** Everything else is in service of that loop staying frictionless.

### The grease pencil (visual annotation layer)

Markup is non-destructive vector overlay, never pixels: strokes are recorded as timestamped vector paths (pressure-aware where hardware allows) referenced to image coordinates, rendered live over the photo, and toggleable like a tracing-paper sheet. Because strokes are events in the same append-only log as speech, a circle drawn in 2026 and a strike-through added in 2028 coexist as layers of the image's history — you can scrub back through your own markings the way you scrub through your words. Marks and words made in the same moment are linked: say "this gesture is the whole picture" while circling a hand, and the stroke and the utterance share a timestamp and session, so the journal preserves *what you meant by the mark*.

### Positioning: not an AI product

Photoproof is not marketed as an AI tool, because it isn't one in any sense the user experiences. The category is saturated with AI that does things *to* your photos — scores them, culls them, tags them, judges them — and the fine-art and long-project audience this serves is precisely the crowd most allergic to a machine grading their art. Here the loop is entirely human: looking, thinking, talking, marking — the same loop as a lightbox and a loupe. The AI is infrastructure for memory, not a creative authority: speech-to-text so talking is frictionless, embeddings and retrieval so your own words come back when you need them, a local model quietly summarizing in the background. It is exactly as relevant to the user as the B-tree inside Lightroom's catalog, and the marketing treats it that way. The pitch is "a journal for your photographs that you can talk to." The technology disappears; the practice remains.

### What it is not

It is not an editor (edits stay in Capture One/darktable/Lightroom), not a DAM replacement, and not another semantic search tool. It is the journal layer the entire ecosystem is missing — and pointedly not another AI opinion about your photographs. Every competitor's AI generates opinions about your photos; Photoproof preserves *yours*.

## The Differentiator

The moat is the **annotation log**: an append-only, timestamped, event-sourced record of everything the photographer has said about — and drawn on — every image. Because entries are never overwritten — only appended — the system can answer temporal questions no other tool can touch: images you've warmed up to, images whose meaning shifted after a life event, the moment a series crystallized in your thinking. Sentiment and taste are treated as trajectories, not snapshots.

This also dictates the prime architectural directive: **the annotation log's integrity and portability outrank every other concern.** A photographer trusting twenty years of creative reflection to this tool must be able to walk away with all of it in open formats at any time.

## Business Model (provisional)

Free tier: local capture, transcription, indexing, and search. Entirely on-device, genuinely private, no account required.

Paid tier: the conversational creative partner — a frontier cloud model (Claude) that talks back: asks questions during culls, surfaces patterns across the log, helps sequence collections. The API cost structurally justifies the subscription, and the privacy story stays honest because the free tier never touches the network and the paid tier is explicit, opt-in, per-conversation.

Open risk worth naming: the one-way recorder must be independently compelling, and "photographers will talk out loud while culling" is untested. Mitigation: typed quick-notes are a first-class input from day one, equal to voice in the data model. Voice is an input method, not the product.

---

# Architecture

## Design Principles

**Local-first, cloud-optional.** Every capability ships with a local backend. Cloud backends are additive, never required.

**Modular connectors behind trait boundaries.** Every external capability — transcription, embedding, language model, vision, storage — is a Rust trait with swappable implementations. Local model today, cloud API tomorrow, different local model next year, without touching application logic.

**The OpenAI-compatible API as the seam.** llama.cpp's server, vLLM, SGLang, Ollama, and most cloud providers all speak it. By making the LLM connector target that protocol, "local vs. cloud" becomes a config change (base URL + model name), not a code change. The Anthropic API gets its own thin adapter behind the same trait.

**SQLite is the index; sidecars are the truth.** The database is a rebuildable cache. Canonical annotation data is continuously mirrored to per-image sidecar files (JSON, with optional XMP export) that live beside the originals and survive the app's death.

**Content-addressed identity.** Images are identified by content hash (BLAKE3), never by path. Photographers reorganize constantly; metadata must follow the pixels, not the filename.

## System Overview

```
┌─────────────────────────────────────────────────────┐
│                  Tauri Desktop App                   │
│        (image browser · session view · search)       │
├─────────────────────────────────────────────────────┤
│                   Rust Core (lib)                    │
│                                                      │
│  Ingest Pipeline      Annotation Engine    Retrieval │
│  hash → thumb →       append-only event    hybrid    │
│  embed → caption      log + sidecar sync   search    │
│                                                      │
│  ── Connector Traits ─────────────────────────────── │
│  Transcriber │ Embedder │ LanguageModel │ VectorStore│
├──────────────┴──────────┴───────────────┴───────────┤
│  Local impls (default)        Cloud impls (later)    │
│  Nemotron 3.5 ASR             hosted ASR             │
│  CLIP + text embedders        embedding APIs         │
│  Gemma 4 / Qwen 3.6 via       Anthropic / OpenAI-    │
│  llama.cpp server             compatible endpoints   │
│  SQLite (+ in-mem vectors)    pgvector / managed DB  │
└──────────────────────────────────────────────────────┘
```

## Connector Traits (the modularity contract)

Sketch of the boundaries — exact signatures will evolve:

```rust
trait Transcriber {
    fn stream(&self, audio: AudioStream) -> impl Stream<Item = TranscriptSegment>;
}

trait Embedder {
    fn embed_image(&self, img: &DecodedImage) -> Result<Embedding>;
    fn embed_text(&self, text: &str) -> Result<Embedding>;
    fn dimensions(&self) -> usize;
    fn model_id(&self) -> &str;   // stored with every vector — see migrations
}

trait LanguageModel {
    fn complete(&self, req: ChatRequest) -> Result<ChatResponse>;
    fn caption_image(&self, img: &DecodedImage, prompt: &str) -> Result<String>;
}

trait VectorStore {
    fn upsert(&self, id: ContentHash, kind: VecKind, v: &Embedding) -> Result<()>;
    fn search(&self, query: &Embedding, kind: VecKind, k: usize) -> Result<Vec<Hit>>;
}
```

Every embedding row records `model_id` and `dimensions`. Swapping embedding models later means a background re-index job, not a migration crisis — old vectors remain queryable until the new index completes.

## Model Selection (June 2026)

| Role | Local default | Notes |
|---|---|---|
| Streaming ASR | **Nemotron 3.5 ASR** (600M, nemotron-3.5-asr-streaming-0.6b) | Sub-100ms end-of-utterance latency, 40 language-locales, runs on laptop-class hardware. English-only Nemotron-Speech-Streaming-En-0.6b as a lighter fallback. |
| LLM + vision (summaries, captions, tagging) | **Gemma 4 E4B** as the floor; **Gemma 4 26B MoE (A4B)** or **Qwen 3.6-35B-A3B** as the quality tier | All natively multimodal — one model handles both image description and transcript summarization. Gemma 4 is Apache 2.0; E4B runs on modest hardware, 26B MoE gives near-31B quality at ~4B active params. Qwen 3.6-27B/35B-A3B are the open-weight Qwen options. **Qwen 3.7 is API-only (closed weights) as of June 2026** — it slots in later as a *cloud* connector if its open weights ship, exactly the kind of swap the trait architecture exists for. |
| Visual embeddings | OpenCLIP ViT-H-14-378 / DFN5B (Immich's top preset, 1024-dim) | Image vectors + short visual queries only. |
| Text embeddings | Small dedicated text-embedding model (Qwen3-Embedding-0.6B-class) | The primary signal: annotation text. CLIP text towers cap at 77 tokens and cannot carry 512-token annotation chunks — hence two embedders (spec/RUNTIME.md §3.3). |
| Premium conversational tier | Claude (Anthropic API) | Cloud connector behind the same `LanguageModel` trait. |

Hardware floor for the free tier: ~8–12 GB VRAM or Apple Silicon unified memory (E4B + 600M ASR + embedder). The developer's RTX 5080 is the quality-tier dev box, not the customer baseline.

Serving strategy: bundle or manage a llama.cpp server child process exposing the OpenAI-compatible endpoint locally. The app never links inference directly — even "local" goes through the protocol seam, so local↔cloud is symmetric by construction.

## Data Model

**Annotation events (the heart).** Append-only log; rows are never updated or deleted, with two precisely-scoped exceptions: *retraction* folds an event out of view (a tombstone — content preserved) and *redaction* physically scrubs content while preserving structure. **The normative schema lives in `spec/EVENTS.md`**; the shape:

```
annotation_events
  id            ulid          -- minted at capture onset; log order = ULID order
  session_id    ulid
  ts            timestamp     -- UTC; testimony, never ordering
  source        voice | typed | pencil | system
  kind          remark | rating | stroke | revision | retraction | redaction
  text          utterance / note (remark, revision)
  payload       kind-specific JSON (stroke geometry, rating value, ASR confidence)
  target_event  fk — what a revision/retraction/redaction modifies
  linked_event  fk — stroke↔utterance link, carried by the later event
  redacted_by   fk — present iff content has been scrubbed

event_targets (event_id, image_hash, position)   -- 0..N images per event
vectors (event_id, vec_kind, model_id, …)        -- derived, many per event,
                                                 -- never in events or sidecars
```

Strokes are first-class events, not a separate system: same log, same sidecar mirror, same temporal queries. A stroke linked to an utterance inherits searchability through the utterance's text and embedding ("the one where I circled the hand"). Rendering is a vector overlay in normalized image coordinates so marks survive any preview resolution, and the overlay toggles like a sheet of tracing paper — including time-scrubbing through strokes by timestamp.

**Derived views (rebuildable, disposable):** per-image rolling summaries, per-image sentiment trajectory snapshots, session summaries. Generated by the local LLM; regenerated freely when models improve. Never the source of truth.

**Collections / intent memory (separate store).** "That collection I've been thinking about" is not per-image data. Collections get their own table: name, description, evolving notes, member images, status. Conflating this with image annotations would corrupt both; keeping it separate makes "pull up images I was considering for X" a join, not a guess.

**Sidecar mirror.** A background writer keeps `IMG_4471.arw.photoproof.json` (and optional XMP keyword export for Lightroom/C1 interop) in sync with the event log. Full library export to open formats is a permanent, prominent feature.

**Images table:** hash, current known paths, EXIF subset (date, camera, lens, ISO/aperture/shutter), thumbnail ref, embedding refs, caption.

## Library Sync & Sidecar Placement

**Sidecars live adjacent to the image, by convention.** Photographers already understand this from XMP — darktable and Lightroom drop `.xmp` beside the RAW, and adjacency is the only placement that survives what photographers actually do: copying shoot folders to archive drives, handing files to clients, reorganizing by year. `IMG_4471.arw.photoproof.json` travels with its image. Each sidecar embeds the content hash so a separated sidecar can always be re-matched to its pixels. For read-only or unwritable volumes (archive drives, network shares), a centralized overflow store inside app data holds the sidecar instead; since identity is the hash, both routes converge in the database.

**Watched roots, not "currently open directory."** The user registers one or more root folders ("Active Work," "2019–2026 Archive"); the app owns awareness of everything beneath them. Two mechanisms keep the index honest: a live filesystem watcher (`notify` crate) while the app runs, and a reconciliation scan at launch and on schedule for changes that happened while it was closed. Because identity is content-hash, a moved or renamed file is a *relink* — new path, same hash, annotations intact — never a re-ingest.

**Offline volumes are first-class.** Photographers unplug archive drives constantly. A file on an offline volume is *offline*, not deleted: annotations persist, search still surfaces it, the UI shows the cached thumbnail with a disconnected badge. Volumes are tracked as their own entity with online/offline state.

## Context Model: Write Scope vs. Read Scope

The five natural context levels (single image → recent images → current folder → multiple folders/collections → whole library) must not behave uniformly. The design splits them:

**Write scope — where words land — is narrow and explicit.** Annotations attach only to the current selection: one image, a multi-select, or "this session" generally. Speech must never silently annotate a whole folder; an ambiguous write scope pollutes the journal, and the journal is the product. The UI permanently displays the active write scope ("speaking about: 3 images").

**Read scope — what the model knows — is layered, concentric, and budget-driven:**

1. **Selection** — full annotation history and transcripts for the selected images.
2. **Recency trail** — the last ~10–20 images viewed this session (summaries), so "the one before this" resolves naturally.
3. **Current folder** — for photographers a folder is usually a *shoot*, semantically meaningful; per-folder rollup summaries are maintained as a derived view.
4. **Active collections** — notes and membership from the intent-memory store, independent of folder structure.
5. **Whole library** — never loaded wholesale; reached only through hybrid retrieval.

Context assembly fills a token budget in that order: selection gets full text, recency gets summaries, folder gets its rollup, collections contribute their notes, and the library contributes only what search pulls in.

## Retrieval

Hybrid search with simple rank fusion across four signals:

1. **Annotation-text embeddings** — the primary signal. "Same melancholic feeling" matches against the photographer's *own words*, which is where feeling actually lives. This is the product's retrieval identity.
2. **FTS5 keyword search** over transcripts, summaries, and tags (built into SQLite, zero infra).
3. **Image embeddings** — fallback for un-annotated images and pure visual similarity.
4. **Structured filters** — date ranges, collection membership, EXIF, rating events.

At 50k images, vectors don't need a database: 50k × 768 floats ≈ 150 MB, and brute-force cosine over a memory-mapped matrix is single-digit milliseconds in Rust. `VectorStore`'s first implementation is a flat file + SQLite metadata. If libraries grow past ~500k, swap in sqlite-vec or usearch behind the same trait. Do not over-engineer this on day one.

Temporal queries ("images I've come around on") run over the event log directly: per-image sentiment scored per event by the local LLM at ingest, trajectory queries become SQL.

## Ingest Pipeline

Queue-based, resumable, idempotent — a 50k-image first run is hours of GPU work and *will* be interrupted:

hash → RAW decode (rawler, with libraw FFI fallback for exotic formats) → thumbnail/preview cache → image embedding → VLM caption & tag pass → index.

RAW decoding in Rust is real, unglamorous work; budget for it. Caption/tag passes are versioned by model_id so they can be re-run incrementally when models upgrade. GPU work runs at low priority and pauses when a live annotation session needs the ASR/LLM — the user's editing apps and this tool will contend for VRAM, so the floor config must fit comfortably alongside Capture One.

## Session Flow (one-way capture, v1)

1. User opens app, browses; selection state defines annotation scope (one image, several, or session-level).
2. Mic toggle starts Nemotron streaming; segments land as events in real time, bound to current selection. Typed notes enter the identical path. With a single image open, the grease pencil is available: strokes land as events bound to that image, linked to any utterance in progress.
3. On session close (or rolling, in background): local LLM generates session summary, updates per-image rolling summaries, scores sentiment, embeds new annotation text, syncs sidecars.
4. No model talkback in v1. The conversational partner is the paid tier and a later milestone.

## Stack Summary

Tauri 2 shell · Rust core · SQLite (WAL) + FTS5 · flat-file vector store · BLAKE3 hashing · rawler for RAW · Nemotron 3.5 ASR sidecar process · llama.cpp server (OpenAI-compatible) hosting Gemma 4 / Qwen 3.6 · JSON/XMP sidecars · canvas-based vector markup overlay · all connectors behind traits with config-driven backend selection.

## Roadmap Sketch

**M1 — Spine.** Ingest pipeline, content-addressed library, browser UI, typed notes → event log → sidecars, FTS5 search. (Proves the journal without any AI risk.)
**M2 — The Sheet.** Nemotron streaming voice capture, selection-scoped sessions, the grease pencil markup layer with stroke↔utterance linking, session summaries via local LLM. (This milestone *is* the contact-sheet experience.)
**M3 — Retrieval.** Embedding pipeline (annotations + images), hybrid search, collections store, natural-language query.
**M4 — Time.** Sentiment trajectories, "changed my mind" queries, timeline view per image, stroke time-scrubbing.
**M5 — Partner.** Cloud connector (Claude), two-way conversation, the premium tier.

## Open Questions & Risks

1. Do photographers talk while culling? Validate with real users before M2 hardens; typed-first is the hedge — and the grease pencil is a second hedge, since marking is an even older habit than talking.
2. Editing-app awareness (knowing what's open in C1/darktable) — explicitly deferred; revisit post-M3.
3. Sentiment scoring quality from small local models — needs evaluation; trajectories built on noisy scores are worse than none.
4. Markup input quality with mouse vs. pen tablet — the grease pencil wants a Wacom/pen display; mouse strokes must still feel acceptable. Pressure support is progressive enhancement.
5. Name and positioning — currently **Photoproof** (a working placeholder; trademark/domain availability unverified, and a final name decision is deliberately deferred). Whether the free tier cannibalizes the paid tier — revisit after M3 dogfooding on the founder's own ~50k-image library. Positioning discipline: no "AI-powered" language anywhere in the product or marketing; the word appears only in the privacy explanation of what runs locally.
