//! spec/RUNTIME.md §13.3 — **Tier 0 / degraded mode is whole**: with the
//! runtime in its WORST state (every backend Failed/NotConfigured), every
//! journal feature works identically to M1 — typed notes, pencil,
//! ratings, sessions, sidecars, FTS — and below the floor NO runtime task
//! exists at all (zero children, zero download activity, not just hidden
//! UI).

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use photoproof_connectors::config::from_toml_str;
use photoproof_connectors::mock::{MockTranscriber, MockVad};
use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};
use photoproof_core::capture::{CaptureEngine, FakeClock, NoFlush, SessionEngine};
use photoproof_core::runtime::{
    ChildHandle, HealthProbe, InstanceLock, PortSource, ProbeOutcome, ProcState, ProcessId,
    RuntimeBus, Spawner, Supervisor, SupervisorConfig, WeightsGate, compiled_manifest, plan,
};
use photoproof_core::sidecar::SidecarEngine;
use photoproof_core::{EventDraft, EventStore, RemarkSource, UtcMillis};

use common::hash;

/// A spawner that PANICS if anything ever tries to spawn — the zero-
/// children assertion in its strongest form.
struct NeverSpawn(Arc<Mutex<u32>>);

impl Spawner for NeverSpawn {
    fn spawn(&mut self, _port: u16) -> std::io::Result<Box<dyn ChildHandle>> {
        *self.0.lock().unwrap() += 1;
        Err(std::io::Error::other("MUST NOT SPAWN in degraded mode"))
    }
}

struct NeverProbe;

impl HealthProbe for NeverProbe {
    fn begin(&mut self, _port: u16, _now_ms: u64) {
        panic!("no health probes in degraded mode");
    }
    fn poll(&mut self, _now_ms: u64) -> Option<(ProbeOutcome, u64)> {
        None
    }
}

struct NeverPorts;

impl PortSource for NeverPorts {
    fn allocate(&mut self) -> std::io::Result<u16> {
        panic!("no port allocation in degraded mode");
    }
}

fn ctx() -> photoproof_core::SessionContext {
    photoproof_core::SessionContext {
        app_version: "0.1.0-test".into(),
        device_id: common::DEVICE_ID.into(),
        root_context: None,
    }
}

#[test]
fn s13_3_every_journal_feature_works_with_the_runtime_in_its_worst_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("journal.db")).unwrap();
    let clock = FakeClock::new(1_780_000_000_000);

    // --- the WORST runtime state ------------------------------------------
    // Tier 0 plan: nothing configured, nothing offered, nothing spawned.
    let config = from_toml_str("").unwrap().config;
    let manifest = compiled_manifest();
    let runtime_plan = plan(&config, 0, &manifest, &BTreeMap::new());
    assert!(runtime_plan.spawns_nothing());
    // Plus a supervisor forced into Failed (every failure path lands in
    // the same quiet floor): driven against never-spawn seams.
    let spawn_attempts = Arc::new(Mutex::new(0u32));
    let lockdir = tempfile::tempdir().unwrap();
    let lock = Arc::new(InstanceLock::acquire(lockdir.path()).unwrap().unwrap());
    let mut failed_sup = Supervisor::new(
        SupervisorConfig::new(ProcessId::Asr),
        clock.clone(),
        RuntimeBus::new(),
        Box::new(NeverSpawn(spawn_attempts.clone())),
        Box::new(NeverProbe),
        Box::new(NeverPorts),
        Some(lock),
        None,
        InFlightGauge::new(),
        LostReports::new(),
        EndpointCell::new(),
    );
    // NotConfigured forever; ticking it is a no-op.
    failed_sup.set_gate(WeightsGate::NotConfigured);
    for _ in 0..100 {
        clock.advance(1_000);
        failed_sup.tick();
    }
    assert_eq!(failed_sup.state(), &ProcState::NotConfigured);
    assert_eq!(*spawn_attempts.lock().unwrap(), 0, "zero children, ever");

    // --- sessions: open, rotate, recover ------------------------------------
    let mut sessions = SessionEngine::open(&store, ctx(), clock.clone()).unwrap();
    let session = sessions.id().clone();

    // --- voice degrades QUIETLY; typed path is untouched ---------------------
    // ASR not Ready (NotConfigured): arming fails into Disarmed(error) —
    // the engine's quiet affordance, no dialog anywhere.
    let vad = MockVad::new(16_000, vec![]);
    let not_ready = MockTranscriber::new("down", 16_000).not_ready();
    let mut capture = CaptureEngine::new(clock.clone(), &not_ready, Box::new(vad), session.clone());
    capture.arm();
    assert_eq!(
        capture.mic(),
        photoproof_core::capture::MicState::DisarmedError,
        "voice fails quietly"
    );
    assert!(capture.indicator().degraded.asr_unavailable);

    // --- typed notes ----------------------------------------------------------
    let image = hash(0x42);
    let note = store
        .append(
            &session,
            EventDraft::Remark {
                source: RemarkSource::Typed,
                text: "moody light over the harbor".into(),
                targets: vec![image.clone()],
            },
            None,
        )
        .expect("typed notes work in degraded mode");

    // --- pencil ---------------------------------------------------------------
    let stroke = capture
        .commit_stroke(&store, image.clone(), common::stroke_payload())
        .expect("pencil works in degraded mode");

    // --- ratings ----------------------------------------------------------------
    store
        .append(
            &session,
            EventDraft::Rating {
                value: 4,
                targets: vec![image.clone()],
            },
            None,
        )
        .expect("ratings work in degraded mode");

    // --- FTS search --------------------------------------------------------------
    let searcher = photoproof_core::search::Searcher::open(dir.path().join("journal.db")).unwrap();
    let results = searcher
        .search("moody", &[])
        .expect("FTS works in degraded mode");
    assert_eq!(results.images.len(), 1);

    // --- sidecars -----------------------------------------------------------------
    let store_arc = Arc::new(EventStore::open(dir.path().join("journal.db")).unwrap());
    let images_dir = dir.path().join("images");
    std::fs::create_dir_all(&images_dir).unwrap();
    let engine = SidecarEngine::new_shared(
        store_arc,
        dir.path().join("journal.db"),
        dir.path(),
        NoLocate(images_dir),
    )
    .expect("sidecar engine");
    let flushed = engine.flush_all(UtcMillis::now()).expect("sidecar flush");
    assert!(!flushed.is_empty(), "sidecars write in degraded mode");

    // --- session close (the §2.5 order, no runtime in sight) ---------------------
    sessions
        .close_current(&store, &mut capture, &mut NoFlush)
        .expect("clean close");
    assert!(store.session(&session).unwrap().unwrap().ended_ts.is_some());

    // The journal carries everything; the runtime carried nothing.
    use photoproof_core::JournalEntry;
    let journal = store.folded_journal(&image).unwrap();
    assert_eq!(journal.len(), 3, "note + stroke + rating all landed");
    assert!(
        journal
            .iter()
            .any(|e| matches!(e, JournalEntry::Remark { id, .. } if *id == note.id))
    );
    assert!(
        journal
            .iter()
            .any(|e| matches!(e, JournalEntry::Stroke { id, .. } if *id == stroke.id))
    );
    assert_eq!(*spawn_attempts.lock().unwrap(), 0);
}

/// Tier-0-whole, the structural half: below the floor the PLAN offers no
/// model, schedules no download, and proposes no child — and the §5.4
/// consent sum is zero (nothing to consent to).
#[test]
fn tier_0_offers_nothing_downloads_nothing_spawns_nothing() {
    let config = from_toml_str("").unwrap().config;
    let manifest = compiled_manifest();
    let p = plan(&config, 0, &manifest, &BTreeMap::new());
    assert!(p.spawns_nothing(), "zero children");
    assert!(manifest.offered_at(0).is_empty(), "zero offers");
    assert_eq!(manifest.total_bytes_at(0), 0, "zero download bytes");
}

/// Locator stub: every image lands in a writable temp spot — sidecar
/// writing itself is what §13.3 exercises here.
struct NoLocate(std::path::PathBuf);

impl photoproof_core::sidecar::ImageLocator for NoLocate {
    fn locate(
        &self,
        image: &photoproof_core::ContentHash,
    ) -> photoproof_core::sidecar::AdjacentLocation {
        photoproof_core::sidecar::AdjacentLocation::Writable {
            image_path: self.0.join(format!("{}.jpg", image.as_str())),
        }
    }
}
