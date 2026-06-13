#!/bin/sh
# tune-check.sh — the metric-regression guard (DESIGN-TUNING-LOOP.md, the
# "Testing (defense)" half). Runs the CHEAP, DETERMINISTIC synthetic benches
# and FAILS (non-zero) if a quality/perf metric regressed past its tolerance
# vs the committed tuning-baselines.json.
#
# Zero setup by construction: the search signal is pp-retrieval-eval
# --synthetic (a 3-image in-process corpus, no models, no real DB) and the
# ingest signal is pp-bench over a small SYNTHETIC corpus. It runs on ANY
# machine with a checkout + cargo. Run it locally before a perf- or
# ranking-touching change; the synthetic form is CI-able (see BUILD-LOOP.md).
#
#   scripts/tune-check.sh                 # guard: green or non-zero
#   scripts/tune-check.sh --json          # machine-readable verdict on stdout
#   scripts/tune-check.sh --update-baseline   # rewrite the baseline (K14: then
#                                             # review + commit it deliberately)
#
# WHY release build: the dev profile pins workspace members at opt-level 0, so
# debug ingest numbers are meaningless (the bench.sh/sweep.sh precedent). The
# search metrics are order-only so they are build-independent, but one release
# build serves both and keeps the ingest baseline comparable.
set -eu

# Repo root (this script lives in scripts/). POSIX-portable dirname dance.
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"

BASELINES="$ROOT/tuning-baselines.json"

# Pass-through flags for the comparator (--json / --update-baseline). The
# bench-run knobs come from the baseline file so the run matches what the
# baseline was measured at.
FORWARD=""
for arg in "$@"; do
    case "$arg" in
        --json | --update-baseline) FORWARD="$FORWARD $arg" ;;
        -h | --help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "tune-check: unknown flag $arg" >&2
            exit 2
            ;;
    esac
done

# Read the ingest bench size from the baseline so the run is apples-to-apples
# with the committed number. No jq dependency (zero-setup): a tiny sed/grep
# pull of the two integer fields, with built-in fallbacks if absent.
read_int() {
    # $1 = field name; prints the integer value or the fallback ($2).
    val=$(grep -o "\"$1\"[[:space:]]*:[[:space:]]*[0-9]*" "$BASELINES" 2>/dev/null \
        | grep -o '[0-9]*$' | head -n1)
    if [ -n "$val" ]; then echo "$val"; else echo "$2"; fi
}
FILES=$(read_int files 24)
EDGE=$(read_int edge 512)

# A scratch dir for the bench JSON, cleaned on exit (success OR failure).
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pp-tune-check.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
RETRIEVAL_JSON="$WORK/retrieval.json"
INGEST_JSONL="$WORK/ingest.jsonl"

echo "tune-check: building release benches..." >&2
cargo build --release -q -p photoproof-core \
    --bin pp-retrieval-eval --bin pp_bench --bin pp-tune-check

# The release binaries (built above) — call them directly so the bench wall
# time is the binary's, not cargo's.
BINDIR="$ROOT/target/release"

echo "tune-check: synthetic retrieval eval..." >&2
"$BINDIR/pp-retrieval-eval" --synthetic --json >"$RETRIEVAL_JSON"

echo "tune-check: synthetic ingest ($FILES files, ${EDGE}px)..." >&2
"$BINDIR/pp_bench" ingest --files "$FILES" --edge "$EDGE" \
    --label tune-check --out "$INGEST_JSONL" >/dev/null

# Compare (or update). The comparator exits non-zero on a regression, which set
# -e then propagates as this script's exit status — the gate.
# shellcheck disable=SC2086
exec "$BINDIR/pp-tune-check" \
    --baselines "$BASELINES" \
    --retrieval "$RETRIEVAL_JSON" \
    --ingest "$INGEST_JSONL" \
    $FORWARD
