//! The filter expression language (DESIGN §7).
//!
//! `genre="jazz"|genre="blues"`, `artist="*ferrell*"&year>2020`.
//!
//! Terse on purpose — this is typed at a `:` prompt, not written in prose.
//! Strings match by glob with `*` as the only metacharacter, because music
//! metadata is made of regex metacharacters and regex fails *silently* on it:
//! see the Decision Log for the counts that decided this.

use crate::format::{Field, Kind};
use crate::model::entity::Recording;
use crate::model::id::Id;
use crate::model::rows::Subject;

/// What a filter can see besides the track itself.
///
/// History lives in its own store keyed by path (§13), not on `Track`, so a
/// predicate that asks about play counts needs handing the store as well.
/// `None` means "no history available", and every history field is then
/// *missing* rather than zero — the difference between "never played" and
/// "cannot say".
/// `Debug` by hand: a trait object cannot derive it, and what a caller wants
/// to see here is whether history was supplied at all rather than all of it.
#[derive(Clone, Copy, Default)]
pub struct Env<'a> {
    /// What the library knows about how things have been listened to.
    ///
    /// Keyed by **identity**, not by path, which is the whole of #81: a
    /// renumber moved a file and stranded a four-star rating because the key
    /// moved with it. A recording's id does not move.
    pub listened: Option<&'a dyn Listened>,
    /// Seconds since the epoch, for the `days_since_*` fields. Passed in so a
    /// test can fix the clock.
    pub now: i64,
}

impl std::fmt::Debug for Env<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("listened", &self.listened.is_some())
            .field("now", &self.now)
            .finish()
    }
}

/// What the library knows about a recording's listening history.
///
/// A trait rather than the `Catalogue` itself so that `filter` keeps not
/// knowing what a log is — it asks four questions and does not care whether
/// the answers were folded, cached or made up by a test.
pub trait Listened {
    /// Times played. Zero is an answer, not a missing value: `play_count=0`
    /// is how you ask for what you have never heard.
    fn plays(&self, recording: Id<Recording>) -> u32;
    /// When it was last played, if it ever was.
    fn last_played(&self, recording: Id<Recording>) -> Option<i64>;
    /// When it entered the library, if anyone has said.
    fn acquired(&self, recording: Id<Recording>) -> Option<i64>;
    /// Stars. `None` is unrated, which is not the same as nought stars.
    fn rating(&self, recording: Id<Recording>) -> Option<u8>;
    /// Every concept **broader than** this one, on this axis — #93's closure.
    ///
    /// Walking *up* rather than down is what makes closure cheap: a row has a
    /// few genres and the question is whether any of them sits under the one
    /// asked about, so this walks each of the row's own values to the top
    /// rather than expanding the target's descendants.
    ///
    /// **Bounded, and the bound is not optional** (#79). A cycle somebody's
    /// data acquired — `Techno` narrower-of `Rock` narrower-of `Techno` — must
    /// not hang a render, and a hierarchy is curated by hand so a cycle is a
    /// question of when rather than whether.
    fn broader(&self, facet_key: &str, label: &str) -> Vec<String>;

    /// How many collections hold this — #119's "in no crate".
    ///
    /// A count rather than a yes/no, so `collected>1` finds the overlap the
    /// filesystem used to forbid: a pop song is in Dance when you are DJing
    /// with it and in General the rest of the time, and both are true.
    fn collected(&self, recording: Id<Recording>) -> u32;
}

/// A glob pattern. `*` is the only metacharacter; `\*` is a literal asterisk.
///
/// `?` is deliberately **not** one. Glob's "exactly one character" is never
/// what anyone wants from a music search, while a question mark in a title is
/// real — the reference library has one and no asterisks at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Glob {
    /// The literal runs between stars, in order.
    segments: Vec<String>,
    /// The pattern did not begin with `*`, so the first segment is anchored.
    anchored_start: bool,
    /// The pattern did not end with `*`.
    anchored_end: bool,
    /// Smart-case: only if the pattern itself contains a capital.
    case_sensitive: bool,
}

impl Glob {
    pub fn new(pattern: &str) -> Self {
        let case_sensitive = pattern.chars().any(char::is_uppercase);
        let mut segments = Vec::new();
        let mut current = String::new();
        let mut saw_star = false;
        let mut anchored_start = true;
        let mut anchored_end = true;

        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&'*') => {
                    chars.next();
                    current.push('*');
                }
                '\\' if chars.peek() == Some(&'\\') => {
                    chars.next();
                    current.push('\\');
                }
                '*' => {
                    if !saw_star && current.is_empty() {
                        anchored_start = false;
                    }
                    saw_star = true;
                    anchored_end = false;
                    if !current.is_empty() {
                        segments.push(std::mem::take(&mut current));
                    }
                }
                c => {
                    if saw_star {
                        anchored_end = true;
                        saw_star = false;
                    }
                    current.push(c);
                }
            }
        }
        if !current.is_empty() {
            segments.push(current);
        }

        // The pattern is flattened whatever the case rule: smart-case is
        // about capitals, and there is no way to "type a capital" to ask for
        // a diaeresis you do not have on the keyboard.
        for s in &mut segments {
            *s = crate::text::flatten(s).into_owned();
            if !case_sensitive {
                *s = s.to_lowercase();
            }
        }
        Glob {
            segments,
            anchored_start,
            anchored_end,
            case_sensitive,
        }
    }

    /// Tries the flattened text, then — only if the text actually spells
    /// differently without its marks — the spelled-out form, so that both
    /// `loffler` and `loeffler` find `Löffler`.
    pub fn matches(&self, text: &str) -> bool {
        if self.matches_folded(&crate::text::flatten(text)) {
            return true;
        }
        crate::text::expanded(text).is_some_and(|s| self.matches_folded(&s))
    }

    fn matches_folded(&self, text: &str) -> bool {
        // Lowering allocates. Measured elsewhere at well under a millisecond
        // for a whole-library pass, and this is not on the audio path.
        let owned;
        let hay: &str = if self.case_sensitive {
            text
        } else {
            owned = text.to_lowercase();
            &owned
        };

        // Nothing but stars matches anything, including the empty string.
        let mut rest = hay;
        let mut segments = self.segments.as_slice();
        if segments.is_empty() {
            return self.anchored_start == self.anchored_end || !self.anchored_start;
        }

        if self.anchored_start {
            let first = &segments[0];
            match rest.strip_prefix(first.as_str()) {
                Some(r) => rest = r,
                None => return false,
            }
            segments = &segments[1..];
        }
        if self.anchored_end
            && let Some((last, head)) = segments.split_last()
        {
            match rest.strip_suffix(last.as_str()) {
                Some(r) => rest = r,
                None => return false,
            }
            segments = head;
        }
        for seg in segments {
            match rest.find(seg.as_str()) {
                Some(i) => rest = &rest[i + seg.len()..],
                None => return false,
            }
        }
        true
    }
}

/// What a comparison looks at. `any` is a filter-only pseudo-field covering
/// every text field, so "search everything" is explicit rather than a bare
/// word with implicit meaning.
/// A field that comes from the history store rather than the file's tags.
///
/// Dates are exposed as **days since**, not as timestamps: `days_since_added<30`
/// reads, and `added>1735689600` does not. It also needs no date literals in
/// the grammar, which is a whole syntax avoided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryField {
    /// How many collections hold this. `!collected?` is "in no crate" (#119).
    Collected,
    PlayCount,
    DaysSincePlayed,
    DaysSinceAdded,
    Rating,
}

impl HistoryField {
    pub const NAMES: &'static [&'static str] = &[
        "collected",
        "days_since_added",
        "days_since_played",
        "play_count",
        "rating",
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "collected" => HistoryField::Collected,
            "play_count" => HistoryField::PlayCount,
            "days_since_played" => HistoryField::DaysSincePlayed,
            "days_since_added" => HistoryField::DaysSinceAdded,
            "rating" => HistoryField::Rating,
            _ => return None,
        })
    }
}

/// How far up the hierarchy a comparison reaches — #93, SPARQL's spelling.
///
/// **One meaning per character.** `*` is zero-or-more and `+` is one-or-more,
/// as in SPARQL and every regex anybody has met. An earlier draft used `+` for
/// both and justified it by the borrowing, which stops being true the moment
/// the meaning changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// `genre="techno"` — this concept, and nothing else.
    Exact,
    /// `genre*="electronic"` — it, or anything narrower, at any depth.
    OrNarrower,
    /// `genre+="electronic"` — strictly narrower. Techno yes, electronic no.
    Narrower,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    Field(Quant, Reach, Field),
    Any,
    History(HistoryField),
}

/// How many of a multi-valued field's values must satisfy the comparison —
/// #93, `query-language.md` §7.
///
/// **The language had both and said neither.** A row carries many genres, and
/// `genre="rock"` meant *some* while `genre!="rock"` meant *none* — two
/// different quantifiers on one syntax, chosen silently. Both readings are
/// useful and a reader could not tell which they were getting.
///
/// **The defaults are unchanged and now stated.** They are the readings people
/// want, and they differ by operator:
///
/// | written | means |
/// | --- | --- |
/// | `genre="rock"` | **any** — some genre is rock |
/// | `genre!="rock"` | **all** — no genre is rock |
///
/// A file tagged `Rock; Blues Rock` *is* Rock, and every one of its genres is
/// not Jazz. Making both default to `any` would have made `genre!="rock"` true
/// of that file, which is not what anybody means — so the asymmetry stays, and
/// what changes is that it has a name and the other reading is sayable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quant {
    /// Nobody wrote one, so the operator decides — see the table above.
    ///
    /// A third state rather than resolving at parse time, because the two
    /// halves of the answer live apart: the quantifier is read before the
    /// operator is.
    Unsaid,
    /// `any(genre)="rock"` — some value matches.
    Any,
    /// `all(genre)="rock"` — every value matches, and a row with none fails.
    ///
    /// **Not vacuously true on an empty set.** Formal logic says every element
    /// of nothing satisfies anything; a listener asking "all of its genres are
    /// rock" about a track with no genre does not mean yes. `!genre?` is how
    /// you ask for the untagged, and it is a different question.
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(Glob),
    Number(i64),
}

/// A parsed filter and the text it was written as.
///
/// The source is kept so `format` can hand back exactly what was typed —
/// §6 requires a command to round-trip through text, and rendering an AST
/// would give `genre="folk"` for a line someone wrote as `genre=folk`.
/// Parsing still happens at the boundary: nothing downstream sees the string.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub source: String,
    pub expr: Expr,
}

impl Filter {
    pub fn parse(source: &str) -> Result<Self, FilterError> {
        Ok(Filter {
            source: source.trim().to_string(),
            expr: Expr::parse(source)?,
        })
    }

    pub fn compile<'a>(&'a self, env: Env<'a>) -> impl Fn(&Subject<'_>) -> bool + 'a {
        self.expr.compile(env)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp {
        selector: Selector,
        op: Op,
        value: Value,
    },
    /// `genre?` — the track has a value for this field at all.
    ///
    /// Expressible without it (`genre="*"` for text, `year>=0` for numbers)
    /// because a missing tag matches nothing, but only if you work that out,
    /// and text and numbers needed different spellings. foobar2000 spells it
    /// `MISSING`/`PRESENT`; this is the same idea in this language's register.
    Present(Selector),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("{name}(…) is missing its closing bracket")]
    UnclosedQuantifier { name: String },
    #[error("a quantifier asks about the values, so `?` belongs outside it")]
    PresenceInQuantifier,
    #[error("unknown field {name:?} — try one of: {}", field_names().join(", "))]
    UnknownField { name: String },
    #[error("expected a field name, found {0:?}")]
    ExpectedField(String),
    #[error("expected an operator after {0:?}")]
    ExpectedOperator(String),
    #[error("{field} holds text, so {op} does not apply to it")]
    NotOrderable { field: String, op: &'static str },
    #[error("{field} holds a number, so it cannot be compared with a quoted pattern")]
    NotText { field: String },
    #[error("{0:?} is not a number")]
    NotANumber(String),
    #[error("unclosed quote")]
    UnclosedQuote,
    #[error("unclosed (")]
    UnclosedParen,
    #[error("unexpected )")]
    UnexpectedParen,
    #[error("the expression is empty")]
    Empty,
    #[error("unexpected {0:?} at the end")]
    Trailing(String),
    #[error("`any?` is not a question — every track has some text")]
    PresentOnAny,
    #[error("no track has zero stars — `rating?` is rated, `!rating?` is not")]
    RatingZero,
    #[error("{name} is how a sort spells it — in a filter, use `{instead}`")]
    WrongLanguage { name: String, instead: &'static str },
}

fn field_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Field::names();
    names.extend_from_slice(HistoryField::NAMES);
    names.push("any");
    names.sort_unstable();
    names
}

impl Expr {
    pub fn parse(source: &str) -> Result<Self, FilterError> {
        let tokens = lex(source)?;
        let mut p = Parser { tokens, at: 0 };
        let expr = p.or_expr()?;
        match p.peek() {
            None => Ok(expr),
            Some(Token::CloseParen) => Err(FilterError::UnexpectedParen),
            Some(t) => Err(FilterError::Trailing(t.describe())),
        }
    }

    /// Build the predicate once. **Do not re-interpret the AST per track** —
    /// this runs over the whole library on every keystroke in view 6.
    pub fn compile<'a>(&'a self, env: Env<'a>) -> impl Fn(&Subject<'_>) -> bool + 'a {
        move |subject| self.eval(subject, &env)
    }

    fn eval(&self, t: &Subject<'_>, env: &Env) -> bool {
        match self {
            Expr::And(a, b) => a.eval(t, env) && b.eval(t, env),
            Expr::Or(a, b) => a.eval(t, env) || b.eval(t, env),
            Expr::Not(e) => !e.eval(t, env),
            Expr::Present(Selector::Field(_, _, f)) => present(*f, t),
            Expr::Present(Selector::Any) => true,
            Expr::Present(Selector::History(h)) => history_number(*h, t, env).is_some(),
            Expr::Cmp {
                selector,
                op,
                value,
            } => compare(*selector, *op, value, t, env),
        }
    }
}

/// Text fields `any` looks at.
///
/// Not `path`: every track's path contains the library root, so including it
/// would make `any="*music*"` match the entire library.
///
/// **A function, not a const**, because a facet's id is a hash of its key and
/// cannot be computed at compile time. Genre alone is here: `any` is the
/// "search everything" escape and every axis in it would make `any=live` match
/// a version somebody never meant to search.
fn any_fields() -> Vec<Field> {
    vec![
        Field::Artist,
        Field::AlbumArtist,
        Field::Album,
        Field::Title,
        Field::Facet(crate::model::id::Id::of_key(crate::model::facet::GENRE)),
        Field::FileName,
    ]
}

/// A history field's value. `None` is "cannot say" — no history, or never
/// played — and a comparison against it is false, like any missing field.
fn history_number(f: HistoryField, s: &Subject<'_>, env: &Env) -> Option<i64> {
    let listened = env.listened?;
    // The row's identity, not its path — and **the library's binding rather
    // than the tags'** (#86). A file with nothing durable to hang history on
    // answers nothing, which is honest where filing it under a path was an
    // answer that went wrong the moment anything moved. Minting it from the
    // tags here was a subtler version of the same thing: it is the identity a
    // relink changes, so a filter would have asked about the recording a file
    // used to be.
    let recording = s.recording?;
    Some(match f {
        // An unplayed track has played zero times, which is an answer;
        // `play_count=0` is how you ask for the ones you have never heard.
        // **Zero collections is missing, not zero** — `!collected?` is "in no
        // crate" (#119), and it is a question worth being able to ask: a file
        // that has not been filed is one nobody has decided about, which is
        // different from one decided to be in nothing.
        HistoryField::Collected => match listened.collected(recording) {
            0 => return None,
            n => i64::from(n),
        },
        HistoryField::PlayCount => i64::from(listened.plays(recording)),
        HistoryField::DaysSincePlayed => days_since(listened.last_played(recording)?, env.now),
        HistoryField::DaysSinceAdded => days_since(listened.acquired(recording)?, env.now),
        // Unrated is *missing*, not zero — `!rating?` is how you find the
        // tracks you have not judged, and calling them nought stars would
        // make them look actively disliked.
        HistoryField::Rating => i64::from(listened.rating(recording)?),
    })
}

fn days_since(then: i64, now: i64) -> i64 {
    (now - then).max(0) / 86_400
}

fn compare(selector: Selector, op: Op, value: &Value, t: &Subject<'_>, env: &Env) -> bool {
    match (selector, value) {
        (Selector::History(h), Value::Number(n)) => match history_number(h, t, env) {
            Some(v) => match op {
                Op::Eq => v == *n,
                Op::Ne => v != *n,
                Op::Lt => v < *n,
                Op::Le => v <= *n,
                Op::Gt => v > *n,
                Op::Ge => v >= *n,
            },
            None => false,
        },
        // A history field against a quoted pattern is rejected by the parser.
        (Selector::History(_), Value::Text(_)) => false,
        (Selector::Any, Value::Text(glob)) => {
            let hit = any_fields()
                .iter()
                .any(|f| texts_of(*f, t).iter().any(|v| glob.matches(v)));
            match op {
                Op::Ne => !hit,
                _ => hit,
            }
        }
        (Selector::Field(q, reach, f), Value::Text(glob)) => match texts_of(f, t).as_slice() {
            // A missing tag matches nothing, including `!=`. Three-valued
            // logic would be more correct and much harder to predict; `!` on
            // the whole comparison is how you ask for "or missing". `all` is
            // not vacuously true here for the same reason.
            [] => false,
            values => {
                // **Closure walks up from the row's own values** (#93). The
                // question is whether one of them sits at or under the concept
                // asked about, and a row has a few genres where a taxonomy has
                // thousands of descendants.
                let hit = |v: &String| match reach {
                    Reach::Exact => glob.matches(v),
                    Reach::OrNarrower | Reach::Narrower => {
                        (reach == Reach::OrNarrower && glob.matches(v))
                            || env.listened.is_some_and(|l| {
                                l.broader(f.name().as_str(), v)
                                    .iter()
                                    .any(|up| glob.matches(up))
                            })
                    }
                };
                // The unsaid quantifier is the operator's default: `=` asks
                // whether any matches, `!=` whether none does.
                let q = match (q, op) {
                    (Quant::Unsaid, Op::Ne) => Quant::All,
                    (Quant::Unsaid, _) => Quant::Any,
                    (said, _) => said,
                };
                match (q, op) {
                    // A file tagged `Rock; Blues Rock` is Rock…
                    (Quant::Any, Op::Ne) => values.iter().any(|v| !hit(v)),
                    (Quant::Any, _) => values.iter().any(hit),
                    // …and every one of its genres is not Jazz.
                    (Quant::All, Op::Ne) => !values.iter().any(hit),
                    (Quant::All, _) => values.iter().all(hit),
                    (Quant::Unsaid, _) => unreachable!("resolved just above"),
                }
            }
        },
        (Selector::Field(_, _, f), Value::Number(n)) => match number_of(f, t) {
            Some(v) => match op {
                Op::Eq => v == *n,
                Op::Ne => v != *n,
                Op::Lt => v < *n,
                Op::Le => v <= *n,
                Op::Gt => v > *n,
                Op::Ge => v >= *n,
            },
            None => false,
        },
        // `any` against a number is rejected by the parser.
        (Selector::Any, Value::Number(_)) => false,
    }
}

/// Whether the track has this field at all.
///
/// An empty tag counts as missing: a `GENRE=""` frame is a tagging artefact,
/// not an answer, and treating it as present would make `!genre?` miss exactly
/// the files worth finding.
fn present(f: Field, t: &Subject<'_>) -> bool {
    match f.kind() {
        Kind::Text => texts_of(f, t).iter().any(|v| !v.trim().is_empty()),
        Kind::Number => number_of(f, t).is_some(),
    }
}

/// The values a text field has: none, one, or several.
///
/// **A list because one field genuinely has several** (#86). A genre tag names
/// three terms in a hierarchical codelist, and comparing against the joined
/// string meant `genre=Rock` could not match a file that was also tagged
/// `Blues Rock`. Every other field returns nought or one, and the comparison
/// is the same either way: does *any* value match.
pub(crate) fn texts_of(f: Field, s: &Subject<'_>) -> Vec<String> {
    let one = |v: Option<&str>| v.map(|v| vec![v.to_string()]).unwrap_or_default();
    let r = &s.row;
    match f {
        Field::Artist => one(r.artist),
        Field::AlbumArtist => one(r.album_artist),
        Field::FileUnder => vec![r.file_under.to_string()],
        // The album and title **as tagged**. `r.album` stands a placeholder in
        // so a tree node always has a label, and `!album?` has to stay
        // meaningful — a filter must not find "No Album" as though somebody
        // had named an album that.
        Field::Album => one(r.album_if_named),
        Field::Title => one(r.title_if_named),
        Field::DisplayTitle => vec![r.title.to_string()],
        Field::Facet(f) => s.row.on(f).to_vec(),
        Field::Label => one(r.label),
        // The two that need the file's whereabouts, asked at the moment of
        // matching and not kept.
        Field::Path => one(s.locator),
        Field::FileName => one(s
            .locator
            .and_then(|l| std::path::Path::new(l).file_name().and_then(|n| n.to_str()))),
        _ => Vec::new(),
    }
}

pub(crate) fn number_of(f: Field, s: &Subject<'_>) -> Option<i64> {
    let r = &s.row;
    Some(match f {
        Field::Track => i64::from(r.track?),
        Field::Disc => i64::from(r.disc?),
        Field::Year => i64::from(r.year?),
        // Seconds, not the `3:41` the format language renders.
        Field::Duration => r.duration.as_secs() as i64,
        Field::Bitrate => i64::from(r.bitrate?),
        Field::SampleRate => i64::from(r.sample_rate?),
        Field::Channels => i64::from(r.channels?),
        _ => return None,
    })
}

// ---- lexing and parsing ----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Quoted(String),
    Op(Op),
    And,
    Or,
    Not,
    OpenParen,
    CloseParen,
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Token::Word(w) => w.clone(),
            Token::Quoted(q) => format!("\"{q}\""),
            Token::Op(_) => "an operator".into(),
            Token::And => "&".into(),
            Token::Or => "|".into(),
            Token::Not => "!".into(),
            Token::OpenParen => "(".into(),
            Token::CloseParen => ")".into(),
        }
    }
}

fn lex(source: &str) -> Result<Vec<Token>, FilterError> {
    let mut out = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '&' => out.push(Token::And),
            '|' => out.push(Token::Or),
            '(' => out.push(Token::OpenParen),
            ')' => out.push(Token::CloseParen),
            '!' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    out.push(Token::Op(Op::Ne));
                } else {
                    out.push(Token::Not);
                }
            }
            '=' => out.push(Token::Op(Op::Eq)),
            '<' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    out.push(Token::Op(Op::Le));
                } else {
                    out.push(Token::Op(Op::Lt));
                }
            }
            '>' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    out.push(Token::Op(Op::Ge));
                } else {
                    out.push(Token::Op(Op::Gt));
                }
            }
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // The glob keeps its own escapes, so a backslash pair
                        // passes through intact.
                        Some('\\') => match chars.next() {
                            Some(n) => {
                                s.push('\\');
                                s.push(n);
                            }
                            None => return Err(FilterError::UnclosedQuote),
                        },
                        Some(c) => s.push(c),
                        None => return Err(FilterError::UnclosedQuote),
                    }
                }
                out.push(Token::Quoted(s));
            }
            c => {
                let mut w = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || "&|!()=<>\"".contains(n) {
                        break;
                    }
                    w.push(n);
                    chars.next();
                }
                out.push(Token::Word(w));
            }
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }
    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        if t.is_some() {
            self.at += 1;
        }
        t
    }

    fn or_expr(&mut self) -> Result<Expr, FilterError> {
        let mut left = self.and_expr()?;
        while self.peek() == Some(&Token::Or) {
            self.next();
            let right = self.and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, FilterError> {
        let mut left = self.unary()?;
        while self.peek() == Some(&Token::And) {
            self.next();
            let right = self.unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, FilterError> {
        if self.peek() == Some(&Token::Not) {
            self.next();
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, FilterError> {
        match self.next() {
            Some(Token::OpenParen) => {
                let inner = self.or_expr()?;
                match self.next() {
                    Some(Token::CloseParen) => Ok(inner),
                    _ => Err(FilterError::UnclosedParen),
                }
            }
            Some(Token::Word(name)) => self.comparison(name),
            Some(t) => Err(FilterError::ExpectedField(t.describe())),
            None => Err(FilterError::Empty),
        }
    }

    fn comparison(&mut self, mut name: String) -> Result<Expr, FilterError> {
        // A trailing `?` asks whether the field is there at all. Checked on
        // the *field* word only, so an unquoted value that ends in a question
        // mark — `title=You?` — is untouched.
        let presence = name.ends_with('?');
        if presence {
            name.pop();
        }

        // **`any(f)` and `all(f)` say which quantifier** (#93). Written as a
        // call because it reads aloud — "all of its genres" — and because
        // `any` on its own is already the search-everything pseudo-field. The
        // parenthesis is what tells them apart, and it has to be read as a
        // token: the grammar already has grouping parens, so `any` and `(`
        // arrive separately.
        let quant = match (name.as_str(), self.peek()) {
            ("all", Some(Token::OpenParen)) => Quant::All,
            ("any", Some(Token::OpenParen)) => Quant::Any,
            _ => Quant::Unsaid,
        };
        if quant != Quant::Unsaid {
            self.next();
            name = match self.next() {
                Some(Token::Word(w)) => w,
                _ => return Err(FilterError::ExpectedOperator(name)),
            };
            if !matches!(self.next(), Some(Token::CloseParen)) {
                return Err(FilterError::UnclosedQuantifier { name });
            }
            // A presence test inside a quantifier — `all(genre?)` — asks
            // nothing extra: presence is about the set, not its members.
            if name.ends_with('?') {
                return Err(FilterError::PresenceInQuantifier);
            }
        }

        // **A trailing `*` or `+` reaches up the hierarchy** (#93). Read
        // *after* the quantifier, because the marker sits against the field
        // name and the field name is inside the brackets: `any(genre*)`, not
        // `any(genre)*`. Closure is about the field; the quantifier is about
        // how many of its values must satisfy the test.
        let reach = if name.ends_with('*') {
            name.pop();
            Reach::OrNarrower
        } else if name.ends_with('+') {
            name.pop();
            Reach::Narrower
        } else {
            Reach::Exact
        };

        let selector = if name == "any" {
            Selector::Any
        } else if let Some(h) = HistoryField::parse(&name) {
            Selector::History(h)
        } else {
            Selector::Field(
                quant,
                reach,
                Field::parse(&name).ok_or_else(|| match name.as_str() {
                    // The sort language's spelling of the two date fields. Naming
                    // the replacement keeps a wrong guess to one message.
                    "added" => FilterError::WrongLanguage {
                        name: name.clone(),
                        instead: "days_since_added",
                    },
                    "last_played" => FilterError::WrongLanguage {
                        name: name.clone(),
                        instead: "days_since_played",
                    },
                    _ => FilterError::UnknownField { name: name.clone() },
                })?,
            )
        };

        if presence {
            return match selector {
                Selector::Any => Err(FilterError::PresentOnAny),
                s => Ok(Expr::Present(s)),
            };
        }

        let op = match self.next() {
            Some(Token::Op(op)) => op,
            _ => return Err(FilterError::ExpectedOperator(name)),
        };

        let kind = match selector {
            Selector::Any => Kind::Text,
            Selector::Field(_, _, f) => f.kind(),
            Selector::History(_) => Kind::Number,
        };

        // Ordering a string is meaningless, and saying so beats comparing
        // alphabetically and surprising someone.
        if kind == Kind::Text && !matches!(op, Op::Eq | Op::Ne) {
            return Err(FilterError::NotOrderable {
                field: name,
                op: match op {
                    Op::Lt => "<",
                    Op::Le => "<=",
                    Op::Gt => ">",
                    Op::Ge => ">=",
                    _ => unreachable!("Eq and Ne are allowed"),
                },
            });
        }

        let value = match self.next() {
            Some(Token::Quoted(s)) => {
                if kind == Kind::Number {
                    return Err(FilterError::NotText { field: name });
                }
                Value::Text(Glob::new(&s))
            }
            // Unquoted is a convenience, not an ambiguity: the field decides
            // the type, so `genre=jazz` and `year>2020` both read plainly.
            Some(Token::Word(w)) => match kind {
                Kind::Text => Value::Text(Glob::new(&w)),
                Kind::Number => Value::Number(w.parse().map_err(|_| FilterError::NotANumber(w))?),
            },
            _ => return Err(FilterError::ExpectedOperator(name)),
        };

        // Ratings are 1 to 5 or absent, so nothing can be zero. Every
        // comparison against it is therefore either dead (`rating=0`,
        // `rating<0`) or a long way of writing `rating?` (`rating>0`) — and it
        // is what someone reaching for "unrated" will type first, so say the
        // idiom rather than returning nothing.
        if selector == Selector::History(HistoryField::Rating) && value == Value::Number(0) {
            return Err(FilterError::RatingZero);
        }

        Ok(Expr::Cmp {
            selector,
            op,
            value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Tagged as Track;
    use std::path::PathBuf;
    use std::time::Duration;

    fn track() -> Track {
        Track {
            path: PathBuf::from("/m/Sierra Ferrell/Long Time Coming/01 The Sea.mp3"),
            artist: Some("Sierra Ferrell".into()),
            albumartist: Some("Sierra Ferrell".into()),
            album: Some("Long Time Coming".into()),
            title: Some("The Sea".into()),
            genres: vec!["folk".into()],
            tracknumber: Some(1),
            date: Some(2021),
            duration: Duration::from_secs(221),
            bitrate: Some(320),
            // History hangs off an identity now (#81), so a track that is
            // meant to have any needs one.
            audio_id: Some(crate::audio_id::AudioId { scheme: 1, hash: 7 }),
            ..Default::default()
        }
    }

    /// A subject as the player assembles one: a row, where the file is, and
    /// what history hangs off.
    ///
    /// The row is *drawn* — folded through the model rather than assembled
    /// here — so a fixture cannot match against a row the library could not
    /// produce. The `Drawn` is returned alongside because a `Subject` borrows
    /// it and it has to outlive the comparison.
    fn subject_of(t: &Track) -> (crate::model::rows::Drawn, Option<Id<Recording>>) {
        use crate::model::rows::Listing;
        let mut h = crate::scan::Holdings::new();
        h.insert(t.held());
        let l = crate::fixture::from_holdings(&h);
        let id = crate::model::id::Id::of_audio(&t.held().seen.audio);
        (t.drawn(), l.recording_of(id))
    }

    fn matches(expr: &str, t: &Track) -> bool {
        let (drawn, recording) = subject_of(t);
        let subject = Subject {
            row: drawn.listed(),
            locator: t.path.to_str(),
            recording,
        };
        Expr::parse(expr)
            .expect("should parse")
            .compile(Env::default())(&subject)
    }

    /// **Every axis is queryable, and a new one costs a line** — #117.
    ///
    /// `genre` was a `Field` variant, so `version` and `function` were not
    /// expressible at all: the query language knew one axis because somebody
    /// had typed it out. `Field::Facet(Id<Facet>)` resolves any key in
    /// `facet::ALL`, and the id derives from the key, so the parser needs no
    /// vocabulary to consult and two axes cannot collide.
    #[test]
    fn a_facet_is_a_query_field() {
        use crate::model::facet;

        // A version, read off the title by `ingest` — the claim that had
        // nowhere to go before #101 and nowhere to be *asked about* before now.
        let mut instrumental = track();
        instrumental.title = Some("212 (Instrumental)".into());
        assert!(matches(r#"version="instrumental""#, &instrumental));
        assert!(!matches(r#"version="live""#, &instrumental));

        // And the axis the `Dance` folder was on. Nothing on disk says a
        // recording is for dancing, so this is asserted the way a curation
        // pass would assert it.
        let mut held = crate::scan::Holdings::new();
        held.insert(instrumental.held());
        let bare = crate::fixture::from_holdings(&held);
        let copy = crate::model::id::Id::of_audio(&instrumental.held().seen.audio);
        let recording = crate::model::rows::Listing::recording_of(&bare, copy).expect("scanned");

        let mut i = crate::model::instance::Instance::new(crate::fixture::device());
        i.assert_concept(
            100,
            crate::model::facet::Described::Recording(recording),
            facet::FUNCTION,
            "dance-floor",
            crate::model::reference::Source::Manual,
            crate::model::ingest::Confidence::Confirmed,
        );
        let l = crate::fixture::from_holdings_and(&held, i.records());
        let row = crate::model::rows::Listing::listed(&l, copy).expect("a row");
        let subject = Subject {
            row,
            locator: instrumental.path.to_str(),
            recording: Some(recording),
        };
        let hit = |e: &str| {
            Expr::parse(e)
                .expect("should parse")
                .compile(Env::default())(&subject)
        };

        assert!(hit(r#"function="dance-floor""#));
        assert!(!hit(r#"function="background""#));
        // **The axes stay apart**, which is what putting the facet in the id
        // buys: a function is not a genre however it is spelled.
        assert!(!hit(r#"genre="dance-floor""#));
        assert!(hit(r#"function="dance-floor" & version="instrumental""#));
    }

    /// A name no axis claims is a typo, and has to say so.
    ///
    /// Resolving any word to a facet id would have been one line shorter and
    /// would have turned `genrre=techno` into a query that quietly matches
    /// nothing — the inference this project keeps turning into a declaration.
    #[test]
    fn a_name_no_axis_claims_is_not_a_field() {
        let err = Expr::parse("genrre=techno").expect_err("a typo parsed");
        assert!(format!("{err}").contains("genrre"), "{err}");
    }

    /// The label joins one vocabulary, so it filters without the filter
    /// language being told about it separately — the whole point of sharing
    /// `format::Field`.
    #[test]
    fn the_label_filters_like_any_other_text_field() {
        let bare = track();
        assert!(
            matches("!label?", &bare),
            "an untagged label must be missing"
        );

        let mut labelled = track();
        labelled.label = Some("trip recordings".into());
        assert!(matches("label?", &labelled));
        assert!(matches(r#"label="trip*""#, &labelled));
        // Smart-case: a capital in the pattern makes it exact.
        assert!(!matches(r#"label="*RECORDINGS""#, &labelled));
        // Smart-case, like every other text field: lowercase is insensitive.
        assert!(matches(r#"label="*recordings""#, &labelled));

        // Not an `any` field: `any` covers what you would search for by name,
        // and a label is a fact about the release rather than its title.
        assert!(!matches(r#"any="*trip*""#, &labelled));
    }

    #[test]
    fn globs_anchor_where_the_stars_are() {
        for (pattern, text, want) in [
            ("Ferrell", "Ferrell", true),
            ("Ferrell", "Sierra Ferrell", false),
            ("Ferrell*", "Ferrell Sings", true),
            ("Ferrell*", "Sierra Ferrell", false),
            ("*Ferrell", "Sierra Ferrell", true),
            ("*Ferrell", "Ferrell Sings", false),
            ("*Ferrell*", "Sierra Ferrell Sings", true),
            ("*", "anything at all", true),
            ("*", "", true),
            ("a*c", "abc", true),
            ("a*c", "ac", true),
            ("a*c", "abd", false),
            ("a*b*c", "axxbyyc", true),
            ("a*b*c", "acb", false),
        ] {
            assert_eq!(
                Glob::new(pattern).matches(text),
                want,
                "{pattern:?} against {text:?}"
            );
        }
    }

    /// Smart-case, as in vim: insensitive until you type a capital. Tags are
    /// inconsistently capitalised, so `genre=jazz` has to find "Jazz".
    #[test]
    fn smart_case() {
        // Lowercase pattern: case-insensitive.
        assert!(Glob::new("*ferrell*").matches("Sierra Ferrell"));
        assert!(Glob::new("*ferrell*").matches("SIERRA FERRELL"));
        // A capital anywhere in the pattern makes it exact.
        assert!(!Glob::new("*FERRELL*").matches("Sierra Ferrell"));
        assert!(!Glob::new("*Ferrell*").matches("sierra ferrell"));
        // And an unstarred pattern is still exact, not a substring.
        assert!(!Glob::new("ferrell").matches("Sierra Ferrell"));
    }

    /// `?` is a literal, and `*` can be escaped. Both matter because music
    /// titles contain question marks and (almost) never asterisks.
    #[test]
    fn question_marks_are_literal_and_stars_escape() {
        assert!(Glob::new("*You?").matches("Have You Got It In You?"));
        assert!(!Glob::new("*You?").matches("Have You Got It In You"));
        assert!(Glob::new(r"*\**").matches("2 * 2"));
        assert!(!Glob::new(r"*\**").matches("no star here"));
    }

    /// The characters that make regex the wrong choice here are ordinary text
    /// to a glob. These are real values from the reference library.
    #[test]
    fn regex_metacharacters_are_just_characters() {
        for (pattern, text) in [
            ("Burial (S/T)", "Burial (S/T)"),
            ("*(S/T)", "Burial (S/T)"),
            ("Anderson .Paak", "Anderson .Paak"),
            ("*In You?", "Have You Got It In You?"),
        ] {
            assert!(Glob::new(pattern).matches(text), "{pattern:?}");
        }
        // And the over-match regex would have allowed does not happen.
        assert!(!Glob::new("Anderson .Paak").matches("Anderson XPaak"));
    }

    #[test]
    fn comparisons_and_booleans() {
        let t = track();
        assert!(matches(r#"artist="Sierra Ferrell""#, &t));
        assert!(matches(r#"artist="*ferrell*""#, &t));
        assert!(matches("year>2020", &t));
        assert!(!matches("year>2021", &t));
        assert!(matches("year>=2021&bitrate>256", &t));
        assert!(matches(r#"genre="jazz"|genre="folk""#, &t));
        assert!(!matches(r#"genre="jazz"|genre="blues""#, &t));
        assert!(matches(r#"!genre="jazz""#, &t));
        assert!(matches(r#"(genre="folk"|genre="country")&year>2000"#, &t));
        assert!(matches("duration>200&duration<300", &t));
    }

    /// `&` binds tighter than `|`, so `a|b&c` is `a or (b and c)`. Getting
    /// this backwards silently changes what a catalog contains.
    #[test]
    fn and_binds_tighter_than_or() {
        let t = track();
        // folk is true, jazz false, year>3000 false.
        // As `folk | (jazz & impossible)` this is true.
        // As `(folk | jazz) & impossible` it would be false.
        assert!(matches(r#"genre="folk"|genre="jazz"&year>3000"#, &t));
        assert!(!matches(r#"(genre="folk"|genre="jazz")&year>3000"#, &t));
    }

    #[test]
    fn unquoted_values_work_where_they_are_unambiguous() {
        let t = track();
        assert!(matches("genre=folk", &t));
        assert!(matches("artist=*ferrell*", &t));
        assert!(matches("year=2021", &t));
    }

    /// `any` is how you search every text field — explicit, and composable
    /// with everything else, unlike a bare word.
    #[test]
    fn any_searches_the_text_fields() {
        let t = track();
        assert!(matches(r#"any="*ferrell*""#, &t));
        assert!(matches(r#"any="*long time*""#, &t));
        assert!(matches(r#"any="*the sea*""#, &t));
        assert!(!matches(r#"any="*nothing here*""#, &t));
        assert!(matches(r#"any="*folk*"&year>2000"#, &t));
    }

    /// `any` must not look at the full path, or the library root would make
    /// every track match anything under it.
    #[test]
    fn any_ignores_the_directory_path() {
        let t = track();
        assert!(matches(r#"path="*Long Time Coming*""#, &t));
        assert!(
            !matches(r#"any="*/m/*""#, &t),
            "any reached into the directory path"
        );
    }

    /// A missing tag matches nothing, including `!=`. Three-valued logic is
    /// more correct and much harder to predict; `!` on the whole comparison is
    /// how you ask for "or missing".
    /// **Any of them, not the first of them** (#86).
    ///
    /// A genre tag names several terms in a hierarchical codelist, and a file
    /// tagged `Rock; Blues Rock` is Rock. Matching only the joined string, or
    /// only the first term, made most of the library unfindable by the genre
    /// somebody would actually search for.
    #[test]
    fn a_genre_matches_any_of_the_terms_a_file_carries() {
        let mut t = track();
        t.genres = vec![
            "Rock".into(),
            "Blues Rock".into(),
            "Alternative Rock".into(),
        ];
        assert!(matches(r#"genre="Rock""#, &t), "the first term");
        assert!(matches(r#"genre="Blues Rock""#, &t), "and a later one");
        assert!(matches(r#"genre="Alternative Rock""#, &t), "and the last");
        assert!(
            !matches(r#"genre="Jazz""#, &t),
            "and nothing it does not carry"
        );
    }

    /// **`*` is zero-or-more and `+` is one-or-more** — #93, SPARQL's
    /// spelling, one meaning per character.
    ///
    /// `genre="electronic"` finds what is tagged exactly that. Neither is what
    /// somebody means by "play me electronic music", which is the whole reason
    /// a hierarchy exists — and until now the only way to ask was to name
    /// every narrower term.
    #[test]
    fn closure_reaches_narrower_concepts() {
        let mut t = track();
        t.genres = vec!["Techno".into()];
        // Techno narrower-of Electronic narrower-of Music.
        let h = Fake {
            broader: vec!["Techno".into(), "Electronic".into(), "Music".into()],
            ..Fake::default()
        };

        // Exact reaches nothing it is not.
        assert!(!matches_env(r#"genre="Electronic""#, &t, &h, 0));

        // `*` is it or anything narrower; `+` is strictly narrower.
        assert!(matches_env(r#"genre*="Electronic""#, &t, &h, 0));
        assert!(matches_env(r#"genre+="Electronic""#, &t, &h, 0));
        assert!(
            matches_env(r#"genre*="Music""#, &t, &h, 0),
            "at any depth, not one hop"
        );

        // And on the concept itself the two differ, which is the point of
        // having both characters.
        let mut electronic = track();
        electronic.genres = vec!["Electronic".into()];
        assert!(matches_env(r#"genre*="Electronic""#, &electronic, &h, 0));
        assert!(
            !matches_env(r#"genre+="Electronic""#, &electronic, &h, 0),
            "`+` is one hop or more, so a concept is not narrower than itself"
        );
    }

    /// Closure composes with the quantifier rather than replacing it.
    #[test]
    fn closure_and_quantification_are_separate_questions() {
        let mut t = track();
        t.genres = vec!["Techno".into(), "Folk".into()];
        let h = Fake {
            broader: vec!["Techno".into(), "Electronic".into()],
            ..Fake::default()
        };

        // **The marker goes inside**: closure is about the field, the
        // quantifier is about how many of its values must satisfy the test.
        assert!(matches_env(r#"any(genre*)="Electronic""#, &t, &h, 0));
        assert!(
            !matches_env(r#"all(genre*)="Electronic""#, &t, &h, 0),
            "folk is not electronic"
        );
    }

    /// **Quantification is explicit, or the language lies** — #93.
    ///
    /// The two readings were both here and neither was sayable: `=` meant
    /// *some* and `!=` meant *none*, one syntax and two quantifiers. Both are
    /// useful and a reader could not tell which they were getting.
    #[test]
    fn any_and_all_say_which_quantifier() {
        let mut t = track();
        t.genres = vec!["Rock".into(), "Blues Rock".into()];

        // Some of them, which is what the bare form has always meant.
        assert!(matches(r#"any(genre)="Rock""#, &t));
        assert!(!matches(r#"any(genre)="Jazz""#, &t));

        // **Every one of them**, which could not be asked before.
        assert!(
            !matches(r#"all(genre)="Rock""#, &t),
            "Blues Rock is not Rock"
        );
        assert!(matches(r#"all(genre)="*Rock""#, &t), "both end in Rock");

        // And the two readings of `!=`, which is where the silence was.
        assert!(
            matches(r#"any(genre)!="Rock""#, &t),
            "some genre is not Rock — Blues Rock"
        );
        assert!(
            !matches(r#"all(genre)!="Rock""#, &t),
            "but not every one of them"
        );
    }

    /// **The defaults are unchanged, and now they have names.**
    ///
    /// `=` is `any` and `!=` is `all`, which is the asymmetry #93 called
    /// silent. Making both `any` would have made `genre!="Rock"` true of a
    /// file tagged `Rock; Blues Rock`, which is not what anybody means.
    #[test]
    fn the_bare_form_is_the_operators_default() {
        let mut t = track();
        t.genres = vec!["Rock".into(), "Blues Rock".into()];

        for (bare, said) in [
            (r#"genre="Rock""#, r#"any(genre)="Rock""#),
            (r#"genre="Jazz""#, r#"any(genre)="Jazz""#),
            (r#"genre!="Rock""#, r#"all(genre)!="Rock""#),
            (r#"genre!="Jazz""#, r#"all(genre)!="Jazz""#),
        ] {
            assert_eq!(
                matches(bare, &t),
                matches(said, &t),
                "{bare} and {said} disagree"
            );
        }
    }

    /// **`all` is not vacuously true on nothing.**
    ///
    /// Formal logic says every element of the empty set satisfies anything; a
    /// listener asking "all of its genres are rock" about an untagged track
    /// does not mean yes. `!genre?` is the question they would be asking, and
    /// it is a different one.
    #[test]
    fn all_of_nothing_is_not_everything() {
        let mut t = track();
        t.genres.clear();
        assert!(!matches(r#"all(genre)="Rock""#, &t));
        assert!(!matches(r#"all(genre)!="Rock""#, &t));
        assert!(matches("!genre?", &t), "which is how you ask for untagged");
    }

    /// And `!=` is *none* of them, not "not the first".
    #[test]
    fn not_equal_means_no_term_matches() {
        let mut t = track();
        t.genres = vec!["Rock".into(), "Blues Rock".into()];
        assert!(!matches(r#"genre!="Blues Rock""#, &t), "it does carry it");
        assert!(
            matches(r#"genre!="Jazz""#, &t),
            "and it does not carry this"
        );
    }

    /// `any=` reaches every term too, since it is the same lookup.
    #[test]
    fn any_reaches_a_later_genre_term() {
        let mut t = track();
        t.genres = vec!["Rock".into(), "Shoegaze".into()];
        assert!(matches(r#"any="Shoegaze""#, &t));
    }

    #[test]
    fn a_missing_tag_matches_nothing() {
        let mut t = track();
        t.genres.clear();
        t.date = None;
        assert!(!matches(r#"genre="folk""#, &t));
        assert!(!matches(r#"genre!="folk""#, &t));
        assert!(!matches("year>1900", &t));
        assert!(matches(r#"!genre="folk""#, &t), "negation still works");
    }

    /// Filing an album-less track under a label is *presentation*: the tag
    /// stays empty, so `!album?` still finds exactly those tracks. Reading
    /// `album_or_unknown()` here instead would silently make every single look
    /// like it belongs to a record called "singles".
    #[test]
    fn filing_a_single_does_not_give_it_an_album() {
        let mut t = track();
        t.album = None;
        assert!(matches("!album?", &t));
        assert!(!matches("album?", &t));
        assert!(
            !matches(r#"album="*singles*""#, &t),
            "the display label leaked into the tag"
        );
    }

    const DAY: i64 = 86_400;

    /// A stand-in library, answering for whatever recording the test track is.
    ///
    /// Written by hand rather than folded from a log, which is the point of
    /// `Listened` being a trait: the filter language is tested against the
    /// four questions it asks, not against the machinery behind them.
    #[derive(Default)]
    struct Fake {
        plays: u32,
        last_played: Option<i64>,
        acquired: Option<i64>,
        rating: Option<u8>,
        collected: u32,
        /// A narrower-of chain, child first, as `broader` would return it.
        broader: Vec<String>,
    }

    impl Listened for Fake {
        fn plays(&self, _: Id<Recording>) -> u32 {
            self.plays
        }
        fn last_played(&self, _: Id<Recording>) -> Option<i64> {
            self.last_played
        }
        fn acquired(&self, _: Id<Recording>) -> Option<i64> {
            self.acquired
        }
        fn collected(&self, _: Id<Recording>) -> u32 {
            self.collected
        }
        fn broader(&self, _: &str, label: &str) -> Vec<String> {
            // The fixture's chain, from wherever the label sits upward.
            match self.broader.iter().position(|l| l == label) {
                Some(at) => self.broader[at + 1..].to_vec(),
                None => Vec::new(),
            }
        }
        fn rating(&self, _: Id<Recording>) -> Option<u8> {
            self.rating
        }
    }

    fn with_history(plays: u32, last_played: Option<i64>, added: i64) -> Fake {
        Fake {
            plays,
            last_played,
            acquired: Some(added),
            rating: None,
            collected: 0,
            broader: Vec::new(),
        }
    }

    /// **"In no crate" is a question somebody can ask** — #119.
    ///
    /// Zero collections is *missing*, not zero, so `!collected?` finds what
    /// nobody has filed. Counting it as nought would make an unfiled track
    /// look like one deliberately put in nothing, which is the same mistake
    /// `rating` avoids: unrated is not nought stars.
    #[test]
    fn collected_says_how_many_crates_hold_it() {
        let t = track();
        let none = Fake::default();
        let one = Fake {
            collected: 1,
            ..Fake::default()
        };
        let both = Fake {
            collected: 2,
            ..Fake::default()
        };

        assert!(
            matches_env("!collected?", &t, &none, 0),
            "unfiled not found"
        );
        assert!(!matches_env("!collected?", &t, &one, 0));
        assert!(matches_env("collected?", &t, &one, 0));

        // **The overlap the filesystem used to forbid.** A pop song is in
        // Dance when you are DJing with it and in General the rest of the
        // time, and both are true — so this is a count, not a flag.
        assert!(matches_env("collected>1", &t, &both, 0));
        assert!(!matches_env("collected>1", &t, &one, 0));
    }

    fn matches_env(expr: &str, t: &Track, h: &Fake, now: i64) -> bool {
        let e = Expr::parse(expr).expect("should parse");
        let env = Env {
            listened: Some(h),
            now,
        };
        let (drawn, recording) = subject_of(t);
        e.compile(env)(&Subject {
            row: drawn.listed(),
            locator: t.path.to_str(),
            recording,
        })
    }

    /// Play counts, and the query that matters most: what have I never heard?
    #[test]
    fn play_count_reads_from_the_history() {
        let t = track();
        let now = 100 * DAY;
        let played = with_history(7, Some(now - DAY), 50 * DAY);

        assert!(matches_env("play_count=7", &t, &played, now));
        assert!(matches_env("play_count>5", &t, &played, now));
        assert!(!matches_env("play_count>7", &t, &played, now));

        // A track the history has never seen has played zero times — an
        // answer, not a missing value, so `play_count=0` finds it.
        let empty = Fake::default();
        assert!(matches_env("play_count=0", &t, &empty, now));
        assert!(!matches_env("play_count>0", &t, &empty, now));
    }

    /// Dates are exposed as *days since*, so a query reads as English and the
    /// grammar needs no date literals.
    #[test]
    fn days_since_played_and_added() {
        let t = track();
        let now = 100 * DAY;
        let h = with_history(3, Some(now - 10 * DAY), now - 40 * DAY);

        assert!(matches_env("days_since_played=10", &t, &h, now));
        assert!(matches_env("days_since_played<30", &t, &h, now));
        assert!(!matches_env("days_since_played>30", &t, &h, now));

        assert!(matches_env("days_since_added=40", &t, &h, now));
        assert!(matches_env("days_since_added>30", &t, &h, now));

        // The two compose, which is the point.
        assert!(matches_env(
            "days_since_added>30&days_since_played<30",
            &t,
            &h,
            now
        ));
    }

    /// Never played has no "days since", so `!days_since_played?` finds
    /// exactly those — and it must not read as zero days, which would make
    /// them look freshly played.
    #[test]
    fn a_track_never_played_has_no_days_since() {
        let t = track();
        let now = 100 * DAY;
        let h = Fake {
            acquired: Some(now - 5 * DAY),
            ..Fake::default()
        };

        assert!(!matches_env("days_since_played?", &t, &h, now));
        assert!(matches_env("!days_since_played?", &t, &h, now));
        assert!(
            !matches_env("days_since_played=0", &t, &h, now),
            "never played must not read as played today"
        );
        assert!(matches_env("days_since_added=5", &t, &h, now));
    }

    /// With no history at all, every history field is *missing* rather than
    /// zero — "cannot say" is not the same as "never". The line prompt keeps
    /// no history, so this is a real case.
    #[test]
    fn without_a_history_the_fields_are_missing() {
        let t = track();
        let (drawn, recording) = subject_of(&t);
        let subject = Subject {
            row: drawn.listed(),
            locator: t.path.to_str(),
            recording,
        };
        let expr = |s: &str| Expr::parse(s).unwrap().compile(Env::default())(&subject);
        assert!(!expr("play_count=0"), "cannot say is not zero");
        assert!(!expr("play_count>0"));
        assert!(!expr("days_since_added?"));
        assert!(expr("!days_since_added?"));
    }

    /// Unrated is missing, not zero — `!rating?` finds what you have not
    /// judged, and calling those nought stars would make them look disliked.
    #[test]
    fn unrated_is_missing_not_zero() {
        let t = track();
        let now = 100 * DAY;
        let mut h = Fake {
            acquired: Some(now),
            ..Fake::default()
        };

        assert!(!matches_env("rating?", &t, &h, now));
        assert!(matches_env("!rating?", &t, &h, now));

        h.rating = Some(4);
        assert!(matches_env("rating?", &t, &h, now));
        assert!(matches_env("rating=4", &t, &h, now));
        assert!(matches_env("rating>=4", &t, &h, now));
        assert!(!matches_env("rating>4", &t, &h, now));
        // The query this whole feature is for.
        assert!(matches_env("rating>=4&play_count=0", &t, &h, now));
    }

    /// Ratings are 1 to 5 or absent — so zero is not a value, and asking
    /// about it says the idiom instead of quietly matching nothing.
    #[test]
    fn zero_is_not_a_rating() {
        for expr in ["rating=0", "rating>0", "rating<0", "rating!=0", "rating>=0"] {
            assert_eq!(
                Expr::parse(expr),
                Err(FilterError::RatingZero),
                "{expr} should have been rejected"
            );
        }
        // The message points at what to write instead.
        let msg = Expr::parse("rating=0").unwrap_err().to_string();
        assert!(msg.contains("!rating?"), "{msg}");

        // Real ratings are untouched, and so are other fields' zeros.
        assert!(Expr::parse("rating=1").is_ok());
        assert!(Expr::parse("rating>=4").is_ok());
        assert!(
            Expr::parse("play_count=0").is_ok(),
            "0 plays is a real answer"
        );
        assert!(Expr::parse("days_since_played=0").is_ok(), "played today");
    }

    #[test]
    fn history_fields_are_numbers() {
        assert_eq!(
            Expr::parse(r#"play_count="a""#),
            Err(FilterError::NotText {
                field: "play_count".into()
            })
        );
        // And they appear in the unknown-field message.
        let msg = Expr::parse("play_counts=1").unwrap_err().to_string();
        assert!(msg.contains("play_count"), "{msg}");
    }

    /// `field?` asks whether the track has the field at all — the one thing
    /// foobar2000 could say and this language could only imply.
    #[test]
    fn presence_and_absence() {
        let full = track();
        let mut bare = track();
        bare.genres.clear();
        bare.date = None;

        assert!(matches("genre?", &full));
        assert!(!matches("genre?", &bare));
        assert!(matches("!genre?", &bare));
        assert!(!matches("!genre?", &full));

        // Numbers too, with the same spelling — the asymmetry this fixes was
        // needing `genre="*"` for text and `year>=0` for numbers.
        assert!(matches("year?", &full));
        assert!(!matches("year?", &bare));
        assert!(matches("!year?", &bare));

        // Composes like any other predicate.
        assert!(matches("genre?&year>2020", &full));
        assert!(matches(r#"!genre?|artist="*ferrell*""#, &bare));
    }

    /// An empty tag is a tagging artefact, not an answer. Counting it as
    /// present would make `!genre?` miss the files worth finding.
    #[test]
    fn an_empty_tag_counts_as_missing() {
        let mut t = track();
        t.genres = vec![String::new()];
        assert!(!matches("genre?", &t));
        t.genres = vec!["   ".into()];
        assert!(!matches("genre?", &t), "whitespace is not a genre either");
    }

    /// A `?` in a value is a question mark, not a presence test. The check is
    /// on the field word only, which is what keeps `title=You?` working.
    #[test]
    fn a_question_mark_in_a_value_is_still_literal() {
        let mut t = track();
        t.title = Some("Have You Got It In You?".into());
        assert!(matches(r#"title="*In You?""#, &t));
        assert!(matches("title=*You?", &t), "unquoted, too");
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert_eq!(
            Expr::parse("nosuch=1"),
            Err(FilterError::UnknownField {
                name: "nosuch".into()
            })
        );
        assert_eq!(
            Expr::parse(r#"artist>"a""#),
            Err(FilterError::NotOrderable {
                field: "artist".into(),
                op: ">"
            })
        );
        assert_eq!(
            Expr::parse(r#"year="2020""#),
            Err(FilterError::NotText {
                field: "year".into()
            })
        );
        assert_eq!(
            Expr::parse("year>abc"),
            Err(FilterError::NotANumber("abc".into()))
        );
        assert_eq!(Expr::parse(r#"artist="x"#), Err(FilterError::UnclosedQuote));
        assert_eq!(Expr::parse("(year>1"), Err(FilterError::UnclosedParen));
        assert_eq!(Expr::parse(""), Err(FilterError::Empty));
        // A bare word is an error, not an implicit any-field search.
        assert!(Expr::parse("ferrell").is_err());
        assert_eq!(Expr::parse("any?"), Err(FilterError::PresentOnAny));
        assert_eq!(
            Expr::parse("nosuch?"),
            Err(FilterError::UnknownField {
                name: "nosuch".into()
            })
        );
        // The unknown-field message lists the alternatives.
        let msg = Expr::parse("artistt=x").unwrap_err().to_string();
        assert!(msg.contains("artistt") && msg.contains("artist"), "{msg}");
    }

    /// The end-to-end shape of what the folding is for: a filter typed on a
    /// keyboard without an umlaut still selects the tracks that have one.
    #[test]
    fn a_filter_finds_an_umlaut_typed_either_way() {
        let g = |pattern: &str| Glob::new(pattern);
        for typed in ["*loffler*", "*loeffler*", "*löffler*", "*Löffler*"] {
            assert!(
                g(typed).matches("Christian Löffler"),
                "{typed:?} should have matched"
            );
        }
    }

    #[test]
    fn a_filter_matches_across_the_fullwidth_stand_ins() {
        // The tag says `/`; this library's filename says `⧸`.
        assert!(
            Glob::new("*thereof / sparing*").matches("Air & Lack Thereof ⧸ Sparing the Horses")
        );
    }

    /// Smart-case governs **capitals only**. Typing one asks for case to
    /// matter; there is no way to type a capital to ask for a diaeresis you do
    /// not have on the keyboard, so marks always fold.
    #[test]
    fn smart_case_governs_capitals_and_not_the_marks() {
        assert!(
            Glob::new("*Loffler*").matches("Christian Löffler"),
            "the capital makes case matter, and the umlaut still folds"
        );
        assert!(
            !Glob::new("*Loffler*").matches("christian löffler"),
            "but the capital does have to match"
        );
        assert!(
            Glob::new("*loffler*").matches("Christian LÖFFLER"),
            "no capital typed, so neither case nor the mark matters"
        );
    }
}
