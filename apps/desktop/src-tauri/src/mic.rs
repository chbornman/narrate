//! P6.4 (6b): the real microphone — cpal input device → mono 16 kHz f32
//! frames → the shared capture engine (`push_audio`/`pump`), minted voice
//! events announced exactly like typed commits (pulse + journal-changed).
//!
//! The cpal `Stream` is !Send on macOS (CoreAudio), so the device and the
//! stream live entirely on ONE dedicated thread: the audio callback
//! (CoreAudio's thread) only forwards raw interleaved buffers over a
//! channel; the `pp-mic` thread downmixes, resamples to the transcriber's
//! 16 kHz mono contract, and drives the engine. Audio is never persisted
//! (kernel K10): frames go ring → VAD → wire, all in memory; dropping the
//! stream is what turns the OS mic indicator off — it happens with the
//! armed state, never later (CAPTURE §6.4: closed, not paused-but-open).
//!
//! Lock discipline: this thread takes ONLY the capture mutex (commands
//! take session → capture), so the nested acquisitions cannot deadlock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use photoproof_connectors::transcriber::AudioFrame;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::indicator;
use crate::dto::{IndicatorPulse, JournalChanged};
use crate::managed_tasks::{SpawnTaskError, TaskPriority};
use crate::state::App;

/// The transcriber's input contract (Nemotron: 16 kHz mono f32).
const TARGET_RATE: u32 = 16_000;
/// Poll cadence for the callback→thread channel; also bounds how fast the
/// stop flag is observed.
const RECV_TICK: Duration = Duration::from_millis(50);
/// Device/config/stream construction normally settles in milliseconds. The
/// command must not wait forever while presenting a false armed state.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Tick of the `pp-mic-drain` thread that pumps trailing finals after a
/// user disarm: fast enough that a trailing final lands promptly, slow
/// enough not to thrash the capture mutex the commands also take — the
/// loop is bounded overall by the engine's 5 s drain window.
const DISARM_DRAIN_TICK: Duration = Duration::from_millis(150);
static NEXT_MIC_WORKER_ID: AtomicU64 = AtomicU64::new(1);

/// Runtime-generic form of the command layer's voice-event announcement.
/// Keeping it here lets mock-runtime lifecycle tests exercise the real mic
/// ownership path while preserving the exact production event payloads.
pub(crate) fn announce_events<R: tauri::Runtime>(
    handle: &AppHandle<R>,
    events: &[photoproof_core::Event],
) {
    if events.is_empty() {
        return;
    }
    for _ in events {
        let _ = handle.emit(
            "indicator-pulse",
            IndicatorPulse {
                event_kind: "remark",
            },
        );
    }
    let mut touched: Vec<String> = events
        .iter()
        .flat_map(|event| event.targets.iter().map(|hash| hash.as_str().to_owned()))
        .collect();
    touched.sort();
    touched.dedup();
    if !touched.is_empty() {
        let _ = handle.emit("journal-changed", JournalChanged { hashes: touched });
    }
}

/// The running mic thread, present in `App.mic` exactly while armed.
/// Dropping it stops and joins the thread (and with it the cpal stream).
pub struct MicHandle {
    id: u64,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    phase: Arc<AtomicU8>,
}

impl Drop for MicHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take()
            && t.thread().id() != std::thread::current().id()
        {
            let _ = t.join();
        }
        self.phase
            .store(WorkerPhase::Finished as u8, Ordering::Release);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum WorkerPhase {
    Initializing = 0,
    Active = 1,
    Finished = 2,
}

enum InitAck {
    Ready,
    Failed(String),
}

impl MicHandle {
    /// "Active" means initialization acknowledged and the owner thread has
    /// not exited. A finished JoinHandle must never count as a live mic.
    pub fn is_active(&self) -> bool {
        self.phase.load(Ordering::Acquire) == WorkerPhase::Active as u8
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }

    #[cfg(test)]
    pub(crate) fn active_test_handle() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        });
        Self {
            id: NEXT_MIC_WORKER_ID.fetch_add(1, Ordering::Relaxed),
            stop,
            thread: Some(thread),
            phase: Arc::new(AtomicU8::new(WorkerPhase::Active as u8)),
        }
    }

    #[cfg(test)]
    pub(crate) fn finished_test_handle() -> Self {
        let thread = std::thread::spawn(|| {});
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(WorkerPhase::Finished as u8));
        Self {
            id: NEXT_MIC_WORKER_ID.fetch_add(1, Ordering::Relaxed),
            stop,
            thread: Some(thread),
            phase,
        }
    }
}

pub fn start<R: tauri::Runtime>(handle: AppHandle<R>) -> std::io::Result<MicHandle> {
    let shutdown = handle
        .try_state::<Arc<App>>()
        .map(|state| Arc::clone(&state.shutdown))
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let cleanup_handle = handle.clone();
    start_worker(shutdown, move |id, stop, init| {
        run(&handle, stop, init);
        remove_finished_handle(&cleanup_handle, id);
    })
}

fn start_worker(
    shutdown: Arc<AtomicBool>,
    runner: impl FnOnce(u64, &AtomicBool, std::sync::mpsc::Sender<InitAck>) + Send + 'static,
) -> std::io::Result<MicHandle> {
    let id = NEXT_MIC_WORKER_ID.fetch_add(1, Ordering::Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let phase = Arc::new(AtomicU8::new(WorkerPhase::Initializing as u8));
    let thread_phase = Arc::clone(&phase);
    let (init_tx, init_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("pp-mic".into())
        .spawn(move || {
            runner(id, &thread_stop, init_tx);
            thread_phase.store(WorkerPhase::Finished as u8, Ordering::Release);
        })?;
    let worker = MicHandle {
        id,
        stop,
        thread: Some(thread),
        phase,
    };
    let deadline = Instant::now() + INIT_TIMEOUT;
    loop {
        if shutdown.load(Ordering::Acquire) {
            worker.stop.store(true, Ordering::Release);
            drop(worker);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "application stopped during microphone initialization",
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            worker.stop.store(true, Ordering::Release);
            drop(worker);
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "microphone initialization timed out",
            ));
        }
        match init_rx.recv_timeout(RECV_TICK.min(deadline.saturating_duration_since(now))) {
            Ok(InitAck::Ready) => {
                if shutdown.load(Ordering::Acquire) {
                    worker.stop.store(true, Ordering::Release);
                    drop(worker);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "application stopped during microphone initialization",
                    ));
                }
                worker
                    .phase
                    .store(WorkerPhase::Active as u8, Ordering::Release);
                if worker.is_active() {
                    return Ok(worker);
                }
                drop(worker);
                return Err(std::io::Error::other(
                    "microphone worker exited during initialization",
                ));
            }
            Ok(InitAck::Failed(error)) => {
                worker.stop.store(true, Ordering::Release);
                drop(worker);
                return Err(std::io::Error::other(error));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                drop(worker);
                return Err(std::io::Error::other(
                    "microphone worker exited before initialization acknowledgement",
                ));
            }
        }
    }
}

fn remove_finished_handle<R: tauri::Runtime>(handle: &AppHandle<R>, id: u64) {
    let Some(app) = handle
        .try_state::<Arc<App>>()
        .map(|state| state.inner().clone())
    else {
        return;
    };
    let finished = {
        let mut current = app.mic.lock().expect("mic mutex");
        if current.as_ref().is_some_and(|worker| worker.id == id) {
            current.take()
        } else {
            None
        }
    };
    // This runs on the mic thread. MicHandle::drop detects its own JoinHandle
    // and detaches that already-finishing handle instead of self-joining.
    drop(finished);
}

/// After a user disarm, trailing finals (their onsets predate the toggle,
/// CAPTURE §6.4) are still due for up to the engine's 5 s drain window —
/// but the mic thread is gone, so no audio frames arrive to drive `pump`.
/// This short thread is the drain driver: it pumps until the engine
/// closes the pipeline (the mid-session sibling of the quit path's
/// `pump::drain_capture_at_quit`), announcing whatever mints.
pub fn spawn_disarm_drain<R: tauri::Runtime>(handle: AppHandle<R>) -> Result<(), SpawnTaskError> {
    let Some(app) = handle.try_state::<Arc<App>>().map(|s| s.inner().clone()) else {
        return Ok(());
    };
    let tasks = Arc::clone(&app.tasks);
    let generation = app
        .mic_drain_generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    tasks.spawn(
        "capture",
        format!("post-disarm-drain-{generation}"),
        TaskPriority::Background,
        move |task| {
            loop {
                if task.wait_for_cancel(DISARM_DRAIN_TICK) {
                    return Ok(());
                }
                if app.mic_drain_generation.load(Ordering::Acquire) != generation {
                    return Ok(());
                }
                let (events, open) = {
                    let mut capture = app.capture.lock().expect("capture mutex");
                    let Some(engine) = capture.as_mut() else {
                        return Ok(());
                    };
                    if engine.mic().is_armed() {
                        return Ok(()); // re-armed: push_audio drives the pump again
                    }
                    (engine.pump(&app.store), engine.stream_open())
                };
                announce_events(&handle, &events);
                if !events.is_empty() || !open {
                    let _ = handle.emit("indicator-state", indicator(&app));
                }
                if !open {
                    return Ok(()); // drained or deadline-abandoned (§6.4, ≤ 5 s)
                }
            }
        },
    )
}

/// Device failure after a successful arm (§6.6 STREAM failure): the mic
/// cannot stay armed without audio — disarm through the engine (trailing
/// finals still mint) and tell the indicator.
fn disarm_on_device_failure<R: tauri::Runtime>(handle: &AppHandle<R>, app: &App, why: &str) {
    tracing::warn!(reason = %why, "mic device failure, disarming");
    let (events, draining) = {
        let mut capture = app.capture.lock().expect("capture mutex");
        match capture.as_mut() {
            Some(engine) => {
                let events = engine.disarm(&app.store);
                (events, engine.stream_open())
            }
            None => (Vec::new(), false),
        }
    };
    app.runtime.capture_live.store(false, Ordering::Relaxed);
    announce_events(handle, &events);
    if draining && let Err(error) = spawn_disarm_drain(handle.clone()) {
        tracing::error!(
            error = %error,
            "failed to start managed drain after microphone device failure"
        );
    }
    let _ = handle.emit("indicator-state", indicator(app));
}

fn run<R: tauri::Runtime>(
    handle: &AppHandle<R>,
    stop: &AtomicBool,
    init: std::sync::mpsc::Sender<InitAck>,
) {
    // The command that armed us manages app state; it exists by now.
    let Some(app) = handle.try_state::<Arc<App>>().map(|s| s.inner().clone()) else {
        let _ = init.send(InitAck::Failed("application state unavailable".into()));
        return;
    };
    let Some(device) = cpal::default_host().default_input_device() else {
        let _ = init.send(InitAck::Failed("no default input device".into()));
        return;
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = init.send(InitAck::Failed(format!("input config: {e}")));
            return;
        }
    };
    let channels = usize::from(config.channels());
    let device_rate = config.sample_rate();

    // CoreAudio's callback thread only forwards; sample-format conversion
    // to f32 happens in the callback (cheap), everything else here.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let cb_err = Arc::new(AtomicBool::new(false));
    let err_flag = Arc::clone(&cb_err);
    let on_err = move |e: cpal::Error| {
        tracing::error!(error = %e, "mic stream error");
        err_flag.store(true, Ordering::Relaxed);
    };
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &_| {
                let _ = tx.send(data.to_vec());
            },
            on_err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &_| {
                let _ = tx.send(data.iter().map(|s| f32::from(*s) / 32768.0).collect());
            },
            on_err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config.into(),
            move |data: &[u16], _: &_| {
                let _ = tx.send(
                    data.iter()
                        .map(|s| (f32::from(*s) - 32768.0) / 32768.0)
                        .collect(),
                );
            },
            on_err,
            None,
        ),
        other => {
            let _ = init.send(InitAck::Failed(format!(
                "unsupported sample format {other}"
            )));
            return;
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = init.send(InitAck::Failed(format!("build stream: {e}")));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = init.send(InitAck::Failed(format!("play: {e}")));
        return;
    }
    if stop.load(Ordering::Acquire) {
        return;
    }
    if init.send(InitAck::Ready).is_err() {
        return;
    }

    let mut resampler = Resampler::new(device_rate, TARGET_RATE);
    // Stream-clock position of the NEXT outgoing sample; the engine anchors
    // its mono-clock conversion off the first frame's `captured_at`.
    let mut out_samples: u64 = 0;
    let mut last_mic = "";
    while !stop.load(Ordering::Acquire) {
        let first = match rx.recv_timeout(RECV_TICK) {
            Ok(buf) => buf,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cb_err.load(Ordering::Relaxed) {
                    disarm_on_device_failure(handle, &app, "stream error callback");
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                disarm_on_device_failure(handle, &app, "input callback disconnected");
                return;
            }
        };
        // Coalesce the burst (callbacks deliver whole frames, so the
        // concatenation stays channel-aligned).
        let mut interleaved = first;
        while let Ok(more) = rx.try_recv() {
            interleaved.extend_from_slice(&more);
        }
        let mono = downmix(&interleaved, channels);
        let samples = resampler.feed(&mono);
        if samples.is_empty() {
            continue;
        }
        let captured_at = out_samples * 1000 / u64::from(TARGET_RATE);
        out_samples += samples.len() as u64;

        let (events, mic) = {
            let mut capture = app.capture.lock().expect("capture mutex");
            let Some(engine) = capture.as_mut() else {
                return;
            };
            if !engine.mic().is_armed() {
                // Disarmed elsewhere (toggle, session close, fatal ASR
                // error): the stream drops with this thread.
                return;
            }
            (
                engine.push_audio(
                    &app.store,
                    AudioFrame {
                        samples,
                        captured_at,
                    },
                ),
                engine.mic().as_str(),
            )
        };
        announce_events(handle, &events);
        // armedIdle ↔ armedSpeaking transitions (and the fatal auto-disarm)
        // render off this; speech itself is far slower than the frame rate,
        // so the change-gate keeps the channel low-rate (UI §7.4).
        if mic != last_mic || !events.is_empty() {
            last_mic = mic;
            let _ = handle.emit("indicator-state", indicator(&app));
        }
        if mic == "disarmedError" {
            return; // engine tore the pipeline down (§6.6); stream drops
        }
    }
    // `stream` drops here: capture stops, the OS mic indicator turns off.
}

/// Interleaved frames → mono by channel average.
fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Streaming linear-interpolation resampler. Quality is ample for ASR
/// (the spike fed the model the same way); the point is correctness of
/// the stream clock: output length is exactly `in_len * out_rate/in_rate`
/// over time, with fractional position carried across feeds.
struct Resampler {
    /// Input samples advanced per output sample.
    step: f64,
    /// Fractional read position into `buf`.
    pos: f64,
    buf: Vec<f32>,
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: f64::from(in_rate) / f64::from(out_rate),
            pos: 0.0,
            buf: Vec::new(),
        }
    }

    fn feed(&mut self, input: &[f32]) -> Vec<f32> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        // Interpolation needs pos+1 in range; the last sample carries over.
        while (self.pos as usize) + 1 < self.buf.len() {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos -= consumed as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tauri::Manager;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[0.5, -0.5, 1.0, 0.0], 2), vec![0.0, 0.5]);
        assert_eq!(downmix(&[0.25, 0.75], 1), vec![0.25, 0.75]);
    }

    /// Identity rate passes samples through unchanged (the common case on
    /// devices that natively offer 16 kHz).
    #[test]
    fn resampler_is_identity_at_equal_rates() {
        let mut r = Resampler::new(16_000, 16_000);
        let input: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let out = r.feed(&input);
        // One sample carries over for interpolation; everything emitted
        // matches the input exactly.
        assert_eq!(out.len(), 99);
        assert_eq!(out, input[..99].to_vec());
    }

    /// 48 kHz → 16 kHz over many feeds: total output is in_len/3 within
    /// the one-sample carry, and the stream clock therefore never drifts.
    #[test]
    fn resampler_holds_the_rate_ratio_across_chunked_feeds() {
        let mut r = Resampler::new(48_000, 16_000);
        let mut total_in = 0usize;
        let mut total_out = 0usize;
        for chunk_len in [480usize, 444, 512, 480, 1000, 7] {
            let chunk: Vec<f32> = (0..chunk_len).map(|i| (i as f32 * 0.01).sin()).collect();
            total_in += chunk_len;
            total_out += r.feed(&chunk).len();
        }
        let expected = total_in / 3;
        assert!(
            total_out.abs_diff(expected) <= 1,
            "out {total_out} vs expected {expected}"
        );
    }

    /// A constant signal resamples to the same constant (no interpolation
    /// artifacts at chunk boundaries).
    #[test]
    fn resampler_preserves_dc() {
        let mut r = Resampler::new(44_100, 16_000);
        for _ in 0..10 {
            for s in r.feed(&vec![0.25f32; 441]) {
                assert!((s - 0.25).abs() < 1e-6);
            }
        }
    }

    fn injected_init_failure(message: &'static str) -> std::io::Error {
        start_worker(Arc::new(AtomicBool::new(false)), move |_id, _stop, init| {
            init.send(InitAck::Failed(message.into())).unwrap();
        })
        .err()
        .expect("injected initialization must fail")
    }

    #[test]
    fn missing_input_device_never_acknowledges_an_active_handle() {
        let error = injected_init_failure("no default input device");
        assert!(error.to_string().contains("no default input device"));
    }

    #[test]
    fn stream_initialization_failure_never_acknowledges_an_active_handle() {
        let error = injected_init_failure("build stream: injected backend failure");
        assert!(error.to_string().contains("build stream"));
    }

    #[test]
    fn successful_start_waits_for_device_and_stream_acknowledgement() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = start_worker(shutdown, move |_id, stop, init| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                init.send(InitAck::Ready).unwrap();
                while !stop.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            });
            result_tx.send(result).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            result_rx.try_recv().is_err(),
            "start cannot acknowledge before stream.play readiness"
        );
        release_tx.send(()).unwrap();
        let worker = result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(worker.is_active());
        drop(worker);
    }

    #[test]
    fn shutdown_during_initialization_cancels_and_joins_the_worker() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = start_worker(worker_shutdown, move |_id, stop, _init| {
                entered_tx.send(()).unwrap();
                while !stop.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            });
            result_tx.send(result.map(drop)).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        shutdown.store(true, Ordering::Release);
        let error = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("initialization shutdown joined")
            .expect_err("quit must cancel initialization");
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_finished_join_handle_is_never_reported_active() {
        let (finish_tx, finish_rx) = mpsc::channel();
        let worker = start_worker(Arc::new(AtomicBool::new(false)), move |_id, _stop, init| {
            init.send(InitAck::Ready).unwrap();
            finish_rx.recv().unwrap();
        })
        .unwrap();
        assert!(worker.is_active());
        finish_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while worker.is_active() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!worker.is_active());
        drop(worker);
    }

    #[test]
    fn terminal_cleanup_is_generation_safe_and_removes_only_its_handle() {
        let dir = tempfile::tempdir().unwrap();
        let tauri_app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let app = Arc::new(App::init(dir.path().join("appdata")).unwrap());
        tauri_app.manage(Arc::clone(&app));

        let old = MicHandle::active_test_handle();
        let old_id = old.id;
        let replacement = MicHandle::active_test_handle();
        let replacement_id = replacement.id;
        *app.mic.lock().expect("mic mutex") = Some(replacement);

        remove_finished_handle(tauri_app.handle(), old_id);
        assert_eq!(
            app.mic
                .lock()
                .expect("mic mutex")
                .as_ref()
                .map(|worker| worker.id),
            Some(replacement_id),
            "a stale worker exit cannot clear a newer armed generation"
        );
        remove_finished_handle(tauri_app.handle(), replacement_id);
        assert!(app.mic.lock().expect("mic mutex").is_none());
        drop(old);
        assert!(app.tasks.shutdown(Duration::from_secs(1)).acknowledged);
    }

    #[test]
    fn post_disarm_drain_is_managed_and_joins_at_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let tauri_app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let app = Arc::new(App::init(dir.path().join("appdata")).unwrap());
        tauri_app.manage(Arc::clone(&app));

        spawn_disarm_drain(tauri_app.handle().clone()).unwrap();
        assert!(
            app.tasks.snapshots().iter().any(|task| {
                task.owner == "capture"
                    && task.key.starts_with("post-disarm-drain-")
                    && task.state == crate::managed_tasks::TaskState::Running
            }),
            "the trailing-final writer is visible to the process shutdown barrier"
        );

        let report = app.tasks.shutdown(Duration::from_secs(1));
        assert!(report.acknowledged);
        let snapshot = app
            .tasks
            .snapshots()
            .into_iter()
            .find(|task| task.owner == "capture" && task.key.starts_with("post-disarm-drain-"))
            .expect("managed task terminal history");
        assert_eq!(snapshot.state, crate::managed_tasks::TaskState::Cancelled);
    }
}
