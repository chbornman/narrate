# test-corpora — standardized benchmark inputs (gitignored, frozen)

Real-file corpora for `scripts/bench.sh` (`pp-bench --source`). The
contents are NOT in git (see .gitignore) and are FROZEN: once copied,
never add, remove, or re-edit files — comparable numbers over months
depend on identical inputs. If a corpus must change, make a NEW
directory with a date suffix and retire the old name.

| Directory | Provenance | Contents | Size |
|---|---|---|---|
| `raw-canon-cr2/` | HomeNAS `RAW/2017/2017-11-09_crowder-concert` (2026-06-11) | 100× Canon CR2 + 14× Sony ARW (mixed shoot) | 2.9 GB |
| `raw-sony-arw/` | HomeNAS `RAW/2024/2024-04-27_climate-summit` (2026-06-11) | 6× Sony ARW (ILCE-7CR class) + XMPs | 346 MB |
| `raw-sony-arw-large/` | HomeNAS `RAW/2025/2025-07-19_countrycabaretjuly2025` (2026-06-11) | 315× Sony ARW + 315 XMPs | 12 GB |
| `jpeg-dcim/` | HomeNAS `RAW/2024/2024-10-07_dcim` (2026-06-11) | 389× camera JPG + 9 ARW | 3.3 GB |
| `jpeg-sample/` | founder-dropped (2026-06-11) | 41× JPEG | 339 MB |
| `heic-sample/` | founder-dropped (2026-06-11) | 43× HEIC + 3 MOV (M1 defers HEIC previews — this corpus tracks the deferral cost and, later, the M1.5 decoder) | 232 MB |

Baseline runs (append-only, machine-local) live in `bench-results.jsonl`
at the repo root. Standard invocations:

    scripts/bench.sh ingest --source test-corpora/raw-canon-cr2 --label "canon-<change>"
    scripts/bench.sh ingest --source test-corpora/raw-sony-arw-large --label "sony-<change>"
    scripts/bench.sh ingest --files 2000 --label "synthetic-2k-<change>"

`--source` mode is read-only by construction (the bench volume is
reported as a system root, which the marker writer skips; the library,
DB, and preview cache live in a discarded tempdir).

History worth knowing (see bench-results.jsonl labels):
- `canon-baseline` — generator v1 (CatmullRom + libwebp method 4).
- `canon-method2-twostep*` — the two-step-resize experiment: REJECTED,
  3.4× slower resize (the note in preview.rs::resize_to_edge).
- `canon-method2-only` — generator v2 ships: libwebp method 2,
  −58% encode time, −23% ingest wall on this corpus.
