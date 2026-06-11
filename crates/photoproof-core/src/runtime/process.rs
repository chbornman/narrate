//! Process-control seams + the real OS implementations
//! (spec/RUNTIME.md §8.2, §8.4, §8.6).
//!
//! The supervisor's decision logic is tick-driven over these traits and
//! the Clock seam — scripted implementations drive every unit test; the
//! real implementations here are exercised against the stub child binary
//! in `tests/runtime_process.rs` (bounded real-time waits only at the
//! honest OS boundaries: spawn, reap, signal delivery).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use photoproof_connectors::http::WsClient;
use photoproof_connectors::openai::HealthStatus;

use super::logs::RotatingLog;
use super::ports::allocate_port;

// ---------------------------------------------------------------- seams ----

/// Health-probe outcome in the supervisor's vocabulary (§8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// 200 / handshake OK.
    Ready,
    /// llama returns 503 while loading.
    Loading,
    /// Nothing listening — one of the exactly-three restart triggers.
    Refused,
    /// Slow/hung probe — Busy-not-Lost territory while requests are in
    /// flight.
    TimedOut,
    Failed(String),
}

/// One in-flight probe at a time; non-blocking poll (the real impl runs
/// the request on a worker thread).
pub trait HealthProbe: Send {
    fn begin(&mut self, port: u16, now_ms: u64);
    /// `Some((outcome, latency_ms))` once resolved.
    fn poll(&mut self, now_ms: u64) -> Option<(ProbeOutcome, u64)>;
}

/// A spawned child (the §8.4 ground truth: `try_wait` IS waitpid).
pub trait ChildHandle: Send {
    fn pid(&self) -> u32;
    /// Platform start-time for the children.json identity proof.
    fn start_time(&self) -> Option<u64>;
    /// Non-blocking waitpid; `Some(code)` once exited (signal deaths map
    /// to negative codes on Unix convention here: -signal).
    fn try_wait(&mut self) -> std::io::Result<Option<i32>>;
    /// Graceful stop (SIGTERM / CTRL-BREAK).
    fn terminate(&mut self);
    /// Hard kill (SIGKILL / TerminateProcess) — of the process GROUP on
    /// Unix, so helpers die too.
    fn kill(&mut self);
}

pub trait Spawner: Send {
    fn spawn(&mut self, port: u16) -> std::io::Result<Box<dyn ChildHandle>>;
}

pub trait PortSource: Send {
    fn allocate(&mut self) -> std::io::Result<u16>;
}

/// §8.2: bind 127.0.0.1:0, read, drop, hand off.
pub struct OsPortSource;

impl PortSource for OsPortSource {
    fn allocate(&mut self) -> std::io::Result<u16> {
        allocate_port()
    }
}

// ------------------------------------------------------------- real OS ----

/// What to launch (the vendored-binary selection table is P6.3's spike;
/// this packet launches the stub child / whatever the plan resolves).
#[derive(Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    /// Arguments; `{port}` placeholders are substituted at spawn time —
    /// the port is allocated per attempt (§8.2).
    pub args: Vec<String>,
    /// Child stdout/stderr destination (§8.6); `None` = inherit (tests).
    pub log: Option<RotatingLog>,
}

/// Real child process: dies with the parent (Linux `PR_SET_PDEATHSIG`
/// (SIGKILL) in `pre_exec` + process-group kill; macOS process group +
/// SIGTERM→SIGKILL; Windows Job Objects — §8.4).
pub struct OsChild {
    child: Child,
    start_time: Option<u64>,
    /// Unix process group id (== child pid; set via pre_exec setpgid).
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: Option<windows_job::Job>,
}

impl ChildHandle for OsChild {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn start_time(&self) -> Option<u64> {
        self.start_time
    }

    fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        Ok(self.child.try_wait()?.map(|status| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                status
                    .code()
                    .unwrap_or_else(|| -status.signal().unwrap_or(0))
            }
            #[cfg(not(unix))]
            {
                status.code().unwrap_or(-1)
            }
        }))
    }

    fn terminate(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pgid.wrapping_neg(), libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            // CTRL-BREAK needs a console group; the Job Object kill below
            // is the real mechanism — founder/CI verified.
            let _ = self.child.kill();
        }
    }

    fn kill(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pgid.wrapping_neg(), libc::SIGKILL);
        }
        let _ = self.child.kill();
    }
}

/// Spawns one process per the spec, wiring §8.4 die-with-parent and §8.6
/// log capture.
pub struct OsSpawner {
    pub spec: SpawnSpec,
}

impl Spawner for OsSpawner {
    fn spawn(&mut self, port: u16) -> std::io::Result<Box<dyn ChildHandle>> {
        let args: Vec<String> = self
            .spec
            .args
            .iter()
            .map(|a| a.replace("{port}", &port.to_string()))
            .collect();
        let mut cmd = Command::new(&self.spec.program);
        cmd.args(&args);
        if self.spec.log.is_some() {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        cmd.stdin(Stdio::null());

        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                // Own process group: group-kill covers any helpers the
                // child forks (§8.4 Linux/macOS).
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Linux: die with the parent even on SIGKILL of the
                // parent. Re-check the parent afterwards: if it already
                // died between fork and prctl, exit now (the classic
                // pdeathsig race).
                #[cfg(target_os = "linux")]
                {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() == 1 {
                        libc::_exit(0);
                    }
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn()?;

        #[cfg(windows)]
        let job = windows_job::Job::assign(&child);

        if let Some(log) = &self.spec.log {
            if let Some(stdout) = child.stdout.take() {
                pipe_to_log(stdout, log.clone(), "out");
            }
            if let Some(stderr) = child.stderr.take() {
                pipe_to_log(stderr, log.clone(), "err");
            }
        }
        let start_time = super::children::process_start_time(child.id());
        #[cfg(unix)]
        let pgid = child.id() as i32;
        Ok(Box::new(OsChild {
            child,
            start_time,
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job,
        }))
    }
}

fn pipe_to_log<R: std::io::Read + Send + 'static>(reader: R, log: RotatingLog, tag: &'static str) {
    std::thread::Builder::new()
        .name(format!("pp-child-log-{tag}"))
        .spawn(move || {
            use std::io::BufRead;
            let buf = std::io::BufReader::new(reader);
            for line in buf.lines() {
                match line {
                    Ok(l) => log.line(&format!("[{tag}] {l}")),
                    Err(_) => break,
                }
            }
        })
        .expect("spawn child log pipe");
}

/// Windows Job Objects (§8.4): `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`,
/// children assigned at spawn; the OS kills them even if the app crashes.
/// **Code-complete behind cfg; founder/CI-verified** — this container
/// cannot compile-check the windows target.
#[cfg(windows)]
mod windows_job {
    use std::process::Child;

    pub struct Job {
        handle: isize,
    }

    // Raw Win32 FFI: kernel32 is always present; no crate dependency this
    // packet cannot verify.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attrs: *mut core::ffi::c_void, name: *const u16) -> isize;
        fn SetInformationJobObject(
            job: isize,
            class: i32,
            info: *const core::ffi::c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Default)]
    struct JobObjectExtendedLimitInformation {
        // JOBOBJECT_BASIC_LIMIT_INFORMATION
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
        // IO_COUNTERS
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    impl Job {
        pub fn assign(child: &Child) -> Option<Job> {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let job = CreateJobObjectW(core::ptr::null_mut(), core::ptr::null());
                if job == 0 {
                    return None;
                }
                let mut info = JobObjectExtendedLimitInformation::default();
                info.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&info as *const JobObjectExtendedLimitInformation).cast(),
                    core::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
                ) == 0
                {
                    CloseHandle(job);
                    return None;
                }
                if AssignProcessToJobObject(job, child.as_raw_handle() as isize) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Job { handle: job })
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                // KILL_ON_JOB_CLOSE: closing the last handle kills the
                // tree.
                CloseHandle(self.handle);
            }
        }
    }
}

// --------------------------------------------------------- real probes ----

/// `GET /health` on a worker thread (P1 — llama-server; §8.1: 503 while
/// loading). Latency is measured at the honest boundary with real time.
pub struct HttpHealthProbe {
    timeout: Duration,
    rx: Option<Receiver<(ProbeOutcome, u64)>>,
}

impl HttpHealthProbe {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout, rx: None }
    }
}

impl HealthProbe for HttpHealthProbe {
    fn begin(&mut self, port: u16, _now_ms: u64) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let timeout = self.timeout;
        std::thread::Builder::new()
            .name("pp-health-http".into())
            .spawn(move || {
                let addr: SocketAddr = ([127, 0, 0, 1], port).into();
                let started = Instant::now();
                let outcome = match photoproof_connectors::http::request(
                    addr,
                    "GET",
                    "/health",
                    &[],
                    None,
                    timeout,
                    timeout,
                ) {
                    Ok((resp, _)) => match resp.status {
                        200 => ProbeOutcome::Ready,
                        503 => ProbeOutcome::Loading,
                        s => ProbeOutcome::Failed(format!("health {s}")),
                    },
                    Err(photoproof_connectors::http::HttpError::Refused) => ProbeOutcome::Refused,
                    Err(photoproof_connectors::http::HttpError::TimedOut) => ProbeOutcome::TimedOut,
                    Err(e) => ProbeOutcome::Failed(e.to_string()),
                };
                let _ = tx.send((outcome, started.elapsed().as_millis() as u64));
            })
            .expect("spawn health probe");
    }

    fn poll(&mut self, _now_ms: u64) -> Option<(ProbeOutcome, u64)> {
        let result = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        Some(result)
    }
}

/// P2 readiness: a WebSocket handshake open/close (§8.1).
pub struct WsHealthProbe {
    timeout: Duration,
    rx: Option<Receiver<(ProbeOutcome, u64)>>,
    seed: u64,
}

impl WsHealthProbe {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            rx: None,
            seed: 0x6865_616c,
        }
    }
}

impl HealthProbe for WsHealthProbe {
    fn begin(&mut self, port: u16, _now_ms: u64) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let timeout = self.timeout;
        self.seed = self.seed.wrapping_add(1);
        let seed = self.seed;
        std::thread::Builder::new()
            .name("pp-health-ws".into())
            .spawn(move || {
                let addr: SocketAddr = ([127, 0, 0, 1], port).into();
                let started = Instant::now();
                let outcome = match WsClient::connect(addr, "/", timeout, timeout, seed) {
                    Ok(mut ws) => {
                        let _ = ws.send_close();
                        ProbeOutcome::Ready
                    }
                    Err(photoproof_connectors::http::HttpError::Refused) => ProbeOutcome::Refused,
                    Err(photoproof_connectors::http::HttpError::TimedOut) => ProbeOutcome::TimedOut,
                    Err(e) => ProbeOutcome::Failed(e.to_string()),
                };
                let _ = tx.send((outcome, started.elapsed().as_millis() as u64));
            })
            .expect("spawn ws health probe");
    }

    fn poll(&mut self, _now_ms: u64) -> Option<(ProbeOutcome, u64)> {
        let result = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        Some(result)
    }
}

/// Convenience mapping from the OpenAI client's health vocabulary.
pub fn outcome_from_health(h: HealthStatus) -> ProbeOutcome {
    match h {
        HealthStatus::Ready => ProbeOutcome::Ready,
        HealthStatus::Loading => ProbeOutcome::Loading,
        HealthStatus::Refused => ProbeOutcome::Refused,
        HealthStatus::TimedOut => ProbeOutcome::TimedOut,
        HealthStatus::Error(e) => ProbeOutcome::Failed(e),
    }
}
