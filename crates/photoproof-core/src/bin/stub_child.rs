//! The scriptable stub child (test fixture for spec/RUNTIME.md §8 / §13).
//!
//! Stands in for llama-server/sherpa in every supervision test that needs
//! a REAL process at the honest OS boundary (spawn, signals, ports,
//! waitpid). Behaviors are scripted via flags:
//!
//! - `--port N`            bind 127.0.0.1:N (bind failure ⇒ exit 2 — the
//!   §8.2 double-bind/spawn-failure path)
//! - `--load-ms N`         /health returns 503 until N ms after start
//!   (llama's loading window), then 200
//! - `--health-503-count N` first N /health requests get 503, then 200
//! - `--hang-health`       /health accepts and never responds
//! - `--hang-slots`        /slots accepts and never responds (the §8.1
//!   cross-check trap: hangs while /health is green)
//! - `--exit-after-ms N`   exit with `--exit-code C` (default 7) after N
//! - `--ignore-sigterm`    SIGTERM is ignored (forces the SIGKILL path)
//! - `--spawn-supervised <prog> [args…]` act as the PARENT: spawn `prog`
//!   through photoproof-core's §8.4 spawn path, print `GRANDCHILD <pid>`,
//!   then sleep — the pdeathsig test SIGKILLs *this* process and asserts
//!   the grandchild died
//! - `GET /work?ms=N`      hold the request N ms then answer 200 "done"
//!   (Busy-not-Lost and mid-call-kill material)
//!
//! Plain std + libc; HTTP/1.1 just enough for the probes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

struct Opts {
    port: u16,
    load_ms: u64,
    health_503_count: u32,
    hang_health: bool,
    hang_slots: bool,
    exit_after_ms: Option<u64>,
    exit_code: i32,
    ignore_sigterm: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Parent mode for the pdeathsig test.
    if let Some(i) = args.iter().position(|a| a == "--spawn-supervised") {
        spawn_supervised_mode(&args[i + 1..]);
        return;
    }
    let mut opts = Opts {
        port: 0,
        load_ms: 0,
        health_503_count: 0,
        hang_health: false,
        hang_slots: false,
        exit_after_ms: None,
        exit_code: 7,
        ignore_sigterm: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().expect("flag value").clone();
        match a.as_str() {
            "--port" => opts.port = val().parse().expect("port"),
            "--load-ms" => opts.load_ms = val().parse().expect("load-ms"),
            "--health-503-count" => opts.health_503_count = val().parse().expect("count"),
            "--hang-health" => opts.hang_health = true,
            "--hang-slots" => opts.hang_slots = true,
            "--exit-after-ms" => opts.exit_after_ms = Some(val().parse().expect("ms")),
            "--exit-code" => opts.exit_code = val().parse().expect("code"),
            "--ignore-sigterm" => opts.ignore_sigterm = true,
            other => {
                eprintln!("stub-child: unknown flag {other}");
                std::process::exit(64);
            }
        }
    }

    if opts.ignore_sigterm {
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }

    let listener = match TcpListener::bind(("127.0.0.1", opts.port)) {
        Ok(l) => l,
        Err(e) => {
            // §8.2: child bind failure = spawn failure (the supervisor
            // sees the exit and picks a new port).
            eprintln!("stub-child: bind failed: {e}");
            std::process::exit(2);
        }
    };
    println!("stub-child: listening on {}", opts.port);
    listener.set_nonblocking(true).expect("nonblocking");

    let started = Instant::now();
    let health_count = Arc::new(AtomicU32::new(0));
    let exit_code = opts.exit_code;
    let exit_after = opts.exit_after_ms;
    let opts = Arc::new(opts);

    loop {
        if let Some(ms) = exit_after
            && started.elapsed() >= Duration::from_millis(ms)
        {
            println!("stub-child: scripted exit {exit_code}");
            std::process::exit(exit_code);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let opts = opts.clone();
                let health_count = health_count.clone();
                std::thread::spawn(move || serve(stream, &opts, started, &health_count));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
}

fn serve(stream: TcpStream, opts: &Opts, started: Instant, health_count: &AtomicU32) {
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    // Drain headers.
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
    }
    let mut w = stream;
    let respond = |w: &mut TcpStream, status: u16, body: &str| {
        let _ = w.write_all(
            format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    };
    if path.starts_with("/health") {
        if opts.hang_health {
            hold_forever(&w);
            return;
        }
        let n = health_count.fetch_add(1, Ordering::SeqCst);
        let loading_by_time = started.elapsed() < Duration::from_millis(opts.load_ms);
        let loading_by_count = n < opts.health_503_count;
        if loading_by_time || loading_by_count {
            respond(&mut w, 503, "loading");
        } else {
            respond(&mut w, 200, "ok");
        }
    } else if path.starts_with("/slots") {
        if opts.hang_slots {
            hold_forever(&w);
            return;
        }
        respond(&mut w, 200, "[]");
    } else if let Some(q) = path.strip_prefix("/work") {
        let ms: u64 = q
            .strip_prefix("?ms=")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1_000);
        std::thread::sleep(Duration::from_millis(ms));
        respond(&mut w, 200, "done");
    } else {
        respond(&mut w, 404, "");
    }
}

fn hold_forever(stream: &TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buf = [0u8; 1];
    let mut r = stream;
    loop {
        match r.read(&mut buf) {
            Ok(0) => return, // peer closed
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// Parent mode: spawn the grandchild through the SAME §8.4 spawn path the
/// app uses (pdeathsig + process group on Linux), print its pid, sleep.
fn spawn_supervised_mode(rest: &[String]) {
    use photoproof_core::runtime::{SpawnSpec, Spawner};
    let program = rest.first().expect("--spawn-supervised <prog> [args…]");
    let mut spawner = photoproof_core::runtime::OsSpawner {
        spec: SpawnSpec {
            program: program.into(),
            args: rest[1..].to_vec(),
            log: None,
        },
    };
    // Port substitution is unused here; pass a dummy.
    let child = spawner.spawn(0).expect("grandchild spawn");
    println!("GRANDCHILD {}", child.pid());
    // Hold the handle so the grandchild stays parented to us; the test
    // SIGKILLs this process and asserts the grandchild died (PDEATHSIG).
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
