//! Process supervision (spec/RUNTIME.md §8.1–8.5): one supervisor task
//! per managed process, the state machine exactly as drawn, tick-driven
//! through the Clock seam — **no real sleeps in decision logic**; the
//! shell's runtime pump ticks supervisors, tests drive a FakeClock.
//!
//! The load-bearing rules, encoded here and pinned by tests:
//!
//! - **Busy is not Lost** (§8.1): `/health` queues behind inference under
//!   load, so a slow/timed-out probe while requests are in flight means
//!   Busy — no restart. Restart triggers are EXACTLY three: child exit
//!   (`try_wait` = waitpid, the ground truth), connection refused, and
//!   health failure with NO request in flight.
//! - An optional `/slots` cross-check MAY feed the debug panel but NEVER
//!   triggers a restart (it can hang while `/health` is green).
//! - `Restarting(n)`: backoff 1, 2, 4, 8 … capped at 60 s; max 5 attempts
//!   per ROLLING 10 minutes, then `Failed` (quiet feature degradation —
//!   §7). The settings "restart runtime" re-enters Spawning with a FRESH
//!   budget.
//! - Ports are per-attempt (§8.2); a child bind failure is a spawn
//!   failure and the next attempt picks a new port.
//! - Supervisors refuse to spawn without the §8.5 instance lock.

use std::collections::VecDeque;
use std::sync::Arc;

use photoproof_connectors::openai::{EndpointCell, InFlightGauge, LostReports};

use super::bus::{ProcessId, RuntimeBus, RuntimeEvent};
use super::children::{ChildRecord, ChildRegistry, InstanceLock};
use super::process::{ChildHandle, HealthProbe, PortSource, ProbeOutcome, Spawner};
use crate::capture::Clock;

/// §8.1 state machine, every named state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcState {
    /// No weights / tier 0 / disabled backend / uninstalled model ref.
    NotConfigured,
    Downloading,
    DownloadFailed,
    Spawning,
    Starting,
    Ready,
    Restarting(u32),
    Backoff {
        n: u32,
        until_ms: u64,
    },
    Failed,
    Stopping,
    Stopped,
}

impl ProcState {
    pub fn name(&self) -> &'static str {
        match self {
            ProcState::NotConfigured => "notConfigured",
            ProcState::Downloading => "downloading",
            ProcState::DownloadFailed => "downloadFailed",
            ProcState::Spawning => "spawning",
            ProcState::Starting => "starting",
            ProcState::Ready => "ready",
            ProcState::Restarting(_) => "restarting",
            ProcState::Backoff { .. } => "backoff",
            ProcState::Failed => "failed",
            ProcState::Stopping => "stopping",
            ProcState::Stopped => "stopped",
        }
    }
}

/// Weights availability, fed by the plan/download manager (§5, §8.1's
/// Downloading/DownloadFailed states).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightsGate {
    /// Backend disabled / tier 0 / model not in the offered set.
    NotConfigured,
    Downloading,
    DownloadFailed,
    Installed,
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub process: ProcessId,
    /// §8.1: poll health every 500 ms while Starting…
    pub starting_poll_ms: u64,
    /// …up to startup_timeout_secs (default 120 — cold-HDD first load).
    pub startup_timeout_ms: u64,
    /// Ready liveness cadence (5 s).
    pub ready_poll_ms: u64,
    /// Backoff base (1 s) and cap (60 s): 1, 2, 4, 8 … 60.
    pub backoff_base_ms: u64,
    pub backoff_cap_ms: u64,
    /// Max restart attempts per rolling window (5 per 10 min).
    pub max_attempts: usize,
    pub attempt_window_ms: u64,
    /// §8.4 normal-order grace: drain ≤ 5 s, then SIGTERM, 5 s, SIGKILL.
    pub drain_grace_ms: u64,
    pub term_grace_ms: u64,
}

impl SupervisorConfig {
    pub fn new(process: ProcessId) -> Self {
        Self {
            process,
            starting_poll_ms: 500,
            startup_timeout_ms: 120_000,
            ready_poll_ms: 5_000,
            backoff_base_ms: 1_000,
            backoff_cap_ms: 60_000,
            max_attempts: 5,
            attempt_window_ms: 600_000,
            drain_grace_ms: 5_000,
            term_grace_ms: 5_000,
        }
    }
}

/// Stopping sub-phases (§8.4 normal order: stop accepting → drain
/// in-flight ≤ 5 s → SIGTERM → 5 s → SIGKILL → reap → clear).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopPhase {
    Draining { deadline_ms: u64 },
    Terminating { deadline_ms: u64 },
    Reaping,
}

/// Capped history/log helper (the §8.1 Failed detail: full history + last
/// 200 lines to the debug panel).
const LOG_CAP: usize = 200;

/// Cap on the backoff shift exponent — a u64 overflow guard, NOT policy.
/// 2^16 times any sane `backoff_base_ms` already exceeds `backoff_cap_ms`,
/// so the `.min(backoff_cap_ms)` after the multiply makes this cap inert;
/// it exists only so `1 << n` cannot overflow during a long failure streak.
const BACKOFF_SHIFT_CAP: u32 = 16;

pub struct Supervisor<C: Clock> {
    cfg: SupervisorConfig,
    clock: C,
    bus: RuntimeBus,
    spawner: Box<dyn Spawner>,
    probe: Box<dyn HealthProbe>,
    /// Optional /slots cross-check: debug-panel detail ONLY.
    slots_probe: Option<Box<dyn HealthProbe>>,
    ports: Box<dyn PortSource>,
    /// §8.5: spawn refused unless the instance lock is held.
    lock: Option<Arc<InstanceLock>>,
    registry: Option<Arc<ChildRegistry>>,
    gauge: InFlightGauge,
    lost: LostReports,
    endpoint: EndpointCell,

    state: ProcState,
    gate: WeightsGate,
    desired_run: bool,
    child: Option<Box<dyn ChildHandle>>,
    port: Option<u16>,
    starting_since: Option<u64>,
    last_probe_started: Option<u64>,
    probe_in_flight: bool,
    slots_in_flight: bool,
    last_slots_started: Option<u64>,
    busy: bool,
    stop_phase: Option<StopPhase>,
    /// Rolling restart-attempt timestamps (the 5-per-10-min budget).
    attempts: VecDeque<u64>,
    /// Consecutive failed attempts since last Ready (backoff exponent).
    consecutive: u32,
    last_exit_code: Option<i32>,
    last_health_latency_ms: Option<u64>,
    history: Vec<(u64, String)>,
    log: VecDeque<String>,
}

impl<C: Clock> Supervisor<C> {
    #[allow(clippy::too_many_arguments)] // wiring constructor: every seam, once
    pub fn new(
        cfg: SupervisorConfig,
        clock: C,
        bus: RuntimeBus,
        spawner: Box<dyn Spawner>,
        probe: Box<dyn HealthProbe>,
        ports: Box<dyn PortSource>,
        lock: Option<Arc<InstanceLock>>,
        registry: Option<Arc<ChildRegistry>>,
        gauge: InFlightGauge,
        lost: LostReports,
        endpoint: EndpointCell,
    ) -> Self {
        Self {
            cfg,
            clock,
            bus,
            spawner,
            probe,
            slots_probe: None,
            ports,
            lock,
            registry,
            gauge,
            lost,
            endpoint,
            state: ProcState::NotConfigured,
            gate: WeightsGate::NotConfigured,
            desired_run: false,
            child: None,
            port: None,
            starting_since: None,
            last_probe_started: None,
            probe_in_flight: false,
            slots_in_flight: false,
            last_slots_started: None,
            busy: false,
            stop_phase: None,
            attempts: VecDeque::new(),
            consecutive: 0,
            last_exit_code: None,
            last_health_latency_ms: None,
            history: Vec::new(),
            log: VecDeque::new(),
        }
    }

    /// Attach the optional `/slots` cross-check (debug detail only —
    /// NEVER a restart trigger).
    pub fn with_slots_probe(mut self, probe: Box<dyn HealthProbe>) -> Self {
        self.slots_probe = Some(probe);
        self
    }

    // ------------------------------------------------------------ inputs ----

    /// Weights gate from the plan/download manager.
    pub fn set_gate(&mut self, gate: WeightsGate) {
        self.gate = gate;
        let now = self.clock.mono_ms();
        // Pre-run states track the gate verbatim; everything else keeps
        // its machine position (the gate matters again at the next
        // spawn decision).
        if matches!(
            self.state,
            ProcState::NotConfigured
                | ProcState::Downloading
                | ProcState::DownloadFailed
                | ProcState::Stopped
        ) {
            let mapped = match gate {
                WeightsGate::NotConfigured => ProcState::NotConfigured,
                WeightsGate::Downloading => ProcState::Downloading,
                WeightsGate::DownloadFailed => ProcState::DownloadFailed,
                WeightsGate::Installed => {
                    if self.desired_run {
                        self.enter_spawning(now);
                        return;
                    }
                    ProcState::Stopped
                }
            };
            if mapped != self.state {
                self.transition(now, mapped);
            }
        }
    }

    /// Desired-run flag (the plan wants this process up).
    pub fn set_desired_run(&mut self, run: bool) {
        self.desired_run = run;
        let now = self.clock.mono_ms();
        if run
            && matches!(self.gate, WeightsGate::Installed)
            && matches!(self.state, ProcState::Stopped | ProcState::NotConfigured)
        {
            self.enter_spawning(now);
        }
        if !run
            && matches!(
                self.state,
                ProcState::Spawning
                    | ProcState::Starting
                    | ProcState::Ready
                    | ProcState::Restarting(_)
                    | ProcState::Backoff { .. }
            )
        {
            self.begin_stop(now);
        }
    }

    /// The settings "restart runtime" action: re-enters Spawning with a
    /// FRESH attempt budget (§8.1).
    pub fn restart_runtime(&mut self) {
        let now = self.clock.mono_ms();
        self.attempts.clear();
        self.consecutive = 0;
        self.note(now, "restart-runtime: fresh attempt budget");
        if matches!(self.gate, WeightsGate::Installed) {
            self.desired_run = true;
            self.kill_and_clear_child();
            self.enter_spawning(now);
        }
    }

    /// §8.4 normal shutdown entry.
    pub fn stop(&mut self) {
        let now = self.clock.mono_ms();
        self.desired_run = false;
        if matches!(
            self.state,
            ProcState::Spawning
                | ProcState::Starting
                | ProcState::Ready
                | ProcState::Restarting(_)
                | ProcState::Backoff { .. }
        ) {
            self.begin_stop(now);
        } else if !matches!(self.state, ProcState::Stopping) {
            self.transition(now, ProcState::Stopped);
        }
    }

    // ----------------------------------------------------------- queries ----

    pub fn state(&self) -> &ProcState {
        &self.state
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, ProcState::Ready)
    }

    /// Busy = slow/hung health while requests are in flight (§8.1).
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.pid())
    }

    pub fn last_exit_code(&self) -> Option<i32> {
        self.last_exit_code
    }

    pub fn last_health_latency_ms(&self) -> Option<u64> {
        self.last_health_latency_ms
    }

    pub fn history(&self) -> &[(u64, String)] {
        &self.history
    }

    pub fn decision_log(&self) -> impl Iterator<Item = &str> {
        self.log.iter().map(String::as_str)
    }

    pub fn restart_attempts_in_window(&self) -> usize {
        self.attempts.len()
    }

    // -------------------------------------------------------------- tick ----

    /// Drive the machine. Call from the runtime pump (or a test loop);
    /// never blocks.
    pub fn tick(&mut self) {
        let now = self.clock.mono_ms();
        match self.state.clone() {
            ProcState::NotConfigured
            | ProcState::Downloading
            | ProcState::DownloadFailed
            | ProcState::Failed
            | ProcState::Stopped => {}
            ProcState::Spawning => {
                // Spawning is transient (enter_spawning resolves it); a
                // tick here means a spawn mid-flight resolved elsewhere.
            }
            ProcState::Starting => self.tick_starting(now),
            ProcState::Ready => self.tick_ready(now),
            ProcState::Restarting(_) => { /* transient */ }
            ProcState::Backoff { n: _, until_ms } => {
                if now >= until_ms {
                    self.enter_spawning(now);
                }
            }
            ProcState::Stopping => self.tick_stopping(now),
        }
    }

    fn tick_starting(&mut self, now: u64) {
        // Child exit during Starting = spawn/bind failure (§8.2): the
        // next attempt picks a new port.
        if let Some(code) = self.poll_child_exit() {
            self.note(now, &format!("child exited during Starting (code {code})"));
            self.begin_restart(now, "spawn failure");
            return;
        }
        if now.saturating_sub(self.starting_since.unwrap_or(now)) >= self.cfg.startup_timeout_ms {
            self.note(now, "startup timeout: kill → Restarting");
            self.kill_and_clear_child();
            self.begin_restart(now, "startup timeout");
            return;
        }
        self.drive_probe(now, self.cfg.starting_poll_ms);
        if let Some((outcome, latency)) = self.poll_probe(now) {
            self.last_health_latency_ms = Some(latency);
            match outcome {
                ProbeOutcome::Ready => self.enter_ready(now),
                // 503-while-loading, refused-before-bind, slow disk —
                // all normal during Starting; keep polling until the
                // startup timeout.
                ProbeOutcome::Loading
                | ProbeOutcome::Refused
                | ProbeOutcome::TimedOut
                | ProbeOutcome::Failed(_) => {}
            }
        }
    }

    fn tick_ready(&mut self, now: u64) {
        // 1. waitpid is the ground truth — detected even while health is
        //    green (§13.10).
        if let Some(code) = self.poll_child_exit() {
            self.note(
                now,
                &format!("child exit detected via waitpid (code {code})"),
            );
            self.begin_restart(now, "child exit");
            return;
        }
        // 2. An in-flight ConnectionLost triggers an immediate check.
        if self.lost.drain() > 0 {
            self.note(now, "in-flight ConnectionLost: immediate health check");
            if !self.probe_in_flight
                && let Some(port) = self.port
            {
                self.probe.begin(port, now);
                self.probe_in_flight = true;
                self.last_probe_started = Some(now);
            }
        }
        // 3. Liveness cadence (5 s).
        self.drive_probe(now, self.cfg.ready_poll_ms);
        if let Some((outcome, latency)) = self.poll_probe(now) {
            self.last_health_latency_ms = Some(latency);
            match outcome {
                ProbeOutcome::Ready => {
                    if self.busy {
                        self.note(now, "health recovered: Busy cleared");
                    }
                    self.busy = false;
                }
                ProbeOutcome::Refused => {
                    // Restart trigger 2 — regardless of in-flight load:
                    // refused means nothing is listening.
                    self.note(now, "connection refused: restart");
                    self.begin_restart(now, "connection refused");
                    return;
                }
                ProbeOutcome::TimedOut | ProbeOutcome::Loading | ProbeOutcome::Failed(_) => {
                    if self.gauge.count() > 0 {
                        // THE BUSY-IS-NOT-LOST RULE: /health queues behind
                        // inference under load; with requests in flight
                        // this is Busy — NO restart.
                        if !self.busy {
                            self.note(
                                now,
                                "health probe failed with requests in flight: Busy, not Lost",
                            );
                        }
                        self.busy = true;
                    } else {
                        // Restart trigger 3: health failure with NO
                        // request in flight.
                        self.note(now, "health failed with nothing in flight: restart");
                        self.begin_restart(now, "health failure (idle)");
                        return;
                    }
                }
            }
        }
        // 4. Optional /slots cross-check: debug detail ONLY — its
        //    outcome (including hangs/failures) never restarts.
        if let Some(slots) = &mut self.slots_probe {
            if !self.slots_in_flight
                && now.saturating_sub(self.last_slots_started.unwrap_or(0))
                    >= self.cfg.ready_poll_ms
                && let Some(port) = self.port
            {
                slots.begin(port, now);
                self.slots_in_flight = true;
                self.last_slots_started = Some(now);
            }
            if self.slots_in_flight
                && let Some((outcome, _)) = slots.poll(now)
            {
                self.slots_in_flight = false;
                let line = format!("slots cross-check: {outcome:?} (informational only)");
                self.note(now, &line);
            }
        }
    }

    fn tick_stopping(&mut self, now: u64) {
        match self.stop_phase {
            Some(StopPhase::Draining { deadline_ms }) => {
                if self.gauge.count() == 0 || now >= deadline_ms {
                    if let Some(child) = &mut self.child {
                        child.terminate();
                    }
                    self.note(now, "drain complete/expired: SIGTERM");
                    self.stop_phase = Some(StopPhase::Terminating {
                        deadline_ms: now + self.cfg.term_grace_ms,
                    });
                }
            }
            Some(StopPhase::Terminating { deadline_ms }) => {
                if self.poll_child_exit().is_some() {
                    self.finish_stop(now);
                } else if now >= deadline_ms {
                    self.note(now, "SIGTERM grace expired: SIGKILL");
                    if let Some(child) = &mut self.child {
                        child.kill();
                    }
                    self.stop_phase = Some(StopPhase::Reaping);
                }
            }
            Some(StopPhase::Reaping) => {
                if self.poll_child_exit().is_some() {
                    self.finish_stop(now);
                }
            }
            None => self.finish_stop(now),
        }
    }

    // ------------------------------------------------------- transitions ----

    fn enter_spawning(&mut self, now: u64) {
        self.transition(now, ProcState::Spawning);
        // §8.5 belt-and-braces: no instance lock, no children. Ever.
        if self.lock.is_none() {
            self.note(now, "spawn refused: instance lock not held (§8.5)");
            self.transition(now, ProcState::Failed);
            return;
        }
        let port = match self.ports.allocate() {
            Ok(p) => p,
            Err(e) => {
                self.note(now, &format!("port allocation failed: {e}"));
                self.begin_restart(now, "port allocation failure");
                return;
            }
        };
        match self.spawner.spawn(port) {
            Ok(child) => {
                if let Some(reg) = &self.registry {
                    let _ = reg.record(ChildRecord {
                        process: self.cfg.process.as_str().to_owned(),
                        pid: child.pid(),
                        start_time: child.start_time(),
                        port,
                    });
                }
                self.note(now, &format!("spawned pid {} on port {port}", child.pid()));
                self.child = Some(child);
                self.port = Some(port);
                self.starting_since = Some(now);
                self.last_probe_started = None;
                self.probe_in_flight = false;
                self.transition(now, ProcState::Starting);
            }
            Err(e) => {
                self.note(now, &format!("spawn failed: {e}"));
                self.begin_restart(now, "spawn error");
            }
        }
    }

    fn enter_ready(&mut self, now: u64) {
        self.consecutive = 0;
        self.busy = false;
        if let Some(port) = self.port {
            self.endpoint.set(Some(([127, 0, 0, 1], port).into()));
        }
        self.transition(now, ProcState::Ready);
        self.bus.publish(RuntimeEvent::Readiness {
            process: self.cfg.process,
            ready: true,
        });
    }

    fn begin_restart(&mut self, now: u64, reason: &str) {
        let was_ready = matches!(self.state, ProcState::Ready);
        self.endpoint.set(None);
        self.kill_and_clear_child();
        if was_ready {
            self.bus.publish(RuntimeEvent::Readiness {
                process: self.cfg.process,
                ready: false,
            });
        }
        // Rolling 10-minute budget: drop attempts older than the window,
        // then check BEFORE consuming the next attempt.
        while let Some(&t) = self.attempts.front() {
            if now.saturating_sub(t) >= self.cfg.attempt_window_ms {
                self.attempts.pop_front();
            } else {
                break;
            }
        }
        if self.attempts.len() >= self.cfg.max_attempts {
            self.note(
                now,
                &format!(
                    "{reason}: {} attempts in the rolling window — Failed (quiet degradation)",
                    self.attempts.len()
                ),
            );
            self.transition(now, ProcState::Failed);
            return;
        }
        self.attempts.push_back(now);
        self.consecutive += 1;
        let n = self.consecutive;
        let delay = self
            .cfg
            .backoff_base_ms
            .saturating_mul(1u64 << (n - 1).min(BACKOFF_SHIFT_CAP))
            .min(self.cfg.backoff_cap_ms);
        self.note(
            now,
            &format!("{reason}: restart attempt {n}, backoff {delay} ms"),
        );
        self.transition(now, ProcState::Restarting(n));
        self.transition(
            now,
            ProcState::Backoff {
                n,
                until_ms: now + delay,
            },
        );
    }

    fn begin_stop(&mut self, now: u64) {
        let was_ready = matches!(self.state, ProcState::Ready);
        self.endpoint.set(None);
        if was_ready {
            self.bus.publish(RuntimeEvent::Readiness {
                process: self.cfg.process,
                ready: false,
            });
        }
        self.transition(now, ProcState::Stopping);
        self.stop_phase = if self.child.is_some() {
            Some(StopPhase::Draining {
                deadline_ms: now + self.cfg.drain_grace_ms,
            })
        } else {
            None
        };
        self.tick_stopping(now);
    }

    fn finish_stop(&mut self, now: u64) {
        self.child = None;
        self.port = None;
        self.stop_phase = None;
        if let Some(reg) = &self.registry {
            let _ = reg.clear(self.cfg.process.as_str());
        }
        self.transition(now, ProcState::Stopped);
    }

    // ----------------------------------------------------------- helpers ----

    fn drive_probe(&mut self, now: u64, cadence_ms: u64) {
        if self.probe_in_flight {
            return;
        }
        let due = match self.last_probe_started {
            None => true,
            Some(at) => now.saturating_sub(at) >= cadence_ms,
        };
        if due && let Some(port) = self.port {
            self.probe.begin(port, now);
            self.probe_in_flight = true;
            self.last_probe_started = Some(now);
        }
    }

    fn poll_probe(&mut self, now: u64) -> Option<(ProbeOutcome, u64)> {
        if !self.probe_in_flight {
            return None;
        }
        let result = self.probe.poll(now)?;
        self.probe_in_flight = false;
        Some(result)
    }

    fn poll_child_exit(&mut self) -> Option<i32> {
        let code = self.child.as_mut()?.try_wait().ok()??;
        self.last_exit_code = Some(code);
        self.child = None;
        if let Some(reg) = &self.registry {
            let _ = reg.clear(self.cfg.process.as_str());
        }
        Some(code)
    }

    fn kill_and_clear_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill();
            // Bounded reap: try_wait is non-blocking; the OS integration
            // test covers the real reap timing. One immediate attempt
            // here collects fast exits.
            if let Ok(Some(code)) = child.try_wait() {
                self.last_exit_code = Some(code);
            }
        }
        if let Some(reg) = &self.registry {
            let _ = reg.clear(self.cfg.process.as_str());
        }
        self.port = None;
    }

    fn transition(&mut self, now: u64, to: ProcState) {
        if self.state == to {
            return;
        }
        self.state = to.clone();
        self.history.push((now, to.name().to_owned()));
        if self.history.len() > LOG_CAP {
            self.history.remove(0);
        }
        self.bus.publish(RuntimeEvent::StateChanged {
            process: self.cfg.process,
            state: to.name().to_owned(),
            at_ms: now,
        });
    }

    fn note(&mut self, now: u64, line: &str) {
        self.log
            .push_back(format!("[{now}ms {}] {line}", self.cfg.process.as_str()));
        if self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }
}
