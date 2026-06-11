//! spec/RUNTIME.md §8 supervision decision logic, fully scripted: fake
//! clock, scripted spawner/children/probes — no real time, no sleeps, no
//! sockets (the OS boundary lives in runtime_process.rs). Traceability:
//! names carry the spec ids (§8.1 …, §13.10).

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};
use photoproof_core::capture::{Clock, FakeClock};
use photoproof_core::runtime::{
    ChildHandle, ChildRegistry, HealthProbe, InstanceLock, PortSource, ProbeOutcome, ProcState,
    ProcessId, RuntimeBus, RuntimeEvent, Spawner, Supervisor, SupervisorConfig, WeightsGate,
};

// ----------------------------------------------------------- scripted seams

#[derive(Default)]
struct ChildState {
    exited: Option<i32>,
    term_received: bool,
    kill_received: bool,
    ignore_term: bool,
    /// Ordered signal record for the §8.4 normal-order assertion.
    signal_log: Vec<&'static str>,
}

#[derive(Clone)]
struct ChildControl(Arc<Mutex<ChildState>>);

impl ChildControl {
    fn exit_with(&self, code: i32) {
        self.0.lock().unwrap().exited = Some(code);
    }

    fn ignore_term(&self) {
        self.0.lock().unwrap().ignore_term = true;
    }

    fn state(&self) -> (bool, bool, Vec<&'static str>) {
        let s = self.0.lock().unwrap();
        (s.term_received, s.kill_received, s.signal_log.clone())
    }
}

struct ScriptedChild {
    pid: u32,
    control: ChildControl,
}

impl ChildHandle for ScriptedChild {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn start_time(&self) -> Option<u64> {
        Some(u64::from(self.pid) * 100)
    }
    fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        Ok(self.control.0.lock().unwrap().exited)
    }
    fn terminate(&mut self) {
        let mut s = self.control.0.lock().unwrap();
        s.term_received = true;
        s.signal_log.push("term");
        if !s.ignore_term {
            s.exited = Some(-15);
        }
    }
    fn kill(&mut self) {
        let mut s = self.control.0.lock().unwrap();
        s.kill_received = true;
        s.signal_log.push("kill");
        s.exited = Some(-9);
    }
}

#[derive(Clone, Default)]
struct ScriptedSpawner {
    next_pid: Arc<Mutex<u32>>,
    spawned_ports: Arc<Mutex<Vec<u16>>>,
    children: Arc<Mutex<Vec<ChildControl>>>,
    fail_next: Arc<Mutex<bool>>,
}

impl ScriptedSpawner {
    fn latest_child(&self) -> ChildControl {
        self.children
            .lock()
            .unwrap()
            .last()
            .expect("a child")
            .clone()
    }

    fn spawn_count(&self) -> usize {
        self.children.lock().unwrap().len()
    }

    fn ports(&self) -> Vec<u16> {
        self.spawned_ports.lock().unwrap().clone()
    }
}

impl Spawner for ScriptedSpawner {
    fn spawn(&mut self, port: u16) -> std::io::Result<Box<dyn ChildHandle>> {
        if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
            return Err(std::io::Error::other("scripted spawn failure"));
        }
        let mut pid = self.next_pid.lock().unwrap();
        *pid += 1;
        self.spawned_ports.lock().unwrap().push(port);
        let control = ChildControl(Arc::new(Mutex::new(ChildState::default())));
        self.children.lock().unwrap().push(control.clone());
        Ok(Box::new(ScriptedChild { pid: *pid, control }))
    }
}

/// Scripted probe: each `begin` consumes one `(delay_ms, outcome)` step;
/// an exhausted script repeats its last step.
#[derive(Clone)]
struct ScriptedProbe {
    script: Arc<Mutex<VecDeque<(u64, ProbeOutcome)>>>,
    last: Arc<Mutex<(u64, ProbeOutcome)>>,
    pending: Arc<Mutex<Option<(u64, ProbeOutcome)>>>,
    begins: Arc<Mutex<u32>>,
}

impl ScriptedProbe {
    fn new(steps: Vec<(u64, ProbeOutcome)>) -> Self {
        let last = steps.last().cloned().unwrap_or((0, ProbeOutcome::Ready));
        Self {
            script: Arc::new(Mutex::new(steps.into())),
            last: Arc::new(Mutex::new(last)),
            pending: Arc::new(Mutex::new(None)),
            begins: Arc::new(Mutex::new(0)),
        }
    }

    fn push(&self, delay_ms: u64, outcome: ProbeOutcome) {
        self.script
            .lock()
            .unwrap()
            .push_back((delay_ms, outcome.clone()));
        *self.last.lock().unwrap() = (delay_ms, outcome);
    }

    fn begin_count(&self) -> u32 {
        *self.begins.lock().unwrap()
    }
}

impl HealthProbe for ScriptedProbe {
    fn begin(&mut self, _port: u16, now_ms: u64) {
        *self.begins.lock().unwrap() += 1;
        let step = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.last.lock().unwrap().clone());
        *self.pending.lock().unwrap() = Some((now_ms + step.0, step.1));
    }

    fn poll(&mut self, now_ms: u64) -> Option<(ProbeOutcome, u64)> {
        let mut pending = self.pending.lock().unwrap();
        if let Some((at, _)) = *pending
            && now_ms >= at
        {
            let (at, outcome) = pending.take().expect("checked");
            return Some((outcome, at));
        }
        None
    }
}

struct SeqPorts(u16);

impl PortSource for SeqPorts {
    fn allocate(&mut self) -> std::io::Result<u16> {
        self.0 += 1;
        Ok(self.0)
    }
}

// ------------------------------------------------------------------ harness

struct Rig {
    clock: FakeClock,
    sup: Supervisor<FakeClock>,
    spawner: ScriptedSpawner,
    probe: ScriptedProbe,
    gauge: InFlightGauge,
    lost: LostReports,
    endpoint: EndpointCell,
    bus_rx: std::sync::mpsc::Receiver<RuntimeEvent>,
    _lockdir: tempfile::TempDir,
}

fn rig(probe_steps: Vec<(u64, ProbeOutcome)>) -> Rig {
    rig_with(probe_steps, true, None)
}

fn rig_with(
    probe_steps: Vec<(u64, ProbeOutcome)>,
    with_lock: bool,
    registry: Option<Arc<ChildRegistry>>,
) -> Rig {
    let clock = FakeClock::new(1_780_000_000_000);
    let bus = RuntimeBus::new();
    let bus_rx = bus.subscribe();
    let spawner = ScriptedSpawner::default();
    let probe = ScriptedProbe::new(probe_steps);
    let gauge = InFlightGauge::new();
    let lost = LostReports::new();
    let endpoint = EndpointCell::new();
    let lockdir = tempfile::tempdir().unwrap();
    let lock = with_lock.then(|| {
        Arc::new(
            InstanceLock::acquire(lockdir.path())
                .unwrap()
                .expect("lock acquired"),
        )
    });
    let sup = Supervisor::new(
        SupervisorConfig::new(ProcessId::Llm),
        clock.clone(),
        bus,
        Box::new(spawner.clone()),
        Box::new(probe.clone()),
        Box::new(SeqPorts(40_000)),
        lock,
        registry,
        gauge.clone(),
        lost.clone(),
        endpoint.clone(),
    );
    Rig {
        clock,
        sup,
        spawner,
        probe,
        gauge,
        lost,
        endpoint,
        bus_rx,
        _lockdir: lockdir,
    }
}

impl Rig {
    fn start(&mut self) {
        self.sup.set_desired_run(true);
        self.sup.set_gate(WeightsGate::Installed);
    }

    /// Advance + tick in `step_ms` increments for `total_ms`.
    fn run(&mut self, total_ms: u64, step_ms: u64) {
        let mut elapsed = 0;
        while elapsed < total_ms {
            self.clock.advance(step_ms);
            elapsed += step_ms;
            self.sup.tick();
        }
    }

    fn drain_bus(&self) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.bus_rx.try_recv() {
            out.push(e);
        }
        out
    }

    fn run_to_ready(&mut self) {
        self.start();
        self.run(2_000, 100);
        assert!(
            self.sup.is_ready(),
            "rig should reach Ready, at {:?}",
            self.sup.state()
        );
        self.drain_bus();
    }
}

// -------------------------------------------------------------------- tests

#[test]
fn defaults_match_the_spec_numbers() {
    let cfg = SupervisorConfig::new(ProcessId::Llm);
    assert_eq!(cfg.starting_poll_ms, 500, "§8.1: poll every 500 ms");
    assert_eq!(cfg.startup_timeout_ms, 120_000, "§4.4 default 120 s");
    assert_eq!(cfg.ready_poll_ms, 5_000, "§8.1: liveness every 5 s");
    assert_eq!(cfg.backoff_cap_ms, 60_000, "§8.1: capped at 60 s");
    assert_eq!(cfg.max_attempts, 5);
    assert_eq!(cfg.attempt_window_ms, 600_000, "per ROLLING 10 minutes");
    assert_eq!(cfg.drain_grace_ms, 5_000);
    assert_eq!(cfg.term_grace_ms, 5_000);
}

#[test]
fn s8_1_starting_polls_at_500ms_cadence_through_503_loading_to_ready() {
    // llama returns 503 while loading: two Loading probes, then 200.
    let mut rig = rig(vec![
        (0, ProbeOutcome::Loading),
        (0, ProbeOutcome::Loading),
        (0, ProbeOutcome::Ready),
    ]);
    rig.start();
    assert_eq!(rig.sup.state(), &ProcState::Starting);
    rig.sup.tick(); // t=0: probe 1 begins + resolves Loading
    assert_eq!(rig.probe.begin_count(), 1);
    rig.run(499, 1); // within the 500 ms cadence: no second probe
    assert_eq!(rig.probe.begin_count(), 1, "§8.1: every 500 ms, not faster");
    rig.run(1, 1); // t=500: probe 2 → Loading
    assert_eq!(rig.probe.begin_count(), 2);
    assert_eq!(rig.sup.state(), &ProcState::Starting, "503 = still loading");
    rig.run(500, 1); // t=1000: probe 3 → Ready
    assert!(rig.sup.is_ready());
    assert!(
        rig.endpoint.get().is_some(),
        "the endpoint cell carries the port only at Ready (§8.2/§8.3)"
    );
    let events = rig.drain_bus();
    assert!(
        events.contains(&RuntimeEvent::Readiness {
            process: ProcessId::Llm,
            ready: true
        }),
        "§8.3 readiness event on the core bus"
    );
}

#[test]
fn s8_1_startup_timeout_kills_and_restarting_picks_a_new_port() {
    let mut rig = rig(vec![(0, ProbeOutcome::Loading)]); // loads forever
    rig.start();
    let first_child = rig.spawner.latest_child();
    rig.run(120_000, 500); // the full startup budget
    assert!(
        matches!(rig.sup.state(), ProcState::Backoff { .. }),
        "timeout ⇒ kill ⇒ Restarting/Backoff, got {:?}",
        rig.sup.state()
    );
    assert!(first_child.state().1, "child was killed");
    // Backoff(1 s) elapses → respawn on a NEW port (§8.2).
    rig.run(1_100, 100);
    assert_eq!(rig.spawner.spawn_count(), 2);
    let ports = rig.spawner.ports();
    assert_ne!(ports[0], ports[1], "Restarting picks a new port");
}

/// §13.10 — THE Busy-is-not-Lost criterion, both directions.
#[test]
fn s13_10_health_timeout_with_in_flight_is_busy_same_timeout_idle_restarts() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();

    // A long completion is in flight; the next liveness probe times out.
    let guard = rig.gauge.enter();
    rig.probe.push(0, ProbeOutcome::TimedOut);
    rig.run(5_100, 100); // past the 5 s liveness cadence
    assert!(
        rig.sup.is_ready(),
        "NO restart while requests are in flight"
    );
    assert!(
        rig.sup.is_busy(),
        "the supervisor reports Busy (debug panel)"
    );
    assert!(
        rig.sup.decision_log().any(|l| l.contains("Busy, not Lost")),
        "the rule is named in the decision log"
    );

    // Health recovers: Busy clears.
    rig.probe.push(0, ProbeOutcome::Ready);
    rig.run(5_100, 100);
    assert!(rig.sup.is_ready());
    assert!(!rig.sup.is_busy());

    // The SAME timeout with nothing in flight: restart trigger 3 (the
    // 1 s backoff may already have elapsed into the next Starting by the
    // time we look — the restart itself is the assertion).
    drop(guard);
    rig.probe.push(0, ProbeOutcome::TimedOut);
    rig.run(5_100, 100);
    assert!(!rig.sup.is_ready(), "idle health failure restarts");
    assert_eq!(rig.spawner.spawn_count(), 2, "a fresh child was spawned");
    let events = rig.drain_bus();
    assert!(events.contains(&RuntimeEvent::Readiness {
        process: ProcessId::Llm,
        ready: false
    }));
}

/// §13.10 — child exit is detected via waitpid, not via health polling
/// (health stays green throughout).
#[test]
fn s13_10_child_exit_via_waitpid_restarts_even_while_health_is_green() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();
    let _guard = rig.gauge.enter(); // even with requests in flight
    rig.spawner.latest_child().exit_with(137);
    rig.run(100, 100); // one tick — no health cadence needed
    assert!(
        matches!(rig.sup.state(), ProcState::Backoff { .. }),
        "waitpid is the ground truth, got {:?}",
        rig.sup.state()
    );
    assert_eq!(rig.sup.last_exit_code(), Some(137));
}

#[test]
fn s8_1_connection_refused_restarts_regardless_of_in_flight_load() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();
    let _guard = rig.gauge.enter();
    rig.probe.push(0, ProbeOutcome::Refused);
    rig.run(5_100, 100);
    assert!(
        !rig.sup.is_ready(),
        "refused = nothing listening = restart trigger 2"
    );
    assert_eq!(rig.spawner.spawn_count(), 2, "a fresh child was spawned");
}

/// The /slots trap: it MAY feed the debug panel but NEVER restarts — it
/// can hang/fail while /health is green.
#[test]
fn slots_cross_check_never_triggers_a_restart() {
    let clock = FakeClock::new(1_780_000_000_000);
    let bus = RuntimeBus::new();
    let spawner = ScriptedSpawner::default();
    let health = ScriptedProbe::new(vec![(0, ProbeOutcome::Ready)]);
    let slots = ScriptedProbe::new(vec![
        (0, ProbeOutcome::TimedOut),
        (0, ProbeOutcome::Failed("hung while /health green".into())),
    ]);
    let lockdir = tempfile::tempdir().unwrap();
    let lock = Arc::new(InstanceLock::acquire(lockdir.path()).unwrap().unwrap());
    let mut sup = Supervisor::new(
        SupervisorConfig::new(ProcessId::Llm),
        clock.clone(),
        bus,
        Box::new(spawner.clone()),
        Box::new(health.clone()),
        Box::new(SeqPorts(41_000)),
        Some(lock),
        None,
        InFlightGauge::new(),
        LostReports::new(),
        EndpointCell::new(),
    )
    .with_slots_probe(Box::new(slots));
    sup.set_desired_run(true);
    sup.set_gate(WeightsGate::Installed);
    for _ in 0..400 {
        clock.advance(100);
        sup.tick();
    }
    assert!(sup.is_ready(), "40 s of failing /slots: still Ready");
    assert!(
        sup.decision_log().any(|l| l.contains("slots cross-check")),
        "…but the debug panel sees it"
    );
}

#[test]
fn s8_1_backoff_series_doubles_and_caps_at_60s() {
    // A child that always dies during Starting (probe loads forever); a
    // wide attempt budget so the doubling series itself is observable.
    let clock = FakeClock::new(1_780_000_000_000);
    let spawner = ScriptedSpawner::default();
    let probe = ScriptedProbe::new(vec![(0, ProbeOutcome::Loading)]);
    let lockdir = tempfile::tempdir().unwrap();
    let lock = Arc::new(InstanceLock::acquire(lockdir.path()).unwrap().unwrap());
    let mut cfg = SupervisorConfig::new(ProcessId::Llm);
    cfg.max_attempts = 100;
    let mut sup = Supervisor::new(
        cfg,
        clock.clone(),
        RuntimeBus::new(),
        Box::new(spawner.clone()),
        Box::new(probe.clone()),
        Box::new(SeqPorts(42_000)),
        Some(lock),
        None,
        InFlightGauge::new(),
        LostReports::new(),
        EndpointCell::new(),
    );
    sup.set_desired_run(true);
    sup.set_gate(WeightsGate::Installed);

    let mut delays = Vec::new();
    for _ in 0..8 {
        assert_eq!(sup.state(), &ProcState::Starting);
        spawner.latest_child().exit_with(1);
        clock.advance(100);
        sup.tick();
        let ProcState::Backoff { until_ms, .. } = sup.state() else {
            panic!("expected Backoff, got {:?}", sup.state());
        };
        delays.push(*until_ms - clock.mono_ms());
        // Wait the backoff out so the next attempt spawns.
        let wait = *delays.last().unwrap();
        let mut left = wait + 200;
        while left > 0 {
            clock.advance(100);
            left -= 100;
            sup.tick();
        }
    }
    assert_eq!(
        delays,
        vec![1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000],
        "§8.1: exponential 1, 2, 4, 8 … capped at 60 s"
    );
}

/// §8.1 Restarting(n): reaching Ready RESETS the backoff exponent — a
/// process that recovers and crashes again later starts the series at the
/// base delay, never where the previous incident left it (the rolling
/// window above tolerates any delay ≤ 60 s, so this is the other half of
/// Restarting(n), pinned separately).
#[test]
fn s8_1_recovery_to_ready_resets_the_backoff_exponent() {
    // The probe loads forever until we push Ready — so crashes during
    // Starting can climb the exponent first.
    let mut rig = rig(vec![(0, ProbeOutcome::Loading)]);
    rig.start();

    // Crash 1 during Starting: base backoff.
    rig.spawner.latest_child().exit_with(1);
    rig.run(100, 100);
    let ProcState::Backoff { until_ms, .. } = rig.sup.state() else {
        panic!("expected Backoff, got {:?}", rig.sup.state());
    };
    assert_eq!(*until_ms - rig.clock.mono_ms(), 1_000, "first crash: 1 s");

    // Crash 2 during Starting: the exponent climbs (proves this rig can
    // observe the doubling at all).
    rig.run(1_100, 100); // backoff out → respawn → Starting
    rig.spawner.latest_child().exit_with(1);
    rig.run(100, 100);
    let ProcState::Backoff { until_ms, .. } = rig.sup.state() else {
        panic!("expected Backoff, got {:?}", rig.sup.state());
    };
    assert_eq!(
        *until_ms - rig.clock.mono_ms(),
        2_000,
        "consecutive crash without a Ready in between: doubles to 2 s"
    );

    // Now let it recover to Ready…
    rig.probe.push(0, ProbeOutcome::Ready);
    rig.run(2_500, 100);
    assert!(rig.sup.is_ready(), "recovered, at {:?}", rig.sup.state());

    // …and crash again: the series restarts at the BASE delay. Without
    // the enter_ready reset this would be 4 s — and every later incident
    // would inherit the stale exponent forever (eventually a permanent
    // 60 s first backoff).
    rig.spawner.latest_child().exit_with(1);
    rig.run(100, 100);
    let ProcState::Backoff { until_ms, .. } = rig.sup.state() else {
        panic!("expected Backoff, got {:?}", rig.sup.state());
    };
    assert_eq!(
        *until_ms - rig.clock.mono_ms(),
        1_000,
        "Ready reset the exponent: post-recovery crash backs off 1 s again"
    );
}

/// The 5-attempt budget is per ROLLING 10 minutes — not lifetime — and
/// `Failed` + the settings restart action gets a fresh budget.
#[test]
fn s8_1_rolling_window_budget_then_failed_then_fresh_budget_on_restart_action() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.start();

    // Five crashes spaced 150 s apart: by the fifth, the earliest
    // attempts have rolled OUT of the 10-minute window — no Failed.
    for _ in 0..5 {
        rig.run(2_000, 100);
        assert!(rig.sup.is_ready(), "recovers between spaced crashes");
        rig.spawner.latest_child().exit_with(1);
        rig.run(150_000, 500);
    }
    assert!(
        !matches!(rig.sup.state(), ProcState::Failed),
        "rolling window: spaced crashes never exhaust the budget"
    );

    // Now crash rapid-fire: budget burns within one window → Failed.
    let mut saw_failed = false;
    for _ in 0..8 {
        rig.run(70_000, 500); // recover (backoff ≤ 60 s) or already failed
        if matches!(rig.sup.state(), ProcState::Failed) {
            saw_failed = true;
            break;
        }
        if rig.sup.is_ready() {
            rig.spawner.latest_child().exit_with(1);
            rig.run(100, 100);
        }
    }
    assert!(
        saw_failed,
        "rapid-fire crashes exhaust the 5-per-10-min budget"
    );

    // Failed is quiet and inert…
    let spawns_at_failed = rig.spawner.spawn_count();
    rig.run(60_000, 1_000);
    assert_eq!(
        rig.spawner.spawn_count(),
        spawns_at_failed,
        "Failed = inert"
    );

    // …and the settings restart action re-enters Spawning with a FRESH
    // budget.
    rig.sup.restart_runtime();
    assert_eq!(rig.sup.restart_attempts_in_window(), 0, "fresh budget");
    rig.run(2_000, 100);
    assert!(rig.sup.is_ready(), "restart-runtime → Spawning → Ready");
}

#[test]
fn s8_1_in_flight_connection_lost_triggers_an_immediate_check() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();
    let probes_before = rig.probe.begin_count();
    // Mid-call loss reported by the connector; well inside the 5 s
    // cadence.
    rig.clock.advance(100);
    rig.lost.report();
    rig.probe.push(0, ProbeOutcome::Refused);
    rig.sup.tick();
    assert_eq!(
        rig.probe.begin_count(),
        probes_before + 1,
        "immediate check, not the 5 s cadence"
    );
    rig.sup.tick();
    assert!(
        matches!(rig.sup.state(), ProcState::Backoff { .. }),
        "refused after the loss: restart"
    );
}

#[test]
fn s8_5_supervisor_refuses_to_spawn_without_the_instance_lock() {
    let mut rig = rig_with(vec![(0, ProbeOutcome::Ready)], false, None);
    rig.start();
    assert_eq!(rig.sup.state(), &ProcState::Failed);
    assert_eq!(rig.spawner.spawn_count(), 0, "no lock, no children, ever");
    assert!(
        rig.sup.decision_log().any(|l| l.contains("instance lock")),
        "the refusal is named"
    );
}

#[test]
fn s8_4_normal_stop_order_drain_then_term_then_kill_then_reap() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();
    let child = rig.spawner.latest_child();
    child.ignore_term(); // forces the SIGKILL path
    let guard = rig.gauge.enter(); // one in-flight request to drain

    rig.sup.stop();
    assert_eq!(rig.sup.state(), &ProcState::Stopping);
    rig.run(1_000, 100);
    let (term, _, _) = child.state();
    assert!(!term, "still draining: no SIGTERM while in flight (≤ 5 s)");

    drop(guard); // drain completes
    rig.run(200, 100);
    let (term, kill, _) = child.state();
    assert!(term, "drained → SIGTERM");
    assert!(!kill, "SIGKILL only after the 5 s term grace");

    rig.run(5_000, 100);
    let (_, kill, log) = child.state();
    assert!(kill, "term ignored → SIGKILL after 5 s");
    assert_eq!(log, vec!["term", "kill"], "§8.4 order");
    rig.run(200, 100);
    assert_eq!(rig.sup.state(), &ProcState::Stopped, "reaped → Stopped");
}

#[test]
fn s8_4_drain_cap_expires_after_5s_even_with_requests_still_in_flight() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.run_to_ready();
    let child = rig.spawner.latest_child();
    let _guard = rig.gauge.enter(); // never released
    rig.sup.stop();
    rig.run(4_900, 100);
    assert!(!child.state().0, "still inside the drain cap");
    rig.run(300, 100);
    assert!(child.state().0, "drain caps at 5 s → SIGTERM regardless");
}

#[test]
fn children_json_records_on_spawn_and_clears_on_stop() {
    let regdir = tempfile::tempdir().unwrap();
    let registry = Arc::new(ChildRegistry::new(regdir.path()).unwrap());
    let mut rig = rig_with(vec![(0, ProbeOutcome::Ready)], true, Some(registry.clone()));
    rig.start();
    let recorded = registry.list();
    assert_eq!(recorded.len(), 1, "recorded at spawn (§8.4)");
    assert_eq!(recorded[0].process, "llm");
    assert!(recorded[0].start_time.is_some(), "identity proof recorded");
    assert_eq!(Some(recorded[0].port), rig.sup.port());
    rig.run(2_000, 100);
    rig.sup.stop();
    rig.run(11_000, 100);
    assert_eq!(rig.sup.state(), &ProcState::Stopped);
    assert!(registry.list().is_empty(), "cleared at reap (§8.4)");
}

#[test]
fn s8_3_readiness_events_ride_the_bus_in_both_directions() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    rig.start();
    rig.run(1_000, 100);
    assert!(rig.sup.is_ready());
    let up: Vec<_> = rig
        .drain_bus()
        .into_iter()
        .filter(|e| matches!(e, RuntimeEvent::Readiness { .. }))
        .collect();
    assert_eq!(
        up,
        vec![RuntimeEvent::Readiness {
            process: ProcessId::Llm,
            ready: true
        }]
    );
    rig.spawner.latest_child().exit_with(1);
    rig.run(100, 100);
    let down: Vec<_> = rig
        .drain_bus()
        .into_iter()
        .filter(|e| matches!(e, RuntimeEvent::Readiness { .. }))
        .collect();
    assert_eq!(
        down,
        vec![RuntimeEvent::Readiness {
            process: ProcessId::Llm,
            ready: false
        }]
    );
    assert!(rig.endpoint.get().is_none(), "endpoint cleared off-Ready");
}

#[test]
fn gate_states_map_to_machine_states_before_run() {
    let mut rig = rig(vec![(0, ProbeOutcome::Ready)]);
    assert_eq!(rig.sup.state(), &ProcState::NotConfigured);
    rig.sup.set_gate(WeightsGate::Downloading);
    assert_eq!(rig.sup.state(), &ProcState::Downloading);
    rig.sup.set_gate(WeightsGate::DownloadFailed);
    assert_eq!(rig.sup.state(), &ProcState::DownloadFailed);
    rig.sup.set_desired_run(true);
    rig.sup.set_gate(WeightsGate::Installed);
    assert_eq!(
        rig.sup.state(),
        &ProcState::Starting,
        "installed + desired ⇒ spawn"
    );
}
