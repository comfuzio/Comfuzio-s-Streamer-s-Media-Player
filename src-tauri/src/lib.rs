//! Comfuzio Streamer's Media Player — backend core.

mod audio;
mod chat;
mod commands;
mod error;
mod events;
mod model;
mod overlay;
mod persistence;
mod player;
mod resolver;
mod state;
mod util;

use tauri::{Manager, RunEvent};
use tracing_subscriber::EnvFilter;

pub fn run() {
    // Must run before any other thread exists (mutates the environment on Linux).
    audio::identity::configure_process_audio_identity();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("COMFUZIO_LOG").unwrap_or_else(|_| EnvFilter::new("info,comfuzio_player_lib=debug")),
        )
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(state::AppState::initialize(&handle))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::player_snapshot,
            commands::player_play,
            commands::player_pause,
            commands::player_stop,
            commands::player_skip,
            commands::player_seek,
            commands::player_set_volume,
            commands::queue_get,
            commands::queue_summary,
            commands::queue_request,
            commands::queue_remove,
            commands::queue_move,
            commands::queue_clear,
            commands::settings_get,
            commands::settings_update,
            commands::settings_set_max_length,
            commands::blocklist_get,
            commands::blocklist_add,
            commands::blocklist_remove,
            commands::audio_devices,
            commands::spotify_status,
            commands::spotify_login,
            commands::spotify_logout,
            commands::ytdlp_version,
            commands::chat_statuses,
            commands::chat_reconnect,
            commands::twitch_login,
            commands::twitch_logout,
            commands::youtube_login,
            commands::youtube_logout,
            commands::youtube_quota,
            commands::overlay_info,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<state::AppState>() {
                state.player.shutdown();
            }
        }
    });
}
