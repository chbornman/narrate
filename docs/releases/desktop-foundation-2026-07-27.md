# Desktop foundation evidence - 2026-07-27

This is the settled local delivery receipt for the A01-A26 desktop-foundation
program. It is not a production release approval. Native, hardware, signing,
updater, and remote-CI evidence remains exactly as listed in
`PLAN-DESKTOP-FOUNDATION.md`.

## Local quality gates

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: passed with only explicitly ignored hardware,
  corpus, and performance cases.
- Desktop library: 290 passed, 3 ignored.
- Core library: 323 passed, 2 ignored.
- Library acceptance: 36 passed, 2 ignored.
- Frontend `check`: 0 errors, 0 warnings.
- Frontend: 100 files and 1,251 tests passed.
- Frontend production build: passed.
- Release contract v0.1.0/schema 16: passed.
- NVIDIA wiring contract: passed.
- No-em-dash UI gate and `git diff --check`: passed.
- All three GitHub workflow files parse as YAML.

## Lifecycle chaos receipt

`node apps/desktop/scripts/run-desktop-chaos-matrix.mjs` executed all 15 local
suites. All suites passed. Of 28 cases, 23 passed and 5 were correctly emitted
as `fixture-passed-platform-drill-pending`. The generated machine report for
this run was `/tmp/photoproof-a25-final-2026-07-27.json`.

## Linux package receipt

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `Photoproof_0.1.0_amd64.deb` | 40,909,892 | `6780abc8dd07c75fb87f225a903cc077debcea0d22ab0a34015d3b1825d658bc` |
| `Photoproof-0.1.0-1.x86_64.rpm` | 40,908,748 | `1b23ed6c670a5c9b3b072e55d1ba70e319503dd9c4aa0216c125482dfd098704` |
| `Photoproof_0.1.0_amd64.AppImage` | 132,880,888 | `32e01c440a8d22d4f3803e2c3f74ebce745896a48562c82150edc0787c944379` |

DEB and actual AppImage execution both passed installed smoke:

- database schema 16;
- `Usable` in 7 ms;
- shutdown in 0 ms;
- adjacent `pp-asr-server` found and size-checked;
- nine backup helper files verified;
- restore succeeded and retained its rollback directory;
- no ASR/LLM Ready claim without installed model weights.

## Mac pre-sync inventory

`bornman@bornmanmac.local` had one clean checkout at
`/Users/bornman/projects/photoproof`, with `Projects` and `projects` resolving
to the same default-APFS directory. Before synchronization it exactly matched
GitHub `main` at `513ca2c`.

The M1 Pro has the native Xcode/Rust/Bun toolchain and is suitable for APFS,
Metal, package, and lifecycle receipts. It has no Developer ID Application
identity, no installed app or pinned ASR models, and about 23 GiB free.
GitHub SSH and `gh` authentication are currently invalid, so this delivery is
synchronized over the trusted LAN after the authoritative local push.
