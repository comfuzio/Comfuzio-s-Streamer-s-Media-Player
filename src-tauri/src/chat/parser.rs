//! `!<namespace><command> [args]` parsing. The namespace is user-configurable
//! (default `mb`), so `!mbsr`, `!musicsr`, `!radiosr` ... all work.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatCommand {
    Request,
    Song,
    Queue,
    WrongSong,
    Start,
    Stop,
    Pause,
    Skip,
    VolUp,
    VolDown,
    Vol,
    Clear,
    Block,
    Unblock,
    MaxLength,
}

impl ChatCommand {
    /// Commands reserved for the broadcaster and moderators.
    pub fn privileged_only(self) -> bool {
        !matches!(self, Self::Request | Self::Song | Self::Queue | Self::WrongSong)
    }

    fn from_suffix(s: &str) -> Option<Self> {
        Some(match s {
            "sr" => Self::Request,
            "song" => Self::Song,
            "queue" => Self::Queue,
            "wrongsong" => Self::WrongSong,
            "start" => Self::Start,
            "stop" => Self::Stop,
            "skip" => Self::Skip,
            "volup" => Self::VolUp,
            "voldown" => Self::VolDown,
            "vol" => Self::Vol,
            "clear" => Self::Clear,
            "block" => Self::Block,
            "unblock" => Self::Unblock,
            "maxlength" => Self::MaxLength,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub command: ChatCommand,
    pub args: String,
}

/// Characters some chat clients append to bypass duplicate-message filters.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{034F}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}')
}

/// Parses a chat line. `privileged` enables the `sr play|start|pause|stop` aliases.
pub fn parse(text: &str, namespace: &str, privileged: bool) -> Option<ParsedCommand> {
    let cleaned: String = text.chars().filter(|c| !is_invisible(*c)).collect();
    let rest = cleaned.trim().strip_prefix('!')?;
    let (word, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    let word = word.to_lowercase();
    let suffix = word.strip_prefix(namespace)?;
    let mut command = ChatCommand::from_suffix(suffix)?;
    let mut args = args.split_whitespace().collect::<Vec<_>>().join(" ");

    if command == ChatCommand::Request && privileged {
        let alias = match args.to_lowercase().as_str() {
            "play" | "start" | "resume" => Some(ChatCommand::Start),
            "pause" => Some(ChatCommand::Pause),
            "stop" => Some(ChatCommand::Stop),
            _ => None,
        };
        if let Some(a) = alias {
            command = a;
            args.clear();
        }
    }
    Some(ParsedCommand { command, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(text: &str, ns: &str, privileged: bool) -> Option<(ChatCommand, String)> {
        parse(text, ns, privileged).map(|p| (p.command, p.args))
    }

    #[test]
    fn parses_with_default_namespace() {
        assert_eq!(cmd("!mbsr daft punk - one more time", "mb", false), Some((ChatCommand::Request, "daft punk - one more time".into())));
        assert_eq!(cmd("!MBSong", "mb", false), Some((ChatCommand::Song, String::new())));
        assert_eq!(cmd("  !mbvol   40 ", "mb", true), Some((ChatCommand::Vol, "40".into())));
        assert_eq!(cmd("!mbmaxlength 5m", "mb", true), Some((ChatCommand::MaxLength, "5m".into())));
    }

    #[test]
    fn namespace_is_dynamic() {
        assert_eq!(cmd("!musicskip", "music", true), Some((ChatCommand::Skip, String::new())));
        assert_eq!(cmd("!mbskip", "music", true), None);
        assert_eq!(cmd("!radioqueue", "radio", false), Some((ChatCommand::Queue, String::new())));
    }

    #[test]
    fn ignores_non_commands() {
        for t in ["mbsr song", "!mb", "!mbsrx", "!sr song", "hello !mbsr", "!mbsr\u{200B}x"] {
            assert_eq!(cmd(t, "mb", false), None, "{t}");
        }
    }

    #[test]
    fn strips_invisible_suffixes() {
        assert_eq!(cmd("!mbsong \u{E0000}", "mb", false), Some((ChatCommand::Song, String::new())));
        assert_eq!(cmd("!mbsong\u{034F}", "mb", false), Some((ChatCommand::Song, String::new())));
    }

    #[test]
    fn sr_aliases_only_for_privileged() {
        assert_eq!(cmd("!mbsr pause", "mb", true), Some((ChatCommand::Pause, String::new())));
        assert_eq!(cmd("!mbsr play", "mb", true), Some((ChatCommand::Start, String::new())));
        assert_eq!(cmd("!mbsr stop", "mb", true), Some((ChatCommand::Stop, String::new())));
        assert_eq!(cmd("!mbsr pause", "mb", false), Some((ChatCommand::Request, "pause".into())));
    }

    #[test]
    fn privilege_table() {
        assert!(!ChatCommand::Request.privileged_only());
        assert!(!ChatCommand::WrongSong.privileged_only());
        assert!(ChatCommand::Skip.privileged_only());
        assert!(ChatCommand::Block.privileged_only());
    }
}
