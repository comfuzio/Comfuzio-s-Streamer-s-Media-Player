//! Gives the process a recognizable name in the OS audio graph.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const AUDIO_APP_NAME: &str = "Comfuzio Streamer Player";

/// Must be called at the very start of `main`, before any thread is spawned
/// (environment mutation is not thread-safe).
///
/// On Windows and macOS the audio session is already attributed to our
/// executable/bundle, which is what OBS's per-application capture selects.
/// On Linux, cpal talks to ALSA, which is routed into PipeWire or PulseAudio;
/// these variables name the resulting stream so it can be picked out in
/// OBS ("Application Audio Capture (PipeWire)") or pavucontrol.
pub fn configure_process_audio_identity() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("PULSE_PROP").is_none() {
            std::env::set_var(
                "PULSE_PROP",
                format!("application.name='{AUDIO_APP_NAME}' application.icon_name='comfuzio-player' media.role='music'"),
            );
        }
        if std::env::var_os("PIPEWIRE_PROPS").is_none() {
            std::env::set_var(
                "PIPEWIRE_PROPS",
                format!(
                    "{{ application.name = \"{AUDIO_APP_NAME}\" node.name = \"comfuzio-player\" node.description = \"{AUDIO_APP_NAME}\" media.role = \"Music\" }}"
                ),
            );
        }
    }
}
