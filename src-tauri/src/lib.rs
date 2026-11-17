//! Comfuzio Streamer's Media Player — backend core.

mod audio;
mod chat;
mod commands;
mod error;
mod events;
mod library;
mod model;
mod overlay;
mod persistence;
mod player;
mod playlists;
mod resolver;
mod state;
mod tray;
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
        // Must be the first plugin: a second launch just focuses the running app.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| tray::show_dashboard(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(state::AppState::initialize(&handle))?;
            let start_in_tray = state.settings.get().start_in_tray;
            let events = state.bus.subscribe();
            app.manage(state);
            tray::build(&handle)?;
            tauri::async_runtime::spawn(tray::tooltip_loop(handle.clone(), events));
            if !start_in_tray {
                tray::show_dashboard(&handle);
            }
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
            commands::library_status,
            commands::library_scan,
            commands::library_search,
            commands::library_enqueue,
            commands::playlists_list,
            commands::playlist_get,
            commands::playlist_create,
            commands::playlist_rename,
            commands::playlist_delete,
            commands::playlist_add_library,
            commands::playlist_add_link,
            commands::playlist_add_queue,
            commands::playlist_remove_item,
            commands::playlist_move_item,
            commands::playlist_enqueue,
            commands::playlist_import_m3u,
            commands::playlist_import_url,
            commands::playlist_sync,
            commands::playlist_export_m3u,
            commands::playlist_export_youtube,
            commands::playlist_export_spotify,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Tauri application");

    app.run(|handle, event| match event {
        // Closing the last window: keep running in the tray (the window and its
        // webview are destroyed, freeing their memory). Tray "Quit" exits with a code.
        RunEvent::ExitRequested { code: None, api, .. } => {
            let keep = handle.try_state::<state::AppState>().is_some_and(|s| s.settings.get().close_to_tray);
            if keep {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => {
            if let Some(state) = handle.try_state::<state::AppState>() {
                state.player.shutdown();
            }
        }
        _ => {}
    });
}
