#!/usr/bin/env bash
# Search-tuning sweep (release build — dev numbers are meaningless; the
# workspace pins members at opt-level 0). Runs the retrieval eval once per
# config over a knob grid, ranks by a quality metric (default nDCG@10), and
# PROPOSES a tuning.toml (K14: it proposes, you commit by copying the proposal
# into tuning.toml). See the pp_sweep.rs header for flags and the JSON schema.
#
#   scripts/sweep.sh search \
#       --db "$HOME/Library/Application Support/com.photoproof.desktop/photoproof.db" \
#       --queries test-corpora/retrieval/golden.json \
#       --grid "s4=0.5,0.75,1.0,1.25;beta=0.3,0.5" \
#       --propose tuning.proposed.toml
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo run --release -p photoproof-core --bin pp-sweep -- "$@"
