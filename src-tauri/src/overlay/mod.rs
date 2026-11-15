//! Local HTTP + WebSocket server for the OBS Browser Source overlay.
//!
//! - `GET /overlay`: overlay page. `?theme=neon` overrides the saved theme,
//!   `?demo=1` shows a sample track while idle, `?preview=1` adds a checkerboard.
//! - `GET /overlay/ws`: push channel (`hello`, then `player` / `config` / `upNext`).
//! - `GET /api/now-playing`: current snapshot as JSON, for other tools.
//!
//! Bound to 127.0.0.1 only. Host and Origin headers are checked so a web page
//! can't reach the server through DNS rebinding.

mod server;

pub use server::OverlayServer;
