//! Owns the connector tasks and routes chat traffic:
//! connector → parse → permission → rate limit → handler → reply → connector.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::async_runtime::JoinHandle;
use tokio::sync::{broadcast, mpsc};

use super::handler::CommandHandler;
use super::limiter::{CommandLimiter, Cooldowns, Verdict};
use super::parser::{parse, ChatCommand};
use super::twitch::TwitchClient;
use super::youtube::YouTubeClient;
use super::{fit_message, ChatMessage, ChatPlatform, ChatStatus, ConnectionState, Inbound, Outbound};
use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::persistence::settings::{Settings, SettingsStore};
use crate::player::PlaybackController;

pub struct ChatHub {
    handler: CommandHandler,
    player: Arc<PlaybackController>,
    settings: Arc<SettingsStore>,
    bus: EventBus,
    pub twitch: Arc<TwitchClient>,
    pub youtube: Arc<YouTubeClient>,
    limiter: Mutex<CommandLimiter>,
    inbound_tx: mpsc::Sender<Inbound>,
    outbound: Mutex<HashMap<ChatPlatform, mpsc::Sender<Outbound>>>,
    tasks: Mutex<HashMap<ChatPlatform, JoinHandle<()>>>,
    statuses: Mutex<HashMap<ChatPlatform, ChatStatus>>,
}

/// The settings that require a connector restart when they change.
fn connector_key(s: &Settings, platform: ChatPlatform) -> String {
    match platform {
        ChatPlatform::Twitch => format!("{}|{:?}|{:?}", s.twitch.enabled, s.twitch.client_id, s.twitch.channel),
        ChatPlatform::YouTube => {
            format!("{}|{:?}|{:?}", s.youtube_chat.enabled, s.youtube_chat.client_id, s.youtube_chat.client_secret)
        }
    }
}

impl ChatHub {
    pub fn start(
        player: Arc<PlaybackController>,
        settings: Arc<SettingsStore>,
        bus: EventBus,
        twitch: Arc<TwitchClient>,
        youtube: Arc<YouTubeClient>,
    ) -> Arc<Self> {
        let (inbound_tx, inbound_rx) = mpsc::channel(512);
        let hub = Arc::new(Self {
            handler: CommandHandler::new(player.clone(), settings.clone()),
            player,
            settings,
            bus: bus.clone(),
            twitch,
            youtube,
            limiter: Mutex::new(CommandLimiter::default()),
            inbound_tx,
            outbound: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
        });
        tauri::async_runtime::spawn(inbound_loop(Arc::downgrade(&hub), inbound_rx));
        tauri::async_runtime::spawn(watch_loop(Arc::downgrade(&hub), bus.subscribe()));
        hub.restart(ChatPlatform::Twitch);
        hub.restart(ChatPlatform::YouTube);
        hub
    }

    pub fn statuses(&self) -> Vec<ChatStatus> {
        let map = self.statuses.lock().expect("status lock");
        [ChatPlatform::Twitch, ChatPlatform::YouTube]
            .into_iter()
            .map(|p| map.get(&p).cloned().unwrap_or_else(|| ChatStatus::new(p, ConnectionState::Disabled)))
            .collect()
    }

    fn set_status(&self, status: ChatStatus) {
        let changed = {
            let mut map = self.statuses.lock().expect("status lock");
            let prev = map.insert(status.platform, status.clone());
            prev.as_ref() != Some(&status)
        };
        if changed {
            if status.state == ConnectionState::Error {
                if let Some(d) = &status.detail {
                    self.bus.notice(NoticeLevel::Warning, format!("{} chat: {d}", status.platform.label()));
                }
            }
            self.bus.emit(AppEvent::ChatStatus(status));
        }
    }

    /// (Re)starts one connector according to current settings.
    pub fn restart(self: &Arc<Self>, platform: ChatPlatform) {
        if let Some(task) = self.tasks.lock().expect("task lock").remove(&platform) {
            task.abort();
        }
        self.outbound.lock().expect("outbound lock").remove(&platform);

        let s = self.settings.get();
        let (enabled, configured) = match platform {
            ChatPlatform::Twitch => (s.twitch.enabled, s.twitch.client_id.is_some()),
            ChatPlatform::YouTube => {
                (s.youtube_chat.enabled, s.youtube_chat.client_id.is_some() && s.youtube_chat.client_secret.is_some())
            }
        };
        if !enabled {
            self.set_status(ChatStatus::new(platform, ConnectionState::Disabled));
            return;
        }
        if !configured {
            self.set_status(ChatStatus::new(platform, ConnectionState::NotConfigured).detail("add your app credentials in settings"));
            return;
        }
        let (out_tx, out_rx) = mpsc::channel(64);
        self.outbound.lock().expect("outbound lock").insert(platform, out_tx);
        let inbound = self.inbound_tx.clone();
        let task = match platform {
            ChatPlatform::Twitch => tauri::async_runtime::spawn(self.twitch.clone().run(inbound, out_rx)),
            ChatPlatform::YouTube => tauri::async_runtime::spawn(self.youtube.clone().run(inbound, out_rx)),
        };
        self.tasks.lock().expect("task lock").insert(platform, task);
    }

    fn reply(&self, msg: &ChatMessage, body: &str) {
        let s = self.settings.get();
        let enabled = match msg.platform {
            ChatPlatform::Twitch => s.twitch.reply_in_chat,
            ChatPlatform::YouTube => s.youtube_chat.reply_in_chat,
        };
        if !enabled {
            return;
        }
        let text = fit_message(&format!("@{} {body}", msg.user.display_name), msg.platform.max_message_len());
        let sender = self.outbound.lock().expect("outbound lock").get(&msg.platform).cloned();
        if let Some(tx) = sender {
            let reply_to = (msg.platform == ChatPlatform::Twitch).then(|| msg.message_id.clone());
            if tx.try_send(Outbound { text, reply_to }).is_err() {
                tracing::warn!(platform = ?msg.platform, "reply queue full; dropping reply");
            }
        }
    }

    fn on_message(self: &Arc<Self>, msg: ChatMessage) {
        let s = self.settings.get();
        let privileged = msg.roles.privileged();
        let Some(cmd) = parse(&msg.text, &s.command_namespace, privileged) else { return };
        if cmd.command.privileged_only() && !privileged {
            return; // silently ignore: replying would invite spam
        }
        let verdict = self.limiter.lock().expect("limiter lock").check(
            msg.platform,
            &msg.user.user_id,
            cmd.command,
            privileged,
            Cooldowns {
                request: Duration::from_secs(u64::from(s.chat.request_cooldown_secs)),
                info: Duration::from_secs(u64::from(s.chat.info_cooldown_secs)),
            },
            Instant::now(),
        );
        if matches!(verdict, Verdict::Drop) {
            return;
        }
        let is_request = cmd.command == ChatCommand::Request;
        if is_request {
            self.limiter.lock().expect("limiter lock").begin_request(msg.platform, &msg.user.user_id);
        }
        tracing::info!(platform = ?msg.platform, user = %msg.user.display_name, command = ?cmd.command, "chat command");

        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let reply = this.handler.execute(&msg, &cmd).await;
            if is_request {
                this.limiter.lock().expect("limiter lock").end_request(msg.platform, &msg.user.user_id);
            }
            if let Some(body) = reply {
                this.reply(&msg, &body);
            }
        });
    }

    async fn on_stream_offline(&self, platform: ChatPlatform) {
        if !self.settings.get().auto_stop_on_offline {
            return;
        }
        let snap = self.player.snapshot().await;
        if snap.status != crate::model::PlayerStatus::Stopped {
            self.player.stop().await;
            self.bus.notice(NoticeLevel::Info, format!("{} stream went offline, playback stopped", platform.label()));
        }
    }
}

async fn inbound_loop(hub: std::sync::Weak<ChatHub>, mut rx: mpsc::Receiver<Inbound>) {
    while let Some(event) = rx.recv().await {
        let Some(hub) = hub.upgrade() else { return };
        match event {
            Inbound::Message(msg) => hub.on_message(msg),
            Inbound::StreamOffline(p) => hub.on_stream_offline(p).await,
            Inbound::Status(s) => hub.set_status(s),
        }
    }
}

/// Restarts connectors after relevant settings changes and completed logins.
async fn watch_loop(hub: std::sync::Weak<ChatHub>, mut rx: broadcast::Receiver<AppEvent>) {
    let mut keys: HashMap<ChatPlatform, String> = match hub.upgrade() {
        Some(h) => {
            let s = h.settings.get();
            [ChatPlatform::Twitch, ChatPlatform::YouTube].into_iter().map(|p| (p, connector_key(&s, p))).collect()
        }
        None => return,
    };
    loop {
        let event = match rx.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(hub) = hub.upgrade() else { return };
        match event {
            AppEvent::Settings(s) => {
                for p in [ChatPlatform::Twitch, ChatPlatform::YouTube] {
                    let key = connector_key(&s, p);
                    if keys.get(&p) != Some(&key) {
                        keys.insert(p, key);
                        hub.restart(p);
                    }
                }
            }
            AppEvent::ChatAuth { platform, .. } => hub.restart(platform),
            _ => {}
        }
    }
}
