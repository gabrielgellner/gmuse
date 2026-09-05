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
    /// A concept on one axis — `genre=techno`, `version=live`,
    /// `function=dance-floor`.
    ///
    /// **One variant for every axis** (#117), where `Genre` was one variant
    /// for one. The id is derived from the facet's key, so the parser resolves
    /// a name without a vocabulary to consult and two axes cannot collide.
    Facet(crate::model::id::Id<crate::model::entity::Facet>),
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
            | Field::Facet(_)
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
            // Any axis, resolved by key — so adding one costs a line in
            // `facet::ALL` and nothing here (#117).
            name if crate::model::facet::key_of(name).is_some() => {
                Field::Facet(crate::model::id::Id::of_key(name))
            }
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

    /// Every name a field can be written as, sorted.
    ///
    /// **Built rather than typed out**, so a new axis in `facet::ALL` reaches
    /// completion and the "unknown field" message with no second list to keep
    /// in step (#117).
    pub fn names() -> Vec<&'static str> {
        let mut out = Self::FIXED.to_vec();
        out.extend(crate::model::facet::ALL.iter().map(|(key, _)| *key));
        out.sort_unstable();
        out
    }

    /// The fields that are not an axis.
    const FIXED: &'static [&'static str] = &[
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
    /// **Asked of a row, not of a file** — issue #78.
    ///
    /// It took a `&Track` and read sixteen fields of it, which is why five of
    /// the player's remaining reaches into the store were this one function.
    /// A `Listed` answers fourteen of them; the two it cannot are the two a
    /// row must never carry, and `Template::parse` refuses those outright.
    fn value(self, r: &crate::model::rows::Listed<'_>) -> Option<String> {
        Some(match self {
            Field::Artist => r.artist?.to_string(),
            Field::AlbumArtist => r.album_artist?.to_string(),
            Field::FileUnder => r.file_under.to_string(),
            Field::Album => r.tagged_album?.to_string(),
            Field::Title => r.tagged_title?.to_string(),
            Field::DisplayTitle => r.title.to_string(),
            Field::Track => r.track?.to_string(),
            Field::Disc => r.disc?.to_string(),
            Field::Year => r.year?.to_string(),
            // Joined, because a row carries every concept asserted on this
            // axis and a line has one slot. `; ` is the separator taggers
            // write, so a library that has had no curation renders exactly its
            // tags.
            Field::Facet(f) => {
                let held = r.on(f);
                if held.is_empty() {
                    return None;
                }
                held.join("; ")
            }
            Field::Label => r.label?.to_string(),
            Field::Duration => {
                let secs = r.duration.as_secs();
                format!("{}:{:02}", secs / 60, secs % 60)
            }
            Field::Bitrate => r.bitrate?.to_string(),
            Field::SampleRate => r.sample_rate?.to_string(),
            Field::Channels => r.channels?.to_string(),
            // Refused at parse time, so this is unreachable rather than blank.
            Field::Path | Field::FileName => return None,
        })
    }

    /// Whether a row can answer this at all.
    ///
    /// `path` and `file_name` are properties of one device's copy, and a row
    /// holds no location — the omission that stopped history being keyed by
    /// path, learned at the cost of a four-star rating on a file that still
    /// existed. A template asking for one is refused when it is parsed rather
    /// than rendering an empty space, because a silent blank is exactly the
    /// inference this project keeps turning into a declaration.
    const fn is_of_a_row(self) -> bool {
        !matches!(self, Field::Path | Field::FileName)
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
    #[error("unknown field {name:?} — try one of: {}", Field::names().join(", "))]
    UnknownField { name: String },
    #[error(
        "{name:?} is a property of this machine's copy, not of the track — a displayed row carries no location"
    )]
    NotOnARow { name: String },
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

    pub fn render(&self, row: &crate::model::rows::Listed<'_>) -> String {
        let mut out = String::new();
        render_atoms(&self.atoms, row, &mut out);
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
                // Refused here rather than rendered blank. A row holds no
                // location, so a template asking for one is a mistake worth
                // being told about at the moment it is written (#78).
                if !field.is_of_a_row() {
                    return Err(FormatError::NotOnARow {
                        name: name.trim().into(),
                    });
                }
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
fn render_atoms(atoms: &[Atom], row: &crate::model::rows::Listed<'_>, out: &mut String) -> bool {
    let mut complete = true;
    for atom in atoms {
        match atom {
            Atom::Literal(text) => out.push_str(text),
            Atom::Field { field, pad } => match field.value(row) {
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
                if render_atoms(inner, row, &mut nested) {
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
    use crate::fixture::Tagged as Track;
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
            genres: vec!["folk".into()],
            duration: Duration::from_secs(221),
            bitrate: Some(320),
            ..Default::default()
        }
    }

    fn render(source: &str, track: &Track) -> String {
        Template::parse(source)
            .unwrap()
            .render(&track.drawn().listed())
    }

    /// **An axis reaches completion and the error message for free** — #117.
    ///
    /// The names were a hand-written list beside the enum, so a new field
    /// meant remembering two places and the second one failed silently: an
    /// axis nobody listed is an axis nobody can be told about when they
    /// mistype it. Built from `facet::ALL` now, so the list cannot fall behind.
    #[test]
    fn every_axis_is_a_field_name() {
        let names = Field::names();
        for (key, _) in crate::model::facet::ALL {
            assert!(names.contains(key), "{key} is an axis and not a field name");
            assert!(
                Field::parse(key).is_some(),
                "{key} is a field name and does not parse"
            );
        }
        assert!(
            names.windows(2).all(|w| w[0] <= w[1]),
            "not sorted: {names:?}"
        );
        assert!(
            Field::parse("genrre").is_none(),
            "a typo resolved to an axis"
        );
    }

    /// A template renders any axis, joined, and disappears when nothing is
    /// asserted on it — the `[…]` group behaviour every optional field has.
    #[test]
    fn a_template_renders_any_axis() {
        use crate::model::facet;
        let mut drawn = full().drawn();
        drawn.concepts.insert(
            crate::model::id::Id::of_key(facet::FUNCTION),
            vec!["dance-floor".to_string()],
        );
        let show = |src: &str, d: &crate::model::rows::Drawn| {
            Template::parse(src).expect("parses").render(&d.listed())
        };
        assert_eq!(show("{genre}", &drawn), "folk");
        assert_eq!(show("{function}", &drawn), "dance-floor");
        assert_eq!(
            show("{title}[ — {function}]", &drawn),
            "The Sea — dance-floor"
        );

        // Nothing on the axis, so the group disappears rather than rendering
        // an empty dash.
        drawn
            .concepts
            .remove(&crate::model::id::Id::of_key(facet::FUNCTION));
        assert_eq!(show("{title}[ — {function}]", &drawn), "The Sea");
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
    /// **A row carries no location, so a template may not ask for one.**
    ///
    /// Refused when the template is parsed rather than rendering an empty
    /// space, because a blank field is an inference and this project keeps
    /// turning those into declarations. The two names still parse — they are
    /// real fields of a file — and it is asking a *row* for them that fails.
    #[test]
    fn a_template_cannot_ask_a_row_where_the_file_is() {
        for name in ["path", "filename"] {
            let err = Template::parse(&format!("{{{name}}}")).unwrap_err();
            assert!(
                matches!(err, FormatError::NotOnARow { .. }),
                "{name} was accepted, and would render nothing"
            );
            // Not confused with a typo: the message has to say which mistake
            // it is, or somebody will go looking for a misspelling.
            assert!(err.to_string().contains("carries no location"), "{err}");
        }
        assert!(matches!(
            Template::parse("{pahт}").unwrap_err(),
            FormatError::UnknownField { .. }
        ));
    }

    /// **The stand-in is not a name.** `album` falls back to a placeholder so
    /// a tree node always has a label, and a template must not show that
    /// placeholder as though the tags had said it — `[ — {album}]` is meant to
    /// disappear for a track that is on no album.
    #[test]
    fn a_track_on_no_album_renders_no_album() {
        let mut bare = full();
        bare.album = None;
        assert_eq!(render("{artist}[ — {album}]", &bare), "Sierra Ferrell");
    }

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
            Template::default().render(&empty.drawn().listed()),
            format!("{} — whatever.mp3", empty.drawn().file_under)
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
