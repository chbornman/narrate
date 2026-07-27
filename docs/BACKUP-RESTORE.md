# Photoproof backup and restore

Photoproof has two different portability promises. A **journal export** is the
open-format, long-term copy of the photographic journal. A **full app-data
backup** is the only way to preserve the exact desktop application state.
Neither should be described as the other.

## What sidecars and journal export restore

Adjacent `*.photoproof.json` files plus Photoproof's overflow and session
journals reconstruct journal events, targets, redaction markers, session
context, image hash/filename snapshots, folded search text, and derived-table
queues. The one-click journal export also includes
`collections.photoproof.json` and `topics.photoproof.json`. The latter contains
saved topic phrases, their selected embedding space, and authored topic notes;
topic rankings remain derived. Settings can union-import that topics file, and
same-id/different-content conflicts abort the whole import rather than replacing
either authored copy.

Sidecars do **not** contain settings, device identity, runtime configuration,
license acceptances, download consent, cached hardware tier, root/volume
registrations, watcher state, preview files, full-decode
files, vectors, downloaded models, logs, or migration recovery snapshots.
Previews, vectors, and search indexes are reproducible. The other listed state
is either configuration/history or user state that only a full app-data backup
preserves.

The SQLite database can contain journal commits not yet flushed to a sidecar
(normally a debounce window, indefinitely while a destination volume is
offline). A journal export produced by the app serializes from the database and
is therefore safer than copying only adjacent sidecars.

## Full app-data backup

Use **Settings → Export and recovery → Back up complete app state**. Choose a
new `.ppbackup` directory name and confirm **Back up and quit**. The live
process performs its coordinated shutdown first. A private helper is already
waiting on an inherited pipe; the operating system cannot deliver EOF on that
pipe until the desktop process and its SQLite/WAL handles are truly gone. Only
then does the helper copy the complete app-data tree, hash every file, reject
symlinks/unmanifested payloads, verify the result, and reopen Photoproof.
Settings shows the durable completion/failure receipt after relaunch.

Keep a journal export as a separate, open-format recovery path. An operator may
still make an external whole-directory copy while Photoproof is fully stopped,
but it does not receive the installed helper's manifest and verification
receipt.

The full tree includes the database, `settings.json` and last-known-good copy,
`device-id` and last-known-good copy, `config.toml`, `tuning.toml`,
`collections.photoproof.json`, exported topic documents, `photoproof/journal/`, `runtime/`, migration
`.pre-upgrade-*.bak` files, and derived/model/log directories. A configured
absolute `runtime.models_dir` lives outside app data and must be copied
separately only if avoiding model re-downloads matters; models are not user
truth.

Never copy app data while Photoproof is running as a claimed consistent backup.
SQLite's online backup API protects migration snapshots internally, but a raw
filesystem copy racing an active WAL, settings replacement, or sidecar flush
has no whole-application consistency boundary.

## Full restore

Use **Settings → Export and recovery → Restore complete app state** and select
the `.ppbackup` directory. Before Photoproof quits, the entire manifest and
every checksum are verified. After the inherited-pipe exit boundary, the helper
renames the current app-data directory to a timestamped sibling, restores and
fsyncs into a staging directory, verifies the staged bytes, atomically publishes
them at the original path, writes a receipt, and restarts Photoproof. A failure
before publication reinstates the previous directory. A successful receipt
shows the retained rollback path; keep it until application health,
roots/volumes, settings, collections, topics, and recent journal entries have
been checked.

Offline/remapped photo roots may need to be reattached. Derived previews and
vectors can be rebuilt; downloaded models can be verified or downloaded again.

If the full database cannot open, keep it and its WAL untouched for forensic
recovery. Use “Rebuild index from sidecars” against a fresh database/export
copy; that operation is intentionally not equivalent to restoring settings,
device identity, or runtime choices. Saved topics and topic notes have their
own explicit `topics.photoproof.json` import action.

## Migration recovery snapshots

Before an on-disk schema upgrade, Photoproof writes and integrity-checks a
sibling `photoproof.db.pre-upgrade-vX-to-vY.bak`. It is an emergency copy of
the pre-upgrade database, not a recurring backup and not a complete app-data
snapshot. Restore it only while the app is stopped, preserve the failed
database/WAL first, and use the app version compatible with that schema or let
a current build migrate a copy.

## Automated evidence

The desktop test suite exercises the offline backup primitive and installed
helper protocol by creating a representative app-data tree (database/WAL,
settings/config/device identity, collections/topics, overflow/session journals,
vectors, and previews), checksumming it, replacing the source, restoring, and
comparing every file. It proves the helper waits for protocol EOF, trailing
input cannot trigger work, tampering is rejected before live app data moves,
restore retains the prior directory, and a receipt is durable. Core tests prove
topic export/import round trips, idempotent union, and transaction-wide rollback
on authored-content conflicts. Migration tests separately verify SQLite's
pre-upgrade backup.
