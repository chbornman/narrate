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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use photoproof_connectors::transcriber::AudioFrame;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{announce_events, indicator};
use crate::state::App;

/// The transcriber's input contract (Nemotron: 16 kHz mono f32).
const TARGET_RATE: u32 = 16_000;
/// Poll cadence for the callback→thread channel; also bounds how fast the
/// stop flag is observed.
const RECV_TICK: Duration = Duration::from_millis(50);

/// The running mic thread, present in `App.mic` exactly while armed.
/// Dropping it stops and joins the thread (and with it the cpal stream).
pub struct MicHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for MicHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(handle: AppHandle) -> MicHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("pp-mic".into())
        .spawn(move || run(&handle, &thread_stop))
        .ok();
    MicHandle { stop, thread }
}

/// Device failure after a successful arm (§6.6 STREAM failure): the mic
/// cannot stay armed without audio — disarm through the engine (trailing
/// finals still mint) and tell the indicator.
fn disarm_on_device_failure(handle: &AppHandle, app: &App, why: &str) {
    eprintln!("photoproof: mic device failure, disarming: {why}");
    let events = {
        let mut capture = app.capture.lock().expect("capture mutex");
        match capture.as_mut() {
            Some(engine) => engine.disarm(&app.store),
            None => Vec::new(),
        }
    };
    app.runtime.capture_live.store(false, Ordering::Relaxed);
    announce_events(handle, &events);
    let _ = handle.emit("indicator-state", indicator(app));
}

fn run(handle: &AppHandle, stop: &AtomicBool) {
    // The command that armed us manages app state; it exists by now.
    let Some(app) = handle.try_state::<Arc<App>>().map(|s| s.inner().clone()) else {
        return;
    };
    let Some(device) = cpal::default_host().default_input_device() else {
        disarm_on_device_failure(handle, &app, "no default input device");
        return;
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            disarm_on_device_failure(handle, &app, &format!("input config: {e}"));
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
        eprintln!("photoproof: mic stream error: {e}");
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
            disarm_on_device_failure(handle, &app, &format!("unsupported sample format {other}"));
            return;
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            disarm_on_device_failure(handle, &app, &format!("build stream: {e}"));
            return;
        }
    };
    if let Err(e) = stream.play() {
        disarm_on_device_failure(handle, &app, &format!("play: {e}"));
        return;
    }

    let mut resampler = Resampler::new(device_rate, TARGET_RATE);
    // Stream-clock position of the NEXT outgoing sample; the engine anchors
    // its mono-clock conversion off the first frame's `captured_at`.
    let mut out_samples: u64 = 0;
    let mut last_mic = "";
    while !stop.load(Ordering::Relaxed) {
        let first = match rx.recv_timeout(RECV_TICK) {
            Ok(buf) => buf,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cb_err.load(Ordering::Relaxed) {
                    disarm_on_device_failure(handle, &app, "stream error callback");
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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
}
