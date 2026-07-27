# Desktop release records

Every production tag `desktop-vX.Y.Z` must have:

- the same version in the workspace, desktop package, Tauri config, and
  `release-contract.json`;
- an `X.Y.Z.md` file with changes, database compatibility, and rollback;
- `release-contract.json` updated to the exact database schema constant and an
  honest migration-verification result.

The production workflow checks these records before any signing job. The
release remains a draft until installed-package smoke and the founder rollout
gate pass.
