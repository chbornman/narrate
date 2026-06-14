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

## Remote GPU test machine (margo)
- `ssh caleb@margo.local` (LAN, key auth, no password) whenever you need to build/test the
  NVIDIA or other GPU paths that CANNOT build on the M1 Mac: **TensorRT / CUDA** (and later
  DirectML / WebGPU / Vulkan).
- margo = founder's desktop: **Arch Linux, Ryzen 9900X + RTX 5080** (16 GB, CUDA 13.3, Vulkan 1.4).
  TensorRT libs not yet installed (CUDA EP works today; TensorRT needs install + a CUDA-13 compat check).
- Default shell is **fish** — wrap remote commands as `ssh caleb@margo.local 'bash -lc "..."'`.
- Repo lives at `~/projects/photoproof` (NOT narrate). Loop: push to origin → `git pull` on margo → build there.
- Connectors GPU build: `cargo build -p photoproof-connectors --features cuda` (floor) / `--features tensorrt`.

## Notes
- Known failing test (pre-existing, ignore): `s02_2_case_only_rename_relinks_sidecar`
- No em-dashes in user-visible UI copy
