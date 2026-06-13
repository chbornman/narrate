# PLAN — Full RAW decode (the `full-raw-decode` pass, M1.5)

Status: research + implementation plan, June 12 2026. Decisions here are
PROPOSED; the founder ratifies the open ones (end of doc) before a build
lane runs. Inputs: spec/LIBRARY.md sections 9.3 / 9.3.1 / 9.4 / 10.3,
`crates/photoproof-core/src/library/{preview,metadata,ingest,embedding}.rs`,
`crates/photoproof-core/src/library/mod.rs` (preview routing + worker loop),
web research on `imagepipe` / `rawkit` / `rawler` / darktable cited inline.

## The bug, stated plainly (founder, June 12 2026)

RAW files (incl. the founder's DNGs) never reach a true 1:1 preview.
`apps/desktop/src/lib/logic/metadata.ts:62` shows "Embedded preview — full
decode pending" permanently because the `full-raw-decode` pass is an
**unbuilt M1.5 feature**: `ingest::claim_next`
(`ingest.rs:227-229`) drains only `[Exif, Preview]`. The 154 enqueued
`full-raw-decode` rows sit `pending` forever — nothing claims them.
`run_pass` (`mod.rs:1636-1642`) explicitly marks any non-M1 pass
`no-worker`, but `claim_next` never even surfaces them, so the rows just
idle. This is exactly the spec's stated M1.5 deferral (LIBRARY 9.4: "rawler;
libraw FFI fallback deferred until a real format gap appears"). This plan
builds the pass.

## What we already have (ground truth — do not re-derive)

- **rawler 0.7.2** is already a dependency (`crates/photoproof-core/Cargo.toml:26`)
  and already does the metadata-only parse + embedded-preview ladder
  (`preview.rs::RawlerExtractor`, `metadata.rs::extract_raw`). rawler
  **decodes the CFA / sensor mosaic + metadata; it does NOT demosaic to
  RGB** (confirmed against the rawler 0.7.2 `RawImage` API —
  <https://docs.rs/rawler/latest/rawler/rawimage/struct.RawImage.html>).
  That is precisely the seam this pass fills.

- **rawler's `RawImage` already exposes every develop input we need** (same
  API doc): `data` (`RawImageData`, `pixels_u16()` / `pixels_u16_mut()`),
  `cropped_cfa()` (CFA pattern after crop), `wb_coeffs` (white-balance
  coefficients, RGBE order), `xyz_to_cam` + `cam_to_xyz()` /
  `cam_to_xyz_normalized()` + `color_matrix` (per-illuminant), `blacklevel`
  / `whitelevel`, `crop_area` / `active_area`, plus `linearize()`,
  `apply_scaling()`, and `develop_params()`. So the camera-specific hard
  parts — color matrix, WB-as-shot, black/white levels, crop, X-Trans/Bayer
  pattern — are **free from rawler**. What we must WRITE is the arithmetic
  that consumes them: black/scale → WB → demosaic → matrix → sRGB → gamma.

- **The preview pipeline downstream is done and reusable.** `preview.rs`
  already owns resize-to-edge (`resize_to_edge`, one CatmullRom pass —
  bench-frozen, do not touch), `write_artifacts` (both thumb+display,
  atomic temp+rename), `GENERATOR_VERSION`, the
  `PreviewSource::FullDecode` enum value + the `'full-decode'` DB CHECK
  (`schema.rs:352`), and `record_artifacts_locked`. A full-decode pass
  produces a **display-oriented sRGB `DynamicImage`** and hands it to the
  EXISTING `write_artifacts` — the artifact slot, cache layout
  (`previews/<h0..2>/<h2..4>/<hash>-{disp,thumb}.webp`), and color contract
  are already built. We are filling ONE function: RAW file → display-oriented
  sRGB `DynamicImage`.

- **The pass is already routed and enqueued.** `mod.rs:1404-1417` enqueues
  `full-raw-decode` `pending` on RAW ingest; `run_preview_pass_raw`
  (`mod.rs:1756-1864`) sets `needs_full_decode=1`, enqueues/promotes the
  pass on threshold miss / no-preview, and SKIPS it `threshold-met` when
  the embedded preview is already ≥ 2048 px (`EMBEDDED_ACCEPT_EDGE`). The
  flag-only / skip-met / strokes-promote machinery is live and tested. The
  worker is the only missing piece.

- **The cancellable background-drain pattern already exists.**
  `embedding.rs::process_embedding_queue` is the model: a separate drain
  that `claim_next_of(&[its passes])`, honors `opts.cancel`
  (`capture_live` wired in so an armed mic preempts mid-sweep — see
  `embedding.rs:124-148`, `download.rs:119-134`), reports via `QueueReport`,
  runs only when its prerequisite is ready and ingest is idle (P7.4 L4
  scheduling). The full-decode drain copies this shape.

## Recommended approach (lean): own develop pipeline on rawler 0.7.2

**Write a small `raw_develop` module in `photoproof-core` that consumes
rawler 0.7.2's `RawImage` and produces a display-oriented sRGB
`DynamicImage`.** No new RAW stack. WHY, and the rejected alternatives:

- **`imagepipe` — REJECTED.** It IS a full RAW→sRGB develop with a one-call
  `simple_decode_8bit`, but: last release **0.5.0, 2022-04-16, dormant ~4
  years** (<https://github.com/pedrocr/imagepipe/commits/master>,
  <https://lib.rs/crates/imagepipe>); it depends on **rawloader 0.37, NOT
  rawler** (the rawler migration, dnglab#478 + imagepipe PR #22, **stalled**
  on `rawler::cropped_cfa()` panicking "not yet implemented") — adopting it
  means **two parallel RAW stacks** (rawler 0.7.2 + rawloader 0.37), double
  binary weight, divergent format support (**imagepipe/rawloader has NO
  CR3**; our rawler does), and it is **LGPL-3.0** (copyleft). Its demosaic
  is a crude 3×3 same-color average and it carries an open
  darkness/WB bug (imagepipe#1). Not worth a second decode stack.

- **`rawkit` — REJECTED.** Despite the lib.rs hit, it is NOT rawler-derived:
  it is the Graphite editor's from-scratch decoder (GSoC 2024,
  <https://github.com/GraphiteEditor/Graphite/tree/master/libraries/rawkit>),
  **Sony `.arw` ONLY** (non-RGGB and other makers hit `todo!()`), published
  once (0.1.0, 2024-11-03). Permissive (MIT/Apache) but useless as a general
  RAW decoder. Its value is as a **reference implementation** for the
  dcraw-style stage order, which we can mirror.

- **libraw FFI — REJECTED for now (kept as the documented fallback).** High
  quality (AHD/AAHD/DCB demosaic, real highlight recovery) but a C build
  dependency, LGPL-2.1/CDDL copyleft, and it duplicates decoding alongside
  rawler. The spec already names "libraw FFI fallback deferred until a real
  format gap appears" (LIBRARY 9.4). Hold to that: invoke only if a format
  rawler decodes-but-we-can't-develop shows up in the founder's library.

**The win:** rawler already hands us CFA + black/white levels + WB +
color matrices for **DNG, CR2, CR3, ARW, NEF, RW2, ORF, RAF/X-Trans** and
~20 more. The develop arithmetic is the only new code — and it is the
exact ~few-hundred-line shape rawkit/dcraw implement, which we can mirror
under our own permissive terms (no LGPL transfer; rawler 0.7.2 is already
in the tree and its LGPL-2.1 status is unchanged by this work).

## The pipeline — what we write, in darktable's load-bearing order

darktable's canonical early pipe (and the universal constraint): **highlight
reconstruction operates on raw data BEFORE demosaic, which must run BEFORE
the input color profile**
(<https://docs.darktable.org/usermanual/development/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/>).
WB-as-shot is applied early (before demosaic) and the camera reference white
is corrected in the matrix stage. We need a **faithful NEUTRAL develop for
viewing/zoom**, not an editor. ESSENTIAL stages (must implement) vs OPTIONAL
(explicitly out of scope):

| Stage | Status | Source |
|---|---|---|
| 1. Linearize + black/white levels → float [0,1] | ESSENTIAL | rawler `blacklevel`/`whitelevel`, `apply_scaling()`/`linearize()` (free) |
| 2. White balance (as-shot camera multipliers) | ESSENTIAL | rawler `wb_coeffs` (free inputs; we apply per-CFA-cell) |
| 3. Demosaic (CFA → RGB) | ESSENTIAL — **we write** | rawler `cropped_cfa()` gives the pattern |
| 4. Camera→XYZ→sRGB color matrix | ESSENTIAL — **we write** | rawler `cam_to_xyz_normalized()` (free); we compose XYZ→sRGB (D65, Bradford if needed) |
| 5. Tone / gamma (sRGB transfer; optional neutral base curve) | ESSENTIAL — **we write** | reuse `preview.rs` sRGB-encode math (already present in `adobe_rgb_to_srgb_in_place`) |
| 6. Orientation → display-oriented; resize; encode | FREE | `preview.rs::apply_exif_orientation` + `write_artifacts` |
| Highlight reconstruction (beyond clip) | OPTIONAL — phase 2+ | clip-to-white in phase 1; clip is honest, not wrong |
| Denoise / sharpen / lens correction / CA | OUT OF SCOPE | not a neutral-view requirement; editing lives in darktable/C1/LR (FEATURES non-features) |

**Phase-1 demosaic = bilinear (Bayer/RGGB family).** Lowest-effort correct
neutral develop. X-Trans (Fuji RAF) and better demosaic (PPG/RCD-class) are
phase 2. Where the CFA pattern is one we don't yet demosaic, the pass marks
the row `skipped` with a reason (`unsupported-cfa`) — the embedded preview
stands, never a crash (mirrors `preview.rs::largest_chained_jpeg`'s
best-effort discipline).

### Geometry is the hard invariant, not color

LIBRARY 9.4: a full decode **MAY alter tone/color** vs the camera-rendered
embedded preview (rawler's render is not Canon's) **but MUST preserve
display-oriented geometry EXACTLY** — same orientation handling, same
aspect, **strokes land where they were drawn**. So the develop output runs
through the SAME `apply_exif_orientation` path and must produce the same
display-oriented aspect as the embedded artifact it replaces. The
existing `embedded_native_acceptable` aspect-agreement check
(`preview.rs:652`, tolerance 0.02) is the template for the acceptance
assertion. The stroke-substrate invariant (9.4: never regenerate a substrate
an existing stroke was drawn over except via a `GENERATOR_VERSION` bump) is
already enforced by the skip-met routing — full-decode only ever touches
FLAGGED (`needs_full_decode=1`) images, which by construction had no
acceptable substrate.

## How it plugs into the ingest queue

1. **Teach a worker the pass.** Do NOT widen `claim_next` (`ingest.rs:228`)
   — that drains on the M1 CPU wave pool and full-decode is memory-hungry
   (LIBRARY 10.3: separate decode pool `max(2, cores/2)`, NOT the
   `min(cores,8)` CPU pool). Instead add `process_raw_decode_queue` modeled
   on `process_embedding_queue`, claiming `claim_next_of(&[FullRawDecode])`.
   It runs at its own concurrency (decision OD-2) and on its own schedule.

2. **The worker body** (new `mod.rs::run_full_raw_decode_pass`, calling the
   new `raw_develop` module): locate a readable original (reuse the
   `best_path` / offline-defer logic from `run_pass`, `mod.rs:1604-1626` —
   offline volume = `defer_offline`, no attempt burned); rawler-decode →
   develop → orient → `write_artifacts`; then
   `record_artifacts_locked(..., PreviewSource::FullDecode, needs=false)`
   (clears `needs_full_decode`, sets `source='full-decode'`, 9.4) and
   `mark_done`. CFA we can't demosaic → `mark_skipped("unsupported-cfa")`.
   Permanent decode failure → `mark_failed` (non-transient); IO →
   transient retry (existing `fail_preview` taxonomy, `mod.rs:1866+`).

3. **Artifact & cache:** identical to today — `previews/` WebP thumb+display
   via `write_artifacts`. No new table, no new path scheme. `source` flips
   `embedded`→`full-decode`; the UI route at `mod.rs:2045` already maps it.

4. **Priority/backfill:** the rows are already enqueued at
   `PRIORITY_BACKFILL` (P2), promoted to `PRIORITY_SCAN` (P1) when the image
   carries strokes (`run_preview_pass_raw`, `mod.rs:1841-1855`) and the
   no-preview case enqueues at P1 (`mod.rs:1781`). No change needed — the
   worker just honors the existing `(priority, enqueued_at)` order via
   `claim_next_of`.

5. **Cancellation / politeness:** wire `opts.cancel` to `capture_live`
   exactly like the embedding drain (`embedding.rs:124-148`) — an armed mic
   preempts the sweep between items; LIBRARY 10.3's "background passes yield
   to live sessions" applies. Cancel latency is bounded by one item (a full
   decode is seconds — see perf), so check `cancel` per item, not just per
   wave. Consider a per-item soft check if a single 100MP decode is too long
   to preempt (OD-3).

6. **Idempotency / versioning:** the pass row PK is
   `(image_hash, pass_name, pass_version)`; re-running is a no-op once
   `done` (`ingest.rs:175`, 10.4). Artifact bytes are versioned by
   `preview::GENERATOR_VERSION` — a develop-algorithm change (better
   demosaic, color fix) that alters bytes **MUST bump GENERATOR_VERSION**
   (the 9.8 regeneration machinery at `mod.rs:187` re-pends caches), and to
   stay honest about the stroke-substrate invariant, an algorithm change is
   the ONLY sanctioned way to regenerate a substrate. Open: whether the
   develop algorithm gets its OWN version axis distinct from the encoder's
   `GENERATOR_VERSION` (OD-4).

7. **UI resolves "pending" → "1:1 ready":** `metadata.ts:52-62` already
   renders `source==='full-decode'` (not `pending`) as just the name, no
   "full decode pending" suffix, once `needs_full_decode` clears. The DTO
   `previewPending` (`dto.rs:335`) flips when the worker calls
   `record_artifacts_locked(..., needs=false)`. No frontend change required
   — the text resolves itself the moment the pass completes. The grid
   thumb-deferred icon (`Thumb.svelte:70`) likewise clears.

## Performance posture (demosaic is heavy)

- **Pool:** the LIBRARY 10.3 decode pool `max(2, physical_cores/2)`, separate
  from the M1 CPU wave pool. Don't oversubscribe — a full decode holds the
  whole sensor buffer (a 60MP RAW is ~120 MB at u16 ×1 channel pre-demosaic,
  ~720 MB as f32 RGB mid-pipe) and parallel decodes multiply that. Memory,
  not CPU, is the cap (mirrors the P6.3 finding that memory gated the model
  tiers).

- **When it runs:** background only, after M1 ingest (Exif+Preview) is idle
  and the original is online, yielding to `capture_live`. Same scheduling
  slot the embedding drain occupies (P7.4 L4 "only when ready AND ingest
  idle").

- **Target (this machine, to be measured, NOT promised):** order of
  **1–5 s/RAW** for a bilinear develop + resize + WebP encode on the decode
  pool — bilinear is cheap, the resize/encode are already bench-known from
  the embedded path. A 154-file backfill is then minutes, fully in the
  background. Establish the real number in the build-loop perf smoke (a
  `#[ignore]` real-RAW timing test, founder-machine, like the embedding e2e).

- **Cap dimension?** OPEN (OD-1). The embedded path tops out at
  `DISPLAY_EDGE=2560`. A true 1:1 develop at full sensor resolution (e.g.
  9504×6336 for the founder's A7CR) is what "1:1 zoom" demands, but
  `write_artifacts` resizes to 2560 anyway. **Decision needed:** does
  full-decode mean (a) just a better-quality 2560 display artifact (cheap,
  geometry-safe, fits the existing two-artifact contract), or (b) an
  additional full-resolution `source='full-decode'` artifact for the Look
  surface's deep-zoom route (parallels the on-demand
  `embedded_native_acceptable` native-size path, `preview.rs:630-683`)?
  The founder's "true 1:1 preview" language points at (b); the existing
  artifact contract is (a). See OD-1.

## Color-correctness acceptance ("correct neutral 1:1")

"Correct neutral 1:1" = a faithful, neutral develop: WB-as-shot applied,
camera→sRGB matrix applied, sRGB gamma, no editorial tone. It need NOT match
Canon's/Nikon's in-camera JPEG (9.4 says so explicitly) — only be neutral,
non-clipped (in normal exposure), and geometrically exact. Verification,
tied to the build-loop honesty gate (BUILD-LOOP "What 'tested' can mean"):

- **Cloud-verifiable:** unit tests on the develop math with a SYNTHETIC RAW
  (a hand-built CFA buffer + known WB + identity-ish matrix → expected RGB),
  the way `largest_chained_jpeg` is tested against a hand-rolled TIFF
  (`preview.rs:824+`). Geometry/aspect assertions (oriented output aspect ==
  embedded aspect within `EMBEDDED_NATIVE_ASPECT_TOLERANCE`). A gray-patch
  RAW must develop to near-neutral gray (WB + matrix sanity). These gate
  CI.
- **Founder-machine:** real-RAW visual check on the failing DNGs + a Bayer
  set (CR3/ARW/NEF), and a checksum-pinned develop of ONE known fixture so
  regressions are caught byte-wise (a develop-algorithm change is then a
  deliberate `GENERATOR_VERSION`/checksum bump, never a silent drift) —
  exactly the SPIKE/e2e `#[ignore]` pattern. Color FIDELITY beyond
  "neutral and plausible" is a founder eyeball item, not a CI claim — stated
  honestly in STATUS, not overclaimed.

## Risks / unknowns

- **Demosaic correctness is fiddly** (edge handling, CFA phase alignment,
  the crop/active-area offset changes which CFA cell a pixel is). Mitigation:
  start bilinear/RGGB, synthetic-test the CFA phase against `cropped_cfa()`,
  borrow rawkit/dcraw stage order as reference.
- **X-Trans (Fuji RAF)** needs a different demosaic; phase 2, skip-clean
  until then.
- **Linear vs CFA DNG.** rawler branches DNG on PhotometricInterpretation
  (CFA 32803 vs LinearRaw 34892) and already decodes both; a linear DNG is
  ALREADY demosaiced — our pass must DETECT that (rawler's
  `develop_params()` / cfa presence) and **skip the demosaic stage**, running
  only WB+matrix+gamma. Feeding a linear DNG through a Bayer demosaic is the
  classic corruption; guard it explicitly. (The founder's failing case is
  DNG — confirm CFA-vs-linear on the actual files in phase 1.)
- **Color matrix illuminant selection** (`color_matrix` is per-illuminant;
  pick D65 / the as-shot, document the choice).
- **Memory under parallel decode** (above) — cap pool width, possibly cap
  in-flight bytes.

## Phased slice

- **Phase 1 (MVP, the founder's DNGs):** `raw_develop` module = black/scale
  → WB → **bilinear Bayer demosaic** → `cam_to_xyz_normalized`→sRGB → gamma
  → orient; CFA-vs-linear DNG guard; the `process_raw_decode_queue` worker
  on the decode pool with `capture_live` cancel; writes the existing 2560
  display+thumb artifacts as `source='full-decode'`, clears the flag; synth
  unit tests + one founder-machine real-DNG + Bayer check. Resolves
  "full decode pending" for DNG/CR3/ARW/NEF/RW2/ORF (Bayer).
- **Phase 2:** X-Trans demosaic (Fuji RAF); better demosaic (PPG/RCD-class)
  behind a `GENERATOR_VERSION` bump; basic highlight reconstruction beyond
  clip; the full-resolution 1:1 artifact if OD-1 picks (b).
- **Phase 3 (only if a real gap appears):** libraw FFI for any format rawler
  decodes but we can't faithfully develop (LIBRARY 9.4's named fallback).

## Founder decisions (June 12 2026 — RESOLVED)

- **OD-1 — "1:1" = FULL SENSOR RESOLUTION.** Decided (b): a full-sensor-res
  `full-decode` artifact, deep-zoomable in Look exactly like 100% in
  Lightroom/darktable. Not just the 2560 display tier. Phase 1 still ships
  the display tier first, but the full-res artifact is the actual goal, not
  a phase-2 maybe.
- **OD-2 — Quality bar: typical neutral RAW decode is enough.** "Nothing
  crazy, just need to see real resolution." Bilinear demosaic + WB + matrix
  + gamma, clip-only highlights — accepted. Better demosaic/highlights stay
  deferred; the point is reviewing finished work at true resolution, not
  re-developing it.
- **OD-3 — see "Memory & concurrency" below** (how LR/darktable do it).
- **OD-4 (minor) — Versioning:** develop-specific reason, reuse the existing
  `GENERATOR_VERSION` regeneration machinery (no second re-pend path).

## Memory & concurrency — how Lightroom and darktable handle full-res develop

The founder asked how the editors solve OD-3. They take two different
stances; we want Lightroom's, not darktable's:

- **Lightroom — develop once, CACHE the result to disk.** LR builds a
  full-res "1:1 preview" on demand or in batch and stores it in the
  `.lrdata` preview cache (effectively a high-quality JPEG). Zoom to 100%
  serves the cached artifact instantly; it is NOT re-developed on every
  zoom, and the full-res float buffer is not held in RAM after the build.
  LR famously lets you discard 1:1 previews after N days because they are
  big on disk. Develop is GPU-accelerated where available.
- **darktable — interactive pixelpipe, develop on demand, NOT cached
  full-res.** For the lighttable grid it uses mipmaps; for darkroom it runs
  a live float pipeline (two pipes: a downsampled preview pipe + the
  display/zoom pipe). A 60 MP image is ~720 MB as 32-bit RGBA per buffer
  and the pipe holds several, so darktable **tiles**: when a module's
  memory exceeds the budget it splits the image into overlapping regions,
  processes each, and stitches — bounding RAM at a speed cost. This is the
  interactive-editor tax we do NOT need to pay.

**Our posture (we review, we don't edit): Lightroom's model, simplified.**
Develop each RAW to a full-res artifact **once**, write it to the
`previews/` cache (a high-quality JPEG or similar), free the float buffers,
and serve Look's deep-zoom from the cached artifact. There is no
interactive pipe, so peak memory = **one full-res develop in flight at a
time**, which is the OD-3 answer: serialize full-decode to a small pool
(`max(2, cores/2)`, likely 1–2 in practice) and, on lower-RAM machines,
fall back to **tiled demosaic** (darktable's escape hatch) rather than
holding the whole float image. Cache-on-disk means a re-zoom never
re-develops. This is strictly cheaper than either editor because we never
re-render interactively.

## OPEN scoping question raised by the founder: reading foreign edit sidecars

The founder: *"the main point of the app should be to review DONE work… we
may want to support reading in sidecar edit files from Lightroom/darktable."*
This is important and partly in TENSION with a neutral develop: if a RAW was
edited in Lightroom (XMP) or darktable (`.xmp`), our **neutral** develop will
look DIFFERENT from what the photographer sees in their editor — wrong, for
an app whose job is reviewing finished work. Honest assessment of the paths:

1. **Review the EXPORTS, not the RAWs (cheapest, highest fidelity).** Done
   work is usually exported to JPEG/TIFF with the edit baked in — which the
   app already handles perfectly. For "review done work," pointing the app
   at an export folder beats any RAW-sidecar scheme. Worth saying out loud
   before building a develop pipeline whose neutral output won't match an
   edited RAW anyway.
2. **Apply the sidecar's edit faithfully = reimplement the editor. NOT
   feasible.** Lightroom XMP carries Camera Raw settings; darktable `.xmp`
   carries darktable module params. Rendering them faithfully means
   reimplementing Adobe's (proprietary) or darktable's pipeline — out of
   scope, and a moving target.
3. **Apply the PORTABLE subset (pragmatic middle).** Crop, orientation/flip,
   rating/label/color, and maybe basic exposure/WB/B&W are legible from XMP
   and approximable on top of our neutral develop — partial fidelity,
   honestly labeled "approximate." Crop + orientation + rating/label is the
   high-value, low-risk slice (it makes the review match the photographer's
   keep/reject intent even if tone differs).
4. **Use the editor's embedded preview if present.** Some export/edit flows
   write a full-res JPEG preview reflecting the edit; when one exists, prefer
   it over developing. (Camera-embedded previews are the UNedited capture, so
   this only helps when the editor refreshed it.)

RECOMMENDATION: keep the neutral full-res develop (this plan) for *seeing
real resolution* — it's the right primitive and unblocks DNG-never-loads
now. Treat foreign-edit fidelity as a SEPARATE thread: first-class the
"review an exports folder" path (cheap, already works), and scope sidecar
reading to the portable subset (crop/orientation/rating) rather than
promising edit-accurate RAW rendering we can't deliver. Flagged as its own
backlog item; do not let it block the develop pass.
