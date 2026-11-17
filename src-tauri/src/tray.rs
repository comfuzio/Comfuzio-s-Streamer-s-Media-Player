//! System tray and dashboard window lifecycle.
//!
//! The dashboard is a WebView2/WebKit window that costs ~180 MB, while the
//! player core (audio, chat bots, overlay server) needs ~10 MB. Closing the
//! window *destroys* the webview instead of hiding it, so a streamer can run
//! the app in the tray during a stream at a fraction of the memory. The tray
//! icon (or launching the app again) recreates the dashboard on demand.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewWindowBuilder};
use tokio::sync::broadcast;

use crate::events::AppEvent;
use crate::model::PlayerStatus;
use crate::player::PlaybackController;
use crate::state::AppState;

pub const MAIN_WINDOW: &str = "main";
const TRAY_ID: &str = "main-tray";
const APP_NAME: &str = "Comfuzio Streamer's Media Player";

/// Focuses the dashboard, recreating it from the window config if it was closed.
pub fn show_dashboard(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let Some(config) = app.config().app.windows.iter().find(|w| w.label == MAIN_WINDOW).cloned() else {
        tracing::error!("no `main` window in tauri.conf.json");
        return;
    };
    match WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()) {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(err) => tracing::error!(%err, "could not open the dashboard"),
    }
}

fn player(app: &AppHandle) -> Option<Arc<PlaybackController>> {
    app.try_state::<AppState>().map(|s| s.player.clone())
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open dashboard", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Play / Pause", true, None::<&str>)?;
    let skip = MenuItem::with_id(app, "skip", "Skip song", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &PredefinedMenuItem::separator(app)?, &toggle, &skip, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(APP_NAME)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_dashboard(app),
            "toggle" => {
                if let Some(p) = player(app) {
                    tauri::async_runtime::spawn(async move {
                        p.toggle().await;
                    });
                }
            }
            "skip" => {
                if let Some(p) = player(app) {
                    tauri::async_runtime::spawn(async move {
                        p.skip().await;
                    });
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_dashboard(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Keeps the tray tooltip showing the current song.
pub async fn tooltip_loop(app: AppHandle, mut rx: broadcast::Receiver<AppEvent>) {
    let mut last = String::new();
    loop {
        let snap = match rx.recv().await {
            Ok(AppEvent::Player(p)) => p,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let text = match (&snap.current, snap.status) {
            (Some(item), PlayerStatus::Playing | PlayerStatus::Loading) => format!("▶ {}", item.track.display_name()),
            (Some(item), PlayerStatus::Paused) => format!("⏸ {}", item.track.display_name()),
            (_, PlayerStatus::Idle) => "Waiting for requests".to_owned(),
            _ => "Stopped".to_owned(),
        };
        // Windows caps tray tooltips at 127 characters.
        let text: String = format!("{APP_NAME}\n{text}").chars().take(127).collect();
        if text != last {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_tooltip(Some(&text));
            }
            last = text;
        }
    }
}
