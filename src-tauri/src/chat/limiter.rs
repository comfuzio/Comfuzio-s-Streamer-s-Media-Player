//! Rate limiting: per-viewer command cooldowns, per-channel info cooldowns,
//! one in-flight request per viewer, and a token bucket for outgoing messages.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::parser::ChatCommand;
use super::ChatPlatform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    Request,
    WrongSong,
    Info(ChatCommand),
}

#[derive(Debug, Clone, Copy)]
pub struct Cooldowns {
    pub request: Duration,
    pub info: Duration,
}

#[derive(Default)]
pub struct CommandLimiter {
    /// (platform, user id, bucket) -> last accepted use
    per_user: HashMap<(ChatPlatform, String, Bucket), Instant>,
    /// (platform, info command) -> last reply
    per_channel: HashMap<(ChatPlatform, ChatCommand), Instant>,
    /// Viewers whose request is still being resolved.
    in_flight: HashSet<(ChatPlatform, String)>,
}

pub enum Verdict {
    Allow,
    /// Ignore silently (spam protection; replying would add to the spam).
    Drop,
}

impl CommandLimiter {
    /// Decides whether a viewer command may run now. Privileged users are never limited.
    pub fn check(
        &mut self,
        platform: ChatPlatform,
        user_id: &str,
        command: ChatCommand,
        privileged: bool,
        cooldowns: Cooldowns,
        now: Instant,
    ) -> Verdict {
        let (request_cooldown, info_cooldown) = (cooldowns.request, cooldowns.info);
        if privileged {
            return Verdict::Allow;
        }
        let (bucket, cooldown) = match command {
            ChatCommand::Request => (Bucket::Request, request_cooldown),
            ChatCommand::WrongSong => (Bucket::WrongSong, request_cooldown),
            ChatCommand::Song | ChatCommand::Queue => {
                let key = (platform, command);
                if self.per_channel.get(&key).is_some_and(|t| now.duration_since(*t) < info_cooldown) {
                    return Verdict::Drop;
                }
                self.per_channel.insert(key, now);
                (Bucket::Info(command), info_cooldown * 2)
            }
            _ => return Verdict::Allow,
        };
        if command == ChatCommand::Request && self.in_flight.contains(&(platform, user_id.to_owned())) {
            return Verdict::Drop;
        }
        let key = (platform, user_id.to_owned(), bucket);
        if self.per_user.get(&key).is_some_and(|t| now.duration_since(*t) < cooldown) {
            return Verdict::Drop;
        }
        self.per_user.insert(key, now);
        if self.per_user.len() > 10_000 {
            let horizon = Duration::from_secs(3600);
            self.per_user.retain(|_, t| now.duration_since(*t) < horizon);
        }
        Verdict::Allow
    }

    pub fn begin_request(&mut self, platform: ChatPlatform, user_id: &str) {
        self.in_flight.insert((platform, user_id.to_owned()));
    }

    pub fn end_request(&mut self, platform: ChatPlatform, user_id: &str) {
        self.in_flight.remove(&(platform, user_id.to_owned()));
    }
}

/// Classic token bucket for outgoing chat messages.
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, per: Duration) -> Self {
        Self {
            capacity: f64::from(capacity),
            tokens: f64::from(capacity),
            refill_per_sec: f64::from(capacity) / per.as_secs_f64(),
            last: Instant::now(),
        }
    }

    /// Time to wait before a token is available (zero = take it now).
    pub fn reserve(&mut self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.tokens -= 1.0;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.refill_per_sec)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CD: Duration = Duration::from_secs(5);

    fn allowed(l: &mut CommandLimiter, user: &str, cmd: ChatCommand, privileged: bool, at: Instant) -> bool {
        matches!(l.check(ChatPlatform::Twitch, user, cmd, privileged, Cooldowns { request: CD, info: CD }, at), Verdict::Allow)
    }

    #[test]
    fn request_cooldown_is_per_user() {
        let mut l = CommandLimiter::default();
        let t0 = Instant::now();
        assert!(allowed(&mut l, "a", ChatCommand::Request, false, t0));
        assert!(!allowed(&mut l, "a", ChatCommand::Request, false, t0 + Duration::from_secs(2)));
        assert!(allowed(&mut l, "b", ChatCommand::Request, false, t0 + Duration::from_secs(2)));
        assert!(allowed(&mut l, "a", ChatCommand::Request, false, t0 + Duration::from_secs(6)));
    }

    #[test]
    fn privileged_and_in_flight() {
        let mut l = CommandLimiter::default();
        let t0 = Instant::now();
        for i in 0..5 {
            assert!(allowed(&mut l, "mod", ChatCommand::Request, true, t0 + Duration::from_millis(i)));
        }
        l.begin_request(ChatPlatform::Twitch, "v");
        assert!(!allowed(&mut l, "v", ChatCommand::Request, false, t0));
        l.end_request(ChatPlatform::Twitch, "v");
        assert!(allowed(&mut l, "v", ChatCommand::Request, false, t0));
    }

    #[test]
    fn info_commands_share_a_channel_cooldown() {
        let mut l = CommandLimiter::default();
        let t0 = Instant::now();
        assert!(allowed(&mut l, "a", ChatCommand::Song, false, t0));
        assert!(!allowed(&mut l, "b", ChatCommand::Song, false, t0 + Duration::from_secs(1)));
        assert!(allowed(&mut l, "b", ChatCommand::Queue, false, t0 + Duration::from_secs(1)));
    }

    #[test]
    fn token_bucket_paces_bursts() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2, Duration::from_secs(2));
        assert_eq!(b.reserve(t0), Duration::ZERO);
        assert_eq!(b.reserve(t0), Duration::ZERO);
        let wait = b.reserve(t0);
        assert!((wait.as_secs_f64() - 1.0).abs() < 0.01, "{wait:?}");
    }
}
