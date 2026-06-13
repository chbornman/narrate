# test-corpora/retrieval — golden-query sets for the M3 retrieval gate

This is where the **real** golden query set lives for the
"Golden-query retrieval eval" gate (BACKLOG; RETRIEVAL §12) — the M3 quality
gate that settles the S4 always-on weight (B69) and the reranker go/no-go.

The query set files here are **gitignored** (only this README is committed):
they encode a private library's image content hashes, which never enter git.

## File format (JSON)

One object with a `queries` array; each query carries its text, the set of
relevant **image content hashes** (`ContentHash` hex — exactly what search
results carry, so they match by string equality), and optional notes. Full
spec + rationale: the header of
`crates/photoproof-core/src/retrieval_eval.rs` (`QuerySet`).

```json
{
  "default_k": 10,
  "queries": [
    {
      "query": "quiet melancholic harbor at dusk",
      "relevant": ["3a7f...64hex...", "9c12...64hex..."],
      "notes": "the slow-series candidates; clip-heavy, few notes"
    }
  ]
}
```

To find an image's content hash for the `relevant` list, read it off the
library DB (the `image_hash` column) or the search debug panel.

## Running the gate (weight sweep)

The runner is `pp-retrieval-eval`. Point it at the live library DB and a
query set; sweep one `FusionWeights` knob at a time and diff the `--json`
reports. The DB lives at
`~/Library/Application Support/com.photoproof.desktop/photoproof.db`.

```sh
DB="$HOME/Library/Application Support/com.photoproof.desktop/photoproof.db"
QS="test-corpora/retrieval/golden.json"

# baseline (spec defaults: s1=1 s2=1 s3=0.5 s4=1)
cargo run -p photoproof-core --bin pp-retrieval-eval -- \
  --db "$DB" --queries "$QS" --json > /tmp/base.json

# does dialing S4 (image_clip, B69) down hurt? sweep it and compare
cargo run -p photoproof-core --bin pp-retrieval-eval -- \
  --db "$DB" --queries "$QS" --s4 0.5 --json > /tmp/s4-half.json

# read the deltas
jq '.mean' /tmp/base.json /tmp/s4-half.json
```

Flags: `--k N` (cutoff; defaults to the file's `default_k`, then 10),
`--s1/--s2/--s3/--s4 W` (per-signal fusion-weight overrides), `--json`
(machine form). Paths also come from `PP_RETRIEVAL_DB` /
`PP_RETRIEVAL_QUERYSET`.

Note on `SIM_BLEND_BETA` (the §5.3 dense-signal tilt): it is a compile-time
constant in `search/hybrid.rs`, not part of the `FusionWeights` API the
runner sweeps, so a beta sweep is a one-line source edit + rebuild measured
by the same report.

The synthetic, no-models CI proof of this whole pipe (so it stays green
without a real library) is the `retrieval_eval_sample` integration test.
