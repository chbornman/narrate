#!/usr/bin/env bash
# Release-excludes-debug-panel assertion (spec/UI.md §10.1, DECISIONS I6).
#
# Asserts, on real build artifacts:
#   1. The production frontend bundle (default `npm run build`) contains no
#      debug-panel module, marker string, or debug command name.
#   2. A PHOTOPROOF_DEBUG=1 build DOES contain them (positive control — the
#      markers actually work).
#   3. A release Rust binary built WITHOUT the `debug-panel` feature contains
#      no debug command names, so invoking one fails as unknown.
#      (Skippable with --skip-rust for a fast frontend-only check.)
#
# Run from apps/desktop/:  ./scripts/assert-release-clean.sh [--skip-rust]

set -euo pipefail
cd "$(dirname "$0")/.."

FRONTEND_MARKERS=("PP_DEBUG_PANEL_MARKER" "DebugPanel" "debug_tail_events" "debug_force_flush")
RUST_MARKERS=("debug_tail_events" "debug_force_flush" "debug_force_rescan" "PP_DEBUG_PANEL_RUST_MARKER")

fail() {
  echo "ASSERTION FAILED: $1" >&2
  exit 1
}

echo "== 1/3 production frontend bundle must be clean =="
rm -rf dist
npm run build >/dev/null 2>&1
for m in "${FRONTEND_MARKERS[@]}"; do
  if grep -rq "$m" dist/; then
    fail "release frontend bundle contains '$m'"
  fi
done
echo "   ok: no debug-panel markers in dist/"

echo "== 2/3 positive control: debug bundle must contain the panel =="
rm -rf dist
PHOTOPROOF_DEBUG=1 npm run build >/dev/null 2>&1
grep -rq "PP_DEBUG_PANEL_MARKER" dist/ || fail "debug bundle missing the marker — the assertion itself is broken"
echo "   ok: marker present in the PHOTOPROOF_DEBUG=1 bundle"

# Leave a CLEAN production bundle behind (so a following `cargo tauri build`
# embeds the release assets).
rm -rf dist
npm run build >/dev/null 2>&1

if [[ "${1:-}" == "--skip-rust" ]]; then
  echo "== 3/3 skipped (--skip-rust) =="
  exit 0
fi

echo "== 3/3 release binary must carry no debug commands =="
cargo build --release -p photoproof-desktop >/dev/null 2>&1
BIN="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/photoproof-desktop"
[[ -f "$BIN" ]] || fail "release binary not found at $BIN"
for m in "${RUST_MARKERS[@]}"; do
  if strings "$BIN" | grep -q "$m"; then
    fail "release binary contains debug command string '$m'"
  fi
done
echo "   ok: no debug command strings in $BIN"
echo "ALL ASSERTIONS PASSED"
