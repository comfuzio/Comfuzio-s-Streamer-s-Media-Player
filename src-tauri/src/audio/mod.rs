//! Native audio pipeline.
//!
//! Audio is rendered by this process itself (cpal → WASAPI / CoreAudio / ALSA→PipeWire),
//! never by the webview. That gives OBS one clean, identifiable audio session:
//! - Windows: "Application Audio Capture" → `comfuzio-player.exe`
//! - macOS 13+: "macOS Audio Capture" → the app bundle
//! - Linux: PipeWire/PulseAudio stream named "Comfuzio Streamer Player"
//!
//! Pipeline: yt-dlp download → [`cache`] → [`decoder`] thread (Symphonia + rubato)
//! → lock-free ring buffer → cpal output callback ([`engine`]).

pub mod cache;
pub mod decoder;
pub mod device;
pub mod engine;
pub mod identity;

pub use engine::{AudioEngine, EngineEvent};

/// Perceptual volume curve: 0..=100 → linear gain (squared for a natural feel).
pub fn volume_to_gain(volume: u8) -> f32 {
    let x = f32::from(volume.min(100)) / 100.0;
    x * x
}
