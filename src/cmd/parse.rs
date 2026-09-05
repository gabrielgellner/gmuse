//! Turning a line of text into a [`Command`].
//!
//! One parser for the rc file, the `:` command line, and `:bind`'s nested
//! command. Hand-rolled recursive descent, per §7's "do not pull in a
//! parser-generator" — the grammar is a verb and some arguments.

use std::path::PathBuf;

use super::keys;
use super::{
    AaaMode, AddTarget, Command, Context, Fold, Motion, PaneTarget, Place, PlaylistSpec, Quality,
    RateSpec, SearchDir, SeekSpec, SettingKey, SettingValue, ViewId, VolSpec,
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
    #[error("{0}")]
    Filter(#[from] crate::filter::FilterError),
}

/// Every verb `command` accepts, aliases included.
///
/// Completion needs an enumerable vocabulary and the parser's match arms are
/// not one. Rather than restructure the parser around a table, this list sits
/// beside it and a test scans the source to prove the two agree — a verb added
/// to one and not the other fails rather than silently going uncompletable.
pub const VERBS: &[&str] = &[
    "activate",
    "add",
    "add-playlist",
    "add-queue",
    "bind",
    "bottom",
    "catalog",
    "collect",
    "down",
    "export",
    "filter",
    "fold",
    "go",
    "place",
    "help",
    "freeze",
    "in",
    "next",
    "playing",
    "page-down",
    "mode",
    "seek-percent",
    "half-page-up",
    "half-page-down",
    "page-up",
    "pane-left",
    "pane-next",
    "pane-right",
    "pause",
    "play",
    "playlist",
    "prev",
    "q",
    "put",
    "put-before",
    "quit",
    "rate",
    "remove",
    "remove-to-end",
    "yank",
    "yank-to-end",
    "redo",
    "rescan",
    "rsearch",
    "search",
    "search-next",
    "nohlsearch",
    "search-prev",
    "seek",
    "set",
    "stop",
    "toggle",
    "top",
    "unbind",
    "undo",
    "up",
    "view",
    "view-last",
    "view-next",
    "view-prev",
    "vol",
];

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
        "up" => Command::Up,
        "down" => Command::Down,
        "page-up" => Command::PageUp,
        "page-down" => Command::PageDown,
        "seek-percent" => Command::SeekPercent,
        "half-page-up" => Command::HalfPageUp,
        "half-page-down" => Command::HalfPageDown,
        "top" => Command::Top,
        "bottom" => Command::Bottom,
        "activate" => Command::Activate,
        "fold" => {
            let word = rest.trim();
            if word.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "fold",
                    what: "one of open, close, toggle, open-deep, close-deep, all, none, more, less",
                });
            }
            Command::Fold(Fold::parse(word).ok_or_else(|| ParseError::Bad(word.into(), "fold"))?)
        }
        "go" => {
            let word = rest.trim();
            if word.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "go",
                    what: "one of next-sibling, prev-sibling, next-section, prev-section, parent, \
                           group-end, next-fold, prev-fold",
                });
            }
            Command::Go(Motion::parse(word).ok_or_else(|| ParseError::Bad(word.into(), "go"))?)
        }
        "mode" => {
            let word = rest.trim();
            if word.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "mode",
                    what: "normal or transport",
                });
            }
            Command::Mode(
                crate::cmd::Mode::parse(word)
                    .ok_or_else(|| ParseError::Bad(word.into(), "mode"))?,
            )
        }
        "place" => {
            let word = rest.trim();
            if word.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "place",
                    what: "one of top, centre, bottom",
                });
            }
            Command::Place(Place::parse(word).ok_or_else(|| ParseError::Bad(word.into(), "place"))?)
        }
        "playing" => Command::Playing,
        "remove" => Command::Remove,
        "remove-to-end" => Command::RemoveToEnd,
        "yank" => Command::Yank,
        "yank-to-end" => Command::YankToEnd,
        "put" => Command::Put { before: false },
        "put-before" => Command::Put { before: true },
        "pane-next" => Command::PaneNext,
        "pane-left" => Command::PaneLeft,
        "pane-right" => Command::PaneRight,
        "in" => {
            let (pane_word, cmd_text) = split_word(rest);
            let pane = PaneTarget::parse(pane_word)
                .ok_or_else(|| ParseError::Bad(pane_word.into(), "pane"))?;
            if cmd_text.trim().is_empty() {
                return Err(ParseError::Missing {
                    cmd: "in",
                    what: "a command to run",
                });
            }
            Command::In {
                pane,
                cmd: Box::new(command(cmd_text)?),
            }
        }

        // Playback. The short aliases are what a person types.
        "play" => Command::Play,
        "pause" => Command::Pause,
        "stop" => Command::Stop,
        "next" => Command::Next,
        "prev" => Command::Prev,
        "seek" => Command::Seek(seek_spec(rest)?),
        "vol" => Command::Vol(vol_spec(rest)?),

        // Shelf.
        "add" => Command::Add {
            target: AddTarget::Library,
            path: optional_path(rest),
        },
        "add-playlist" => Command::Add {
            target: AddTarget::Playlist,
            path: optional_path(rest),
        },
        "add-queue" => Command::Add {
            target: AddTarget::Queue,
            path: optional_path(rest),
        },
        "rescan" => Command::Rescan,
        "undo" => Command::Undo,
        "redo" => Command::Redo,

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
        // **One verb, dispatching on its argument** (#94). A bare number is
        // the numbered view it has always been; anything else composes. #97
        // removes the numbered views and the first case goes with them.
        "view" => match rest.parse::<u8>() {
            Ok(n) => Command::View(
                ViewId::from_number(n)
                    .ok_or_else(|| ParseError::Bad(rest.into(), "view number"))?,
            ),
            Err(_) => Command::Compose(compose_term(rest)?),
        },
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
        "nohlsearch" => Command::NoHighlight,
        "search-next" => Command::SearchNext,
        "search-prev" => Command::SearchPrev,
        "rate" => Command::Rate(rate_spec(rest)?),
        "catalog" => Command::Catalog(catalog_spec(rest)?),
        "playlist" => Command::Playlist(playlist_spec(rest)?),
        "export" => {
            if rest.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "export",
                    what: "a path to write",
                });
            }
            Command::Export(rest.to_string())
        }
        "collect" => {
            if rest.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "collect",
                    what: "a collection name",
                });
            }
            Command::Collect(rest.to_string())
        }
        "freeze" => Command::Freeze,
        "filter" => Command::Filter(if rest.is_empty() {
            None
        } else {
            Some(crate::filter::Filter::parse(rest)?)
        }),
        "help" => Command::Help,
        "quit" | "q" => Command::Quit,

        other => return Err(ParseError::UnknownCommand(other.to_string())),
    })
}

/// `all`, a list of names, or a run of `+name` / `-name` adjustments.
///
/// Mixing bare names with adjustments is rejected rather than guessed at:
/// `:catalog jazz -rock` could mean either "just jazz, minus rock" or "add
/// jazz, remove rock", and being wrong silently is worse than asking again.
/// `:view` with something other than a number after it.
///
/// ```text
/// :view                    take it apart
/// :view drop 2              drop the second term
/// :view genre="folk"       set it
/// :view +genre="jazz"      or       — widen
/// :view &year>=2015        and      — narrow
/// :view -genre="*xmas*"    and not  — exclude
/// ```
///
/// The sign is the operator and it comes first, because that is how the stack
/// reads back and a line somebody can read back is a line they can retype.
fn compose_term(rest: &str) -> Result<crate::cmd::ViewTerm, ParseError> {
    use crate::cmd::ViewTerm;
    use crate::model::view::Op;

    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(ViewTerm::Clear);
    }
    // **Dropping is spelled out**, because a bare number is already the
    // numbered view and reaching this function means it was not one. `:view 2`
    // switches to view 2 and always did; `:view drop 2` takes the second term
    // off. When #97 removes the numbered views the number is free again.
    if let Some(n) = rest.strip_prefix("drop") {
        let n = n.trim();
        return n
            .parse::<usize>()
            .map(ViewTerm::Drop)
            .map_err(|_| ParseError::Bad(n.into(), "term number"));
    }

    let (op, expr, first) = match rest.split_at(1) {
        ("+", tail) => (Op::Or, tail, false),
        ("&", tail) => (Op::And, tail, false),
        ("-", tail) => (Op::AndNot, tail, false),
        // No sign sets the view rather than adding to it, which is what
        // `:view dance` means in §2.
        _ => (Op::Or, rest, true),
    };
    let filter = crate::filter::Filter::parse(expr.trim())?;
    Ok(ViewTerm::Compose(op, Box::new(filter), first))
}

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
/// `open <name>`, `save <name>`, or `list`.
///
/// A bare name is **rejected** rather than treated as `open`. It was the
/// spelling for a day, and keeping it would mean a playlist called `save`
/// could never be opened — the ambiguity is small and permanent, and the error
/// names the replacement.
fn playlist_spec(rest: &str) -> Result<PlaylistSpec, ParseError> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(PlaylistSpec::List);
    }
    let (word, name) = match rest.split_once(char::is_whitespace) {
        Some((w, n)) => (w, n.trim()),
        None => (rest, ""),
    };
    let needs_name = |verb: &'static str| -> Result<String, ParseError> {
        if name.is_empty() {
            Err(ParseError::Missing {
                cmd: verb,
                what: "a playlist name",
            })
        } else {
            Ok(name.to_string())
        }
    };
    Ok(match word {
        "list" => PlaylistSpec::List,
        "open" => PlaylistSpec::Open(needs_name("playlist open")?),
        "new" => PlaylistSpec::New(needs_name("playlist new")?),
        "save" => PlaylistSpec::Save(needs_name("playlist save")?),
        other => {
            return Err(ParseError::Bad(
                other.into(),
                "playlist action — try `open`, `new`, `save`, or `list`",
            ));
        }
    })
}

/// `4`, `none`, or nothing at all.
///
/// A helper rather than a nested match inside `command`, like every other verb
/// that takes an argument — the test that keeps `VERBS` in step with the
/// parser reads that function's arms, and a nested match reads to it as more
/// verbs.
fn rate_spec(rest: &str) -> Result<RateSpec, ParseError> {
    Ok(match rest {
        "" => RateSpec::Report,
        "none" => RateSpec::Clear,
        // Zero is not a rating here either: `:rate 0` reads as setting a
        // value, and there is no such value. Same rule as `rating=0`.
        "0" => {
            return Err(ParseError::Bad(
                rest.into(),
                "rating — `rate none` clears it",
            ));
        }
        n => RateSpec::Set(
            n.parse::<u8>()
                .ok()
                .filter(|n| (1..=5).contains(n))
                .ok_or_else(|| ParseError::Bad(rest.into(), "rating, 1 to 5, or `none`"))?,
        ),
    })
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
        SettingKey::ReplayGain => SettingValue::ReplayGain(
            crate::cmd::ReplayGainMode::parse(text)
                .ok_or_else(|| ParseError::Bad(text.into(), "replaygain mode"))?,
        ),
        SettingKey::ResampleQuality => SettingValue::Quality(match text {
            "fast" => Quality::Fast,
            "balanced" => Quality::Balanced,
            "high" => Quality::High,
            _ => return Err(ParseError::Bad(text.into(), "resample quality")),
        }),
        // The one setting parsed as a number. Milliseconds, because that is
        // what it is — and a bad one names itself rather than falling through
        // to the boolean arm, where "300" would have been rejected as not a
        // boolean and said so.
        SettingKey::WhichKeyDelay => SettingValue::Millis(
            text.parse::<u16>()
                .map_err(|_| ParseError::Bad(text.into(), "milliseconds"))?,
        ),
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
        Command::Up => "up".into(),
        Command::Down => "down".into(),
        Command::Mode(m) => format!("mode {}", m.name()),
        Command::SeekPercent => "seek-percent".into(),
        Command::PageUp => "page-up".into(),
        Command::PageDown => "page-down".into(),
        Command::HalfPageUp => "half-page-up".into(),
        Command::HalfPageDown => "half-page-down".into(),
        Command::Top => "top".into(),
        Command::Bottom => "bottom".into(),
        Command::Activate => "activate".into(),
        Command::Fold(f) => format!("fold {}", f.name()),
        Command::Place(p) => format!("place {}", p.name()),
        Command::Go(m) => format!("go {}", m.name()),
        Command::Playing => "playing".into(),
        Command::Remove => "remove".into(),
        Command::RemoveToEnd => "remove-to-end".into(),
        Command::Yank => "yank".into(),
        Command::YankToEnd => "yank-to-end".into(),
        Command::Put { before: false } => "put".into(),
        Command::Put { before: true } => "put-before".into(),
        Command::PaneNext => "pane-next".into(),
        Command::PaneLeft => "pane-left".into(),
        Command::PaneRight => "pane-right".into(),
        Command::In { pane, cmd } => format!("in {} {}", pane.name(), format(cmd)),
        Command::Play => "play".into(),
        Command::Pause => "pause".into(),
        Command::Stop => "stop".into(),
        Command::Next => "next".into(),
        Command::Prev => "prev".into(),
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
        Command::Rescan => "rescan".into(),
        Command::Undo => "undo".into(),
        Command::Redo => "redo".into(),
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
        Command::Rate(RateSpec::Report) => "rate".into(),
        Command::Rate(RateSpec::Clear) => "rate none".into(),
        Command::Rate(RateSpec::Set(n)) => format!("rate {n}"),
        Command::Playlist(spec) => match spec {
            PlaylistSpec::List => "playlist list".into(),
            PlaylistSpec::Open(n) => format!("playlist open {n}"),
            PlaylistSpec::Save(n) => format!("playlist save {n}"),
            PlaylistSpec::New(n) => format!("playlist new {n}"),
        },
        Command::Collect(name) => format!("collect {name}"),
        Command::Export(path) => format!("export {path}"),
        Command::Freeze => "freeze".into(),
        // Round-trips through `command`, which is what `:bind` shown in the
        // settings view depends on.
        Command::Compose(crate::cmd::ViewTerm::Clear) => "view".into(),
        Command::Compose(crate::cmd::ViewTerm::Drop(n)) => format!("view drop {n}"),
        Command::Compose(crate::cmd::ViewTerm::Compose(op, f, first)) => {
            let sign = if *first {
                ""
            } else {
                match op {
                    crate::model::view::Op::Or => "+",
                    crate::model::view::Op::And => "&",
                    crate::model::view::Op::AndNot => "-",
                }
            };
            format!("view {sign}{}", f.source)
        }
        Command::Filter(None) => "filter".into(),
        Command::Filter(Some(f)) => format!("filter {}", f.source),
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
        Command::NoHighlight => "nohlsearch".into(),
        Command::SearchNext => "search-next".into(),
        Command::SearchPrev => "search-prev".into(),
        Command::Help => "help".into(),
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
            ("play", "play"),
            ("pause", "pause"),
            ("stop", "stop"),
            ("next", "next"),
            ("prev", "prev"),
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
    fn replaygain_mode_parses() {
        use crate::cmd::ReplayGainMode;
        for (text, want) in [
            ("off", ReplayGainMode::Off),
            ("track", ReplayGainMode::Track),
            ("album", ReplayGainMode::Album),
            ("auto", ReplayGainMode::Auto),
        ] {
            assert_eq!(
                command(&format!("set replaygain={text}")).unwrap(),
                Command::Set {
                    key: SettingKey::ReplayGain,
                    value: SettingValue::ReplayGain(want),
                }
            );
        }
        assert!(command("set replaygain=loud").is_err());
    }

    /// A rating is 1 to 5 or `none` — the command language has no zero, just
    /// as the filter language has none.
    #[test]
    fn rate_takes_one_to_five_or_none() {
        assert_eq!(command("rate").unwrap(), Command::Rate(RateSpec::Report));
        assert_eq!(command("freeze").unwrap(), Command::Freeze);
        assert_eq!(
            command("rate none").unwrap(),
            Command::Rate(RateSpec::Clear)
        );
        assert_eq!(command("rate 1").unwrap(), Command::Rate(RateSpec::Set(1)));
        assert_eq!(command("rate 5").unwrap(), Command::Rate(RateSpec::Set(5)));

        // Zero is refused, and the message says what to write instead.
        let e = command("rate 0").unwrap_err().to_string();
        assert!(e.contains("rate none"), "{e}");

        assert!(command("rate 6").is_err());
        assert!(command("rate -1").is_err());
        assert!(command("rate lots").is_err());
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
        let got = command("bind common ^n next").unwrap();
        assert_eq!(
            got,
            Command::Bind {
                ctx: Context::Common,
                key: vec![keys::Key::ctrl(KeyCode::Char('n'))],
                cmd: Box::new(Command::Next),
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
        // Built from `Fold::ALL` rather than listed, so a new fold form is
        // covered the moment it exists.
        let mut samples: Vec<Command> = Fold::ALL.iter().copied().map(Command::Fold).collect();
        samples.extend(Place::ALL.iter().copied().map(Command::Place));
        samples.extend(crate::cmd::Mode::ALL.iter().copied().map(Command::Mode));
        samples.push(Command::SeekPercent);
        samples.extend(Motion::ALL.iter().copied().map(Command::Go));
        samples.push(Command::Playing);
        samples.extend(vec![
            Command::Up,
            Command::Down,
            Command::PageUp,
            Command::PageDown,
            Command::HalfPageUp,
            Command::HalfPageDown,
            Command::Top,
            Command::Bottom,
            Command::Activate,
            Command::Remove,
            Command::PaneNext,
            Command::PaneLeft,
            Command::PaneRight,
            Command::In {
                pane: PaneTarget::Tree,
                cmd: Box::new(Command::Down),
            },
            Command::Play,
            Command::Pause,
            Command::Stop,
            Command::Next,
            Command::Prev,
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
            Command::Rescan,
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
                cmd: Box::new(Command::Next),
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
            Command::View(ViewId::Guide),
            Command::Search {
                pattern: "burial".into(),
                dir: SearchDir::Forward,
            },
            Command::Search {
                pattern: "burial".into(),
                dir: SearchDir::Backward,
            },
            Command::SearchNext,
            Command::NoHighlight,
            Command::SearchPrev,
            Command::Quit,
        ]);

        for cmd in samples {
            let text = format(&cmd);
            let back = command(&text)
                .unwrap_or_else(|e| panic!("{cmd:?} formatted as {text:?} and failed: {e}"));
            assert_eq!(back, cmd, "{text:?} did not round trip");
        }
    }

    /// The vocabulary table and the parser must not drift. Scanning the source
    /// is blunt, but it is the only way to catch a verb added to the match and
    /// forgotten in `VERBS`, which would silently make it uncompletable.
    #[test]
    fn the_verb_table_matches_the_parser() {
        let src = include_str!("parse.rs");
        let body = &src[src.find("pub fn command(").unwrap()..src.find("fn split_word(").unwrap()];

        let mut found: Vec<String> = Vec::new();
        for line in body.lines() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('"') || !line.contains("=>") {
                continue;
            }
            let head = &trimmed[..trimmed.find("=>").unwrap()];
            for part in head.split('|') {
                let part = part.trim().trim_matches('"');
                if !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
                {
                    found.push(part.to_string());
                }
            }
        }
        found.sort();
        found.dedup();

        let mut listed: Vec<String> = VERBS.iter().map(|s| s.to_string()).collect();
        listed.sort();

        let missing: Vec<&String> = found.iter().filter(|v| !listed.contains(v)).collect();
        let extra: Vec<&String> = listed.iter().filter(|v| !found.contains(v)).collect();
        assert!(
            missing.is_empty(),
            "in the parser but not VERBS: {missing:?}"
        );
        assert!(extra.is_empty(), "in VERBS but not the parser: {extra:?}");
    }

    /// And every listed verb must actually parse, with an argument where one
    /// is required.
    #[test]
    fn every_listed_verb_parses() {
        for verb in VERBS {
            let line = match *verb {
                "seek" => "seek +10".to_string(),
                "vol" | "volume" => "vol 50".to_string(),
                "set" => "set shuffle=true".to_string(),
                "toggle" => "toggle shuffle".to_string(),
                "bind" => "bind common x quit".to_string(),
                "unbind" => "unbind common x".to_string(),
                "view" => "view 1".to_string(),
                "catalog" => format!("{verb} all"),
                "collect" => "collect Dance".to_string(),
                "export" => "export /tmp/x.m3u8".to_string(),
                "playlist" => "playlist open fresh".to_string(),
                "filter" => "filter year>2000".to_string(),
                "rate" => "rate 4".to_string(),
                "fold" => "fold toggle".to_string(),
                "place" => "place centre".to_string(),
                "mode" => "mode transport".to_string(),
                "go" => "go parent".to_string(),
                "in" => "in tree down".to_string(),
                "search" | "rsearch" => format!("{verb} text"),
                other => other.to_string(),
            };
            command(&line).unwrap_or_else(|e| panic!("{verb:?} does not parse: {e}"));
        }
    }

    #[test]
    fn playlist_takes_subcommands() {
        assert_eq!(
            command("playlist open fresh").unwrap(),
            Command::Playlist(PlaylistSpec::Open("fresh".into()))
        );
        assert_eq!(
            command("playlist save late night").unwrap(),
            Command::Playlist(PlaylistSpec::Save("late night".into())),
            "a name keeps its spaces"
        );
        assert_eq!(
            command("playlist list").unwrap(),
            Command::Playlist(PlaylistSpec::List)
        );
        // A bare `:playlist` is the listing — the useful thing to do with no
        // argument, rather than an error.
        assert_eq!(
            command("playlist").unwrap(),
            Command::Playlist(PlaylistSpec::List)
        );
    }

    /// A bare name was the spelling for a day (#15). Keeping it would mean a
    /// playlist called `save` could never be opened, so it is rejected — and
    /// the error names the replacement.
    #[test]
    fn a_bare_playlist_name_is_rejected_by_name() {
        let e = command("playlist fresh").unwrap_err();
        assert!(e.to_string().contains("open"), "{e}");
        assert!(
            matches!(command("playlist open"), Err(ParseError::Missing { .. })),
            "open needs a name"
        );
        assert!(matches!(
            command("playlist save"),
            Err(ParseError::Missing { .. })
        ));
    }

    #[test]
    fn unknown_commands_say_so() {
        assert_eq!(
            command("frobnicate").unwrap_err(),
            ParseError::UnknownCommand("frobnicate".into())
        );
    }
}
