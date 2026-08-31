//! The display format language (DESIGN §7).
//!
//! `"{artist} — {title}"`, with `[…]` groups that vanish when a field inside
//! them is missing. A literal delimiter is backslash-escaped — `\[`, `\{` —
//! rather than doubled, because `]]` cannot be told apart from two nested
//! groups closing together.
//!
//! **Parsed once, rendered per frame.** The now-playing line is drawn at the UI
//! tick rate, so a [`Template`] holds the parsed atoms and `render` does no
//! parsing at all. Bad templates are rejected at config-load time, where the
//! error can name the line it came from.

use std::fmt::Write as _;

use crate::store::Track;

/// One field of a track that a template can name.
///
/// Named rather than single-letter (`%a`): the alphabet runs out, and fields
/// worth adding later — `play_count`, `rating` — have no sensible letter left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Artist,
    AlbumArtist,
    /// The artist a track files under: album artist, or artist. What the
    /// library tree groups by.
    FileUnder,
    Album,
    Title,
    /// Title, falling back to the file name — never empty.
    DisplayTitle,
    Track,
    Disc,
    Year,
    Genre,
    /// The record label (`TPUB`), which is where a label sampler's imprint
    /// belongs — not in `albumartist`, where it looks like an artist.
    Label,
    Duration,
    Bitrate,
    SampleRate,
    Channels,
    Path,
    FileName,
}

/// Whether a field compares as text or as a number. Only the filter language
/// (§7) cares, but it lives here because the field vocabulary is shared: one
/// place to add `play_count`, and both languages get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
}

impl Field {
    pub fn kind(self) -> Kind {
        match self {
            Field::Artist
            | Field::AlbumArtist
            | Field::FileUnder
            | Field::Album
            | Field::Title
            | Field::DisplayTitle
            | Field::Genre
            | Field::Label
            | Field::Path
            | Field::FileName => Kind::Text,
            Field::Track
            | Field::Disc
            | Field::Year
            | Field::Duration
            | Field::Bitrate
            | Field::SampleRate
            | Field::Channels => Kind::Number,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "artist" => Field::Artist,
            "albumartist" => Field::AlbumArtist,
            "file_under" => Field::FileUnder,
            "album" => Field::Album,
            "title" => Field::Title,
            "display_title" => Field::DisplayTitle,
            "track" => Field::Track,
            "disc" => Field::Disc,
            "year" => Field::Year,
            "genre" => Field::Genre,
            "label" => Field::Label,
            "duration" => Field::Duration,
            "bitrate" => Field::Bitrate,
            "samplerate" => Field::SampleRate,
            "channels" => Field::Channels,
            "path" => Field::Path,
            "filename" => Field::FileName,
            _ => return None,
        })
    }

    /// Every name, for error messages and completion.
    pub const NAMES: &'static [&'static str] = &[
        "album",
        "albumartist",
        "artist",
        "bitrate",
        "channels",
        "disc",
        "display_title",
        "duration",
        "file_under",
        "filename",
        "genre",
        "label",
        "path",
        "samplerate",
        "title",
        "track",
        "year",
    ];

    /// The field's value, or `None` when the track does not have it.
    ///
    /// `None` is what makes a `[…]` group disappear, so the fields that always
    /// have a value — `display_title`, `file_under` — deliberately never
    /// return it.
    fn value(self, t: &Track) -> Option<String> {
        Some(match self {
            Field::Artist => t.artist.clone()?,
            Field::AlbumArtist => t.albumartist.clone()?,
            Field::FileUnder => t.file_under().to_string(),
            Field::Album => t.album.clone()?,
            Field::Title => t.title.clone()?,
            Field::DisplayTitle => t.display_title().to_string(),
            Field::Track => t.tracknumber?.to_string(),
            Field::Disc => t.discnumber?.to_string(),
            Field::Year => t.date?.to_string(),
            Field::Genre => t.genre.clone()?,
            Field::Label => t.label.clone()?,
            Field::Duration => {
                let secs = t.duration.as_secs();
                format!("{}:{:02}", secs / 60, secs % 60)
            }
            Field::Bitrate => t.bitrate?.to_string(),
            Field::SampleRate => t.samplerate?.to_string(),
            Field::Channels => t.channels?.to_string(),
            Field::Path => t.path.display().to_string(),
            Field::FileName => t
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())?,
        })
    }
}

/// How a field is sized. `{track:02}`, `{artist:20}`, `{duration:>8}`,
/// `{title:.30}`, `{title:10.30}`.
///
/// A minimum pads, a maximum truncates, and **the maximum always wins**: the
/// value is padded to `min` and then cut to `max`, which is foobar2000's
/// `$padcut` and the only rule that stays predictable when both are given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pad {
    align: Align,
    min: Option<usize>,
    /// Truncation is a plain cut, with no ellipsis. An ellipsis would be
    /// friendlier and would make the width arithmetic depend on whether the
    /// cut happened, which is worse in a status line that is already tight.
    max: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Align {
    /// Padded on the right, so the text is left-aligned.
    #[default]
    Left,
    /// Padded on the left with spaces.
    Right,
    /// Padded on the left with zeros — `{track:02}`, the reason this exists.
    Zero,
}

impl Pad {
    fn parse(spec: &str) -> Option<Self> {
        if spec.is_empty() {
            return Some(Pad::default());
        }
        let (align, rest) = match spec.strip_prefix('>') {
            Some(r) => (Align::Right, r),
            None => match spec.strip_prefix('0') {
                Some(r) => (Align::Zero, r),
                None => (Align::Left, spec),
            },
        };
        let (min_text, max_text) = match rest.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (rest, None),
        };
        let min = if min_text.is_empty() {
            None
        } else {
            Some(min_text.parse().ok()?)
        };
        let max = match max_text {
            Some(t) => Some(t.parse().ok()?),
            None => None,
        };
        // A spec that sizes nothing is a typo, not a rule — `{track:0}` and
        // `{title:.}` should say so rather than silently doing nothing.
        if min.is_none() && max.is_none() {
            return None;
        }
        Some(Pad { align, min, max })
    }

    fn apply(self, s: &str) -> String {
        let mut out = s.to_string();
        if let Some(min) = self.min {
            let len = out.chars().count();
            if len < min {
                let fill = min - len;
                out = match self.align {
                    Align::Left => format!("{out}{}", " ".repeat(fill)),
                    Align::Right => format!("{}{out}", " ".repeat(fill)),
                    Align::Zero => format!("{}{out}", "0".repeat(fill)),
                };
            }
        }
        if let Some(max) = self.max {
            // By characters, not bytes: cutting a multi-byte character in half
            // would produce something that is not a string.
            if out.chars().count() > max {
                out = out.chars().take(max).collect();
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Atom {
    Literal(String),
    Field {
        field: Field,
        pad: Pad,
    },
    /// Emitted only if every field inside it has a value. This is what lets one
    /// template cope with tracks that are tagged differently, without a
    /// conditional in the caller.
    Group(Vec<Atom>),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FormatError {
    #[error("unknown field {name:?} — try one of: {}", Field::NAMES.join(", "))]
    UnknownField { name: String },
    #[error("bad width {0:?} — expected a number, optionally prefixed with 0 or >")]
    BadPad(String),
    #[error("unclosed {0}")]
    Unclosed(&'static str),
    #[error("stray backslash before {0:?} — escapable characters are {{ }} [ ] and \\")]
    BadEscape(char),
    #[error("a backslash at the end of the template escapes nothing")]
    TrailingEscape,
    #[error("unexpected {0} — a literal one must be doubled")]
    Unexpected(char),
}

/// A parsed template. Cheap to render, so build one at config load and keep it.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    atoms: Vec<Atom>,
}

impl Default for Template {
    /// What the status line showed before it was configurable.
    fn default() -> Self {
        Self::parse("{file_under} — {display_title}").expect("the default template must parse")
    }
}

impl Template {
    /// The transport panel's headline (#34).
    ///
    /// Richer than the status line's, because the panel has a whole line
    /// where the status line has whatever is left over — which is the reason
    /// the panel exists. Every optional part is bracketed, so a track with
    /// nothing but a filename still reads as one line rather than as dangling
    /// punctuation.
    pub fn transport_default() -> Self {
        Self::parse("{file_under} — {display_title}[  ·  {album}][  ·  {year}]")
            .expect("the transport template must parse")
    }

    pub fn parse(source: &str) -> Result<Self, FormatError> {
        let mut chars = source.chars().peekable();
        let atoms = parse_atoms(&mut chars, false)?;
        Ok(Template { atoms })
    }

    pub fn render(&self, track: &Track) -> String {
        let mut out = String::new();
        render_atoms(&self.atoms, track, &mut out);
        out
    }
}

fn parse_atoms(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    in_group: bool,
) -> Result<Vec<Atom>, FormatError> {
    let mut atoms = Vec::new();
    let mut literal = String::new();

    macro_rules! flush {
        () => {
            if !literal.is_empty() {
                atoms.push(Atom::Literal(std::mem::take(&mut literal)));
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            // Backslash, not doubling. Doubling is what Rust's format strings
            // use and it cannot work here: `]]` is ambiguous between a literal
            // `]` and two nested groups closing together, and Rust only avoids
            // that by having nothing that nests.
            '\\' => match chars.next() {
                Some(c @ ('{' | '}' | '[' | ']' | '\\')) => literal.push(c),
                // Strict, because a stray backslash in a template is far more
                // likely a mistake than an intention, and this is checked at
                // config load where saying so costs nothing.
                Some(other) => return Err(FormatError::BadEscape(other)),
                None => return Err(FormatError::TrailingEscape),
            },
            '{' => {
                flush!();
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(FormatError::Unclosed("{")),
                    }
                }
                let (name, spec) = match name.split_once(':') {
                    Some((n, s)) => (n, s),
                    None => (name.as_str(), ""),
                };
                let field = Field::parse(name.trim())
                    .ok_or_else(|| FormatError::UnknownField { name: name.into() })?;
                let pad = Pad::parse(spec).ok_or_else(|| FormatError::BadPad(spec.into()))?;
                atoms.push(Atom::Field { field, pad });
            }
            '[' => {
                flush!();
                atoms.push(Atom::Group(parse_atoms(chars, true)?));
            }
            ']' if in_group => {
                flush!();
                return Ok(atoms);
            }
            ']' => return Err(FormatError::Unexpected(']')),
            '}' => return Err(FormatError::Unexpected('}')),
            c => literal.push(c),
        }
    }
    if in_group {
        return Err(FormatError::Unclosed("["));
    }
    flush!();
    Ok(atoms)
}

/// Render into `out`, returning whether every field encountered had a value.
fn render_atoms(atoms: &[Atom], track: &Track, out: &mut String) -> bool {
    let mut complete = true;
    for atom in atoms {
        match atom {
            Atom::Literal(text) => out.push_str(text),
            Atom::Field { field, pad } => match field.value(track) {
                Some(v) => {
                    let _ = write!(out, "{}", pad.apply(&v));
                }
                None => complete = false,
            },
            Atom::Group(inner) => {
                // Rendered aside, so a group with a missing field leaves
                // nothing behind — including its literal text, which is the
                // whole point of grouping.
                let mut nested = String::new();
                if render_atoms(inner, track, &mut nested) {
                    out.push_str(&nested);
                }
            }
        }
    }
    complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn full() -> Track {
        Track {
            path: PathBuf::from("/m/Sierra Ferrell/Long Time Coming/01 The Sea.mp3"),
            artist: Some("Sierra Ferrell".into()),
            albumartist: Some("Sierra Ferrell".into()),
            album: Some("Long Time Coming".into()),
            title: Some("The Sea".into()),
            tracknumber: Some(1),
            discnumber: Some(1),
            date: Some(2021),
            genre: Some("folk".into()),
            duration: Duration::from_secs(221),
            bitrate: Some(320),
            ..Default::default()
        }
    }

    fn render(source: &str, track: &Track) -> String {
        Template::parse(source).unwrap().render(track)
    }

    #[test]
    fn fields_and_literals() {
        assert_eq!(
            render("{artist} — {title}", &full()),
            "Sierra Ferrell — The Sea"
        );
        assert_eq!(render("{year} {genre}", &full()), "2021 folk");
        assert_eq!(render("plain text", &full()), "plain text");
    }

    /// `{track:02}` is the reason padding exists at all.
    #[test]
    fn padding() {
        assert_eq!(render("{track:02}", &full()), "01");
        assert_eq!(render(r"\[{duration:>7}\]", &full()), "[   3:41]");
        assert_eq!(render("{genre:8}|", &full()), "folk    |");
        // Already wide enough: a minimum is never a truncation.
        assert_eq!(render("{artist:3}", &full()), "Sierra Ferrell");
    }

    /// The status line is one line, so a long template is otherwise clipped by
    /// the terminal with no say in where. foobar2000 spells this `$padcut`.
    #[test]
    fn truncation() {
        assert_eq!(render("{artist:.6}", &full()), "Sierra");
        assert_eq!(render("{artist:.99}", &full()), "Sierra Ferrell");
        // Padded to the minimum, then cut to the maximum: `$padcut`.
        assert_eq!(render("{genre:.2}", &full()), "fo");
        assert_eq!(render("{genre:8.8}|", &full()), "folk    |");
        assert_eq!(render("{artist:8.8}|", &full()), "Sierra F|");
        // The maximum wins when the two disagree.
        assert_eq!(render("{artist:20.4}", &full()), "Sier");
        // Alignment still applies to the minimum.
        assert_eq!(render("{genre:>8.8}|", &full()), "    folk|");
    }

    /// Cutting by characters, not bytes — half a multi-byte character is not
    /// a string, and this library is full of them.
    #[test]
    fn truncation_is_character_safe() {
        let mut t = full();
        t.artist = Some("Christian Löffler".into());
        assert_eq!(render("{artist:.11}", &t), "Christian L");
        assert_eq!(render("{artist:.12}", &t), "Christian Lö");
        t.artist = Some("日本語のタイトル".into());
        assert_eq!(render("{artist:.3}", &t), "日本語");
    }

    /// A spec that sizes nothing is a typo, not a rule.
    #[test]
    fn an_empty_size_spec_is_rejected() {
        assert_eq!(
            Template::parse("{track:0}"),
            Err(FormatError::BadPad("0".into()))
        );
        assert_eq!(
            Template::parse("{title:.}"),
            Err(FormatError::BadPad(".".into()))
        );
        assert_eq!(
            Template::parse("{title:>}"),
            Err(FormatError::BadPad(">".into()))
        );
    }

    /// A group vanishes entirely — including its literal text — when a field
    /// inside it has no value. Without that, a missing album leaves a stray
    /// " — " behind, which is the whole reason for grouping.
    #[test]
    fn a_group_with_a_missing_field_leaves_nothing_behind() {
        let mut bare = full();
        bare.album = None;
        bare.date = None;

        assert_eq!(
            render("{artist}[ — {album}][ ({year})]", &full()),
            "Sierra Ferrell — Long Time Coming (2021)"
        );
        assert_eq!(
            render("{artist}[ — {album}][ ({year})]", &bare),
            "Sierra Ferrell",
            "the separators should have gone with the fields"
        );
    }

    /// A bare field outside a group leaves a hole rather than the whole line,
    /// so one missing tag does not blank the status line.
    #[test]
    fn a_missing_field_outside_a_group_is_simply_empty() {
        let mut bare = full();
        bare.album = None;
        assert_eq!(
            render("{artist}/{album}/{title}", &bare),
            "Sierra Ferrell//The Sea"
        );
    }

    /// The fields with a fallback never report missing, so a group built on
    /// them cannot vanish — which is what makes the default template safe.
    #[test]
    fn the_fallback_fields_always_have_a_value() {
        let empty = Track {
            path: PathBuf::from("/m/whatever.mp3"),
            ..Default::default()
        };
        assert_eq!(render("{display_title}", &empty), "whatever.mp3");
        assert!(!render("{file_under}", &empty).is_empty());
        assert_eq!(
            Template::default().render(&empty),
            format!("{} — whatever.mp3", empty.file_under())
        );
    }

    /// Backslash rather than doubling — see the note on `parse_atoms`.
    #[test]
    fn a_backslash_writes_a_literal_delimiter() {
        assert_eq!(render(r"\{{artist}\}", &full()), "{Sierra Ferrell}");
        assert_eq!(render(r"\[{title}\]", &full()), "[The Sea]");
        assert_eq!(render(r"a\\b", &full()), r"a\b");
        assert_eq!(Template::parse(r"\q"), Err(FormatError::BadEscape('q')));
        assert_eq!(Template::parse("x\\"), Err(FormatError::TrailingEscape));
    }

    #[test]
    fn bad_templates_are_rejected_with_something_useful() {
        assert_eq!(
            Template::parse("{nosuchfield}"),
            Err(FormatError::UnknownField {
                name: "nosuchfield".into()
            })
        );
        assert_eq!(Template::parse("{artist"), Err(FormatError::Unclosed("{")));
        assert_eq!(
            Template::parse("[{artist}"),
            Err(FormatError::Unclosed("["))
        );
        assert_eq!(Template::parse("a]b"), Err(FormatError::Unexpected(']')));
        assert_eq!(
            Template::parse("{track:x}"),
            Err(FormatError::BadPad("x".into()))
        );
        // The error names the field, so a typo in a config says which one.
        let msg = Template::parse("{titel}").unwrap_err().to_string();
        assert!(msg.contains("titel") && msg.contains("title"), "{msg}");
    }

    /// Nesting is not special-cased anywhere, so it has to be checked: an
    /// inner group that vanishes must not take the outer one with it.
    #[test]
    fn groups_nest() {
        let mut no_year = full();
        no_year.date = None;
        assert_eq!(
            render("[{album}[ ({year})]]", &no_year),
            "Long Time Coming",
            "a missing inner group should not empty the outer one"
        );
        no_year.album = None;
        assert_eq!(render("[{album}[ ({year})]]", &no_year), "");
    }
}
