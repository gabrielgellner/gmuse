//! A line-based driver for `PlayerHandle`, standing in for the UI until M4.
//!
//! DESIGN §6 makes parsing the boundary of the application: a line becomes a
//! typed command here, and nothing downstream ever sees a raw string. The real
//! command layer in M6 replaces `Line` with the full `Command` enum; keeping
//! the parse pure and tested now means that swap is mechanical.

use std::path::PathBuf;
use std::time::Duration;

use crate::audio::SeekTo;

/// A parsed input line.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Load(PathBuf),
    Play,
    Pause,
    Toggle,
    Stop,
    Seek(SeekTo),
    Volume(f32),
    Status,
    Help,
    Quit,
    /// Blank input: do nothing, without complaining.
    Empty,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("unknown command {0:?} — try 'help'")]
    Unknown(String),
    #[error("{cmd} needs an argument")]
    MissingArg { cmd: &'static str },
    #[error("{0:?} is not a number")]
    NotANumber(String),
}

/// Parse one input line.
///
/// Times are seconds. `seek 90` is absolute; `seek +10` and `seek -10` are
/// relative, and the sign is what distinguishes them — so `seek +0` is a
/// no-op rather than a jump to the start.
pub fn parse(line: &str) -> Result<Line, ParseError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(Line::Empty);
    }

    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };

    match word.to_ascii_lowercase().as_str() {
        "load" | "open" | "l" => {
            if rest.is_empty() {
                return Err(ParseError::MissingArg { cmd: "load" });
            }
            // Tolerate a quoted or backslash-escaped path, since shell-style
            // completion is the likely way a path gets in here.
            Ok(Line::Load(PathBuf::from(unquote(rest))))
        }
        "play" | "p" => Ok(Line::Play),
        "pause" => Ok(Line::Pause),
        "toggle" | "t" | " " => Ok(Line::Toggle),
        "stop" | "s" => Ok(Line::Stop),
        "seek" | "k" => {
            if rest.is_empty() {
                return Err(ParseError::MissingArg { cmd: "seek" });
            }
            let relative = rest.starts_with('+') || rest.starts_with('-');
            let secs: f64 = rest
                .parse()
                .map_err(|_| ParseError::NotANumber(rest.to_string()))?;
            Ok(Line::Seek(if relative {
                SeekTo::Relative(secs)
            } else {
                SeekTo::Absolute(Duration::from_secs_f64(secs.max(0.0)))
            }))
        }
        "vol" | "volume" | "v" => {
            if rest.is_empty() {
                return Err(ParseError::MissingArg { cmd: "vol" });
            }
            let v: f32 = rest
                .parse()
                .map_err(|_| ParseError::NotANumber(rest.to_string()))?;
            Ok(Line::Volume(v))
        }
        "status" | "st" => Ok(Line::Status),
        "help" | "h" | "?" => Ok(Line::Help),
        "quit" | "q" | "exit" => Ok(Line::Quit),
        other => Err(ParseError::Unknown(other.to_string())),
    }
}

/// Strip matching quotes, or unescape `\ ` from shell completion.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.replace("\\ ", " ")
}

pub const HELP: &str = "\
commands:
  load <path>     load a file and start playing   (l, open)
  play            resume                          (p)
  pause           pause
  toggle          play/pause                      (t)
  stop            stop and rewind                 (s)
  seek 90         seek to 90s                     (k)
  seek +10        seek forwards 10s
  seek -10        seek backwards 10s
  vol 0.5         set volume, 0.0-2.0             (v)
  status          position, state, underruns      (st)
  help            this                            (h, ?)
  quit            exit                            (q)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_words() {
        assert_eq!(parse("play").unwrap(), Line::Play);
        assert_eq!(parse("  PAUSE  ").unwrap(), Line::Pause);
        assert_eq!(parse("q").unwrap(), Line::Quit);
        assert_eq!(parse("").unwrap(), Line::Empty);
        assert_eq!(parse("   ").unwrap(), Line::Empty);
    }

    #[test]
    fn seek_sign_selects_absolute_or_relative() {
        assert_eq!(
            parse("seek 90").unwrap(),
            Line::Seek(SeekTo::Absolute(Duration::from_secs(90)))
        );
        assert_eq!(
            parse("seek +10").unwrap(),
            Line::Seek(SeekTo::Relative(10.0))
        );
        assert_eq!(
            parse("seek -10").unwrap(),
            Line::Seek(SeekTo::Relative(-10.0))
        );
        // A signed zero stays relative, so it is a no-op rather than a rewind.
        assert_eq!(parse("seek +0").unwrap(), Line::Seek(SeekTo::Relative(0.0)));
        assert_eq!(
            parse("seek 0").unwrap(),
            Line::Seek(SeekTo::Absolute(Duration::ZERO))
        );
    }

    #[test]
    fn fractional_seek_and_volume() {
        assert_eq!(
            parse("seek 1.5").unwrap(),
            Line::Seek(SeekTo::Absolute(Duration::from_secs_f64(1.5)))
        );
        assert_eq!(parse("vol 0.25").unwrap(), Line::Volume(0.25));
    }

    #[test]
    fn paths_with_spaces() {
        assert_eq!(
            parse(r#"load "/a b/c d.mp3""#).unwrap(),
            Line::Load(PathBuf::from("/a b/c d.mp3"))
        );
        assert_eq!(
            parse(r"load /a\ b/c.mp3").unwrap(),
            Line::Load(PathBuf::from("/a b/c.mp3"))
        );
        // An unadorned path with spaces still works: everything after the
        // command word is the argument.
        assert_eq!(
            parse("load /a b/c.mp3").unwrap(),
            Line::Load(PathBuf::from("/a b/c.mp3"))
        );
    }

    #[test]
    fn errors_are_specific() {
        assert_eq!(
            parse("frobnicate").unwrap_err(),
            ParseError::Unknown("frobnicate".into())
        );
        assert_eq!(
            parse("seek").unwrap_err(),
            ParseError::MissingArg { cmd: "seek" }
        );
        assert_eq!(
            parse("seek soon").unwrap_err(),
            ParseError::NotANumber("soon".into())
        );
        assert_eq!(
            parse("load").unwrap_err(),
            ParseError::MissingArg { cmd: "load" }
        );
    }
}
