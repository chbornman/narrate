#!/bin/sh
# Reproducible catalog-scale regression gates.
#
# The default CI tier stays bounded: 20k catalog rows, no image payload
# generation, and the ordinary 20k/100k frontend transform tests. The founder
# tier raises the SQLite catalog to 100k and enables the bounded 250k frontend
# case. Neither tier is a RAW/decode/filesystem/installed-webview throughput
# claim; use --source and the real-library soak recipe in
# docs/DESKTOP-EXPERIENCE-BUDGETS.md for those.
#
#   scripts/scale-check.sh
#   scripts/scale-check.sh --tier founder
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
TIER=ci

if [ "$#" -gt 0 ]; then
    if [ "$#" -eq 2 ] && [ "$1" = "--tier" ]; then
        TIER=$2
    else
        echo "usage: scripts/scale-check.sh [--tier ci|founder]" >&2
        exit 2
    fi
fi

case "$TIER" in
    ci)
        FILES=20000
        ITERATIONS=10
        GRID_BUDGET_MS=75
        CONTENTION_BUDGET_MS=250
        FRONTEND_TIER=ci
        ;;
    founder)
        FILES=100000
        ITERATIONS=20
        GRID_BUDGET_MS=350
        CONTENTION_BUDGET_MS=900
        FRONTEND_TIER=founder
        ;;
    *)
        echo "scale-check: unknown tier $TIER (expected ci or founder)" >&2
        exit 2
        ;;
esac

WORK=$(mktemp -d "${TMPDIR:-/tmp}/pp-scale-check.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
RESULTS="$WORK/results.jsonl"

echo "scale-check: building release catalog bench..." >&2
cargo build --release -q -p photoproof-core --bin pp_bench

echo "scale-check: $TIER grid-list ($FILES catalog rows)..." >&2
"$ROOT/target/release/pp_bench" grid-list \
    --catalog-fixture --files "$FILES" --iterations "$ITERATIONS" \
    --p99-budget-ms "$GRID_BUDGET_MS" --label "scale-$TIER" --out "$RESULTS"

echo "scale-check: $TIER progress/list contention ($FILES images, 5 passes each)..." >&2
"$ROOT/target/release/pp_bench" activity-contention \
    --catalog-fixture --files "$FILES" --passes-per-image 5 \
    --iterations "$ITERATIONS" --p99-budget-ms "$CONTENTION_BUDGET_MS" \
    --label "scale-$TIER" --out "$RESULTS"

echo "scale-check: $FRONTEND_TIER frontend snapshot transforms..." >&2
(
    cd "$ROOT/apps/desktop"
    PHOTOPROOF_SCALE_TIER="$FRONTEND_TIER" \
        bunx vitest run \
        tests/catalog-snapshot-scale.test.ts \
        tests/fling-load-budget.test.ts
)

echo "scale-check: passed ($TIER); JSON receipts were printed above" >&2
