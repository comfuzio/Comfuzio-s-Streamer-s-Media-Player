//! Decoder thread: file → PCM f32 at the device's rate/channel layout → ring buffer.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

const RESAMPLE_CHUNK: usize = 1024;

/// State shared between one playback session's decoder thread, output
/// callback and the engine thread.
#[derive(Default)]
pub struct SessionState {
    pub stop: AtomicBool,
    pub decode_done: AtomicBool,
    pub ended: AtomicBool,
    decode_error: Mutex<Option<String>>,
    stream_error: Mutex<Option<String>>,
}

impl SessionState {
    pub fn set_decode_error(&self, e: String) {
        if let Ok(mut g) = self.decode_error.lock() {
            g.get_or_insert(e);
        }
    }
    pub fn take_decode_error(&self) -> Option<String> {
        self.decode_error.lock().ok().and_then(|mut g| g.take())
    }
    pub fn set_stream_error(&self, e: String) {
        if let Ok(mut g) = self.stream_error.lock() {
            g.get_or_insert(e);
        }
    }
    pub fn take_stream_error(&self) -> Option<String> {
        self.stream_error.lock().ok().and_then(|mut g| g.take())
    }
}

pub struct DecodeJob<'a> {
    pub path: &'a Path,
    pub start_ms: u64,
    pub out_rate: u32,
    pub out_channels: usize,
}

/// Runs to completion (or until `state.stop`); errors are reported through `state`.
pub fn run_decoder(job: DecodeJob<'_>, mut prod: HeapProd<f32>, state: &SessionState) {
    if let Err(e) = decode(&job, &mut prod, state) {
        state.set_decode_error(e);
    }
    state.decode_done.store(true, Ordering::Release);
}

/// Folds any channel layout to stereo (Symphonia orders FL, FR, FC, LFE, RL, RR, ...).
#[inline]
fn to_stereo(frame: &[f32]) -> (f32, f32) {
    match frame.len() {
        0 => (0.0, 0.0),
        1 => (frame[0], frame[0]),
        2 => (frame[0], frame[1]),
        n => {
            let c = frame[2] * 0.707;
            let (sl, sr) = if n >= 6 { (frame[4] * 0.5, frame[5] * 0.5) } else { (0.0, 0.0) };
            (frame[0] + c + sl, frame[1] + c + sr)
        }
    }
}

/// Pushes all of `data`, waiting for space; returns false if asked to stop.
fn push_all(prod: &mut HeapProd<f32>, mut data: &[f32], stop: &AtomicBool) -> bool {
    while !data.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let n = prod.push_slice(data);
        data = &data[n..];
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    true
}

/// Converts stereo planar → device layout (interleaved) and pushes it.
struct Sink<'a> {
    prod: &'a mut HeapProd<f32>,
    stop: &'a AtomicBool,
    out_channels: usize,
    /// Resampler latency (frames) still to drop from the start of the output.
    trim_frames: usize,
    scratch: Vec<f32>,
}

impl Sink<'_> {
    fn write(&mut self, left: &[f32], right: &[f32]) -> bool {
        let skip = self.trim_frames.min(left.len());
        self.trim_frames -= skip;
        self.scratch.clear();
        for (l, r) in left[skip..].iter().zip(&right[skip..]) {
            match self.out_channels {
                1 => self.scratch.push((l + r) * 0.5),
                n => {
                    self.scratch.push(*l);
                    self.scratch.push(*r);
                    self.scratch.extend(std::iter::repeat_n(0.0, n - 2));
                }
            }
        }
        push_all(self.prod, &self.scratch, self.stop)
    }
}

fn decode(job: &DecodeJob<'_>, prod: &mut HeapProd<f32>, state: &SessionState) -> Result<(), String> {
    let file = File::open(job.path).map_err(|e| format!("cannot open audio file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = job.path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|e| format!("unsupported audio container: {e}"))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track in file")?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let src_rate = params.sample_rate.ok_or("audio track has no sample rate")?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported audio codec: {e}"))?;

    let mut skip_until_ts = 0u64;
    if job.start_ms > 0 {
        let seeked = format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time { time: Time::from(job.start_ms as f64 / 1000.0), track_id: Some(track_id) },
            )
            .map_err(|e| format!("seek failed: {e}"))?;
        skip_until_ts = seeked.required_ts;
        decoder.reset();
    }

    let mut resampler = if src_rate != job.out_rate {
        Some(
            FftFixedIn::<f32>::new(src_rate as usize, job.out_rate as usize, RESAMPLE_CHUNK, 2, 2)
                .map_err(|e| format!("resampler init failed: {e}"))?,
        )
    } else {
        None
    };
    let mut sink = Sink {
        prod,
        stop: &state.stop,
        out_channels: job.out_channels.max(1),
        trim_frames: resampler.as_ref().map(|r| r.output_delay()).unwrap_or(0),
        scratch: Vec::with_capacity(RESAMPLE_CHUNK * 8),
    };
    let mut pending: [Vec<f32>; 2] = [Vec::with_capacity(RESAMPLE_CHUNK * 4), Vec::with_capacity(RESAMPLE_CHUNK * 4)];
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        if state.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("read error: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::debug!("skipping undecodable packet: {e}");
                continue;
            }
            Err(e) => return Err(format!("decode error: {e}")),
        };
        let spec = *decoded.spec();
        let frames = decoded.frames();
        if frames == 0 {
            continue;
        }
        let buf = match &mut sample_buf {
            Some(b) if b.capacity() >= decoded.capacity() * spec.channels.count() => b,
            slot => slot.insert(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)),
        };
        buf.copy_interleaved_ref(decoded);
        let channels = spec.channels.count().max(1);

        // After an accurate seek, drop frames that precede the target.
        let mut first_frame = 0usize;
        if packet.ts() < skip_until_ts {
            let skip = (skip_until_ts - packet.ts()) as usize;
            if skip >= frames {
                continue;
            }
            first_frame = skip;
        }
        for frame in buf.samples()[first_frame * channels..frames * channels].chunks_exact(channels) {
            let (l, r) = to_stereo(frame);
            pending[0].push(l);
            pending[1].push(r);
        }

        match resampler.as_mut() {
            Some(rs) => {
                while pending[0].len() >= rs.input_frames_next() {
                    let need = rs.input_frames_next();
                    let chunk: Vec<Vec<f32>> = pending.iter_mut().map(|ch| ch.drain(..need).collect()).collect();
                    let out = rs.process(&chunk, None).map_err(|e| format!("resample error: {e}"))?;
                    if !sink.write(&out[0], &out[1]) {
                        return Ok(());
                    }
                }
            }
            None => {
                if !sink.write(&pending[0], &pending[1]) {
                    return Ok(());
                }
                pending[0].clear();
                pending[1].clear();
            }
        }
    }

    // Flush the resampler tail.
    if let Some(rs) = resampler.as_mut() {
        if !pending[0].is_empty() {
            let out = rs.process_partial(Some(&pending[..]), None).map_err(|e| format!("resample error: {e}"))?;
            if !sink.write(&out[0], &out[1]) {
                return Ok(());
            }
        }
        let tail = rs.process_partial(None::<&[Vec<f32>]>, None).map_err(|e| format!("resample error: {e}"))?;
        sink.write(&tail[0], &tail[1]);
    }
    Ok(())
}

/// Opt-in tests against a real file: `COMFUZIO_TEST_AUDIO=path cargo test -- --ignored`.
#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Consumer, Observer, Split};
    use ringbuf::HeapRb;
    use std::sync::Arc;

    struct Decoded {
        frames: u64,
        rms: f64,
        finite: bool,
    }

    fn decode_file(start_ms: u64, out_rate: u32, out_channels: usize) -> Decoded {
        let path = std::env::var("COMFUZIO_TEST_AUDIO").expect("set COMFUZIO_TEST_AUDIO");
        let (prod, mut cons) = HeapRb::<f32>::new(out_rate as usize * out_channels).split();
        let state = Arc::new(SessionState::default());
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            let job = DecodeJob { path: Path::new(&path), start_ms, out_rate, out_channels };
            run_decoder(job, prod, &worker_state);
        });
        let mut buf = vec![0f32; 8192];
        let (mut samples, mut sum_sq, mut finite) = (0u64, 0f64, true);
        loop {
            let n = cons.pop_slice(&mut buf);
            for s in &buf[..n] {
                finite &= s.is_finite();
                sum_sq += f64::from(*s) * f64::from(*s);
            }
            samples += n as u64;
            if n == 0 {
                if state.decode_done.load(Ordering::Acquire) && cons.is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        worker.join().unwrap();
        assert_eq!(state.take_decode_error(), None);
        Decoded { frames: samples / out_channels as u64, rms: (sum_sq / samples.max(1) as f64).sqrt(), finite }
    }

    #[test]
    #[ignore]
    fn decodes_and_resamples_real_file() {
        for (rate, ch) in [(48_000u32, 2usize), (44_100, 2), (96_000, 8)] {
            let full = decode_file(0, rate, ch);
            let secs = full.frames as f64 / f64::from(rate);
            println!("{rate} Hz x{ch}: {secs:.2}s, rms {:.4}", full.rms);
            assert!(full.finite);
            assert!(full.rms > 0.01, "output is silent");
            assert!((secs - 213.0).abs() < 2.0, "unexpected length {secs}");
        }
    }

    #[test]
    #[ignore]
    fn seeks_accurately_in_real_file() {
        let full = decode_file(0, 48_000, 2);
        let from_60 = decode_file(60_000, 48_000, 2);
        let skipped = (full.frames - from_60.frames) as f64 / 48_000.0;
        println!("seek to 60s skipped {skipped:.3}s");
        assert!((skipped - 60.0).abs() < 0.1, "seek landed {skipped}s in");
    }
}
