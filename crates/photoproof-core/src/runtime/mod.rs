//! The local model runtime (spec/RUNTIME.md) — supervision, weights,
//! tiers, scheduling. Packet P6.2, mock/stub-verified by design
//! (docs/BUILD-LOOP.md honest-scope table: real llama.cpp/sherpa/GPU
//! numbers are the P6.3 founder-machine spike).
//!
//! - `bus`: readiness/state events on the core bus (§8.3)
//! - `supervisor`: the §8.1 state machine, tick-driven over seams —
//!   no real sleeps in decision logic; the Clock seam rules (P6.1 pattern)
//! - `process`: seam traits + the real OS implementations (§8.4 spawn
//!   with pdeathsig/process groups, kill/reap; §8.6 child log capture)
//! - `children`: the `children.json` crash net + the §8.5 instance lock
//! - `ports`: §8.2 random localhost port allocation
//! - `logs`: §8.6 rotating log files (5 MB × 5)
//! - `manifest`: the compiled-in model manifest (§5.1) + license records
//! - `download`: resumable SHA-pinned downloads, license-gated (§5.2–5.3)
//! - `tier`: hardware tiers — detection seam + the §6.2 decision table
//! - `scheduler`: the §9 pass scheduler (lanes, cancellation, mic hold)
//! - `gc`: model updates + GC holds (§11)
//! - `plan`: config + tier + installed → per-process plans (§4.4, §7)

pub mod bus;
pub mod children;
pub mod download;
pub mod gc;
pub mod launch;
pub mod logs;
pub mod manifest;
pub mod plan;
pub mod ports;
pub mod process;
pub mod scheduler;
pub mod supervisor;
pub mod tier;

pub use bus::{ProcessId, RuntimeBus, RuntimeEvent};
pub use children::{ChildRecord, ChildRegistry, InstanceLock, process_start_time};
pub use download::{
    DownloadError, DownloadManager, DownloadOutcome, InstalledRecord, NoPace, Pacer, SleepPacer,
    available_disk_bytes, is_retryable_status,
};
pub use gc::{GcCoordinator, GcDecision, ReindexGate, StubReindexGate};
pub use logs::RotatingLog;
pub use manifest::{
    AcceptanceRecord, Acceptances, FileEntry, License, Manifest, ModelEntry, compiled_manifest,
};
pub use plan::{ProcessPlan, RuntimePlan, plan};
pub use ports::allocate_port;
pub use process::{
    ChildHandle, HealthProbe, HttpHealthProbe, OsPortSource, OsSpawner, PortSource, ProbeOutcome,
    SpawnSpec, Spawner, WsHealthProbe,
};
pub use scheduler::{
    BackgroundJob, JobId, LlmSlotBackend, PassScheduler, SchedulerEvent, SlotBackend,
};
pub use supervisor::{ProcState, Supervisor, SupervisorConfig, WeightsGate};
pub use tier::{
    GpuAdapter, HardwareProbe, HardwareReport, TierCache, TierDecision, decide_tier, detect_tier,
    resolve_tier,
};
