use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};

use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::model::{PlayerSnapshot, QueueItem};
use crate::persistence::settings::{OverlaySettings, SettingsStore};
use crate::player::PlaybackController;

const OVERLAY_HTML: &str = include_str!("../../overlay/overlay.html");
const OVERLAY_CSS: &str = include_str!("../../overlay/overlay.css");
const OVERLAY_JS: &str = include_str!("../../overlay/overlay.js");

/// Minimal "up next" info for the overlay.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct UpNext {
    title: String,
    artist: String,
    requester: String,
    queued: usize,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
enum OverlayMessage<'a> {
    #[serde(rename_all = "camelCase")]
    Hello { player: &'a PlayerSnapshot, config: &'a OverlaySettings, up_next: Option<&'a UpNext> },
    Player(&'a PlayerSnapshot),
    Config(&'a OverlaySettings),
    UpNext(Option<&'a UpNext>),
}

fn up_next(items: &[QueueItem]) -> Option<UpNext> {
    items.first().map(|i| UpNext {
        title: i.track.title.clone(),
        artist: i.track.artist.clone(),
        requester: i.requester.display_name.clone(),
        queued: items.len(),
    })
}

#[derive(Clone)]
struct Ctx {
    player: Arc<PlaybackController>,
    settings: Arc<SettingsStore>,
    bus: EventBus,
}

struct Running {
    port: u16,
    shutdown: oneshot::Sender<()>,
}

pub struct OverlayServer {
    ctx: Ctx,
    running: Mutex<Option<Running>>,
}

impl OverlayServer {
    pub fn start(player: Arc<PlaybackController>, settings: Arc<SettingsStore>, bus: EventBus) -> Arc<Self> {
        let server = Arc::new(Self { ctx: Ctx { player, settings: settings.clone(), bus: bus.clone() }, running: Mutex::new(None) });
        let port = settings.get().overlay_port;
        let s = server.clone();
        tauri::async_runtime::spawn(async move {
            s.bind(port).await;
            s.watch_port(bus.subscribe()).await;
        });
        server
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/overlay", self.ctx.settings.get().overlay_port)
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().expect("overlay lock").is_some()
    }

    async fn bind(&self, port: u16) {
        if let Some(old) = self.running.lock().expect("overlay lock").take() {
            let _ = old.shutdown.send(());
        }
        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(err) => {
                self.ctx.bus.notice(
                    NoticeLevel::Error,
                    format!("Overlay server couldn't use port {port} ({err}). Pick another port in Settings."),
                );
                return;
            }
        };
        let (tx, rx) = oneshot::channel();
        *self.running.lock().expect("overlay lock") = Some(Running { port, shutdown: tx });
        let app = router(self.ctx.clone(), port);
        tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, app).with_graceful_shutdown(async { let _ = rx.await; }).await;
            if let Err(err) = result {
                tracing::error!(%err, "overlay server stopped");
            }
        });
        tracing::info!(port, "overlay server listening on http://127.0.0.1:{port}/overlay");
    }

    async fn watch_port(&self, mut rx: broadcast::Receiver<AppEvent>) {
        loop {
            match rx.recv().await {
                Ok(AppEvent::Settings(s)) => {
                    let current = self.running.lock().expect("overlay lock").as_ref().map(|r| r.port);
                    if current != Some(s.overlay_port) {
                        self.bind(s.overlay_port).await;
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

fn router(ctx: Ctx, port: u16) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/overlay") }))
        .route("/overlay", get(|| async { asset(OVERLAY_HTML, "text/html; charset=utf-8") }))
        .route("/overlay/overlay.css", get(|| async { asset(OVERLAY_CSS, "text/css; charset=utf-8") }))
        .route("/overlay/overlay.js", get(|| async { asset(OVERLAY_JS, "text/javascript; charset=utf-8") }))
        .route("/overlay/ws", get(ws_upgrade))
        .route("/api/now-playing", get(now_playing))
        .layer(middleware::from_fn(move |req: Request, next: Next| guard(req, next, port)))
        .with_state(ctx)
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        ],
        body,
    )
        .into_response()
}

fn is_local_host(host: &str) -> bool {
    let name = host.rsplit_once(':').map(|(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host }).unwrap_or(host);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

/// Allowed origins: the overlay itself, the dashboard (tauri / Vite dev), OBS (no Origin / "null").
fn is_allowed_origin(origin: &str, port: u16) -> bool {
    if origin == "null" {
        return true;
    }
    let Ok(url) = url::Url::parse(origin) else { return false };
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => match url.host_str() {
            Some("tauri.localhost") => true,
            Some("127.0.0.1" | "localhost" | "[::1]") => url.port() == Some(port) || url.port() == Some(1420),
            _ => false,
        },
        _ => false,
    }
}

async fn guard(req: Request, next: Next, port: u16) -> Response {
    let headers: &HeaderMap = req.headers();
    let host_ok = headers.get(header::HOST).and_then(|h| h.to_str().ok()).is_some_and(is_local_host);
    let origin_ok = headers
        .get(header::ORIGIN)
        .map(|o| o.to_str().ok().is_some_and(|o| is_allowed_origin(o, port)))
        .unwrap_or(true);
    if !host_ok || !origin_ok {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    next.run(req).await
}

async fn now_playing(State(ctx): State<Ctx>) -> Json<PlayerSnapshot> {
    Json(ctx.player.snapshot().await)
}

async fn ws_upgrade(State(ctx): State<Ctx>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| overlay_socket(socket, ctx))
}

async fn send(socket: &mut WebSocket, msg: &OverlayMessage<'_>) -> bool {
    match serde_json::to_string(msg) {
        Ok(json) => socket.send(Message::Text(json.into())).await.is_ok(),
        Err(err) => {
            tracing::error!(%err, "overlay message serialization failed");
            true
        }
    }
}

async fn overlay_socket(mut socket: WebSocket, ctx: Ctx) {
    // Subscribe before taking the snapshot so no change can slip in between.
    let mut events = ctx.bus.subscribe();
    let mut config = ctx.settings.get().overlay;
    let snapshot = ctx.player.snapshot().await;
    let mut next = up_next(&ctx.player.queue().await.items);
    if !send(&mut socket, &OverlayMessage::Hello { player: &snapshot, config: &config, up_next: next.as_ref() }).await {
        return;
    }

    let mut ping = tokio::time::interval(Duration::from_secs(25));
    ping.tick().await;
    loop {
        tokio::select! {
            event = events.recv() => {
                let ok = match event {
                    Ok(AppEvent::Player(p)) => send(&mut socket, &OverlayMessage::Player(&p)).await,
                    Ok(AppEvent::Settings(s)) if s.overlay != config => {
                        config = s.overlay.clone();
                        send(&mut socket, &OverlayMessage::Config(&config)).await
                    }
                    Ok(AppEvent::Queue(q)) => {
                        let n = up_next(&q.items);
                        if n != next {
                            next = n;
                            send(&mut socket, &OverlayMessage::UpNext(next.as_ref())).await
                        } else {
                            true
                        }
                    }
                    Ok(_) => true,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Missed events: resync with a fresh hello.
                        let snapshot = ctx.player.snapshot().await;
                        config = ctx.settings.get().overlay;
                        next = up_next(&ctx.player.queue().await.items);
                        send(&mut socket, &OverlayMessage::Hello { player: &snapshot, config: &config, up_next: next.as_ref() }).await
                    }
                    Err(broadcast::error::RecvError::Closed) => false,
                };
                if !ok {
                    return;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_guard() {
        for ok in ["127.0.0.1:3847", "localhost:3847", "localhost", "[::1]:3847"] {
            assert!(is_local_host(ok), "{ok}");
        }
        for bad in ["evil.com", "evil.com:3847", "127.0.0.1.evil.com:3847", "192.168.1.5:3847"] {
            assert!(!is_local_host(bad), "{bad}");
        }
    }

    #[test]
    fn origin_guard() {
        for ok in ["null", "http://127.0.0.1:3847", "http://localhost:1420", "tauri://localhost", "http://tauri.localhost"] {
            assert!(is_allowed_origin(ok, 3847), "{ok}");
        }
        for bad in ["https://evil.com", "http://127.0.0.1:8080", "http://localhost.evil.com:3847", "file://x"] {
            assert!(!is_allowed_origin(bad, 3847), "{bad}");
        }
    }
}
