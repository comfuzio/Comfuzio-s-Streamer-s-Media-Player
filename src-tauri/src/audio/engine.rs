//! Audio engine thread: owns the cpal output stream (which is `!Send` on some
//! platforms) and the per-track decoder thread. Controlled through a cheap,
//! cloneable [`AudioEngine`] handle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::{HeapCons, HeapRb};
use tokio::sync::mpsc::UnboundedSender;

use super::decoder::{run_decoder, DecodeJob, SessionState};
use super::device::open_output_device;
use super::volume_to_gain;
use crate::error::{AppError, AppResult};

/// Seconds of decoded audio buffered ahead of the output callback.
const RING_SECONDS: usize = 2;
const MAX_STREAM_RECOVERIES: u32 = 3;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// The track for `generation` played to the end.
    Ended { generation: u64 },
    /// The track for `generation` could not be (or stopped being) played.
    Failed { generation: u64, message: String },
}

enum Command {
    Load { path: PathBuf, generation: u64, start_ms: u64 },
    Seek { position_ms: u64 },
    Stop,
    SetDevice(Option<String>),
    Shutdown,
}

/// State readable from any thread without going through the engine thread.
struct Shared {
    volume: AtomicU8,
    paused: AtomicBool,
    active: AtomicBool,
    frames_played: AtomicU64,
    out_rate: AtomicU32,
    base_ms: AtomicU64,
}

#[derive(Clone)]
pub struct AudioEngine {
    tx: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    thread: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl AudioEngine {
    pub fn spawn(events: UnboundedSender<EngineEvent>, volume: u8, device: Option<String>) -> AppResult<Self> {
        let shared = Arc::new(Shared {
            volume: AtomicU8::new(volume.min(100)),
            paused: AtomicBool::new(false),
            active: AtomicBool::new(false),
            frames_played: AtomicU64::new(0),
            out_rate: AtomicU32::new(48_000),
            base_ms: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let handle = std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || EngineThread { shared: thread_shared, events, device, session: None }.run(rx))
            .map_err(|e| AppError::Audio(format!("failed to start audio thread: {e}")))?;
        Ok(Self { tx, shared, thread: Arc::new(std::sync::Mutex::new(Some(handle))) })
    }

    fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            tracing::error!("audio engine thread is gone");
        }
    }

    /// Starts playing `path` from `start_ms`. `paused` holds it on the first frame.
    pub fn load(&self, path: PathBuf, generation: u64, start_ms: u64, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
        self.reset_position(start_ms);
        self.send(Command::Load { path, generation, start_ms });
    }

    pub fn seek(&self, position_ms: u64) {
        self.reset_position(position_ms);
        self.send(Command::Seek { position_ms });
    }

    pub fn stop(&self) {
        self.shared.active.store(false, Ordering::Release);
        self.send(Command::Stop);
    }

    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Release);
    }

    pub fn set_volume(&self, volume: u8) {
        self.shared.volume.store(volume.min(100), Ordering::Relaxed);
    }

    pub fn set_device(&self, device: Option<String>) {
        self.send(Command::SetDevice(device));
    }

    /// Playback position of the loaded track.
    pub fn position_ms(&self) -> u64 {
        if !self.shared.active.load(Ordering::Acquire) {
            return 0;
        }
        position_of(&self.shared)
    }

    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
        if let Some(handle) = self.thread.lock().ok().and_then(|mut g| g.take()) {
            let _ = handle.join();
        }
    }

    fn reset_position(&self, ms: u64) {
        self.shared.base_ms.store(ms, Ordering::Release);
        self.shared.frames_played.store(0, Ordering::Release);
        self.shared.active.store(true, Ordering::Release);
    }
}

fn position_of(shared: &Shared) -> u64 {
    let rate = u64::from(shared.out_rate.load(Ordering::Acquire).max(1));
    shared.base_ms.load(Ordering::Acquire) + shared.frames_played.load(Ordering::Acquire) * 1000 / rate
}

/// One loaded track: output stream + decoder thread.
struct Session {
    stream: Option<Stream>,
    decoder: Option<JoinHandle<()>>,
    state: Arc<SessionState>,
    path: PathBuf,
    generation: u64,
    recoveries: u32,
    reported: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        // Stop the callback before joining the producer.
        drop(self.stream.take());
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}

struct EngineThread {
    shared: Arc<Shared>,
    events: UnboundedSender<EngineEvent>,
    device: Option<String>,
    session: Option<Session>,
}

impl EngineThread {
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(Command::Load { path, generation, start_ms }) => {
                    self.session = None;
                    self.open(path, generation, start_ms, 0);
                }
                Ok(Command::Seek { position_ms }) => {
                    if let Some(s) = self.session.take() {
                        let (path, generation, recoveries) = (s.path.clone(), s.generation, s.recoveries);
                        drop(s);
                        self.open(path, generation, position_ms, recoveries);
                    }
                }
                Ok(Command::Stop) => {
                    self.session = None;
                    self.shared.active.store(false, Ordering::Release);
                }
                Ok(Command::SetDevice(device)) => {
                    self.device = device;
                    self.reopen_current(false);
                }
                Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.session = None;
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            self.housekeeping();
        }
    }

    fn open(&mut self, path: PathBuf, generation: u64, start_ms: u64, recoveries: u32) {
        match self.start_session(path, generation, start_ms) {
            Ok(mut s) => {
                s.recoveries = recoveries;
                self.session = Some(s);
            }
            Err(e) => {
                self.shared.active.store(false, Ordering::Release);
                let _ = self.events.send(EngineEvent::Failed { generation, message: e.to_string() });
            }
        }
    }

    /// Rebuilds the current session at its current position (device change or loss).
    fn reopen_current(&mut self, is_recovery: bool) {
        let Some(s) = self.session.take() else { return };
        let position = position_of(&self.shared);
        let (path, generation) = (s.path.clone(), s.generation);
        let recoveries = s.recoveries + u32::from(is_recovery);
        drop(s);
        self.shared.base_ms.store(position, Ordering::Release);
        self.shared.frames_played.store(0, Ordering::Release);
        self.open(path, generation, position, recoveries);
    }

    fn housekeeping(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if let Some(err) = s.state.take_stream_error() {
            let generation = s.generation;
            if s.recoveries >= MAX_STREAM_RECOVERIES {
                self.session = None;
                self.shared.active.store(false, Ordering::Release);
                let _ = self.events.send(EngineEvent::Failed { generation, message: format!("output device error: {err}") });
            } else {
                tracing::warn!(%err, "audio stream error; reopening output");
                self.reopen_current(true);
            }
            return;
        }
        if let Some(err) = s.state.take_decode_error() {
            let generation = s.generation;
            self.session = None;
            self.shared.active.store(false, Ordering::Release);
            let _ = self.events.send(EngineEvent::Failed { generation, message: err });
            return;
        }
        if !s.reported && s.state.ended.load(Ordering::Acquire) {
            s.reported = true;
            let _ = self.events.send(EngineEvent::Ended { generation: s.generation });
        }
    }

    fn start_session(&self, path: PathBuf, generation: u64, start_ms: u64) -> AppResult<Session> {
        let device = open_output_device(self.device.as_deref())?;
        let supported = device.default_output_config().map_err(|e| AppError::Audio(format!("output config: {e}")))?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let out_rate = config.sample_rate.0;
        let channels = usize::from(config.channels.max(1));

        let (prod, cons) = HeapRb::<f32>::new(out_rate as usize * channels * RING_SECONDS).split();
        let state = Arc::new(SessionState::default());

        self.shared.out_rate.store(out_rate, Ordering::Release);
        self.shared.base_ms.store(start_ms, Ordering::Release);
        self.shared.frames_played.store(0, Ordering::Release);
        self.shared.active.store(true, Ordering::Release);

        let decoder_state = state.clone();
        let decoder_path = path.clone();
        let decoder = std::thread::Builder::new()
            .name("audio-decoder".into())
            .spawn(move || {
                let job = DecodeJob { path: &decoder_path, start_ms, out_rate, out_channels: channels };
                run_decoder(job, prod, &decoder_state);
            })
            .map_err(|e| AppError::Audio(format!("failed to start decoder: {e}")))?;

        let mut session = Session {
            stream: None,
            decoder: Some(decoder),
            state: state.clone(),
            path,
            generation,
            recoveries: 0,
            reported: false,
        };

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, cons, &self.shared, &state),
            SampleFormat::F64 => build_stream::<f64>(&device, &config, cons, &self.shared, &state),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, cons, &self.shared, &state),
            SampleFormat::I32 => build_stream::<i32>(&device, &config, cons, &self.shared, &state),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, cons, &self.shared, &state),
            SampleFormat::I8 => build_stream::<i8>(&device, &config, cons, &self.shared, &state),
            SampleFormat::U8 => build_stream::<u8>(&device, &config, cons, &self.shared, &state),
            other => Err(AppError::Audio(format!("unsupported device sample format {other:?}"))),
        }?;
        stream.play().map_err(|e| AppError::Audio(format!("cannot start output: {e}")))?;
        session.stream = Some(stream);
        tracing::info!(out_rate, channels, ?sample_format, "audio session started");
        Ok(session)
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut cons: HeapCons<f32>,
    shared: &Arc<Shared>,
    state: &Arc<SessionState>,
) -> AppResult<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    let shared = shared.clone();
    let cb_state = state.clone();
    let err_state = state.clone();
    let mut scratch = vec![0.0f32; 16_384];
    let mut gain = volume_to_gain(shared.volume.load(Ordering::Relaxed));
    // ~10 ms smoothing time constant at 48 kHz: no zipper noise on volume changes.
    let smoothing = 1.0 / (config.sample_rate.0 as f32 * 0.01);

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let n = data.len();
                if scratch.len() < n {
                    scratch.resize(n, 0.0);
                }
                let buf = &mut scratch[..n];
                let paused = shared.paused.load(Ordering::Acquire);
                let got = if paused {
                    0
                } else {
                    // Only ever take whole frames so channels never drift.
                    let avail = cons.occupied_len().min(n);
                    cons.pop_slice(&mut buf[..avail - avail % channels])
                };
                buf[got..].fill(0.0);

                let target = volume_to_gain(shared.volume.load(Ordering::Relaxed));
                for frame in buf.chunks_exact_mut(channels) {
                    gain += (target - gain) * smoothing;
                    for s in frame {
                        *s = (*s * gain).clamp(-1.0, 1.0);
                    }
                }
                for (out, s) in data.iter_mut().zip(buf.iter()) {
                    *out = T::from_sample(*s);
                }

                if got > 0 {
                    shared.frames_played.fetch_add((got / channels) as u64, Ordering::AcqRel);
                } else if !paused && cb_state.decode_done.load(Ordering::Acquire) && cons.is_empty() {
                    cb_state.ended.store(true, Ordering::Release);
                }
            },
            move |err| err_state.set_stream_error(err.to_string()),
            None,
        )
        .map_err(|e| AppError::Audio(format!("cannot open output stream: {e}")))
}

/// Opt-in: plays ~6 s of `COMFUZIO_TEST_AUDIO` on the default output device.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn plays_pauses_and_seeks_on_real_device() {
        let path = PathBuf::from(std::env::var("COMFUZIO_TEST_AUDIO").expect("set COMFUZIO_TEST_AUDIO"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = AudioEngine::spawn(tx, 30, None).unwrap();
        let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));

        engine.load(path, 1, 0, false);
        sleep(3000);
        let p1 = engine.position_ms();
        println!("after 3s: {p1} ms");
        assert!((2500..=3300).contains(&p1), "position {p1}");

        engine.pause();
        sleep(800);
        let p2 = engine.position_ms();
        sleep(800);
        assert!(engine.position_ms().abs_diff(p2) < 50, "position moved while paused");
        engine.resume();

        engine.seek(200_000); // 13 s before the end
        sleep(1500);
        let p3 = engine.position_ms();
        println!("after seek: {p3} ms");
        assert!((200_500..=202_000).contains(&p3), "position {p3}");

        engine.set_volume(10);
        sleep(1000);
        engine.stop();
        sleep(200);
        assert_eq!(engine.position_ms(), 0);
        assert!(rx.try_recv().is_err(), "unexpected engine event");
        engine.shutdown();
    }

    #[test]
    #[ignore]
    fn reports_end_of_track() {
        let path = PathBuf::from(std::env::var("COMFUZIO_TEST_AUDIO").expect("set COMFUZIO_TEST_AUDIO"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = AudioEngine::spawn(tx, 15, None).unwrap();
        engine.load(path, 7, 210_000, false); // last ~3 s
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(ev) = rx.try_recv() {
                assert!(matches!(ev, EngineEvent::Ended { generation: 7 }), "{ev:?}");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no Ended event");
            std::thread::sleep(Duration::from_millis(50));
        }
        engine.shutdown();
    }
}
