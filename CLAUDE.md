# PhotoProof

Spec wins over docs. Closed backlog work moves `docs/BACKLOG.md` → `docs/LANDED.md`.

## Where things are
- **Specs (normative):** `spec/`
- **Doc map:** `docs/README.md` (or `docs/index.html`)
- **What's built / status:** `docs/STATUS.md`, `docs/features.html`
- **Open work:** `docs/BACKLOG.md` · **shipped:** `docs/LANDED.md`
- **Build loop & gate:** `docs/BUILD-LOOP.md`
- **Architecture:** `docs/architecture.html`

## Runtime data (`~/Library/Application Support/com.photoproof.desktop/`)
- `logs/photoproof.log` — backend log, fresh each `tauri dev` launch; always available to read or quick-add a `tracing` line
- `photoproof.db` — SQLite (`sqlite3 -readonly` for triage) · `models/` · `runtime/` · `previews/` · `vectors/`

## Notes
- Known failing test (pre-existing, ignore): `s02_2_case_only_rename_relinks_sidecar`
- No em-dashes in user-visible UI copy
