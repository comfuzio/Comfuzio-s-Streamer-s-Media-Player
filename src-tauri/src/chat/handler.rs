//! Executes parsed chat commands against the player and phrases the replies.

use std::sync::Arc;

use super::parser::{ChatCommand, ParsedCommand};
use super::ChatMessage;
use crate::error::AppError;
use crate::model::PlayerStatus;
use crate::persistence::settings::SettingsStore;
use crate::player::PlaybackController;
use crate::util::duration::{format_clock_ms, format_human_ms, MAX_LENGTH_CEILING, MAX_LENGTH_FLOOR};

pub struct CommandHandler {
    player: Arc<PlaybackController>,
    settings: Arc<SettingsStore>,
}

fn describe_error(err: &AppError) -> String {
    match err {
        AppError::Rejected(r) => r.to_string(),
        AppError::Invalid(m) | AppError::NotFound(m) => m.clone(),
        AppError::YtDlp(_) => "couldn't fetch that from YouTube right now".into(),
        AppError::Spotify(_) => "couldn't look that up on Spotify right now".into(),
        AppError::Http(_) => "network problem, try again in a moment".into(),
        other => {
            tracing::error!(error = %other, "chat command failed");
            "something went wrong".into()
        }
    }
}

impl CommandHandler {
    pub fn new(player: Arc<PlaybackController>, settings: Arc<SettingsStore>) -> Self {
        Self { player, settings }
    }

    /// Runs `cmd` for `msg`. Returns the reply body (without the @mention), or
    /// `None` when nothing should be said.
    pub async fn execute(&self, msg: &ChatMessage, cmd: &ParsedCommand) -> Option<String> {
        let ns = self.settings.get().command_namespace;
        let privileged = msg.roles.privileged();
        let args = cmd.args.as_str();
        let who = msg.user.display_name.as_str();

        let reply = match cmd.command {
            ChatCommand::Request => {
                if args.is_empty() {
                    return Some(format!("usage: !{ns}sr <YouTube/Spotify link or artist - song>"));
                }
                match self.player.request(args, msg.user.clone(), privileged).await {
                    Ok(out) => {
                        let track = &out.item.track;
                        let when = if out.eta_ms == 0 && out.position == 1 {
                            "up next".to_owned()
                        } else {
                            format!("plays in ~{}", format_human_ms(out.eta_ms))
                        };
                        let stopped = if self.player.snapshot().await.status == PlayerStatus::Stopped {
                            " (player is stopped)"
                        } else {
                            ""
                        };
                        format!(
                            "added #{} {} [{}], {when}{stopped}",
                            out.position,
                            track.display_name(),
                            format_clock_ms(track.duration_ms)
                        )
                    }
                    Err(e) => describe_error(&e),
                }
            }

            ChatCommand::Song => {
                let snap = self.player.snapshot().await;
                match (&snap.current, snap.status) {
                    (Some(item), PlayerStatus::Playing | PlayerStatus::Paused | PlayerStatus::Loading) => {
                        let paused = if snap.status == PlayerStatus::Paused { " (paused)" } else { "" };
                        let who = if item.autoplay {
                            format!("from the {} playlist", item.requester.display_name)
                        } else {
                            format!("requested by {}", item.requester.display_name)
                        };
                        format!(
                            "now playing: {} [{} / {}]{paused}, {who}",
                            item.track.display_name(),
                            format_clock_ms(snap.position_ms),
                            format_clock_ms(snap.duration_ms),
                        )
                    }
                    _ => "nothing is playing right now".into(),
                }
            }

            ChatCommand::Queue => {
                let summary = self.player.queue_summary().await;
                if summary.count == 0 {
                    format!("the queue is empty, request a song with !{ns}sr")
                } else {
                    let queue = self.player.queue().await;
                    let mine = queue.items.iter().position(|i| i.requester.is_same_user(&msg.user));
                    let yours = mine.map(|p| format!(", your next song is #{}", p + 1)).unwrap_or_default();
                    format!(
                        "{} song{} in the queue, ~{} total wait{yours}",
                        summary.count,
                        if summary.count == 1 { "" } else { "s" },
                        format_human_ms(summary.wait_ms)
                    )
                }
            }

            ChatCommand::WrongSong => match self.player.wrong_song(&msg.user).await {
                Some(item) => format!("removed {} from the queue", item.track.display_name()),
                None => "you have no songs in the queue".into(),
            },

            ChatCommand::Start => {
                let before = self.player.snapshot().await.status;
                let snap = self.player.play().await;
                let queued = self.player.queue_summary().await.count;
                match (before, snap.status) {
                    (PlayerStatus::Paused, _) => "resumed".into(),
                    (PlayerStatus::Stopped, _) if queued == 0 => {
                        format!("player started, the queue is empty (request with !{ns}sr)")
                    }
                    (PlayerStatus::Stopped, _) => "player started".into(),
                    _ => "the player is already running".into(),
                }
            }

            ChatCommand::Stop => {
                let before = self.player.snapshot().await.status;
                self.player.stop().await;
                if before == PlayerStatus::Stopped { "the player is already stopped".into() } else { "player stopped".into() }
            }

            ChatCommand::Pause => {
                let snap = self.player.pause().await;
                if snap.status == PlayerStatus::Paused { "paused".into() } else { "nothing is playing".into() }
            }

            ChatCommand::Skip => match self.player.skip().await {
                Some(item) => format!("skipped {}", item.track.display_name()),
                None => "nothing to skip".into(),
            },

            ChatCommand::VolUp | ChatCommand::VolDown => {
                let delta = if cmd.command == ChatCommand::VolUp { 10 } else { -10 };
                match self.player.adjust_volume(delta).await {
                    Ok(v) => format!("volume {v}%"),
                    Err(e) => describe_error(&e),
                }
            }

            ChatCommand::Vol => {
                if args.is_empty() {
                    format!("volume is {}%", self.settings.get().volume)
                } else {
                    match args.trim_end_matches('%').trim().parse::<u8>() {
                        Ok(v) if v <= 100 => match self.player.set_volume(v).await {
                            Ok(v) => format!("volume {v}%"),
                            Err(e) => describe_error(&e),
                        },
                        _ => format!("usage: !{ns}vol <0-100>"),
                    }
                }
            }

            ChatCommand::Clear => {
                let n = self.player.clear().await;
                format!("cleared {n} song{} from the queue", if n == 1 { "" } else { "s" })
            }

            ChatCommand::Block => {
                let target = (!args.is_empty()).then_some(args);
                match self.player.block(target, who).await {
                    Ok(out) => {
                        let title = out.title.unwrap_or_else(|| "that song".into());
                        let mut s = format!("blocked {title}");
                        if out.skipped_current {
                            s.push_str(" and skipped it");
                        }
                        if out.removed_from_queue > 0 {
                            s.push_str(&format!(", removed {} from the queue", out.removed_from_queue));
                        }
                        s
                    }
                    Err(e) => describe_error(&e),
                }
            }

            ChatCommand::Unblock => {
                if args.is_empty() {
                    return Some(format!("usage: !{ns}unblock <link>"));
                }
                match self.player.unblock(args).await {
                    Ok(removed) => {
                        let title = removed.iter().find_map(|e| e.title.clone()).unwrap_or_else(|| "that song".into());
                        format!("unblocked {title}")
                    }
                    Err(e) => describe_error(&e),
                }
            }

            ChatCommand::MaxLength => {
                if args.is_empty() {
                    let current = self.settings.get().max_length_secs;
                    format!("max song length is {}", format_human_ms(u64::from(current) * 1000))
                } else {
                    match self.player.set_max_length(args).await {
                        Ok(d) => {
                            let clamped = if d == MAX_LENGTH_FLOOR || d == MAX_LENGTH_CEILING { " (allowed range 30s-1h)" } else { "" };
                            format!("max song length set to {}{clamped}", format_human_ms(d.as_millis() as u64))
                        }
                        Err(_) => format!("usage: !{ns}maxlength <30s / 5m / 1h>"),
                    }
                }
            }
        };
        Some(reply)
    }
}
