# docs/ — what each file is

One line per file so nothing here reads as randomly named. The normative
implementation contract lives in `spec/`, not here; where any doc disagrees
with a spec, the spec wins. Retired docs are listed at the bottom of
BUILD-LOOP.md with where their content went; full text stays in git history.

## Live (maintained continuously)

| File | What it is |
|---|---|
| `STATUS.md` | **The capability ledger** — every spec obligation, its build state (five states), and the evidence. Updated at every packet close. |
| `BUILD-LOOP.md` | The packet-grain build ledger: how packets run, gates, the status table, retired-docs list. |
| `BACKLOG.md` | Decided-but-not-scheduled work; items graduate into packets. Open items only — shipped items move to LANDED.md. |
| `ROADMAP.md` | The milestone-organized map of everything left to build: the navigational view over BACKLOG + STATUS + the spec milestone tags. |
| `LANDED.md` | The shipped-from-backlog archive: every `[x]` item, moved verbatim with its hashes and context — the de facto changelog of backlog-sourced work. |
| `FOUNDER-CHECKLIST.md` | Decisions awaiting Caleb + founder-machine verification pending. |

## Reference (stable; updated when their subject changes)

| File | What it is |
|---|---|
| `SCOPE.md` | The vision and architecture overview — the pitch, the problem, the shape of the product. |
| `FEATURES.md` | The milestone-tagged feature inventory the specs elaborate. |
| `UI-FEATURESET.md` | Normative UI addendum (desktop-conventions agreement); where UI.md is silent, this wins. |
| `UI-ARCHITECTURE.md` | Frontend architecture contracts (action registry, slices, guardrails) — frozen by FOUNDATIONS. |
| `DOGFOOD-M1.md` / `DOGFOOD-M2.md` | Founder-machine verification scripts: what to run, what to look at. |
| `architecture.html` | The one-page visual architecture (self-contained HTML+SVG): processes, models, seams, truth stores. Regenerate at packet close, beside STATUS.md. |
| `diagrams.html` | Seven Mermaid diagrams for understanding the system (self-contained HTML + one CDN script): process/truth-store architecture, the 5 models, capture flow, ingest pipeline, search fusion, the concurrency priority ladder, fallback chains. Complements architecture.html; honest about M3/planned parts. |
| `features.html` | The one-page cascading feature tree (self-contained HTML): everything BUILT, by product area, with honest partial markers. Regenerate beside STATUS.md. |
| `index.html` | The docs landing page (self-contained HTML): this catalog as a linked map — one row per doc, Live / Reference / spec/. |
| `MODELS.md` | The connector-options matrix: per seam, the current pick, alternates evaluated, candidates, watch triggers. |
| `SPIKE-ASR35.md` | Nemotron 3.5 dev evaluation + the chunk-size root-cause finding (B74). |
| `SPIKE-P6.3.md` | Model-spike findings and recipes (ASR/LLM/VAD pins, flags, measurements) — load-bearing for RUNTIME. |
| `SPEC-GAPS.md` | CLOSED historical record: the gap-id registry the spec status banners cite ("Closes E5"). Not a TODO. |
| `research/` | The cited pre-build research reports (archive). |
