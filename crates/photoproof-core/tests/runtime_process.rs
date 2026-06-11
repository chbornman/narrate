//! spec/RUNTIME.md §8 at the honest OS boundary: the REAL spawner,
//! probes, signals, ports, and the children.json net, exercised against
//! the scriptable stub child binary (BUILD-LOOP: supervision is verified
//! against stub child processes). Real-time waits are bounded and tight;
//! decision logic itself is covered by runtime_supervisor.rs on the fake
//! clock.
#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};
use photoproof_core::capture::SystemClock;
use photoproof_core::runtime::{
    ChildRecord, ChildRegistry, HttpHealthProbe, InstanceLock, OsPortSource, OsSpawner, ProcState,
    ProcessId, RotatingLog, RuntimeBus, SpawnSpec, Spawner, Supervisor, SupervisorConfig,
    WeightsGate,
};

const STUB: &str = env!("CARGO_BIN_EXE_photoproof-stub-child");

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn sigkill(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Snappy test timings; the spec'd defaults are asserted in
/// runtime_supervisor.rs.
fn fast_cfg() -> SupervisorConfig {
    let mut cfg = SupervisorConfig::new(ProcessId::Llm);
    cfg.starting_poll_ms = 20;
    cfg.startup_timeout_ms = 10_000;
    cfg.ready_poll_ms = 100;
    cfg.backoff_base_ms = 50;
    cfg.drain_grace_ms = 200;
    cfg.term_grace_ms = 400;
    cfg
}

struct OsRig {
    sup: Supervisor<SystemClock>,
    registry: Arc<ChildRegistry>,
    endpoint: EndpointCell,
    _dir: tempfile::TempDir,
}

fn os_rig(stub_args: &[&str], log: Option<RotatingLog>) -> OsRig {
    let dir = tempfile::tempdir().unwrap();
    let lock = Arc::new(InstanceLock::acquire(dir.path()).unwrap().unwrap());
    let registry = Arc::new(ChildRegistry::new(dir.path()).unwrap());
    let endpoint = EndpointCell::new();
    let mut args: Vec<String> = vec!["--port".into(), "{port}".into()];
    args.extend(stub_args.iter().map(|s| (*s).to_string()));
    let sup = Supervisor::new(
        fast_cfg(),
        SystemClock::new(),
        RuntimeBus::new(),
        Box::new(OsSpawner {
            spec: SpawnSpec {
                program: STUB.into(),
                args,
                log,
            },
        }),
        Box::new(HttpHealthProbe::new(Duration::from_millis(500))),
        Box::new(OsPortSource),
        Some(lock),
        Some(registry.clone()),
        InFlightGauge::new(),
        LostReports::new(),
        endpoint.clone(),
    );
    OsRig {
        sup,
        registry,
        endpoint,
        _dir: dir,
    }
}

/// Tick until the predicate holds (bounded).
fn pump_until(
    sup: &mut Supervisor<SystemClock>,
    timeout: Duration,
    mut pred: impl FnMut(&Supervisor<SystemClock>) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        sup.tick();
        if pred(sup) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn spawns_health_gates_through_503s_and_recovers_from_a_sigkill_mid_ready() {
    let mut rig = os_rig(&["--health-503-count", "2"], None);
    rig.sup.set_desired_run(true);
    rig.sup.set_gate(WeightsGate::Installed);
    assert!(
        pump_until(&mut rig.sup, Duration::from_secs(10), |s| s.is_ready()),
        "stub answers 503 twice then 200 → Ready; got {:?}",
        rig.sup.state()
    );
    let first_pid = rig.sup.pid().expect("pid at Ready");
    assert!(rig.endpoint.get().is_some(), "endpoint carries the port");
    assert_eq!(
        rig.registry.list().len(),
        1,
        "children.json holds the child"
    );

    // §13.1/§13.2 shape: SIGKILL the child mid-Ready → waitpid detects →
    // restart with backoff → Ready again, no human in the loop.
    sigkill(first_pid);
    assert!(
        pump_until(&mut rig.sup, Duration::from_secs(10), |s| {
            s.is_ready() && s.pid() != Some(first_pid)
        }),
        "supervisor restarted to a fresh child; got {:?}",
        rig.sup.state()
    );
    assert_ne!(rig.sup.pid(), Some(first_pid));

    // Clean stop: child gone, registry cleared.
    let last_pid = rig.sup.pid().unwrap();
    rig.sup.stop();
    assert!(pump_until(&mut rig.sup, Duration::from_secs(5), |s| {
        s.state() == &ProcState::Stopped
    }));
    assert!(rig.registry.list().is_empty(), "cleared at reap (§8.4)");
    let gone = Instant::now() + Duration::from_secs(3);
    while pid_alive(last_pid) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!pid_alive(last_pid), "no orphan after normal stop (§13.1)");
}

/// §8.4 Linux: PR_SET_PDEATHSIG(SIGKILL) in pre_exec — children die when
/// the PARENT is SIGKILLed. The intermediate parent uses the production
/// spawn path (`--spawn-supervised` calls OsSpawner inside the stub).
#[test]
#[cfg(target_os = "linux")]
fn pdeathsig_kills_the_grandchild_when_the_parent_is_sigkilled() {
    use std::io::BufRead;
    let mut parent = std::process::Command::new(STUB)
        .args([
            "--spawn-supervised",
            STUB,
            "--port",
            "0", // ephemeral; the grandchild just needs to live
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn parent");
    let stdout = parent.stdout.take().expect("parent stdout");
    let mut lines = std::io::BufReader::new(stdout).lines();
    let grandchild: u32 = loop {
        let line = lines
            .next()
            .expect("parent prints the grandchild pid")
            .expect("readable");
        if let Some(rest) = line.strip_prefix("GRANDCHILD ") {
            break rest.trim().parse().expect("pid");
        }
    };
    assert!(pid_alive(grandchild), "grandchild runs under the parent");

    // SIGKILL the parent — no cleanup code can run; only PDEATHSIG saves
    // us.
    sigkill(parent.id());
    let _ = parent.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !pid_alive(grandchild),
        "PDEATHSIG: the grandchild died with its parent (§8.4)"
    );
}

/// §8.4 normal order: a SIGTERM-ignoring child is SIGKILLed after the
/// term grace.
#[test]
fn sigterm_ignoring_child_takes_the_sigkill_path() {
    let mut rig = os_rig(&["--ignore-sigterm"], None);
    rig.sup.set_desired_run(true);
    rig.sup.set_gate(WeightsGate::Installed);
    assert!(pump_until(&mut rig.sup, Duration::from_secs(10), |s| s.is_ready()));
    let pid = rig.sup.pid().unwrap();
    let stop_started = Instant::now();
    rig.sup.stop();
    assert!(pump_until(&mut rig.sup, Duration::from_secs(5), |s| {
        s.state() == &ProcState::Stopped
    }));
    assert!(
        stop_started.elapsed() >= Duration::from_millis(400),
        "the SIGKILL escalation waited out the term grace"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!pid_alive(pid), "killed despite ignoring SIGTERM");
}

/// §8.4 crash net: a TRUE stale child (matching PID + start-time) is
/// killed on startup sweep; the recycled-PID refusal is pinned in the
/// children-module unit tests.
#[test]
fn children_json_net_kills_a_true_stale_child_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ChildRegistry::new(dir.path()).unwrap();
    // Orphan a real stub child (no supervisor watching it).
    let mut spawner = OsSpawner {
        spec: SpawnSpec {
            program: STUB.into(),
            args: vec!["--port".into(), "0".into()],
            log: None,
        },
    };
    let child = spawner.spawn(0).unwrap();
    let pid = child.pid();
    let start_time = child.start_time();
    assert!(start_time.is_some(), "identity proof available on Linux");
    registry
        .record(ChildRecord {
            process: "llm".into(),
            pid,
            start_time,
            port: 0,
        })
        .unwrap();
    std::mem::forget(child); // simulate the crash: nobody reaps it

    assert!(pid_alive(pid), "orphan is alive before the sweep");
    let (killed, skipped) = registry.kill_orphans().unwrap();
    assert_eq!(killed, vec!["llm".to_owned()]);
    assert!(skipped.is_empty());
    // The test process is the orphan's parent (mem::forget skipped the
    // reap), so the killed child lingers as a zombie until we waitpid it
    // — which doubles as the proof of death: waitpid returns a
    // SIGKILL-terminated status.
    let mut status: libc::c_int = 0;
    let reaped = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
    assert_eq!(reaped, pid as i32, "the orphan exited");
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "killed by the sweep's SIGKILL, status {status}"
    );
    assert!(registry.list().is_empty());
}

/// §8.2: child bind failure = spawn failure; Restarting picks a new port.
#[test]
fn double_bind_is_a_spawn_failure_and_the_next_attempt_picks_a_new_port() {
    use photoproof_core::runtime::PortSource;
    // A port source that first hands out an OCCUPIED port.
    struct CollideFirst {
        occupied: u16,
        handed: bool,
    }
    impl PortSource for CollideFirst {
        fn allocate(&mut self) -> std::io::Result<u16> {
            if !self.handed {
                self.handed = true;
                return Ok(self.occupied);
            }
            photoproof_core::runtime::allocate_port()
        }
    }
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied = blocker.local_addr().unwrap().port();

    let dir = tempfile::tempdir().unwrap();
    let lock = Arc::new(InstanceLock::acquire(dir.path()).unwrap().unwrap());
    let mut sup = Supervisor::new(
        fast_cfg(),
        SystemClock::new(),
        RuntimeBus::new(),
        Box::new(OsSpawner {
            spec: SpawnSpec {
                program: STUB.into(),
                args: vec!["--port".into(), "{port}".into()],
                log: None,
            },
        }),
        Box::new(HttpHealthProbe::new(Duration::from_millis(500))),
        Box::new(CollideFirst {
            occupied,
            handed: false,
        }),
        Some(lock),
        None,
        InFlightGauge::new(),
        LostReports::new(),
        EndpointCell::new(),
    );
    sup.set_desired_run(true);
    sup.set_gate(WeightsGate::Installed);
    assert!(
        pump_until(&mut sup, Duration::from_secs(10), |s| s.is_ready()),
        "bind failure → exit 2 → Restarting → fresh port → Ready; got {:?}",
        sup.state()
    );
    assert_ne!(
        sup.port(),
        Some(occupied),
        "the new attempt left the occupied port"
    );
    assert_eq!(
        sup.last_exit_code(),
        Some(2),
        "the bind failure was observed"
    );
    sup.stop();
    pump_until(&mut sup, Duration::from_secs(5), |s| {
        s.state() == &ProcState::Stopped
    });
}

/// §8.6: child stdout is captured into the rotating log.
#[test]
fn child_output_lands_in_the_rotating_log() {
    let logdir = tempfile::tempdir().unwrap();
    let log = RotatingLog::open_default(logdir.path().join("llm.log")).unwrap();
    let mut rig = os_rig(&[], Some(log.clone()));
    rig.sup.set_desired_run(true);
    rig.sup.set_gate(WeightsGate::Installed);
    assert!(pump_until(&mut rig.sup, Duration::from_secs(10), |s| s.is_ready()));
    // The stub prints "stub-child: listening on {port}" at startup.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut captured = String::new();
    while Instant::now() < deadline {
        captured = std::fs::read_to_string(log.path()).unwrap_or_default();
        if captured.contains("listening on") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        captured.contains("[out] stub-child: listening on"),
        "stdout piped to the §8.6 log, got: {captured:?}"
    );
    rig.sup.stop();
    pump_until(&mut rig.sup, Duration::from_secs(5), |s| {
        s.state() == &ProcState::Stopped
    });
}
