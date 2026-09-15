//! Turning a line of text into a [`Command`].
//!
//! One parser for the rc file, the `:` command line, and `:bind`'s nested
//! command. Hand-rolled recursive descent, per §7's "do not pull in a
//! parser-generator" — the grammar is a verb and some arguments.

use std::path::PathBuf;

use super::keys;
use super::{
    AddTarget, Command, Context, Fold, Motion, PaneTarget, Place, PlaylistSpec, Quality, RateSpec,
    Scope, SearchDir, SeekSpec, SettingKey, SettingValue, ViewId, VolSpec,
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
    #[error(
        "{0:?} on its own names nothing — a collection is `in={0:?}`, a saved view is `saved={0:?}`, a genre is `genre={0:?}`"
    )]
    Bare(String),
    #[error("{0}")]
    Key(#[from] keys::KeyParseError),
    #[error("{0}")]
    Filter(#[from] crate::query::filter::FilterError),
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
    "add-view",
    "add-playlist",
    "add-queue",
    "bind",
    "bottom",
    "clear-queue",
    "collect",
    "down",
    "edit",
    "export",
    "find",
    "only",
    "other-pane",
    "split",
    "vsplit",
    "filter",
    "fold",
    "go",
    "place",
    "help",
    "back",
    "info",
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
        "clear-queue" => Command::ClearQueue,
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
        // `query-language.md` §11's third tier. No argument: what it edits is
        // what is composed, which is the only thing it could sensibly mean.
        "edit" => Command::Edit,
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
        "add-view" => Command::Add {
            target: AddTarget::View,
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
        "playlist" => Command::Playlist(playlist_spec(rest)?),
        "find" => Command::Find(picker_source(rest)?),
        "split" => Command::Split(crate::ui::Axis::Horizontal),
        "vsplit" => Command::Split(crate::ui::Axis::Vertical),
        "only" => Command::Only,
        "other-pane" => Command::OtherPane,
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
        "freeze" => {
            if rest.is_empty() {
                return Err(ParseError::Missing {
                    cmd: "freeze",
                    what: "a collection name",
                });
            }
            Command::Freeze(rest.to_string())
        }
        "filter" => Command::Filter(if rest.is_empty() {
            None
        } else {
            Some(crate::query::filter::Filter::parse(rest)?)
        }),
        "help" => Command::Help,
        "quit" | "q" => Command::Quit,
        "info" => Command::Info(info_spec(rest)?),
        "back" => Command::Back,

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
/// One `:view` term, from the text after the verb.
///
/// Public because `view::Stack::parse` reads a saved definition back through
/// it — **one parser, so a saved view cannot mean something the line would
/// not** (#119).
pub fn view_term(rest: &str) -> Result<crate::cmd::ViewTerm, ParseError> {
    compose_term(rest)
}

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

    // **The space is load-bearing.** `saved="mornings"` starts with `save`,
    // so a bare prefix test read a term naming a view as the verb that saves
    // one — caught by the round-trip test, which is what it is for.
    if let Some(spec) = rest
        .strip_prefix("levels")
        .filter(|r| r.is_empty() || r.starts_with(' '))
    {
        return Ok(ViewTerm::Levels(levels_spec(spec.trim())?));
    }
    if let Some(name) = rest
        .strip_prefix("save ")
        .or_else(|| (rest.trim_end() == "save").then_some(""))
    {
        let name = name.trim();
        if name.is_empty() {
            return Err(ParseError::Missing {
                cmd: "view save",
                what: "a name",
            });
        }
        return Ok(ViewTerm::Save(name.to_string()));
    }

    let (op, expr, first) = match rest.split_at(1) {
        ("+", tail) => (Op::Or, tail, false),
        ("&", tail) => (Op::And, tail, false),
        ("-", tail) => (Op::AndNot, tail, false),
        // No sign sets the view rather than adding to it, which is what
        // `:view in="dance"` means in §2.
        _ => (Op::Or, rest, true),
    };
    Ok(ViewTerm::Compose(op, Box::new(source(expr.trim())?), first))
}

/// What `:info` was pointed at — #129.
///
/// **Through the grammar the `:` line already has.** `in=` and a facet key are
/// what a view term spells, and reusing [`source`] is what stops this becoming
/// a second dialect that has to be kept in step with the first.
fn info_spec(rest: &str) -> Result<Option<crate::cmd::Named>, ParseError> {
    use crate::cmd::Named;
    use crate::model::about::Kind;
    use crate::view::What;

    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(None);
    }
    // The exact form first: `recording:0192…` is not a filter term and would
    // only confuse the one below.
    if let Some(focus) = crate::model::focus::Focus::parse(rest) {
        return Ok(Some(Named::Exactly(focus)));
    }
    if let Some(name) = quoted_value(rest, "artist") {
        return Ok(Some(Named::Called(Kind::Artist, name)));
    }
    Ok(Some(match source(rest)? {
        What::Collection(name) => Named::Called(Kind::Collection, name),
        // A facet, on its own axis. `genre="Techno"` and `version="live"` are
        // the same shape and always were (#117).
        What::Where(filter) => {
            let (key, label) = facet_term(&filter.source)
                .ok_or_else(|| ParseError::Bad(rest.into(), "a thing to look up"))?;
            Named::Called(Kind::Concept(key), label)
        }
        // **A saved view is a definition, not a thing** — #96 said the same of
        // adding to one. What it *names* has a page; the name does not.
        What::Saved(name) => {
            return Err(ParseError::Bad(name, "a definition rather than a thing"));
        }
        What::Everything | What::Group(_) => {
            return Err(ParseError::Bad(rest.into(), "a thing to look up"));
        }
    }))
}

/// `genre="Techno"` split into its axis and its label.
fn facet_term(source: &str) -> Option<(&'static str, String)> {
    let (field, _) = source.split_once('=')?;
    let key = crate::model::facet::key_of(field.trim())?;
    let value = quoted_value(source, field.trim())?;
    (!value.is_empty()).then_some((key, value))
}

/// One term's source: a collection, a saved view, or a predicate.
///
/// **A bare word is refused rather than guessed at** (#119). `jazz` could name
/// a collection, a saved view or a genre concept, and the codebase already
/// settled that question for the filter language — `any` is an explicit
/// pseudo-field "so 'search everything' is explicit rather than a bare word
/// with implicit meaning". Three namespaces resolved by guessing would be that
/// rule broken in the place it matters most.
fn source(expr: &str) -> Result<crate::view::What, ParseError> {
    use crate::view::What;

    if let Some(name) = quoted_value(expr, "in") {
        return Ok(What::Collection(name));
    }
    if let Some(name) = quoted_value(expr, "saved") {
        return Ok(What::Saved(name));
    }
    // **`all` is a keyword, not a bare word**, because it is already the
    // written form of `What::Everything` — and without it the round trip was
    // broken where it matters most: `:view -in="Christmas"` normalises to
    // `all -in="Christmas"`, so *every* stack beginning with an exclusion
    // failed to parse back, and a saved view of one reported as missing.
    if expr == "all" {
        return Ok(What::Everything);
    }
    // A word on its own is not a field, and the filter parser's "unknown
    // field" would not say what the two ways of naming a group are.
    if !expr.is_empty()
        && expr
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ')
    {
        return Err(ParseError::Bare(expr.to_string()));
    }
    Ok(What::Where(Box::new(crate::query::filter::Filter::parse(
        expr,
    )?)))
}

/// `key="value"` or `key=value`, if this is that key.
fn quoted_value(expr: &str, key: &str) -> Option<String> {
    let rest = expr.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    Some(
        match rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            Some(inner) => inner.to_string(),
            None => rest.to_string(),
        },
    )
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

/// `outer,inner` — the two fields a tree is grouped by.
///
/// Empty is the default pair, which is `artist,album`: `:view levels` puts the
/// tree back rather than being an error, because that is what somebody means
/// by it.
pub fn levels_spec(rest: &str) -> Result<crate::shelf::Levels, ParseError> {
    if rest.is_empty() {
        return Ok(crate::shelf::Levels::default());
    }
    // **`flat` is a level list of none**, which is what view 2 was.
    if rest == "flat" {
        return Ok(crate::shelf::Levels::Flat);
    }
    let (outer, inner) = rest.split_once(',').unwrap_or((rest, "album"));
    Ok(crate::shelf::Levels::By {
        outer: level(outer)?,
        inner: level(inner)?,
    })
}

/// One level: a field, optionally at a depth in its hierarchy (#79, #174).
///
/// `genre@1` is the broad term and `genre@2` the specific one — what the
/// `Broad; Style` tag convention imitates.
///
/// **Two kinds of field have a graph to climb, and they are different
/// graphs.** A facet climbs the codelist; an artist climbs the monikers, so
/// `file_under@1` is the act behind the credit — AFX and Aphex Twin on one
/// shelf, Adele on her own because she needs no relation to have one. Nothing
/// else has either, and `year@2` is refused rather than silently ignored.
fn level(spec: &str) -> Result<crate::shelf::Level, ParseError> {
    use crate::query::format::Field;

    let spec = spec.trim();
    let (name, at) = match spec.split_once('@') {
        None => (spec, None),
        Some((name, depth)) => {
            let n = depth
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| {
                    ParseError::Bad(depth.trim().into(), "hierarchy level, 1 or more")
                })?;
            (name.trim(), Some(n))
        }
    };
    let field = Field::parse(name).ok_or_else(|| ParseError::Bad(name.into(), "field"))?;
    let climbable = matches!(
        field,
        Field::Facet(_) | Field::Artist | Field::AlbumArtist | Field::FileUnder
    );
    if at.is_some() && !climbable {
        return Err(ParseError::Bad(
            spec.into(),
            "level on a field with no hierarchy",
        ));
    }
    Ok(crate::shelf::Level { field, at })
}

/// Which picker: the library, or the groups you have made.
///
/// **A function rather than a nested match**, like `playlist_spec` — the verb
/// table's audit scans this file for match arms, and a sub-match inside the
/// dispatch reads as two more verbs.
fn picker_source(rest: &str) -> Result<crate::ui::picker::Source, ParseError> {
    match rest.trim() {
        "" | "tracks" => Ok(crate::ui::picker::Source::Tracks),
        "groups" => Ok(crate::ui::picker::Source::Groups),
        other => Err(ParseError::Bad(
            other.into(),
            "picker source — `tracks` or `groups`",
        )),
    }
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
        // **Through `Scope::parse`, not a second copy of it.** This was its own
        // three-arm match, which knew `all`, `artist` and `album` — and stopped
        // knowing the whole set the moment #234 added `outer` and `inner`. One
        // declaration is the fix; the round-trip test is what caught it.
        SettingKey::Scope => SettingValue::Scope(
            Scope::parse(text).ok_or_else(|| ParseError::Bad(text.into(), "scope"))?,
        ),
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
        Command::Edit => "edit".into(),
        Command::Info(None) => "info".into(),
        Command::Info(Some(crate::cmd::Named::Exactly(focus))) => format!("info {focus}"),
        Command::Info(Some(crate::cmd::Named::Called(kind, name))) => {
            use crate::model::about::Kind;
            let field = match kind {
                Kind::Artist => "artist",
                Kind::Collection => "in",
                Kind::Concept(key) => key,
            };
            format!("info {field}={name:?}")
        }
        Command::Back => "back".into(),
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
        Command::ClearQueue => "clear-queue".into(),
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
                AddTarget::View => "add-view",
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
        Command::Split(crate::ui::Axis::Horizontal) => "split".into(),
        Command::Split(crate::ui::Axis::Vertical) => "vsplit".into(),
        Command::Only => "only".into(),
        Command::OtherPane => "other-pane".into(),
        Command::Find(source) => match source {
            crate::ui::picker::Source::Tracks => "find".into(),
            crate::ui::picker::Source::Groups => "find groups".into(),
            // **Not a `:find` anybody types.** A handed-over set is opened by
            // `:info` finding several, so there is no line that produces one
            // and nothing for this to write back.
            crate::ui::picker::Source::Among(_) => "find".into(),
        },
        Command::Freeze(name) => format!("freeze {name}"),
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
            format!("view {sign}{}", crate::view::written(f))
        }
        Command::Compose(crate::cmd::ViewTerm::Save(name)) => format!("view save {name}"),
        Command::Compose(crate::cmd::ViewTerm::Levels(l)) => {
            format!("view levels {}", l.written())
        }
        Command::Filter(None) => "filter".into(),
        Command::Filter(Some(f)) => format!("filter {}", f.source),
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
    /// **`genre@1` parses and writes itself back** — #79's spelling, and the
    /// round trip `:view levels` rests on.
    #[test]
    fn a_level_may_name_a_depth_in_the_hierarchy() {
        let parsed = |s: &str| match super::compose_term(s) {
            Ok(crate::cmd::ViewTerm::Levels(l)) => Ok(l),
            Ok(other) => panic!("not a levels term: {other:?}"),
            Err(e) => Err(e),
        };

        let both = parsed("levels genre@1,genre@2").expect("it parses");
        assert_eq!(both.written(), "genre@1,genre@2", "it did not write back");
        assert!(both.outer_is_multi(), "a facet level is still multi-valued");

        assert_eq!(
            parsed("levels genre,album").expect("plain").written(),
            "genre,album",
            "a level with no depth is unchanged"
        );

        // **A depth on something with no hierarchy is refused at the parse**,
        // not silently ignored: nothing in the model climbs a year.
        assert!(parsed("levels year@2,album").is_err());
        // And a level number has to be one or more — `@0` names nothing.
        assert!(parsed("levels genre@0,album").is_err());
    }

    /// **The flag and the `:` line are one parser** — #182.
    ///
    /// `shelf tree --levels` exists so a level has a reading outside the thing
    /// being read. That is only worth having if the flag understands exactly
    /// what `:view levels` understands, and the way to guarantee it is for
    /// there to be one function rather than two that agree today.
    #[test]
    fn the_tree_flag_reads_levels_the_way_the_colon_line_does() {
        // What `shelf tree` with no flag gives: the browser's own default, so
        // the command with nothing said draws the tree as it is drawn.
        assert_eq!(
            super::levels_spec("").expect("empty is the default"),
            crate::shelf::Levels::default(),
        );

        // A depth survives the flag, which is the case the issue is about.
        assert_eq!(
            super::levels_spec("genre@1,album")
                .expect("a depth parses")
                .written(),
            "genre@1,album",
        );

        // And the flag refuses what the line refuses, rather than falling back
        // to the default and drawing a tree nobody asked for.
        assert!(super::levels_spec("year@2,album").is_err());
        assert!(super::levels_spec("nosuchfield,album").is_err());

        // `flat` parses — it is a real `Levels` — and `pair()` is `None`,
        // which is what the command turns into "that is `shelf query`"
        // rather than a tree of one level.
        assert!(
            super::levels_spec("flat")
                .expect("flat parses")
                .pair()
                .is_none()
        );
    }

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
        assert_eq!(
            command("freeze crate").unwrap(),
            Command::Freeze("crate".into())
        );
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
            command("set scope=album").unwrap(),
            Command::Set {
                key: SettingKey::Scope,
                value: SettingValue::Scope(Scope::Inner)
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
        assert!(command("set scope=true").is_err());
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
                key: SettingKey::Scope,
                value: SettingValue::Scope(Scope::Outer),
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
                "collect" => "collect Dance".to_string(),
                "freeze" => "freeze Dance".to_string(),
                "find" => "find groups".to_string(),
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
