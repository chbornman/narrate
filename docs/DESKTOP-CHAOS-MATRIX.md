# Desktop lifecycle chaos matrix

Status: executable A25 evidence packet, July 27 2026.

The authoritative requirement is A25 in `docs/BACKLOG.md`. This packet does
not treat source inspection or a checklist as acceptance evidence. The runner
at `apps/desktop/scripts/run-desktop-chaos-matrix.mjs` invokes the real Rust and
Vitest acceptance binaries, then emits JSON for every case with:

- lifecycle phase and host platform;
- exact suite command and result;
- result for each applicable invariant; and
- an explicit platform-drill reason where a local fixture is not equivalent
  to the real failure.

Run all locally executable suites from the repository root:

```text
node apps/desktop/scripts/run-desktop-chaos-matrix.mjs
```

Run or enumerate one focused packet:

```text
node apps/desktop/scripts/run-desktop-chaos-matrix.mjs --suite archived_roots
node apps/desktop/scripts/run-desktop-chaos-matrix.mjs --list
```

The process exits nonzero when any executed suite fails. A platform drill is
never relabeled as passed merely because its deterministic fixture passed.

## Local execution record

The settled-source Linux run on July 27 2026 executed all 15 suites
successfully. The report contains 28 cases: 23 passed locally and 5 recorded as
`fixture-passed-platform-drill-pending`. In particular:

- desktop library: 290 passed, 3 ignored;
- core library: 323 passed, 2 ignored;
- library acceptance: 36 passed, 2 ignored;
- runtime download: 20 passed, 1 ignored;
- runtime process: 6 passed;
- frontend boot/settings: 35 passed; and
- frontend runtime/library status: 37 passed.

The machine-readable report was emitted by:

```text
node apps/desktop/scripts/run-desktop-chaos-matrix.mjs \
  > /tmp/photoproof-a25-final-2026-07-27.json
```

The runner needs loopback permission for its local download and child-runtime
fixtures. A restricted-sandbox run therefore reports those suites failed with
socket `PermissionDenied`; that is an environment refusal, not passing or
failing product evidence.

One earlier full-suite pass missed the managed-task test's one-second shutdown
acknowledgement while the shared build tree was under load. The focused test,
the next full desktop suite, and the canonical matrix all passed. This remains
tracked as timing-sensitive hardening: a retry is diagnostic evidence, not a
reason to erase the observed deadline miss.

## Invariants

| ID | Required observation |
|---|---|
| `window-usable-independent-of-optional-work` | Optional volume, runtime, cache, or probe work cannot hold the usable shell hostage. |
| `authored-truth-preserved` | Journals, collection membership, settings commits, and root identity survive failure/recovery. |
| `derived-state-valid-or-repended` | Cache/vector/model/index state is verified, quarantined, repaired, or durably queued for repair. |
| `work-terminal-or-paused` | Work ends Completed/Failed/Cancelled or remains deliberately dormant without retry churn. |
| `ui-projects-committed-backend-truth` | Backend snapshots and events expose failure, fallback, recovery, and terminal state. |
| `no-unowned-child-or-writer-after-clean-shutdown` | The shutdown barrier rejects new work and joins or kills every owned worker/child. |

## Executable evidence map

The runner is the machine-readable matrix. The table below names the most
specific assertions behind each group; broad suite commands also execute
adjacent invariants so a focused test cannot conceal an integration regression.

| Phase | Required cases | Primary executable evidence |
|---|---|---|
| Startup | unavailable/hard NAS; unplugged external drive | `library_acceptance::incomplete_root_walk_never_stales_unseen_paths`, `l13_08_offline_volume_never_burns_attempts_and_poisoned_rows_heal`; desktop lifecycle/health suite |
| Startup | corrupt settings/config/installed registry | desktop `settings::*corrupt*`, `runtime::tests::corrupt_config_recovers_lkg_*`, `model_registry::tests::missing_index_recovers_only_fully_hash_valid_manifest_files` |
| Startup | newer DB; interrupted migration | core `store::schema::tests::refuses_a_newer_schema_version`, `every_migration_batch_and_version_boundary_rolls_back_then_resumes`, `v14_statement_failures_roll_back_and_resume` |
| Startup | full disk | desktop `disk::tests::capacity_thresholds_are_strict_and_unknown_is_not_zero`, `runtime::tests::disk_shortfall_blocks_only_a_real_known_shortfall`; core download disk-admission tests |
| Startup | missing child binary/runtime library | desktop `supervisors::tests::plan_run_without_binary_records_a_blocked_reason`; ORT resolver tests; runtime supervisor convergence suite |
| Startup | slow/hung hardware probe | desktop `hardware::tests::hung_probe_is_killed_at_the_deadline`, `capability_probe_cancellation_kills_and_reaps_the_helper`, runtime provisional-capability tests |
| Startup | corrupt/same-size model | desktop model registry hash-recovery and disagreement tests; core `runtime_download` same-size/checksum tests |
| Live | add/archive/remove/re-add root | desktop `commands::library::tests::archive_remove_and_readd_transfer_watcher_and_scan_ownership`; core `archive_root_pauses_background_lifecycle_but_keeps_search_truth` |
| Live | volume offline/online | core `l13_06_volume_remount_new_mount_point`, `l13_08_offline_volume_never_burns_attempts_and_poisoned_rows_heal`, sidecar offline deferral |
| Live | watcher overflow | deterministic `library_watcher` overflow/reconcile assertions |
| Live | sleep/resume | desktop wall-gap boundary tests plus core `Library::on_system_resume` reconciliation cases |
| Live | model download/remove/reinstall | desktop serialized-operation tests and core `runtime_download` resume/verify/install tests |
| Live | tier/config change | desktop runtime capability tests plus `supervisors` Run-A/Run-B, argv-change, dark/return transition matrix |
| Live | GPU fallback | connector provider fixtures, desktop actual-provider projection, frontend runtime status projection |
| Live | runtime crash budget/restart | core `runtime_supervisor` fresh-budget tests and OS-process SIGKILL/recovery suite; desktop explicit restart tests |
| Live | cache deletion | core library doctor/cache regeneration and orphan-retention suites, including archived repair dormancy/resume |
| Live | multi-window mutation | mock-Tauri broadcast tests plus frontend cold-read/event revision arbitration and listener-health tests |
| Shutdown/crash | scan; queue; doctor | cancellable scan/queue fixtures, incomplete-walk no-stale invariant, managed-task barrier, `doctor_cancellation_stops_before_a_new_repair_unit_and_resumes_cleanly`, process SIGKILL recovery |
| Shutdown/crash | model build; download | embedder stale-landing/shutdown matrix, killable capability helper, terminal download settlement |
| Shutdown/crash | sidecar flush; collection flush | sidecar kill-during-write and byte-determinism suite; collection canonical file, debounce, flush, corrupt-file recovery suite |
| Shutdown/crash | WAL checkpoint | migration transaction/failpoint suite, backup destroy/restore drill, application shutdown ordering |

## Archived-root contract

Archive is a non-destructive resting state:

1. The root and its active path rows remain durable. Journals, collection
   membership, previews, and authored search results remain available.
2. The root leaves active-root health, offline-volume burden, startup/resume
   reconciliation, stale inference, and watcher ownership.
3. Pending filesystem and model-derived passes remain `pending` but are not
   claimable while every retained path is archived. This is intentional pause,
   not retry churn.
4. One duplicate path under an active root keeps the image eligible.
5. Unarchive restores watcher ownership, starts reconciliation, and resumes the
   same pending derived repairs. Remove/re-add revives the same root identity.
6. Archive/remove signal any in-flight root-scan token before the durable state
   flip. A late scan also fails the active-root location guard.

Executable proof:

- `library::ingest::tests::archived_root_pauses_every_pass_until_an_active_path_exists`
- `library_acceptance::archive_root_pauses_background_lifecycle_but_keeps_search_truth`
- `commands::library::tests::archive_remove_and_readd_transfer_watcher_and_scan_ownership`
- `commands::health` active-root projection tests

## Platform-only evidence still required

These drills cannot be honestly closed by an in-process simulation:

| Drill | Why fixture evidence is not the same fact |
|---|---|
| Installed shell with a kernel-blocked NAS mount | Cooperative cancellation and an offline fake volume do not reproduce an uninterruptible kernel filesystem call. |
| Real OS suspend/resume on Linux, macOS, and Windows | A fake wall-clock jump proves detection/reconcile logic, not platform event delivery, mount recovery, or native watcher behavior. |
| Real CoreML, CUDA, and TensorRT fallback | Forced provider fixtures prove state projection; only matching hardware/runtime libraries prove actual provider selection. |
| Two native webviews mutating concurrently | Mock event broadcasts and revision arbitration prove ordering, not native webview/plugin delivery. |
| Installed-app quit/kill during hard filesystem and native-runtime phases | Unit/process fixtures prove atomicity and ownership; installed packages must also prove platform signal, sandbox, mount, and runtime behavior. |

Each remains `fixture-passed-platform-drill-pending` in runner output until the
matching installed-platform record is attached. This packet therefore supplies
the executable local matrix without weakening the broader A25 completion rule.
