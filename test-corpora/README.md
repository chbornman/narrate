# test-corpora — standardized benchmark inputs (gitignored, frozen)

Real-file corpora for `scripts/bench.sh` (`pp-bench --source`). The
contents are NOT in git (see .gitignore) and are FROZEN: once copied,
never add, remove, or re-edit files — comparable numbers over months
depend on identical inputs. If a corpus must change, make a NEW
directory with a date suffix and retire the old name.

| Directory | Provenance | Contents | Size |
|---|---|---|---|
| `raw-canon-cr2/` | HomeNAS `iris_images/RAW/2017/2017-11-09_crowder-concert` (copied 2026-06-11) | 100× Canon CR2 + 14× Sony ARW + XMPs (a realistic mixed shoot) | ~2.9 GB |
| `raw-sony-arw/` | HomeNAS `iris_images/RAW/2024/2024-04-27_climate-summit` (copied 2026-06-11) | 6× Sony ARW (ILCE-7CR class) + XMP sidecars | ~340 MB |

Baseline runs (append-only, machine-local) live in `bench-results.jsonl`
at the repo root. Standard invocations:

    scripts/bench.sh ingest --source test-corpora/raw-canon-cr2 --label "canon-baseline"
    scripts/bench.sh ingest --source test-corpora/raw-sony-arw  --label "sony-baseline"
    scripts/bench.sh ingest --files 2000 --label "synthetic-2k"

`--source` mode is read-only by construction (the bench volume is
reported as a system root, which the marker writer skips; the library,
DB, and preview cache live in a discarded tempdir).
