#!/usr/bin/env bash
# Re-runnable performance scenarios (release build — dev numbers are
# meaningless; the workspace pins workspace members at opt-level 0).
# Results append to bench-results.jsonl: one JSON line per run, diffable
# over time. See the pp_bench.rs header for flags and the output schema.
#
#   scripts/bench.sh ingest --files 2000 --label "before-X"
#   scripts/bench.sh ingest --source ~/Pictures/temp/shoogie --label "real-arw"
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo run --release -p photoproof-core --bin pp_bench -- "$@"
