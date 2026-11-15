//! Chat bot engine: platform connectors (Twitch EventSub, YouTube Live Chat),
//! the `!<namespace><command>` parser, permissions, rate limiting and replies.

pub mod handler;
pub mod hub;
pub mod limiter;
pub mod parser;
pub mod twitch;
pub mod youtube;

use serde::{Deserialize, Serialize};

use crate::model::{Platform, Requester};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ChatPlatform {
    Twitch,
    YouTube,
}

impl ChatPlatform {
    pub fn platform(self) -> Platform {
        match self {
            Self::Twitch => Platform::Twitch,
            Self::YouTube => Platform::YouTube,
        }
    }

    /// Maximum characters per outgoing message.
    pub fn max_message_len(self) -> usize {
        match self {
            Self::Twitch => 500,
            Self::YouTube => 200,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Twitch => "Twitch",
            Self::YouTube => "YouTube",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatRoles {
    pub broadcaster: bool,
    pub moderator: bool,
    pub vip: bool,
    pub subscriber: bool,
}

impl ChatRoles {
    /// Streamer & moderators: may use control commands and bypass request limits.
    pub fn privileged(&self) -> bool {
        self.broadcaster || self.moderator
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub platform: ChatPlatform,
    pub message_id: String,
    pub user: Requester,
    pub roles: ChatRoles,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Outbound {
    pub text: String,
    /// Platform message id to thread the reply under (Twitch only).
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Disabled,
    NotConfigured,
    Connecting,
    Connected,
    /// Signed in, but there is no live broadcast (YouTube) to attach to.
    WaitingForStream,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatStatus {
    pub platform: ChatPlatform,
    pub state: ConnectionState,
    /// Channel name / broadcast title we're attached to.
    pub channel: Option<String>,
    /// Signed-in account.
    pub account: Option<String>,
    pub detail: Option<String>,
}

impl ChatStatus {
    pub fn new(platform: ChatPlatform, state: ConnectionState) -> Self {
        Self { platform, state, channel: None, account: None, detail: None }
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// Events flowing from connectors into the hub.
#[derive(Debug)]
pub enum Inbound {
    Message(ChatMessage),
    StreamOffline(ChatPlatform),
    Status(ChatStatus),
}

/// Truncates on a char boundary, adding an ellipsis when shortened.
pub fn fit_message(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
