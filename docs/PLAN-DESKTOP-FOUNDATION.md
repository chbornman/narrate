# PLAN - desktop application foundation

Status: local foundation delivered; native/release proof program remains
active, July 27 2026.

The source backlog is `docs/BACKLOG.md`, section "Desktop application
foundation audit - 2026-07-26". This plan is the packet and proof map for that
program. It does not narrow the backlog. The program is complete only when all
26 audit items and the linked canonical items named below are implemented,
verified at the scope they claim, and moved from BACKLOG to LANDED.

## Outcome

Photoproof:

1. maps a usable window promptly even when optional storage, hardware probes,
   caches, or model runtimes are unavailable;
2. owns every long-running task through explicit lifecycle, priority,
   cancellation, progress, error, and shutdown contracts;
3. preserves authored truth and can detect, explain, and repair every derived
   state class;
4. selects, verifies, loads, restarts, unloads, and removes models from one
   authoritative capability and operation model;
5. tells every window the same committed state, including degraded and
   fallback states;
6. ships and updates as a tested installed desktop application on Linux,
   macOS, and Windows; and
7. proves the above with executable failure, crash, platform, and performance
   evidence rather than source inspection alone.

## Requirement IDs

IDs are stable for tests, packet ledgers, and the completion audit.

| ID | Backlog requirement | Required proof |
|---|---|---|
| A01 | SupervisorHost convergence | Shell transition matrix: Run A to B, Run to dark to Run, binary loss/return, remove/reinstall, changed argv; bounded stop and replacement child |
| A02 | Real runtime restart | Failed child gets a fresh budget and reaches Ready; failed embedder retries; acknowledgement follows committed state |
| A03 | Unhosted fp16 safety | Config, offers, and explicit download all reject staged-only bytes until an immutable hosted pin exists |
| A04 | AppLifecycle and early usable window | Lifecycle transition tests plus a blocked-probe installed-shell/Tauri timing case |
| A05 | Frontend boot states | Inject each boot dependency failure; honest fatal/degraded UI and retry without relaunch |
| A06 | Managed task registry | Owner/token/key/priority/time/progress/error/terminal tests, single-flight, cancellation, zero tasks at shutdown |
| A07 | Separated work lanes and resource governance | Blocked probe/repair cannot delay ingest/status/RAW; six-hour fake clock; Eco/Balanced/Max, pause, per-pass, new-root policy |
| A08 | Coordinated shutdown | Quit at every work phase; no write after finalization begins; all tasks/children terminal by deadline |
| A09 | Application health snapshot | Backend-originated health/action coverage for every named subsystem; Rust/TypeScript contract tests |
| A10 | Readiness-driven repair | Embedder Ready triggers active-vector reconcile once; roots fail independently; S5 retention cleanup |
| A11 | Transactional migrations | Historical fixtures, statement failpoints, v14 interruption, verified backup, and two-process lock |
| A12 | Durable control files | Missing/corrupt/truncated/permission/interrupted matrix; quarantine/LKG; stable device identity; config apply/rollback |
| A13 | Idle DB maintenance | Fake-idle scheduling, WAL bound, blocked-reader retry/health, shutdown sequencing |
| A14 | Disk and backup safety | Per-store thresholds and pause policy; full backup/destroy/restore drill |
| A15 | RuntimeCapabilities and compatible offers | CPU/Metal/CUDA/TensorRT/unsupported fixtures; one compatible default per role; cache invalidation and bounded redetect |
| A16 | Actual execution provider | Requested/available/selected/fallback reporting after session creation; forced fallback; scheduler uses actual provider |
| A17 | Restartable embedder state machine | Full transition matrix, dispatch failure, hang/watchdog, retry, cancellation, switch, stale landing, role isolation |
| A18 | Serialized model operations | Concurrent download/verify/load/unload/remove/GC tests; no lost registry writes; D4/D6/D7 |
| A19 | Terminal model events | Exact operation sequence with one committed terminal snapshot; two-window broadcasts; no 100% downloading residue |
| A20 | Runtime UI from backend truth | Alternatives and all state/provider/error/compatibility cases; no model-id tables; confirmed unload/remove |
| A21 | Bundled child runtimes | Per-OS package contents and clean-machine installed smoke reaching ASR Ready; explicit llama-server decision |
| A22 | Crash diagnostics and lifecycle telemetry | Crash/relaunch retains logs; marker/panic behavior; phase/capability/build metadata; reveal/copy UI |
| A23 | Packaging and safe updater | Three OS artifacts, signing/notarization, installed smoke, signed update and rollback/failure drill |
| A24 | CI and release gates | Checked-in three-OS workflows run source, migration, bundle, and installed gates; no tolerated S6 red at completion |
| A25 | Executable lifecycle chaos matrix | Every listed startup/live/shutdown case reports phase, platform, and invariant results |
| A26 | Experience budgets | Numeric regression gates and recorded SSD/removable/NAS/CPU/Apple/NVIDIA evidence, including T2/T4 |

## Linked canonical work

These remain in their original BACKLOG entries but are required for this
program:

- S5 is complete under A10: the 30-day retention/move-correlation policy,
  boundary and protection tests, preview/all-vector cleanup, crash-safe retry,
  and retained-summary relink rebuild have landed.
- S6 is a standalone filesystem-semantics packet. It is not satisfied by A24
  preserving the known-red exception. It must define case-sensitive versus
  case-insensitive behavior, make `s02_2` green, and record default-APFS proof.
- D4, D6, and D7 belong to A18.
- D8 is split across A03 and A15. Hiding a row is insufficient.
- Backend-aware offers belong to A15.
- Terminal 100%-download settlement belongs to A19/A20.
- Ingest intensity belongs to A07 and A26.
- Unresponsive-volume launch belongs to A04 and A07.
- CUDA/provider observability and normal launch wiring belong to
  A15/A16/A20/A23.
- T2 and T4 belong to A26.

## Packet order

Packets are dependency-ordered. Later packets may add tests to earlier
infrastructure, but must not bypass it with a second lifecycle or status path.

### F0 - proof seams and immediate safety

- Add injectable child, probe, clock, build, and failpoint seams where the
  acceptance tests need them.
- A01: SupervisorHost convergence.
- A02: real runtime restart.
- A03: staged-only fp16 fail-closed behavior.

Exit: focused tests cover all P0 transitions and the public model offer set is
downloadable on a fresh install.

### F1 - lifecycle kernel

- A04 lifecycle state model and early usable window.
- A06 managed task registry.
- Initial A09 health schema.
- A22 startup phase timing, retained logs, and crash marker primitives.

Exit: all current spawn sites are inventoried and either managed or explicitly
scheduled for conversion in F2. A blocked optional probe cannot prevent a
window.

### F2 - work ownership and shutdown

- A07 independent work lanes and user governance.
- A08 bounded coordinated shutdown.
- Finish A04 blocked-volume acceptance.
- A05 frontend boot and retry UI on the stable lifecycle/health API.

Exit: no detached database/filesystem writer exists and quit-at-phase tests
prove the finalization barrier.

### F3 - persistence and repair

- A11 through A14.
- A10 including S5.
- Standalone S6 filesystem semantics.

Exit: every persisted state class has version, corruption, interruption,
recovery, reset, and observability evidence. `s02_2` is green.

### F4 - runtime authority

- A15 and A16, including normal CUDA launch wiring.
- A17 and A18.
- A19 and A20 after the operation registry exists.

Exit: one backend model owns capability, desired/actual runtime state,
operations, events, and UI projection.

### F5 - installed product

- A21 child runtime bundling.
- A23 platform packages and updater.
- A24 CI/release gates.

Exit: clean installed artifacts launch on all target OSes without workspace or
developer PATH assumptions, and signed update/rollback behavior is recorded.

### F6 - final proof

- Expand A25 continuously as each packet lands; run the full matrix.
- Close A26/T2/T4 on representative fixtures.
- Run the requirement-by-requirement completion audit.
- Move completed BACKLOG entries to LANDED and update STATUS, BUILD-LOOP,
  ROADMAP, FOUNDER-CHECKLIST, and architecture references.

## Evidence rules

The standing source gates are necessary:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets`
- `cargo test --workspace`
- frontend `check`, `test`, and `build`
- `scripts/check-no-emdash.sh`

They are not sufficient proof for installed packages, hard mounts, APFS
semantics, real execution providers, interrupted migrations, crash recovery,
updates, or performance. Those requirements close only with the scoped
evidence in the table above.

## Delivery ledger - July 27 2026

The settled local completion audit classifies 15 of 26 requirements as locally
proven. Eleven are implemented or scaffolded but remain open for the exact
native/external fact named below.

| State | Requirement IDs |
|---|---|
| Locally proven | A01, A02, A03, A05, A06, A07, A09, A10, A11, A13, A16, A17, A18, A19, A20 |
| Native/platform receipt pending | A04, A08, A12, A14, A21, A22, A25, A26 |
| Real accelerator package/profile pending | A15 |
| Signed multi-OS release proof pending | A23 |
| Remote CI receipt pending | A24 |

Local release evidence now includes Linux DEB, RPM, and AppImage artifacts.
Installed DEB and AppImage smoke both reached `Usable` in 7 ms and shut down in
0 ms, found the adjacent ASR sidecar, and completed the full backup/restore
helper contract. ASR Ready remains false because no redistribution-safe pinned
model fixture is available.

The full Rust workspace, strict Clippy, frontend typecheck/test/build, release
contract, NVIDIA wiring, punctuation gate, and 15-suite chaos matrix are green.
The chaos receipt contains 28 cases: 23 passed and 5 explicitly retained as
`fixture-passed-platform-drill-pending`.

The remaining proof is deliberately concrete: kernel-blocked NAS and CPAL
device-removal drills; installed control-file, backup, and crash/relaunch
drills; pinned-model ASR Ready; real CUDA/TensorRT execution; signed/notarized
macOS and signed Windows packages plus update rollback; remote workflow runs;
and representative SSD/removable/NAS/Apple/NVIDIA/Windows performance
receipts. S6 also awaits its default-APFS founder-Mac receipt.

Every packet close records:

1. requirement IDs changed;
2. invariant and failure behavior;
3. exact automated gates and results;
4. remaining platform/founder-machine evidence;
5. documentation and backlog movement; and
6. known reds. At final completion there are no known reds in this program.
