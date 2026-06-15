# State integrity audit

> Generated 2026-06-14 by a multi-agent audit (state-integrity-audit workflow). 10 state classes, 70 confirmed findings (36 silent failures, 18 high severity), 10 refuted.
> Re-run: `Workflow({scriptPath: 'scripts/state-integrity-audit.workflow.js'})`. This is a living doc; update the checklist as gaps close.

## Summary

The audit covered 10 state classes and confirmed 70 findings (10 refuted). Overall posture is sound at the format/byte level (PPVEC crash-safety, atomic temp+rename writes, WAL) but weak at the consistency/version/cross-process layer: the recurring failure shape is "disk and DB drift apart, or a version constant skews, and nothing detects or warns." 36 confirmed findings are silent failures (no log, no UI, no error path). By severity: 18 high, and the rest medium/low (the verifier downgraded a handful from their originally-filed severity; the per-finding tables below carry the adjusted values). The single biggest theme is missing version/downgrade guards (DB user_version > max, GENERATOR_VERSION downgrade, manifest_version never validated, PASS_VERSION no bump strategy) plus missing disk-vs-DB reconciliation (orphaned vector spaces, orphaned preview rows, missing model files). The two most acute security/integrity items are the WAL-checkpoint-blocked-at-shutdown leak of scrubbed plaintext and the newer-version DB opening silently with incomplete schema knowledge. Most items are recoverable, but several have no escape hatch and none warn the user.

## Prioritized findings

| State class | Finding | Sev | Silent | Recovery | User warned | Reset path | Fix |
|---|---|---|---|---|---|---|---|
| SQLite db | Newer-version DB (user_version > 15) opens silently; no guard `schema.rs:618-758` | high | yes | manual | no | delete db / downgrade | add `version > 15` guard → `StoreError::IncompatibleVersion` |
| App data dir | GENERATOR_VERSION downgrade serves future artifacts undetected `mod.rs:1912-1920`, `preview.rs:28-34` | high | yes | auto | no | clear_preview_cache(All) | persist highest gen_version marker row; refuse/warn on downgrade |
| App data dir | WAL checkpoint blocked at shutdown leaves scrubbed plaintext in WAL `store/mod.rs:1460-1520` | high | yes | manual | no | manually delete -wal/-shm | force-close readers + retry; block exit / warn if still blocked |
| ModelDownloads | manifest_version stored but never validated against running `plan.rs:70-93`, `download.rs:140-145` | high | yes | manual | no | delete installed.json / model dir | compare installed vs current manifest_version in local_model_plan() |
| ModelDownloads | Missing model file not detected until runtime load `plan.rs:82-89` | high | yes | manual | yes | delete model in settings | spot-check critical files exist at plan/status; mark NotConfigured |
| ModelDownloads | installed.json malformed → silent empty (re-download all) `download.rs:178-183` | high | yes | manual | no | delete installed.json | log warn distinguishing IO vs parse; write .backup |
| ingest_passes | PASS_VERSION hard-coded, no bump strategy → silent queue partition `ingest.rs:56` | high | yes | manual | no | DELETE old pass_version rows + re-ingest | test asserting PASS_VERSION bumps with schema; doctor clears orphans |
| VectorStore | Orphaned vector rows: file deleted, DB rows persist, retrieval silently empty `ppvec.rs:189-208,523-524,897-923` | high | yes | manual | no | doctor rebuild/clear_vectors | doctor subscan: verify space file exists per (vec_kind,model_id) |
| VectorStore | Model ID skew (fp16 CLIP regression) — FIXED `embedding.rs:95-118`, commit a3b1e14 | high | yes | auto | no | none (post-fix) | already deployed; pre-fix: doctor rebuild_derived |
| Frontend prefs | Surround prefs diverge across webviews (no broadcast) `surround-store.svelte.ts:1-70` | high | yes | manual | no | clear localStorage / restart | add cross-window emit/listen mirroring theme-store |
| Frontend prefs | Theme-store cross-window broadcast uncommitted `theme-store.svelte.ts` | high | yes | auto | no | commit pending diff | commit staged emit/listen/THEME_EVENT changes |
| Sidecar | Case-only rename does not relink sidecar (test s02_2) `engine.rs:985-1014,869` | high | yes | manual | no | none | run case_rename_cleanup at reconciliation, not only post-flush |
| Sidecar | Old-case sidecar silently absorbed as truth after rename `engine.rs:1380-1454`, `doc.rs:273` | high | yes | manual | no | delete stale sidecar / full scan | detect case-mismatched adjacent in converge_file; flag for delete |
| Sidecar | Concurrent modify-during-read (TOCTOU) on sidecar `engine.rs:1336` | high | yes | auto | no | next scan retries | retry read w/ backoff on parse error (scan() not in prod path) |
| ModelDownloads | fp16 CLIP offered tier-1 but not hosted (`local-fp16-convert`) `manifest.rs:530-568` | high | no | manual | yes | switch config to int8 / wait | pre-flight reject placeholder revision; or tiers: vec![] |
| App data dir | Disk-full during DB record leaves partial artifact rows `mod.rs:2137-2225` | high | yes | manual | no | delete orphans / clear_preview_cache | wrap record_artifacts_locked in one transaction |
| SQLite db | Concurrent EventStore+Library, no inter-process DB lock `store/mod.rs:306-340`, `library/mod.rs:211-220` | high | yes | none | no | ensure single instance / manual repair | flock()/fcntl() on db file at open |
| Runtime IPC | Non-atomic download completion (record after renames, no fsync) `download.rs:323-333` | high | yes | auto | no | restart re-scans disk | fsync after rename; commit marker / two-phase |
| Runtime IPC | Missing encoder.int8.onnx → silent engine dispatch `pp-asr-server/main.rs:337` | high | yes | auto | no | delete model dir + re-download | validate parakeet files (config.json) before returning parakeet |
| Runtime IPC | ASR engine dispatch not validated against manifest `main.rs:335-343`, `launch.rs:138-181` | high | yes | auto | no | model re-download | pass `--engine` from manifest; file-detect only as fallback |
| SQLite db | Vector model_id/dims mismatch not caught at open `ppvec.rs:296-313`, `library/mod.rs:179-235` | med | yes | manual | no | delete .ppvec files | check_vectors_space_metadata() at Library::open |
| App data dir | clear_preview_cache races concurrent ingest → stale artifact `mod.rs:1850-1975`, `preview.rs:305-340` | med | yes | auto | yes | re-run clear_preview_cache(All) | re-pend ALL passes after clear, regardless of state |
| App data dir | Artifact DB row inserted without verifying file exists `preview.rs:870-913`, `mod.rs:2180-2186` | med | yes | auto | no | rebuild/clear_preview_cache | existence check in record_artifacts_locked before INSERT |
| preview_cache | Cache dir deleted → orphan rows, recovery delayed to doctor `mod.rs:200-207`, `preview.rs:344` | med | yes | manual | no | delete db+rescan / manual SQL | run doctor existence walk at open, not only 6h tick |
| ingest_passes | PASS_VERSION (dup of above, ingest scope) `ingest.rs:56` | med | yes | manual | no | DELETE old rows + re-ingest | see high-sev row |
| ingest_passes | model_id accepts arbitrary strings, no validation `ingest.rs` | med | yes | manual | no | doctor re-pend | normalize/validate model_id at mark_done_with_model |
| ingest_passes | defer_offline records error on pending row (invariant) `ingest.rs:158-180` | med | yes | auto | no | none | clear error on defer or add 'deferred' state |
| Tuning/Config | Malformed TOML silently falls back to defaults (warn only) `tuning.rs:821-826` | med | yes | manual | no | fix/delete tuning.toml | log at ERROR; write parse-error marker for UI banner |
| Tuning/Config | No schema versioning / skew detection for tuning.toml `tuning.rs:793-801` | med | yes | manual | no | manual re-compare vs default | optional schema_version field; warn on drift |
| Tuning/Config | OnceLock re-init ignored; edits need restart, no notice `tuning.rs:848-868` | med | yes | manual | no | restart app | file-watch INFO log "restart to apply" |
| ModelDownloads | InstalledRecord drift: partial download leaves orphan record `download.rs:188-220` | med | yes | manual | no | rm model dir + edit installed.json | startup cross-check: stat all files per installed record |
| Runtime IPC | InstalledRecord state drift / corrupt model spawned `download.rs:188-220`, `process.rs:160-200` | med | yes | manual | no | rm model dir + installed.json entry | validate model files complete before spawn |
| ModelDownloads | Over-long .part silently deleted, no log `download.rs:366-368` | med | yes | auto | no | part deleted, re-fetched | emit warn/bus event naming file+bytes |
| SQLite db | PRAGMA user_version not atomic with DDL (crash mid-migration) `schema.rs:618-758,729-748` | med | no | manual | no | inspect schema / downgrade / rebuild | wrap each migration block in transaction + version bump |
| SQLite db | No disk-full/permission-denied classification at open `schema.rs:211-215`, `library/mod.rs:179-235` | med | no | manual | no | free disk / fix perms | classify rusqlite errors (EACCES/ENOSPC vs corrupt) |
| App data dir | App-data dir deleted mid-run halts preview queue `state.rs:158-159`, `mod.rs:1700-1701` | med | no | manual | no | restore dir / restart | test-write app_data at init + periodically; warn UI |
| preview_cache | Concurrent app instances race on backfill + artifact write `preview.rs:324-340`, `mod.rs` | med | no | auto | no | restart loser, retry succeeds | .photoproof.lock single-instance guard |
| VectorStore | DB row pointer beyond file end (truncation/corruption) `ppvec.rs:532-768` | med | no | manual | yes | doctor rebuild / delete space | per-row CRC + doctor verify_vectors |
| VectorStore | Dims mismatch query vs header; MRL_DIMS not in header `ppvec.rs:52,297-314,916-921` | med | no | manual | yes | delete space + re-embed | store MRL_DIMS in header; check at read |
| VectorStore | File header corruption (bad magic/dtype, no CRC) `ppvec.rs:1325-1358` | med | no | manual | yes | delete file + re-embed | add CRC over header fields |
| Sidecar | Corrupt sidecar regen skipped if no journal `engine.rs:1352-1368` | med | no | manual | yes | restore aside / rebuild from backup | rebuild index from aside, or write snapshot-only sidecar |
| Sidecar | Newer-version sidecar blocks adjacent write, no UI warn `engine.rs:929-935`, `rebuild.rs:114` | med | no | auto | no | upgrade app | surface newer_version_files prominently at startup |
| Sidecar | Offline→writable flush acks dirty before durable `engine.rs:845-889` | med | no | auto | no | restart re-flushes | ack_dirty_row before clearing debouncer / one txn |
| Sidecar | Redaction queue row persists if adjacent routes to overflow `engine.rs:1203-1241` | med | no | manual | yes | resolve collision file | dequeue in Overflow branch (overflow already scrubbed) |
| Runtime IPC | Parakeet config.json hunt may pick wrong subdir `engine_parakeet.rs:74-86` | med | no | auto | yes | delete model dir + re-download | verify encoder.onnx present after resolving config.json |
| Runtime IPC | Port allocate-then-spawn TOCTOU race `ports.rs:10-14`, `supervisor.rs:550-580` | med | no | auto | no | auto: new port on retry | accepted by spec; aggressive restart backoff |
| preview_cache | MICRO missing on v2→v3 partial write (orphan display/thumb) `preview.rs:870-914` | low | no | auto | no | delete cache + rescan / kill+delete micro | micro write best-effort or before thumb/display loop |
| preview_cache | MICRO not regenerated reliably on gen bump (partial) `mod.rs:206`, `preview.rs:34` | med | no | auto | no | restart re-backfills | doctor reconciliation of artifact set |
| preview_cache | Stale MICRO not swept if edge/quality changes w/o gen bump `preview.rs:47-63` | low | no | none | no | manual cache clear | enforce gen bump on MICRO_EDGE/QUALITY change |
| preview_cache | Temp file stranded if write() fails before rename `preview.rs:324-340` | low | no | auto | no | auto: sweep_temp_files at open | RAII temp-file guard |
| VectorStore | Torn append (crash mid-write, partial row) `ppvec.rs:1412-1431` | low | yes | auto | no | none (next append truncates) | optional log on torn-tail truncation |
| VectorStore | Image-vector redaction guard missing (path blocked) `ppvec.rs:260-295` | low | no | manual | no | image-embed sweep | extend guard to image-keyed vectors (defensive) |
| VectorStore | Compaction crash + manual temp delete leaves stale pointers `ppvec.rs:1237-1269` | low | no | manual | no | doctor rebuild_vectors | verify rename succeeded / grace-period temp cleanup |
| VectorStore | Lock-order (conn then file) not type-enforced `ppvec.rs:258-259` | low | no | auto | no | none (design sound) | RAII guard enforcing lock order |
| ingest_passes | attempts counter i64 overflow wraps negative `ingest.rs:286,407` | low | yes | manual | no | UPDATE attempts=0 WHERE <0 | guard before increment or use u32 |
| ingest_passes | Poisoned mutex crashes recover_running at startup `library/mod.rs:194-196` | med | no | manual | no | manual SQL: running→pending | lock().unwrap_or_else(into_inner); recovery marker |
| ingest_passes | Single-writer Mutex, no read pool (contention) `library/mod.rs:158,1666` | low | no | auto | no | none | read-only pool if concurrent ingest added |
| Tuning/Config | FusionWeights::default() reads global during deser `tuning.rs:327-354` | med | no | auto | no | delete tuning.toml | debug_assert init order; #[serde(default)] on fusion |
| Tuning/Config | Truncated/invalid UTF-8 in tuning.toml not distinguished `tuning.rs:810-817` | low | no | none | no | delete tuning.toml | atomic write if app ever writes; doc backup advice |
| Tuning/Config | No protection vs concurrent external editor writes `tuning.rs:810-817` | low | no | none | no | restart if suspected corrupt | retry loop on parse fail; trust atomic editors |
| Tuning/Config | Read-only/permission-denied app-data: only warn `state.rs:159`, `tuning.rs:815` | med | no | manual | no | fix perms / delete tuning.toml | distinguish NotFound from perm/IO; log ERROR |
| ModelDownloads | installed.json no fsync after rename `download.rs:203-209` | low | no | auto | no | relaunch re-verifies SHA-256 | sync_all on models_dir after rename |
| ModelDownloads | TOCTOU double-enqueue guard `runtime.rs:589-590` | low | no | auto | no | manager serializes; harmless | move enqueue into lock scope / CAS |
| ModelDownloads | Manifest write_to() failure discarded `runtime.rs:221-223` | low | yes | auto | no | relaunch retries | log warn (signals models_dir write issue) |
| Runtime IPC | Health-probe vs busy-not-lost gauge lag → false restart `supervisor.rs:463-482` | low | no | auto | no | auto: restart resumes | time-windowed gauge / longer timeout under load |
| App data dir | v14 migration not transaction-wrapped (non-idempotent) `schema.rs:729-748` | low | no | none | no | manual cleanup roots_v14 | wrap rebuild in SAVEPOINT; clear error message |
| App data dir | Config files diverge from runtime w/o restart `state.rs:164`, `runtime.rs:194` | low | no | manual | no | restart app | documented v1 design; reload_config cmd is v2 |
| Sidecar | case_rename_cleanup delete failure swallowed (`let _`) `engine.rs:1009-1010` | med | yes | manual | no | manually delete old sidecar | return/log delete error count |
| Sidecar | Orphan temp files linger up to 1h `writer.rs:163-201`, `engine.rs:1478-1495` | low | no | auto | no | scan / manual delete | shorter ORPHAN_TEMP_MAX_AGE or sweep at startup |
| Sidecar | Manifest cross-check discrepancies advisory-only `rebuild.rs:221-261` | low | no | manual | yes | restore from backup / accept loss | option to fail rebuild on discrepancy |
| Sidecar | Session metadata conflict: first-seen wins `engine.rs:742-750` | low | no | manual | yes | re-import / fix row | merge most-recent timestamp/app_version |
| Sidecar | Offline volume transitions not atomic to pump/flush `engine.rs:815-823,873-887` | low | no | auto | no | union-merge dedupes | pre-snapshot locator per pump cycle |

## Silent-failure watchlist

These are the silentFailure=true findings (no error surfaced to user). Highest priority.

1. **Newer-version DB opens silently** `schema.rs:618-758` — v1 app reads v2 schema with incomplete knowledge and can corrupt derived data; nothing rejects user_version > 15. Fix: add `version > 15` guard returning `StoreError::IncompatibleVersion`.
2. **GENERATOR_VERSION downgrade serves future artifacts** `mod.rs:1912-1920` — downgraded binary can't see it's serving v4 artifacts to v3 code; silent visual/format degradation. Fix: persist a highest-gen-version marker row and warn/refuse on downgrade.
3. **WAL checkpoint blocked at shutdown leaks scrubbed plaintext** `store/mod.rs:1460-1520` — redacted text persists in -wal and is recoverable on next open; spec §7 redaction-supremacy violation, only logged (and log may not flush). Fix: force-close readers and retry; block exit (or warn) if still blocked.
4. **manifest_version never validated** `plan.rs:70-93` — old installed.json survives a manifest upgrade and runs stale weights; field is write-only. Fix: compare stored vs current manifest_version in local_model_plan().
5. **Missing model file not detected until load** `plan.rs:82-89` — plan declares Ready on contains_key alone; deleted/GC'd files only fail at supervisor load. Fix: spot-check critical files exist; mark NotConfigured.
6. **installed.json malformed → silent empty** `download.rs:178-183` — `.ok().and_then(...).unwrap_or_default()` makes corruption indistinguishable from first launch, triggering re-download of all models. Fix: log warn distinguishing IO vs parse; write .backup.
7. **PASS_VERSION no bump strategy** `ingest.rs:56` — a forgotten bump silently partitions the queue; old done rows ignored forever. Fix: CI/test linking PASS_VERSION to schema; doctor clears orphans.
8. **Orphaned vector rows** `ppvec.rs:189-208,523-524` — deleted .ppvec file leaves DB rows; search/score return empty while UI still shows "embedded"; space_stats returns (0,0) so compaction never triggers. Fix: doctor subscan verifying file existence per space.
9. **Model ID skew (fp16 CLIP)** `embedding.rs:95-118` — historic silent zero-affinity; now FIXED in a3b1e14. Watch: pre-fix libraries still need doctor rebuild_derived.
10. **Surround prefs diverge across webviews** `surround-store.svelte.ts:1-70` — Settings change never reaches main window until restart; no broadcast. Fix: add emit/listen mirroring theme-store.
11. **Theme-store broadcast uncommitted** `theme-store.svelte.ts` — fix exists in working tree but not committed; ship it. Fix: commit the staged diff.
12. **Case-only rename doesn't relink sidecar** `engine.rs:985-1014` — stale sidecar persists at old path indefinitely when no new event follows (test s02_2). Fix: run case_rename_cleanup at reconciliation.
13. **Old-case sidecar absorbed as truth** `engine.rs:1380-1454` — reconciliation ingests stale old-case sidecar by hash, then silently returns OK treating the (case-renamed) image as absent. Fix: detect case-mismatched adjacent in converge_file.
14. **Sidecar modify-during-read TOCTOU** `engine.rs:1336` — concurrent write during read can yield mixed bytes on NFS/SMB; parse fails or silently corrupts. Fix: retry-with-backoff on parse error.
15. **Disk-full during DB record → partial artifact rows** `mod.rs:2137-2225` — auto-commit per INSERT means Display row commits but Thumb fails, orphaning state; pass stuck running. Fix: wrap record_artifacts_locked in one transaction.
16. **Concurrent EventStore+Library, no inter-process DB lock** `store/mod.rs:306-340` — two instances can multi-write the WAL DB and silently corrupt. Fix: flock()/fcntl() on db file at open.
17. **Non-atomic download completion** `download.rs:323-333` — crash between last rename and record write leaves model on disk but unrecognized. Fix: fsync after rename / commit marker.
18. **Missing encoder.int8.onnx → silent engine dispatch** `pp-asr-server/main.rs:337` — file-existence is the only discriminator; parakeet chosen without validating its files. Fix: validate config.json before returning parakeet.
19. **ASR dispatch not validated against manifest** `main.rs:335-343` — stale sherpa files override a parakeet-pinned model; manifest benefits never activate, no error. Fix: pass `--engine` from manifest.
20. **clear_preview_cache races concurrent ingest** `mod.rs:1850-1975` — walkdir deletes a mid-write temp while pass completes; row marked done with stale/missing artifact. Fix: re-pend ALL passes after clear.
21. **Artifact DB row inserted without file-exists check** `preview.rs:870-913` — file deleted between write and INSERT leaves orphan row; 404 served until 6h doctor. Fix: existence check before INSERT.
22. **Cache dir deleted → orphan rows** `mod.rs:200-207` — recovery deferred up to 10 min to doctor; broken previews meanwhile. Fix: run doctor existence walk at open.
23. **Vector model_id/dims mismatch not caught at open** `ppvec.rs:296-313` — model skew detected only at search/upsert, late. Fix: check_vectors_space_metadata() at Library::open.
24. **model_id accepts arbitrary strings** `ingest.rs` — whitespace/encoding skew could mis-trigger or miss re-pend. Fix: normalize/validate at mark_done_with_model.
25. **defer_offline error on pending row** `ingest.rs:158-180` — violates "error NULL unless state=error" invariant; confuses integrity queries. Fix: clear error on defer or add 'deferred' state.
26. **Malformed TOML silent fallback** `tuning.rs:821-826` — custom config silently ignored, warn-only log. Fix: log ERROR; write parse-error marker for UI banner.
27. **OnceLock edits need restart, no notice** `tuning.rs:848-868` — live edits silently ignored until restart. Fix: file-watch INFO "restart to apply".
28. **InstalledRecord drift / corrupt model spawned** `download.rs:188-220` — partial new download over old install spawns mixed/corrupt model; supervisor exhausts retries to Failed with no root-cause surfaced. Fix: validate files complete before spawn.
29. **Over-long .part silently deleted** `download.rs:366-368` — `let _ = remove_file`; hides serious IO corruption signal. Fix: emit warn/bus event.
30. **Manifest write_to() failure discarded** `runtime.rs:221-223` — `let _`; stale debug panel and a missed early signal of models_dir write issues. Fix: log warn.
31. **attempts i64 overflow wraps negative** `ingest.rs:286,407` — wrapped value < MAX_AUTO_ATTEMPTS re-pends a permanently-failing row; only via corruption in practice. Fix: guard before increment.
32. **Torn append truncation** `ppvec.rs:1412-1431` — safe by design but no log on crash recovery. Fix: optional log on truncation.
33. **case_rename_cleanup delete failure swallowed** `engine.rs:1009-1010` — `let _ = remove_file`; two sidecars persist for one image. Fix: return/log delete error count.
34. **FusionWeights::default reads global during deser** `tuning.rs:327-354` — wrong-order init silently yields code defaults over file values (only if [search.fusion] omitted). Fix: debug_assert init order / `#[serde(default)]` on fusion.

## Recovery & reset gaps

Findings where recovery is none/manual or resetPath is none. The missing escape hatch is the operational risk.

**No recovery at all (recovery=none):**
- **Concurrent EventStore+Library multi-writer corruption** `store/mod.rs:306-340` — recovery=none; reset is "ensure single instance / manual DB repair." Missing: any inter-process lock. This is the only confirmed path to true silent DB corruption.
- **v14 migration not transaction-wrapped** `schema.rs:729-748` — recovery=none; failed INSERT leaves intermediate `roots_v14` and a non-idempotent migration that errors on next open. Missing: SAVEPOINT + a clear "migration failed, here's how to recover" message.

**resetPath=none (no escape hatch even manually short of nuking state):**
- **Case-only rename doesn't relink sidecar** `engine.rs:985-1014` — resetPath=none; the stale sidecar is only fixed by code change or a full scan trigger, not a documented user action.
- **defer_offline error-on-pending** `ingest.rs:158-180` — resetPath=none; harmless but no cleanup path; integrity queries must special-case it.
- **Stale MICRO not swept on edge/quality change** `preview.rs:47-63` — recovery=none, reset=manual cache clear; relies entirely on a policy ("always bump GENERATOR_VERSION") with no enforcement.
- **Truncated UTF-8 / concurrent external editor on tuning.toml** `tuning.rs:810-817` — recovery=none; only escape is delete-and-restart. Acceptable for user-edited files but undocumented.

**Manual recovery + no user warning (silent, requires SQL/CLI):**
- Newer-version DB, manifest_version skew, installed.json malformed, PASS_VERSION partition, orphaned vector rows, disk-full partial artifact rows, cache-dir-deleted orphans, model_id skew (pre-fix) — all require the user to know to run `doctor`, hand-edit installed.json, or run raw `DELETE FROM ...`. The common gap: **no doctor/self-check is wired into startup**, and **no UI affordance surfaces these states**. The single highest-leverage fix across this group is a startup-time doctor sweep (disk-vs-DB reconciliation + version/marker checks) that re-pends or warns instead of waiting for the 6-hour maintenance tick.

**Manual recovery, user warned (better, still needs a button):**
- fp16 CLIP not hosted, PPVEC row-beyond-end / dims mismatch / header corruption, corrupt-sidecar-no-journal, redaction-queue-stuck-on-collision, manifest rebuild discrepancies, session-conflict, parakeet wrong-subdir — these name the file/error so the user can act, but recovery is still hand-run `doctor rebuild_vectors` / delete-and-re-embed / restore-from-backup. Missing: an in-app "repair this" action.

## Living checklist

Reusable matrix. For each state class, check whether each failure mode is detected, recovered, and surfaced. `Y` = handled, `N` = gap (from this audit), `-` = N/A.

| State class | Newer-version / downgrade guard | Disk⇄DB reconciliation | Atomic write+commit | Crash mid-op recovery | Inter-process / multi-window lock | Disk-full / perm classified | Silent-fail surfaced (log/UI) | Reset path documented |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [ ] SQLite db | N | N | N (migration) | N (user_version) | N | N | N | N |
| [ ] preview_cache | N (downgrade) | N (doctor@open) | N (micro) | Y (sweep) | N (multi-instance) | partial | N | partial |
| [ ] VectorStore | N (MRL_DIMS) | N (orphans) | Y (2-phase) | Y (torn tail) | Y (single-proc) | - | N | Y |
| [ ] ModelDownloads | N (manifest_ver) | N (file stat) | N (fsync) | partial | N (TOCTOU) | N | N | partial |
| [ ] ingest_passes | N (PASS_VERSION) | - | Y | N (poison) | N (single-writer ok) | - | N | partial |
| [ ] Tuning/Config | N (schema_ver) | - | - (user-edited) | - | N (OnceLock) | N | N | N |
| [ ] Frontend prefs | N (range drift) | - | - | - | N (broadcast) | - | N | partial |
| [ ] Sidecar | Y (NewerVersion) | N (case mismatch) | Y (temp+rename) | partial (no-journal) | N (TOCTOU read) | N | N | partial |
| [ ] Runtime IPC | N (manifest vs files) | N (model files) | N (download) | Y (supervisor) | partial (port race) | - | partial | Y |
| [ ] App data dir | N (gen downgrade) | N (row⇄file) | N (write/record) | N (WAL@shutdown) | N | N | N | partial |

When you add a new state class / version constant, verify (rules distilled from the findings):

1. **Every version constant has an upper-bound guard AND a downgrade detector.** If you read `user_version` / `GENERATOR_VERSION` / `PASS_VERSION` / `manifest_version` / `schema_version`, reject (or warn) when the stored value exceeds what the binary supports — do not let `if x < N` chains silently no-op (Newer-version DB, GENERATOR_VERSION downgrade).
2. **Persist the highest version ever written, not just per-row versions.** A per-row version cannot detect a downgrade; a single marker row can (App data dir downgrade).
3. **If a version constant gates queue/cache partitioning, wire an automatic re-pend on bump and a test asserting it moves in lockstep with the schema.** GENERATOR_VERSION does this; PASS_VERSION does not — that asymmetry is the bug.
4. **Any stored version field must be read somewhere.** A write-only version field (manifest_version) is a latent skew bug; add the comparison at the same time you add the field.
5. **Disk and DB must be reconciled at startup, not only on a 6-hour tick.** If a DB row implies a file (preview_artifacts, vectors, installed.json, models), walk/stat it at open and re-pend or warn on mismatch (orphaned vectors, deleted cache dir, missing model files, orphan artifact rows).
6. **File write + DB record must be one atomic unit, or detect the gap.** Wrap multi-INSERT records in a transaction; fsync after the final rename; and either verify file existence before INSERT or have a sweep that heals orphans (disk-full partial rows, non-atomic download).
7. **For any new persisted file, define the corruption-vs-collision-vs-truncation taxonomy and surface it.** Distinguish NotFound (expected) from parse/IO error (unusual); log the latter at ERROR and leave a UI-detectable marker (installed.json, tuning.toml, sidecar classify).
8. **Never `let _ = remove_file(...)` / `let _ = write_to(...)` on integrity-relevant ops.** Swallowed deletes/writes hide IO corruption signals (over-long .part, case_rename_cleanup, manifest write_to).
9. **Multi-process or multi-webview state needs an explicit lock or broadcast.** A process-level instance.lock does not guard DB files; per-webview singletons do not share localStorage. Add flock on shared files; add emit/listen for shared prefs (EventStore+Library, surround/theme stores).
10. **Validation ranges and dispatch discriminators must be derived, not hard-coded.** Validate against `ARRAY.length`, header-stored params, or the manifest — not a literal that drifts (thumbStep `<= 3`, ASR file-existence dispatch, MRL_DIMS).
11. **Redaction/security paths must confirm durability before declaring success.** A blocked WAL checkpoint at shutdown must not be treated as done — force-close readers and retry, or block exit (WAL plaintext leak).
12. **Every silent failure needs at minimum a log line, ideally a doctor entry and a user-visible state.** "Logged to a file the user never reads" still counts as silent for this checklist.

## Refuted / out of scope

10 findings were refuted by code review and are not actionable:
- **PPVEC truncated-header silent loss** — bounds checks (`.get(start..start+dims)`) on every read path plus file-first-commit ordering prevent silent loss; recovery never pairs remapped pointers with the old file.
- **gen_version bump doesn't validate files** — re-enqueued passes unconditionally regenerate; serve returns 404 (not silent); doctor heals.
- **Missing recovery for vector-space remap after crash** — marker is a durable SQLite row committed atomically with the remap, not a best-effort file; two-phase recovery is correct.
- **Silent /micro 404 with no fallback** — frontend GraphThumbCache implements and tests the /micro→/thumb fallback; 404 is expected, fallback always precedes the error placeholder.
- **scale/offset quantization drift** — current constants (1/127, 0.0) round-trip exactly; tests assert exact equality; data-derived calibration is a documented future design point, not a live bug.
- **DB dims vs header dims divergence** — upsert validates incoming length against existing dims (rejects on mismatch); all reads use header.dims; no silent wrong-data path.
- **Model-aware re-pend skips legacy NULL rows permanently** — enqueue_embedding_backfill creates fresh pending rows; repend targets done rows only by design; pending rows are claimed normally.
- **No model_id validation (whitespace/encoding skew)** — same immutable embedder instance used for store and compare within a drain; worst case is one extra re-pend cycle.
- **No NULL-safety on repend_passes_for_model** — current_model_id is `&str` (cannot be NULL); SQL uses the correct `(model_id IS NULL OR model_id <> ?)` idiom.
- **Truncated UTF-8 sidecar misclassified as collision** — theoretical only; all writes go through write_atomic (temp+sync+rename), so the target never holds a partial write; in-memory serialization cannot truncate.

Out of scope by verifier downgrade (real but lower risk than originally filed, kept in tables at adjusted severity): image-vector redaction guard (path architecturally blocked), MICRO orphan on partial write (transaction fails, no DB rows), port-bind race (spec-accepted), config-requires-restart (documented v1 design).
