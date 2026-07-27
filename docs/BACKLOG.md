# BACKLOG — deferred features & ideas, consolidated

The TODO list. One home for everything decided-but-not-scheduled, scattered
until now across UI-FEATURESET §9, DECISIONS K17, and the founder checklist.
Maintained by the coordinator; items graduate into packets via the build
loop. The vision filter applies to every line (reviewing/processing = core;
managing = off-thesis). Shipped items move to LANDED.md verbatim — only open
work lives here.

## Desktop application foundation audit - 2026-07-26

Read-only sweep of startup, shutdown, background work, roots/volumes,
previews/caches/vectors, persisted control files, model/runtime convergence,
degraded-state UX, observability, packaging, and release operations. Overall:
the domain/core repair primitives are strong, but the desktop lifecycle is
implicit and needs one coordinator before the app can reliably promise
"opens quickly, tells the truth, heals itself, and closes cleanly."

Existing items are PART OF this program and stay canonical where already
recorded (S5 is now landed; S6/D4/D6/D7/D8 retain their current statuses);
Dogfood round 5's backend-aware model offer,
terminal 100%-download settlement, ingest intensity controls, unresponsive-
volume startup hang, and CUDA/provider observability. Do not create parallel
implementations when packetizing this section.

### P0 - runtime state machines that can strand required services

- [x] **Make `SupervisorHost` truly converge desired runtime state**
  - Track a comparable desired spec/model id for each ASR/LLM slot.
  - Implement `Run A -> Run B`, `Run -> dark -> Run`, binary
    disappear/reappear, remove/reinstall, tier/config changes, and retired-slot
    cleanup. Today `apply()` stops a supervisor but leaves the occupied slot,
    and only creates when `slot.is_none()`, so a later valid plan can remain
    stopped forever. `apps/desktop/src-tauri/src/supervisors.rs:267-381`.
  - Done means a shell-level transition-matrix test pins every transition and
    proves changed launch arguments/models replace the old child after a
    bounded stop.
  - Delivered 2026-07-27: each role retains comparable desired/current
    `SupervisorTarget` state (model id, program, and argv), converges changed
    targets by bounded retirement/replacement, clears dark slots, and recreates
    roles after binary reappearance. `supervisors.rs` pins Run A -> Run B,
    same-model launch-spec change, Run -> dark -> Run, binary disappearance/
    reappearance, explicit restart, terminal shutdown, and bounded ticker join.

- [x] **Make "Restart runtime" perform a real restart**
  - Call the core supervisor fresh-budget restart, restart/rebuild failed
    embedders, clear terminal backoff where appropriate, and re-apply the
    current plan. It must not merely clear download errors while Settings says
    "Restarted." `apps/desktop/src-tauri/src/runtime.rs:966-972`;
    `apps/desktop/src/lib/settings/SettingsApp.svelte:560-569`.
  - Done means a five-crash `Failed -> restart -> Spawning -> Ready` acceptance
    test exists for child supervisors and an embedder load failure has an
    equivalent retry-now path.
  - Delivered 2026-07-27: `RuntimeHost::restart_runtime` clears download
    errors, gives every desired supervisor a fresh attempt budget, retries only
    failed embedder slots, and reapplies the current plan. Core acceptance
    `s8_1_rolling_window_budget_then_failed_then_fresh_budget_on_restart_action`
    proves crash-budget exhaustion and `Failed -> Spawning -> Ready`;
    `explicit_restart_resets_only_failed_embedder_slots` pins the embedder path.

- [x] **Stop offering the unhosted CUDA-default fp16 CLIP artifact now**
  - This is the immediate safety half of D8: either remove
    `ViT-H-14-378-quickgelu__dfn5b-fp16` from public tiers/default selection or
    make pin validation reject its `local-fp16-convert` staged-only revision.
    Re-enable only after an immutable hosted pin is real. Current CUDA config
    selects a fresh-install download that is knowingly unavailable.
    `crates/photoproof-connectors/src/config.rs:386-408`;
    `crates/photoproof-core/src/runtime/manifest.rs:523-568`.
  - Delivered 2026-07-27: the hosted int8 DFN5B artifact is the public default.
    The fp16 recipe remains staged with no offered tiers, and
    `local-fp16-convert` deliberately fails immutable-pin validation.
    `fp16_clip_is_staged_unpinned_and_offered_at_no_tier` pins all three facts.

### P1 - one explicit application lifecycle

- [~] **Introduce an `AppLifecycle` coordinator and show a usable window early**
  - Model at least `Cold -> OpeningData -> Usable -> Reconciling -> Ready`,
    plus independent degraded states for database, roots/volumes, runtime,
    previews, vectors, and settings.
  - Keep only the minimum database/open contract before `Usable`; move volume
    probes, watcher creation, hardware detection, native VAD/model session
    construction, and integrity walks off Tauri's setup thread.
    `apps/desktop/src-tauri/src/lib.rs:162-210`;
    `apps/desktop/src-tauri/src/state.rs:157-282`.
  - The already-recorded unresponsive-network-volume launch item is the first
    acceptance case. Done means the main window maps promptly with an offline
    root while a hard/unresponsive mount probe remains blocked or times out.
  - Delivered deterministic slice 2026-07-27: `AppLifecycle` now models
    `Cold -> OpeningData -> Usable -> Reconciling -> Ready -> Stopping` with
    independent subsystem health. `App::init` opens only minimum durable state
    before `Usable`; hardware/model/capture construction, volume/watcher
    restoration, integrity work, and pumps start afterward as owned tasks.
    `blocked_startup_volume_probe_does_not_hold_the_usable_barrier` injects a
    probe that remains blocked and proves the app stays usable while the work
    remains observable. Residual acceptance is a real native hard/unresponsive
    network-mount launch receipt proving actual window-map latency; do not
    round this item up without that platform drill.

- [x] **Give frontend boot explicit loading, degraded, retry, and fatal states**
  - Stop fire-and-forget `void ui.init()` from silently stranding a partial UI.
    Catch boot-critical failures, retain the successful subsystem snapshots,
    show a useful recovery action, and parallelize independent post-first-paint
    reads. `apps/desktop/src/App.svelte:210-267`;
    `apps/desktop/src/lib/state/app.svelte.ts:458-529`.
  - Done means injected failures for settings, roots, initial folder, ingest
    status, runtime, collections, and topics each render an honest state and
    recover without relaunch where recovery is possible.
  - Apply the same contract to the Settings window: its current `void
    refresh()` serially loads roots, runtime, settings, and cache, aborts the
    rest on the first error, and has no explicit recovery state.
  - Add monotone snapshot revisions or equivalent arbitration between cold
    reads and live settings/roots/collections/runtime events. A slower launch
    response must never overwrite a newer event-delivered snapshot.
  - Settings boot now arbitrates its own cold reads against live roots,
    settings, and runtime events, but the backend payloads still carry no
    monotone revision. A failed Tauri event subscription can only be logged,
    and cache changes made by another window have no live event at all; add
    revisioned snapshots/subscription health before treating cross-window
    convergence as proven.
  - Treat event-listener installation as boot health in both windows. A
    rejected subscription currently leaves that window permanently stale
    while only logging or ignoring the error; expose retry and backend
    revision/catch-up semantics instead.
  - Settings post-boot mutations still use `void` async event handlers without
    one action-error/single-flight surface. A rejected add/remove folder,
    cache clear, model operation, export, or rebuild can therefore become an
    unhandled rejection even though boot itself now settles safely. Give these
    actions the same explicit pending/failed/retry discipline.
  - Make topic-to-collection bake one transaction or compensate on failure.
    It currently creates the collection before all member hashes are parsed
    and added, so invalid input or a later add failure can leave an empty,
    unannounced collection. Topic suggestions also suppress some collection
    list/member read failures and return incomplete healthy-looking results.
  - Make folder navigation commit intentionally: `openFolder` currently
    changes scope/root before `list_folder`, then replaces items before
    `folder_tree`, exposing a partially advanced view when either read fails.
  - Distinguish an unsupported archived-roots command from a transient read
    failure; do not silently map both to an empty healthy archive.
  - Remove the unrelated unawaited `reportScope` write that `applySettings`
    can dispatch during boot when stack-display settings change.
  - Make root lifecycle commands transactional at the product-state level.
    Add/unarchive currently commits an active root before scan-task dispatch
    and watcher installation are known to succeed. A dispatch failure must
    leave a durable degraded/retryable state rather than returning a healthy
    active root with no owned convergence work.
  - Delivered 2026-07-27: main and Settings boot now settle independent
    dependencies into explicit loading/ready/degraded/fatal states, retain
    successful snapshots, retry failed dependencies/actions, arbitrate cold
    reads against revisioned live events, and expose event-subscription health
    with catch-up. Folder navigation and topic/root mutations commit
    transactionally. `boot-state.test.ts`, `settings-boot.test.ts`, and
    `settings-window-boot.test.ts` cover every named injected dependency
    failure, recovery without relaunch, event gaps, and atomic view changes.

- [x] **Create a managed background-task registry**
  - Every pump, scan, startup doctor, resume reconcile, watcher activation,
    model build, download, and plan-convergence loop needs an owner,
    cancellation token, single-flight key, priority, start time,
    progress/last-error snapshot, and join handle or bounded shutdown
    acknowledgement.
  - Replace detached threads and the endless plan-converge loop with managed
    tasks that cannot outlive shutdown or silently duplicate work.
    `apps/desktop/src-tauri/src/state.rs:218-228`;
    `apps/desktop/src-tauri/src/pump.rs`; `apps/desktop/src-tauri/src/doctor.rs`.
  - Remaining direct-thread inventory: the supervisor drive loop
    (`supervisors.rs`), both ORT embedder builders (`embedders.rs`), and the
    post-disarm mic drain (`mic.rs`) still lack registry ownership. The
    supervisor host stores no tick-thread join handle; `shutdown()` currently
    only flips its stop flag. Model downloads are now registered and settle
    product state on cancellation, but a blocking DNS/connect/body read has no
    cooperative cancellation boundary and can still miss the three-second
    join deadline; configure a bounded transport or isolate it in a killable
    process before calling that shutdown proof complete.
  - Audit Tauri `spawn_blocking` command work as well as explicit OS threads:
    mutating commands accepted immediately before quit can still be running
    after the finalization barrier begins. Classify the bounded preview
    protocol pool and external-program child reaper as explicit process-scope
    exceptions only after proving they cannot mutate durable state after that
    barrier.
  - Route every database/filesystem IPC operation through a process-wide
    admission registry. Closing it must reject queued late commands and join
    already-admitted reads and mutations before finalization; a Tauri
    blocking-pool future is not itself application ownership.
  - Delivered 2026-07-27: `ManagedTaskRegistry` provides owner/key
    single-flight, priority, cancellation, timing, progress/error snapshots,
    terminal history, and bounded acknowledgement. Pumps, scans, watcher
    restore, doctor/reconcile, live control, registry recovery, vector repair,
    and downloads use it; `CommandWorkRegistry` closes IPC admission and joins
    accepted reads/mutations. Supervisors retain and join their owned ticker;
    ORT builds/inference run in hard-killable same-executable helpers;
    microphone initialization/live/post-drain work is explicitly stopped and
    joined. The fixed preview protocol pool and external-program reaper are
    process infrastructure and cannot mutate durable application state. Tests
    prove single-flight, observable failures, late-work rejection, and zero
    live managed tasks after the two-phase barrier.

- [x] **Separate ingest, volume monitoring, repair, and derived backfill lanes**
  - Volume probes and full maintenance/reconciliation must never run
    synchronously on the ingest pump. Split interactive RAW development,
    live ingest, preview work, embedding backfill, root reconciliation, volume
    polling, and status publication into independently paced/prioritized work.
  - Correct the documented six-hour maintenance policy versus the currently
    wired 600-second interval, and avoid the maintenance tick probing volumes a
    second time. `apps/desktop/src-tauri/src/pump.rs:37-40,358-400`;
    `crates/photoproof-core/src/library/mod.rs:1262-1275`.
  - Log and surface drain/probe/maintenance failures; never turn a failed
    `process_queue` into a default empty report. Pair with the existing ingest
    intensity/pause/Eco-Balanced-Max item.
  - Delivered 2026-07-27: essential ingest/status, preview generation,
    interactive RAW development, embedding backfill, volume polling, and
    maintenance/reconciliation are independently paced managed lanes.
    Resource-governor admission preserves interactive priority, capture
    preemption, pause/intensity policy, and bounded batches without serializing
    the lanes behind ingest idleness. Maintenance is six-hour, does not reprobe
    volumes, and records failures. The long-lived-lane shutdown test includes
    all three derived workers.

- [~] **Implement coordinated, bounded shutdown**
  - Transition lifecycle to `Stopping`, reject new work, cancel scans/doctor/
    builds/download dispatch, stop watchers, and join or receive bounded
    acknowledgements from every database/filesystem writer before session,
    sidecar, collection, and WAL finalization.
  - Preserve the current correct ordering of mic drain before ASR stop.
    `apps/desktop/src-tauri/src/state.rs:335-385`.
  - Done means quitting during initial scan, resume reconciliation, preview
    generation, embedding, model load, download, and sidecar flush is tested;
    no worker writes after the final checkpoint begins.
  - Include the detached post-disarm mic drain in quit-at-phase coverage: it
    pumps capture events into `EventStore` for up to five seconds, does not
    currently check `App::shutdown`, and can overlap the quit drain/final
    checkpoint. Download workers and late command `spawn_blocking` mutations
    need the same explicit admission/cancel/join barrier.
  - Treat failure to spawn the live microphone thread as a failed arm
    transition. The capture engine must not remain armed behind a `MicHandle`
    that owns no thread; surface the error and roll back to disarmed.
  - Complete microphone device/stream initialization before acknowledging a
    stable armed transition, or expose an explicit `arming` state. Initialization
    currently happens inside the spawned worker, so a missing device can
    briefly return `armed` before the worker corrects it to disarmed; the
    finished handle also remains stored until the next toggle or shutdown.
  - Delivered microphone slice 2026-07-27: the arm command now waits for an
    explicit acknowledgement emitted only after device selection, input
    configuration, stream construction, and `stream.play()` succeed. Missing
    devices, stream-init errors, thread-spawn failure, timeout, and quit during
    the injected initialization seam stop/join the worker, return an error, and
    roll the capture engine and download-pacing flag back to disarmed.
    `MicHandle::is_active` includes JoinHandle completion, terminal workers
    remove only their exact generation, and capture rejects a returned handle
    that has already finished. The managed post-disarm drain remains
    cancellation/join covered by the process task barrier.
  - Evidence: microphone lifecycle tests 11/11 and capture worker transition
    tests 3/3 passed; full desktop library 273/3 ignored passed. Residual
    platform fact: CPAL's native device/config/build/play calls expose no
    force-cancel API. Shutdown cancellation is deterministic around that
    boundary, but an OS backend call wedged inside CPAL can delay the join
    until the native call returns; a real device-removal/platform drill remains
    necessary.
  - Delivered deterministic slice 2026-07-27: shutdown transitions to
    `Stopping`, closes download/task/IPC admission, cancels and joins owned
    filesystem/DB work before finalization, drops watchers, preserves mic drain
    before supervisor stop, kills/reaps ORT helpers, and skips all final writes
    if the barrier times out. The managed quit phase matrix covers initial
    scan, resume reconcile, preview, embedding, download, and sidecar work;
    `quit_during_model_load_kills_and_reaps_the_helper_before_acknowledging`
    covers a native model load. Barrier tests prove checkpoint ordering and the
    no-write-on-timeout branch. The sole remaining hard-bound gap is the native
    CPAL wedge above: `MicHandle::drop` joins the OS audio thread and cannot
    impose a deadline while a backend call itself refuses to return.

### P1 - health, repair, and persistent-state integrity

- [x] **Publish one application-health snapshot and recovery surface**
  - Include boot phase; DB/schema/WAL; settings/config/device identity; disk
    space; each root/volume/watcher; scan/ingest queues; preview/cache/vector
    integrity; model registry/files/providers; supervisors; and current repair
    jobs.
  - Each unhealthy state must name whether it is blocking or degraded, its last
    error/time, and a precise safe action such as Retry volume, Verify model,
    Rebuild previews, Retry runtime, Reveal logs, or Restore defaults.
  - Settings and the header should consume backend truth rather than inventing
    separate staleness/loading stories.
  - Represent fatal storage/open/migration failure before `App` can be fully
    managed. The current health command exists only after successful
    initialization, so the most important blocking failure aborts Tauri setup
    before any recovery UI can query it.
  - Delivered 2026-07-27: `ApplicationHealth.issues` is the one product
    projection across lifecycle, DB/schema/WAL, roots/volumes/watchers,
    managed work, settings/config/tuning/device recovery, disk/cache,
    ingest/repair, runtime capabilities/providers/models/helpers, supervisors,
    and diagnostics. Every issue carries subsystem, blocking/degraded truth,
    summary, retained error/time, and a closed action vocabulary whose verbs
    all invoke real safe commands. Settings executes those actions through its
    serialized retryable lane and refreshes backend truth; the main header
    consumes the same projection instead of inventing a second status model.
    Pre-`App` database/open/migration failure remains queryable through the
    separately managed bootstrap state and renders a real relaunch surface.
  - Evidence: health projection tests 5/5 (including blocked/critical WAL and
    queued/building model-helper work), desktop library 263/3 ignored at the
    health handoff, full frontend 98 files/1,242 tests, rendered health/header
    tests 46/46, strict desktop Clippy, and Svelte check 0/0.

- [x] **Make repair scheduling readiness-driven and best-effort per subsystem**
  - Split safe startup vector orphan cleanup from active-model reconciliation;
    rerun active-space reconciliation on
    `Embedder Ready(model_id,generation)` so the startup doctor cannot race an
    asynchronously loading model. `apps/desktop/src-tauri/src/doctor.rs:34-78`;
    `apps/desktop/src-tauri/src/runtime.rs:396-427`.
  - Key readiness repair by `(role, model_id, ready_generation)`, not model id
    alone. A same-model unload/failure/reload is a new readiness generation and
    must receive a fresh integrity pass even though its model id is unchanged.
  - Prevent startup doctor and live ingest from creating an uncontrolled I/O
    storm.
  - Make `reconcile_all` continue to later roots after one non-offline root
    error, returning a per-root report instead of aborting the whole sweep.
    `crates/photoproof-core/src/library/mod.rs:1230-1246`.
  - Treat an unreadable/missing root walk as incomplete evidence, not an empty
    directory. Today `WalkDir` can increment `io_errors` and continue to the
    unseen-row phase, which can stale every indexed path under an allegedly
    online root. Suppress destructive stale/move inference unless the root walk
    completed authoritatively; surface the root as degraded and retry it.
    `crates/photoproof-core/src/library/scan.rs`.
  - S5's conservative orphan preview/all-vector retention cleanup is complete;
    keep its report and relink repair visible through this program's health
    snapshot rather than building a second cleanup loop.
  - Delivered 2026-07-27: repair is keyed by role, model id, and ready
    generation; each root settles independently; incomplete metadata/walk
    evidence cannot stale unseen paths; and retained S5/relink reports remain
    visible and retryable through Application Health.

- [x] **Make schema upgrades transactional and recoverable**
  - Acquire the process/database migration lock before any connection can
    migrate, create a verified pre-upgrade backup, run an upgrade atomically
    where SQLite permits, and make each step idempotent after interruption.
    The v14 table rebuild is the priority crash-injection case.
    `crates/photoproof-core/src/store/schema.rs:639-646,738-775`.
  - Keep the already-landed newer-schema downgrade refusal.
  - Done means failure injection at every migration statement either rolls
    back to the prior schema/data or resumes safely on the next launch.
  - Shipped: the whole ladder runs beneath one `BEGIN IMMEDIATE`; the version
    is read only after that writer reservation, so racing processes serialize
    and the waiter rechecks the winner's committed version. Existing databases
    receive a deterministic SQLite online-backup artifact whose integrity and
    source version are independently verified before the first upgrade
    statement. `concurrent_migrators_serialize_and_recheck_version` and
    `on_disk_upgrade_writes_verified_pre_upgrade_backup` cover both contracts.
  - Shipped: multi-statement migration programs are divided with SQLite's own
    `sqlite3_complete` parser, then executed one statement at a time inside the
    same transaction. This preserves trigger bodies and quoted semicolons while
    exposing every literal SQL boundary, not merely each former
    `execute_batch`, to fault injection. The exhaustive v1-v16 matrix
    `every_migration_statement_and_version_boundary_rolls_back_then_resumes`
    compares the exact pre-upgrade schema/version after every injected failure,
    retries to head, and runs `integrity_check`; the dedicated v14 table-swap
    matrix also verifies the original row and narrow CHECK survive every
    interruption. The complete core crate suite is green (including 317 unit
    tests, with 2 fixture/perf tests intentionally ignored), and strict
    all-target core Clippy is green.

- [~] **Harden settings/config/device-id/consent control files**
  - Distinguish missing from corrupt; quarantine invalid files; keep a
    last-known-good copy; write atomically with file and parent-directory
    durability; expose recovery instead of silently substituting defaults.
  - Never silently mint a new replica/device identity because its file became
    malformed. `apps/desktop/src-tauri/src/settings.rs:79-115`.
  - Make `config.toml` validation and unsupported options visible in product
    status, not debug-only lines; define reload/apply/rollback semantics rather
    than launch-only behavior. Resolve relative model paths against an explicit
    base, never process CWD.
  - Apply the same typed, durable contract to runtime consent, per-license
    acceptances, tier cache, compiled-manifest publication, and the supervisor
    child registry. Current reads often collapse missing, corrupt, permission,
    and unknown values; several writes mutate memory before an ignored/unsynced
    disk failure, use a static temp path, or silently default corrupt records.
    The UI must acknowledge only a committed consent/license/config mutation.
  - Retain settings/device-id recovery metadata in `App` health instead of
    calling the compatibility loader that logs an unrecoverable settings fault
    and continues with unlabelled in-memory defaults. Define a product action
    and retention policy for quarantined control files.
  - Make every control mutation persist-then-publish: several settings
    commands currently edit the shared in-memory value before `save()`, so a
    failed disk commit returns an error while the running process keeps using
    the rejected value. Build a candidate copy, commit it, then swap/broadcast;
    apply the same rollback invariant to runtime consent and acceptances.
  - Bring `tuning.toml` under the same inventory. It is another launch-time
    control file whose read errors currently collapse to defaults without
    last-known-good recovery, live reload semantics, or product health.
  - Remove or explicitly confine legacy convenience loaders that discard the
    structured recovery result. Add installed Linux/macOS/Windows failure
    drills for missing, corrupt, truncated, permission-denied, interrupted,
    and read-only control-file cases.
  - Local implementation is complete: versioned durable control files use
    atomic persist-then-publish, quarantine/LKG recovery, explicit fatal
    device-identity reset, and health-visible config apply/rollback. Missing,
    corrupt, truncated, interrupted, and permission fixtures pass. Installed
    Linux/macOS/Windows read-only and interruption receipts remain required.

- [x] **Schedule real idle database maintenance**
  - Call `EventStore::maintain()` during genuine idle periods for
    `PRAGMA optimize` and WAL truncation, with retry/health reporting when a
    reader blocks it. Do not wait until shutdown for a long-running session.
    `crates/photoproof-core/src/store/mod.rs:1439-1472`.
  - Add WAL-size monitoring and prove shutdown checkpointing begins only after
    all managed readers/writers have acknowledged stop.
  - Shipped: the managed maintenance lane runs only after the exact six-hour
    cadence and a fully idle ingest/scan/capture snapshot, retries without
    resetting the due deadline, and records SQLite's explicit blocked-reader
    verdict in application health until a later idle checkpoint succeeds.
    WAL health is distinct from combined DB inventory: current bytes,
    modification age, warning/critical thresholds, inventory failure, and the
    last maintenance attempt/success/failure/error remain observable.
  - Shipped: shutdown now obtains an unforgeable finalization gate only after
    both managed tasks and admitted IPC reads/writes acknowledge. Session,
    sidecar, collection, and WAL finalization all sit after that gate; a timeout
    takes the crash-recovery path and skips checkpointing. Deterministic tests
    `blocked_reader_is_wal_health_until_the_next_idle_retry_succeeds`,
    `shutdown_checkpoint_begins_only_after_tasks_and_commands_acknowledge`, and
    `shutdown_barrier_timeout_skips_checkpoint` cover recovery, ordering, and
    the timeout path.

- [~] **Add disk-space and backup/restore operational safety**
  - Monitor free space for DB/WAL, previews, vectors, downloads/parts, and RAW
    full-decode cache; warn before operations fail and pause safe derived work
    when critically low.
  - Define backup/restore for app-data truth that sidecars do not reconstruct
    completely: settings/config, device identity, collections export state,
    overflow/session journals, and migration recovery. Document what sidecars
    can and cannot rebuild.
  - Backend safety is now implemented: the managed volume lane samples
    app-data and separately configured model-volume capacity every 30 seconds,
    inventories DB/WAL, previews, full decodes, vectors, models, and `.part`
    files off the startup/ingest lanes every 15 minutes, and reports lower-bound
    inventory errors in `ApplicationHealth`. Below 2 GiB, reproducible preview,
    full-decode, vector, and maintenance writes pause while EXIF/journal work
    remains admitted; model downloads retain their separate required-size +
    2-GiB preflight. Settings health renders the warning and pause reason.
    Linux/macOS use `statvfs`; Windows uses `GetDiskFreeSpaceExW`.
  - `docs/BACKUP-RESTORE.md` distinguishes journal portability from a complete
    app-data snapshot. Settings now offers **Back up complete app state** and
    **Restore complete app state** with explicit quit/restart confirmation. A
    private same-executable helper receives its request only through an
    anonymous pipe retained for the lifetime of the desktop process; it cannot
    copy SQLite/WAL or replace app data until OS process teardown delivers EOF.
    Backups are checksummed and reject overwrite, tampering, symlinks,
    unmanifested payloads, and destinations inside the live source. Restore
    verifies before quit, stages and verifies again, atomically publishes, and
    retains the old app-data directory as a rollback copy; failure before
    publication reinstates it. The durable receipt is rendered after relaunch.
  - Saved topic phrases and authored topic notes now travel in the deterministic
    `topics.photoproof.json` journal export. Settings exposes an explicit
    all-or-nothing union import; same-id/different-content conflicts abort the
    transaction rather than choosing a winner. Core tests cover round-trip,
    idempotence, and conflict rollback, while backup/helper tests cover seven
    exit-boundary/tamper/replace/receipt cases. Final checkbox closure still
    requires the installed-package helper drill on the platform matrix.

### P1 - authoritative runtime/model management

- [x] **Introduce one authoritative `RuntimeCapabilities` report**
  - Record OS, detected adapters/vendor/backend/memory, compiled execution
    providers, runtime-library availability, actual provider initialization,
    and per-model compatibility.
  - Derive default offers and execution plans from capabilities, not tier
    alone. Preserve advanced alternate models outside the default consent set.
    This owns the already-recorded backend-aware model-offer and CUDA/provider
    observability items.
  - Hardware cache entries need age/version/driver/adapter fingerprints.
    Perform bounded background detection and atomically adopt changes; manual
    re-detect must not block an IPC/UI thread indefinitely.
  - Landed 2026-07-27: discovery now runs in a private helper process with a
    20-second deadline and cooperative shutdown/manual-redetect cancellation;
    timeout kills and reaps the helper, so a wedged vendor driver cannot outlive
    the managed-task barrier. The atomically adopted report includes OS/arch,
    physical/unified memory, adapter vendor/device/backend/VRAM/driver
    fingerprint, report schema/time, compiled and runtime-exported ORT EPs, and
    per-model compatibility. Cached reports remain visible only as provisional
    recovery context.
  - The runtime plan is explicitly Tier-0/dark until this launch adopts the
    authoritative report. Once ready, both offers and local Run plans reject
    incompatible model rows. A stale cached or user-forced tier therefore
    cannot launch a child or construct an ORT session by itself.
  - Verification: production helper smoke returned schema/provider/RAM JSON;
    desktop all-target compile is green; focused runtime suite 24/24; frontend
    Svelte check 0 warnings/errors and full 95-file/1,219-test suite are green.

- [x] **Report the actual model execution provider and fallback reason**
  - Do not infer acceleration from the provider ladder requested before ORT
    session construction. Surface requested, available, selected, and fallback
    reason per loaded model, including CPU fallback.
  - Scheduling decisions such as whether CLIP may run during CPU ASR must use
    actual execution, not `runs_on_accelerator()` derived from the requested
    ladder. `crates/photoproof-connectors/src/ort_embedder.rs:203-220,288-303,
    585-620`; `apps/desktop/src-tauri/src/pump.rs:489-556`.
  - Landed 2026-07-27: every live embedder session reports requested,
    runtime-available, registered, selected, fallback reason, measurement
    method, and optional profile path. CPU-only construction is proven CPU.
    ORT rc.12 does not expose post-partition per-node provider selection, so an
    accelerator registration is honestly `unknown`, never mislabeled GPU;
    capture scheduling pauses unknown work conservatively. Setting
    `PHOTOPROOF_ORT_PROFILE_DIR` enables ORT per-node JSON profiling and flushes
    it when the final embedder handle drops, providing the feasible measurement
    seam needed to turn unknown into evidence. Settings surfaces the same truth.
  - Strange but now explicit: “EP available,” “EP registered,” and “graph ran
    on EP” are three different facts. CUDA/TensorRT's rc.12 fail-silent ladder
    cannot reveal which registration succeeded, and CoreML may partition only
    part of a graph; neither is claimed as actual acceleration without profile
    evidence.
  - Verification: connector ORT suite 15/15 and strict all-target/all-feature
    connector clippy are green. Desktop strict clippy reaches an unrelated
    concurrent core dead-code warning after compiling this slice.

- [x] **Make embedder loading a complete restartable state machine**
  - Use explicit `Idle/Queued/Building/Ready/Failed/Stopping` states with
    attempt id, desired model, generation, start time, timeout/watchdog, retry
    budget, retry-now, cancellation, and reset semantics.
  - A thread-dispatch failure must become `Failed`, never leave `Building`
    forever; a hung load must not hold the shared build lock and block the other
    role indefinitely. Preserve the existing generation gate that prevents a
    stale build landing after a plan change/shutdown.
    `apps/desktop/src-tauri/src/embedders.rs`.
  - The shell state machine now makes a timed-out native result stale and
    terminally reports `Failed`, but ONNX Runtime session construction itself
    cannot be force-cancelled or joined in-process. A truly wedged constructor
    can retain the serialized build mutex, causing the other role and retries
    to queue and then time out until that native call returns. Isolate native
    construction in a killable helper process (or provide an equivalently
    revocable build lane) before calling hard recovery and bounded shutdown
    proven.
  - Delivered 2026-07-27: local ORT construction and inference now live in one
    same-executable helper process per role. The desktop retains
    `Idle/Queued/Building/Ready/Failed/Stopping`, attempt/generation truth, and
    proxies live text/image inference to the already-constructed child over a
    versioned, length-bounded binary stdio protocol. ORT is never constructed
    again in the journal process.
  - Plan changes, timeout, explicit retry/remove convergence, helper failure,
    and shutdown invalidate the generation, kill and reap the owned child, and
    reject stale proxy calls. Text and CLIP no longer share a native build
    mutex, so one wedged constructor cannot block the other role or its own
    replacement generation. A wedged inference read is kill-interruptible
    during quit.
  - Evidence: `embedders::tests` 18 passed/2 ignored; real subprocess fixtures
    prove hung-construction timeout/reap, other-role progress, replacement
    retry, crash-to-Failed projection, stale-generation rejection, oversized
    protocol refusal, and quit during wedged inference. The full desktop
    library gate passed 227/3 ignored, and strict desktop all-target clippy is
    green.

- [x] **Build one serialized model-operation registry**
  - Coordinate download, verify, install-index commit, load, unload, remove,
    and GC under one per-model operation/state machine. Removal must cancel or
    wait for download, stop consumers, unload, delete files, then commit the
    registry transition without lost updates.
  - Incorporate existing D4/D6/D7: partial reclamation, post-install
    existence/size verification plus hash-on-suspicion or explicit Verify, and
    locked/durable `installed.json` mutation.
  - Recover a malformed/missing installed index from files that verify against
    the current immutable manifest; identify true orphan model directories.
    `crates/photoproof-core/src/runtime/download.rs:196-242,329-338`;
    `apps/desktop/src-tauri/src/runtime.rs:951-963`.
  - Do not parse and trust `installed.json` while holding the shared runtime
    state lock on every status request. Publish a verified in-memory snapshot
    after serialized durable commits; status reads must stay cheap and surface
    registry/file disagreement rather than treating a row as installed.
  - Serialize `remove_model` against an active/queued download and every live
    consumer. The managed download worker alone does not prevent removal from
    racing its verify/install commit.
  - Delivered 2026-07-27: the desktop now opens one
    `ModelOperationRegistry`, validates `installed.json` once into a cheap
    in-memory snapshot, durably repairs rejected records, hash-recovers only
    manifest-valid files after a missing/corrupt index, and surfaces
    disagreements plus true orphan directories. One operation gate is
    intentionally global (stronger than per-model) because different model ids
    share the same index commit. Download/install, Verify, discard-partial, and
    removal all enter it; a second process without the runtime instance lock
    cannot mutate model files.
  - Removal cancels queued/live transfer work, makes the model unavailable to
    the plan, invalidates queued builds, kills and reaps constructing/ready
    embedder helpers, drains supervised children, then deletes files and
    durably commits the index removal. The internal production GC seam runs the
    same gated retirement pipeline after scheduler/reindex approval; no GC UI
    or automatic policy is implied.
  - A same-model download is rejected once installed, including a worker-side
    recheck under the gate, so weights cannot be rewritten beneath a live
    consumer. Adversarial evidence covers concurrent
    download/Verify/load/unload/remove/GC admission, helper-build cancellation,
    GC/remove versus queued transfers, and exact durable/in-memory
    `installed.json` equality with sibling records preserved
    (`model_registry::tests` 7 passed, `embedders::tests` 20 passed/2 ignored,
    `runtime::tests` 30 passed).

- [x] **Emit explicit terminal model-operation events**
  - Model download state should be
    `queued/downloading/verifying/installing/installed/failed/cancelled`, with
    one committed terminal snapshot after registry mutation and internal
    progress cleanup. A row must never remain "downloading 100%."
  - Add tier, installed-set, error-detail, desired/active model, and operation
    changes to event propagation rather than the readiness-only timeout
    fingerprint. `apps/desktop/src-tauri/src/pump.rs:683-700,703-769`.
  - Every runtime mutation (consent, acceptance, download, cancel, remove,
    restart, redetect/config apply) must emit the committed global snapshot to
    every window, not only return it to the caller.
  - Split durable consent from automatic enqueue settlement in the command
    contract. If consent commits successfully but download dispatch fails, the
    response must report a committed consent plus a retryable operation failure
    rather than implying the whole mutation rolled back.
  - Delivered 2026-07-27: every runtime mutation command now publishes its
    committed global `runtime-status`, and the idle pump fingerprint includes
    tier, consent, full installed/progress/error/registry/operation rows,
    capability detail, blocked reasons, and complete embedder slots. The pump
    has a terminal-download regression proof, so a cleared operation cannot
    leave a 100%-downloading row resident.
  - Consent now has an explicit two-settlement command DTO: a failed durable
    write still rejects, while a saved decision plus failed automatic enqueue
    returns `consentCommitted: true`, the committed status, and a retryable
    operation error. The consent card adopts that truth and offers an honest
    retry.
  - The row contract now exposes the exact
    `queued/downloading/verifying/installing/installed/failed/cancelled`
    sequence and commits one terminal snapshot after registry/progress
    settlement. Failed and cancelled variants plus two-window broadcast tests
    prove no 100%-downloading residue survives.

- [x] **Render model/runtime UI entirely from backend truth**
  - Return active model id, desired model id, role, state, actual provider,
    operation, progress, error/retry, and compatibility per model.
  - Remove hardcoded embedder ID arrays; do not label every recognized installed
    alternative from one role-level bool; use the existing rich
    `idle/building/ready/failed` DTO and show failure detail instead of
    "idle (loading)" forever. Include the omitted fp16 variant if it becomes a
    real supported artifact. `apps/desktop/src/lib/settings/SettingsApp.svelte:
    59-77,529-552`; `apps/desktop/src-tauri/src/dto.rs:273-333`.
  - Require confirmation and coordinated unload for destructive model removal.
  - Delivered 2026-07-27: Settings no longer carries embedder-id allowlists.
    It matches rows against the backend slot's `modelId`, renders the slot's
    `idle/queued/building/ready/failed/stopping` state, native failure detail,
    configured versus actually observed provider, and requires inline
    confirmation before the existing gated unload/remove pipeline.
  - Each backend model row now joins desired/active model ids, role,
    compatibility, operation/retry/error truth, provider selection/fallback,
    and complete runtime state. Rendered tests cover every state and confirmed
    unload/remove behavior.

- [~] **Bundle and smoke-test every required child runtime**
  - Configure Tauri `externalBin`/resources and platform packaging so
    `pp-asr-server` is present beside/in the installed application as the
    resolver expects; make an explicit product decision for shipping or
    excluding `llama-server`. Build hooks compiling a sibling in Cargo target
    directories are not evidence that a clean installed package contains it.
    `apps/desktop/src-tauri/tauri.conf.json:6-10,29-37`;
    `apps/desktop/src-tauri/src/supervisors.rs:67-93`.
  - Done means the installed artifact reaches Ready on a clean machine with no
    Cargo workspace, developer PATH, or manually staged binaries.
  - PARTIAL 2026-07-27: the supported bundle recipe builds and target-stages
    `pp-asr-server` through Tauri `externalBin`; Linux/macOS/Windows jobs
    extract the native package, find the child beside the installed app, and
    launch the installed app against fresh data without a workspace or
    developer PATH. `docs/DESKTOP-RELEASE.md` explicitly excludes
    `llama-server` while no shipped feature consumes it.
  - STILL OPEN: the model-free smoke proves child placement and a clean
    degraded `Usable` launch, but does not execute the ASR child with pinned
    weights or prove the supervisor reaches Ready. That native/model receipt is
    the acceptance criterion and cannot be replaced by archive inspection.

### P2 - observability, resource governance, release, and proof

- [~] **Finish desktop packaging and safe updates**
  - Add macOS and Windows bundles alongside Linux, code signing/notarization,
    updater signing, staged rollout/rollback policy, release notes/version
    migration compatibility, and installed-package smoke tests.
  - Ensure the normal dev/release recipes intentionally select supported
    acceleration features and runtime libraries. The native CPU bundle matrix
    is now cross-platform, but default builds still do not enable the NVIDIA
    path.
    `apps/desktop/src-tauri/tauri.conf.json:29-38`.
  - 2026-07-27 foundation landed: native Linux/macOS/Windows bundle matrix,
    packaged ASR sidecar staging, extracted installed-binary smoke, signed
    updater state machine and explicit Settings UX, release/schema/rollback
    contract gate, and a fail-closed draft production workflow for Developer
    ID notarization, Authenticode, and mandatory updater signatures. Also fixed
    Windows runtime resolution to look for the bundled `pp-asr-server.exe`;
    the older presence-only archive check could have passed while the runtime
    stayed dark.
  - External closure only: provision a cohort-aware HTTPS endpoint and matching
    updater keypair, Apple Developer/notary secrets, Windows signing identity
    and timestamp service, then run the signed workflow and founder/model-ready
    installed gates. NVIDIA package feature/runtime selection remains part of
    the capability-specific release matrix rather than this CPU-native base
    package. See `docs/DESKTOP-RELEASE.md`.

- [~] **Build a desktop lifecycle chaos/acceptance matrix**
  - Startup: unavailable/hard NAS, unplugged external drive, corrupt settings/
    config/installed registry, newer DB, interrupted migration, full disk,
    missing child binaries/runtime libraries, slow/hung hardware probe, and
    corrupt/same-size model files.
  - Live transitions: add/archive/remove/re-add roots, volume offline/online,
    watcher overflow, sleep/resume, model download/remove/reinstall, tier and
    config changes, GPU fallback, runtime crash budget/restart, cache deletion,
    and multi-window mutations.
  - Shutdown/crash: quit/kill during each scan, queue, doctor, model build,
    download, sidecar flush, collection flush, and WAL checkpoint phase.
  - Pin invariants: window becomes usable promptly; authored truth is never
    lost; derived state either remains valid or is re-pended; every task reaches
    a terminal/paused state; UI matches backend truth; clean shutdown leaves no
    unowned children or writers.
  - Define and test archived-root semantics across health, search, offline
    volume burden, watcher ownership, repair, stale inference, and pending
    ingest. Filtering an archived root from one health list is not proof that
    retained path rows and background work honor the same lifecycle.
  - FOUNDATION EVIDENCE 2026-07-27: the executable 28-case/15-suite runner and
    evidence map live in `apps/desktop/scripts/run-desktop-chaos-matrix.mjs`
    and `docs/DESKTOP-CHAOS-MATRIX.md`. The settled Linux run passed all local
    suites: 23 cases passed and 5 honestly remain installed-platform drills
    (kernel-blocked NAS, real suspend/resume, real GPU provider fallback, two
    native webviews, and a wedged native model build).
  - Archived roots now keep authored/search truth and durable pending rows but
    leave offline burden, watcher/scan ownership, stale inference, queue
    claims, and active-work status until unarchived. Acceptance coverage proves
    cache deletion/repair dormancy, active duplicate-path eligibility, and
    remove/re-add identity revival.
  - Timing hardening still open: one loaded full-suite pass missed the
    managed-task registry's one-second shutdown acknowledgement, then the
    focused test, full desktop suite, and canonical matrix passed. Add a
    deterministic scheduler-delay seam or deadline telemetry so this cannot
    remain a retry-only observation.

- [~] **Establish measurable desktop experience budgets**
  - Gate time-to-window, time-to-usable-library, startup I/O, idle CPU/wakeups,
    memory by library/worker tier, root reconciliation fairness, shutdown
    latency, model-load timeout/fallback, progress update smoothness, cache
    growth/eviction, and installed-package launch.
  - Use representative local SSD, removable drive, sleeping NAS, CPU-only,
    Apple, and NVIDIA fixtures. Pair with T2/T4 performance backlog items and
    the existing ingest intensity design.
  - CLOSED SLICE 2026-07-27: ordinary model-registry launch does only
    existence/size checks, and missing/corrupt/stale `installed.json` recovery
    no longer hashes multi-GB payloads before `Usable`. Candidate models stay
    dark while a managed, cancellable post-Usable task verifies and durably
    adopts them. Watchdog/cancellation tests cover the recovery lane; native
    model-set timing remains part of the real-device receipt matrix.
  - FOUNDATION EVIDENCE 2026-07-27: the ingest governor now has deterministic
    mode-budget tests for total concurrency, hashing/ingest workers,
    queue/embed/RAW batch sizes, and the bounded-pipeline decoded-frame memory
    proxy (Eco 2, Balanced 4, Max 16 maximum queued+working frames). Admission
    tests prove foreground priority plus eventual root-scan service and dynamic
    watcher ceilings; pump tests pin immediate initial progress followed by
    change-only 400-ms coalescing. These close the mode-dependent ingest slice,
    not the remaining real-device time-to-window/NAS/package benchmark matrix.

## Audit 2026-07-07 - deferred items (wave-1 fixes shipped; see LANDED.md + docs/AUDIT-2026-07-07.md)

The full-codebase audit shipped its wave-1 fixes (G1, S1-S4, U1/U2/U4-U7,
F1/F2/F3, D1/D2/D3/D5 - all in LANDED.md July 7 2026). These findings were
logged but deliberately deferred; each names its audit ID and why.

- [x] **S5: orphan preview/vector sweep** - the doctor applies a conservative
  30-day window after every path becomes stale, with active, recent,
  timestamp-unknown, and running-pass images protected. It removes preview
  files/metadata and retires `image_clip`, `annotation_chunk`, and
  `image_summary` vectors through PPVEC's dead-row sweep and crash-atomic
  compaction; authored annotations, stale path tombstones, and
  `derived_summaries`/`summaries_fts` text remain intact. Shared annotation
  vectors retire only when every target is eligible. The text-embedding pass
  now deterministically rebuilds an image-summary vector from the newest
  retained per-image summary text, including automatic missing/stale-vector
  reconciliation for already-completed libraries. Relinking revives all three
  derived passes, and interruption/idempotence/relink coverage proves the
  retained text restores both text-vector spaces without an LLM call.
- [~] **S6 / `s02_2`: case-only rename correlation on APFS** - implemented
  2026-07-27. Exact path spelling remains authoritative on case-sensitive
  volumes. A case-folded candidate is recased in place only after an injectable
  live-filesystem seam proves both spellings are one entry; watcher
  post-rename-only events, paired events, and reconciliation scans preserve
  path identity and never rehash that alias. Distinct case-sensitive entries
  remain separate. Sidecar cleanup verifies hash/content, distinguishes actual
  directory-entry spelling, and uses a temporary rename hop when an
  alias-to-alias APFS rename is a no-op. `s02_2` and cross-platform injected
  fixtures are green locally; three-OS CI no longer skips or tolerates the
  test. Remaining proof only: after this worktree is synchronized to the
  founder Mac, run `./scripts/verify-apfs-case-rename.sh <receipt-path>` on its
  confirmed default APFS Data volume and retain the emitted receipt.
- [x] **D4: reclaim failed partial downloads from the UI** - a terminally-
  failed model keeps `.part` gigabytes the orphan scan skips (it has a manifest
  entry) and the settings row offers only Download. Surface partial bytes + a
  "Discard partial" action. Delivered 2026-07-27: status retains the cheap
  in-memory partial-byte count after cancel/failure, Settings shows it and a
  Discard partial action, and the backend removes only manifest-owned `.part`
  files under the serialized model gate.
- [x] **D6: post-install model re-verification** - installed files are trusted
  by byte-length only; disk corruption surfaces as an opaque spawn failure. A
  doctor/verify action re-hashing a model's files (`sha256_file` exists).
  Delivered 2026-07-27: Settings Verify runs off the IPC thread, hashes every
  final artifact against the immutable manifest, and adopts an unindexed
  directory only after the full proof.
- [x] **D7: lock-guard installed.json mutation** - single-worker today, but
  `remove_model` runs on a command thread; also `remove_installed_record`
  lacks the `create_dir_all` its counterpart has. Delivered 2026-07-27:
  installed-index writers use the shared operation gate plus unique-temp,
  file-fsync, atomic-replace, parent-fsync durable writes; non-lock-holder
  processes fail closed.
- [x] **D8: unhosted fp16 CLIP manifest entry** - the staged
  `ViT-H-14-378-quickgelu__dfn5b-fp16` recipe has no offered tiers, immutable
  pin validation rejects `local-fp16-convert`, and fresh installs choose the
  hosted int8 artifact. Hosting/re-pinning is tracked separately before fp16
  can re-enter public offers.
- [ ] **U3: force-reembed affordance** - `force_reembed` is registered but has
  no UI caller (dev-console only). Wants a product decision on where the
  affordance lives (Settings -> Models "Force re-embed", or explicit
  dev-only). `commands/app.rs:322`.
- [ ] **U8: Duplicates lens interactivity** - cluster thumbnails are
  non-interactive `<figure>`s bypassing the Thumb retry path (moot: a grouped
  image necessarily has a preview). Tier-1 follow-up. `DuplicatesView.svelte`.
- [ ] **F4: client-side fling load ordering** - the grid delegates all loading
  to native `loading="eager"`; superseded fling requests keep completing. The
  graph's `ThumbQueue` solves this. Medium effort; measure first.
- [ ] **F5/F6: grid derived recompute + per-row EXISTS subqueries** - full-array
  deriveds recompute per mid-scan re-list; `list_folder` runs two correlated
  subqueries per row. Bounded, off the render path. Baseline via the T2
  grid-list bench before optimizing.
- [ ] **F7: full-res protocol routes read whole files** - `/original`,
  `/embedded`, `/full-decode` have no HTTP range support. Look-scale only.
- [~] **T2/T4 desktop journey performance evidence** - T2 now ships
  `pp_bench grid-list`, `preview-generate`, and `preview-serve` with committed
  `tune-check` ceilings. T4 has a deterministic 50k-item/60-frame fling-load
  request-growth gate. Representative installed device receipts remain under
  A26.

## Dogfood round 4 (founder, June 12 2026 evening — second live session)

- [x] **Search ranking is rank-flat: any note outranks a perfect CLIP
  match** — landed `0907fe7` (B75): similarity-aware RRF — dense
  (cosine) signals tilt their contribution by `w·(1/(k+rank))·(1+β·cos)`
  (β=0.5, so a perfect match earns up to +50% over its rank baseline
  and can BEAT, not just tie, a same-rank keyword hit); sparse bm25
  signals stay pure RRF; S4 raised 0.5→1.0 (visual = a note's full
  weight), S3 held at 0.5 (derived prose never outvotes own words).
  Spec deviation in DECISIONS B75 + RETRIEVAL §5.3; regression test
  pins the founder scenario. NOTE: weights/β are data the §12 eval
  still owns; the search-UI overhaul will make them user-visible.
  ORIGINAL: (founder, THE headline bug): "ANY saved note in the image
  journal is outranking even perfect semantic visual clip search."
  ROOT CAUSE FOUND (`search/hybrid.rs` FusionWeights): weighted RRF
  with k=60. S2 (note keyword FTS) and S1 (note own-words vectors) are
  weight 1.0; S4 (image_clip visual) is weight 0.5. Because RRF scores
  by RANK not similarity — score = weight / (60 + rank) — an image
  ranked #1 by a weak note keyword hit scores 1.0/61 = 0.0164 and a
  PERFECT CLIP visual match ranked #1 scores 0.5/61 = 0.0082, so the
  note ALWAYS wins regardless of how strong the visual match is or how
  weak the note hit. The 0.5 CLIP weight (B69: "protected by WEIGHT not
  exclusion") was a spec default explicitly flagged as "data not
  findings, the §12 golden-set eval is the named gate." This is that
  gate arriving via dogfood. Two moves, likely both: (a) re-weight —
  CLIP visual should not sit at half a note's vote when the query is
  visual; consider raising S4 or making weights query-shaped (a
  visually-descriptive query leans S4, a "what did I say about…" query
  leans S1/S2); (b) RRF's rank-flatness is itself the deeper culprit —
  a near-miss and a perfect match at the same rank score identically;
  consider a similarity-aware fusion or a score-floor so a high-cosine
  CLIP hit can't be buried under a tangential keyword brush. PAIRS WITH
  the search-as-scope UI overhaul the founder asked to start now (see
  "Lighting up M3" + the search-scope riff) — the relevance-sort and
  per-signal toggles from that design make the weighting VISIBLE and
  tunable by the user, not just an invisible constant. (Founder, June
  12 2026.)
- [x] **Backend logs to a file** — landed `6c1f44b`: fresh
  file per `tauri dev` launch (founder preferred over rotating) at
  `<app_data>/logs/photoproof.log`, installed in `lib.rs::install_logging`
  (console + truncate-on-start file sharing one env filter). Recorded
  in CLAUDE.md as the first-class debug surface. NOT done: folding the
  stray `eprintln!`s into tracing; surfacing the path in settings.
  ORIGINAL ASK:
  (founder asked; also: the
  assistant can't see runtime behavior without it): `lib.rs` installs
  a `tracing_subscriber::fmt()` to STDERR only (`info` default,
  `photoproof_core/desktop=debug`), plus scattered `eprintln!`s
  (mic.rs, pump.rs, state.rs, embedders.rs). Nothing persists, so a
  crash/jank is unreviewable after the fact. Add a file layer
  (`tracing-appender` non-blocking rolling appender) writing to the
  app-data dir (e.g. `<app>/logs/photoproof.log`, daily roll, keep N);
  keep the stderr layer for `tauri dev`. Fold the stray `eprintln!`s
  into `tracing` while there so one sink captures everything. Surface
  the log path in the debug panel / settings for "reveal in Finder."
  (Founder, June 12 2026.)
- [x] (LANDED `6d7c4fb`, merge `0722efe`; details in LANDED.md) **Full RAW decode (1:1 preview) — PLAN WRITTEN `docs/PLAN-RAW-DECODE.md`**
  (`ffd118a`): the founder asked to build it (not just hide the count).
  Key finding — NO new dependency: rawler 0.7.2 already exposes WB
  coeffs, cam→XYZ matrix, CFA, levels; we write the develop arithmetic
  (black/scale→WB→demosaic→matrix→sRGB→gamma) as a cancellable
  `full-raw-decode` pass draining like the embedding queue. FOUNDER
  DECISIONS RESOLVED (June 12, in the plan): (1) "1:1" = FULL SENSOR
  resolution, deep-zoom like LR/darktable 100% (not just 2560px); (2)
  quality = typical neutral decode, "just need real resolution"; (3)
  memory = Lightroom's model (develop once → cache full-res artifact to
  disk → serve zoom from cache; one develop in flight, tiled-demosaic
  fallback on low RAM). (4) ON-DEMAND not eager — do NOT develop every
  RAW on ingest; develop lazily when the user opens/zooms an image in
  Look (the "ask"), cache to disk, serve from cache after. Removes the
  eager enqueue that created the 154 stuck rows. READY TO BUILD.
  ORIGINAL DIAGNOSIS:
  "154 RAWs left to decode" reads as stuck — it's an UNBUILT pass,
  not a stall: (founder: "154 raws left to decode that seem stuck").
  DIAGNOSED: `ingest_passes` has 154 `full-raw-decode` rows in state
  `pending`, `attempts=0`, no error — they were enqueued and NEVER
  claimed, because `ingest::claim_next` drains only `Exif` + `Preview`;
  `full-raw-decode` is M1.5 and has NO worker yet ("stay pending in the
  queue by design"). So nothing is broken — but the UI advertises a
  count of work that will never move until M1.5 ships, which reads as a
  hang. Fix is honesty, not a decoder (unless M1.5 graduates now): stop
  surfacing pending counts for passes that have no worker, or label
  them "available in a future version," not "left to decode." (Same
  root cause as the DNG item below.) (Founder, June 12 2026.)
- [x] (LANDED `6d7c4fb`; same root cause, resolved by the RAW decode pass above) **DNG (and other RAW) never loads a 1:1 preview** (founder:
  "Embedded preview — full decode pending… a dng file never loads
  1-to-1 preview"). SAME ROOT CAUSE as the stuck-RAW item: the 1:1
  view needs a full demosaic, which IS the `full-raw-decode` M1.5 pass
  — unbuilt, never claimed, so "full decode pending" is permanent. The
  embedded preview (the in-RAW JPEG) loads; the true 1:1 cannot until
  the decode pass exists (`preview.rs` already enqueues it at backfill
  priority and notes the CR3 HDR-PQ / chained-JPEG ladder it would
  feed). DECISION NEEDED: graduate the M1.5 full-RAW-decode pass now
  (rawler demosaic → 1:1 artifact), or make the UI stop promising a 1:1
  that won't arrive. For DNG specifically, verify rawler's DNG path and
  whether a larger embedded preview exists to show meanwhile. (Founder,
  June 12 2026.)
- [x] **Add-to-collection from the grid offers "New collection…"** — landed `589a0fd`: new `new-collection-add` thumb seat (available even at zero collections), captures targets synchronously, reuses the rail's inline name input (one create UX), runs create-then-add in order; blank name leaves nothing empty.
  ORIGINAL ASK:
  (founder: "if I right click on image(s) in grid, I want to add to a
  collection even if none exists / add to new collection"). Today the
  thumb context menu's add-to-collection only lists EXISTING collections
  (`collectionRows` over the current set); with zero collections there's
  no path, and you can't mint one from the selection. Add a "New
  collection…" item to the add-to-collection submenu that creates the
  collection AND adds the current selection in one evented step (the
  rail already has an inline "New collection…" creator —
  `SourceRail.svelte` — reuse its create path, then chain
  add-to-collection). This is also the natural feeder for the
  autosuggest/encourage-collecting thesis. (Founder, June 12 2026.)
- [ ] **Review "done work": exports-folder path + foreign edit sidecars**
  (founder, June 12 2026: "the main point of the app should be to review
  DONE work… we may want to support reading in sidecar edit files from
  Lightroom/darktable"). In TENSION with a neutral RAW develop: an edited
  RAW shown via our neutral develop looks WRONG vs the editor. Honest
  scoping (see PLAN-RAW-DECODE.md "foreign edit sidecars"): (a) FIRST-CLASS
  the export-folder review path — done work is usually exported JPEG/TIFF
  with the edit baked in, which the app already handles; cheapest, highest
  fidelity. (b) Faithful XMP/`.xmp` render = reimplementing Adobe/darktable
  = NOT feasible. (c) Pragmatic middle: read the PORTABLE subset from the
  sidecar — crop, orientation/flip, rating/label/color (and maybe basic
  exposure/WB) — approximated on the neutral develop, labeled "approximate";
  crop+orientation+rating is the high-value low-risk slice (matches the
  photographer's keep/reject intent even if tone differs). (d) Prefer an
  editor-written embedded full-res preview when present. SEPARATE from the
  develop pass — must not block it. Needs a design round. (Founder, June
  12 2026.)
- [x] (LANDED `91bfa15`, merge `e8faf55`) **Grid right-click submenus are janky** (founder: "submenus don't
  stick out the side, don't always open/close smoothly"). The whole
  context menu is `ContextMenuHost.svelte` (a 1 KB stub) — submenus
  (add-to-collection, surround, etc.) don't flyout to the side and
  open/close unreliably. Needs a real submenu implementation: side
  flyout with edge-aware flipping (open left when the right edge is
  near the viewport), hover-intent open/close with a small close delay
  so diagonal travel into the submenu doesn't dismiss it, keyboard
  arrows. Likely wants a small reusable Menu primitive rather than
  more ad-hoc positioning. (Founder, June 12 2026.)
- [x] (LANDED `d541854`, merge `10796c8`) **T cell-info should grow the cell, not overlay the image; info at
  the TOP** (founder). Today the cell-info row (`cellinfo.ts` cycled by
  T) is `position: absolute` over the bottom of the thumbnail
  (`Thumb.svelte` ~234), covering the image. Founder wants: when info
  is shown, the cell EXTENDS DOWNWARD to make room (image stays fully
  visible, info sits in its own strip below — or per the founder, info
  at the TOP of the cell). Touches the grid layout math (cell height
  becomes image + info-strip when active) and the gridlayout row-height
  calc, not just Thumb CSS. (Founder, June 12 2026.)
- [x] **No em-dashes in UI copy** — landed `ddb0e86`: 41 visible
  strings across 22 files de-dashed (spaced hyphen or clean range);
  residual dashes all non-visible (comments + the menus.ts separator
  sentinel rendering as <hr/>). Recorded as a rule in CLAUDE.md. NOT
  done: a CI grep-gate to stop creep. ORIGINAL ASK: (founder, emphatic:
  "no emdashes in the
  UI!!!"). Sweep user-VISIBLE strings (EmptyState lines, button labels,
  settings copy, station/indicator text, welcome/consent cards,
  tooltips) and replace `—` with " - " or a rephrase. ~408 `—` occur in
  the frontend but MOST are code comments — target only rendered text;
  do not touch comments or this backlog. Consider a tiny lint (grep gate
  in CI over .svelte template regions / string literals) so they don't
  creep back. (Founder, June 12 2026.)

## Dogfood round 5 (founder, June 21 2026 - fresh-install model setup)

Came out of a clean-slate run (fresh `cargo clean` + deleted downloaded
models) on the margo desktop (Arch / Hyprland / Ryzen 9900X + RTX 5080),
exercising the very first-launch model-download + first-ingest path. Findings
span the consent/download moment, the header progress indicator, ingest
intensity / RAM, and a startup-reliability bug.

- [ ] **Model-license consent card should be a centered modal, bigger**
  (founder, June 21 2026: "the screen asking you to accept licenses is not
  large enough... should probably be a centered modal and bigger"). Today the
  gate is `ConsentCard.svelte` - a quiet fixed-`380px` `<aside>` pinned
  bottom-right (`position: fixed; right:16px; bottom:48px; z-index:50`, no
  backdrop), deliberately un-modal so journaling stays visible behind it
  (`logic/consent.ts:23-45`: shows once after the first root is added, only
  when `consent==='undecided'`, Tier >= 1). On a FRESH install the user must
  actually read several model licenses + tick a per-model acceptance checkbox
  + weigh the total download size, and 380px in the corner is too cramped for
  that. Founder wants it promoted to a CENTERED MODAL with a backdrop and more
  room for the license list. Note the design tension: the corner-card was an
  intentional "quiet, non-blocking" choice - but first-run licensing IS a hard
  gate (§13.7), so a modal is right THERE; keep it unobtrusive if it ever
  re-appears mid-session (e.g. a later tier offering new models). Pairs with
  the first-run onboarding flow item below. Anchors:
  `apps/desktop/src/lib/components/shell/ConsentCard.svelte:24-116`,
  `apps/desktop/src/lib/logic/consent.ts:23-45`. (Founder, June 21 2026.)

- [x] **Model download offers ALL variants (int8 + fp16 duplicates), not the
  hardware-best pick** (founder, June 21 2026, after a fresh `cargo clean` +
  delete: "it asked me to download ALL models?? like duplicates that shouldn't
  be if we are analyzing the machine's capabilities and selecting the best
  options"). CONFIRMED - the offered set is TIER-ONLY, never backend-aware.
  `Manifest::offered_at(tier)` (`runtime/manifest.rs:110-115`) returns every
  entry whose `tiers` vec contains the effective tier; `runtime.rs:378-425`
  renders all of them into the consent card. Tier detection itself
  (`runtime/tier.rs:90-114`) gates on VRAM / Apple unified memory ONLY - it
  never consults the detected accelerator backend, even though
  `HardwareReport.adapters[].backend` (Metal/Vulkan/DXGI) is right there and
  unused for filtering. So BOTH DFN5B CLIP towers ship to every machine - int8
  `ViT-H-14-378-quickgelu__dfn5b` (`manifest.rs:512`) AND fp16 `...__dfn5b-fp16`
  (`manifest.rs:537`), both `tiers:[1,2]` - plus several alt Gemma LLMs (E2B
  default + E4B + the MTP variants) all at once. The app ALREADY KNOWS the
  right per-platform pick (the CoreML spike decided fp16-on-Metal for Mac,
  int8-on-CPU for plain CPU, fp16-on-CUDA/TensorRT for the 5080 - see the two
  "CoreML EP" / "CoreML CLIP graph fragmentation" items + `docs/RUNTIME-MATRIX.md`);
  it just never applies that knowledge to the DOWNLOAD set. FIX: a backend-aware
  narrowing step after tier detection so each seam (CLIP visual, LLM, ASR,
  text-embed) offers ONE variant matched to the machine (Metal->fp16 CLIP,
  CPU->int8, CUDA->fp16; the default LLM, not every alt), with the other
  variants reachable only via an explicit "advanced / other models" affordance
  rather than the default consent list. This is the concrete first cut of the
  hardware-aware model selection the first-run onboarding item wants - it makes
  the "optimizing for your hardware" promise true at the download gate, not
  just in copy. (Founder, June 21 2026.)
  - Delivered 2026-07-27: the authoritative capability report narrows default
    consent offers to one compatible model per role, keeps alternates
    explicitly available, and fails closed while capability truth is
    provisional. CPU/Metal/NVIDIA/unsupported fixtures pin the selection.

- [x] **Header still shows "downloading" when the last model is at 100%**
  (founder, June 21 2026: "the last model to download is at 100% but it still
  shows as downloading in the header"). The header pill / settlement lives in
  `LibraryStatus.svelte` + `logic/librarystatus.ts`. The `downloading` flag is
  raised when any model row reports `state==='downloading'`
  (`librarystatus.ts:318-327`) and the indicator only settles when
  `waitingOn.length === 0` AND nothing is mid-download (`:355-360`). A model
  that has reached 100% bytes but whose state has not yet flipped
  `downloading -> installed` (the post-download verify/move step, or a
  not-yet-arrived status event) keeps the pill in "downloading". Fix: treat
  100%-bytes as visually complete per row, and/or settle on the terminal state
  transition rather than a lingering `downloading` row; make sure the final
  model's `installed` event actually clears the aggregate. Anchors:
  `apps/desktop/src/lib/logic/librarystatus.ts:318-327,355-360`,
  `apps/desktop/src/lib/components/shell/LibraryStatus.svelte:149-183`.
  (Founder, June 21 2026.)
  - Delivered 2026-07-27: the header consumes the committed terminal operation
    snapshot, and exact installed/failed/cancelled transition tests prove that
    progress cleanup cannot leave a 100%-downloading aggregate.

- [ ] **Status hover card makes large jumps between number updates** (founder,
  June 21 2026: "the hover card when i hover over it seems to do large jumps
  between number updates"). The expanded panel
  (`LibraryStatus.svelte:89-146`) shows per-stage counts + rate + a summed
  overall ETA (`librarystatus.ts:369-377`). Two jitter sources: (a) the overall
  ETA is the SUM of every working+pending stage's `remaining / ratePerSec`, and
  while the rate is EMA-smoothed (`pump.rs RATE_ALPHA=0.3`) the per-model
  download % is a RAW `downloadedBytes/totalBytes` ratio - a single fast model
  can swing the sum; (b) the pump emits coalesced progress only every 400ms
  (`PROGRESS_INTERVAL`) or on a rate-quantum change, so the card updates in
  bursty steps rather than smoothly. Fix: smooth the displayed numbers
  (monotonic counts that never go backwards, eased % / ETA), and/or update the
  hover card on a steadier cadence so it reads continuous. Anchors above +
  `apps/desktop/src/lib/components/shell/LibraryStatus.svelte:271` (the 300ms
  width transition that can lag the number). (Founder, June 21 2026.)

- [x] **User control over ingest intensity + "what processes when" (left
  sidebar) - the app runs too hard / eats RAM** (founder, June 21 2026: "i'm
  not sure the best strategy is to just cue up every image instantly to go
  through all the operations... probably in the left sidebar we need more
  control / options for first time processing, re-processing, what happens when
  you add a new folder of source images... the app is currently using a ton of
  ram, so we probably just need to make sure we are being thoughtful about user
  control over how hard our app runs"). CURRENT BEHAVIOR (confirmed): `add_root`
  spawns a `pp-initial-scan` thread (`commands/library.rs:83-150`) that walks
  the folder and enqueues EVERY discovered image through `new_image_in_tx`
  (`library/mod.rs`) - exif + preview passes at scan priority immediately,
  embeddings as P3 backfill, full-RAW-decode on-demand. The pump drains with
  HARD-CODED batch bounds (`pump.rs:22-40`: `QUEUE_BATCH=64`, `EMBED_BATCH=8`,
  `DECODE_BATCH=2`) and the only existing throttle is the capture-pause
  (`pump.rs:356-358`) plus the env-only `PHOTOPROOF_INGEST_WORKERS` decode-pool
  cap. There is NO user-facing processing control anywhere: `SourceRail.svelte`
  (Folders/Collections/Topics) and the 4-section settings window have none;
  only `rescan_root` / `rebuild_previews` context verbs exist
  (`commands/library.rs:283-319`). WANT: surface processing as a user-governed
  thing - (a) a left-sidebar / settings affordance for an overall intensity
  budget ("how hard the app runs": worker/RAM/CPU ceiling, maybe Eco/Balanced/Max);
  (b) explicit control over FIRST-TIME processing vs RE-processing; (c) a clear
  policy + prompt for what happens when a new source folder is added (process
  now / process later / preview-only); (d) pause/resume + per-pass toggles
  (e.g. defer embeddings). Ties directly to the RAM concern and to the
  first-run onboarding flow item. The batch constants and pool caps already
  exist as the mechanism - this is about USER GOVERNANCE over them. Needs a
  design round (where it lives, the default that stays quiet and fast, how RAM
  ceiling maps to worker/batch sizing). (Founder, June 21 2026.)
  - FOUNDATION LANDED 2026-07-27: `ResourceGovernor` is now the single
    process-wide admission/priority authority. Persisted Eco/Balanced/Max map
    to real total-lane, ingest worker, queue-batch, embed-batch, RAW-batch, and
    scan-hash concurrency ceilings. The Settings window exposes the mode,
    Pause/Resume, and a new-folder Process now/Process later default; health
    reports active/waiting counts for every lane, and `IngestStatus` carries
    the same Pause/intensity truth so the header says "Paused by you" instead
    of pretending frozen counters are active work.
  - The authority now gates metadata/live ingest ahead of preview work,
    interactive RAW ahead of both, embedding after them, initial/resume/
    maintenance scans, startup doctor/root I/O, vector repair, six-hour
    maintenance, and managed model-download workers. Queue and scan tests prove
    the configured concurrency ceiling; governor tests prove Pause, priority,
    and dynamic mode wake-up. Critical-disk admission remains the stronger
    gate and composes with manual Pause.
  - CLOSED 2026-07-27: `NewRootPolicy` now includes a durable `preview-only`
    contract. Every source freezes its effective policy at add time in
    `rootProcessingPolicies`; a one-shot Default / Process now / Previews only /
    Process later chooser is available in both the left rail and Settings, and
    the override does not mutate the saved future default. A failed policy
    fsync rolls registration back instead of leaving an ungoverned active root.
    Explicit Rescan promotes that root to full processing.
  - Text and image embedding are independent persisted switches. Deferral does
    not rewrite/drop queue rows: disabled passes remain pending and resume
    normally. Preview-only/later roots are excluded at claim time; an image
    shared with any full-processing active root remains eligible. Core tests
    prove both the exclusive-root deferral and shared-image escape.
  - Watcher event bursts, per-path hashing, overflow recovery, and polled scans
    now acquire the desktop's dynamic RootScan lane through
    `start_watcher_with_options`; every turn snapshots the live intensity's
    hash ceiling. Startup-restored, add, and unarchive watchers all use it.
  - Pause is now a waitable process signal, not only a next-batch check.
    Filesystem walks observe it before every entry and phase; controlled hashes
    check it every 64 KiB. A paused walk retains its complete in-memory seen
    set and resumes the same idempotent operation, so stale inference can never
    run from a partial traversal. Cancellation breaks the wait without
    publishing a partial digest.
  - In-flight model transport observes the same signal after each 64-KiB
    resumable `.part` write through `GovernorDownloadPacer`; Pause suspends and
    Resume continues the same body, while shutdown/user cancel still wins.
    Deterministic tests prove an admitted scan and download chunk both stop and
    resume.
  - Budget evidence is exact and executable: Eco/Balanced/Max tests pin total
    concurrency, ingest/hash concurrency, queue/embed/RAW batch bounds, and the
    bounded-pipeline decoded-frame memory proxy (2/4/16 frames). Separate tests
    pin foreground-before-root fairness, eventual RootScan admission, dynamic
    watcher ceilings, and the immediate-then-400-ms progress cadence.

- [~] **Startup hang when a library root's volume is on an unresponsive
  network mount (silent windowless freeze)** (margo, June 21 2026 - diagnosed
  live). REPRO: a hung `hard` NFS mount (`bornmanserver:/HomeNAS/raw_photos` at
  `/mnt/raw_photos`) that a library root lives on. SYMPTOM: app launches, the
  process and the WebKit children spawn, but NO window ever maps and there is
  zero error - it looks completely broken. ROOT CAUSE: `App::init` ->
  `state.library.probe_volumes()` runs SYNCHRONOUSLY on the main thread in
  `setup()` (`apps/desktop/src-tauri/src/lib.rs:162,166`) and stats the mount
  to re-identify the volume by its `.photoproof-volume` marker; on a `hard`
  NFS mount the stat blocks FOREVER (kernel `nfs4_handle_exception` /
  `rpc_wait_bit_killable`), so `setup()` never returns, Tauri's event loop
  never starts, and the window never shows. Confirmed it is NOT graphics:
  zenity (GTK) and MiniBrowser (WebKitGTK 2.52.4) both opened windows fine; the
  fix that unblocked it was `sudo umount -f -l /mnt/raw_photos` then relaunch
  (plain, no env flags). PRODUCTION IMPACT: build-agnostic (same code path in
  release) and arguably MORE likely for real users - a NAS asleep/off-network,
  a laptop that left the LAN, an unplugged/spun-down external drive that a root
  sits on all reproduce it. The volumes schema already models `online/offline`
  state precisely so it CAN degrade gracefully; the startup just doesn't. The
  team already knew about main-thread startup blocking - the startup-doctor's
  preview walk was deliberately moved to a background thread for exactly this
  reason (`lib.rs:183-194` comment) - but `probe_volumes()` and the
  watcher-start loop (`lib.rs:166-181`) were left on the main thread. FIX: run
  `probe_volumes()` (and the watcher start) off the main thread, or give each
  per-volume probe a short timeout / non-blocking stat, so an unreachable
  volume just marks itself offline and the window still comes up. (Founder /
  margo dogfood, June 21 2026.)
  - Deterministic implementation delivered 2026-07-27: the usable barrier no
    longer waits for volume probing or watcher restoration, and a blocked
    injected probe remains owned and observable after the shell is usable. A
    real installed launch against a kernel-blocked NAS remains the acceptance
    receipt.

- [~] **Default `tauri dev` launch on NVIDIA silently runs CPU embedding (no
  CUDA) - and nothing tells the user** (margo, June 21 2026 - "did we detect
  CUDA? / embedding is super slow"). CONFIRMED on the 5080: the live app log
  shows ONLY `CPUExecutionProvider` (1262 CPU BFCArena allocs, zero
  CUDA/TensorRT), `nvidia-smi` shows the GPU idle with NO photoproof process on
  it, and the dev build runs `cargo run --no-default-features` (tauri.conf
  `devCommand`) so the `cuda`/`tensorrt`/`cuda-dynamic` features are not even
  compiled in - `ort` falls back to the only EP present (CPU). Result: CLIP
  embeds at the CPU rate (~41 img/min) instead of the validated 54x CUDA /
  85x TensorRT (2259 / 3635 img/min), i.e. "super slow", with NO indication
  anywhere that the GPU is unused. TWO GAPS, file under both: (1) WIRING - the
  open CUDA EP item already owns getting the `cuda-dynamic` build + `ORT_DYLIB_PATH`
  (cuda13 / Blackwell sm_120, `docs/PLAN-ORT-BLACKWELL.md`) into the actual
  desktop launch (today it only works via the `cuda_spike` harness); the plain
  `bun run tauri dev` / `tauri build` has no NVIDIA-accelerated path. (2)
  OBSERVABILITY - even once wired, the app should SURFACE the active execution
  provider per model ("CLIP: CUDA / TensorRT / CoreML / CPU") so a silent
  CPU-fallback (wrong build, missing dylib, EP init failure) is visible, not a
  mystery slowdown. This is the legible-hardware promise from the first-run
  onboarding item, extended to runtime: the user (and the assistant) should be
  able to SEE that the 5080 is or isn't doing the work. Pairs with the
  hardware-aware model-selection item above and the CUDA EP item below.
  (Founder / margo dogfood, June 21 2026.)
  - 2026-07-27 contract: ordinary `dev` and CPU-native bundles stay portable
    and do not claim CUDA. NVIDIA machines use the explicit `dev:nvidia` /
    `bundle:nvidia` profile, whose feature, staged-library, resolver, package,
    and provider-observability wiring is verified. A real CUDA/TensorRT
    installed run is still required before this item can close.

## Founder thread, June 14 2026 - model-usage walkthrough (decisions)

Came out of a walkthrough of every ML model and where they overlap (see
`docs/RUNTIME-MATRIX.md` "Model concurrency", `docs/ROADMAP.md`). Four items, two
of them posture changes the founder made deliberately.

- [ ] **Derived summaries become VISIBLE journal entries, marked "system" and
  DELETABLE** (founder, June 14 2026: "I do want to have derived summaries be shown
  to the user and marked as 'system', which allows users to delete them if the
  system made an error instead of keeping that hidden wrong forever. All this can be
  in the journal tab."). A DELIBERATE BEND of K14 (machine prose was "retrieval fuel
  only, never user-visible"): instead of hiding machine summaries, SURFACE them in
  the journal tab with a `system` source chip and let the user DELETE a wrong one (a
  retraction on a system-authored entry) - transparency beats hidden-wrong-forever.
  Spec impact: RETRIEVAL §9 (summaries no longer invisible fuel-only), EVENTS (a
  `system` source + a user-deletable system entry; reuse retraction). Pairs with the
  summary-GENERATION work (still unbuilt, M3) and the type-chip item below. The
  derived-summary text must STILL be embedded by EmbeddingGemma into the
  `image_summary` vector (the S3 search lane already consumes it) so a visible system
  summary stays searchable - confirmed that is the existing design, just not yet
  generating. (Founder, June 14 2026.)


- [ ] **Generate tags from our data and EXPORT them INTO the image files (writes
  real file metadata)** (founder, June 14 2026: "a feature which is to generate tags
  based off our data and export them directly to the images, with clear warnings
  that this actually does change the meta-data of the actual files"). Generate
  keyword tags from what the app already knows (collections, topic-graph clusters,
  derived summaries, CLIP/embedding neighborhoods) and write them as IPTC/XMP
  keywords INTO the originals (or an adjacent `.xmp`), for use in Lightroom / Bridge
  / Finder. POSTURE CHANGE: this is the FIRST feature that writes to the user's
  originals - it knowingly breaks the strict non-destructive posture, so it MUST be
  opt-in, explicitly warned ("this modifies your actual files"), and ideally
  backup/undo-aware. RELATION: goes beyond SIDECARS §14 (a one-way XMP *sidecar*
  export that never touches files); and it REOPENS the "Won't build: metadata
  editing" line at the bottom of this file - founder-directed, but scoped to warned,
  opt-in tag EXPORT, not general in-app metadata management. Needs a design round
  (what writes, where, how warned, reversibility). (Founder, June 14 2026.)


- [~] **CUDA execution provider for the `ort` embedders (Ryzen 9900X + RTX 5080
  desktop)** - VALIDATED June 14 2026: the FP16 CLIP runs on the 5080 at **54.47x**
  over CPU (2259 vs 41 img/min, near-lossless cosine 0.9998). The per-model NVIDIA
  gating (`OrtEmbedder::clip` -> `Accel::Nvidia` -> the TensorRT/CUDA ladder, behind
  the `cuda`/`tensorrt`/`cuda-dynamic` features) + the `cuda_spike` harness are
  committed. The Blackwell (sm_120) blocker (no prebuilt onnxruntime kernels) was
  solved with the official **cuda13 onnxruntime tarball** (real sm_120 SASS) loaded
  via `ort/load-dynamic` + `ORT_DYLIB_PATH` (recipe + result in
  `docs/PLAN-ORT-BLACKWELL.md`). The TensorRT EP rung ALSO validated: **85.79x**
  (3635 img/min, +1.58x over CUDA, cosine 0.99994) with TensorRT 10.16.1
  (`pip 'tensorrt-cu12<11'`, ships the sm120 builder resource). REMAINING: (a) wire
  `ORT_DYLIB_PATH` + `LD_LIBRARY_PATH` + the `cuda-dynamic` build into the desktop app
  launch on NVIDIA (the analog of the macOS CoreML flip); (b) re-measure EmbeddingGemma
  on CUDA/TensorRT (whole-graph, unlike CoreML); (c) the decode-pool + batching levers
  (the bottleneck moved to decode). The 5080 can also run a higher tier (bigger LLM +
  Gemma 4 MTP, see PLAN-GEMMA-MTP). (Founder, June 14 2026.)

- [ ] **Cross-platform GPU path for the `ort` embedders (non-Apple, non-NVIDIA)**
  (founder, June 14 2026: "and Vulkan too") - CORRECTED framing, see `docs/PLAN-VULKAN.md`:
  ONNX Runtime has NO Vulkan EP and never shipped one, so "raw Vulkan" is the wrong
  target. The real answers: (a) **DirectML EP** (Windows, any DX12 GPU - AMD/Intel/NVIDIA)
  - cheapest near-term win, exists in `ort` behind the `directml` feature, runs FP16 (not
  int8 - matches our split exactly), consumes the SAME single-file FP16 model, mirrors the
  CoreML/CUDA gating; Windows-only. (b) **WebGPU EP** (Dawn -> Vulkan on Linux / DX12 on
  Win / Metal on Mac) - the strategic ONE-EP-for-all-non-Apple/NVIDIA bet, exists in `ort`
  (`webgpu` feature), op coverage looks right, but younger/needs a real-hardware spike
  before shipping. (c) ncnn / Burn-wgpu / vendor EPs (OpenVINO-Intel, MIGraphX-AMD) only
  as a last resort for Linux-AMD - each costs a conversion away from ONNX. Sequencing:
  DirectML when the Windows bucket opens -> WebGPU-EP spike in parallel. Lower priority
  than the M1 (done) + 5080/CUDA (in progress) targets. (Founder, June 14 2026.)

- [~] **The GPU embed moved the bottleneck to DECODE - two levers** (founder, June 14
  2026, after the 5080 hit 54x): now that CLIP image embedding is GPU-fast (54x on the
  5080, 8.77x on the M1 CoreML), decode/resize is the new ingest ceiling. Decode IS
  parallel today (rayon pool `min(cores,8)`, `library/mod.rs`; BLAKE3 `min(cores,8)`)
  but two wins are now worth it: (a) **re-bench the `min(cores,8)` decode-pool cap** - it
  was tuned on the M1 (`preview.rs` "more workers thrash the cache"); on the 9900X
  (12c/24t) feeding a 54x GPU it likely STARVES the GPU, so make it per-machine/tunable
  and re-bench on the desktop; (b) **BATCH the GPU embed** - `OrtEmbedder::embed_image`
  does ONE image per forward pass; GPUs love batches (the MLX text spike saw ~24x batched
  vs single-item), so batching the CLIP visual tower (16-32/forward) could beat even the
  TensorRT +1.5x. These are the next perf frontier on capable GPUs. (Founder, June 14 2026.)
  - LEVER (a) LANDED June 14: the ingest pool cap is now env-overridable via
    `PHOTOPROOF_INGEST_WORKERS` (`ingest_pool_size()` in `library/mod.rs`; default UNSET =
    the prior `min(cores,8)`, byte-for-byte). Re-tune on the desktop with the `#[ignore]`
    `bench_ingest_pool_width` harness (`PP_BENCH_DECODE_DIR=/jpegs cargo test -p
    photoproof-core -- --ignored --nocapture bench_ingest_pool_width`), which sweeps
    candidate widths over a real JPEG folder and prints img/s. Graduate the winning value
    to a config FIELD (pairs with the CoreML env->field graduation).
  - LEVER (b) BLOCKED on a model re-export, NOT a code change: the shipped DFN5B visual
    ONNX (BOTH the int8 external-data tower AND the FP16 single-file tower on CoreML/CUDA)
    has its batch dim FIXED at 1 - the `image` input is `[1,3,378,378]` (concrete `1`, no
    dim_param) and the PyTorch trace baked that `1` into ~350 internal Reshape constants,
    so ORT rejects any other batch size (verified June 14 against the on-disk graph
    metadata + asserted by the `#[ignore]` real-model test
    `visual_tower_is_batch_one_so_batching_needs_a_reexport`). FOLLOW-UP: re-export the
    visual tower with `dynamic_axes={"image":{0:"batch"}}`, re-run the COCO eval to confirm
    retrieval-safety, THEN add `OrtEmbedder::embed_images(&[DecodedImage]) -> Vec<Embedding>`
    (one `[N,...]` forward pass) + batch the image-embedding pass (16-32/wave). The note on
    `run_clip_image` (`ort_embedder.rs`) records this; the test flips loud when a dynamic
    re-export lands. Single query path stays one-at-a-time regardless.

- [ ] **First-run onboarding flow: "optimizing for your hardware" + guided setup**
  (founder, June 14 2026: "on our welcome screen we should explain / walk through initial
  setup... we probably want a whole flow"). Today there is a welcome card + a consent gate
  for model download, but not a guided FIRST-RUN FLOW. Build one that: (1) welcomes +
  explains the local-first thesis (your photos never leave; models run on-device); (2)
  **DETECTS the hardware and SHOWS it** - "optimizing for your Apple M1 Pro: CoreML / your
  RTX 5080: CUDA / no GPU detected: CPU" - turning the intelligent-detection matrix
  (`docs/RUNTIME-MATRIX.md`) into a visible, reassuring moment; (3) walks the model-download
  consent (sizes, the license gates, what each model enables, what works WITHOUT models =
  the Tier-0 floor); (4) the first-folder / watched-root add; (5) progress while the library
  digests (ties into the "Digest visibility" item). Should gracefully convey "you'll get
  the best your hardware allows" without jargon. A real design round - it is the user's
  first impression + the place the hardware intelligence becomes legible. Pairs with the
  Station / progressive-import / digest-visibility items. (Founder, June 14 2026.)

## Visualizer + state-integrity follow-ups (founder, June 15 2026)

Context: this session reworked the semantic visualizer end to end and landed a
batch of state-integrity / self-heal fixes. Full session narrative + file
pointers in `docs/HANDOFF.md`; audit in `docs/STATE-INTEGRITY-AUDIT.md`. LANDED
this session (see git, `Visualizer:` + state-integrity commits): bounded/annealed
force sim that always settles; semantic CLIP+note k-NN spring attraction (alike
photos draw together); rebalance + live-tunable knobs (`graph.attraction`,
`graph.neighbor_attraction`, `graph.neighbor_rest_length`); a live topic-strength
slider; "soft topics" unified INTO the Overlooked lens (detect unnamed coherent
clusters via `synthesis.ts unnamedClusters`, list + glow them). Open items below.

> **Seam 1 visualizer proof + the sim-state pass LANDED** (`32251af` + `b883dd3`,
> June 17 2026 - see LANDED.md). The self-heal poll is deleted (visualizer now
> refreshes on `vectorsVersion`), every node-set re-seed reheats via
> `reseedAndRestart` (the `expandSuper` jitter is fixed), and the rest predicate /
> anneal clamp are pure + unit-tested. What REMAINS of the packet is below.

- [ ] **Frontend coupling packet (P1-P7) - the audit's punch-list** (founder,
  June 17 2026). Full findings + anchors + dependency-ordered packet in
  **`docs/AUDIT-FRONTEND-COUPLING.md`** (the same implicit-seam / staleness bug
  class as the visualizer fixes, swept across all 5 frontend axes; the dispatch
  spine, escape ladder, lane machine, and note/mic snapshots audited CLEAN). The
  debt concentrates one ring out from the visualizer. Order:
  - **P1 - `ingestExpecting` watchdog** (CONFIRMED `STATE-MACHINE.md §6e`): set in
    3 paths (`app.svelte.ts:696/1500/2277`), cleared only on an `ingest-progress`
    event (`:861`); a rescan that returns Ok but emits no status (deleted path /
    zero change) strands the grid on "Indexing..." forever. Clear on the
    `scanning`/images signal it bridges to + a named-const watchdog timeout.
    Smallest blast radius, highest user-visible payoff. (The shell comment
    claiming "cannot strand" is wrong - fix it too.)
  - **P2 - generalize Seam 1 to grid + inspector** (= the old "Seam 1 generalized"
    item; `ARCHITECTURE-CONTRACTS.md` rollout step 3). Add `images` + `journal`
    monotonic counters beside `vectorsVersion`; DELETE the `App.svelte:261`
    `setInterval(INGEST_RELIST_MS)` AND the `app.svelte.ts:867` 2s throttle AND
    the `onJournalChanged` membership relist (`:1781`) - two redundant timers +
    a membership test where the visualizer now has one versioned signal. Unblocks P3.
  - **P3 - key the visualizer caches on the data-version** (B1/B2): `graphStateKey`
    (`graphstore.ts:42`, used at `TopicGraph.svelte:2115`) omits `alpha` /
    `fullLibrary` / version, so a reopen-after-ingest can restore a stale layout
    the missing-half guard won't refresh (the persistence-side twin of the poll bug).
  - **P4-P7** (low): `selectedTopic` by phrase not array index (B3); explicit
    query/similar->graph scope instead of silent `{kind:'library'}` fallback (B4);
    single-clearer pencil session rotation (C1); source-window event tag (E1) +
    the Seam 3 constants sweep (B5). Detail + anchors in the audit doc.
  - **Watch item:** if the coarse library-wide `vectors` counter shows false
    refreshes (unrelated-scope write nudging an empty-join graph), refine to
    per-(scope x space) granularity. Not urgent - the visualizer is calm today.

- [ ] **Seam 2 tail: re-embed on an unchanged-`model_id` swap** (June 17 2026 -
  the one remaining Seam 2 gap; the transient-skip coverage LANDED, see LANDED.md).
  If weights are replaced under the SAME `model_id`, `repend_passes_for_model`
  sees no model change and re-embeds nothing - the user must `rebuild_index` /
  rescan. Options: a content hash of the weights file folded into the staleness
  check, or an explicit "force re-embed this space" op. Low priority (you don't
  normally reuse an id for new weights), but it's the honest hole.

- [ ] **Soft-topic v2: dogfood the force balance + tune defaults** (founder,
  June 15 2026): the ghost-anchor + promote-to-topic feature LANDED (`acc74c8`,
  see LANDED.md). Remaining is the numbers pass: restart `tauri dev`, work the
  topic-strength slider + `tuning.toml` (`[graph] attraction /
  neighbor_attraction / neighbor_rest_length / repulsion`), and tune the
  defaults from the feel. Architecture is settled; this is dials.

- [ ] **Host the fp16 CLIP + re-pin the manifest** (founder, June 15 2026 — ops):
  the fp16 single-file CLIP is a NOMINAL/unhostable manifest entry (the immich-app
  `local-fp16-convert` revision 404s). It was regenerated on margo, staged
  locally, and registered in `installed.json`; the embedder-bypass (fp16 ->
  installed compatible model) covers fresh machines for now. TO MAKE IT
  DOWNLOADABLE: host the 3 files (on margo at `~/fp16-convert/dfn5b-fp16/`) then
  re-pin the fp16 entry in `crates/photoproof-core/src/runtime/manifest.rs` with
  the real repo + revision + SHAs. SHAs already computed: visual `06554df3…`,
  textual `8617a89a…`, tokenizer `6d9109cc…`. margo scratch (~6 GB at
  `~/fp16-convert/`) can be cleaned once hosted.

## Performance / SOTA (audit June 13 2026)

Cited gap analysis (our stack vs 2025-2026 SOTA, adversarially verified). The
findings live in **docs/PERF-AUDIT.md**; the dependency-ordered build plan (where
each lands, exact API, effort/risk/win/validation) is **docs/PLAN-PERF.md**.
Ordered by the plan below. Validate the spikes; do not act on unverified
magnitudes.

- [ ] **CoreML CLIP graph fragmentation - the fp16 macOS load tax** (founder,
  June 16 2026): the fp16 DFN5B CLIP fragments into ~64 CoreML partitions (visual
  36 + textual 28; ~100 of ~1500 nodes per tower fall off CoreML), and per-
  partition model-prep is pathological: a MEASURED cache-WARM load was **878s
  (~14.6 min)** on an M1, stalling the embed drain + UI for a quarter hour EVERY
  launch (surfaced as "embedder loading" that never finished; slot stuck
  `Building`). IT IS NOT EP-SPECIFIC: Metal (`CPUAndGPU`) cold AND warm both
  measured **~18-21 min** too, so the cause is the GRAPH, not ANE vs Metal.
  WHAT WE RULED OUT: the 96+72 `Sqrt` "can't constant-fold" warnings were a RED
  HERRING (an ORT optimization miss, not the CoreML partition cause). Folded them
  on margo (onnxsim shape-infer+constant-fold in fp32 -> `Sqrt`/`Shape` = 0,
  re-converted to fp16, numerically equivalent, staged at
  `models/...dfn5b-fp16-folded/` on the M1) and re-measured: load was STILL ~18-21
  min. So a DIFFERENT op forces the ~36 boundaries (one per transformer block).
  PRIME SUSPECTS from the folded op histogram: `LayerNormalization` (66; fused op
  some CoreML EP versions reject) and the QuickGELU `Sigmoid`/`Gather` patterns -
  UNCONFIRMED. NEXT STEP: run onnxruntime+CoreML VERBOSE on the M1 to capture the
  exact op-type kicked to CPU, then re-export to avoid it (decompose/substitute).
  INTERIM SHIPPED: macOS + plain-CPU builds default to the **int8 base on CPU**
  (`config.rs EmbedderConfig::default`, platform-cfg) - ~1.5s load, background
  embedding; the fp16 model stays in the manifest (one `[embedder] model=` line
  away). CUDA build still defaults to fp16 (TensorRT compiles the whole graph, no
  fragmentation; 54-85x). Knobs/harness left in place: `PHOTOPROOF_ORT_COREML_UNITS`
  (ane|gpu|all), `coreml_spike.rs coreml_spike_fp16_gpu_load` + `PP_FP16_MODEL_DIR`
  override. Pairs with the CoreML EP item below + docs/SPIKE-COREML.md.

- [~] **CoreML EP spike (the embedding bottleneck)** - SPIKE DONE June 14, verdict
  **SHIP-WITH-FP16** (`docs/SPIKE-COREML.md` + `crates/photoproof-connectors/tests/
  coreml_spike.rs`, merged `17255f5`). The int8 tower could not load under CoreML (the
  397-file external-data split), but an INLINED FP16 visual tower (converted from the
  Immich FP32, same lineage as our int8) LOADS and runs **8.77x** faster than CPU
  (18 -> 162 img/min), near-lossless (cosine vs CPU min 0.9956; vs FP32 min 0.99998).
  MLProgram + CPUAndNeuralEngine. ONE caveat: a ~16.5 min FIRST-LOAD compile, so
  production must set `.with_model_cache_dir(...)`. CODE WIRING LANDED June 14: the
  CoreML compiled-model cache (`.with_model_cache_dir`, beside each tower) + the
  `...__dfn5b-fp16` model spec (`ort_embedder.rs`/`model_specs.rs`, gate green, CPU
  default byte-identical) - so the env-knob CoreML path now compiles once not per
  launch, and the fp16 id is buildable by the eval rig. EVAL HELD + FLIPPED ON THE
  M1 PRO June 14: COCO-1k nDCG 0.8212 (fp16/CoreML) vs 0.8225 (int8/CPU), R@10 up,
  MRR within 0.3% - retrieval-safe. Per-model CoreML gating
  (`OrtEmbedder::clip`, macOS + `-fp16` only; int8/text stay CPU) + the fp16
  `manifest.rs` entry are committed; this machine's `installed.json` + `config.toml`
  select fp16, so the desktop app runs CLIP on CoreML (re-embeds the library under
  the fp16 space on next launch; revert = delete config.toml). REMAINING for ALL
  users: (a) HOST the fp16 files at a real URL + re-pin (it is locally converted);
  (b) graduate the env knob to a config FIELD; (c) CUDA EP for the Ryzen/5080
  desktop (`docs/RUNTIME-MATRIX.md` target-hardware). NOTE (d) text-embed on
  CoreML was SPIKED + REJECTED June 14 (0.48-0.64x slower; the EmbeddingGemma
  transformer graph does not partition to the ANE) - it STAYS int8/CPU, its best
  path (`docs/SPIKE-COREML-TEXT.md`, `coreml_spike_text.rs`). ORIGINAL: we run
  `ort` CPU-only; enable ONNX Runtime's CoreML EP (MLProgram, NOT legacy
  NeuralNetwork which casts FP16 and can flip predictions). Immich shipped this in
  v2.2.0 (PR #17718).
- [ ] **Visualizer off main thread, then WebGL** (WKWebView check: GO - the probe
  + `docs/SPIKE-WKWEBVIEW.md` landed; Workers/WebGL2 universal, OffscreenCanvas on
  Sonoma 14+; confirm via the startup `webviewcaps` console line on the target Mac).
  The graph sim is all-pairs O(N^2) Canvas-2D on the MAIN THREAD (sustains ~5k
  nodes). Interim: move the existing sim into a Web Worker so it stops blocking.
  Full: WebGL render (Sigma.js) + GPU/Barnes-Hut O(N log N) layout (cosmos.gl scales
  to 1M+). Pairs with the existing graph-perf work.
- [ ] **Off-main-thread thumbnail decode** (small/optional; WKWebView check GO -
  Workers + createImageBitmap universal). CORRECTION from the recon: the grid is
  ALREADY virtualized (`gridlayout.ts` visible-window + DOM pool) and `Thumb.svelte`
  already uses `<img decoding="async">`, so this is a control upgrade, not a fix.
  Optional: `createImageBitmap` in a Worker. Do only if scroll-decode jank is
  actually measured. (See P7 in PLAN-PERF.md.)
- [ ] **USearch HNSW at scale** - DEFERRED, scale-triggered. Brute-force int8
  MRL-512 is CORRECT now (negligible vs HNSW under ~100k per arXiv 2409.06464).
  Trigger: when a library crosses ~tens of thousands of images, benchmark the
  M-series brute-force scan against the <100ms contract and adopt USearch HNSW
  (int8 274k QPS vs 171k f32 @ 98.9% recall@1) if needed. (The "~10x past 1M"
  justification was refuted 1-2.)

## Next polish round (small, founder-requested)

- [ ] **Voice chunking tuning** — first live run (June 2026) works end to
  end ("it is making finals and saving notes"), but utterance
  segmentation needs a deliberate tuning round against real dictation.
  The knobs, all in one place so the round is empirical, not archaeology:
  (a) server-side endpoint rules in `pp-asr-server` — rule2 1.2 s
  trailing silence after decoded speech (the main "when does a sentence
  end" feel), rule1 2.4 s, rule3 20 s max utterance; (b) the engine's
  `TRAILING_SHIP_MS` 3 s ship window (must stay > the rules it feeds);
  (c) silero hang `HANG_WINDOWS` 15 x 32 ms = 480 ms (gate flap vs
  intra-sentence pauses) and ENTER/EXIT 0.5/0.35 thresholds; (d)
  `asr.chunk_ms` config (160 ms default — latency vs throughput).
  Consider whether consecutive finals within a short gap on the SAME
  scope should merge into one journal entry (a capture-policy question,
  not a knob). THE TOOL EXISTS: `pp_voice_bench` (synth + run modes, all
  knobs as flags, --json for sweeps) — first sweeps bracket rule2
  between 0.6 (over-splits intra-sentence pauses) and 1.2 (merges 0.8 s
  thought-pauses); real tuning needs founder dictation clips (drop wavs
  in gitignored test-corpora/voice/). The harness's first catch — the
  engine's FIFO onset-association binding text to the WRONG onset when
  VAD and ASR disagree on segment count — is FIXED (B72: proximity
  association + merged-onset retirement + one stream clock,
  `8c2393b`/`6739de9`); the tuning round itself remains open. (Founder,
  first voice dogfood, June 2026.)
  TUNING ROUND 1 FINDINGS (June 12, founder-corpus-driven): cold-start
  first-word chop FIXED (engine pre-roll PRE_ROLL_MS 400, `cec8604`,
  verified on the corpus). Endpoint-tail truncation ("actually incred",
  "Kee[per]") is INVARIANT to rule2 (1.2/1.5/2.0), feed pacing
  (realtime vs fast), wire chunk size (50/160 ms), and pre-roll length
  - while flush-minted finals (disarm/Done path) always come back
  COMPLETE and raw ungated feeds through the SAME server emit full
  tails. Conclusion: something in the gated stream's content around
  the tail; NEXT FORENSIC: a --dump-shipped tee in pp_voice_bench
  (write exactly what the engine shipped to a wav; raw-feed that wav
  back - splits engine-content from server-behavior in one move).
  Mumble-zone mid-word dropouts ("fogens") are invariant to exit/hang
  knobs - likely model-level on quiet speech; quantify with the
  audiobook WER harness (below). pp-asr-server has an endpoint-grace
  mechanism (--endpoint-grace-ms + energy early-out) defaulted OFF:
  the corpus showed deferred resets clip the next word's start when
  pauses run short.
  RE-PRIORITIZED BY B74 (June 12): the truncation class root-caused to
  the export's baked-in lookahead (docs/SPIKE-ASR35.md) - the 560 ms pin
  swap supersedes further old-model pipeline forensics (dump-shipped tee
  et al now low-priority); chunking FEEL tuning (rule2, merge policy)
  remains live and applies to any model.

- [~] **Roots and subfolders: the long-practice design round** (founder,
  June 2026): MOSTLY LANDED `770fc5f` (merge `7c26126`) - see LANDED. Resolved:
  overlapping roots (decided: REFUSE nesting + navigate to the existing root,
  no double-ingest); deep-tree ergonomics (lazy expansion, filter/jump-to-folder);
  root lifecycle (archive/unarchive non-destructive via v14, moved/removed-root
  relink + `root-removed` stale already existed). STILL OPEN: (a) **group-by-volume**
  in the Folders tab (greyed offline groups) - explicitly deferred in `770fc5f` as
  "more than a small change, would reshape the row provider"; online/volume state
  is already on `RootDto`. (b) the open framing: whether the Folders tab should
  group roots by year-shaped naming, and how the collections-first philosophy
  shapes how much folder UI we even want. Pairs with the sidebar design pass.
  (Founder, June 2026.)
- [ ] **Model-landscape survey** (founder, June 2026 - periodic): the
  toolchain is modular by seam, so every block deserves a recurring
  look at the leading alternatives: ASR, VAD, LLM, image embedder, text
  embedder, reranker. docs/MODELS.md is the living matrix; refresh it
  quarterly or when a release moves the frontier (the Nemotron 3.5 day
  proved the swap evaluation costs an afternoon).
- [ ] **Nemotron 3.5 upgrade watch** (B74): trigger = sherpa-onnx Rust
  crate release with 3.5 support (runtime landed in their master June
  12; official exports live at csukuangfj2/...-2026-06-11). Then: pin
  the 560 ms int8 export, wire the per-stream language option, rerun
  the voice corpus + Alice WER STREAMED, spike-style latency/RSS
  numbers. Brings native punctuation/capitalization + 40 locales.
  PLAN WRITTEN `docs/PLAN-NEMOTRON-35.md` (June 14): go/no-go = NO-GO
  today, STAGED. Trigger UNMET - newest published `sherpa-onnx` Rust
  crate is 1.13.2 (May 14), predating 3.5 (C++ master only, ~June 12,
  PR 3671); pp-asr-server still pins 1.13.2. The 560 ms int8 export entry
  is staged in `manifest.rs` with REAL SHAs at `tiers: vec![]` (offered
  nowhere - live ASR path untouched) + a guard test. GO = a crate release
  carrying 3.5 + the language binding; then flip tiers, bump the crate,
  wire `en`/`auto`, run validation. See the plan for the full delta.
  UPDATE (June 14): 3.5 ALREADY LANDED via a DIFFERENT path - the
  `parakeet-rs` engine behind `engine-parakeet` (`docs/PLAN-NEMOTRON-35-SIDECAR.md`),
  whose §7.4 latency/RSS A/B PASSED on both machines (see LANDED). So 3.5 is
  shipping-ready today without the crate. This B74 crate-watch now narrows to
  a LATER CONSOLIDATION: if/when the k2-fsa sherpa-onnx Rust crate ships 3.5,
  evaluate retiring the younger `parakeet-rs` engine for the mature crate
  (int8, lighter RAM) - a bench-off, not a blocker.
- [ ] **Audiobook WER stress harness** (founder idea, June 2026): run a
  LONG known-transcript recording through the full pipeline - a LibriVox
  public-domain audiobook chapter (librivox.org) with its Project
  Gutenberg text. Gives three things the cards cannot: (a) word-error
  rate at scale, separating MODEL accuracy from PIPELINE truncation
  (score raw feed vs gated feed against the same transcript); (b)
  endurance - memory and drift over an hour of armed decode; (c) a
  fixed public corpus any machine reproduces. Recipe: fetch one chapter
  (solo reader, clean recording), afconvert to 16 kHz mono PCM16 into
  gitignored test-corpora/voice-long/, align the Gutenberg chapter
  text, add a WER scorer (sidecar script or a pp_voice_bench --expect
  upgrade). CORPUS FETCHED June 12: test-corpora/voice-long/ holds Alice
  ch1 (LibriVox v8 solo, 64+128 kbps -> 16 kHz wavs) + the exact
  Gutenberg transcript + caveats README; the scorer is the remaining
  piece. (Founder, June 2026.)
  SCORER LANDED `a4b9604` (June 13): `voice_wer` module + `pp-voice-bench
  --expect <transcript>` scoring RAW vs GATED feeds (gating-cost delta),
  `--json`, 10 unit tests. REMAINING: run it on the Alice corpus on the founder
  machine and read the raw-vs-gated WER delta (needs the model + gitignored wavs).
- [ ] **Import progressively: cards before hashes, previews in tiers** —
  big-folder import should SHOW something immediately: (a) discovery
  pass lists filenames and paints placeholder cards before hashing
  completes (needs a pre-identity card state — today an image exists
  only once hashed, K1; the card would carry the path until its hash
  arrives and the card re-keys), (b) a quiet per-card indicator while
  the preview builds (the previewReady placeholder is the seam — give it
  a subtle building shimmer instead of dead gray), (c) consider a
  low-res-first tier: a tiny embedded thumbnail (EXIF IFD1 ~160px) is
  readable in milliseconds even over SMB — paint it blurred-up, replace
  with the real 512px artifact when the preview pass lands. Performance
  work should be DRIVEN by pp-bench numbers (scripts/bench.sh), not
  vibes. (Founder, dogfood round 3, June 2026.)
  FRESH-INSTANCE DOGFOOD (founder, June 12, 2026) sharpened two more
  edges of the same flow — BOTH LANDED `d066fe8`: (d) instant scanning
  state — `ingestExpecting` optimistic bridge set synchronously on
  add-root/drop/rescan, cleared by the first real ingest event; the
  walk itself now reads as running (root cause was structural:
  scan_root walked the entire tree before any pass row existed, so
  `running` was false for the whole walk); (e) live discovered count —
  a per-file atomic counter on ScanOptions rides the existing
  ingest-progress channel; the empty state reads "Indexing — N
  photographs found so far…". Items (a)–(c) above (pre-identity cards,
  shimmer, low-res tier) remain open. The whole shebang remains the
  goal: add folder → instant "scanning" → live count → cards appear →
  previews fill in.
- [ ] **Stronger storage story beyond the welcome card** — the residue of
  the welcome-card item: hash-keyed sidecar recovery sweep,
  case-insensitive-filesystem rename semantics (APFS: a case-only rename
  isn't a rename; s02_2 fails on macOS today), import-time warnings on
  risky volumes. (Founder, dogfood round 3, June 2026.)

- [ ] **Full metrics suite across every pipeline stage** — when the product is feature-complete, instrument each step (ingest passes, hash/preview throughput, search latency, fold cost, capture/binding latencies, overlay render, IPC round-trips) into one coherent metrics surface (debug panel growing into a perf dashboard); founder wants "blazing fast" to be measured, not vibes. (Founder, June 2026.)

## M1.5 (scheduled concept, not yet a packet)

- [ ] Full RAW decode backfill pass (rawler/libheif worker; queue already
  knows the pass kind) — unlocks HEIC previews + RAW 1:1 zoom.
- [ ] **HEIC preview support — DEFERRED (founder, June 16 2026: "worry about
  compatibility later")**. Today HEIC is INGESTED but not decoded: the preview
  pass enqueues `Skipped`/"deferred-heic" (`library/mod.rs:1640`, §9.5), and the
  embed pass now skips to match ("preview-deferred", `f467860`) so the library
  SETTLES (no eternal "working" — the stuck-at-471 bug). HEICs simply have no
  thumbnail/embedding until this lands. RESEARCH (so it's not re-litigated): HEIC =
  HEVC-in-HEIF; there is **no production pure-Rust HEVC decoder** (patent-
  encumbered), so the options are (a) **macOS-native Image I/O** (`CGImageSource`
  via objc bindings) — ZERO bundled libs, hardware-accelerated, Apple owns HEVC
  licensing, but macOS-only; (b) **bundle libheif + libde265** — cross-platform
  but a C build matrix per OS/arch, app-size weight, and LGPL/HEVC-patent
  considerations; (c) Windows **WIC** (needs the user's HEVC Video Extension).
  RECOMMENDED when built: a `decode_heic(bytes) -> RGB8` SEAM feeding the existing
  preview tiers; macOS Image I/O behind it, libheif as the Linux/Windows backend
  later. WIRING: flip the `mod.rs:1640` Heic arm to `Pending` + decode in the
  preview pass; bump `GENERATOR_VERSION`; doctor re-pends the existing
  `deferred-heic` previews AND `preview-deferred` embeds. Fixtures already in repo:
  `test-corpora/heic-sample/*.HEIC` (43 files).
- [ ] Preview-policy settings (which previews to build/keep; LrC-style
  "build 1:1 on demand, discard after N days" knobs) — founder asked for
  exposure of these as toggles eventually.

## Milestone-attached extras (build with their milestone)

- **M2a (pencil) — P5.1 SHIPPED** (`1e06f1e`): B/E/O keys, overlay, undo/eraser, journal stroke micro-previews. The toolbar idea is ruled out for good — zero-chrome wins (U14); the old P/E/V band is retired. Review-sourced polish landed (LANDED.md) except:
- [ ] Pencil: one-euro live-stroke filter (CAPTURE §8.3 MAY) — add only if real-pen dogfood shows live wobble. (P5.1, DOGFOOD-M2.)
- **M2b (voice) — P6.1 engine (`9a5eece`) + P6.2 runtime (`fd0adc8`) SHIPPED**: sessions/scope ring/VAD-onset binding/voice pipeline/corrections/linking, mock/stub-verified (supervisor, downloads incl. byte-zero license gate, tiers, scheduler, consent card, OpenAI-compatible + sherpa-WS clients); M-key mic row still reserved — un-reserving needs the real arm path (P6.3). All eight P6.1→P6.2 wiring obligations closed by P6.2 (the items live in LANDED.md).
- [ ] M2b: hold-to-talk duality; journal-changed event (above) becomes load-bearing.
- **M3 (retrieval/collections)**: rail source-list grows collections + saved
  searches; drag-selection-to-rail filing; query-residue indicator segment
  with one-key clear; chip-creation UI (parser-driven); select-from-note ↔
  collection filing workflow chain.
- **M3 north star (founder)**: ONE unified retrieval system across all
  surfaces — toggles, filters, and sorting modes power users can configure
  precisely, over an excellent zero-config default where a quick search
  just pops the right image. Power-user depth must never tax the quick
  path (the <100 ms as-you-type budget and quiet defaults are the floor).
- **Stroke-aware retrieval (founder + design, pre-M3)**: strokes are
  already searchable via has_strokes (built), the stroke↔utterance link
  (K9 — words spoken while drawing find the stroke; provenance carries
  linked_stroke), and stroke provenance in results. NEW: (a) gesture
  semantics — classify stroke geometry (circle/X/underline/arrow) into
  searchable intent ("images I X'd out"); raw points are stored, pure
  downstream consumer. (b) region-conditioned visual embeddings — embed
  the CIRCLED CROP, not the frame: visual search conditioned on where the
  photographer's attention went. Both M3+/M4 candidates.
- **M3 additions (founder, dogfood round 2)**: the fuzzy quiet-toggle over
  metadata (camera/lens/filename, typo-tolerant) LANDED — see LANDED.md
  (additive widening below exact FTS, never default-on, lexical-lane only).
  **M3 design decision still to make**: when collections become
  browsable grids ("collection view"), does search turn contextual — e.g.
  a right sidebar scoped to the collection — instead of the full-canvas
  destination? (Tension: the right edge is reserved for journal/partner;
  founder suspects he'll want search-as-sidebar there. Decide at M3 design
  time, not before.) Full-canvas search stands until then.
- **M4 (time)**: Look bottom-edge stroke scrubber (seat reserved); journal
  timeline rendering upgrade; trajectories as an alternate grid lens.
  - **Library-wide event timeline** (founder, June 2026): a view of WHEN
    annotation activity happened across ALL folders — every event is
    db-stored with ts + session, so this is a query + rendering problem,
    no new capture machinery: sessions as spans, events as marks, click
    lands on the image/journal. Natural M4 fit (it IS the time milestone);
    consider it the journal-timeline upgrade's library-level sibling.
- **M5 (partner)**: right-edge dockable panel sharing the inspector slot;
  summon key reserved; obeys Tab lights-out unconditionally.

## Visualization lenses (founder, June 13 2026 — design docs written)

- [x] (LANDED `cbe20c2`, merge `feefde4`; details in LANDED.md) **Attention / engagement heatmap** — see `docs/DESIGN-ATTENTION-HEATMAP.md`.
  Engagement-intensity per image from capped dwell (NEW local telemetry, 60s/
  focus cap, tiered: Look-open full, grid-select far less) + annotation counts
  (stroke COUNT small; effort dropped). Grid heat-tint toggle + sort-by-attention.
  NOT gaze surveillance; dwell lives outside the journal, local-only, resettable.
- [ ] **Semantic topic-graph (v3)** — see `docs/DESIGN-SEMANTIC-GRAPH.md`. v1 +
  v2 LANDED (see LANDED.md): v1 = manual-seed topics + cheap suggestions +
  looks/said blend slider + live force layout + full-library scale spike; v2 =
  `cluster_topics` note-grounded auto-labels (deterministic k-means) + a
  full-library LOD (super-node aggregation / expand-on-click) + the v3 seam
  scaffold. REMAINING: v3 LLM topic suggestion — wire the real Gemma connector
  into the existing `suggest_topics_llm` seam (it returns `Unavailable` until
  then). ALSO OPEN (v2 founder-review): reconcile `graph.lod_threshold`
  (placeholder 1500) with the real full-library scale-spike numbers once the
  founder profiles the spike.
- [ ] **Heatmap x graph synthesis (FUTURE opportunity)** (founder, June 13 2026):
  once both exist, combine them. Two payoffs the founder named: (a) **"hot
  topics"** — overlay engagement intensity onto the topic-graph so the themes
  you actually spend attention on light up (where heat clusters in the semantic
  space); (b) **"missing themes from ignored images"** — the inverse: surface
  topic regions / image clusters with LOW engagement (high semantic coherence
  but little dwell/annotation), i.e. coherent groups of work you've been
  neglecting. The graph gives the semantic structure, the heatmap gives the
  attention field; multiplying them reveals both what's hot and what's been
  overlooked. Design round of its own once the two primitives land.
- [ ] **Compare module (4th view mode)** (founder, June 13 2026 - deferred, not
  high priority yet): a side-by-side compare view. ARCHITECTURE IS READY - it
  drops onto the `viewMode` axis (`grid|visualizer|look|compare`) as ~5 additive
  edits per the `docs/DESIGN-VIEW-MODES.md` litmus (a `ViewMode` member, one
  App.svelte render arm, an `activeHash` + `dwellRefocus` arm, an
  `enterCompare`/`leaveCompare` pair on the `openVisualizer` template + a trigger,
  a `CompareSurface.svelte`, and optionally one `scope.ts` rule). DEFAULTS already
  reasoned: 2-up side-by-side, 3-4 in a small grid, synced zoom/pan on with a
  toggle, click a pane to focus it. THE DICTATION/NOTES QUESTION the founder flagged
  ("tag the other photo's hash, similar to multiselect"): a compare note can reuse
  the existing multi-target `event_targets` (ordered by `position`, no schema
  change) - the focused pane is the subject (position 0) and the comparand(s) ride
  along tagged (positions 1+), so the note shows in both journals. THREE options to
  decide when picked up: (1) focused-primary + tagged comparand (recommended - note
  is A's, framed as "compared from A" in B's journal); (2) equal multi-target like
  multiselect (identical on both, no subject); (3) focused-only (single target, no
  comparison link). OPEN QUESTIONS raised but not resolved: whether the
  "compared from <subject>" back-reference in the comparand's journal is wanted or
  noise; one shared note across both panes vs noting each pane separately (two
  independent notes each tagging the other); how rating works (rate the focused
  pane, or a "pick this one" verb that ranks A over B); strictly 2-up vs genuine
  N-up (changes "the other hash" from singular to plural); and whether compare is a
  persistent view you return to or a transient "hold these two up" gesture. Needs a
  short design round on those before building. (Founder, June 13 2026.)

- [ ] **Similarity grouping + duplication-tolerance lens** (founder, June 17 2026
  - "worth a wide think"). OBSERVATION that sparked it: with NO topics, the
  visualizer ALREADY groups by visual similarity (founder screenshot of
  `photoproof_test_set`: near-dup bursts stack tightly, B&W / silhouette / family
  shots pool separately). That grouping is EMERGENT from the CLIP-cosine k-NN
  neighbor springs (`graph_neighbors` -> `knn_within`, `ppvec.rs:1030`); the
  union-find we built for soft topics (`synthesis.ts unnamedClusters:367`) already
  carves that graph into clumps. So the SIGNAL is built; this is about SURFACING it.
  THE PRODUCT IDEA (founder): a **"duplication-tolerance" slider** that does not
  delete but **HIDES** images similar enough to each other (often same-session
  bursts) so the founder sees fewer, MORE VARIED, more interesting results - a
  diversity/representative-subset view, not a cull-and-destroy tool. At tolerance 0
  show everything; raise it and each similarity cluster collapses to a
  representative (medoid / highest-rated / sharpest), hiding the rest.
  TWO TIERS, be explicit (the algorithms differ): (a) CLIP cosine = "do these LOOK
  alike / same scene" (already built, drives the slider's grouping); (b) a cheap
  PERCEPTUAL HASH (dHash/pHash on the preview we already decode; derived +
  rebuildable u64) = "is this the SAME photo" for precise near-dup cull + burst
  detection when fused with `capture_ts`. Exact byte-dupes already collapse via
  BLAKE3 (K13). Surfaces as stacks (folds in the open "Burst/HDR-bracket stacks"
  item) or a "Duplicates"/"Diversify" scope; keep/cull decisions are TRUTH ->
  sidecar events. Opt-in toggle (it is destructive-adjacent). REFERENCE: look at
  dupeGuru's picture mode (block-based average-color + match-% threshold).
  **WIDE THINK + cited SOTA research LANDED: `docs/DESIGN-DEDUP-AND-SIMILARITY.md`**
  (deep-research, 24 verified claims). Headline recommendation: a THREE-tier stack
  - BLAKE3 exact (built) / perceptual hash dHash or pHash via the Rust `img_hash`
  crate for precise "same photo" near-dup (the small add; threshold EMPIRICALLY
  calibrated - the normality assumption was refuted) / CLIP cosine kNN for "looks
  alike" (built; also our crop+rotation fallback, so likely NO local-feature tier
  needed). The tolerance slider = greedy FACILITY-LOCATION (or MMR-lambda) over the
  existing `knn_within` graph to collapse each cluster to a representative; the
  0-100% dupeGuru/digiKam slider is the UX template. Bursts = `capture_ts` +
  similarity. Recommended phasing + open questions (esp. the empirical Hamming
  threshold, and whether one slider spans both Hamming+cosine spaces) in the doc.
  Adjacent use cases the engine generalizes to: ML dataset de-dup, reverse-image/
  copyright (Meta SSCD), content-versioning, "best of burst" auto-pick.

## Lighting up M3 (the semantic-search chain, in order)

- [ ] **Real embedder connector + backfill packet**: implement the
  Embedder seam against the pinned models (RUNTIME process or in-process
  ort, per spike findings), let the existing P7.1 embedding passes chew
  through the library, flip STATUS.md's mock-only retrieval rows live.
- [ ] **Spike session 2, desktop half** (needs the RTX 5080 machine):
  tier-2 throughput calibration, CUDA posture, the full RUNTIME 12.4
  concurrency matrix.
- [ ] **Golden-query retrieval eval** (post-dogfood, M3 quality gate):
  founder-built query set over his real annotated library; settles S4
  always-on weight (B69) and the reranker go/no-go. HARNESS BUILT (awaiting
  the real query set): pure IR metrics + golden-set format in
  `crates/photoproof-core/src/retrieval_eval.rs` (P@k/R@k/MRR/nDCG, unit
  tested), a CI-gated synthetic sample in
  `crates/photoproof-core/tests/retrieval_eval_sample.rs`, and the runner
  `pp-retrieval-eval` (`src/bin/pp_retrieval_eval.rs`). TO RUN THE GATE: drop
  the real query set (JSON; format documented in `retrieval_eval.rs`) at the
  gitignored `test-corpora/retrieval/`, then sweep weights, e.g.
  `cargo run -p photoproof-core --bin pp-retrieval-eval -- --db <photoproof.db>
  --queries test-corpora/retrieval/golden.json --json` and re-run with `--s4
  0.5` (etc.) to diff the metric deltas. See `test-corpora/retrieval/`.

## Collections (B71 — the M3 curation thread)

- [ ] **Collection-note composer (UI slice)**: the storage, merge rules,
  and commands (add_collection_note / collection_notes) landed with
  P7.3 - collections carry their own append-only notes, a deliberately
  separate kind from image journal events (about the grouping's intent,
  not any image). Missing: the composer - a notes area when viewing a
  collection in the rail tab, possibly a "note the collection" verb
  while its grid is open. (Founder, June 2026.)
- [ ] **Collection-level rollups from member notes (LLM)** - founder
  idea, June 2026; posture split to respect K14 ("machine prose is
  retrieval fuel only; the journal preserves YOURS"): (a) FUEL TIER,
  uncontroversial: LLM-derived collection summaries, invisible,
  search/context only - "find that melancholy series" works without
  visible machine prose; (b) NUDGE TIER: surface quiet observations
  ("seven of twelve notes here mention fog") that invite the USER to
  write the collection note - machine notices, human authors; ties into
  the encourage-collecting principle and autosuggest below. AVOIDED by
  recommendation: machine-drafted notes entering the store as content,
  even behind an accept button - search provenance would quote words the
  photographer never said. FOUNDER CALL pending on whether (b) ever
  graduates toward drafting.
- [ ] **Autosuggest collections** (founder, June 2026): the app should
  NATURALLY encourage collecting — that is the point of gathering all
  this disparate context. Beyond manual creation, propose collections
  quietly from signals the app already has: images co-annotated in one
  session, repeated phrases across voice/typed notes, time+folder
  affinity, search queries the user runs repeatedly. Surface as a quiet
  suggestion (never a modal); accepting one creates the collection with
  evented membership. Needs a design round — record signals first,
  suggest later is a legitimate v1 (the membership tables make late
  suggestions retroactively useful).

## Decided, awaiting founder appetite

- [ ] Full interface themes (light chrome + grays) — token architecture
  ready; surround-luminance shipped in P4.2 (D6).
- [ ] Configurable external editor (D4 revisit).
- [ ] Type-to-jump filename in grid (Search covers it meanwhile).
- [ ] Burst/HDR-bracket stacks beyond RAW+JPEG.
- [ ] GPS map view; histogram in Look (needs decode-pipeline access).
- [ ] Very-large grid cells served by display previews (>512px targets).
- [~] CI pipeline: checked-in workflows cover three-OS quality/package smoke,
  production signing/update gates, dependency advisories, NVIDIA packaging,
  and mandatory APFS proof. Remote runs and the nightly full-scale `#[ignore]`
  lane remain open.

## Recorded, not designed (K17 — unchanged)

Future fine-tuning of a small LLM for app tasks; voice-command retraction;
audio-retention opt-in; multi-machine sync as a product feature.

## Won't build (UI-FEATURESET §8 + D3 — kept here so they stay decided)

Color labels / pick-reject flags · metadata editing · image editing ·
import/copy/move workflows · in-app deletion (D3) · multi-window/tabs ·
auto-hide chrome · keyword taxonomies (collections are intent groupings with
evented membership — "tags with time" — never hierarchical vocabularies).

NOTE (June 14 2026): "metadata editing" is NARROWLY REOPENED as an opt-in, warned
TAG-EXPORT-to-files feature (see the June 14 founder thread above) - generating
keyword tags from app data and writing them into originals for interop. That is
export, not in-app metadata management; editing existing metadata as a workflow
stays off-thesis.
