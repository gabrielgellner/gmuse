//! Turning a line of text into a [`Command`].
//!
//! One parser for the rc file, the `:` command line, and `:bind`'s nested
//! command. Hand-rolled recursive descent, per §7's "do not pull in a
//! parser-generator" — the grammar is a verb and some arguments.

use std::path::PathBuf;

use super::keys;
use super::{
    AaaMode, AddTarget, Command, Context, Quality, SearchDir, SeekSpec, SettingKey, SettingValue,
    ViewId, VolSpec,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("unknown command {0:?}")]
    UnknownCommand(String),
    #[error("{cmd} needs {what}")]
    Missing {
        cmd: &'static str,
        what: &'static str,
    },
    #[error("{0:?} is not a {1}")]
    Bad(String, &'static str),
    #[error("{0}")]
    Key(#[from] keys::KeyParseError),
}

/// Parse one line. Blank lines and `#` comments give `Ok(None)`, so the rc
/// file and the `:` line can share this without either special-casing.
pub fn line(input: &str) -> Result<Option<Command>, ParseError> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('#') {
        return Ok(None);
    }
    command(input).map(Some)
}

pub fn command(input: &str) -> Result<Command, ParseError> {
    let input = input.trim();
    let (verb, rest) = split_word(input);
    let rest = rest.trim();

    Ok(match verb {
        // Navigation.
        "win-up" | "up" => Command::WinUp,
        "win-down" | "down" => Command::WinDown,
        "win-page-up" | "page-up" => Command::WinPageUp,
        "win-page-down" | "page-down" => Command::WinPageDown,
        "win-top" | "top" => Command::WinTop,
        "win-bottom" | "bottom" => Command::WinBottom,
        "win-activate" | "activate" => Command::WinActivate,
        "win-toggle" => Command::WinToggle,
        "win-remove" | "remove" => Command::WinRemove,
        "win-update" => Command::WinUpdate,
        "win-next-pane" | "pane" => Command::WinNextPane,
        "win-expand-all" => Command::WinExpandAll,
        "win-collapse-all" => Command::WinCollapseAll,

        // Playback. The short aliases are what a person types.
        "player-play" | "play" => Command::PlayerPlay,
        "player-pause" | "pause" => Command::PlayerPause,
        "player-stop" | "stop" => Command::PlayerStop,
        "player-next" | "next" => Command::PlayerNext,
        "player-prev" | "prev" | "previous" => Command::PlayerPrev,
        "seek" => Command::Seek(seek_spec(rest)?),
        "vol" | "volume" => Command::Vol(vol_spec(rest)?),

        // Library.
        "add" => Command::Add {
            target: AddTarget::Library,
            path: optional_path(rest),
        },
        "add-playlist" | "append" => Command::Add {
            target: AddTarget::Playlist,
            path: optional_path(rest),
        },
        "add-queue" | "enqueue" => Command::Add {
            target: AddTarget::Queue,
            path: optional_path(rest),
        },
        "update" | "rescan" => Command::Update,

        // State.
        "set" => {
            let (k, v) = rest.split_once('=').ok_or(ParseError::Missing {
                cmd: "set",
                what: "key=value",
            })?;
            let key = SettingKey::parse(k.trim())
                .ok_or_else(|| ParseError::Bad(k.trim().into(), "setting"))?;
            Command::Set {
                key,
                value: setting_value(key, v.trim())?,
            }
        }
        "toggle" => {
            let key =
                SettingKey::parse(rest).ok_or_else(|| ParseError::Bad(rest.into(), "setting"))?;
            Command::Toggle(key)
        }
        "bind" => {
            // bind <context> <keys> <command...>
            let (ctx_word, rest) = split_word(rest);
            let (key_word, cmd_text) = take_keys(rest.trim());
            let ctx = Context::parse(ctx_word)
                .ok_or_else(|| ParseError::Bad(ctx_word.into(), "context"))?;
            let key = keys::parse_seq(&key_word)?;
            if cmd_text.trim().is_empty() {
                return Err(ParseError::Missing {
                    cmd: "bind",
                    what: "a command to bind",
                });
            }
            Command::Bind {
                ctx,
                key,
                cmd: Box::new(command(cmd_text)?),
            }
        }
        "unbind" => {
            let (ctx_word, rest) = split_word(rest);
            let (key_word, _) = take_keys(rest.trim());
            let ctx = Context::parse(ctx_word)
                .ok_or_else(|| ParseError::Bad(ctx_word.into(), "context"))?;
            Command::Unbind {
                ctx,
                key: keys::parse_seq(&key_word)?,
            }
        }

        // Views.
        "view" => {
            let n: u8 = rest
                .parse()
                .map_err(|_| ParseError::Bad(rest.into(), "view number"))?;
            Command::View(
                ViewId::from_number(n)
                    .ok_or_else(|| ParseError::Bad(rest.into(), "view number"))?,
            )
        }
        "view-next" => Command::ViewNext,
        "view-prev" => Command::ViewPrev,
        "view-last" => Command::ViewLast,
        "search" => Command::Search {
            pattern: rest.to_string(),
            dir: SearchDir::Forward,
        },
        "rsearch" => Command::Search {
            pattern: rest.to_string(),
            dir: SearchDir::Backward,
        },
        "search-next" => Command::SearchNext,
        "search-prev" => Command::SearchPrev,
        "catalog" | "cat" => Command::Catalog(catalog_spec(rest)?),
        "quit" | "q" | "exit" => Command::Quit,

        other => return Err(ParseError::UnknownCommand(other.to_string())),
    })
}

/// `all`, a list of names, or a run of `+name` / `-name` adjustments.
///
/// Mixing bare names with adjustments is rejected rather than guessed at:
/// `:catalog jazz -rock` could mean either "just jazz, minus rock" or "add
/// jazz, remove rock", and being wrong silently is worse than asking again.
fn catalog_spec(rest: &str) -> Result<crate::catalog::Spec, ParseError> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    if words.is_empty() {
        return Err(ParseError::Missing {
            cmd: "catalog",
            what: "a catalog name, a list, +name/-name, or `all`",
        });
    }
    if words.len() == 1 && words[0] == "all" {
        return Ok(crate::catalog::Spec::All);
    }

    let adjusting = words.iter().any(|w| w.starts_with(['+', '-']));
    let bare = words.iter().any(|w| !w.starts_with(['+', '-']));
    if adjusting && bare {
        return Err(ParseError::Bad(
            rest.into(),
            "catalog list — use either names or +/- adjustments, not both",
        ));
    }

    if adjusting {
        Ok(crate::catalog::Spec::Adjust(
            words
                .iter()
                .map(|w| (w.starts_with('+'), w[1..].to_string()))
                .collect(),
        ))
    } else {
        Ok(crate::catalog::Spec::Only(
            words.iter().map(|w| w.to_string()).collect(),
        ))
    }
}

/// Take the key field, which may be a quoted sequence (`"g g"`) or a single
/// key. Returns the key text and whatever follows.
///
/// Quoting is what makes a chord expressible on one line: without it there is
/// no way to tell where the keys stop and the command starts.
fn take_keys(s: &str) -> (String, &str) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return (rest[..end].to_string(), rest[end + 1..].trim_start());
    }
    let (word, rest) = split_word(s);
    (word.to_string(), rest)
}

fn split_word(s: &str) -> (&str, &str) {
    match s.trim().split_once(char::is_whitespace) {
        Some((a, b)) => (a, b),
        None => (s.trim(), ""),
    }
}

fn optional_path(rest: &str) -> Option<PathBuf> {
    (!rest.is_empty()).then(|| PathBuf::from(unquote(rest)))
}

fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.replace("\\ ", " ")
}

/// `90`, `1:30`, `+10`, `-1:05`. The sign is what makes it relative, so `+0`
/// is a no-op rather than a jump to the start.
fn seek_spec(rest: &str) -> Result<SeekSpec, ParseError> {
    if rest.is_empty() {
        return Err(ParseError::Missing {
            cmd: "seek",
            what: "a time",
        });
    }
    let relative = rest.starts_with('+') || rest.starts_with('-');
    let negative = rest.starts_with('-');
    let body = rest.trim_start_matches(['+', '-']);
    let seconds = clock(body).ok_or_else(|| ParseError::Bad(rest.into(), "time"))?;
    let seconds = if negative { -seconds } else { seconds };
    Ok(if relative {
        SeekSpec::Relative(seconds)
    } else {
        SeekSpec::Absolute(seconds)
    })
}

/// `90`, `1:30`, `1:02:03`.
fn clock(s: &str) -> Option<f64> {
    let mut total = 0.0;
    for part in s.split(':') {
        let v: f64 = part.parse().ok()?;
        if v < 0.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// `50`, `50%`, `+10`, `-5%`. Percentages, because that is what a volume is.
fn vol_spec(rest: &str) -> Result<VolSpec, ParseError> {
    if rest.is_empty() {
        return Err(ParseError::Missing {
            cmd: "vol",
            what: "a level",
        });
    }
    let relative = rest.starts_with('+') || rest.starts_with('-');
    let body = rest.trim_end_matches('%');
    let value: f32 = body
        .parse()
        .map_err(|_| ParseError::Bad(rest.into(), "volume"))?;
    Ok(if relative {
        VolSpec::Relative(value / 100.0)
    } else {
        VolSpec::Absolute(value / 100.0)
    })
}

fn setting_value(key: SettingKey, text: &str) -> Result<SettingValue, ParseError> {
    Ok(match key {
        SettingKey::AaaMode => SettingValue::Aaa(match text {
            "all" => AaaMode::All,
            "artist" => AaaMode::Artist,
            "album" => AaaMode::Album,
            _ => return Err(ParseError::Bad(text.into(), "aaa mode")),
        }),
        SettingKey::ResampleQuality => SettingValue::Quality(match text {
            "fast" => Quality::Fast,
            "balanced" => Quality::Balanced,
            "high" => Quality::High,
            _ => return Err(ParseError::Bad(text.into(), "resample quality")),
        }),
        _ => SettingValue::Bool(match text {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" => false,
            _ => return Err(ParseError::Bad(text.into(), "boolean")),
        }),
    })
}

/// Render a command back to text, for view 7 and for saving an rc file.
///
/// Must round-trip through [`command`], or `:bind` shown in the settings view
/// would not be the binding that is actually in force.
pub fn format(cmd: &Command) -> String {
    match cmd {
        Command::WinUp => "win-up".into(),
        Command::WinDown => "win-down".into(),
        Command::WinPageUp => "win-page-up".into(),
        Command::WinPageDown => "win-page-down".into(),
        Command::WinTop => "win-top".into(),
        Command::WinBottom => "win-bottom".into(),
        Command::WinActivate => "win-activate".into(),
        Command::WinToggle => "win-toggle".into(),
        Command::WinRemove => "win-remove".into(),
        Command::WinUpdate => "win-update".into(),
        Command::WinNextPane => "win-next-pane".into(),
        Command::WinExpandAll => "win-expand-all".into(),
        Command::WinCollapseAll => "win-collapse-all".into(),
        Command::PlayerPlay => "player-play".into(),
        Command::PlayerPause => "player-pause".into(),
        Command::PlayerStop => "player-stop".into(),
        Command::PlayerNext => "player-next".into(),
        Command::PlayerPrev => "player-prev".into(),
        Command::Seek(SeekSpec::Absolute(s)) => format!("seek {s}"),
        Command::Seek(SeekSpec::Relative(s)) if *s >= 0.0 => format!("seek +{s}"),
        Command::Seek(SeekSpec::Relative(s)) => format!("seek {s}"),
        Command::Vol(VolSpec::Absolute(v)) => format!("vol {}%", v * 100.0),
        Command::Vol(VolSpec::Relative(v)) if *v >= 0.0 => format!("vol +{}%", v * 100.0),
        Command::Vol(VolSpec::Relative(v)) => format!("vol {}%", v * 100.0),
        Command::Add { target, path } => {
            let verb = match target {
                AddTarget::Library => "add",
                AddTarget::Playlist => "add-playlist",
                AddTarget::Queue => "add-queue",
            };
            match path {
                Some(p) => format!("{verb} {}", p.display()),
                None => verb.into(),
            }
        }
        Command::Update => "update".into(),
        Command::Set { key, value } => format!("set {}={}", key.name(), value),
        Command::Toggle(key) => format!("toggle {}", key.name()),
        Command::Bind { ctx, key, cmd } => {
            let keys = keys::format_seq(key);
            let quoted = if key.len() > 1 {
                format!("\"{keys}\"")
            } else {
                keys
            };
            format!("bind {} {} {}", ctx.name(), quoted, format(cmd))
        }
        Command::Unbind { ctx, key } => {
            let keys = keys::format_seq(key);
            let quoted = if key.len() > 1 {
                format!("\"{keys}\"")
            } else {
                keys
            };
            format!("unbind {} {}", ctx.name(), quoted)
        }
        Command::View(v) => format!("view {}", *v as u8),
        Command::ViewNext => "view-next".into(),
        Command::ViewPrev => "view-prev".into(),
        Command::ViewLast => "view-last".into(),
        Command::Catalog(spec) => match spec {
            crate::catalog::Spec::All => "catalog all".into(),
            crate::catalog::Spec::Only(names) => format!("catalog {}", names.join(" ")),
            crate::catalog::Spec::Adjust(deltas) => format!(
                "catalog {}",
                deltas
                    .iter()
                    .map(|(add, n)| format!("{}{n}", if *add { "+" } else { "-" }))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        },
        Command::Search { pattern, dir } => match dir {
            SearchDir::Forward => format!("search {pattern}"),
            SearchDir::Backward => format!("rsearch {pattern}"),
        },
        Command::SearchNext => "search-next".into(),
        Command::SearchPrev => "search-prev".into(),
        Command::Quit => "quit".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn blank_lines_and_comments_are_not_errors() {
        assert_eq!(line("").unwrap(), None);
        assert_eq!(line("   ").unwrap(), None);
        assert_eq!(line("# a comment").unwrap(), None);
        assert_eq!(line("  # indented").unwrap(), None);
    }

    #[test]
    fn short_aliases_and_canonical_names_agree() {
        for (short, long) in [
            ("play", "player-play"),
            ("pause", "player-pause"),
            ("stop", "player-stop"),
            ("next", "player-next"),
            ("prev", "player-prev"),
            ("q", "quit"),
        ] {
            assert_eq!(command(short).unwrap(), command(long).unwrap(), "{short}");
        }
    }

    #[test]
    fn seek_accepts_clock_times_and_signs() {
        assert_eq!(
            command("seek 90").unwrap(),
            Command::Seek(SeekSpec::Absolute(90.0))
        );
        assert_eq!(
            command("seek 1:30").unwrap(),
            Command::Seek(SeekSpec::Absolute(90.0))
        );
        assert_eq!(
            command("seek 1:02:03").unwrap(),
            Command::Seek(SeekSpec::Absolute(3723.0))
        );
        assert_eq!(
            command("seek +10").unwrap(),
            Command::Seek(SeekSpec::Relative(10.0))
        );
        assert_eq!(
            command("seek -10").unwrap(),
            Command::Seek(SeekSpec::Relative(-10.0))
        );
        assert_eq!(
            command("seek -1:05").unwrap(),
            Command::Seek(SeekSpec::Relative(-65.0))
        );
        // A signed zero stays relative, so it is a no-op rather than a rewind.
        assert_eq!(
            command("seek +0").unwrap(),
            Command::Seek(SeekSpec::Relative(0.0))
        );
        assert!(command("seek").is_err());
        assert!(command("seek soon").is_err());
    }

    #[test]
    fn volume_is_a_percentage() {
        assert_eq!(
            command("vol 50").unwrap(),
            Command::Vol(VolSpec::Absolute(0.5))
        );
        assert_eq!(
            command("vol 50%").unwrap(),
            Command::Vol(VolSpec::Absolute(0.5))
        );
        assert_eq!(
            command("vol +10%").unwrap(),
            Command::Vol(VolSpec::Relative(0.1))
        );
        assert_eq!(
            command("vol -5").unwrap(),
            Command::Vol(VolSpec::Relative(-0.05))
        );
        assert!(command("vol loud").is_err());
    }

    #[test]
    fn settings_are_typed_per_key() {
        assert_eq!(
            command("set shuffle=true").unwrap(),
            Command::Set {
                key: SettingKey::Shuffle,
                value: SettingValue::Bool(true)
            }
        );
        assert_eq!(
            command("set aaa_mode=album").unwrap(),
            Command::Set {
                key: SettingKey::AaaMode,
                value: SettingValue::Aaa(AaaMode::Album)
            }
        );
        assert_eq!(
            command("set resample_quality=high").unwrap(),
            Command::Set {
                key: SettingKey::ResampleQuality,
                value: SettingValue::Quality(Quality::High)
            }
        );
        // A boolean value for an enum key is an error, not a silent default.
        assert!(command("set aaa_mode=true").is_err());
        assert!(command("set shuffle=maybe").is_err());
        assert!(command("set nosuchthing=1").is_err());
        assert!(command("set shuffle").is_err());
    }

    #[test]
    fn bind_nests_a_whole_command() {
        let got = command("bind common ^n player-next").unwrap();
        assert_eq!(
            got,
            Command::Bind {
                ctx: Context::Common,
                key: vec![keys::Key::ctrl(KeyCode::Char('n'))],
                cmd: Box::new(Command::PlayerNext),
            }
        );

        // Including one with arguments of its own.
        let got = command("bind library J seek +30").unwrap();
        let Command::Bind { cmd, .. } = got else {
            panic!("not a bind")
        };
        assert_eq!(*cmd, Command::Seek(SeekSpec::Relative(30.0)));
    }

    #[test]
    fn bind_reports_what_is_missing() {
        assert!(matches!(
            command("bind common ^n"),
            Err(ParseError::Missing { cmd: "bind", .. })
        ));
        assert!(command("bind nosuchview j play").is_err());
        assert!(command("bind common notakey play").is_err());
    }

    /// §9's M6 criterion: a rebinding must survive a trip through the rc file.
    #[test]
    fn every_command_round_trips_through_text() {
        let samples = vec![
            Command::WinUp,
            Command::WinDown,
            Command::WinPageUp,
            Command::WinPageDown,
            Command::WinTop,
            Command::WinBottom,
            Command::WinActivate,
            Command::WinToggle,
            Command::WinRemove,
            Command::WinUpdate,
            Command::WinNextPane,
            Command::WinExpandAll,
            Command::WinCollapseAll,
            Command::PlayerPlay,
            Command::PlayerPause,
            Command::PlayerStop,
            Command::PlayerNext,
            Command::PlayerPrev,
            Command::Seek(SeekSpec::Absolute(90.0)),
            Command::Seek(SeekSpec::Relative(10.0)),
            Command::Seek(SeekSpec::Relative(-10.0)),
            Command::Vol(VolSpec::Absolute(0.5)),
            Command::Vol(VolSpec::Relative(0.1)),
            Command::Vol(VolSpec::Relative(-0.05)),
            Command::Add {
                target: AddTarget::Library,
                path: Some(PathBuf::from("/m")),
            },
            Command::Add {
                target: AddTarget::Playlist,
                path: None,
            },
            Command::Add {
                target: AddTarget::Queue,
                path: None,
            },
            Command::Update,
            Command::Set {
                key: SettingKey::Shuffle,
                value: SettingValue::Bool(true),
            },
            Command::Set {
                key: SettingKey::AaaMode,
                value: SettingValue::Aaa(AaaMode::Artist),
            },
            Command::Set {
                key: SettingKey::ResampleQuality,
                value: SettingValue::Quality(Quality::Fast),
            },
            Command::Toggle(SettingKey::Repeat),
            Command::Bind {
                ctx: Context::Library,
                key: vec![keys::Key::ctrl(KeyCode::Char('n'))],
                cmd: Box::new(Command::PlayerNext),
            },
            Command::Bind {
                ctx: Context::Common,
                key: vec![keys::Key::new(KeyCode::F(5))],
                cmd: Box::new(Command::Seek(SeekSpec::Relative(30.0))),
            },
            Command::Unbind {
                ctx: Context::Queue,
                key: vec![keys::Key::new(KeyCode::Char(' '))],
            },
            Command::View(ViewId::Settings),
            Command::Search {
                pattern: "burial".into(),
                dir: SearchDir::Forward,
            },
            Command::Search {
                pattern: "burial".into(),
                dir: SearchDir::Backward,
            },
            Command::SearchNext,
            Command::SearchPrev,
            Command::Quit,
        ];

        for cmd in samples {
            let text = format(&cmd);
            let back = command(&text)
                .unwrap_or_else(|e| panic!("{cmd:?} formatted as {text:?} and failed: {e}"));
            assert_eq!(back, cmd, "{text:?} did not round trip");
        }
    }

    #[test]
    fn unknown_commands_say_so() {
        assert_eq!(
            command("frobnicate").unwrap_err(),
            ParseError::UnknownCommand("frobnicate".into())
        );
    }
}
