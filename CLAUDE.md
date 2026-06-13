# PhotoProof — project working notes for Claude

Operational facts that save time on this repo. The normative contract is in
`spec/`; the doc map and discipline are in `docs/` (start at `docs/index.html`
or `docs/README.md`). Where any doc disagrees with a spec, the spec wins.

## Runtime data & logs (check these when debugging behavior)

The running app keeps everything under:

```
~/Library/Application Support/com.photoproof.desktop/
  logs/photoproof.log   ← backend log, FRESH each `tauri dev` launch (truncated on start)
  photoproof.db         ← SQLite (journal, ingest_passes, vectors meta, …)
  models/               ← downloaded model weights + installed.json + manifest.json
  runtime/              ← acceptances, children.json, tier.json, instance.lock
  previews/  vectors/   ← preview cache, PPVEC flat-file store
```

- **`logs/photoproof.log` is always available** — read it to see what the
  running app actually did (downloads, ingest, mic, search, supervisors).
  It's one clean session per launch, so a jank is reviewable after the fact.
- When diagnosing, **just add a `tracing::debug!`/`info!` line** in core or the
  shell — it lands in that file (filter default is `info` + `debug` for
  photoproof crates; `RUST_LOG=trace` for the firehose). Remove temporary
  ones before committing.
- The log layer installs in `apps/desktop/src-tauri/src/lib.rs::install_logging`.
- Inspecting the DB directly with `sqlite3 -readonly` is fair game for triage
  (e.g. `ingest_passes` state counts, journal `annotation_events`).

## Verification

The standing gate (see `docs/BUILD-LOOP.md`): `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
frontend adds `npx svelte-check --fail-on-warnings` and `vitest` (run from
`apps/desktop`). **Known pre-existing failure:** `s02_2_case_only_rename_relinks_sidecar`
(APFS case-only rename) — it predates current work; don't chase it as a regression.

## Conventions specific to this app

- **No em-dashes in user-VISIBLE UI copy** (founder rule). Comments/docs are fine.
- HTML pages in `docs/` (`index`, `architecture`, `features`) are RENDERED VIEWS —
  regenerate at packet close; markdown/spec is the source of truth.
- Closed backlog work moves `docs/BACKLOG.md` → `docs/LANDED.md` with its commit hash.
- Parallel feature builds run as agents in isolated git worktrees, then merge to
  main one at a time with the gate re-run on the merged tree (the session pattern).
- The mic is on **Space** (tap toggles, hold = push-to-talk); M is reserved/free.
