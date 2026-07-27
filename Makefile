# PhotoProof — convenience targets. The normative gate lives in
# docs/BUILD-LOOP.md; these wrap it so the loop is one command. The
# metric-regression guard (tune-check) is the DESIGN-TUNING-LOOP.md defense
# half — NOT in the per-commit gate (the deep real-model checks are
# founder-machine), but the cheap synthetic guard below runs anywhere.

.PHONY: fmt lint test gate tune-check tune-baseline scale-check scale-check-founder soak-harness-test

# The standing per-commit gate (BUILD-LOOP.md).
fmt:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

gate: fmt lint test

# Metric-regression guard: run the cheap synthetic search + ingest benches and
# FAIL if a metric regressed past tolerance vs tuning-baselines.json. Run it
# locally before a perf- or ranking-touching change. `make tune-check JSON=1`
# emits machine-readable verdict JSON.
tune-check:
	scripts/tune-check.sh $(if $(JSON),--json,)

# Regenerate the baselines from a fresh run (K14: the machine measures, the
# human reviews + commits the new reference when an intended improvement lands).
tune-baseline:
	scripts/tune-check.sh --update-baseline

# Bounded 20k catalog + 20k/100k frontend scale gates. The founder tier raises
# SQLite to 100k and enables the optional 250k frontend transform receipt.
scale-check:
	scripts/scale-check.sh

scale-check-founder:
	scripts/scale-check.sh --tier founder

soak-harness-test:
	node --test scripts/real-library-soak.test.mjs
